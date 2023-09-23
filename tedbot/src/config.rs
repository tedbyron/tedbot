#![allow(clippy::module_name_repetitions)]

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub log_level: Option<String>,
    pub discord: DiscordConfig,
    pub openai: OpenAiConfig,
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
pub struct OpenAiConfig {
    pub api_key: String,
}
