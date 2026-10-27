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
//!
//! Users Seerr lets request 4K can also request a 4K version of what's already downloaded in
//! HD, with a button switching the card between the two.

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

use super::{escape, is_admin, remove_buttons, requests::profiles};
use crate::{
    Context, Data,
    arr::{Arr, Removed},
    seerr::{
        self, Cancellable, Changed, Details, Media, Profile, Profiles, Quotas, Request,
        RequestStatus, Requested, Requester, SearchResult, Status,
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
    let mut card = Card::build(data, &results, 0, &viewer, false).await?;
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
            Some(choice @ ("-seasons" | "-quality" | "-switch")) => {
                // Only the menus and buttons change, so answer with them straight away
                let values = match &press.data.kind {
                    ComponentInteractionDataKind::StringSelect { values } => values.as_slice(),
                    _ => &[],
                };
                let numbers = values.iter().filter_map(|v| v.parse().ok());
                match choice {
                    "-seasons" => card.picked = numbers.collect(),
                    "-quality" => card.profile = numbers.into_iter().next(),
                    _ => card.switch(),
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
        // The same result stays on the version it was on
        let is4k = index == card.index && card.is4k;
        match Card::build(data, &results, index, &viewer, is4k).await {
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
    /// The default HD and 4K servers' quality profiles. A server missing here can't be
    /// requested from.
    profiles: Vec<Profiles>,
}

impl Viewer {
    /// Fetches the user's quotas and the servers' quality profiles
    async fn load(data: &Data, user: Option<Requester>, admin: bool) -> Self {
        let Some(user) = user else {
            return Self {
                user,
                admin,
                quotas: None,
                profiles: Vec::new(),
            };
        };
        let (quotas, profiles) = tokio::join!(Self::quotas(data, user), profiles(data));
        Self {
            user: Some(user),
            admin,
            quotas,
            profiles,
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

    /// The default HD or 4K server's profiles for `media`, if there's such a server
    fn server(&self, media: Media, is4k: bool) -> Option<&Profiles> {
        let movies = matches!(media, Media::Movie(_));
        self.profiles
            .iter()
            .find(|p| p.movies == movies && p.is4k == is4k)
    }
}

/// What the card can do with one version of its movie or show, HD or 4K
#[derive(Default)]
struct Version {
    /// Whether it can be requested
    requestable: bool,
    /// Seasons that can be requested, shows only
    open: Vec<u32>,
    /// Quality profiles to pick from, empty if the invoker may not
    profiles: Vec<Profile>,
    /// Requests the invoker may cancel. Ones already sent to Radarr or Sonarr are left out if
    /// the bot can't reach it.
    cancellable: Vec<Cancellable>,
}

impl Version {
    /// Whether there's anything to do with it
    const fn active(&self) -> bool {
        self.requestable || !self.cancellable.is_empty()
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
    /// Whether the invoker is linked to a Seerr user, and so can request
    linked: bool,
    /// The HD and 4K versions
    versions: [Version; 2],
    /// Which version the menus and buttons are for
    is4k: bool,
    /// Seasons picked in the menu. Empty means all of the version's open seasons.
    picked: Vec<u32>,
    /// Profile picked in the menu, `None` for Seerr's default
    profile: Option<u32>,
    /// Whether the invoker has a quota for this kind of media and may bypass it
    can_bypass: bool,
    /// TVDB id, shows only, once Seerr knows of it
    tvdb_id: Option<u32>,
    /// Jellyfin page, the HD version's if there's one, once it's available
    watch: Option<String>,
}

impl Card {
    /// Fetches result `index` (wrapping around) from Seerr and renders it, for the 4K version
    /// if `is4k` and there's anything to do with it
    async fn build(
        data: &Data,
        results: &[Media],
        index: usize,
        viewer: &Viewer,
        is4k: bool,
    ) -> Result<Self> {
        let index = index % results.len();
        let media = results[index];
        let details = data.seerr.details(&data.http, media).await?;
        let title = seerr::with_year(&details.title, details.release_date.as_deref());
        let tvdb_id = details.media_info.as_ref().and_then(|m| m.tvdb_id);

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

        // 4K is only an upgrade: it can be requested once the HD version is downloaded
        let can_4k = viewer.user.is_some_and(|u| u.can_request_4k(media))
            && viewer.server(media, true).is_some();
        let blocked = details.status() == Status::Blocklisted;
        let numbers: Vec<u32> = details.season_numbers().collect();
        // Each season's status in HD and 4K, or the movie's as season `None`
        let keys: Vec<Option<u32>> = match media {
            Media::Movie(_) => vec![None],
            Media::Tv(_) => numbers.iter().copied().map(Some).collect(),
        };
        let status_of = |n: Option<u32>, is4k| {
            n.map_or_else(
                || details.status_of(is4k),
                |n| details.season_status_of(n, is4k),
            )
        };
        let processing = |is4k| {
            keys.iter()
                .any(|&n| status_of(n, is4k) == Status::Processing)
        };
        let started: [Started; 2] = tokio::join!(
            started_if(data, media, tvdb_id, false, processing(false)),
            started_if(data, media, tvdb_id, true, processing(true)),
        )
        .into();
        let label = |n: Option<u32>, is4k: bool| {
            status(
                status_of(n, is4k),
                details.auto_approved(n, is4k),
                started[usize::from(is4k)].of(n),
            )
        };

        let (kind, open) = match media {
            Media::Movie(_) => {
                let (hd, uhd) = (status_of(None, false), status_of(None, true));
                let (hd_label, uhd_label) = (label(None, false), label(None, true));
                let text = match (hd, uhd) {
                    (_, Status::Missing) => hd_label.to_owned(),
                    (Status::Missing, _) => format!("{uhd_label} in 4K"),
                    _ if hd_label == uhd_label => format!("{hd_label} in HD and 4K"),
                    _ => format!("{hd_label}\n{uhd_label} in 4K"),
                };
                embed = embed.field("Status", text, false);
                let open_4k = can_4k && hd == Status::Available && uhd == Status::Missing;
                (
                    "Movie",
                    [hd == Status::Missing, open_4k].map(|open| (open, Vec::new())),
                )
            }
            Media::Tv(_) => {
                let lines = |is4k| -> Vec<(u32, &'static str, Status)> {
                    numbers
                        .iter()
                        .map(|&n| (n, label(Some(n), is4k), status_of(Some(n), is4k)))
                        .collect()
                };
                embed = embed.field("Seasons", season_lines(&lines(false)), false);
                let known_4k: Vec<_> = lines(true)
                    .into_iter()
                    .filter(|&(.., s)| s != Status::Missing)
                    .collect();
                if !known_4k.is_empty() {
                    embed = embed.field("4K seasons", season_lines(&known_4k), false);
                }
                let open = |is4k| -> Vec<u32> {
                    numbers
                        .iter()
                        .copied()
                        .filter(|&n| {
                            status_of(Some(n), is4k) == Status::Missing
                                && (!is4k
                                    || can_4k && status_of(Some(n), false) == Status::Available)
                        })
                        .collect()
                };
                let (open_hd, open_4k) = (open(false), open(true));
                (
                    "TV show",
                    [
                        (!blocked && !open_hd.is_empty(), open_hd),
                        (!blocked && !open_4k.is_empty(), open_4k),
                    ],
                )
            }
        };

        let can_pick = viewer.user.is_some_and(Requester::can_pick_profile);
        let cancellable = details.cancellable(viewer.user.map(|u| u.id), viewer.admin);
        let versions = open.map(|(requestable, open)| Version {
            requestable,
            open,
            ..Version::default()
        });
        let [hd, uhd] = versions;
        let versions = [(hd, false), (uhd, true)].map(|(version, is4k)| Version {
            profiles: viewer
                .server(media, is4k)
                .filter(|_| can_pick)
                .map(|p| p.list.clone())
                .unwrap_or_default(),
            cancellable: cancellable
                .iter()
                .filter(|c| c.is4k == is4k && (can_unsend(data, media, is4k) || !c.sent))
                .cloned()
                .collect(),
            ..version
        });
        let requestable = versions.iter().any(|v| v.requestable);

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

        let is4k = if versions[usize::from(is4k)].active() {
            is4k
        } else {
            !versions[0].active() && versions[1].active()
        };
        let watch = details
            .media_info
            .as_ref()
            .and_then(|m| m.jellyfin_url(false).or_else(|| m.jellyfin_url(true)))
            .map(str::to_owned);
        Ok(Self {
            embed,
            media,
            title,
            index,
            count: results.len(),
            linked,
            versions,
            is4k,
            picked: Vec::new(),
            profile: None,
            can_bypass: limited && viewer.user.is_some_and(Requester::can_bypass_quota),
            tvdb_id,
            watch,
        })
    }

    /// The version the menus and buttons are for
    const fn version(&self) -> &Version {
        &self.versions[self.is4k as usize]
    }

    /// The seasons a request would ask for: the picked ones, or all open ones
    fn seasons(&self) -> &[u32] {
        if self.picked.is_empty() {
            &self.version().open
        } else {
            &self.picked
        }
    }

    /// Switches the menus and buttons to the other version, clearing what was picked
    fn switch(&mut self) {
        self.is4k = !self.is4k;
        self.picked.clear();
        self.profile = None;
    }

    /// Requests the movie, or the picked seasons, as Seerr user `user` with the picked quality,
    /// skipping their quota if `bypass` and they may. Returns what to tell the invoker, if
    /// anything (a request made shows on the card), and whether the request was made.
    async fn request(&self, data: &Data, user: u32, bypass: bool) -> (Option<String>, bool) {
        let options = seerr::Options {
            seasons: self.seasons(),
            profile: self.profile,
            ignore_quota: self.can_bypass && bypass,
            is4k: self.is4k,
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

    /// Cancels the version's cancellable requests, returning what to tell the invoker and
    /// whether anything changed
    async fn cancel(&self, data: &Data) -> (String, bool) {
        cancel(
            data,
            self.media,
            self.tvdb_id,
            &self.title,
            &self.version().cancellable,
        )
        .await
    }

    /// The season menu for shows with more than one open season and the quality menu, then
    /// ◀ Request, Request bypassing the quota, Cancel request, Switch to 4K or HD, ▶ and a link
    /// to Jellyfin once it's there
    fn components(&self, id: &str) -> Vec<CreateActionRow> {
        let button = |suffix: &str| CreateButton::new(format!("{id}-{suffix}"));
        let version = self.version();
        let mut rows = Vec::new();

        if version.requestable && self.linked && version.open.len() > 1 {
            let options = version
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

        if version.requestable && self.linked && !version.profiles.is_empty() {
            let options = version
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
        if version.requestable {
            buttons.push(
                button("request")
                    .label(self.request_label())
                    .style(ButtonStyle::Primary)
                    .disabled(!self.linked),
            );
        }
        if version.requestable && self.can_bypass {
            buttons.push(
                button("request-bypass")
                    .label("Request and bypass quota")
                    .style(ButtonStyle::Secondary),
            );
        }
        if !version.cancellable.is_empty() {
            let label = if self.is4k {
                "Cancel 4K request"
            } else {
                "Cancel request"
            };
            buttons.push(button("cancel").label(label).style(ButtonStyle::Danger));
        }
        if self.versions.iter().all(Version::active) {
            let label = if self.is4k {
                "Switch to HD"
            } else {
                "Switch to 4K"
            };
            buttons.push(button("switch").label(label).style(ButtonStyle::Secondary));
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

    /// "Request", "Request season 2", "Request 3 seasons" or "Request all seasons", with " in 4K"
    /// for the 4K version
    fn request_label(&self) -> String {
        let label = match (self.media, self.seasons()) {
            (Media::Movie(_), _) => "Request".into(),
            (_, [n]) => format!("Request season {n}"),
            _ if self.picked.is_empty() => "Request all seasons".into(),
            (_, many) => format!("Request {} seasons", many.len()),
        };
        if self.is4k {
            format!("{label} in 4K")
        } else {
            label
        }
    }
}

/// Which downloads of a movie or show have started in Radarr or Sonarr
#[derive(Debug, Default)]
pub(super) struct Started {
    movie: bool,
    seasons: Vec<u32>,
}

impl Started {
    /// Whether season `n`'s download has, or with `None` the movie's
    fn of(&self, n: Option<u32>) -> bool {
        n.map_or(self.movie, |n| self.seasons.contains(&n))
    }
}

/// Which downloads of `media`'s HD or 4K version have started, if `check`. Best effort: without
/// Radarr or Sonarr, or if it can't be reached, none have.
async fn started_if(
    data: &Data,
    media: Media,
    tvdb_id: Option<u32>,
    is4k: bool,
    check: bool,
) -> Started {
    let Some(arr) = arr(data, media, is4k).filter(|_| check) else {
        return Started::default();
    };
    let started = match (media, tvdb_id) {
        (Media::Movie(tmdb_id), _) => {
            arr.movie_started(&data.http, tmdb_id)
                .await
                .map(|movie| Started {
                    movie,
                    seasons: Vec::new(),
                })
        }
        (Media::Tv(_), Some(tvdb_id)) => {
            arr.seasons_started(&data.http, tvdb_id)
                .await
                .map(|seasons| Started {
                    movie: false,
                    seasons,
                })
        }
        (Media::Tv(_), None) => return Started::default(),
    };
    started
        .inspect_err(|e| {
            warn!(
                "Checking {}'s downloads of {media}: {e:#}",
                arr_name(media, is4k)
            )
        })
        .unwrap_or_default()
}

/// Whether a download of what `request` asked for has started, if it's been sent to Radarr or
/// Sonarr and isn't downloaded yet. `details` is the media's.
pub(super) async fn request_started(data: &Data, request: &Request, details: &Details) -> bool {
    let Some(media) = request.media() else {
        return false;
    };
    let check = request.status == RequestStatus::Approved
        && details.request_status(request) == Status::Processing;
    let tvdb_id = details.media_info.as_ref().and_then(|m| m.tvdb_id);
    let started = started_if(data, media, tvdb_id, request.is4k, check).await;
    if request.seasons.is_empty() {
        started.movie
    } else {
        request
            .season_numbers()
            .iter()
            .any(|n| started.seasons.contains(n))
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

/// How a movie or season's status reads on the card. `started` is whether an approved one's
/// download has.
pub(super) const fn status(s: Status, auto_approved: bool, started: bool) -> &'static str {
    match s {
        Status::Missing => "Not requested",
        Status::Pending => "Requested, waiting for approval",
        Status::Processing => match (auto_approved, started) {
            (true, true) => "Automatically approved, download started",
            (true, false) => "Automatically approved, download queued",
            (false, true) => "Approved, download started",
            (false, false) => "Approved, download queued",
        },
        Status::PartiallyAvailable => "Partly downloaded",
        Status::Available => "Downloaded",
        Status::Blocklisted => "Blocklisted",
    }
}

/// One line per label, available first: "Downloaded: 1–3, 5". Each season is its number, how
/// its status reads, and the status.
fn season_lines(seasons: &[(u32, &str, Status)]) -> String {
    if seasons.is_empty() {
        return "None yet".into();
    }
    let rank = |s: Status| match s {
        Status::Available => 0,
        Status::PartiallyAvailable => 1,
        Status::Processing => 2,
        Status::Pending => 3,
        Status::Missing => 4,
        Status::Blocklisted => 5,
    };
    let mut lines: Vec<(u8, &str, Vec<u32>)> = Vec::new();
    for &(n, label, s) in seasons {
        match lines.iter_mut().find(|(_, l, _)| *l == label) {
            Some((.., numbers)) => numbers.push(n),
            None => lines.push((rank(s), label, vec![n])),
        }
    }
    // Stable, so labels of the same status keep the order they first came in
    lines.sort_by_key(|&(rank, ..)| rank);
    lines
        .iter()
        .map(|(_, label, numbers)| format!("{label}: {}", ranges(numbers)))
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
        let season = |n, s, auto, started| (n, status(s, auto, started), s);
        let seasons = [
            season(6, Status::Missing, false, false),
            season(1, Status::Available, false, false),
            season(2, Status::Available, false, false),
            season(3, Status::Processing, true, false),
            season(4, Status::Processing, false, true),
            season(5, Status::Pending, false, false),
            season(7, Status::Processing, true, false),
        ];
        assert_eq!(
            season_lines(&seasons),
            "Downloaded: 1\u{2013}2\nAutomatically approved, download queued: 3, 7\n\
             Approved, download started: 4\nRequested, waiting for approval: 5\n\
             Not requested: 6"
        );
    }

    #[test]
    fn truncated() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("a few words here", 8), "a few\u{2026}");
    }
}
