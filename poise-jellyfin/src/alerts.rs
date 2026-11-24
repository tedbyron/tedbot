//! Alertmanager's webhook, for alerts that need a button. Posts each alert in `admin_channels`
//! styled like Alertmanager's own Discord posts: the summary as the title, red while firing, and
//! green once resolved with the `resolved` annotation (or "Resolved: " and the summary).
//!
//! A firing alert with a `restart = "jellyfin"` label gets a Restart Jellyfin button, which
//! does what `/restart jellyfin` does, for admins only: an ephemeral confirmation with how many
//! streams it stops, then the restart. The alert's buttons are taken off once Jellyfin is back
//! up, or once the alert resolves. Which posts have them is only kept until the bot restarts, so
//! a press on a post from before then first checks Jellyfin still needs restarting.
//!
//! An alert already posted in the last hour isn't posted again, so a notification Alertmanager
//! retries after it was partly posted (or posted just as it timed out) doesn't post it twice.
//!
//! Point an Alertmanager `webhook_configs` receiver (with `send_resolved`) at
//! `http://localhost:<port>/alertmanager`, the Seerr webhook's listener.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::http::StatusCode;
use poise::serenity_prelude::{
    self as serenity, ButtonStyle, ChannelId, Colour, ComponentInteraction, CreateActionRow,
    CreateAllowedMentions, CreateButton, CreateEmbed, CreateInteractionResponse,
    CreateInteractionResponseMessage, CreateMessage, EditInteractionResponse, EditMessage, Http,
    MessageId,
};
use serde::Deserialize;
use tracing::{info, warn};

use crate::{
    MediaServer, Result,
    commands::{
        is_admin_user,
        restart::{jellyfin_interrupts, restart_jellyfin},
    },
};

/// Alertmanager's colour for firing alerts
pub const FIRING: Colour = Colour::new(0x0099_2d22);
/// Alertmanager's colour for resolved alerts
pub const RESOLVED: Colour = Colour::new(0x002e_cc71);
/// Custom id prefix of the buttons on alert posts and their confirmations
const PREFIX: &str = "alert-restart:";
/// How long a posted alert isn't posted again. Alertmanager's retries come well within it, and
/// its reminders for alerts still firing (`repeat_interval`) after.
const REPOST_AFTER: Duration = Duration::from_secs(60 * 60);

/// A post's channel and message
type Post = (ChannelId, MessageId);

/// Alert posts with buttons, and the alerts posted lately
#[derive(Debug, Default)]
pub struct Posts {
    /// Each firing alert's posts with a button, by fingerprint
    buttons: Mutex<HashMap<String, Vec<Post>>>,
    /// When each alert was posted, by [`Alert::key`]
    sent: Mutex<HashMap<(String, String, String), Instant>>,
}

/// The parts of Alertmanager's webhook payload the bot reads
#[derive(Debug, Deserialize)]
struct Notification {
    alerts: Vec<Alert>,
}

#[derive(Debug, Deserialize)]
struct Alert {
    /// `firing` or `resolved`
    status: String,
    #[serde(default)]
    labels: HashMap<String, String>,
    #[serde(default)]
    annotations: HashMap<String, String>,
    /// When it started firing, which tells a later firing of the same alert apart
    #[serde(rename = "startsAt", default)]
    starts_at: String,
    /// Identifies the alert across notifications
    fingerprint: String,
}

impl Alert {
    fn firing(&self) -> bool {
        self.status != "resolved"
    }

    /// What's the same when Alertmanager sends this alert again
    fn key(&self) -> (String, String, String) {
        (
            self.fingerprint.clone(),
            self.status.clone(),
            self.starts_at.clone(),
        )
    }

    fn annotation(&self, name: &str) -> Option<&str> {
        self.annotations
            .get(name)
            .map(String::as_str)
            .filter(|s| !s.is_empty())
    }

    /// The post's title, as Alertmanager's template in dotfiles' `alertmanager.nix` has it
    fn title(&self) -> String {
        let summary = self.annotation("summary").unwrap_or("Alert");
        if self.firing() {
            summary.to_owned()
        } else {
            self.annotation("resolved")
                .map_or_else(|| format!("Resolved: {summary}"), str::to_owned)
        }
    }

    /// The post's button, if it gets one
    fn button(&self) -> Option<CreateButton> {
        (self.firing() && self.labels.get("restart").map(String::as_str) == Some("jellyfin")).then(
            || {
                CreateButton::new(format!("{PREFIX}jellyfin:{}", self.fingerprint))
                    .label("Restart Jellyfin")
                    .style(ButtonStyle::Danger)
            },
        )
    }
}

/// Posts each alert in Alertmanager's notification `body` in the admin channels. Answers 502 if
/// an alert couldn't be posted anywhere, so Alertmanager tries again.
pub async fn notify(discord: &Http, data: &MediaServer, body: &[u8]) -> StatusCode {
    let notification: Notification = match serde_json::from_slice(body) {
        Ok(n) => n,
        Err(e) => {
            warn!("Reading Alertmanager's webhook: {e}");
            return StatusCode::BAD_REQUEST;
        }
    };
    let mut status = StatusCode::OK;
    for alert in &notification.alerts {
        {
            let mut sent = data.alerts.sent.lock().unwrap();
            sent.retain(|_, at| at.elapsed() < REPOST_AFTER);
            if sent.contains_key(&alert.key()) {
                info!("Alertmanager resent {} ({})", alert.title(), alert.status);
                continue;
            }
        }
        info!("Alertmanager sent {} ({})", alert.title(), alert.status);
        if !alert.firing() {
            take_buttons(discord, data, &alert.fingerprint, None).await;
        }
        let embed = CreateEmbed::new()
            .title(alert.title())
            .colour(if alert.firing() { FIRING } else { RESOLVED });
        let mut message = CreateMessage::new()
            .embed(embed)
            .allowed_mentions(CreateAllowedMentions::new());
        if let Some(button) = alert.button() {
            message = message.components(vec![CreateActionRow::Buttons(vec![button])]);
        }
        let mut sent = Vec::new();
        for &channel in &data.channels.admin_channels {
            match channel.send_message(discord, message.clone()).await {
                Ok(m) => sent.push((channel, m.id)),
                Err(e) => warn!("Posting an alert in channel {channel}: {e}"),
            }
        }
        if sent.is_empty() {
            if !data.channels.admin_channels.is_empty() {
                status = StatusCode::BAD_GATEWAY;
            }
            continue;
        }
        data.alerts
            .sent
            .lock()
            .unwrap()
            .insert(alert.key(), Instant::now());
        if alert.button().is_some() {
            data.alerts
                .buttons
                .lock()
                .unwrap()
                .entry(alert.fingerprint.clone())
                .or_default()
                .extend(sent);
        }
    }
    status
}

/// Takes the buttons off alert `fingerprint`'s posts, and off `also` if it's one from before the
/// bot started
async fn take_buttons(discord: &Http, data: &MediaServer, fingerprint: &str, also: Option<Post>) {
    let mut posts = data
        .alerts
        .buttons
        .lock()
        .unwrap()
        .remove(fingerprint)
        .unwrap_or_default();
    if let Some(post) = also
        && !posts.contains(&post)
    {
        posts.push(post);
    }
    for (channel, message) in posts {
        let edit = EditMessage::new().components(vec![]);
        if let Err(e) = channel.edit_message(discord, message, edit).await {
            warn!("Removing the button from alert post {message}: {e}");
        }
    }
}

/// Handles presses on alert posts' buttons and their confirmations; ignores other presses. The
/// buttons carry the alert's fingerprint, and the confirmation's the alert post too, to take the
/// buttons off once Jellyfin's back up.
pub async fn press(
    ctx: &serenity::Context,
    data: &MediaServer,
    press: &ComponentInteraction,
) -> Result<()> {
    let Some(rest) = press.data.custom_id.strip_prefix(PREFIX) else {
        return Ok(());
    };
    let mut parts = rest.split(':');
    let (Some("jellyfin"), Some(fingerprint)) = (parts.next(), parts.next()) else {
        return Ok(());
    };
    if !is_admin_user(data, press.user.id) {
        let reply = CreateInteractionResponseMessage::new()
            .content("Only admins can restart services")
            .ephemeral(true);
        press
            .create_response(ctx, CreateInteractionResponse::Message(reply))
            .await?;
        return Ok(());
    }
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (None, ..) => confirm(ctx, data, press, fingerprint).await,
        (Some(channel), Some(message), Some(action), None) => {
            let (Ok(channel), Ok(message)) = (channel.parse(), message.parse()) else {
                return Ok(());
            };
            let post = (ChannelId::new(channel), MessageId::new(message));
            confirmed(ctx, data, press, fingerprint, post, action == "restart").await
        }
        _ => Ok(()),
    }
}

/// The alert post's button: confirms, just for whoever pressed. A post from before the bot
/// started may be for an alert that's resolved since, so if Jellyfin's up and needs no restart,
/// its button is taken off instead.
async fn confirm(
    ctx: &serenity::Context,
    data: &MediaServer,
    press: &ComponentInteraction,
    fingerprint: &str,
) -> Result<()> {
    press
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await?;
    let post = (press.channel_id, press.message.id);
    let known = data
        .alerts
        .buttons
        .lock()
        .unwrap()
        .get(fingerprint)
        .is_some_and(|posts| posts.contains(&post));
    if !known && matches!(data.jellyfin.pending_restart(&data.http).await, Ok(false)) {
        take_buttons(&ctx.http, data, fingerprint, Some(post)).await;
        let edit = EditInteractionResponse::new().content("Jellyfin doesn't need a restart now");
        press.edit_response(ctx, edit).await?;
        return Ok(());
    }

    let id = format!("{PREFIX}jellyfin:{fingerprint}:{}:{}", post.0, post.1);
    let buttons = CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{id}:restart"))
            .label("Restart")
            .style(ButtonStyle::Danger),
        CreateButton::new(format!("{id}:cancel"))
            .label("Cancel")
            .style(ButtonStyle::Secondary),
    ]);
    let edit = EditInteractionResponse::new()
        .content(format!(
            "Restart jellyfin? {}",
            jellyfin_interrupts(data).await
        ))
        .components(vec![buttons]);
    press.edit_response(ctx, edit).await?;
    Ok(())
}

/// The confirmation's buttons: restarts if `restart`, then takes the alert's buttons off once
/// Jellyfin's back up
async fn confirmed(
    ctx: &serenity::Context,
    data: &MediaServer,
    press: &ComponentInteraction,
    fingerprint: &str,
    post: Post,
    restart: bool,
) -> Result<()> {
    let update = CreateInteractionResponseMessage::new()
        .content(if restart {
            "Restarting jellyfin\u{2026}"
        } else {
            "Not restarted"
        })
        .components(vec![]);
    press
        .create_response(ctx, CreateInteractionResponse::UpdateMessage(update))
        .await?;
    if !restart {
        return Ok(());
    }
    let (back, content) = restart_jellyfin(data, &press.user.name).await;
    press
        .edit_response(ctx, EditInteractionResponse::new().content(content))
        .await?;
    if back {
        take_buttons(&ctx.http, data, fingerprint, Some(post)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_buttons() {
        let alert = |status: &str, labels: &str, annotations: &str| -> Alert {
            serde_json::from_str(&format!(
                r#"{{"status": "{status}", "fingerprint": "a", "labels": {labels},
                    "annotations": {annotations}}}"#
            ))
            .unwrap()
        };
        let labels = r#"{"alertname": "JellyfinRestartPending", "restart": "jellyfin"}"#;
        let annotations =
            r#"{"summary": "Jellyfin needs a restart", "resolved": "Jellyfin has been restarted"}"#;
        let firing = alert("firing", labels, annotations);
        assert_eq!(firing.title(), "Jellyfin needs a restart");
        assert!(firing.button().is_some());
        let resolved = alert("resolved", labels, annotations);
        assert_eq!(resolved.title(), "Jellyfin has been restarted");
        assert!(resolved.button().is_none());
        let plain = alert("resolved", "{}", r#"{"summary": "Disk is full"}"#);
        assert_eq!(plain.title(), "Resolved: Disk is full");
        assert!(
            alert("firing", "{}", r#"{"summary": "x"}"#)
                .button()
                .is_none()
        );
    }
}
