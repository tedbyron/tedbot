use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use chrono::{DateTime, FixedOffset, TimeZone};
use serde::{Deserialize, Deserializer};
use tokio::sync::Mutex;

#[derive(Debug, Deserialize)]
pub struct Guide {
    #[serde(rename = "channel", default)]
    pub channels: Vec<Channel>,
    #[serde(rename = "programme", default)]
    pub programs: Vec<Program>,
    #[serde(skip)]
    schedule: HashMap<String, Vec<usize>>,
}

#[derive(Debug, Deserialize)]
pub struct Channel {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "display-name", default)]
    pub display_names: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct Program {
    #[serde(rename = "@channel")]
    pub channel: String,
    #[serde(rename = "@start", deserialize_with = "xmltv_time")]
    pub start: DateTime<FixedOffset>,
    #[serde(rename = "@stop", deserialize_with = "xmltv_time")]
    pub stop: DateTime<FixedOffset>,
    pub title: Text,
    #[serde(rename = "episode-num", default)]
    pub episode_nums: Vec<EpisodeNum>,
    pub date: Option<String>,
    pub icon: Option<Icon>,
}

#[derive(Debug, Deserialize)]
pub struct Icon {
    #[serde(rename = "@src")]
    pub src: String,
}

#[derive(Debug, Deserialize)]
pub struct Text {
    #[serde(rename = "$text", default)]
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct EpisodeNum {
    #[serde(rename = "@system")]
    pub system: Option<String>,
    #[serde(rename = "$text", default)]
    pub value: String,
}

fn xmltv_time<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<FixedOffset>, D::Error> {
    let s = String::deserialize(d)?;
    DateTime::parse_from_str(&s, "%Y%m%d%H%M%S %z").map_err(serde::de::Error::custom)
}

/// The guide can be tens of MB with many channels, so share one copy for a short while.
#[derive(Debug)]
pub struct GuideCache {
    http: reqwest::Client,
    url: String,
    ttl: Duration,
    cached: Mutex<Option<(Instant, Arc<Guide>)>>,
}

impl GuideCache {
    pub fn new(http: reqwest::Client, url: String, ttl: Duration) -> Self {
        Self {
            http,
            url,
            ttl,
            cached: Mutex::new(None),
        }
    }

    pub async fn get(&self) -> Result<Arc<Guide>> {
        let mut cached = self.cached.lock().await;
        if let Some((fetched, guide)) = &*cached
            && fetched.elapsed() < self.ttl
        {
            return Ok(Arc::clone(guide));
        }
        let guide = Arc::new(Guide::fetch(&self.http, &self.url).await?);
        *cached = Some((Instant::now(), Arc::clone(&guide)));
        drop(cached);
        Ok(guide)
    }
}

impl Guide {
    pub fn parse(xml: &str) -> Result<Self> {
        let mut guide: Self = quick_xml::de::from_str(xml.trim_start_matches('\u{feff}'))?;
        for (i, p) in guide.programs.iter().enumerate() {
            guide.schedule.entry(p.channel.clone()).or_default().push(i);
        }
        let programs = &guide.programs;
        for ids in guide.schedule.values_mut() {
            ids.sort_by_key(|&i| programs[i].start);
        }
        Ok(guide)
    }

    pub async fn fetch(http: &reqwest::Client, base_url: &str) -> Result<Self> {
        let xml = http
            .get(format!("{}/iptv/xmltv.xml", base_url.trim_end_matches('/')))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Self::parse(&xml)
    }

    pub async fn fetch_image(http: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
        Ok(http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?
            .to_vec())
    }

    /// The program airing on `channel` at `at`, and the one after it.
    pub fn on_at<Tz: TimeZone>(
        &self,
        channel: &Channel,
        at: &DateTime<Tz>,
    ) -> (Option<&Program>, Option<&Program>) {
        let Some(ids) = self.schedule.get(&channel.id) else {
            return (None, None);
        };
        let first = ids.partition_point(|&i| self.programs[i].stop <= *at);
        let mut upcoming = ids[first..].iter().map(|&i| &self.programs[i]);
        match upcoming.next() {
            Some(p) if p.start <= *at => (Some(p), upcoming.next()),
            next => (None, next),
        }
    }

    /// Index of the channel matching `query`: its number, then a whole name, then part of one.
    pub fn find(&self, query: &str) -> Option<usize> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return None;
        }
        let channels = &self.channels;
        channels
            .iter()
            .position(|c| c.number() == Some(query.as_str()))
            .or_else(|| {
                channels
                    .iter()
                    .position(|c| c.display_names.iter().any(|n| n.to_lowercase() == query))
            })
            .or_else(|| {
                channels
                    .iter()
                    .position(|c| c.title().to_lowercase().contains(&query))
            })
    }

    /// Up to `limit` channels matching `query`, number matches first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<&Channel> {
        let query = query.trim().to_lowercase();
        let by_number = |c: &Channel| c.number().is_some_and(|n| n.starts_with(&query));
        let mut matches: Vec<&Channel> = self
            .channels
            .iter()
            .filter(|c| by_number(c) || c.title().to_lowercase().contains(&query))
            .collect();
        matches.sort_by_key(|c| !by_number(c));
        matches.truncate(limit);
        matches
    }
}

impl Channel {
    pub fn name(&self) -> &str {
        self.display_names.first().map_or(&self.id, String::as_str)
    }

    pub fn number(&self) -> Option<&str> {
        self.display_names.get(1).map(String::as_str)
    }

    /// "1: Movies" from ErsatzTV's display names ("1 Movies", "1", "Movies").
    pub fn title(&self) -> String {
        match self.display_names.as_slice() {
            [_, number, name, ..] => format!("{number}: {name}"),
            _ => self.name().to_owned(),
        }
    }
}

impl Program {
    pub fn episode(&self) -> Option<String> {
        let ep = self
            .episode_nums
            .iter()
            .find(|e| e.system.as_deref() == Some("onscreen"))?;
        Some(match ep.value.split_once('E') {
            Some((season, episode)) => format!("{season} E{episode}"),
            None => ep.value.clone(),
        })
    }

    pub fn jellyfin_item_id(&self) -> Option<&str> {
        let (_, rest) = self.icon.as_ref()?.src.split_once("/jellyfin/Items/")?;
        rest.split('/').next().filter(|id| !id.is_empty())
    }

    pub fn poster_url(&self, height: u32) -> Option<String> {
        let mut url = reqwest::Url::parse(&self.icon.as_ref()?.src).ok()?;
        let query: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| k != "fillHeight")
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(query)
            .append_pair("fillHeight", &height.to_string());
        Some(url.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = "\u{feff}<?xml version=\"1.0\" encoding=\"utf-8\"?><tv generator-info-name=\"ersatztv\">\
        <channel id=\"C1\"><display-name>1 Zorp</display-name><display-name>1</display-name><display-name>Zorp</display-name></channel>\
        <channel id=\"C2\"><display-name>2 Blat</display-name></channel>\
        <programme start=\"20261015200302 -0400\" stop=\"20261015230045 -0400\" channel=\"C1\">\
        <title lang=\"en\">Flimbar</title><desc lang=\"en\">Glorp&#39;s</desc><date>1995</date></programme>\
        <programme start=\"20261015230045 -0400\" stop=\"20261016015828 -0400\" channel=\"C1\">\
        <title lang=\"en\">Snorvel</title><date>1995</date></programme>\
        <programme start=\"20261015223509 -0400\" stop=\"20261015231629 -0400\" channel=\"C2\">\
        <title lang=\"en\">Wubbleton</title><icon src=\"http://127.0.0.1:8409/iptv/artwork/posters/jellyfin/Items/abc123/Images/Primary?tag=xyz&amp;fillHeight=440\"/><sub-title lang=\"en\">Quixxle Frond</sub-title>\
        <episode-num system=\"onscreen\">S02E07</episode-num><episode-num system=\"xmltv_ns\">1.6.0/1</episode-num></programme>\
        </tv>";

    #[test]
    fn current_and_next() {
        let guide = Guide::parse(XML).unwrap();
        let at = DateTime::parse_from_rfc3339("2026-10-15T22:50:00-04:00").unwrap();

        assert_eq!(guide.channels.len(), 2);
        let zorp = &guide.channels[0];
        assert_eq!(zorp.title(), "1: Zorp");
        assert_eq!(zorp.number(), Some("1"));
        let (now, next) = guide.on_at(zorp, &at);
        assert_eq!(now.unwrap().title.value, "Flimbar");
        assert_eq!(now.unwrap().date.as_deref(), Some("1995"));
        assert_eq!(next.unwrap().title.value, "Snorvel");

        let blat = &guide.channels[1];
        assert_eq!(blat.title(), "2 Blat");
        let (now, next) = guide.on_at(blat, &at);
        assert_eq!(now.unwrap().episode().as_deref(), Some("S02 E07"));
        assert!(next.is_none());
    }

    #[test]
    fn gap_between_programs() {
        let guide = Guide::parse(XML).unwrap();
        let at = DateTime::parse_from_rfc3339("2026-10-15T19:00:00-04:00").unwrap();
        let (now, next) = guide.on_at(&guide.channels[0], &at);
        assert!(now.is_none());
        assert_eq!(next.unwrap().title.value, "Flimbar");
    }

    #[test]
    fn find_and_search() {
        let guide = Guide::parse(XML).unwrap();
        assert_eq!(guide.find("1"), Some(0));
        assert_eq!(guide.find(" zorp "), Some(0));
        assert_eq!(guide.find("bla"), Some(1));
        assert_eq!(guide.find("nope"), None);
        assert_eq!(guide.find(""), None);

        let titles = |q| {
            guide
                .search(q, 25)
                .iter()
                .map(|c| c.title())
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(""), ["1: Zorp", "2 Blat"]);
        assert_eq!(titles("bl"), ["2 Blat"]);
        assert_eq!(guide.search("", 1).len(), 1);
    }

    #[test]
    fn poster() {
        let guide = Guide::parse(XML).unwrap();
        let p = &guide.programs[2];
        assert_eq!(p.jellyfin_item_id(), Some("abc123"));
        assert_eq!(
            p.poster_url(160).as_deref(),
            Some(
                "http://127.0.0.1:8409/iptv/artwork/posters/jellyfin/Items/abc123/Images/Primary?tag=xyz&fillHeight=160"
            )
        );
    }
}
