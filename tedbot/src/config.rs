//! The TOML config file. See `config.example.toml` for a commented example.

#![allow(clippy::module_name_repetitions)]

use poise::serenity_prelude::UserId;
use serde::{Deserialize, Deserializer, de::Error as _};
use tracing_subscriber::filter::LevelFilter;

/// Bot config
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Log level: `off`, `error`, `warn` (default), `info`, `debug` or `trace`. `RUST_LOG`
    /// overrides it.
    #[serde(default = "default_log_level", deserialize_with = "level")]
    pub log_level: LevelFilter,
    /// Discord connection info
    pub discord: DiscordConfig,
    /// The media server's services
    #[serde(flatten)]
    pub server: poise_jellyfin::Config,
}

/// Discord connection info
#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    /// Bot token from the Discord developer portal
    pub token: String,
    /// User ids of the media server's admins, in every server, pinged for requests waiting for
    /// approval in servers they're in. Without any nobody is an admin.
    #[serde(default)]
    pub admins: Vec<UserId>,
    /// Optional Discord activity
    pub activity: Option<DiscordActivityConfig>,
}

/// Bot status line, e.g. "Listening to /commands". Nothing is shown unless both `type` and `name`
/// are set.
#[derive(Debug, Deserialize)]
pub struct DiscordActivityConfig {
    /// `competing`, `listening`, `playing`, `streaming` or `watching`
    #[serde(rename = "type")]
    pub r#type: Option<String>,
    /// The text after the verb, e.g. `/commands` in "Listening to /commands"
    pub name: Option<String>,
    /// Twitch or YouTube URL, required when `type` is `streaming`
    pub streaming_url: Option<String>,
}

const fn default_log_level() -> LevelFilter {
    LevelFilter::WARN
}

/// A level from its name, failing on any other
fn level<'de, D: Deserializer<'de>>(d: D) -> Result<LevelFilter, D::Error> {
    String::deserialize(d)?.parse().map_err(D::Error::custom)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses() {
        let cfg: Config = toml::from_str(include_str!("../../config.example.toml")).unwrap();
        assert_eq!(cfg.server.jellyfin.url, "http://localhost:8096");
        assert!(cfg.server.radarr.is_none());
        assert_eq!(cfg.log_level, LevelFilter::WARN);
    }

    #[test]
    fn bad_log_level() {
        let example = include_str!("../../config.example.toml");
        let cfg = format!("log_level = 'loud'\n{example}");
        assert!(toml::from_str::<Config>(&cfg).is_err());
    }
}
