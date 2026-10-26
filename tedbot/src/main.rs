//! tedbot, a Discord bot for Ted's home media server.
//!
//! Reads its config (see `config.example.toml`), registers the slash commands in [`commands`]
//! globally, and runs until the gateway connection fails.

#![warn(clippy::all, clippy::nursery, rust_2018_idioms)]

use std::{env, fs, process, sync::Arc, time::Duration};

use anyhow::{Context as _, Error, Result};
use poise::{
    CreateReply, FrameworkOptions,
    builtins::register_globally,
    serenity_prelude::{self as serenity, ActivityData, GatewayIntents, Permissions},
};
use tracing::{Level, error, info, warn};

mod announce;
mod arr;
mod commands;
mod config;
mod ersatztv;
mod jellyfin;
mod seerr;
mod webhook;

/// State shared by every command invocation
#[derive(Debug)]
pub struct Data {
    /// One HTTP client for all upstream services, so connections are pooled
    http: reqwest::Client,
    /// ErsatzTV's XMLTV guide, refetched at most once a minute
    guide: ersatztv::GuideCache,
    /// Jellyfin connection info
    jellyfin: jellyfin::Jellyfin,
    /// Seerr connection info
    seerr: seerr::Seerr,
    /// Radarr connection info, if configured
    radarr: Option<arr::Arr>,
    /// Sonarr connection info, if configured
    sonarr: Option<arr::Arr>,
    /// Members with this role count as admins
    admin_role: Option<serenity::RoleId>,
}
// In an `Arc` so the webhook can share it
type Context<'a> = poise::Context<'a, Arc<Data>, Error>;
type Framework = poise::Framework<Arc<Data>, Error>;
type FrameworkError<'a> = poise::FrameworkError<'a, Arc<Data>, Error>;

/// Runs the bot, exiting with status 1 and the error chain if it stops
#[tokio::main]
async fn main() {
    process::exit(match run().await {
        Ok(()) => 0,
        Err(e) => {
            error!("{e:#}");
            eprintln!("{e:#}");
            1
        }
    });
}

/// Loads the config from `$TEDBOT_CONFIG` (default `config.toml`), sets up logging, and runs the
/// Discord client. Only returns on a startup or gateway error.
async fn run() -> Result<()> {
    let path = env::var("TEDBOT_CONFIG").unwrap_or_else(|_| "config.toml".into());
    let cfg = toml::from_str::<config::Config>(
        &fs::read_to_string(&path).with_context(|| format!("reading {path}"))?,
    )
    .with_context(|| format!("parsing {path}"))?;
    anyhow::ensure!(
        !cfg.jellyfin.api_key.is_empty(),
        "{path}: jellyfin.api_key is empty"
    );
    anyhow::ensure!(
        !cfg.seerr.api_key.is_empty(),
        "{path}: seerr.api_key is empty"
    );

    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(
            cfg.log_level
                .as_deref()
                .and_then(|lvl| lvl.parse().ok())
                .unwrap_or(Level::WARN),
        )
        .init();

    let options = FrameworkOptions {
        commands: vec![
            commands::request::request(),
            commands::requests::requests(),
            commands::streams::streams(),
            commands::tv::tv(),
        ],
        on_error: |e| Box::pin(on_error(e)),
        pre_command: |ctx| Box::pin(pre_command(ctx)),
        command_check: Some(|ctx| Box::pin(command_check(ctx))),
        event_handler: |ctx, event| Box::pin(event_handler(ctx, event)),
        ..FrameworkOptions::default()
    };

    let activity = cfg.discord.activity;
    let admin_role = cfg.discord.admin_role;
    let webhook = match cfg.seerr.webhook {
        Some(c) => {
            let targets = announce::Targets {
                requests: c.requests_channels,
                available: c.available_channels,
                admin_role,
            };
            Some(webhook::Listener::bind(c.port, c.forward, targets).await?)
        }
        None => None,
    };
    let http = reqwest::Client::new();
    let data = Data {
        guide: ersatztv::GuideCache::new(http.clone(), cfg.ersatztv.url, Duration::from_secs(60)),
        http,
        jellyfin: jellyfin::Jellyfin {
            url: cfg.jellyfin.url,
            api_key: cfg.jellyfin.api_key,
        },
        seerr: seerr::Seerr {
            url: cfg.seerr.url,
            api_key: cfg.seerr.api_key,
            public_url: cfg.seerr.public_url,
        },
        radarr: cfg.radarr.map(|c| arr::Arr {
            url: c.url,
            api_key: c.api_key,
        }),
        sonarr: cfg.sonarr.map(|c| arr::Arr {
            url: c.url,
            api_key: c.api_key,
        }),
        admin_role,
    };
    let framework = Framework::builder()
        .setup(move |ctx, ready, framework| {
            Box::pin(setup(ctx, ready, framework, activity, webhook, data))
        })
        .options(options)
        .build();

    serenity::ClientBuilder::new(cfg.discord.token, GatewayIntents::non_privileged())
        .framework(framework)
        .await?
        .start_autosharded()
        .await?;

    Ok(())
}

/// Logs the error and tells the user something went wrong, visible only to them. Error details
/// stay in the log.
async fn on_error(err: FrameworkError<'_>) {
    let reply = |content| CreateReply::default().ephemeral(true).content(content);
    if let poise::FrameworkError::Command { error, ctx, .. } = err {
        error!("{error:#}");
        drop(ctx.send(reply("Error executing command \u{1fae4}")).await);
    } else {
        error!("{err}");
        if let Some(ctx) = err.ctx() {
            drop(ctx.send(reply("An error occurred \u{1fae4}")).await);
        }
    }
}

/// Handles presses on request posts, whose buttons outlive any command
async fn event_handler(
    ctx: poise::FrameworkContext<'_, Arc<Data>, Error>,
    event: &serenity::FullEvent,
) -> Result<()> {
    if let serenity::FullEvent::InteractionCreate {
        interaction: serenity::Interaction::Component(press),
    } = event
    {
        commands::requests::post_press(ctx.serenity_context, ctx.user_data, press).await?;
    }
    Ok(())
}

/// Logs who ran which command
#[tracing::instrument(skip_all)]
async fn pre_command(ctx: Context<'_>) {
    info!("{} used `{}`", ctx.author().name, ctx.invocation_string());
}

/// Ignores commands from other bots
#[allow(clippy::unused_async)]
async fn command_check(ctx: Context<'_>) -> Result<bool> {
    Ok(!ctx.author().bot)
}

/// Runs once the gateway is ready: registers commands, logs the invite link, sets the activity,
/// and starts serving the webhook. Returns the shared [`Data`] for poise to hand to commands.
#[tracing::instrument(skip_all)]
async fn setup(
    ctx: &serenity::Context,
    ready: &serenity::Ready,
    framework: &Framework,
    cfg: Option<config::DiscordActivityConfig>,
    webhook: Option<webhook::Listener>,
    data: Data,
) -> Result<Arc<Data>> {
    register_globally(ctx, &framework.options().commands).await?;
    clear_guild_commands(ctx, ready).await;

    info!(
        "{} available in {} servers",
        ready.user.tag(),
        ready.guilds.len()
    );
    info!("{}", invite_url(ready));

    if let Some(cfg) = &cfg {
        activity(ctx, cfg);
    }
    let data = Arc::new(data);
    if let Some(webhook) = webhook {
        webhook.spawn(ctx.http.clone(), data.clone());
    }

    Ok(data)
}

/// Commands are registered globally; remove guild-scoped ones left over from older versions,
/// which Discord keeps showing until they're deleted
async fn clear_guild_commands(ctx: &serenity::Context, ready: &serenity::Ready) {
    for guild in &ready.guilds {
        let stale = match guild.id.get_commands(ctx).await {
            Ok(commands) if commands.is_empty() => continue,
            Ok(commands) => commands,
            Err(e) => {
                warn!("Listing commands in guild {}: {e}", guild.id);
                continue;
            }
        };
        let names: Vec<&str> = stale.iter().map(|c| c.name.as_str()).collect();
        match guild.id.set_commands(ctx, Vec::new()).await {
            Ok(_) => info!(
                "Removed stale commands in guild {}: {}",
                guild.id,
                names.join(", ")
            ),
            Err(e) => warn!("Removing stale commands in guild {}: {e}", guild.id),
        }
    }
}

/// OAuth link that adds the bot to a server with the permissions its commands need
fn invite_url(ready: &serenity::Ready) -> String {
    let permissions = Permissions::ADD_REACTIONS
        | Permissions::EMBED_LINKS
        | Permissions::READ_MESSAGE_HISTORY
        | Permissions::SEND_MESSAGES
        | Permissions::VIEW_CHANNEL;
    format!(
        "https://discord.com/oauth2/authorize?client_id={}&scope=bot+applications.commands&permissions={}",
        ready.application.id,
        permissions.bits()
    )
}

/// Sets the bot's status ("Listening to /commands") from `[discord.activity]`. Logs a warning
/// and sets nothing if a streaming activity has no valid `streaming_url`. An unknown `type`, or a
/// missing `type` or `name` sets nothing.
#[tracing::instrument(skip_all)]
fn activity(ctx: &serenity::Context, cfg: &config::DiscordActivityConfig) {
    let activity = match (cfg.r#type.as_deref(), cfg.name.as_deref()) {
        (Some("competing"), Some(name)) => Some(ActivityData::competing(name)),
        (Some("listening"), Some(name)) => Some(ActivityData::listening(name)),
        (Some("playing"), Some(name)) => Some(ActivityData::playing(name)),
        (Some("watching"), Some(name)) => Some(ActivityData::watching(name)),
        (Some("streaming"), Some(name)) => cfg.streaming_url.as_deref().map_or_else(
            || {
                warn!("Missing discord.activity.streaming_url");
                None
            },
            |url| {
                ActivityData::streaming(name, url)
                    .inspect_err(|e| warn!("Invalid discord.activity.streaming_url: {e}"))
                    .ok()
            },
        ),
        _ => None,
    };

    if let Some(activity) = activity {
        info!("{:?} {}", activity.kind, activity.name);
        ctx.set_activity(Some(activity));
    }
}
