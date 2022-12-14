#![warn(clippy::all, clippy::cargo, rust_2018_idioms)]
#![windows_subsystem = "console"]
#![doc = include_str!("../README.md")]

mod commands;
mod util;

use std::env;
use std::process;
use std::time::Duration;

use anyhow::{bail, Error, Result};
use poise::serenity_prelude::oauth::Scope;
use poise::serenity_prelude::{self as serenity, Activity, GatewayIntents, Permissions};
use poise::{EditTracker, FrameworkOptions, PrefixFrameworkOptions};
use tracing::{debug, error, info, trace, warn};
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

#[derive(Debug, Clone)]
struct Data {}
type Context<'a> = poise::Context<'a, Data, Error>;
type Framework = poise::Framework<Data, Error>;

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
    #[cfg(feature = "dotenv")]
    dotenvy::dotenv()?;

    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(EnvFilter::try_from_env("TEDBOT_LOG").unwrap_or_else(|_| {
            EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy()
        }))
        .init();

    // NOTE: https://github.com/rust-lang/rust/issues/79524.
    trace!(command = %env::args().collect::<Vec<_>>().join(" "));

    let token = match env::var("TEDBOT_TOKEN") {
        Ok(token) => token,
        Err(_) => bail!("Missing TEDBOT_TOKEN env var"),
    };
    let prefix_options = PrefixFrameworkOptions {
        prefix: None,
        edit_tracker: Some(EditTracker::for_timespan(Duration::from_secs(3600))),
        ..PrefixFrameworkOptions::default()
    };
    let options = FrameworkOptions {
        on_error: move |err| Box::pin(async move { error!(?err) }),
        commands: vec![],
        command_check: Some(move |ctx| Box::pin(command_check(ctx))),
        prefix_options,
        ..FrameworkOptions::default()
    };

    Framework::builder()
        .token(token)
        .setup(|c, r, _| Box::pin(setup(c, r)))
        .options(options)
        .intents(GatewayIntents::non_privileged())
        .run_autosharded()
        .await?;

    Ok(())
}

#[tracing::instrument(skip_all)]
async fn command_check(ctx: crate::Context<'_>) -> Result<bool> {
    Ok(!ctx.author().bot)
}

#[tracing::instrument(skip_all)]
async fn setup(ctx: &serenity::Context, ready: &serenity::Ready) -> Result<Data> {
    debug!(guilds = ?ready.guilds);
    info!("Logged in as {}", ready.user.tag());

    invite_url(ctx, ready).await;
    activity_from_env(ctx).await;

    Ok(Data {})
}

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

#[tracing::instrument(skip_all)]
async fn activity_from_env(ctx: &serenity::Context) {
    let activity = match (
        env::var("TEDBOT_ACTIVITY_TYPE"),
        env::var("TEDBOT_ACTIVITY_NAME"),
    ) {
        (Ok(type_), Ok(name)) => match type_.as_str() {
            "competing" => Some(Activity::competing(name)),
            "listening" => Some(Activity::listening(name)),
            "playing" => Some(Activity::playing(name)),
            "streaming" => env::var("TEDBOT_ACTIVITY_STREAMING").map_or_else(
                |_| {
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
        ctx.set_activity(activity).await;
    }
}
