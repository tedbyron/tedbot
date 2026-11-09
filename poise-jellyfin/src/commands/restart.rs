//! `/restart`: restarts Jellyfin, or the bot itself if it can, for admins only.
//!
//! Discord hides it from members without Administrator unless the server's Integrations settings
//! give admins access too; either way the bot only lets `discord.admins` restart. Asks to confirm with
//! what it would interrupt, then waits for Jellyfin's health check to pass again. Jellyfin restarts
//! with its configured restart command, or else restarts itself. The bot restarts however it was
//! given ([`crate::RestartBot`]).

use std::time::{Duration, Instant};

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
use crate::{Context, Data, Error, MediaServer, Result, error::chain};

/// How long the confirmation's buttons work
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);
/// How long to wait for the service to come back
const START_TIMEOUT: Duration = Duration::from_secs(3 * 60);
/// How often to check whether it's back
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// What `/restart` can restart
#[derive(Clone, Copy, Debug, ChoiceParameter)]
enum Service {
    #[name = "Jellyfin"]
    Jellyfin,
    #[name = "This bot"]
    Bot,
}

impl Service {
    /// What restarting it interrupts, for the confirmation
    async fn interrupts(self, data: &MediaServer) -> String {
        match self {
            Self::Jellyfin => match data.jellyfin.playing(&data.http).await {
                Ok(sessions) => match sessions.len() {
                    0 => "Nothing is playing.".to_owned(),
                    1 => "This stops the 1 stream playing.".to_owned(),
                    n => format!("This stops the {n} streams playing."),
                },
                Err(e) => {
                    warn!("Fetching sessions: {}", chain(&e));
                    "Jellyfin isn't answering, so streams can't be counted.".to_owned()
                }
            },
            Self::Bot => "Buttons on command replies already sent stop working (request \
                             posts' keep working), and Seerr notifications sent while it's down \
                             are missed."
                .to_owned(),
        }
    }
}

/// Restart a media server service
#[command(
    check = "super::linked",
    slash_command,
    ephemeral,
    guild_only,
    default_member_permissions = "ADMINISTRATOR"
)]
pub async fn restart<U: Data, E: From<Error>>(
    ctx: Context<'_, U, E>,
    #[description = "Service to restart"] service: Service,
) -> Result<(), E> {
    restart_run(ctx, service).await.map_err(E::from)
}

/// [`restart()`], failing with [`Error`](crate::Error)
async fn restart_run<U: Data, E: From<Error>>(
    ctx: Context<'_, U, E>,
    service: Service,
) -> Result<()> {
    let name = match service {
        Service::Jellyfin => service.name().to_owned(),
        Service::Bot => ctx.cache().current_user().name.clone(),
    };
    if !is_admin(ctx) {
        ctx.say("Only admins can restart services").await?;
        return Ok(());
    }
    let data = ctx.data().server();
    if matches!(service, Service::Bot) && data.restart_bot.is_none() {
        ctx.say(format!("{name} can't restart itself")).await?;
        return Ok(());
    }
    ctx.defer_ephemeral().await?;

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
        .content(match (restart, service) {
            (true, Service::Bot) => "Restarting, back in a few seconds\u{2026}".to_owned(),
            (true, Service::Jellyfin) => format!("Restarting {name}\u{2026}"),
            (false, _) => "Not restarted".to_owned(),
        })
        .components(vec![]);
    press
        .create_response(ctx, CreateInteractionResponse::UpdateMessage(update))
        .await?;
    if !restart {
        return Ok(());
    }
    if let (Service::Bot, Some(restart)) = (service, &data.restart_bot) {
        info!("{} restarted the bot", ctx.author().name);
        (restart.0)(ctx.framework().shard_manager().clone()).await;
        return Ok(());
    }

    let started = Instant::now();
    let restarted = match &data.jellyfin_restart {
        Some(command) => run(command).await,
        None => data.jellyfin.restart(&data.http).await.map_err(Error::from),
    };
    let content = if let Err(e) = restarted {
        error!("Restarting {name}: {}", chain(&e));
        format!("Couldn't restart {name} \u{1fae4}")
    } else {
        info!("{} restarted {name}", ctx.author().name);
        sleep(POLL_INTERVAL).await;
        loop {
            if data.jellyfin.healthy(&data.http).await {
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

/// Runs `command` (program, then arguments), failing if it does
async fn run(command: &[String]) -> Result<()> {
    let (program, args) = command
        .split_first()
        .ok_or(Error::Config("jellyfin.restart_command is empty"))?;
    let out = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|source| Error::Spawn {
            command: command.join(" "),
            source,
        })?;
    if !out.status.success() {
        return Err(Error::Command {
            command: command.join(" "),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(())
}
