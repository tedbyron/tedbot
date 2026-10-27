//! Radarr and Sonarr API clients. Only the parts the bot uses: how far a request's download has
//! got, and taking a cancelled request back out while it's still downloading.

use std::collections::HashMap;

use anyhow::Result;
use reqwest::Method;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

/// Where to reach Radarr or Sonarr and how to authenticate. The HTTP client is passed in per call
/// so the bot shares one connection pool.
#[derive(Debug)]
pub struct Arr {
    pub url: String,
    pub api_key: String,
}

/// A Radarr movie
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Movie {
    id: u32,
    /// Whether it's been downloaded
    has_file: bool,
}

/// What came of taking a request back out
#[derive(Debug, PartialEq, Eq)]
pub enum Removed {
    /// Removed, or never there
    Done,
    /// Some of it has been downloaded already, so nothing was touched
    Downloaded,
}

/// One download in the queue. A season pack is listed once per episode, all with the same
/// download.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct QueueItem {
    id: u32,
    /// Radarr only
    movie_id: Option<u32>,
    /// Sonarr only
    series_id: Option<u32>,
    /// Sonarr only
    season_number: Option<u32>,
    /// Bytes in all
    #[serde(default)]
    size: f64,
    /// Bytes still to download
    #[serde(default, rename = "sizeleft")]
    size_left: f64,
}

/// How far a download in Radarr's or Sonarr's queue has got
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Download {
    /// A release was grabbed, but none of it has downloaded: it's waiting for the download
    /// client or for peers
    Queued,
    /// Some of it has downloaded
    Started,
}

impl QueueItem {
    fn download(&self) -> Download {
        if self.size > 0.0 && self.size_left < self.size {
            Download::Started
        } else {
            Download::Queued
        }
    }
}

/// Body of `DELETE /api/v3/queue/bulk`
#[derive(Debug, Serialize)]
struct QueueIds {
    ids: Vec<u32>,
}

/// A Sonarr episode file
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EpisodeFile {
    season_number: u32,
}

impl Arr {
    /// Starts an authenticated request to `path` under `/api/v3` (with a leading `/`)
    fn request(
        &self,
        http: &reqwest::Client,
        method: Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        http.request(
            method,
            format!("{}/api/v3{path}", self.url.trim_end_matches('/')),
        )
        .header("X-Api-Key", &self.api_key)
    }

    /// `GET`s `path` as JSON
    async fn get<T: DeserializeOwned>(&self, http: &reqwest::Client, path: &str) -> Result<T> {
        Ok(self
            .request(http, Method::GET, path)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// `DELETE`s `path`
    async fn delete(&self, http: &reqwest::Client, path: &str) -> Result<()> {
        self.request(http, Method::DELETE, path)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    /// Removes queue items `ids` and their torrents, without blocklisting the releases
    async fn remove_queued(&self, http: &reqwest::Client, ids: Vec<u32>) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        self.request(
            http,
            Method::DELETE,
            "/queue/bulk?removeFromClient=true&blocklist=false",
        )
        .json(&QueueIds { ids })
        .send()
        .await?
        .error_for_status()?;
        Ok(())
    }

    /// Radarr: how far the furthest download of the movie with TMDB id `tmdb_id` has got.
    /// `None` if nothing's in the queue for it.
    pub async fn movie_download(
        &self,
        http: &reqwest::Client,
        tmdb_id: u32,
    ) -> Result<Option<Download>> {
        let movies: Vec<Movie> = self.get(http, &format!("/movie?tmdbId={tmdb_id}")).await?;
        let mut furthest = None;
        for movie in movies {
            let queue: Vec<QueueItem> = self
                .get(http, &format!("/queue/details?movieId={}", movie.id))
                .await?;
            let download = queue
                .iter()
                .filter(|q| q.movie_id == Some(movie.id))
                .map(QueueItem::download)
                .max();
            furthest = furthest.max(download);
        }
        Ok(furthest)
    }

    /// Sonarr: how far the furthest download of each season of the show with TVDB id `tvdb_id`
    /// has got, for seasons with anything in the queue
    pub async fn season_downloads(
        &self,
        http: &reqwest::Client,
        tvdb_id: u32,
    ) -> Result<HashMap<u32, Download>> {
        let shows: Vec<Value> = self.get(http, &format!("/series?tvdbId={tvdb_id}")).await?;
        let Some(id) = shows.first().and_then(|s| s["id"].as_u64()) else {
            return Ok(HashMap::new());
        };
        let queue: Vec<QueueItem> = self
            .get(http, &format!("/queue/details?seriesId={id}"))
            .await?;
        let mut seasons = HashMap::new();
        for q in queue
            .iter()
            .filter(|q| q.series_id.map(u64::from) == Some(id))
        {
            if let Some(n) = q.season_number {
                let furthest = seasons.entry(n).or_insert(Download::Queued);
                *furthest = (*furthest).max(q.download());
            }
        }
        Ok(seasons)
    }

    /// Sonarr: the seasons of the show with TVDB id `tvdb_id` with a file for every monitored
    /// episode that's aired
    pub async fn complete_seasons(&self, http: &reqwest::Client, tvdb_id: u32) -> Result<Vec<u32>> {
        let shows: Vec<Value> = self.get(http, &format!("/series?tvdbId={tvdb_id}")).await?;
        Ok(shows.first().map(complete).unwrap_or_default())
    }

    /// Radarr: removes the movie with TMDB id `tmdb_id` and its downloads, unless it's been
    /// downloaded already
    pub async fn remove_movie(&self, http: &reqwest::Client, tmdb_id: u32) -> Result<Removed> {
        let movies: Vec<Movie> = self.get(http, &format!("/movie?tmdbId={tmdb_id}")).await?;
        if movies.iter().any(|m| m.has_file) {
            return Ok(Removed::Downloaded);
        }
        for movie in movies {
            let queue: Vec<QueueItem> = self
                .get(http, &format!("/queue/details?movieId={}", movie.id))
                .await?;
            let ids = queue
                .into_iter()
                .filter(|q| q.movie_id == Some(movie.id))
                .map(|q| q.id)
                .collect();
            self.remove_queued(http, ids).await?;
            self.delete(
                http,
                &format!(
                    "/movie/{}?deleteFiles=false&addImportExclusion=false",
                    movie.id
                ),
            )
            .await?;
        }
        Ok(Removed::Done)
    }

    /// Sonarr: stops `seasons` of the show with TVDB id `tvdb_id` downloading, unless any of
    /// their episodes have been downloaded already. Other seasons are left alone. If that leaves
    /// nothing monitored and no files, the show is removed altogether.
    pub async fn remove_seasons(
        &self,
        http: &reqwest::Client,
        tvdb_id: u32,
        seasons: &[u32],
    ) -> Result<Removed> {
        let shows: Vec<Value> = self.get(http, &format!("/series?tvdbId={tvdb_id}")).await?;
        let Some(mut show) = shows.into_iter().next() else {
            return Ok(Removed::Done);
        };
        let Some(id) = show["id"].as_u64() else {
            anyhow::bail!("Sonarr series without an id");
        };

        let in_seasons = |n: Option<u32>| n.is_some_and(|n| seasons.contains(&n));
        let files: Vec<EpisodeFile> = self
            .get(http, &format!("/episodefile?seriesId={id}"))
            .await?;
        if files.iter().any(|f| in_seasons(Some(f.season_number))) {
            return Ok(Removed::Downloaded);
        }

        // Sent back whole, so fields this doesn't know about survive. Unmonitoring a season
        // unmonitors its episodes too.
        for season in show["seasons"].as_array_mut().into_iter().flatten() {
            let number = season["seasonNumber"].as_u64();
            if number.is_some_and(|n| seasons.iter().any(|&s| u64::from(s) == n)) {
                season["monitored"] = false.into();
            }
        }
        self.request(http, Method::PUT, &format!("/series/{id}"))
            .json(&show)
            .send()
            .await?
            .error_for_status()?;

        let queue: Vec<QueueItem> = self
            .get(http, &format!("/queue/details?seriesId={id}"))
            .await?;
        let ids = queue
            .into_iter()
            .filter(|q| q.series_id.map(u64::from) == Some(id) && in_seasons(q.season_number))
            .map(|q| q.id)
            .collect();
        self.remove_queued(http, ids).await?;

        let show: Value = self.get(http, &format!("/series/{id}")).await?;
        if !in_use(&show) {
            self.delete(
                http,
                &format!("/series/{id}?deleteFiles=false&addImportListExclusion=false"),
            )
            .await?;
        }
        Ok(Removed::Done)
    }
}

/// The seasons of a Sonarr show with a file for every monitored episode that's aired
fn complete(show: &Value) -> Vec<u32> {
    show["seasons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| {
            let count = |field: &str| s["statistics"][field].as_u64().unwrap_or_default();
            s["monitored"].as_bool() == Some(true)
                && count("episodeCount") > 0
                && count("episodeFileCount") >= count("episodeCount")
        })
        .filter_map(|s| u32::try_from(s["seasonNumber"].as_u64()?).ok())
        .collect()
}

/// Whether a Sonarr show has any season monitored or any episode file left
fn in_use(show: &Value) -> bool {
    show["seasons"].as_array().into_iter().flatten().any(|s| {
        s["monitored"].as_bool() == Some(true)
            || s["statistics"]["episodeFileCount"]
                .as_u64()
                .is_some_and(|n| n > 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_download() {
        let item = |json: &str| serde_json::from_str::<QueueItem>(json).unwrap().download();
        assert_eq!(
            item(r#"{"id": 1, "size": 100, "sizeleft": 40}"#),
            Download::Started
        );
        assert_eq!(
            item(r#"{"id": 1, "size": 100, "sizeleft": 0}"#),
            Download::Started
        );
        assert_eq!(
            item(r#"{"id": 1, "size": 100, "sizeleft": 100}"#),
            Download::Queued
        );
        assert_eq!(item(r#"{"id": 1}"#), Download::Queued);
    }

    #[test]
    fn complete_seasons() {
        let show: Value = serde_json::from_str(
            r#"{"seasons": [
                {"seasonNumber": 0, "monitored": false,
                 "statistics": {"episodeCount": 0, "episodeFileCount": 0}},
                {"seasonNumber": 1, "monitored": true,
                 "statistics": {"episodeCount": 9, "episodeFileCount": 9}},
                {"seasonNumber": 2, "monitored": true,
                 "statistics": {"episodeCount": 12, "episodeFileCount": 11}},
                {"seasonNumber": 3, "monitored": false,
                 "statistics": {"episodeCount": 15, "episodeFileCount": 15}},
                {"seasonNumber": 4, "monitored": true,
                 "statistics": {"episodeCount": 0, "episodeFileCount": 0}}
            ]}"#,
        )
        .unwrap();
        assert_eq!(complete(&show), [1]);
    }

    #[test]
    fn show_in_use() {
        let show = |json: &str| -> Value { serde_json::from_str(json).unwrap() };
        assert!(in_use(&show(
            r#"{"seasons": [
                {"seasonNumber": 0, "monitored": false, "statistics": {"episodeFileCount": 0}},
                {"seasonNumber": 1, "monitored": true, "statistics": {"episodeFileCount": 0}}
            ]}"#
        )));
        assert!(in_use(&show(
            r#"{"seasons": [
                {"seasonNumber": 1, "monitored": false, "statistics": {"episodeFileCount": 3}}
            ]}"#
        )));
        assert!(!in_use(&show(
            r#"{"seasons": [
                {"seasonNumber": 0, "monitored": false, "statistics": {"episodeFileCount": 0}},
                {"seasonNumber": 1, "monitored": false}
            ]}"#
        )));
    }
}
