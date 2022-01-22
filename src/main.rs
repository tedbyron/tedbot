#![forbid(unsafe_code)]
#![warn(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    rust_2018_idioms
)]
#![windows_subsystem = "console"]
#![doc = include_str!("../README.md")]

mod config;
mod handler;

use std::{env, process};

use serenity::Client;
use tracing_subscriber::EnvFilter;

pub type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
pub type Result<T> = std::result::Result<T, Error>;

#[tokio::main]
async fn main() {
    process::exit(match run().await {
        Ok(_) => 0,
        Err(e) => {
            tracing::error!("{:?}", e);
            1
        }
    });
}

async fn run() -> crate::Result<()> {
    #[cfg(feature = "dotenv")]
    dotenv::dotenv()?;

    // Tracing level.
    if env::var("TEDBOT_LOG").is_err() {
        env::set_var("TEDBOT_LOG", "INFO");
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_env("TEDBOT_LOG"))
        .init();

    let cfg = config::load()?;

    // NOTE: Intersperse is not yet stable https://github.com/rust-lang/rust/issues/79524
    tracing::trace!(command = %env::args().collect::<Vec<_>>().join(" "));

    let mut client = Client::builder(&cfg.token)
        .event_handler(handler::Handler)
        .await?;
    client.start_autosharded().await?;

    Ok(())
}
