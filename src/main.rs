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

use std::env;

use serenity::client::{Context, EventHandler};
use serenity::model::{channel::Message, gateway::Ready};
use serenity::{async_trait, Client};
use tracing_subscriber::EnvFilter;

pub type Error = Box<dyn std::error::Error>;

#[tokio::main]
async fn main() -> Result<(), crate::Error> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // NOTE: Intersperse is not yet stable https://github.com/rust-lang/rust/issues/79524
    tracing::trace!(command = %env::args().collect::<Vec<_>>().join(" "));

    // Load the config file into a buffer and deserialize it.
    let mut buf = String::with_capacity(1024);
    let mut path_buf = env::current_dir()?;
    path_buf.push("config.toml");
    let cfg = config::load(path_buf.as_os_str(), &mut buf)?;

    let mut client = Client::builder(&cfg.token).event_handler(Handler).await?;
    client.start().await?;

    Ok(())
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.content == "!ping" {
            if let Err(e) = msg.channel_id.say(&ctx.http, "Pong!").await {
                tracing::error!(error = %e);
            }
        }
    }
    async fn ready(&self, _: Context, ready: Ready) {
        tracing::info!("{} is online", ready.user.name);
    }
}
