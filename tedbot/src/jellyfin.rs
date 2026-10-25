//! Jellyfin API client. Only the parts the bot uses.

use anyhow::Result;
use serde::Deserialize;

/// Where to reach Jellyfin and how to authenticate. The HTTP client is passed in per call so the
/// bot shares one connection pool.
#[derive(Debug)]
pub struct Jellyfin {
    pub url: String,
    pub api_key: String,
}

/// Response of `GET /Items`. Only the fields we read are deserialized.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Items {
    /// The matching items; empty if none match
    items: Vec<Item>,
}

/// A movie, episode, etc.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    /// Cast and crew. Only included when the request asks for `fields=People`.
    #[serde(default)]
    people: Vec<Person>,
}

/// One cast or crew credit
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Person {
    /// Full name, e.g. "Guillermo del Toro"
    name: String,
    /// `Director`, `Actor`, `Writer`, ...
    r#type: Option<String>,
}

impl Jellyfin {
    /// Directors credited on the item, in Jellyfin's order. Empty if there are none or the item
    /// doesn't exist.
    pub async fn directors(&self, http: &reqwest::Client, item_id: &str) -> Result<Vec<String>> {
        let items: Items = http
            .get(format!("{}/Items", self.url.trim_end_matches('/')))
            .query(&[("ids", item_id), ("fields", "People")])
            .header(
                "Authorization",
                format!("MediaBrowser Token=\"{}\"", self.api_key),
            )
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(items
            .items
            .into_iter()
            .flat_map(|item| item.people)
            .filter(|p| p.r#type.as_deref() == Some("Director"))
            .map(|p| p.name)
            .collect())
    }
}
