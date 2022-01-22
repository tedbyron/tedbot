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

mod handler;

use std::env;
use std::process;

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

    // NOTE: Intersperse is not yet stable https://github.com/rust-lang/rust/issues/79524
    tracing::trace!(command = %env::args().collect::<Vec<_>>().join(" "));

    // Config from env vars.
    let token = match env::var("TEDBOT_TOKEN") {
        Ok(token) => token,
        Err(_) => return Err(Box::from("Missing TEDBOT_TOKEN env var")),
    };
    let app_id = match env::var("TEDBOT_APPLICATION_ID") {
        Ok(app_id) => match app_id.parse::<u64>() {
            Ok(app_id_int) => app_id_int,
            Err(_) => return Err(Box::from("Invalid TEDBOT_APPLICATION_ID env var")),
        },
        Err(_) => return Err(Box::from("Missing TEDBOT_APPLICATION_ID env var")),
    };
    // let token_components = match client::parse_token(&token) {
    //     Some(components) => components,
    //     None => return Err(Box::from("Invalid TEDBOT_TOKEN env var")),
    // };

    // Guild whitelist.
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

    let mut client = Client::builder(token)
        .event_handler(handler::Handler)
        .application_id(app_id)
        .await?;
    client.start_autosharded().await?;

    Ok(())
}
