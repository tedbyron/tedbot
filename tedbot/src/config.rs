//! The TOML config file. See `config.example.toml` for a commented example.

#![allow(clippy::module_name_repetitions)]

use poise::serenity_prelude::UserId;
use serde::Deserialize;

/// Bot config
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Optional log level: `error`, `warn` (default), `info`, `debug` or `trace`
    pub log_level: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses() {
        let cfg: Config = toml::from_str(include_str!("../../config.example.toml")).unwrap();
        assert_eq!(cfg.server.jellyfin.url, "http://localhost:8096");
        assert!(cfg.server.radarr.is_none());
    }
}
