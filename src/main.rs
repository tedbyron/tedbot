#![warn(clippy::all, clippy::cargo, rust_2018_idioms)]
#![windows_subsystem = "console"]
#![doc = include_str!("../README.md")]

mod handlers;
mod util;

use std::env;
use std::process;
use std::time::Duration;

use anyhow::{bail, Result};
use poise::serenity_prelude::{validate_token, GatewayIntents};
use poise::{EditTracker, PrefixFrameworkOptions};
use tracing::{error, trace};
use tracing_subscriber::filter::{EnvFilter, LevelFilter};

#[derive(Debug, Clone)]
pub struct Data {
    tag: String,
}
pub type Context<'a> = poise::Context<'a, Data, Error>;
pub type Error = anyhow::Error;
pub type Framework = poise::Framework<Data, Error>;
pub type FrameworkError<'a> = poise::FrameworkError<'a, Data, Error>;
type FrameworkOptions = poise::FrameworkOptions<Data, Error>;

#[tokio::main]
async fn main() {
    process::exit(match run().await {
        Ok(()) => 0,
        Err(e) => {
            error!("{:?}", e);
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
        Ok(token) => match validate_token(&token) {
            Ok(()) => token,
            Err(_) => bail!("Invalid TEDBOT_TOKEN env var"),
        },
        Err(_) => bail!("Missing TEDBOT_TOKEN env var"),
    };
    let app_id = match env::var("TEDBOT_APPLICATION_ID") {
        Ok(id) => match id.parse::<u64>() {
            Ok(id) => id,
            Err(_) => bail!("INVALID TEDBOT_APPLICATION_ID env var"),
        },
        Err(_) => bail!("Missing TEDBOT_APPLICATION_ID env var"),
    };

    let options = FrameworkOptions {
        commands: vec![],
        on_error: |err| Box::pin(async move { error!(?err) }),
        command_check: Some(|ctx| Box::pin(handlers::command_check(ctx))),
        prefix_options: PrefixFrameworkOptions {
            prefix: None,
            edit_tracker: Some(EditTracker::for_timespan(Duration::from_secs(3600))),
            ..PrefixFrameworkOptions::default()
        },
        ..FrameworkOptions::default()
    };

    Framework::builder()
        .token(token)
        .setup(|c, r, f| Box::pin(handlers::setup(c, r, f)))
        .options(options)
        .intents(GatewayIntents::non_privileged() | GatewayIntents::MESSAGE_CONTENT)
        .run_autosharded()
        .await?;

    Ok(())
}
