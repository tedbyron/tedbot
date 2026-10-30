//! `/streams`: how many streams are playing on Jellyfin, who's watching, and the upload in use.
//! Admins also see what each stream is playing and how.
//!
//! Replies ephemerally, since admins and others see different things, with a 🔄 button that
//! rebuilds the embed until [`REFRESH_TIMEOUT`] passes with no press.

use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use poise::{
    CreateReply, command,
    serenity_prelude::{
        Builder as _, ButtonStyle, ComponentInteractionCollector, CreateActionRow,
        CreateAllowedMentions, CreateButton, CreateEmbed, CreateEmbedFooter,
        CreateInteractionResponse, CreateInteractionResponseFollowup, EditInteractionResponse,
        Mentionable as _, UserId,
    },
};
use tracing::warn;

use super::{escape, is_admin, remove_buttons};
use crate::{
    Context,
    jellyfin::{self, Session},
    seerr,
};

/// How long 🔄 keeps working after the last press. An ephemeral reply can only be edited with an
/// interaction token, which expires after 15 minutes, so this stays under that to remove the
/// button at the end.
const REFRESH_TIMEOUT: Duration = Duration::from_secs(14 * 60);
/// Discord allows at most 25 fields per embed
const MAX_FIELDS: usize = 25;

/// Who's streaming from the Jellyfin server
#[command(slash_command, ephemeral)]
pub async fn streams(ctx: Context<'_>) -> Result<()> {
    ctx.defer_ephemeral().await?;

    let admin = is_admin(ctx);
    let refresh = CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{}-refresh", ctx.id()))
            .emoji('\u{1f504}')
            .style(ButtonStyle::Secondary),
    ]);
    let reply = CreateReply::default()
        .embed(embed(ctx, admin).await?)
        .components(vec![refresh])
        .allowed_mentions(CreateAllowedMentions::new());
    let handle = ctx.send(reply).await?;
    let message = handle.message().await?.id;

    // Acknowledge first so a slow Jellyfin can't push the reply past Discord's 3 seconds
    let mut last = None;
    while let Some(press) = ComponentInteractionCollector::new(ctx)
        .message_id(message)
        .timeout(REFRESH_TIMEOUT)
        .await
    {
        press
            .create_response(ctx, CreateInteractionResponse::Acknowledge)
            .await?;
        match embed(ctx, admin).await {
            Ok(embed) => {
                EditInteractionResponse::new()
                    .embed(embed)
                    .execute(ctx, &press.token)
                    .await?;
            }
            Err(e) => {
                warn!("Refreshing sessions: {e:#}");
                CreateInteractionResponseFollowup::new()
                    .content("Couldn't reach Jellyfin, try again in a bit \u{1fae4}")
                    .ephemeral(true)
                    .execute(ctx, (None, &press.token))
                    .await?;
            }
        }
        last = Some(press.token);
    }

    // The newest token is the only one guaranteed not to have expired
    remove_buttons(ctx, last).await
}

/// The stream count, upload in use, and who's watching. For admins, also LAN bandwidth and one
/// field per stream, sorted by user.
async fn embed(ctx: Context<'_>, admin: bool) -> Result<CreateEmbed> {
    let data = ctx.data();
    let mut sessions = data.jellyfin.playing(&data.http).await?;
    let embed = CreateEmbed::new();
    let count = match sessions.len() {
        0 => return Ok(embed.description("Nothing playing")),
        1 => "1 stream".to_owned(),
        n => format!("{n} streams"),
    };
    sessions.sort_by(|a, b| a.user_name.cmp(&b.user_name));
    let users = users(ctx, &sessions).await;

    if !admin {
        let mut names = users;
        names.dedup();
        return Ok(embed.description(format!(
            "{count} \u{b7} {}\n{}",
            upload(&sessions),
            names.join(", ")
        )));
    }

    let mut embed = sessions.iter().zip(&users).take(MAX_FIELDS).fold(
        embed.description(format!(
            "{count} \u{b7} {} \u{b7} {}",
            upload(&sessions),
            lan(&sessions)
        )),
        |embed, (s, user)| {
            let (name, value) = field(s, user);
            embed.field(name, value, false)
        },
    );
    if sessions.len() > MAX_FIELDS {
        embed = embed.footer(CreateEmbedFooter::new(format!(
            "{} more not shown",
            sessions.len() - MAX_FIELDS
        )));
    }
    Ok(embed)
}

/// How to show each session's user: a mention if they linked a Discord account in Seerr and are
/// in this server, else their Jellyfin name. A mention in an embed never pings.
async fn users(ctx: Context<'_>, sessions: &[Session]) -> Vec<String> {
    let data = ctx.data();
    let jellyfin_ids: Vec<&str> = sessions
        .iter()
        .filter_map(|s| s.user_id.as_deref())
        .collect();
    let discord_ids = match ctx.guild_id() {
        Some(_) => data
            .seerr
            .discord_ids(&data.http, &jellyfin_ids)
            .await
            .inspect_err(|e| warn!("Fetching Discord ids from Seerr: {e:#}"))
            .unwrap_or_default(),
        None => HashMap::new(),
    };

    let mut members: HashMap<UserId, bool> = HashMap::new();
    let mut users = Vec::with_capacity(sessions.len());
    for s in sessions {
        let discord = s
            .user_id
            .as_deref()
            .and_then(|id| discord_ids.get(&seerr::normalize(id)))
            .copied();
        let mention = match (discord, ctx.guild_id()) {
            (Some(id), Some(guild)) => {
                let member = match members.get(&id) {
                    Some(&member) => member,
                    None => {
                        let member = guild.member(ctx, id).await.is_ok();
                        members.insert(id, member);
                        member
                    }
                };
                member.then(|| id.mention().to_string())
            }
            _ => None,
        };
        users.push(mention.unwrap_or_else(|| escape(s.user_name.as_deref().unwrap_or("Someone"))));
    }
    users
}

/// "Upload: **32.1** Mbps" for remote streams that aren't paused
fn upload(sessions: &[Session]) -> String {
    format!("Upload: **{}** Mbps", mbps(total(sessions, false)))
}

/// "LAN: **10.2** Mbps" for local streams that aren't paused
fn lan(sessions: &[Session]) -> String {
    format!("LAN: **{}** Mbps", mbps(total(sessions, true)))
}

/// Bits per second of the unpaused local or remote streams
fn total(sessions: &[Session], local: bool) -> u64 {
    sessions
        .iter()
        .filter(|s| s.is_local() == local && !s.play_state.is_paused)
        .filter_map(Session::bitrate)
        .sum()
}

/// The field name and value for one session. Embed field names can't show mentions, so the
/// title goes in the name and the user starts the value:
///
/// Show S01 E02
/// @user · ▶ 12:34 / 52:10
/// Transcode (video) · 8.1 Mbps · Jellyfin Web on Firefox · LAN
fn field(s: &Session, user: &str) -> (String, String) {
    let Some(item) = &s.now_playing_item else {
        return (String::new(), String::new());
    };

    let title = match (item.r#type.as_deref(), &item.series_name) {
        (Some("Episode"), Some(series)) => {
            let mut title = escape(series);
            if let (Some(season), Some(episode)) = (item.parent_index_number, item.index_number) {
                title = format!("{title} S{season:02} E{episode:02}");
            }
            title
        }
        _ => {
            let year = item.production_year.map(|y| format!(" ({y})"));
            format!("{}{}", escape(&item.name), year.unwrap_or_default())
        }
    };

    let icon = if s.play_state.is_paused {
        '\u{23f8}'
    } else {
        '\u{25b6}'
    };
    let position = s.play_state.position_ticks.unwrap_or_default();
    let length = item.run_time_ticks.map(|t| format!(" / {}", clock(t)));
    let progress = format!("{icon} {}{}", clock(position), length.unwrap_or_default());

    let mut delivery = vec![method(s)];
    delivery.extend(s.bitrate().map(|b| format!("{} Mbps", mbps(b))));
    let device = match (&s.client, &s.device_name) {
        (Some(client), Some(device)) => Some(format!("{client} on {device}")),
        (client, device) => client.as_ref().or(device.as_ref()).cloned(),
    };
    delivery.extend(device.map(|d| escape(&d)));
    if s.is_local() {
        delivery.push("LAN".into());
    }

    let value = format!("{user} \u{b7} {progress}\n{}", delivery.join(" \u{b7} "));
    (title, value)
}

/// "Direct play", "Direct stream", or "Transcode" with what's being re-encoded
fn method(s: &Session) -> String {
    match s.play_state.play_method.as_deref() {
        Some("DirectPlay") => "Direct play".into(),
        Some("DirectStream") => "Direct stream".into(),
        Some("Transcode") => {
            let parts: Vec<&str> = s
                .transcoding_info
                .iter()
                .flat_map(|t| {
                    [
                        (!t.is_video_direct).then_some("video"),
                        (!t.is_audio_direct).then_some("audio"),
                    ]
                })
                .flatten()
                .collect();
            if parts.is_empty() {
                "Transcode".into()
            } else {
                format!("Transcode ({})", parts.join(", "))
            }
        }
        _ => "Unknown".into(),
    }
}

/// Bits per second as megabits with one decimal: "8.1"
#[allow(clippy::cast_precision_loss)]
fn mbps(bits: u64) -> String {
    format!("{:.1}", bits as f64 / 1e6)
}

/// Ticks as "1:02:03", or "2:03" under an hour
fn clock(ticks: u64) -> String {
    let secs = jellyfin::seconds(ticks);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
