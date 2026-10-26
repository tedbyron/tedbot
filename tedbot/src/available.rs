//! Posts in `discord.available_channels` when Seerr's webhook says a request's movie or seasons
//! are available in Jellyfin.

use anyhow::{Context as _, Result};
use poise::serenity_prelude::{
    Channel, ChannelId, CreateActionRow, CreateAllowedMentions, CreateButton, CreateEmbed,
    CreateMessage, Http, Mentionable as _, UserId,
};
use tracing::warn;

use crate::{
    commands::{
        escape,
        request::{ranges, truncate},
    },
    seerr::{self, Request, Seerr, Status},
};

/// TMDB image URL prefix for posters, 342 pixels wide
const POSTER_URL: &str = "https://image.tmdb.org/t/p/w342";
/// Longest overview shown, in characters
const OVERVIEW_MAX: usize = 350;

/// Posts `request` in each of `channels`, unless what it asked for isn't all available after all.
/// Fails if nothing was posted anywhere, so it can be tried again.
pub async fn post(
    discord: &Http,
    http: &reqwest::Client,
    seerr: &Seerr,
    channels: &[ChannelId],
    request: &Request,
) -> Result<()> {
    let media = request
        .media()
        .context("request isn't for a movie or show")?;
    let details = seerr.details(http, media).await?;
    if details.request_status(request) != Status::Available {
        return Ok(());
    }

    let mut embed = CreateEmbed::new().title(seerr::with_year(
        &details.title,
        details.release_date.as_deref(),
    ));
    if let Some(overview) = details.overview.as_deref().filter(|o| !o.is_empty()) {
        embed = embed.description(truncate(overview, OVERVIEW_MAX));
    }
    if let Some(path) = &details.poster_path {
        embed = embed.thumbnail(format!("{POSTER_URL}{path}"));
    }
    if !request.seasons.is_empty() {
        embed = embed.field("Seasons", ranges(&request.season_numbers()), true);
    }
    let links: Vec<CreateButton> = [
        (
            "Jellyfin",
            details
                .media_info
                .as_ref()
                .and_then(|m| m.media_url.clone()),
        ),
        ("Seerr", seerr.page(media)),
    ]
    .into_iter()
    .filter_map(|(label, url)| Some(CreateButton::new_link(url?).label(label)))
    .collect();
    let components = if links.is_empty() {
        Vec::new()
    } else {
        vec![CreateActionRow::Buttons(links)]
    };

    let by = request.requested_by.as_ref();
    let linked = match by {
        Some(by) => seerr
            .discord_id(http, by.id)
            .await
            .inspect_err(|e| warn!("Fetching Seerr user {}: {e:#}", by.id))
            .ok()
            .flatten(),
        None => None,
    };
    let name = by.and_then(|by| by.display_name.as_deref()).map(escape);

    let mut posted = false;
    for &channel in channels {
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
        match channel.send_message(discord, message).await {
            Ok(_) => posted = true,
            Err(e) => warn!("Posting in channel {channel}: {e}"),
        }
    }
    anyhow::ensure!(posted, "couldn't post in any channel");
    Ok(())
}

/// A mention of `linked`, the requester's Discord account, if it's in `channel`'s server. A
/// mention in an embed never pings.
async fn requester(discord: &Http, channel: ChannelId, linked: Option<UserId>) -> Option<String> {
    let linked = linked?;
    let Ok(Channel::Guild(channel)) = channel.to_channel(discord).await else {
        return None;
    };
    channel
        .guild_id
        .member(discord, linked)
        .await
        .is_ok()
        .then(|| linked.mention().to_string())
}
