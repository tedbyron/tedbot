//! Posts in Discord when Seerr's webhook says a request was made (`seerr.webhook.requests_channels`)
//! or its movie or seasons are available in Jellyfin (`seerr.webhook.available_channels`).

use std::sync::Arc;

use anyhow::{Context as _, Result};
use poise::serenity_prelude::{
    Channel, ChannelId, CreateActionRow, CreateAllowedMentions, CreateButton, CreateEmbed,
    CreateMessage, GuildId, Http, Mentionable as _, MessageId, UserId,
};
use tracing::warn;

use crate::{
    Data,
    commands::{
        escape,
        request::{ranges, truncate},
        requests::post_card,
    },
    seerr::{self, Request, RequestStatus, Status},
};

/// TMDB image URL prefix for posters, 342 pixels wide
const POSTER_URL: &str = "https://image.tmdb.org/t/p/w342";
/// Longest overview shown, in characters
const OVERVIEW_MAX: usize = 350;

/// What happened to a request
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event {
    /// Made, whether it's waiting for approval or was approved automatically
    Requested,
    /// Its movie or seasons are in Jellyfin
    Available,
}

/// Where to post
#[derive(Debug, Default)]
pub struct Targets {
    pub requests: Vec<ChannelId>,
    pub available: Vec<ChannelId>,
    /// Pinged for requests waiting for approval, where they're in the channel's server
    pub admins: Vec<UserId>,
}

impl Targets {
    pub fn channels(&self, event: Event) -> &[ChannelId] {
        match event {
            Event::Requested => &self.requests,
            Event::Available => &self.available,
        }
    }
}

/// Posts `request` in each of `event`'s channels, returning the posts. New requests get the same
/// card `/requests` shows admins, with its buttons. Available requests are skipped if what they
/// asked for isn't all available after all. Fails if nothing was posted anywhere, so it can be
/// tried again.
pub async fn post(
    discord: &Arc<Http>,
    data: &Data,
    targets: &Targets,
    event: Event,
    request: &Request,
) -> Result<Vec<(ChannelId, MessageId)>> {
    let mut posts = Vec::new();
    match event {
        Event::Requested => {
            // Only requests waiting for approval need an admin
            let pending = request.status == RequestStatus::Pending;
            for &channel in &targets.requests {
                let guild = guild(discord, channel).await;
                let mut ping = Vec::new();
                if let Some(guild) = guild.filter(|_| pending) {
                    for &admin in &targets.admins {
                        if guild.member(discord, admin).await.is_ok() {
                            ping.push(admin);
                        }
                    }
                }
                let (embed, components) =
                    post_card(data, discord.clone(), guild, request.clone()).await?;
                let mut message = CreateMessage::new()
                    .embed(embed)
                    .components(components)
                    .allowed_mentions(CreateAllowedMentions::new().users(ping.iter().copied()));
                if !ping.is_empty() {
                    let mentions: Vec<_> = ping.iter().map(|u| u.mention().to_string()).collect();
                    message = message.content(mentions.join(" "));
                }
                send(discord, channel, message, &mut posts).await;
            }
        }
        Event::Available => {
            let Some((embed, components)) = available(data, request).await? else {
                return Ok(posts);
            };
            let by = request.requested_by.as_ref();
            let linked = match by {
                Some(by) => data
                    .seerr
                    .discord_id(&data.http, by.id)
                    .await
                    .inspect_err(|e| warn!("Fetching Seerr user {}: {e:#}", by.id))
                    .ok()
                    .flatten(),
                None => None,
            };
            let name = by.and_then(|by| by.display_name.as_deref()).map(escape);
            for &channel in &targets.available {
                let mut embed = embed.clone();
                if let Some(requester) = requester(discord, channel, linked)
                    .await
                    .or_else(|| name.clone())
                {
                    embed = embed.field("Requested by", requester, true);
                }
                let message = CreateMessage::new()
                    .embed(embed)
                    .components(components.clone())
                    .allowed_mentions(CreateAllowedMentions::new());
                send(discord, channel, message, &mut posts).await;
            }
        }
    }
    anyhow::ensure!(!posts.is_empty(), "couldn't post in any channel");
    Ok(posts)
}

/// The "now available" card, without the requester, which depends on the channel. `None` if what
/// `request` asked for isn't all available.
async fn available(
    data: &Data,
    request: &Request,
) -> Result<Option<(CreateEmbed, Vec<CreateActionRow>)>> {
    let media = request
        .media()
        .context("request isn't for a movie or show")?;
    let details = data.seerr.details(&data.http, media).await?;
    if details.request_status(request) != Status::Available {
        return Ok(None);
    }

    let title = seerr::with_year(&details.title, details.release_date.as_deref());
    let mut embed = CreateEmbed::new().title(if request.is_4k {
        format!("{title} (4K)")
    } else {
        title
    });
    if let Some(overview) = details.overview.as_deref().filter(|o| !o.is_empty()) {
        embed = embed.description(truncate(overview, OVERVIEW_MAX));
    }
    if let Some(path) = &details.poster_path {
        embed = embed.thumbnail(format!("{POSTER_URL}{path}"));
    }
    if !request.seasons.is_empty() {
        embed = embed.field("Seasons", ranges(&request.season_numbers()), true);
    }
    let info = details.media_info.as_ref();
    let jellyfin = info.and_then(|m| m.jellyfin_url(request.is_4k).map(str::to_owned));
    let moonfin = info
        .filter(|_| data.jellyfin.moonfin)
        .and_then(|m| m.moonfin_url(request.is_4k));
    let links: Vec<CreateButton> = [
        ("Jellyfin", jellyfin),
        ("Moonfin", moonfin),
        ("Seerr", data.seerr.page(media)),
    ]
    .into_iter()
    .filter_map(|(label, url)| Some(CreateButton::new_link(url?).label(label)))
    .collect();
    let components = if links.is_empty() {
        Vec::new()
    } else {
        vec![CreateActionRow::Buttons(links)]
    };
    Ok(Some((embed, components)))
}

/// Sends `message` in `channel`, adding it to `posts` if it went
async fn send(
    discord: &Http,
    channel: ChannelId,
    message: CreateMessage,
    posts: &mut Vec<(ChannelId, MessageId)>,
) {
    match channel.send_message(discord, message).await {
        Ok(sent) => posts.push((channel, sent.id)),
        Err(e) => warn!("Posting in channel {channel}: {e}"),
    }
}

/// The server `channel` is in, if it's in one
async fn guild(discord: &Http, channel: ChannelId) -> Option<GuildId> {
    match channel.to_channel(discord).await {
        Ok(Channel::Guild(channel)) => Some(channel.guild_id),
        _ => None,
    }
}

/// A mention of `linked`, the requester's Discord account, if it's in `channel`'s server. A
/// mention in an embed never pings.
async fn requester(discord: &Http, channel: ChannelId, linked: Option<UserId>) -> Option<String> {
    let linked = linked?;
    guild(discord, channel)
        .await?
        .member(discord, linked)
        .await
        .is_ok()
        .then(|| linked.mention().to_string())
}
