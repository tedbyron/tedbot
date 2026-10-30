//! Jellyfin API client. Only the parts the bot uses.

use std::{net::IpAddr, time::Duration};

use anyhow::Result;
use serde::Deserialize;

/// Jellyfin's time unit: 100 ns
const TICKS_PER_SECOND: u64 = 10_000_000;

/// Where to reach Jellyfin and how to authenticate. The HTTP client is passed in per call so the
/// bot shares one connection pool.
#[derive(Debug)]
pub struct Jellyfin {
    pub url: String,
    pub api_key: String,
    /// Whether it has the Moonfin plugin
    pub moonfin: bool,
    /// Its launchd agent's label, if `/restart` may restart it
    pub launchd_label: Option<String>,
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

/// A connected client, from `GET /Sessions`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Session {
    /// Signed-in user's id, if any
    pub user_id: Option<String>,
    /// Signed-in user's name, if any
    pub user_name: Option<String>,
    /// App name, e.g. "Jellyfin Web"
    pub client: Option<String>,
    /// Device name, e.g. "Firefox"
    pub device_name: Option<String>,
    /// Client IP. Caddy is a known proxy, so this is the real client, not localhost.
    pub remote_end_point: Option<String>,
    /// What's playing, if anything
    pub now_playing_item: Option<NowPlaying>,
    /// Position, pause state and play method
    #[serde(default)]
    pub play_state: PlayState,
    /// Only while transcoding, and missing while the transcode is throttled
    pub transcoding_info: Option<TranscodingInfo>,
}

/// The item a session is playing
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NowPlaying {
    /// Movie or episode title
    pub name: String,
    /// `Movie`, `Episode`, `Audio`, `TvChannel`, ...
    pub r#type: Option<String>,
    /// Show title, for episodes
    pub series_name: Option<String>,
    /// Season number, for episodes
    pub parent_index_number: Option<u32>,
    /// Episode number, for episodes
    pub index_number: Option<u32>,
    /// Release year
    pub production_year: Option<i32>,
    /// Length in ticks (100 ns)
    pub run_time_ticks: Option<u64>,
    /// Video, audio and subtitle tracks of the source file
    #[serde(default)]
    pub media_streams: Vec<MediaStream>,
}

/// One track of the source file
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaStream {
    /// `Video`, `Audio`, `Subtitle`, ...
    pub r#type: Option<String>,
    /// Track index that [`PlayState::audio_stream_index`] refers to
    pub index: Option<i32>,
    /// Bits per second
    pub bit_rate: Option<u64>,
    /// Whether it's a separate file, like an `.srt` next to the video
    #[serde(default)]
    pub is_external: bool,
}

/// Where playback is and how it's delivered
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PlayState {
    /// Position in ticks (100 ns)
    pub position_ticks: Option<u64>,
    /// Whether playback is paused
    #[serde(default)]
    pub is_paused: bool,
    /// `DirectPlay`, `DirectStream` or `Transcode`
    pub play_method: Option<String>,
    /// [`MediaStream::index`] of the audio track playing
    pub audio_stream_index: Option<i32>,
}

/// What a transcode is doing
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TranscodingInfo {
    /// Output bits per second
    pub bitrate: Option<u64>,
    /// Whether video is copied rather than re-encoded
    #[serde(default)]
    pub is_video_direct: bool,
    /// Whether audio is copied rather than re-encoded
    #[serde(default)]
    pub is_audio_direct: bool,
}

impl Jellyfin {
    /// Starts an authenticated `GET` of `path` (with a leading `/`)
    fn get(&self, http: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        http.get(format!("{}{path}", self.url.trim_end_matches('/')))
            .header(
                "Authorization",
                format!("MediaBrowser Token=\"{}\"", self.api_key),
            )
    }

    /// Directors credited on the item, in Jellyfin's order. Empty if there are none or the item
    /// doesn't exist.
    pub async fn directors(&self, http: &reqwest::Client, item_id: &str) -> Result<Vec<String>> {
        let items: Items = self
            .get(http, "/Items")
            .query(&[("ids", item_id), ("fields", "People")])
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

    /// Whether Jellyfin's health check passes within 5 seconds
    pub async fn healthy(&self, http: &reqwest::Client) -> bool {
        http.get(format!("{}/health", self.url.trim_end_matches('/')))
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }

    /// Sessions that are playing something, in Jellyfin's order
    pub async fn playing(&self, http: &reqwest::Client) -> Result<Vec<Session>> {
        let sessions: Vec<Session> = self
            .get(http, "/Sessions")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(sessions
            .into_iter()
            .filter(|s| s.now_playing_item.is_some())
            .collect())
    }
}

impl Session {
    /// Bits per second sent to the client: the transcode's output, or else the source's video
    /// plus the audio track playing. `None` if Jellyfin reports no bitrates.
    pub fn bitrate(&self) -> Option<u64> {
        if let Some(bitrate) = self.transcoding_info.as_ref().and_then(|t| t.bitrate) {
            return Some(bitrate);
        }
        let streams = &self.now_playing_item.as_ref()?.media_streams;
        let is = |s: &MediaStream, kind| !s.is_external && s.r#type.as_deref() == Some(kind);
        let video: u64 = streams
            .iter()
            .filter(|s| is(s, "Video"))
            .filter_map(|s| s.bit_rate)
            .sum();
        let audio = streams
            .iter()
            .filter(|s| is(s, "Audio"))
            .find(|s| {
                self.play_state
                    .audio_stream_index
                    .is_none_or(|i| s.index == Some(i))
            })
            .and_then(|s| s.bit_rate)
            .unwrap_or_default();
        Some(video + audio).filter(|&b| b > 0)
    }

    /// Whether the client is on the LAN (or this machine), so it doesn't use upload bandwidth
    pub fn is_local(&self) -> bool {
        self.remote_end_point
            .as_deref()
            .and_then(|ip| ip.parse::<IpAddr>().ok())
            .is_some_and(|ip| match ip.to_canonical() {
                IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
                IpAddr::V6(ip) => {
                    ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
                }
            })
    }
}

/// Converts Jellyfin ticks to whole seconds
pub const fn seconds(ticks: u64) -> u64 {
    ticks / TICKS_PER_SECOND
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(json: &str) -> Session {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn local() {
        let at = |ip| session(&format!(r#"{{"RemoteEndPoint":"{ip}"}}"#)).is_local();
        assert!(at("192.168.2.116"));
        assert!(at("127.0.0.1"));
        assert!(at("::ffff:10.0.0.4"));
        assert!(at("fd00::1"));
        assert!(!at("174.104.135.82"));
        assert!(!at("2600::1"));
    }

    #[test]
    fn bitrate() {
        let direct = session(
            r#"{
                "NowPlayingItem": {"Name": "Flim", "MediaStreams": [
                    {"Type": "Video", "Index": 0, "BitRate": 8000000},
                    {"Type": "Audio", "Index": 1, "BitRate": 640000},
                    {"Type": "Audio", "Index": 2, "BitRate": 128000},
                    {"Type": "Subtitle", "Index": 3, "BitRate": 100, "IsExternal": true}
                ]},
                "PlayState": {"AudioStreamIndex": 2}
            }"#,
        );
        assert_eq!(direct.bitrate(), Some(8_128_000));

        let transcode = session(
            r#"{"NowPlayingItem": {"Name": "Flim"}, "TranscodingInfo": {"Bitrate": 4000000}}"#,
        );
        assert_eq!(transcode.bitrate(), Some(4_000_000));
    }
}
