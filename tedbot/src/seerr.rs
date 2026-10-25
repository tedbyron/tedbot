//! Seerr API client. Only the parts the bot uses.

use std::collections::HashMap;

use anyhow::Result;
use poise::serenity_prelude::UserId;
use serde::Deserialize;

/// Where to reach Seerr and how to authenticate. The HTTP client is passed in per call so the bot
/// shares one connection pool.
#[derive(Debug)]
pub struct Seerr {
    pub url: String,
    pub api_key: String,
}

/// Response of `GET /api/v1/user`
#[derive(Debug, Deserialize)]
struct Users {
    /// One page of users
    results: Vec<User>,
}

/// A Seerr user, as listed
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct User {
    /// Seerr's user id
    id: u32,
    /// The linked Jellyfin account, absent for local Seerr users
    jellyfin_user_id: Option<String>,
}

/// Response of `GET /api/v1/user/{id}`, which unlike the list includes settings
#[derive(Debug, Deserialize)]
struct UserDetail {
    /// Absent until the user saves any settings
    settings: Option<UserSettings>,
}

/// A user's preferences
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserSettings {
    /// Discord user ids from the user's notification settings
    discord_ids: Option<Vec<String>>,
}

impl Seerr {
    /// Starts an authenticated `GET` of `path` (with a leading `/`)
    fn get(&self, http: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        http.get(format!("{}/api/v1{path}", self.url.trim_end_matches('/')))
            .header("X-Api-Key", &self.api_key)
    }

    /// The Discord account each of `jellyfin_ids` linked in Seerr, keyed by Jellyfin user id.
    /// Users without a Seerr account or Discord id are left out.
    pub async fn discord_ids(
        &self,
        http: &reqwest::Client,
        jellyfin_ids: &[&str],
    ) -> Result<HashMap<String, UserId>> {
        let users: Users = self
            .get(http, "/user")
            .query(&[("take", "1000")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        let mut linked = HashMap::new();
        for user in users.results {
            let Some(jellyfin_id) = user.jellyfin_user_id.map(|id| normalize(&id)) else {
                continue;
            };
            if !jellyfin_ids.iter().any(|id| normalize(id) == jellyfin_id) {
                continue;
            }
            let detail: UserDetail = self
                .get(http, &format!("/user/{}", user.id))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let discord_id = detail
                .settings
                .and_then(|s| s.discord_ids)
                .into_iter()
                .flatten()
                .find_map(|id| id.parse().ok().filter(|&id| id != 0));
            if let Some(id) = discord_id {
                linked.insert(jellyfin_id, UserId::new(id));
            }
        }
        Ok(linked)
    }
}

/// Jellyfin user ids without dashes and in lowercase, so either spelling of a GUID matches
pub fn normalize(id: &str) -> String {
    id.chars()
        .filter(|&c| c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}
