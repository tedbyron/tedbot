//! `/request`: find a movie or show in Seerr and request it without opening Seerr.
//!
//! Replies ephemerally with one result per card: poster, year, overview and where it stands, with
//! ◀ ▶ through the results, a season picker for shows, and a Request button. Buttons work until
//! [`NAV_TIMEOUT`] passes with no press, then they're removed.
//!
//! Requests are made as the Seerr user who put the invoker's Discord id in their notification
//! settings, so Seerr's permissions, quotas and auto-approval apply as if they'd used the site.
//! Users Seerr would show advanced options also get a quality picker, and those who can bypass
//! their quota get a second button that requests without counting against it.

use std::time::Duration;

use anyhow::{Context as _, Result};
use poise::{
    CreateReply, command,
    serenity_prelude::{
        AutocompleteChoice, Builder as _, ButtonStyle, ComponentInteractionCollector,
        ComponentInteractionDataKind, CreateActionRow, CreateAutocompleteResponse, CreateButton,
        CreateEmbed, CreateEmbedFooter, CreateInteractionResponse,
        CreateInteractionResponseFollowup, CreateInteractionResponseMessage, CreateSelectMenu,
        CreateSelectMenuKind, CreateSelectMenuOption, EditInteractionResponse,
    },
};
use tracing::warn;

use super::{escape, is_admin, remove_buttons};
use crate::{
    Context, Data,
    arr::{Arr, Removed},
    seerr::{
        self, Cancellable, Changed, Media, Profile, Quotas, Requested, Requester, SearchResult,
        Status,
    },
};

/// How long buttons keep working after the last press. An ephemeral reply can only be edited with
/// an interaction token, which expires after 15 minutes, so this stays under that to remove the
/// buttons at the end.
const NAV_TIMEOUT: Duration = Duration::from_secs(14 * 60);
/// TMDB image URL prefix for posters, 342 pixels wide
const POSTER_URL: &str = "https://image.tmdb.org/t/p/w342";
/// Longest overview shown, in characters
const OVERVIEW_MAX: usize = 350;
/// Discord allows at most 25 options per select menu
const MAX_OPTIONS: usize = 25;
/// Reply when Seerr can't be reached
const UNREACHABLE: &str = "Couldn't reach Seerr, try again in a bit \u{1fae4}";

/// Request a movie or show
#[command(slash_command)]
pub async fn request(
    ctx: Context<'_>,
    #[description = "Movie or show title"]
    #[autocomplete = "autocomplete_title"]
    title: String,
) -> Result<()> {
    ctx.defer_ephemeral().await?;

    let data = ctx.data();
    let (user, results) = tokio::join!(
        data.seerr.user_for(&data.http, ctx.author().id),
        results(data, &title),
    );
    let (user, results) = (user?, results?);
    if results.is_empty() {
        ctx.say(format!("Nothing matches \"{}\"", escape(&title)))
            .await?;
        return Ok(());
    }

    let id = ctx.id().to_string();
    let mut viewer = Viewer::load(data, user, is_admin(ctx).await).await;
    let mut card = Card::build(data, &results, 0, &viewer).await?;
    let reply = CreateReply::default()
        .components(card.components(&id))
        .embed(std::mem::take(&mut card.embed));
    let handle = ctx.send(reply).await?;
    let message = handle.message().await?.id;
    let mut last = None;

    while let Some(press) = ComponentInteractionCollector::new(ctx)
        .message_id(message)
        .timeout(NAV_TIMEOUT)
        .await
    {
        // `bypass` is whether to skip the quota, for request buttons only
        let (index, action) = match press.data.custom_id.strip_prefix(id.as_str()) {
            Some("-next") => (card.index + 1, None),
            Some("-prev") => (card.index + results.len() - 1, None),
            Some("-request") => (card.index, Some(Action::Request { bypass: false })),
            Some("-request-bypass") => (card.index, Some(Action::Request { bypass: true })),
            Some(choice @ ("-seasons" | "-quality")) => {
                // Only the menus and buttons change, so answer with them straight away
                let values = match &press.data.kind {
                    ComponentInteractionDataKind::StringSelect { values } => values.as_slice(),
                    _ => &[],
                };
                let numbers = values.iter().filter_map(|v| v.parse().ok());
                if choice == "-seasons" {
                    card.picked = numbers.collect();
                } else {
                    card.profile = numbers.into_iter().next();
                }
                let update =
                    CreateInteractionResponseMessage::new().components(card.components(&id));
                press
                    .create_response(ctx, CreateInteractionResponse::UpdateMessage(update))
                    .await?;
                last = Some(press.token);
                continue;
            }
            Some("-cancel") => (card.index, Some(Action::Cancel)),
            _ => continue,
        };

        // Acknowledge first: Seerr looks things up on TMDB, which may take longer than the 3
        // seconds Discord allows before an interaction fails
        press
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await?;
        let token = press.token;
        last = Some(token.clone());

        let done = match (action, user) {
            (Some(Action::Request { bypass }), Some(user)) => {
                Some(card.request(data, user.id, bypass).await)
            }
            (Some(Action::Cancel), _) => {
                let (notice, changed) = card.cancel(data).await;
                Some((Some(notice), changed))
            }
            _ => None,
        };
        // Both use up or give back quota
        if let (Some((_, true)), Some(user)) = (&done, user) {
            viewer.quotas = Viewer::quotas(data, user).await;
        }
        let mut notice = done.and_then(|(notice, _)| notice);
        match Card::build(data, &results, index, &viewer).await {
            Ok(next) => {
                card = next;
                EditInteractionResponse::new()
                    .components(card.components(&id))
                    .embed(std::mem::take(&mut card.embed))
                    .execute(ctx, &token)
                    .await?;
            }
            Err(e) => {
                warn!("Loading request card: {e:#}");
                notice.get_or_insert_with(|| UNREACHABLE.into());
            }
        }
        if let Some(notice) = notice {
            CreateInteractionResponseFollowup::new()
                .content(notice)
                .ephemeral(true)
                .execute(ctx, (None, &token))
                .await?;
        }
    }

    // The newest token is the only one guaranteed not to have expired
    remove_buttons(ctx, last).await
}

/// Suggests up to 25 movies and shows (Discord's limit), filling in `movie:id` or `tv:id` so the
/// command opens that exact one. A failed search gives no suggestions rather than an error.
async fn autocomplete_title(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let partial = partial.trim();
    if partial.is_empty() {
        return CreateAutocompleteResponse::new();
    }
    let data = ctx.data();
    let choices = match data.seerr.search(&data.http, partial).await {
        Ok(results) => results
            .iter()
            .filter_map(|r| {
                let media = r.media()?;
                let kind = match media {
                    Media::Movie(_) => "Movie",
                    Media::Tv(_) => "TV show",
                };
                let name: String = format!("{} \u{b7} {kind}", r.label())
                    .chars()
                    .take(100)
                    .collect();
                Some(AutocompleteChoice::new(name, media.to_string()))
            })
            .take(25)
            .collect(),
        Err(e) => {
            warn!("Searching Seerr for autocomplete: {e:#}");
            Vec::new()
        }
    };
    CreateAutocompleteResponse::new().set_choices(choices)
}

/// What `title` refers to: the one an autocomplete suggestion picked, or else Seerr's search
/// results
async fn results(data: &Data, title: &str) -> Result<Vec<Media>> {
    if let Ok(media) = title.parse() {
        return Ok(vec![media]);
    }
    let results = data.seerr.search(&data.http, title).await?;
    Ok(results.iter().filter_map(SearchResult::media).collect())
}

/// What a button press changes in Seerr
#[derive(Clone, Copy)]
enum Action {
    /// Request the movie or picked seasons, skipping the quota if `bypass`
    Request { bypass: bool },
    /// Cancel the requests on the card the invoker may cancel
    Cancel,
}

/// Who's looking, and what Seerr lets them do. Everything past the user is best effort: if it
/// can't be fetched, the card goes without it.
struct Viewer {
    /// The invoker's Seerr user, `None` if they haven't linked their Discord id
    user: Option<Requester>,
    /// Whether the invoker is a bot admin, who may cancel anyone's request at any point
    admin: bool,
    /// The user's quotas
    quotas: Option<Quotas>,
    /// Movie quality profiles, if the user may pick one
    movie_profiles: Vec<Profile>,
    /// Show quality profiles, if the user may pick one
    tv_profiles: Vec<Profile>,
}

impl Viewer {
    /// Fetches the user's quotas and, if they may pick one, the quality profiles
    async fn load(data: &Data, user: Option<Requester>, admin: bool) -> Self {
        let Some(user) = user else {
            return Self {
                user,
                admin,
                quotas: None,
                movie_profiles: Vec::new(),
                tv_profiles: Vec::new(),
            };
        };
        let profiles = |media| async move {
            if !user.can_pick_profile() {
                return Vec::new();
            }
            data.seerr
                .profiles(&data.http, media, false)
                .await
                .inspect_err(|e| warn!("Fetching quality profiles: {e:#}"))
                .ok()
                .flatten()
                .map(|p| p.list)
                .unwrap_or_default()
        };
        let (quotas, movie_profiles, tv_profiles) = tokio::join!(
            Self::quotas(data, user),
            profiles(Media::Movie(0)),
            profiles(Media::Tv(0)),
        );
        Self {
            user: Some(user),
            admin,
            quotas,
            movie_profiles,
            tv_profiles,
        }
    }

    /// The user's quotas, `None` if they can't be fetched
    async fn quotas(data: &Data, user: Requester) -> Option<Quotas> {
        data.seerr
            .quotas(&data.http, user.id)
            .await
            .inspect_err(|e| warn!("Fetching quotas: {e:#}"))
            .ok()
    }
}

/// One rendered result, plus what the buttons need
struct Card {
    /// Taken (left empty) when sent
    embed: CreateEmbed,
    media: Media,
    /// "Title (2022)"
    title: String,
    /// Position in the results
    index: usize,
    /// Number of results, for ◀ ▶
    count: usize,
    /// Whether the movie, or any season of the show, can be requested
    requestable: bool,
    /// Whether the invoker is linked to a Seerr user, and so can request
    linked: bool,
    /// Seasons that can be requested, shows only
    open: Vec<u32>,
    /// Seasons picked in the menu. Empty means all of [`Self::open`].
    picked: Vec<u32>,
    /// Quality profiles to pick from, empty if the invoker may not
    profiles: Vec<Profile>,
    /// Profile picked in the menu, `None` for Seerr's default
    profile: Option<u32>,
    /// Whether the invoker has a quota for this kind of media and may bypass it
    can_bypass: bool,
    /// Requests the invoker may cancel. Ones already sent to Radarr or Sonarr are left out if
    /// the bot can't reach it.
    cancellable: Vec<Cancellable>,
    /// TVDB id, shows only, once Seerr knows of it
    tvdb_id: Option<u32>,
    /// Jellyfin page, once it's available
    watch: Option<String>,
}

impl Card {
    /// Fetches result `index` (wrapping around) from Seerr and renders it
    async fn build(data: &Data, results: &[Media], index: usize, viewer: &Viewer) -> Result<Self> {
        let index = index % results.len();
        let media = results[index];
        let details = data.seerr.details(&data.http, media).await?;
        let title = seerr::with_year(&details.title, details.release_date.as_deref());

        let mut embed = CreateEmbed::new().title(&title);
        if let Some(url) = data.seerr.page(media) {
            embed = embed.url(url);
        }
        if let Some(overview) = details.overview.as_deref().filter(|o| !o.is_empty()) {
            embed = embed.description(truncate(overview, OVERVIEW_MAX));
        }
        if let Some(path) = &details.poster_path {
            embed = embed.thumbnail(format!("{POSTER_URL}{path}"));
        }

        let blocked = details.status() == Status::Blocklisted;
        let (kind, requestable, open) = match media {
            Media::Movie(_) => {
                let label = status(details.status(), details.auto_approved(None));
                embed = embed.field("Status", label, false);
                ("Movie", details.status() == Status::Missing, Vec::new())
            }
            Media::Tv(_) => {
                let seasons: Vec<(u32, Status, bool)> = details
                    .season_numbers()
                    .map(|n| (n, details.season_status(n), details.auto_approved(Some(n))))
                    .collect();
                embed = embed.field("Seasons", season_lines(&seasons), false);
                let open: Vec<u32> = seasons
                    .iter()
                    .filter(|&&(_, s, _)| s == Status::Missing)
                    .map(|&(n, ..)| n)
                    .collect();
                ("TV show", !blocked && !open.is_empty(), open)
            }
        };

        // One line each: "TV show quota: 4 of 6 seasons left (7 days)", "1 of 20", and how to link
        let linked = viewer.user.is_some();
        let quota = viewer.quotas.as_ref().map(|q| match media {
            Media::Movie(_) => (&q.movie, "movies"),
            Media::Tv(_) => (&q.tv, "seasons"),
        });
        let limited = quota.is_some_and(|(q, _)| q.limit > 0);
        let mut footer = Vec::new();
        if let Some((q, unit)) = quota.filter(|_| limited && requestable) {
            let left = q.remaining.unwrap_or_default();
            let mut line = format!("{kind} quota: {left} of {} {unit} left", q.limit);
            if q.days > 0 {
                line = format!("{line} ({} days)", q.days);
            }
            footer.push(line);
        }
        if results.len() > 1 {
            footer.push(format!("{} of {}", index + 1, results.len()));
        }
        if requestable && !linked {
            footer.push(
                "To request from here, add your Discord user ID to your Seerr notification \
                 settings"
                    .into(),
            );
        }
        if !footer.is_empty() {
            embed = embed.footer(CreateEmbedFooter::new(footer.join("\n")));
        }

        let profiles = match media {
            Media::Movie(_) => &viewer.movie_profiles,
            Media::Tv(_) => &viewer.tv_profiles,
        };
        let can_bypass = limited && viewer.user.is_some_and(Requester::can_bypass_quota);
        // This card requests and shows the HD copy, so 4K requests are left to `/requests`
        let cancellable = details
            .cancellable(viewer.user.map(|u| u.id), viewer.admin)
            .into_iter()
            .filter(|c| !c.is4k && (can_unsend(data, media, false) || !c.sent))
            .collect();

        Ok(Self {
            embed,
            media,
            title,
            index,
            count: results.len(),
            requestable,
            linked,
            open,
            picked: Vec::new(),
            profiles: profiles.clone(),
            profile: None,
            can_bypass,
            cancellable,
            tvdb_id: details.media_info.as_ref().and_then(|m| m.tvdb_id),
            watch: details.media_info.and_then(|m| m.media_url),
        })
    }

    /// Requests the movie, or the picked seasons, as Seerr user `user` with the picked quality,
    /// skipping their quota if `bypass` and they may. Returns what to tell the invoker, if
    /// anything (a request made shows on the card), and whether the request was made.
    async fn request(&self, data: &Data, user: u32, bypass: bool) -> (Option<String>, bool) {
        let options = seerr::Options {
            seasons: if self.picked.is_empty() {
                &self.open
            } else {
                &self.picked
            },
            profile: self.profile,
            ignore_quota: self.can_bypass && bypass,
        };
        match data
            .seerr
            .request(&data.http, user, self.media, &options)
            .await
        {
            Ok(Requested::Made) => (None, true),
            Ok(Requested::Refused(message)) => (Some(message), false),
            Err(e) => {
                warn!("Requesting {}: {e:#}", self.media);
                (Some(UNREACHABLE.into()), false)
            }
        }
    }

    /// Cancels [`Self::cancellable`], returning what to tell the invoker and whether anything
    /// changed
    async fn cancel(&self, data: &Data) -> (String, bool) {
        cancel(
            data,
            self.media,
            self.tvdb_id,
            &self.title,
            &self.cancellable,
        )
        .await
    }

    /// The season menu for shows with more than one open season and the quality menu, then
    /// ◀ Request, Request bypassing the quota, Cancel request, ▶ and a link to Jellyfin once it's
    /// there
    fn components(&self, id: &str) -> Vec<CreateActionRow> {
        let button = |suffix: &str| CreateButton::new(format!("{id}-{suffix}"));
        let mut rows = Vec::new();

        if self.requestable && self.linked && self.open.len() > 1 {
            let options = self
                .open
                .iter()
                .take(MAX_OPTIONS)
                .map(|&n| {
                    CreateSelectMenuOption::new(format!("Season {n}"), n.to_string())
                        .default_selection(self.picked.contains(&n))
                })
                .collect::<Vec<_>>();
            #[allow(clippy::cast_possible_truncation)]
            let max = options.len() as u8;
            rows.push(CreateActionRow::SelectMenu(
                CreateSelectMenu::new(
                    format!("{id}-seasons"),
                    CreateSelectMenuKind::String { options },
                )
                .placeholder("All seasons")
                .min_values(0)
                .max_values(max),
            ));
        }

        if self.requestable && self.linked && !self.profiles.is_empty() {
            let options = self
                .profiles
                .iter()
                .take(MAX_OPTIONS)
                .map(|p| {
                    CreateSelectMenuOption::new(&p.name, p.id.to_string())
                        .default_selection(self.profile == Some(p.id))
                })
                .collect();
            rows.push(CreateActionRow::SelectMenu(
                CreateSelectMenu::new(
                    format!("{id}-quality"),
                    CreateSelectMenuKind::String { options },
                )
                .placeholder("Default quality")
                .min_values(0)
                .max_values(1),
            ));
        }

        let mut buttons = Vec::new();
        if self.count > 1 {
            buttons.push(
                button("prev")
                    .emoji('\u{25c0}')
                    .style(ButtonStyle::Secondary),
            );
        }
        if self.requestable {
            buttons.push(
                button("request")
                    .label(self.request_label())
                    .style(ButtonStyle::Primary)
                    .disabled(!self.linked),
            );
        }
        if self.requestable && self.can_bypass {
            buttons.push(
                button("request-bypass")
                    .label("Request and bypass quota")
                    .style(ButtonStyle::Secondary),
            );
        }
        if !self.cancellable.is_empty() {
            buttons.push(
                button("cancel")
                    .label("Cancel request")
                    .style(ButtonStyle::Danger),
            );
        }
        if self.count > 1 {
            buttons.push(
                button("next")
                    .emoji('\u{25b6}')
                    .style(ButtonStyle::Secondary),
            );
        }
        if let Some(url) = &self.watch {
            buttons.push(CreateButton::new_link(url).label("Jellyfin"));
        }
        // Discord allows at most 5 buttons per row
        rows.extend(
            buttons
                .chunks(5)
                .map(|row| CreateActionRow::Buttons(row.to_vec())),
        );
        rows
    }

    /// "Request", "Request season 2", "Request 3 seasons" or "Request all seasons"
    fn request_label(&self) -> String {
        if let Media::Movie(_) = self.media {
            return "Request".into();
        }
        let seasons = if self.picked.is_empty() {
            &self.open
        } else {
            &self.picked
        };
        match seasons.as_slice() {
            [n] => format!("Request season {n}"),
            _ if self.picked.is_empty() => "Request all seasons".into(),
            many => format!("Request {} seasons", many.len()),
        }
    }
}

/// Cancels `cancellable`, requests for `media` titled `title`: takes approved ones back out of
/// Radarr or Sonarr, downloads included, then deletes them from Seerr. The caller has already
/// checked who may cancel what, so Seerr is asked as its admin. Returns what to tell the invoker,
/// and whether anything changed.
pub(super) async fn cancel(
    data: &Data,
    media: Media,
    tvdb_id: Option<u32>,
    title: &str,
    cancellable: &[Cancellable],
) -> (String, bool) {
    let mut changed = false;
    match try_cancel(data, media, tvdb_id, cancellable, &mut changed).await {
        Ok(None) => {
            let n = cancellable.len();
            let what = if n == 1 {
                "the request".to_owned()
            } else {
                format!("{n} requests")
            };
            let extra = if cancellable.iter().any(|c| c.sent) {
                ", downloads included"
            } else {
                ""
            };
            let title = escape(title);
            (format!("Cancelled {what} for **{title}**{extra}"), changed)
        }
        Ok(Some(refused)) => (refused, changed),
        Err(e) => {
            warn!("Cancelling requests for {media}: {e:#}");
            (
                "Couldn't cancel it, try again in a bit \u{1fae4}".into(),
                changed,
            )
        }
    }
}

/// The steps of [`cancel`]. Radarr or Sonarr goes first, so if that fails the request is still in
/// Seerr to try again. `Some` is a refusal to tell the invoker: Seerr's, or because Radarr or
/// Sonarr already has some of it downloaded, which Seerr may not know yet.
async fn try_cancel(
    data: &Data,
    media: Media,
    tvdb_id: Option<u32>,
    cancellable: &[Cancellable],
    changed: &mut bool,
) -> Result<Option<String>> {
    // The HD and 4K copies each have their own Radarr or Sonarr
    for is4k in [false, true] {
        let sent: Vec<&Cancellable> = cancellable
            .iter()
            .filter(|c| c.sent && c.is4k == is4k)
            .collect();
        if sent.is_empty() {
            continue;
        }
        let name = arr_name(media, is4k);
        let arr = arr(data, media, is4k).with_context(|| format!("{name} isn't configured"))?;
        let removed = match media {
            Media::Movie(tmdb_id) => arr.remove_movie(&data.http, tmdb_id).await,
            Media::Tv(_) => {
                let tvdb_id = tvdb_id.context("Seerr has no TVDB id")?;
                let mut seasons: Vec<u32> = sent
                    .iter()
                    .flat_map(|c| c.seasons.iter().copied())
                    .collect();
                seasons.sort_unstable();
                seasons.dedup();
                arr.remove_seasons(&data.http, tvdb_id, &seasons).await
            }
        }
        .context(name)?;
        if removed == Removed::Downloaded {
            return Ok(Some(
                "It's already been downloaded, so it can't be cancelled".into(),
            ));
        }
        *changed = true;
    }

    for c in cancellable {
        match data.seerr.cancel(&data.http, None, c.id).await? {
            Changed::Done => *changed = true,
            Changed::Refused(message) => return Ok(Some(message)),
        }
    }

    Ok(None)
}

/// The Radarr or Sonarr that `media`'s HD or 4K copy goes to, if it's configured
const fn arr(data: &Data, media: Media, is4k: bool) -> Option<&Arr> {
    match (media, is4k) {
        (Media::Movie(_), false) => data.radarr.as_ref(),
        (Media::Tv(_), false) => data.sonarr.as_ref(),
        (Media::Movie(_), true) => data.radarr_4k.as_ref(),
        (Media::Tv(_), true) => data.sonarr_4k.as_ref(),
    }
}

/// [`arr`]'s name, for errors
const fn arr_name(media: Media, is4k: bool) -> &'static str {
    match (media, is4k) {
        (Media::Movie(_), false) => "Radarr",
        (Media::Tv(_), false) => "Sonarr",
        (Media::Movie(_), true) => "4K Radarr",
        (Media::Tv(_), true) => "4K Sonarr",
    }
}

/// Whether Radarr or Sonarr is configured for `media`'s HD or 4K copy, so approved requests for
/// it can be taken back out
pub(super) const fn can_unsend(data: &Data, media: Media, is4k: bool) -> bool {
    arr(data, media, is4k).is_some()
}

/// How a movie or season's status reads on the card
pub(super) const fn status(s: Status, auto_approved: bool) -> &'static str {
    match s {
        Status::Missing => "Not requested",
        Status::Pending => "Requested, waiting for approval",
        Status::Processing if auto_approved => "Automatically approved, not downloaded",
        Status::Processing => "Approved, not downloaded",
        Status::PartiallyAvailable => "Partly downloaded",
        Status::Available => "Downloaded",
        Status::Blocklisted => "Blocklisted",
    }
}

/// One line per status, available first: "Downloaded: 1–3, 5". Each season is its number,
/// status, and whether it was approved automatically.
fn season_lines(seasons: &[(u32, Status, bool)]) -> String {
    if seasons.is_empty() {
        return "None yet".into();
    }
    [
        Status::Available,
        Status::PartiallyAvailable,
        Status::Processing,
        Status::Pending,
        Status::Missing,
        Status::Blocklisted,
    ]
    .into_iter()
    .flat_map(|s| [(s, true), (s, false)])
    .map(|(s, auto)| (status(s, auto), s, auto))
    // Only approved seasons differ by `auto`, so the rest would show twice
    .filter(|&(_, s, auto)| s == Status::Processing || !auto)
    .filter_map(|(label, s, auto)| {
        let numbers: Vec<u32> = seasons
            .iter()
            .filter(|&&(_, status, a)| status == s && (s != Status::Processing || a == auto))
            .map(|&(n, ..)| n)
            .collect();
        (!numbers.is_empty()).then(|| format!("{label}: {}", ranges(&numbers)))
    })
    .collect::<Vec<_>>()
    .join("\n")
}

/// Ascending numbers with runs collapsed: `[1, 2, 3, 5]` -> "1–3, 5"
pub fn ranges(numbers: &[u32]) -> String {
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for &n in numbers {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == n => *end = n,
            _ => runs.push((n, n)),
        }
    }
    runs.iter()
        .map(|&(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}\u{2013}{end}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `s` cut at a word to at most `max` characters, with "…" if anything was cut
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut: String = s.chars().take(max - 1).collect();
    let cut = cut
        .rfind(char::is_whitespace)
        .map_or(cut.as_str(), |i| &cut[..i]);
    format!("{}\u{2026}", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn season_ranges() {
        assert_eq!(ranges(&[1, 2, 3, 5]), "1\u{2013}3, 5");
        assert_eq!(ranges(&[4]), "4");
        assert_eq!(ranges(&[1, 3, 4]), "1, 3\u{2013}4");
    }

    #[test]
    fn lines() {
        let seasons = [
            (1, Status::Available, false),
            (2, Status::Available, false),
            (3, Status::Processing, true),
            (4, Status::Processing, false),
            (5, Status::Pending, false),
            (6, Status::Missing, false),
        ];
        assert_eq!(
            season_lines(&seasons),
            "Downloaded: 1\u{2013}2\nAutomatically approved, not downloaded: 3\n\
             Approved, not downloaded: 4\nRequested, waiting for approval: 5\nNot requested: 6"
        );
    }

    #[test]
    fn truncated() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a few words here", 8), "a few\u{2026}");
    }
}
