//! The TOML config file. See `config.example.toml` for a commented example.

#![allow(clippy::module_name_repetitions)]

use poise::serenity_prelude::{ChannelId, RoleId};
use serde::{Deserialize, Deserializer};

/// Bot config
#[derive(Debug, Deserialize)]
pub struct Config {
    /// Optional log level: `error`, `warn` (default), `info`, `debug` or `trace`
    pub log_level: Option<String>,
    /// Discord connection info
    pub discord: DiscordConfig,
    /// Optional ErsatzTV connection info
    #[serde(default)]
    pub ersatztv: ErsatzTvConfig,
    /// Jellyfin connection info
    pub jellyfin: JellyfinConfig,
    /// Seerr connection info
    pub seerr: SeerrConfig,
    /// Optional Radarr connection info, for taking approved movie requests back out
    pub radarr: Option<RadarrConfig>,
    /// Optional Sonarr connection info, for taking approved show requests back out
    pub sonarr: Option<SonarrConfig>,
    /// Optional Radarr for 4K requests (Seerr's 4K server), for taking them back out
    pub radarr_4k: Option<Radarr4kConfig>,
    /// Optional Sonarr for 4K requests (Seerr's 4K server), for taking them back out
    pub sonarr_4k: Option<Sonarr4kConfig>,
}

/// Discord connection info
#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    /// Bot token from the Discord developer portal
    pub token: String,
    /// Optional role whose members count as admins, pinged for requests waiting for approval.
    /// Without it nobody is an admin.
    pub admin_role: Option<RoleId>,
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

/// ErsatzTV connection info
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ErsatzTvConfig {
    /// Base URL, without the `/iptv/...` path. Defaults to `http://localhost:8409`.
    pub url: String,
}

impl Default for ErsatzTvConfig {
    fn default() -> Self {
        Self {
            url: "http://localhost:8409".into(),
        }
    }
}

/// Jellyfin connection info
#[derive(Debug, Deserialize)]
pub struct JellyfinConfig {
    /// Base URL. Defaults to `http://localhost:8096`.
    #[serde(default = "default_jellyfin_url")]
    pub url: String,
    /// API key made for the bot under Dashboard → API Keys
    pub api_key: String,
    /// Whether Jellyfin has the Moonfin plugin, so "now available" posts link to Moonfin Web too
    #[serde(default)]
    pub moonfin: bool,
}

fn default_jellyfin_url() -> String {
    "http://localhost:8096".into()
}

/// Seerr connection info
#[derive(Debug, Deserialize)]
pub struct SeerrConfig {
    /// Base URL, without `/api/v1`. Defaults to `http://localhost:5055`.
    #[serde(default = "default_seerr_url")]
    pub url: String,
    /// API key from Settings → General
    pub api_key: String,
    /// Optional address people open in a browser, for links to Seerr pages
    pub public_url: Option<String>,
    /// Optional listener for Seerr's webhook
    pub webhook: Option<WebhookConfig>,
}

fn default_seerr_url() -> String {
    "http://localhost:5055".into()
}

/// Radarr connection info
#[derive(Debug, Deserialize)]
pub struct RadarrConfig {
    /// Base URL, without `/api/v3`. Defaults to `http://localhost:7878`.
    #[serde(default = "default_radarr_url")]
    pub url: String,
    /// API key from Settings → General
    pub api_key: String,
}

fn default_radarr_url() -> String {
    "http://localhost:7878".into()
}

/// Sonarr connection info
#[derive(Debug, Deserialize)]
pub struct SonarrConfig {
    /// Base URL, without `/api/v3`. Defaults to `http://localhost:8989`.
    #[serde(default = "default_sonarr_url")]
    pub url: String,
    /// API key from Settings → General
    pub api_key: String,
}

fn default_sonarr_url() -> String {
    "http://localhost:8989".into()
}

/// 4K Radarr connection info
#[derive(Debug, Deserialize)]
pub struct Radarr4kConfig {
    /// Base URL, without `/api/v3`. Defaults to `http://localhost:7879`.
    #[serde(default = "default_radarr_4k_url")]
    pub url: String,
    /// API key from Settings → General
    pub api_key: String,
}

fn default_radarr_4k_url() -> String {
    "http://localhost:7879".into()
}

/// 4K Sonarr connection info
#[derive(Debug, Deserialize)]
pub struct Sonarr4kConfig {
    /// Base URL, without `/api/v3`. Defaults to `http://localhost:8990`.
    #[serde(default = "default_sonarr_4k_url")]
    pub url: String,
    /// API key from Settings → General
    pub api_key: String,
}

fn default_sonarr_4k_url() -> String {
    "http://localhost:8990".into()
}

/// Listener for Seerr's webhook, on localhost
#[derive(Debug, Deserialize)]
pub struct WebhookConfig {
    pub port: u16,
    /// URLs to pass every notification on to, since Seerr only has one webhook. One URL or a list.
    #[serde(default, deserialize_with = "one_or_many")]
    pub forward: Vec<String>,
    /// Channels to post in when a request is made
    #[serde(default)]
    pub requests_channels: Vec<ChannelId>,
    /// Channels to post in when a request is available in Jellyfin
    #[serde(default)]
    pub available_channels: Vec<ChannelId>,
}

/// A string or a list of them
fn one_or_many<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    Ok(match OneOrMany::deserialize(deserializer)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_one_or_many() {
        let forward = |toml: &str| toml::from_str::<WebhookConfig>(toml).unwrap().forward;
        assert!(forward("port = 1").is_empty());
        assert_eq!(forward("port = 1\nforward = 'a'"), ["a"]);
        assert_eq!(forward("port = 1\nforward = ['a', 'b']"), ["a", "b"]);
    }
}
