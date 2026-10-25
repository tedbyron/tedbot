//! The TOML config file. See `config.example.toml` for a commented example.

#![allow(clippy::module_name_repetitions)]

use serde::Deserialize;

/// Bot config
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Optional log level: `error`, `warn`, `info` (default), `debug` or `trace`
    pub log_level: Option<String>,
    /// Discord connection settings
    pub discord: DiscordConfig,
    /// Optional ErsatzTV connection settings
    #[serde(default)]
    pub ersatztv: ErsatzTvConfig,
    /// Jellyfin connection settings
    pub jellyfin: JellyfinConfig,
}

/// Discord connection settings
#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    /// Bot token from the Discord developer portal
    pub token: String,
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

/// ErsatzTV connection settings
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ErsatzTvConfig {
    /// Base URL, without the `/iptv/...` path. Defaults to `http://127.0.0.1:8409`.
    pub url: String,
}

impl Default for ErsatzTvConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8409".into(),
        }
    }
}

/// Jellyfin connection settings
#[derive(Debug, Deserialize)]
pub struct JellyfinConfig {
    /// Base URL. Defaults to `http://127.0.0.1:8096`.
    #[serde(default = "default_jellyfin_url")]
    pub url: String,
    /// API key made for the bot under Dashboard → API Keys
    pub api_key: String,
}

fn default_jellyfin_url() -> String {
    "http://127.0.0.1:8096".into()
}
