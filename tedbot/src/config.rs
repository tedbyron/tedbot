#![allow(clippy::module_name_repetitions)]

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub log_level: Option<String>,
    pub discord: DiscordConfig,
    #[serde(default)]
    pub ersatztv: ErsatzTvConfig,
    pub jellyfin: Option<JellyfinConfig>,
}

#[derive(Debug, Deserialize)]
pub struct DiscordConfig {
    pub token: String,
    pub activity: Option<DiscordActivityConfig>,
}

#[derive(Debug, Deserialize)]
pub struct DiscordActivityConfig {
    #[serde(rename = "type")]
    pub r#type: Option<String>,
    pub name: Option<String>,
    pub streaming_url: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct ErsatzTvConfig {
    pub url: String,
}

impl Default for ErsatzTvConfig {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8409".into(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct JellyfinConfig {
    #[serde(default = "default_jellyfin_url")]
    pub url: String,
    pub api_key: String,
}

fn default_jellyfin_url() -> String {
    "http://127.0.0.1:8096".into()
}
