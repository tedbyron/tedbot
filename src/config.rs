//! Environment variable configuration.

use std::env;

use serenity::client::{self, TokenComponents};
use serenity::model::id::GuildId;
use serenity::model::prelude::Activity;

/// A parsed app configuration from environment variables.
#[derive(Debug)]
pub struct Config {
    pub token: String,
    pub token_components: TokenComponents,
    pub whitelist: Option<Vec<GuildId>>,
    pub prefix: String,
    pub activity: Option<Activity>,
}

/// Load and parse environment variables into a [`Config`].
#[tracing::instrument]
pub fn load() -> crate::Result<Config> {
    // Discord bot token.
    let token = match env::var("TEDBOT_TOKEN") {
        Ok(token) => token,
        Err(_) => return Err(Box::from("Missing TEDBOT_TOKEN env var")),
    };
    let token_components = match client::parse_token(&token) {
        Some(components) => components,
        None => return Err(Box::from("Invalid TEDBOT_TOKEN env var")),
    };

    // Guild whitelist.
    let whitelist = match env::var("TEDBOT_WHITELIST") {
        Ok(wl_string) => {
            if wl_string.is_empty() {
                None
            } else {
                let wl = wl_string
                    .trim()
                    .split(|c: char| !c.is_ascii_digit())
                    .map(str::parse::<u64>)
                    .filter_map(Result::ok)
                    .map(GuildId::from)
                    .collect::<Vec<_>>();

                if wl.is_empty() {
                    return Err(Box::from("Invalid TEDBOT_WHITELIST env var"));
                }
                Some(wl)
            }
        }
        Err(_) => None,
    };

    // Command prefix.
    let prefix = match env::var("TEDBOT_PREFIX") {
        Ok(prefix) => {
            if prefix.is_empty() {
                return Err(Box::from("Empty TEDBOT_PREFIX env var"));
            }
            prefix
        }
        Err(_) => return Err(Box::from("Missing TEDBOT_PREFIX env var")),
    };

    // Bot user activity.
    let activity = match (
        env::var("TEDBOT_ACTIVITY_TYPE"),
        env::var("TEDBOT_ACTIVITY_NAME"),
    ) {
        (Ok(type_), Ok(name)) => {
            let activity_type = match type_.as_str() {
                type_ @ ("competing" | "listening" | "playing" | "streaming" | "watching") => type_,
                _ => return Err(Box::from("Invalid TEDBOT_ACTIVITY_TYPE env var")),
            };

            Some(match activity_type {
                "competing" => Activity::competing(name),
                "listening" => Activity::listening(name),
                "playing" => Activity::playing(name),
                "streaming" => match env::var("TEDBOT_ACTIVITY_STREAMING") {
                    Ok(streaming) => Activity::streaming(name, streaming),
                    _ => return Err(Box::from("Missing TEDBOT_ACTIVITY_STREAMING env var")),
                },
                "watching" => Activity::watching(name),
                _ => unreachable!(),
            })
        }
        _ => None,
    };

    let cfg = Config {
        token,
        token_components,
        whitelist,
        prefix,
        activity,
    };

    tracing::debug!(?cfg);
    tracing::info!("Config loaded from env");

    Ok(cfg)
}
