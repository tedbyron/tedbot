//! The TOML config file. See `config.example.toml` for a commented example.

#![allow(clippy::module_name_repetitions)]

use poise::serenity_prelude::{RoleId, UserId};
use serde::Deserialize;

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
}

/// Discord connection info
#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    /// Bot token from the Discord developer portal
    pub token: String,
    /// Optional role whose members count as admins
    pub admin_role: Option<RoleId>,
    /// Users who count as admins anywhere, including DMs
    #[serde(default)]
    pub admin_users: Vec<UserId>,
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

/// Jellyfin connection info
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

/// Seerr connection info
#[derive(Debug, Deserialize)]
pub struct SeerrConfig {
    /// Base URL, without `/api/v1`. Defaults to `http://127.0.0.1:5055`.
    #[serde(default = "default_seerr_url")]
    pub url: String,
    /// API key from Settings → General
    pub api_key: String,
    /// Optional address people open in a browser, for links to Seerr pages
    pub public_url: Option<String>,
}

fn default_seerr_url() -> String {
    "http://127.0.0.1:5055".into()
}

/// Radarr connection info
#[derive(Debug, Deserialize)]
pub struct RadarrConfig {
    /// Base URL, without `/api/v3`. Defaults to `http://127.0.0.1:7878`.
    #[serde(default = "default_radarr_url")]
    pub url: String,
    /// API key from Settings → General
    pub api_key: String,
}

fn default_radarr_url() -> String {
    "http://127.0.0.1:7878".into()
}

/// Sonarr connection info
#[derive(Debug, Deserialize)]
pub struct SonarrConfig {
    /// Base URL, without `/api/v3`. Defaults to `http://127.0.0.1:8989`.
    #[serde(default = "default_sonarr_url")]
    pub url: String,
    /// API key from Settings → General
    pub api_key: String,
}

fn default_sonarr_url() -> String {
    "http://127.0.0.1:8989".into()
}
