#![warn(clippy::all, clippy::cargo, clippy::nursery, rust_2018_idioms)]

use std::fs;
use std::process;
use std::time::Duration;

use anyhow::{Error, Result};
use poise::builtins::register_globally;
use poise::serenity_prelude::oauth::Scope;
use poise::serenity_prelude::{self as serenity, Activity, GatewayIntents, Permissions};
use poise::{EditTracker, FrameworkOptions, PrefixFrameworkOptions};
use serde::Deserialize;
use tracing::{error, info, warn, Level};

mod commands;

#[derive(Debug, Deserialize)]
pub struct Config {
    log_level: Option<String>,
    token: String,
    activity: Option<ConfigActivity>,
}

#[derive(Debug, Deserialize)]
pub struct ConfigActivity {
    #[serde(rename = "type")]
    _type: Option<String>,
    name: Option<String>,
    streaming_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Data {}
type Context<'a> = poise::Context<'a, Data, Error>;
type Framework = poise::Framework<Data, Error>;
type FrameworkError<'a> = poise::FrameworkError<'a, Data, Error>;

#[tokio::main]
async fn main() {
    process::exit(match run().await {
        Ok(_) => 0,
        Err(e) => {
            error!("{e}");
            1
        }
    });
}

async fn run() -> Result<()> {
    let cfg = toml::from_str::<Config>(&fs::read_to_string("config.toml")?)?;

    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(
            cfg.log_level
                .as_ref()
                .map_or(Level::INFO, |lvl| match lvl.as_str() {
                    "error" => Level::ERROR,
                    "warn" => Level::WARN,
                    "debug" => Level::DEBUG,
                    "trace" => Level::TRACE,
                    _ => Level::INFO,
                }),
        )
        .init();

    let prefix_options = PrefixFrameworkOptions {
        prefix: None,
        edit_tracker: Some(EditTracker::for_timespan(Duration::from_secs(3600))),
        ..PrefixFrameworkOptions::default()
    };
    let options = FrameworkOptions {
        commands: vec![commands::ping()],
        on_error: move |e| Box::pin(on_error(e)),
        pre_command: move |ctx| Box::pin(pre_command(ctx)),
        command_check: Some(move |ctx| Box::pin(command_check(ctx))),
        prefix_options,
        ..FrameworkOptions::default()
    };

    Framework::builder()
        .token(cfg.token)
        .setup(move |c, r, f| Box::pin(setup(c, r, f, cfg.activity)))
        .options(options)
        .intents(GatewayIntents::non_privileged())
        .run_autosharded()
        .await?;

    Ok(())
}

async fn on_error(err: FrameworkError<'_>) {
    match err {
        poise::FrameworkError::Command { error, ctx } => {
            error!("{error}");
            drop(
                ctx.send(|m| {
                    m.ephemeral(true)
                        .content("Error executing command \u{1fae4}")
                })
                .await,
            );
        }
        _ => {
            error!("{err}");
            error!("{err:?}");
            if let Some(ctx) = err.ctx() {
                drop(
                    ctx.send(|m| m.ephemeral(true).content("An error occurred \u{1fae4}"))
                        .await,
                );
            }
        }
    }
}

#[tracing::instrument(skip_all)]
async fn pre_command(ctx: Context<'_>) {
    info!("{} used `{}`", ctx.author().tag(), ctx.invocation_string());
}

async fn command_check(ctx: Context<'_>) -> Result<bool> {
    Ok(!ctx.author().bot)
}

#[tracing::instrument(skip_all)]
async fn setup(
    ctx: &serenity::Context,
    ready: &serenity::Ready,
    framework: &Framework,
    cfg: Option<ConfigActivity>,
) -> Result<Data> {
    register_globally(ctx, &framework.options().commands).await?;

    info!(
        "{} available in {} servers",
        ready.user.tag(),
        ready.guilds.len()
    );

    invite_url(ctx, ready).await;
    if let Some(cfg) = &cfg {
        activity(ctx, cfg).await;
    }

    Ok(Data {})
}

/// Generate invite URL with permissions.
#[tracing::instrument(skip_all)]
async fn invite_url(ctx: &serenity::Context, ready: &serenity::Ready) {
    ready
        .user
        .invite_url_with_oauth2_scopes(
            &ctx,
            Permissions::ADD_REACTIONS
                | Permissions::SEND_MESSAGES
                | Permissions::SEND_MESSAGES_IN_THREADS
                | Permissions::VIEW_CHANNEL,
            &[Scope::Bot, Scope::ApplicationsCommands],
        )
        .await
        .map_or_else(
            |_| warn!("Could not create a bot invite URL"),
            |url| info!("{url}"),
        )
}

/// Set bot activity.
#[tracing::instrument(skip_all)]
async fn activity(ctx: &serenity::Context, cfg: &ConfigActivity) {
    let activity = match (&cfg._type, &cfg.name) {
        (Some(type_), Some(name)) => match type_.as_str() {
            "competing" => Some(Activity::competing(name)),
            "listening" => Some(Activity::listening(name)),
            "playing" => Some(Activity::playing(name)),
            "streaming" => cfg.streaming_url.as_ref().map_or_else(
                || {
                    warn!("Missing TEDBOT_ACTIVITY_STREAMING env var");
                    None
                },
                |streaming| Some(Activity::streaming(name, streaming)),
            ),
            "watching" => Some(Activity::watching(name)),
            _ => None,
        },
        _ => None,
    };

    if let Some(activity) = activity {
        info!("{:?} {}", &activity.kind, &activity.name);
        ctx.set_activity(activity).await;
    } else {
        info!("No complete activity declared");
    }
}
