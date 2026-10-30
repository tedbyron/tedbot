//! `/restart`: restarts a service's launchd agent, for admins only. Only Jellyfin so far.
//!
//! Discord hides it from members without Administrator unless the server's Integrations settings
//! give the admin role access too; the bot checks the role itself either way. Asks to confirm with
//! what it would interrupt, then waits for the service's health check to pass again.

use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use poise::{
    ChoiceParameter, CreateReply, command,
    serenity_prelude::{
        Builder as _, ButtonStyle, ComponentInteractionCollector, CreateActionRow, CreateButton,
        CreateInteractionResponse, CreateInteractionResponseMessage, EditInteractionResponse,
    },
};
use tokio::{process::Command, time::sleep};
use tracing::{error, info, warn};

use super::{is_admin, remove_buttons};
use crate::{Context, Data};

/// How long the confirmation's buttons work
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for the service to come back
const START_TIMEOUT: Duration = Duration::from_secs(3 * 60);
/// How often to check whether it's back
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// What `/restart` can restart
#[derive(Clone, Copy, Debug, ChoiceParameter)]
pub enum Service {
    #[name = "Jellyfin"]
    Jellyfin,
}

impl Service {
    /// Its launchd agent's label
    fn label(self, data: &Data) -> &str {
        match self {
            Self::Jellyfin => &data.jellyfin.launchd_label,
        }
    }

    /// What restarting it interrupts, for the confirmation
    async fn interrupts(self, data: &Data) -> String {
        match self {
            Self::Jellyfin => match data.jellyfin.playing(&data.http).await {
                Ok(sessions) => match sessions.len() {
                    0 => "Nothing is playing.".to_owned(),
                    1 => "This stops the 1 stream playing.".to_owned(),
                    n => format!("This stops the {n} streams playing."),
                },
                Err(e) => {
                    warn!("Fetching sessions: {e:#}");
                    "Jellyfin isn't answering, so streams can't be counted.".to_owned()
                }
            },
        }
    }

    /// Whether its health check passes
    async fn healthy(self, data: &Data) -> bool {
        match self {
            Self::Jellyfin => data.jellyfin.healthy(&data.http).await,
        }
    }
}

/// Restart a media server service
#[command(
    slash_command,
    ephemeral,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn restart(
    ctx: Context<'_>,
    #[description = "Service to restart"] service: Service,
) -> Result<()> {
    let name = service.name();
    if !is_admin(ctx).await {
        ctx.say("Only admins can restart services").await?;
        return Ok(());
    }
    ctx.defer_ephemeral().await?;

    let data = ctx.data();
    let label = service.label(data);

    let interrupts = service.interrupts(data).await;
    let id = ctx.id();
    let buttons = CreateActionRow::Buttons(vec![
        CreateButton::new(format!("{id}-restart"))
            .label("Restart")
            .style(ButtonStyle::Danger),
        CreateButton::new(format!("{id}-cancel"))
            .label("Cancel")
            .style(ButtonStyle::Secondary),
    ]);
    let reply = CreateReply::default()
        .content(format!("Restart {name}? {interrupts}"))
        .components(vec![buttons]);
    let message = ctx.send(reply).await?.message().await?.id;

    let Some(press) = ComponentInteractionCollector::new(ctx)
        .message_id(message)
        .timeout(CONFIRM_TIMEOUT)
        .await
    else {
        return remove_buttons(ctx, None).await;
    };
    let restart = press.data.custom_id.ends_with("-restart");
    let update = CreateInteractionResponseMessage::new()
        .content(if restart {
            format!("Restarting {name}\u{2026}")
        } else {
            "Not restarted".to_owned()
        })
        .components(vec![]);
    press
        .create_response(ctx, CreateInteractionResponse::UpdateMessage(update))
        .await?;
    if !restart {
        return Ok(());
    }

    let started = Instant::now();
    let content = if let Err(e) = kickstart(label).await {
        error!("Restarting {name}: {e:#}");
        format!("Couldn't restart {name} \u{1fae4}")
    } else {
        info!("{} restarted {name}", ctx.author().name);
        sleep(POLL_INTERVAL).await;
        loop {
            if service.healthy(data).await {
                break format!("{name} is back up after {}s", started.elapsed().as_secs());
            }
            if started.elapsed() > START_TIMEOUT {
                break format!("{name} restarted, but isn't answering after 3 minutes \u{1fae4}");
            }
            sleep(POLL_INTERVAL).await;
        }
    };
    EditInteractionResponse::new()
        .content(content)
        .execute(ctx, &press.token)
        .await?;
    Ok(())
}

/// Restarts launchd agent `label` in the bot's own session, killing it first if it's running
async fn kickstart(label: &str) -> Result<()> {
    let target = format!("gui/{}/{label}", rustix::process::getuid().as_raw());
    let out = Command::new("/bin/launchctl")
        .args(["kickstart", "-k", &target])
        .output()
        .await?;
    ensure!(
        out.status.success(),
        "launchctl kickstart -k {target}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}
