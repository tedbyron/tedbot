//! `/requests`: recent Seerr requests, the invoker's own or, for admins, everyone's.
//!
//! Replies ephemerally with a page of requests, newest first, and a menu to open one as a card
//! with its poster, seasons and status. Requesters can cancel their own from the card while
//! nothing's downloaded. Admins also get Approve and Decline on pending requests, and can change
//! their quality and seasons before approving. Buttons work until [`NAV_TIMEOUT`] passes with no
//! press, then they're removed.
//!
//! New requests are also posted in `seerr.webhook.requests_channels` as the same card, made for
//! admins ([`post_card`]). Those buttons work for as long as the post is there ([`post_press`]).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result};
use chrono::DateTime;
use futures::future::join_all;
use poise::{
    ChoiceParameter, CreateReply, command,
    serenity_prelude::{
        self as serenity, ActionRowComponent, Builder as _, Button, ButtonKind, ButtonStyle,
        Channel, ChannelId, ComponentInteraction, ComponentInteractionCollector,
        ComponentInteractionDataKind, CreateActionRow, CreateButton, CreateEmbed,
        CreateEmbedFooter, CreateInteractionResponse, CreateInteractionResponseFollowup,
        CreateInteractionResponseMessage, CreateSelectMenu, CreateSelectMenuKind,
        CreateSelectMenuOption, EditInteractionResponse, EditMessage, Embed, GuildId, Http,
        Mentionable as _, Message, MessageId, UserId,
    },
};
use tracing::warn;

use super::{
    escape, has_admin_role, is_admin, remove_buttons,
    request::{can_unsend, cancel, ranges, request_download, status, truncate},
};
use crate::{
    Context, Data,
    arr::Download,
    seerr::{
        self, Cancellable, Changed, Details, Media, Profiles, Request, RequestStatus, RequestedBy,
        Requester, Status,
    },
};

/// How long buttons keep working after the last press. An ephemeral reply can only be edited with
/// an interaction token, which expires after 15 minutes, so this stays under that to remove the
/// buttons at the end.
const NAV_TIMEOUT: Duration = Duration::from_secs(14 * 60);
/// TMDB image URL prefix for posters, 342 pixels wide
const POSTER_URL: &str = "https://image.tmdb.org/t/p/w342";
/// Requests per page of the list
const PAGE_SIZE: usize = 10;
/// Discord allows at most 25 options per select menu
const MAX_OPTIONS: usize = 25;
/// Longest gap between one request and the next for them to count as one batch, in seconds
const BATCH_GAP: i64 = 60 * 60;
/// Most pending requests from one user looked through for a batch
const BATCH_MAX: usize = 100;
/// Most requests in a batch listed by name when confirming
const BATCH_SHOWN: usize = 25;
/// Reply when Seerr can't be reached
const UNREACHABLE: &str = "Couldn't reach Seerr, try again in a bit \u{1fae4}";
/// Start of custom ids on request posts, followed by the request id: `request-post:12-approve`
const POST: &str = "request-post:";

/// Which requests to list
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ChoiceParameter)]
pub enum Filter {
    #[default]
    #[name = "All"]
    All,
    #[name = "Waiting for approval"]
    Pending,
    #[name = "Approved, not downloaded"]
    Processing,
    #[name = "Downloaded"]
    Available,
    #[name = "Failed"]
    Failed,
}

impl Filter {
    /// Seerr's name for it
    const fn seerr(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Pending => "pending",
            Self::Processing => "processing",
            Self::Available => "available",
            Self::Failed => "failed",
        }
    }
}

/// See your recent requests
#[command(slash_command)]
pub async fn requests(
    ctx: Context<'_>,
    #[description = "Only requests that are"] status: Option<Filter>,
) -> Result<()> {
    ctx.defer_ephemeral().await?;

    let data = ctx.data();
    let (user, admin) = tokio::join!(
        data.seerr.user_for(&data.http, ctx.author().id),
        is_admin(ctx),
    );
    let user = user?;
    if user.is_none() && !admin {
        ctx.say(
            "To see your requests here, add your Discord user ID to your Seerr notification \
             settings",
        )
        .await?;
        return Ok(());
    }

    let id = ctx.id().to_string();
    let viewer = Viewer::load(ctx, user, admin, status.unwrap_or_default()).await;
    let mut screen = Screen::build(data, &viewer, View::List { page: 0 }, &id).await?;
    if screen.total == 0 {
        let what = match viewer.filter {
            Filter::All => "",
            Filter::Pending => " waiting for approval",
            Filter::Processing => " approved and not downloaded",
            Filter::Available => " downloaded",
            Filter::Failed => " that failed",
        };
        ctx.say(format!("No requests{what}")).await?;
        return Ok(());
    }

    let reply = CreateReply::default()
        .components(std::mem::take(&mut screen.components))
        .embed(std::mem::take(&mut screen.embed));
    let handle = ctx.send(reply).await?;
    let message = handle.message().await?.id;
    let mut last = None;

    while let Some(press) = ComponentInteractionCollector::new(ctx)
        .message_id(message)
        .timeout(NAV_TIMEOUT)
        .await
    {
        let values: Vec<&str> = match &press.data.kind {
            ComponentInteractionDataKind::StringSelect { values } => {
                values.iter().map(String::as_str).collect()
            }
            _ => Vec::new(),
        };
        let action = match press.data.custom_id.strip_prefix(id.as_str()) {
            Some("-prev") => Action::Go(screen.view.step(false, screen.total)),
            Some("-next") => Action::Go(screen.view.step(true, screen.total)),
            Some("-refresh") => Action::Go(screen.view),
            Some("-back") => Action::Go(View::List {
                page: screen.view.index() / PAGE_SIZE,
            }),
            Some("-open") => match values.first().and_then(|v| v.parse().ok()) {
                Some(index) => Action::Go(View::Card { index, id: None }),
                None => continue,
            },
            Some("-batch") => match (&screen.card, screen.view) {
                (Some(card), View::Card { index, .. }) => Action::Go(View::Batch {
                    index,
                    id: card.request.id,
                }),
                _ => continue,
            },
            Some("-batch-back") => match screen.view {
                View::Batch { index, id } => Action::Go(View::Card {
                    index,
                    id: Some(id),
                }),
                _ => continue,
            },
            Some("-approve-all") => Action::ApproveAll,
            Some("-approve") => Action::Review { approve: true },
            Some("-decline") => Action::Review { approve: false },
            Some("-cancel") => Action::Cancel,
            Some("-quality") => Action::Quality(values.first().and_then(|v| v.parse().ok())),
            Some("-seasons") => {
                Action::Seasons(values.iter().filter_map(|v| v.parse().ok()).collect())
            }
            _ => continue,
        };

        // Acknowledge first: Seerr looks things up on TMDB, which may take longer than the 3
        // seconds Discord allows before an interaction fails
        press
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await?;
        let token = press.token;
        last = Some(token.clone());

        let (view, mut notice) = match (action, &screen.card) {
            (Action::Go(view), _) => (view, None),
            (Action::ApproveAll, _) => match screen.view {
                View::Batch { index, id } if viewer.admin => (
                    View::Card {
                        index,
                        id: Some(id),
                    },
                    approve_all(data, &screen.batch).await,
                ),
                _ => continue,
            },
            (action, Some(card)) => (screen.view, card.act(data, &viewer, action).await),
            (_, None) => continue,
        };
        match Screen::build(data, &viewer, view, &id).await {
            Ok(next) => {
                screen = next;
                EditInteractionResponse::new()
                    .components(std::mem::take(&mut screen.components))
                    .embed(std::mem::take(&mut screen.embed))
                    .execute(ctx, &token)
                    .await?;
            }
            Err(e) => {
                warn!("Loading requests: {e:#}");
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

/// What a press asks for
enum Action {
    /// Show another page or card
    Go(View),
    /// Approve or decline the request on the card
    Review { approve: bool },
    /// Cancel the request on the card
    Cancel,
    /// Change the card's request to this quality profile, `None` for the default
    Quality(Option<u32>),
    /// Change the card's request to these seasons
    Seasons(Vec<u32>),
    /// Approve the batch being confirmed
    ApproveAll,
}

/// What's showing
#[derive(Clone, Copy)]
enum View {
    /// A page of the list
    List { page: usize },
    /// One request by position in the list. With `id`, that request even if the list has moved
    /// since, e.g. because it was approved while only pending ones are listed.
    Card { index: usize, id: Option<u32> },
    /// Confirming approval of request `id`'s batch: its requester's pending requests, each made
    /// within [`BATCH_GAP`] of the one before. `index` is the card's, to go back to.
    Batch { index: usize, id: u32 },
}

impl View {
    /// The page or position
    const fn index(self) -> usize {
        match self {
            Self::List { page } => page * PAGE_SIZE,
            Self::Card { index, .. } | Self::Batch { index, .. } => index,
        }
    }

    /// The next page or request, or the previous one, wrapping around. `total` is the number of
    /// requests listed.
    const fn step(self, forward: bool, total: usize) -> Self {
        let (n, count) = match self {
            Self::List { page } => (page, total.div_ceil(PAGE_SIZE)),
            Self::Card { index, .. } | Self::Batch { index, .. } => (index, total),
        };
        let count = if count == 0 { 1 } else { count };
        let n = if forward {
            (n + 1) % count
        } else {
            (n + count - 1) % count
        };
        match self {
            Self::List { .. } => Self::List { page: n },
            Self::Card { .. } | Self::Batch { .. } => Self::Card { index: n, id: None },
        }
    }
}

/// Who's looking, and what they may do
struct Viewer {
    /// The invoker's Seerr user, `None` if they haven't linked their Discord id
    user: Option<Requester>,
    /// Whether the invoker is a bot admin, who sees everyone's requests and may act on them
    admin: bool,
    filter: Filter,
    /// The default HD and 4K servers' movie and show quality profiles, admins only
    profiles: Vec<Profiles>,
    /// The server the command was used in, whose members show by their Discord account
    guild: Option<GuildId>,
    /// For looking up members
    discord: Arc<Http>,
    /// Seerr users' Discord accounts, if they're in [`Self::guild`], as looked up so far
    members: Mutex<HashMap<u32, Option<UserId>>>,
}

impl Viewer {
    /// Fetches the quality profiles for admins. Without them, there's no quality menu.
    async fn load(ctx: Context<'_>, user: Option<Requester>, admin: bool, filter: Filter) -> Self {
        let profiles = if admin {
            profiles(ctx.data()).await
        } else {
            Vec::new()
        };
        Self {
            user,
            admin,
            filter,
            profiles,
            guild: ctx.guild_id(),
            discord: ctx.serenity_context().http.clone(),
            members: Mutex::default(),
        }
    }

    /// An admin's view, for request posts in `guild`'s channels, which only admins can act on
    async fn post(data: &Data, discord: Arc<Http>, guild: Option<GuildId>) -> Self {
        Self {
            user: None,
            admin: true,
            filter: Filter::All,
            profiles: profiles(data).await,
            guild,
            discord,
            members: Mutex::default(),
        }
    }

    /// How to show Seerr user `by`: a mention if they linked a Discord account in Seerr and are
    /// in this server, else their Seerr name. A mention in an embed never pings.
    async fn name(&self, data: &Data, by: &RequestedBy) -> String {
        let known = self.members.lock().unwrap().get(&by.id).copied();
        let member = match known {
            Some(member) => member,
            None => {
                let member = self.member(data, by.id).await;
                self.members.lock().unwrap().insert(by.id, member);
                member
            }
        };
        member.map_or_else(
            || {
                by.display_name
                    .as_deref()
                    .map_or_else(|| "Unknown".into(), escape)
            },
            |id| id.mention().to_string(),
        )
    }

    /// Seerr user `id`'s Discord account, if they linked one and it's in this server
    async fn member(&self, data: &Data, id: u32) -> Option<UserId> {
        let guild = self.guild?;
        let discord = data
            .seerr
            .discord_id(&data.http, id)
            .await
            .inspect_err(|e| warn!("Fetching Seerr user {id}: {e:#}"))
            .ok()
            .flatten()?;
        guild
            .member(&*self.discord, discord)
            .await
            .is_ok()
            .then_some(discord)
    }

    /// Whose requests to list: `None` for everyone's
    fn requester(&self) -> Option<u32> {
        if self.admin {
            None
        } else {
            self.user.map(|u| u.id)
        }
    }

    /// The profiles `request` can be changed between: the default HD or 4K server's, if that's
    /// where it's going
    fn profiles(&self, request: &Request) -> Option<&Profiles> {
        let movies = matches!(request.media()?, Media::Movie(_));
        self.profiles.iter().find(|p| {
            p.movies == movies
                && p.is_4k == request.is_4k
                && request.server_id.is_none_or(|s| s == p.server)
        })
    }
}

/// A rendered page or card
struct Screen {
    /// Taken (left empty) when sent
    embed: CreateEmbed,
    /// Taken (left empty) when sent
    components: Vec<CreateActionRow>,
    /// What's showing, after clamping to the list
    view: View,
    /// Number of requests listed
    total: usize,
    /// The request on a card
    card: Option<Card>,
    /// The requests being confirmed for approval, with their titles
    batch: Vec<(u32, String)>,
}

impl Screen {
    /// Fetches and renders `view`. A card whose request is gone shows its page instead, and a
    /// page or card past the end of the list shows the last one.
    async fn build(data: &Data, viewer: &Viewer, view: View, id: &str) -> Result<Self> {
        let (filter, requester) = (viewer.filter.seerr(), viewer.requester());
        match view {
            View::Card {
                index,
                id: Some(request),
            } => {
                if let Some(request) = data.seerr.request_by_id(&data.http, request).await? {
                    // Only for the count
                    let page = data
                        .seerr
                        .requests(&data.http, filter, requester, 0, 1)
                        .await?;
                    let total = page.total as usize;
                    return Self::card(
                        data,
                        viewer,
                        Some((index.min(total.max(1) - 1), total)),
                        request,
                        id,
                    )
                    .await;
                }
                Box::pin(Self::build(
                    data,
                    viewer,
                    View::List {
                        page: index / PAGE_SIZE,
                    },
                    id,
                ))
                .await
            }
            View::Card { index, id: None } => {
                let page = data
                    .seerr
                    .requests(&data.http, filter, requester, index, 1)
                    .await?;
                let total = page.total as usize;
                match page.requests.into_iter().next() {
                    Some(request) => {
                        Self::card(data, viewer, Some((index, total)), request, id).await
                    }
                    None if total > 0 => {
                        let last = View::Card {
                            index: total - 1,
                            id: None,
                        };
                        Box::pin(Self::build(data, viewer, last, id)).await
                    }
                    None => Self::list(data, viewer, 0, id).await,
                }
            }
            View::Batch { index, id: request } => {
                if let Some(screen) = Self::batch(data, viewer, Some(index), request, id).await? {
                    return Ok(screen);
                }
                let card = View::Card {
                    index,
                    id: Some(request),
                };
                Box::pin(Self::build(data, viewer, card, id)).await
            }
            View::List { page } => Self::list(data, viewer, page, id).await,
        }
    }

    /// Page `page` of the list, with a menu to open each request on it
    async fn list(data: &Data, viewer: &Viewer, page: usize, id: &str) -> Result<Self> {
        let (filter, requester) = (viewer.filter.seerr(), viewer.requester());
        let mut page = page;
        let mut listed = data
            .seerr
            .requests(&data.http, filter, requester, page * PAGE_SIZE, PAGE_SIZE)
            .await?;
        let total = listed.total as usize;
        let pages = total.div_ceil(PAGE_SIZE).max(1);
        if page >= pages {
            page = pages - 1;
            listed = data
                .seerr
                .requests(&data.http, filter, requester, page * PAGE_SIZE, PAGE_SIZE)
                .await?;
        }

        // Requests don't carry titles, so each one's details are looked up
        let details = join_all(listed.requests.iter().map(|r| async move {
            let media = r.media()?;
            data.seerr
                .details(&data.http, media)
                .await
                .inspect_err(|e| warn!("Fetching {media}: {e:#}"))
                .ok()
        }));
        let names = join_all(listed.requests.iter().map(|r| async move {
            match &r.requested_by {
                Some(by) if viewer.admin => Some(viewer.name(data, by).await),
                _ => None,
            }
        }));
        let (details, names) = tokio::join!(details, names);
        let downloads = join_all(
            listed
                .requests
                .iter()
                .zip(&details)
                .map(|(r, d)| async move {
                    match d {
                        Some(d) => request_download(data, r, d).await,
                        None => None,
                    }
                }),
        )
        .await;

        let mut rows = Vec::new();
        let mut options = Vec::new();
        for (i, (((request, details), name), download)) in listed
            .requests
            .iter()
            .zip(&details)
            .zip(names)
            .zip(downloads)
            .enumerate()
        {
            let title = details.as_ref().map_or_else(
                || "Unknown title".to_owned(),
                |d| seerr::with_year(&d.title, d.release_date.as_deref()),
            );
            let label = label(request, details.as_ref(), download);
            let mut line = format!("**{}**{}", escape(&title), seasons(request, " \u{b7} "));
            let mut about = vec![label.clone()];
            about.extend(name);
            about.extend(requested_at(request));
            line = format!("{line}\n{}", about.join(" \u{b7} "));
            rows.push(line);

            let name = format!("{title}{}", seasons(request, ", "));
            options.push(
                CreateSelectMenuOption::new(
                    truncate(&name, 100),
                    (page * PAGE_SIZE + i).to_string(),
                )
                .description(truncate(&label, 100)),
            );
        }

        let heading = if viewer.admin {
            "Requests"
        } else {
            "Your requests"
        };
        let mut embed = CreateEmbed::new().title(heading);
        embed = embed.description(if rows.is_empty() {
            "Nothing here".to_owned()
        } else {
            rows.join("\n\n")
        });
        let mut footer = Vec::new();
        if viewer.filter != Filter::All {
            footer.push(viewer.filter.name().to_owned());
        }
        if pages > 1 {
            footer.push(format!("Page {} of {pages}", page + 1));
        }
        if !footer.is_empty() {
            embed = embed.footer(CreateEmbedFooter::new(footer.join(" \u{b7} ")));
        }

        let button = |suffix: &str| CreateButton::new(format!("{id}-{suffix}"));
        let mut components = Vec::new();
        if !options.is_empty() {
            components.push(CreateActionRow::SelectMenu(
                CreateSelectMenu::new(
                    format!("{id}-open"),
                    CreateSelectMenuKind::String {
                        options: options.into_iter().take(MAX_OPTIONS).collect(),
                    },
                )
                .placeholder("View request details"),
            ));
        }
        let mut buttons = Vec::new();
        if pages > 1 {
            buttons.push(
                button("prev")
                    .emoji('\u{25c0}')
                    .style(ButtonStyle::Secondary),
            );
        }
        buttons.push(
            button("refresh")
                .emoji('\u{1f504}')
                .style(ButtonStyle::Secondary),
        );
        if pages > 1 {
            buttons.push(
                button("next")
                    .emoji('\u{25b6}')
                    .style(ButtonStyle::Secondary),
            );
        }
        components.push(CreateActionRow::Buttons(buttons));

        Ok(Self {
            embed,
            components,
            view: View::List { page },
            total,
            card: None,
            batch: Vec::new(),
        })
    }

    /// A request as a card. `position` is its index in the list and the number listed; without
    /// one it's a post, with no list to go back to but a refresh button and a link to Seerr.
    async fn card(
        data: &Data,
        viewer: &Viewer,
        position: Option<(usize, usize)>,
        request: Request,
        id: &str,
    ) -> Result<Self> {
        let (index, total) = position.unwrap_or((0, 1));
        let media = request
            .media()
            .context("request isn't for a movie or show")?;
        let details = data.seerr.details(&data.http, media).await?;
        let title = seerr::with_year(&details.title, details.release_date.as_deref());

        let mut embed = CreateEmbed::new().title(&title);
        // Posts link to Seerr with a button instead
        if let (Some(_), Some(url)) = (position, data.seerr.page(media)) {
            embed = embed.url(url);
        }
        if let Some(path) = &details.poster_path {
            embed = embed.thumbnail(format!("{POSTER_URL}{path}"));
        }
        let download = request_download(data, &request, &details).await;
        embed = embed.field("Status", label(&request, Some(&details), download), false);
        if !request.seasons.is_empty() {
            embed = embed.field("Seasons", ranges(&request.season_numbers()), true);
        }
        if viewer.admin
            && let Some(by) = &request.requested_by
        {
            embed = embed.field("Requested by", viewer.name(data, by).await, true);
        }
        if let Some(at) = requested_at(&request) {
            embed = embed.field("Requested", at, true);
        }
        let reviewed = match request.status {
            RequestStatus::Approved | RequestStatus::Completed => Some("Approved by"),
            RequestStatus::Declined => Some("Declined by"),
            _ => None,
        };
        if let (Some(heading), Some(by), false) =
            (reviewed, &request.modified_by, request.automatic())
        {
            embed = embed.field(heading, viewer.name(data, by).await, true);
        }
        let profiles = viewer.profiles(&request);
        if viewer.admin {
            let quality = request.profile_id.map_or_else(
                || "Default".into(),
                |p| {
                    profiles
                        .and_then(|ps| ps.name(p))
                        .map_or_else(|| format!("Profile {p}"), escape)
                },
            );
            embed = embed.field("Quality", quality, true);
        }
        if total > 1 {
            embed = embed.footer(CreateEmbedFooter::new(format!("{} of {total}", index + 1)));
        }

        let cancellable: Vec<Cancellable> = details
            .cancellable(viewer.user.map(|u| u.id), viewer.admin)
            .into_iter()
            .filter(|c| c.id == request.id && (can_unsend(data, media, c.is_4k) || !c.sent))
            .collect();
        let reviewable = viewer.admin && request.status == RequestStatus::Pending;
        // How many pending requests would be approved together, itself included
        let batch = if reviewable {
            batch_of(data, &request)
                .await
                .inspect_err(|e| warn!("Fetching the batch of request {}: {e:#}", request.id))
                .map_or(0, |b| b.len())
        } else {
            0
        };

        let mut components = Vec::new();
        if reviewable && !request.seasons.is_empty() {
            // The request's own seasons plus any nobody's requested yet
            let mut numbers: Vec<u32> = details
                .season_numbers()
                .filter(|&n| details.season_status(n) == Status::Missing)
                .chain(request.season_numbers())
                .collect();
            numbers.sort_unstable();
            numbers.dedup();
            let picked = request.season_numbers();
            let options: Vec<_> = numbers
                .iter()
                .take(MAX_OPTIONS)
                .map(|&n| {
                    CreateSelectMenuOption::new(format!("Season {n}"), n.to_string())
                        .default_selection(picked.contains(&n))
                })
                .collect();
            if options.len() > 1 {
                #[allow(clippy::cast_possible_truncation)]
                let max = options.len() as u8;
                components.push(CreateActionRow::SelectMenu(
                    CreateSelectMenu::new(
                        format!("{id}-seasons"),
                        CreateSelectMenuKind::String { options },
                    )
                    .min_values(1)
                    .max_values(max),
                ));
            }
        }
        if let (true, Some(profiles)) = (reviewable, profiles) {
            let options = profiles
                .list
                .iter()
                .take(MAX_OPTIONS)
                .map(|p| {
                    CreateSelectMenuOption::new(&p.name, p.id.to_string())
                        .default_selection(request.profile_id == Some(p.id))
                })
                .collect();
            components.push(CreateActionRow::SelectMenu(
                CreateSelectMenu::new(
                    format!("{id}-quality"),
                    CreateSelectMenuKind::String { options },
                )
                .placeholder("Default quality")
                .min_values(0)
                .max_values(1),
            ));
        }

        let button = |suffix: &str| CreateButton::new(format!("{id}-{suffix}"));
        let mut buttons = Vec::new();
        if total > 1 {
            buttons.push(
                button("prev")
                    .emoji('\u{25c0}')
                    .style(ButtonStyle::Secondary),
            );
        }
        if position.is_some() {
            buttons.push(
                button("back")
                    .label("All requests")
                    .style(ButtonStyle::Secondary),
            );
        }
        if reviewable {
            buttons.push(
                button("approve")
                    .label("Approve")
                    .style(ButtonStyle::Success),
            );
            if batch > 1 {
                buttons.push(
                    button("batch")
                        .label(format!("Approve all {batch}"))
                        .style(ButtonStyle::Success),
                );
            }
            buttons.push(
                button("decline")
                    .label("Decline")
                    .style(ButtonStyle::Danger),
            );
        } else if !cancellable.is_empty() {
            buttons.push(
                button("cancel")
                    .label("Cancel request")
                    .style(ButtonStyle::Danger),
            );
        }
        if total > 1 {
            buttons.push(
                button("next")
                    .emoji('\u{25b6}')
                    .style(ButtonStyle::Secondary),
            );
        }
        if position.is_none() {
            buttons.push(
                button("refresh")
                    .emoji('\u{1f504}')
                    .style(ButtonStyle::Secondary),
            );
        }
        if let Some(url) = details
            .media_info
            .as_ref()
            .and_then(|m| m.jellyfin_url(request.is_4k))
        {
            buttons.push(CreateButton::new_link(url).label("Jellyfin"));
        }
        if let (None, Some(url)) = (position, data.seerr.page(media)) {
            buttons.push(CreateButton::new_link(url).label("Seerr"));
        }
        // Discord allows at most 5 buttons per row
        components.extend(
            buttons
                .chunks(5)
                .map(|row| CreateActionRow::Buttons(row.to_vec())),
        );

        Ok(Self {
            embed,
            components,
            view: View::Card {
                index,
                id: Some(request.id),
            },
            total,
            card: Some(Card {
                media,
                title,
                tvdb_id: details.media_info.as_ref().and_then(|m| m.tvdb_id),
                reviewable,
                cancellable,
                request,
            }),
            batch: Vec::new(),
        })
    }

    /// Asks to confirm approving request `request`'s batch, listing what's in it. `index` is the
    /// card's, to go back to; without one there's no Back button. `None` if there's no batch to
    /// approve: the viewer isn't an admin, or the request is gone, no longer pending, or on its
    /// own.
    async fn batch(
        data: &Data,
        viewer: &Viewer,
        index: Option<usize>,
        request: u32,
        id: &str,
    ) -> Result<Option<Self>> {
        if !viewer.admin {
            return Ok(None);
        }
        let Some(request) = data.seerr.request_by_id(&data.http, request).await? else {
            return Ok(None);
        };
        if request.status != RequestStatus::Pending {
            return Ok(None);
        }
        let batch = batch_of(data, &request).await?;
        if batch.len() < 2 {
            return Ok(None);
        }

        let details = join_all(batch.iter().map(|r| async move {
            let media = r.media()?;
            data.seerr
                .details(&data.http, media)
                .await
                .inspect_err(|e| warn!("Fetching {media}: {e:#}"))
                .ok()
        }))
        .await;
        let titles: Vec<String> = details
            .iter()
            .map(|d| {
                d.as_ref().map_or_else(
                    || "Unknown title".to_owned(),
                    |d| seerr::with_year(&d.title, d.release_date.as_deref()),
                )
            })
            .collect();

        let mut lines = Vec::new();
        if let Some(by) = &request.requested_by {
            lines.push(format!(
                "From {}, each within an hour of the one before:",
                viewer.name(data, by).await
            ));
        }
        // Discord allows at most 4096 characters in a description
        for (r, title) in batch.iter().zip(&titles).take(BATCH_SHOWN) {
            let mut line = format!("**{}**{}", escape(title), seasons(r, " \u{b7} "));
            if let Some(at) = requested_at(r) {
                line = format!("{line} \u{b7} {at}");
            }
            lines.push(line);
        }
        let n = batch.len();
        if n > BATCH_SHOWN {
            lines.push(format!("\u{2026}and {} more", n - BATCH_SHOWN));
        }
        let embed = CreateEmbed::new()
            .title(format!("Approve {n} requests?"))
            .description(lines.join("\n"));

        let button = |suffix: &str| CreateButton::new(format!("{id}-{suffix}"));
        let mut buttons = Vec::new();
        if index.is_some() {
            buttons.push(
                button("batch-back")
                    .label("Back")
                    .style(ButtonStyle::Secondary),
            );
        }
        buttons.push(
            button("approve-all")
                .label(format!("Approve all {n}"))
                .style(ButtonStyle::Success),
        );
        let components = vec![CreateActionRow::Buttons(buttons)];

        Ok(Some(Self {
            embed,
            components,
            view: View::Batch {
                index: index.unwrap_or(0),
                id: request.id,
            },
            total: 0,
            card: None,
            batch: batch.iter().map(|r| r.id).zip(titles).collect(),
        }))
    }
}

impl Screen {
    /// A post whose request is no longer in Seerr, from the card it had, without buttons
    fn gone(embeds: &[Embed]) -> Self {
        Self {
            embed: gone_embed(embeds.first()),
            components: Vec::new(),
            view: View::List { page: 0 },
            total: 0,
            card: None,
            batch: Vec::new(),
        }
    }
}

/// The request on a card, and what the buttons on it may do
struct Card {
    request: Request,
    media: Media,
    /// "Title (2022)"
    title: String,
    /// TVDB id, shows only, once Seerr knows of it
    tvdb_id: Option<u32>,
    /// Whether the viewer may approve, decline and edit it
    reviewable: bool,
    /// The request, if the viewer may cancel it. Ones already sent to Radarr or Sonarr are left
    /// out if the bot can't reach it.
    cancellable: Vec<Cancellable>,
}

impl Card {
    /// Does `action` if the card allows it, returning what to tell the invoker, if anything. A
    /// change that worked shows on the card instead.
    async fn act(&self, data: &Data, viewer: &Viewer, action: Action) -> Option<String> {
        let result = match action {
            Action::Go(_) => return None,
            Action::Cancel if !self.cancellable.is_empty() => {
                let (notice, _) = cancel(
                    data,
                    self.media,
                    self.tvdb_id,
                    &self.title,
                    &self.cancellable,
                )
                .await;
                return Some(notice);
            }
            Action::Review { approve } if self.reviewable => {
                data.seerr
                    .review(&data.http, self.request.id, approve)
                    .await
            }
            Action::Quality(profile)
                if self.reviewable && viewer.profiles(&self.request).is_some() =>
            {
                let seasons = self.request.season_numbers();
                data.seerr
                    .edit(&data.http, &self.request, profile, &seasons)
                    .await
            }
            Action::Seasons(seasons) if self.reviewable && !seasons.is_empty() => {
                data.seerr
                    .edit(&data.http, &self.request, self.request.profile_id, &seasons)
                    .await
            }
            _ => return None,
        };
        match result {
            Ok(Changed::Done) => None,
            Ok(Changed::Refused(message)) => Some(message),
            Err(e) => {
                warn!("Changing request {}: {e:#}", self.request.id);
                Some(UNREACHABLE.into())
            }
        }
    }
}

/// The default HD and 4K servers' movie and show quality profiles, for admins to change
/// requests between and for requesting
pub(super) async fn profiles(data: &Data) -> Vec<Profiles> {
    let profiles = |media, is_4k| async move {
        data.seerr
            .profiles(&data.http, media, is_4k)
            .await
            .inspect_err(|e| warn!("Fetching quality profiles: {e:#}"))
            .ok()
            .flatten()
    };
    let all: [Option<Profiles>; 4] = tokio::join!(
        profiles(Media::Movie(0), false),
        profiles(Media::Tv(0), false),
        profiles(Media::Movie(0), true),
        profiles(Media::Tv(0), true),
    )
    .into();
    all.into_iter().flatten().collect()
}

/// `request` as a post in `guild`'s channels, with buttons for admins
pub async fn post_card(
    data: &Data,
    discord: Arc<Http>,
    guild: Option<GuildId>,
    request: Request,
) -> Result<(CreateEmbed, Vec<CreateActionRow>)> {
    let viewer = Viewer::post(data, discord, guild).await;
    let id = format!("{POST}{}", request.id);
    let screen = Screen::card(data, &viewer, None, request, &id).await?;
    Ok((screen.embed, screen.components))
}

/// Rebuilds the post `message` in `channel` of request `id`, after it changed somewhere else.
/// Returns whether the request is still open, so it may change again: not declined, done, or
/// gone from Seerr.
pub async fn refresh_post(
    data: &Data,
    discord: &Arc<Http>,
    channel: ChannelId,
    message: MessageId,
    id: u32,
) -> Result<bool> {
    let mut open = false;
    let edit = match data.seerr.request_by_id(&data.http, id).await? {
        Some(request) => {
            open = !matches!(
                request.status,
                RequestStatus::Declined | RequestStatus::Completed
            );
            let guild = match channel.to_channel(discord).await? {
                Channel::Guild(channel) => Some(channel.guild_id),
                _ => None,
            };
            let (embed, components) = post_card(data, discord.clone(), guild, request).await?;
            EditMessage::new().embed(embed).components(components)
        }
        None => {
            let old = channel.message(discord, message).await?;
            EditMessage::new()
                .embed(gone_embed(old.embeds.first()))
                .components(vec![])
        }
    };
    channel.edit_message(discord, message, edit).await?;
    Ok(open)
}

/// The request `message` is a post of, if it's one
pub fn post_request(message: &Message) -> Option<u32> {
    message
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|component| match component {
            ActionRowComponent::Button(Button {
                data: ButtonKind::NonLink { custom_id, .. },
                ..
            }) => custom_id
                .strip_prefix(POST)?
                .split_once('-')?
                .0
                .parse()
                .ok(),
            _ => None,
        })
}

/// A post's card once its request is no longer in Seerr
fn gone_embed(old: Option<&Embed>) -> CreateEmbed {
    old.cloned()
        .map_or_else(CreateEmbed::new, CreateEmbed::from)
        .footer(CreateEmbedFooter::new("No longer in Seerr"))
}

/// Handles a press on a request post's buttons, or on the batch confirmation they open. Anyone
/// can refresh; everything else is for admins. Other presses are left alone.
pub async fn post_press(
    ctx: &serenity::Context,
    data: &Data,
    press: &ComponentInteraction,
) -> Result<()> {
    let Some((request, action)) = press
        .data
        .custom_id
        .strip_prefix(POST)
        .and_then(|rest| rest.split_once('-'))
    else {
        return Ok(());
    };
    let Ok(request) = request.parse::<u32>() else {
        return Ok(());
    };
    if action != "refresh" && !has_admin_role(data, press.member.as_ref()) {
        let reply = CreateInteractionResponseMessage::new()
            .content("Only admins can change requests")
            .ephemeral(true);
        press
            .create_response(ctx, CreateInteractionResponse::Message(reply))
            .await?;
        return Ok(());
    }
    let id = format!("{POST}{request}");
    let values: Vec<&str> = match &press.data.kind {
        ComponentInteractionDataKind::StringSelect { values } => {
            values.iter().map(String::as_str).collect()
        }
        _ => Vec::new(),
    };

    // Opens the confirmation for approving the batch, just for whoever pressed
    if action == "batch" {
        press
            .create_response(
                ctx,
                CreateInteractionResponse::Defer(
                    CreateInteractionResponseMessage::new().ephemeral(true),
                ),
            )
            .await?;
        let viewer = Viewer::post(data, ctx.http.clone(), press.guild_id).await;
        let edit = match Screen::batch(data, &viewer, None, request, &id).await {
            Ok(Some(screen)) => EditInteractionResponse::new()
                .embed(screen.embed)
                .components(screen.components),
            Ok(None) => EditInteractionResponse::new().content("Nothing to approve together now"),
            Err(e) => {
                warn!("Fetching the batch of request {request}: {e:#}");
                EditInteractionResponse::new().content(UNREACHABLE)
            }
        };
        press.edit_response(ctx, edit).await?;
        return Ok(());
    }

    // Acknowledge first: Seerr looks things up on TMDB, which may take longer than the 3 seconds
    // Discord allows before an interaction fails
    press
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await?;
    let viewer = Viewer::post(data, ctx.http.clone(), press.guild_id).await;

    // Pressed on the batch confirmation, which then says how it went
    if action == "approve-all" {
        let content = match Screen::batch(data, &viewer, None, request, &id).await {
            Ok(Some(screen)) => approve_all(data, &screen.batch)
                .await
                .unwrap_or_else(|| format!("Approved {} requests", screen.batch.len())),
            Ok(None) => "Nothing to approve together now".into(),
            Err(e) => {
                warn!("Fetching the batch of request {request}: {e:#}");
                UNREACHABLE.into()
            }
        };
        let edit = EditInteractionResponse::new()
            .content(content)
            .embeds(vec![])
            .components(vec![]);
        press.edit_response(ctx, edit).await?;
        return Ok(());
    }

    let action = match action {
        "refresh" => None,
        "approve" => Some(Action::Review { approve: true }),
        "decline" => Some(Action::Review { approve: false }),
        "cancel" => Some(Action::Cancel),
        "quality" => Some(Action::Quality(values.first().and_then(|v| v.parse().ok()))),
        "seasons" => Some(Action::Seasons(
            values.iter().filter_map(|v| v.parse().ok()).collect(),
        )),
        _ => return Ok(()),
    };
    let mut notice = None;
    let screen = match data.seerr.request_by_id(&data.http, request).await {
        Ok(Some(found)) => {
            let mut screen = Screen::card(data, &viewer, None, found, &id).await;
            if let (
                Some(action),
                Ok(Screen {
                    card: Some(card), ..
                }),
            ) = (action, &screen)
            {
                notice = card.act(data, &viewer, action).await;
                screen = match data.seerr.request_by_id(&data.http, request).await {
                    Ok(Some(found)) => Screen::card(data, &viewer, None, found, &id).await,
                    Ok(None) => Ok(Screen::gone(&press.message.embeds)),
                    Err(e) => Err(e),
                };
            }
            screen
        }
        Ok(None) => Ok(Screen::gone(&press.message.embeds)),
        Err(e) => Err(e),
    };
    match screen {
        Ok(screen) => {
            let edit = EditInteractionResponse::new()
                .embed(screen.embed)
                .components(screen.components);
            press.edit_response(ctx, edit).await?;
        }
        Err(e) => {
            warn!("Loading request {request}: {e:#}");
            notice.get_or_insert_with(|| UNREACHABLE.into());
        }
    }
    if let Some(notice) = notice {
        let followup = CreateInteractionResponseFollowup::new()
            .content(notice)
            .ephemeral(true);
        press.create_followup(ctx, followup).await?;
    }
    Ok(())
}

/// Approves `batch`, requests by id with their titles, one at a time. Returns which couldn't be
/// approved and why, if any.
async fn approve_all(data: &Data, batch: &[(u32, String)]) -> Option<String> {
    let mut failed = Vec::new();
    for (id, title) in batch {
        let why = match data.seerr.review(&data.http, *id, true).await {
            Ok(Changed::Done) => continue,
            Ok(Changed::Refused(message)) => message,
            Err(e) => {
                warn!("Approving request {id}: {e:#}");
                "couldn't reach Seerr".into()
            }
        };
        failed.push(format!("**{}**: {why}", escape(title)));
    }
    (!failed.is_empty()).then(|| format!("Couldn't approve some:\n{}", failed.join("\n")))
}

/// `request`'s batch: its requester's pending requests, oldest first, in an unbroken run with it
/// where each was made within [`BATCH_GAP`] of the one before
async fn batch_of(data: &Data, request: &Request) -> Result<Vec<Request>> {
    let Some(by) = &request.requested_by else {
        return Ok(Vec::new());
    };
    let pending = data
        .seerr
        .requests(
            &data.http,
            Filter::Pending.seerr(),
            Some(by.id),
            0,
            BATCH_MAX,
        )
        .await?;
    Ok(batch(pending.requests, request.id))
}

/// The run of `requests` around request `id`, oldest first, where each was made within
/// [`BATCH_GAP`] of the one before. Requests without a time are left out.
fn batch(requests: Vec<Request>, id: u32) -> Vec<Request> {
    let mut timed: Vec<(i64, Request)> = requests
        .into_iter()
        .filter_map(|r| {
            let at = DateTime::parse_from_rfc3339(r.created_at.as_deref()?).ok()?;
            Some((at.timestamp(), r))
        })
        .collect();
    timed.sort_by_key(|&(at, ref r)| (at, r.id));
    let Some(at) = timed.iter().position(|(_, r)| r.id == id) else {
        return Vec::new();
    };
    let mut start = at;
    while start > 0 && timed[start].0 - timed[start - 1].0 <= BATCH_GAP {
        start -= 1;
    }
    let mut end = at;
    while end + 1 < timed.len() && timed[end + 1].0 - timed[end].0 <= BATCH_GAP {
        end += 1;
    }
    timed.drain(start..=end).map(|(_, r)| r).collect()
}

/// How a request's status reads: its own while it's pending, declined or failed, else where
/// what it asked for stands. `details` is the media's, if it could be fetched, and `download`
/// how far its download has got.
fn label(request: &Request, details: Option<&Details>, download: Option<Download>) -> String {
    let label = match request.status {
        RequestStatus::Pending => status(Status::Pending, false, None),
        RequestStatus::Declined => "Declined",
        RequestStatus::Failed => "Failed, see Seerr",
        RequestStatus::Approved | RequestStatus::Completed => {
            match details.map_or(Status::Processing, |d| d.request_status(request)) {
                s @ (Status::Available | Status::PartiallyAvailable | Status::Blocklisted) => {
                    status(s, false, None)
                }
                Status::Missing if request.status == RequestStatus::Completed => "Deleted since",
                _ => status(Status::Processing, request.automatic(), download),
            }
        }
    };
    if request.is_4k {
        format!("{label} (4K)")
    } else {
        label.to_owned()
    }
}

/// "Season 2" or "Seasons 1–3" after `separator`, nothing for movies
fn seasons(request: &Request, separator: &str) -> String {
    match request.season_numbers().as_slice() {
        [] => String::new(),
        [n] => format!("{separator}Season {n}"),
        many => format!("{separator}Seasons {}", ranges(many)),
    }
}

/// When it was made, as a Discord timestamp that reads "2 days ago"
fn requested_at(request: &Request) -> Option<String> {
    let at = DateTime::parse_from_rfc3339(request.created_at.as_deref()?).ok()?;
    Some(format!("<t:{}:R>", at.timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batches() {
        let request = |id: u32, at: &str| -> Request {
            serde_json::from_str(&format!(
                r#"{{"id": {id}, "status": 1, "is4k": false, "createdAt": "{at}"}}"#
            ))
            .unwrap()
        };
        let requests = || {
            vec![
                request(5, "2026-10-25T15:00:00.000Z"),
                request(1, "2026-10-25T10:00:00.000Z"),
                request(2, "2026-10-25T10:50:00.000Z"),
                request(3, "2026-10-25T11:40:00.000Z"),
                request(4, "2026-10-25T12:41:00.000Z"),
            ]
        };
        let ids = |id| {
            batch(requests(), id)
                .iter()
                .map(|r| r.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(2), [1, 2, 3]);
        assert_eq!(ids(1), [1, 2, 3]);
        assert_eq!(ids(4), [4]);
        assert_eq!(ids(5), [5]);
        assert!(ids(9).is_empty());
    }
}
