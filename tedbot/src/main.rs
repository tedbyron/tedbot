//! tedbot, a Discord bot for Ted's home media server.
//!
//! Reads its config (see `config.example.toml`), registers [`poise_jellyfin`]'s slash commands
//! globally, and runs until the gateway connection fails.

use std::{env, fs, process, time::Duration};

use anyhow::{Context as _, Error, Result};
use poise::{
    CreateReply, FrameworkOptions,
    builtins::register_globally,
    serenity_prelude::{self as serenity, ActivityData, GatewayIntents, Permissions},
};
use poise_jellyfin::{MediaServer, RestartBot};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

mod config;

/// How long an HTTP request may take to connect, to Discord or upstream services
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an HTTP request may take in all, so a lost connection fails instead of hanging
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// The media server is the bot's only state
type Data = MediaServer;
type Context<'a> = poise::Context<'a, Data, Error>;
type Framework = poise::Framework<Data, Error>;
type FrameworkError<'a> = poise::FrameworkError<'a, Data, Error>;

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
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(cfg.log_level.into())
                .from_env_lossy(),
        )
        .init();

    let options = FrameworkOptions {
        commands: poise_jellyfin::commands(),
        on_error: |e| Box::pin(on_error(e)),
        pre_command: |ctx| Box::pin(pre_command(ctx)),
        event_handler: |ctx, event| Box::pin(poise_jellyfin::handle_event(ctx, event)),
        ..FrameworkOptions::default()
    };

    let activity = cfg.discord.activity;
    let http = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    // The client then stops, `main` exits, and the service manager (launchd's `KeepAlive` in
    // dotfiles) starts the bot again
    let restart = RestartBot::new(|shards| async move { shards.shutdown_all().await });
    let data =
        MediaServer::new(&cfg.server, cfg.discord.admins, http, Some(restart)).context(path)?;
    let webhook = poise_jellyfin::Listener::bind(&cfg.server).await?;
    let framework = Framework::builder()
        .setup(move |ctx, ready, framework| {
            Box::pin(setup(ctx, ready, framework, activity, webhook, data))
        })
        .options(options)
        .build();

    // serenity's default client has no timeout, so a lost connection could hang every reply
    let discord = reqwest_012::Client::builder()
        .use_rustls_tls()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()?;
    let discord = serenity::HttpBuilder::new(cfg.discord.token)
        .client(discord)
        .build();
    serenity::ClientBuilder::new_with_http(discord, GatewayIntents::non_privileged())
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
    } else if let poise::FrameworkError::CommandCheckFailed {
        error: None, ctx, ..
    } = err
    {
        // Already told why, if they're not a bot
        info!("{} can't use `{}`", ctx.author().name, ctx.command().name);
    } else if let poise::FrameworkError::EventHandler { error, event, .. } = err {
        // Its `Display` leaves out the error itself
        error!("Handling {}: {error:#}", event.snake_case_name());
    } else {
        error!("{err}");
        if let Some(ctx) = err.ctx() {
            drop(ctx.send(reply("An error occurred \u{1fae4}")).await);
        }
    }
}

/// Logs who ran which command
#[tracing::instrument(skip_all)]
async fn pre_command(ctx: Context<'_>) {
    info!("{} used `{}`", ctx.author().name, ctx.invocation_string());
}

/// Runs once the gateway is ready: registers commands, logs the invite link, sets the activity,
/// and starts serving the webhook. Returns the shared [`Data`] for poise to hand to commands.
#[tracing::instrument(skip_all)]
async fn setup(
    ctx: &serenity::Context,
    ready: &serenity::Ready,
    framework: &Framework,
    cfg: Option<config::DiscordActivityConfig>,
    webhook: Option<poise_jellyfin::Listener>,
    data: MediaServer,
) -> Result<Data> {
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
