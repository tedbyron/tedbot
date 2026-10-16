use anyhow::Result;
use serde::Deserialize;

#[derive(Debug)]
pub struct Jellyfin {
    pub url: String,
    pub api_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Items {
    items: Vec<Item>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    #[serde(default)]
    people: Vec<Person>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Person {
    name: String,
    r#type: Option<String>,
}

impl Jellyfin {
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
