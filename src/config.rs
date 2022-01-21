//! Environment variable configuration.

use std::env;

use serenity::client::{self, TokenComponents};
use serenity::model::id::GuildId;
use serenity::model::prelude::Activity;

pub struct Config {
    pub log: String,
    pub token: String,
    pub token_components: TokenComponents,
    pub whitelist: Option<Vec<GuildId>>,
    pub prefix: String,
    pub activity: Option<Activity>,
}

pub fn load() -> Result<Config, crate::Error> {
    let log = if let Ok(level) = env::var("TEDBOT_LOG") {
        level
    } else {
        env::set_var("TEDBOT_LOG", "INFO");
        String::from("INFO")
    };

    let token = env::var("TEDBOT_TOKEN")?;
    let token_components = match client::parse_token(&token) {
        Some(components) => components,
        None => return Err(Box::from("Invalid TEDBOT_TOKEN env var")),
    };

    let whitelist_res = env::var("TEDBOT_WHITELIST");
    let whitelist = if let Ok(wl) = whitelist_res {
        if wl.is_empty() {
            None
        } else {
            let whitelist_int = wl
                .trim()
                .split(|c: char| !c.is_ascii_digit())
                .map(str::parse::<u64>)
                .collect::<Vec<_>>();

            if whitelist_int.iter().all(Result::is_ok) {
                Some(
                    whitelist_int
                        .into_iter()
                        .filter_map(Result::ok)
                        .map(GuildId::from)
                        .collect(),
                )
            } else {
                return Err(Box::from("Invalid TEDBOT_WHITELIST env var"));
            }
        }
    } else {
        None
    };

    let prefix = env::var("TEDBOT_PREFIX")?;

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
                "streaming" => {
                    if let Ok(streaming) = env::var("TEDBOT_ACTIVITY_STREAMING") {
                        Activity::streaming(name, streaming)
                    } else {
                        return Err(Box::from("Missing TEDBOT_ACTIVITY_STREAMING env var"));
                    }
                }
                "watching" => Activity::watching(name),
                _ => unreachable!(),
            })
        }
        _ => None,
    };

    Ok(Config {
        log,
        token,
        token_components,
        whitelist,
        prefix,
        activity,
    })
}
