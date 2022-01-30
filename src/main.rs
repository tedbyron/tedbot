#![forbid(unsafe_code)]
#![warn(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    rust_2018_idioms
)]
#![allow(clippy::unreadable_literal)]
#![windows_subsystem = "console"]
#![doc = include_str!("../README.md")]

mod codec;
mod commands;
mod db;
mod handler;
mod util;
mod wordle;

use std::env;
use std::process;

use serenity::client::bridge::gateway::GatewayIntents;
use serenity::client::{self, Client};
use serenity::model::oauth2::OAuth2Scope;
use serenity::model::Permissions;
use tracing_subscriber::EnvFilter;

const INTENTS: GatewayIntents = GatewayIntents::GUILD_MESSAGES;
const SCOPES: &[OAuth2Scope] = &[OAuth2Scope::Bot, OAuth2Scope::ApplicationsCommands];

lazy_static::lazy_static! {
    static ref PERMISSIONS: Permissions =
    Permissions::READ_MESSAGES | Permissions::READ_MESSAGE_HISTORY | Permissions::SEND_MESSAGES | Permissions::ADD_REACTIONS;
}

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type Result<T> = std::result::Result<T, Error>;

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
    // NOTE: Should only be used in development.
    #[cfg(feature = "dotenv")]
    dotenv::dotenv()?;

    // Tracing level.
    if env::var("TEDBOT_LOG").is_err() {
        env::set_var("TEDBOT_LOG", "INFO");
    };

    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(EnvFilter::from_env("TEDBOT_LOG"))
        .init();

    // NOTE: Intersperse is not yet stable https://github.com/rust-lang/rust/issues/79524.
    tracing::trace!(command = %env::args().collect::<Vec<_>>().join(" "));

    // Get and validate discord bot token from env vars.
    let token = match env::var("TEDBOT_TOKEN") {
        Ok(token) => token,
        Err(_) => return Err(Box::from("Missing TEDBOT_TOKEN env var")),
    };
    if client::validate_token(&token).is_err() {
        return Err(Box::from("Invalid TEDBOT_TOKEN env var"));
    };
    let app_id = match env::var("TEDBOT_APPLICATION_ID") {
        Ok(id) => match id.parse::<u64>() {
            Ok(parsed) => parsed,
            Err(_) => return Err(Box::from("INVALID TEDBOT_APPLICATION_ID env var")),
        },
        Err(_) => return Err(Box::from("Missing TEDBOT_APPLICATION_ID env var")),
    };

    // TODO: Guild whitelist.
    // let whitelist = match env::var("TEDBOT_WHITELIST") {
    //     Ok(wl_string) => {
    //         if wl_string.is_empty() {
    //             None
    //         } else {
    //             let wl = wl_string
    //                 .trim()
    //                 .split(|c: char| !c.is_ascii_digit())
    //                 .map(str::parse::<u64>)
    //                 .filter_map(Result::ok)
    //                 .map(GuildId::from)
    //                 .collect::<Vec<_>>();

    //             if wl.is_empty() {
    //                 tracing::warn!("Invalid TEDBOT_WHITELIST env var, ignoring");
    //             }
    //             Some(wl)
    //         }
    //     }
    //     Err(_) => None,
    // };

    let db = db::init("tedbot_db")?;

    let mut client = Client::builder(token)
        .event_handler(handler::Handler { db })
        .application_id(app_id)
        .intents(crate::INTENTS)
        .await?;
    client.start_autosharded().await?;

    Ok(())
}
