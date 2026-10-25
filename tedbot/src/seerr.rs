//! Seerr API client. Only the parts the bot uses.

use std::{collections::HashMap, fmt};

use anyhow::Result;
use poise::serenity_prelude::UserId;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

/// Where to reach Seerr and how to authenticate. The HTTP client is passed in per call so the bot
/// shares one connection pool.
#[derive(Debug)]
pub struct Seerr {
    pub url: String,
    pub api_key: String,
    /// Address people open in a browser, for links. `None` leaves links out.
    pub public_url: Option<String>,
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
    /// Permission bits
    #[serde(default)]
    permissions: u32,
}

/// Seerr's `MANAGE_REQUESTS` permission, which also allows bypassing quotas
const MANAGE_REQUESTS: u32 = 16;
/// Seerr's `ADMIN` permission, which implies every other
const ADMIN: u32 = 2;
/// Seerr's `REQUEST_ADVANCED` permission, for picking a quality profile
const REQUEST_ADVANCED: u32 = 8192;

/// The Seerr user a request is made as
#[derive(Clone, Copy, Debug)]
pub struct Requester {
    pub id: u32,
    /// Permission bits
    permissions: u32,
}

impl Requester {
    /// Whether they have any of `permissions`, as Seerr checks it
    const fn has_any(self, permissions: u32) -> bool {
        self.permissions & (permissions | ADMIN) != 0
    }

    /// Whether Seerr lets them skip their quota
    pub const fn can_bypass_quota(self) -> bool {
        self.has_any(MANAGE_REQUESTS)
    }

    /// Whether Seerr's request page offers them a quality profile. Seerr doesn't check this
    /// itself, so it's up to the bot.
    pub const fn can_pick_profile(self) -> bool {
        self.has_any(MANAGE_REQUESTS | REQUEST_ADVANCED)
    }
}

/// Response of `GET /api/v1/user/{id}/quota`
#[derive(Debug, Deserialize)]
pub struct Quotas {
    pub movie: Quota,
    /// Counted in seasons
    pub tv: Quota,
}

/// How many requests a user has left
#[derive(Debug, Deserialize)]
pub struct Quota {
    /// Over this many days. 0 means forever.
    #[serde(default)]
    pub days: u32,
    /// 0 when unlimited
    #[serde(default)]
    pub limit: u32,
    /// Absent when unlimited
    pub remaining: Option<u32>,
}

/// A Radarr or Sonarr server, from `GET /api/v1/service/{radarr,sonarr}`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Server {
    id: u32,
    is4k: bool,
    is_default: bool,
}

/// Response of `GET /api/v1/service/{radarr,sonarr}/{id}`
#[derive(Debug, Deserialize)]
struct ServerDetails {
    profiles: Vec<Profile>,
}

/// A quality profile
#[derive(Clone, Debug, Deserialize)]
pub struct Profile {
    pub id: u32,
    pub name: String,
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

/// Response of `GET /api/v1/search`
#[derive(Debug, Deserialize)]
struct Search {
    /// One page of movies, shows, people and collections
    results: Vec<SearchResult>,
}

/// One search result. People and collections can't be requested, so they're [`Self::Other`].
#[derive(Debug, Deserialize)]
#[serde(tag = "mediaType", rename_all = "lowercase")]
pub enum SearchResult {
    #[serde(rename_all = "camelCase")]
    Movie {
        /// TMDB id
        id: u32,
        title: String,
        /// `YYYY-MM-DD`, empty or absent if unknown
        release_date: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Tv {
        /// TMDB id
        id: u32,
        name: String,
        /// `YYYY-MM-DD`, empty or absent if unknown
        first_air_date: Option<String>,
    },
    #[serde(other)]
    Other,
}

impl SearchResult {
    /// The movie or show this result is, `None` for people and collections
    pub const fn media(&self) -> Option<Media> {
        match *self {
            Self::Movie { id, .. } => Some(Media::Movie(id)),
            Self::Tv { id, .. } => Some(Media::Tv(id)),
            Self::Other => None,
        }
    }

    /// "Title (2022)"
    pub fn label(&self) -> String {
        match self {
            Self::Movie {
                title,
                release_date: date,
                ..
            }
            | Self::Tv {
                name: title,
                first_air_date: date,
                ..
            } => with_year(title, date.as_deref()),
            Self::Other => String::new(),
        }
    }
}

/// A movie or show by TMDB id
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Media {
    Movie(u32),
    Tv(u32),
}

impl Media {
    /// The Seerr service that handles it
    const fn service(self) -> &'static str {
        match self {
            Self::Movie(_) => "radarr",
            Self::Tv(_) => "sonarr",
        }
    }

    /// The path under `/api/v1` and Seerr's web pages, e.g. `/tv/95396`
    fn path(self) -> String {
        match self {
            Self::Movie(id) => format!("/movie/{id}"),
            Self::Tv(id) => format!("/tv/{id}"),
        }
    }
}

impl fmt::Display for Media {
    /// `movie:123` or `tv:123`, parsed back by [`str::parse`]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Movie(id) => write!(f, "movie:{id}"),
            Self::Tv(id) => write!(f, "tv:{id}"),
        }
    }
}

impl std::str::FromStr for Media {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let (kind, id) = s.split_once(':').ok_or(())?;
        let id = id.parse().map_err(|_| ())?;
        match kind {
            "movie" => Ok(Self::Movie(id)),
            "tv" => Ok(Self::Tv(id)),
            _ => Err(()),
        }
    }
}

/// A movie or show's details, from `GET /api/v1/movie/{id}` or `GET /api/v1/tv/{id}`. A show's
/// `name` and `firstAirDate` are read into the movie field names.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Details {
    #[serde(alias = "name")]
    pub title: String,
    /// `YYYY-MM-DD`, empty or absent if unknown
    #[serde(alias = "firstAirDate")]
    pub release_date: Option<String>,
    pub overview: Option<String>,
    /// TMDB poster path, e.g. `/abc.jpg`
    pub poster_path: Option<String>,
    /// Seasons TMDB knows of, shows only
    #[serde(default)]
    pub seasons: Vec<Season>,
    /// What Seerr knows, absent if it's never been requested or seen in Jellyfin
    pub media_info: Option<MediaInfo>,
}

/// A season as TMDB lists it
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Season {
    /// 0 for specials
    pub season_number: u32,
    pub episode_count: u32,
}

/// Seerr's own record of a movie or show
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaInfo {
    /// Shows only, once Seerr has looked it up
    pub tvdb_id: Option<u32>,
    pub status: Status,
    /// Jellyfin page, once it's available
    pub media_url: Option<String>,
    /// Per-season status, shows only
    #[serde(default)]
    pub seasons: Vec<SeasonInfo>,
    #[serde(default)]
    pub requests: Vec<Request>,
}

/// A season's status
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonInfo {
    pub season_number: u32,
    pub status: Status,
}

/// A request for a movie or some seasons of a show
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: u32,
    pub status: RequestStatus,
    pub is4k: bool,
    /// Who made it
    pub requested_by: Option<RequestedBy>,
    /// Requested seasons, shows only. Their statuses here are request statuses.
    #[serde(default)]
    pub seasons: Vec<RequestSeason>,
}

/// The user who made a request
#[derive(Debug, Deserialize)]
pub struct RequestedBy {
    /// Seerr's user id
    pub id: u32,
}

/// One season of a show request
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSeason {
    pub season_number: u32,
}

/// Where a movie or season stands. Seerr's unknown and deleted are both [`Self::Missing`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(from = "u8")]
pub enum Status {
    /// Not requested, or deleted since
    Missing,
    /// Waiting for approval
    Pending,
    /// Approved, not downloaded yet
    Processing,
    PartiallyAvailable,
    Available,
    Blocklisted,
}

impl From<u8> for Status {
    fn from(n: u8) -> Self {
        match n {
            2 => Self::Pending,
            3 => Self::Processing,
            4 => Self::PartiallyAvailable,
            5 => Self::Available,
            6 => Self::Blocklisted,
            _ => Self::Missing,
        }
    }
}

/// A request's own status, separate from the media's
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(from = "u8")]
pub enum RequestStatus {
    Pending,
    Approved,
    Declined,
    Failed,
    Completed,
}

impl From<u8> for RequestStatus {
    fn from(n: u8) -> Self {
        match n {
            2 => Self::Approved,
            3 => Self::Declined,
            4 => Self::Failed,
            5 => Self::Completed,
            _ => Self::Pending,
        }
    }
}

impl Details {
    /// Where the movie, or the whole show, stands
    pub fn status(&self) -> Status {
        self.media_info
            .as_ref()
            .map_or(Status::Missing, |m| m.status)
    }

    /// Seasons that can be requested, in order: not specials, not empty
    pub fn season_numbers(&self) -> impl Iterator<Item = u32> {
        self.seasons
            .iter()
            .filter(|s| s.season_number > 0 && s.episode_count > 0)
            .map(|s| s.season_number)
    }

    /// Requests the viewer may cancel: ones waiting for approval or approved but not downloaded
    /// yet, not even in part. As `admin`, anyone's; otherwise only Seerr user `user`'s own.
    /// Declined and 4K requests are left out.
    pub fn cancellable(&self, user: Option<u32>, admin: bool) -> Vec<Cancellable> {
        let Some(info) = &self.media_info else {
            return Vec::new();
        };
        let downloaded = |s: Status| matches!(s, Status::Available | Status::PartiallyAvailable);
        info.requests
            .iter()
            .filter(|r| {
                !r.is4k
                    && matches!(r.status, RequestStatus::Pending | RequestStatus::Approved)
                    && (admin
                        || (user.is_some() && r.requested_by.as_ref().map(|by| by.id) == user))
            })
            .map(|r| Cancellable {
                id: r.id,
                sent: r.status == RequestStatus::Approved,
                seasons: r.seasons.iter().map(|s| s.season_number).collect(),
            })
            .filter(|c| {
                if self.seasons.is_empty() {
                    !downloaded(info.status)
                } else {
                    !c.seasons.iter().any(|&n| downloaded(self.season_status(n)))
                }
            })
            .collect()
    }

    /// Where season `n` stands. Like Seerr, a season in an open (not declined or completed)
    /// request counts as requested even if its own status hasn't caught up.
    pub fn season_status(&self, n: u32) -> Status {
        let Some(info) = &self.media_info else {
            return Status::Missing;
        };
        let own = info
            .seasons
            .iter()
            .find(|s| s.season_number == n)
            .map_or(Status::Missing, |s| s.status);
        if own != Status::Missing {
            return own;
        }
        info.requests
            .iter()
            .filter(|r| {
                !r.is4k && !matches!(r.status, RequestStatus::Declined | RequestStatus::Completed)
            })
            .find(|r| r.seasons.iter().any(|s| s.season_number == n))
            .map_or(Status::Missing, |r| match r.status {
                RequestStatus::Pending => Status::Pending,
                _ => Status::Processing,
            })
    }
}

/// Body of `POST /api/v1/request`
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NewRequest<'a> {
    media_type: &'static str,
    media_id: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    seasons: Option<&'a [u32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    profile_id: Option<u32>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    ignore_quota: bool,
}

/// What to request beyond the movie or show itself
#[derive(Debug, Default)]
pub struct Options<'a> {
    /// Seasons, shows only
    pub seasons: &'a [u32],
    /// Quality profile on the default server, `None` for Seerr's choice
    pub profile: Option<u32>,
    /// Don't count it against the requester's quota
    pub ignore_quota: bool,
}

/// Response of a successful `POST /api/v1/request`
#[derive(Debug, Deserialize)]
struct Created {
    status: RequestStatus,
}

/// Body of a Seerr error
#[derive(Debug, Deserialize)]
struct ErrorBody {
    message: Option<String>,
}

/// A request the viewer may cancel, and what cancelling it involves
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cancellable {
    pub id: u32,
    /// Approved, so it's been sent to Radarr or Sonarr and has to come out of there
    pub sent: bool,
    /// Requested seasons, shows only
    pub seasons: Vec<u32>,
}

/// What came of cancelling a request
#[derive(Debug)]
pub enum Cancelled {
    Done,
    /// Seerr said no, e.g. because it's been approved since. The message is Seerr's.
    Refused(String),
}

/// What came of a request
#[derive(Debug)]
pub enum Requested {
    /// Made, and approved already if `approved`
    Made { approved: bool },
    /// Seerr said no, e.g. over permissions, quota, or a duplicate. The message is Seerr's.
    Refused(String),
}

impl Seerr {
    /// Starts an authenticated `GET` of `path` (with a leading `/`)
    fn get(&self, http: &reqwest::Client, path: &str) -> reqwest::RequestBuilder {
        http.get(self.api(path)).header("X-Api-Key", &self.api_key)
    }

    /// The API URL of `path` (with a leading `/`)
    fn api(&self, path: &str) -> String {
        format!("{}/api/v1{path}", self.url.trim_end_matches('/'))
    }

    /// Seerr's web page for `media`, if [`Self::public_url`] is set
    pub fn page(&self, media: Media) -> Option<String> {
        self.public_url
            .as_deref()
            .map(|url| format!("{}{}", url.trim_end_matches('/'), media.path()))
    }

    /// The first nonzero Discord id in the notification settings of Seerr user `id`
    async fn discord_id(&self, http: &reqwest::Client, id: u32) -> Result<Option<UserId>> {
        let detail: UserDetail = self
            .get(http, &format!("/user/{id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(detail
            .settings
            .and_then(|s| s.discord_ids)
            .into_iter()
            .flatten()
            .find_map(|id| id.parse().ok().filter(|&id| id != 0))
            .map(UserId::new))
    }

    /// Every Seerr user, as listed
    async fn users(&self, http: &reqwest::Client) -> Result<Vec<User>> {
        let users: Users = self
            .get(http, "/user")
            .query(&[("take", "1000")])
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(users.results)
    }

    /// The Discord account each of `jellyfin_ids` linked in Seerr, keyed by Jellyfin user id.
    /// Users without a Seerr account or Discord id are left out.
    pub async fn discord_ids(
        &self,
        http: &reqwest::Client,
        jellyfin_ids: &[&str],
    ) -> Result<HashMap<String, UserId>> {
        let mut linked = HashMap::new();
        for user in self.users(http).await? {
            let Some(jellyfin_id) = user.jellyfin_user_id.map(|id| normalize(&id)) else {
                continue;
            };
            if !jellyfin_ids.iter().any(|id| normalize(id) == jellyfin_id) {
                continue;
            }
            if let Some(id) = self.discord_id(http, user.id).await? {
                linked.insert(jellyfin_id, id);
            }
        }
        Ok(linked)
    }

    /// The Seerr user who put `discord` in their notification settings, if any
    pub async fn user_for(
        &self,
        http: &reqwest::Client,
        discord: UserId,
    ) -> Result<Option<Requester>> {
        for user in self.users(http).await? {
            if self.discord_id(http, user.id).await? == Some(discord) {
                return Ok(Some(Requester {
                    id: user.id,
                    permissions: user.permissions,
                }));
            }
        }
        Ok(None)
    }

    /// How many requests Seerr user `id` has left
    pub async fn quotas(&self, http: &reqwest::Client, id: u32) -> Result<Quotas> {
        Ok(self
            .get(http, &format!("/user/{id}/quota"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Quality profiles on the default non-4K server for `media`, which is where a request
    /// without a server goes. Empty if there's no such server.
    pub async fn profiles(&self, http: &reqwest::Client, media: Media) -> Result<Vec<Profile>> {
        let service = media.service();
        let servers: Vec<Server> = self
            .get(http, &format!("/service/{service}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let Some(server) = servers.iter().find(|s| s.is_default && !s.is4k) else {
            return Ok(Vec::new());
        };
        let details: ServerDetails = self
            .get(http, &format!("/service/{service}/{}", server.id))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(details.profiles)
    }

    /// Movies and shows matching `query`, best match first. One page, so at most 20.
    pub async fn search(&self, http: &reqwest::Client, query: &str) -> Result<Vec<SearchResult>> {
        let search: Search = self
            .get(http, &format!("/search?query={}&page=1", encode(query)))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(search
            .results
            .into_iter()
            .filter(|r| r.media().is_some())
            .collect())
    }

    /// A movie or show's details and where it stands
    pub async fn details(&self, http: &reqwest::Client, media: Media) -> Result<Details> {
        Ok(self
            .get(http, &media.path())
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Requests `media` as Seerr user `user`, so their permissions, quota and auto-approval
    /// apply. Seasons are ignored for movies.
    pub async fn request(
        &self,
        http: &reqwest::Client,
        user: u32,
        media: Media,
        options: &Options<'_>,
    ) -> Result<Requested> {
        let (media_type, media_id, seasons) = match media {
            Media::Movie(id) => ("movie", id, None),
            Media::Tv(id) => ("tv", id, Some(options.seasons)),
        };
        let body = NewRequest {
            media_type,
            media_id,
            seasons,
            profile_id: options.profile,
            ignore_quota: options.ignore_quota,
        };
        let response = http
            .post(self.api("/request"))
            .header("X-Api-Key", &self.api_key)
            .header("X-API-User", user.to_string())
            .json(&body)
            .send()
            .await?;

        // Seerr answers 202 when every season was already requested, so only 201 means success
        match response.status() {
            StatusCode::CREATED => {
                let created: Created = response.json().await?;
                Ok(Requested::Made {
                    approved: created.status == RequestStatus::Approved,
                })
            }
            StatusCode::ACCEPTED | StatusCode::FORBIDDEN | StatusCode::CONFLICT => {
                let body: ErrorBody = response.json().await?;
                Ok(Requested::Refused(
                    body.message
                        .unwrap_or_else(|| "Seerr refused the request".into()),
                ))
            }
            status => {
                response.error_for_status()?;
                anyhow::bail!("unexpected status {status} from Seerr")
            }
        }
    }

    /// Deletes request `id`. Seerr only lets users delete their own requests while pending,
    /// unless they can manage requests, so with `user` `None` this acts as Seerr's admin and
    /// the caller has to have checked.
    pub async fn cancel(
        &self,
        http: &reqwest::Client,
        user: Option<u32>,
        id: u32,
    ) -> Result<Cancelled> {
        let mut request = http
            .delete(self.api(&format!("/request/{id}")))
            .header("X-Api-Key", &self.api_key);
        if let Some(user) = user {
            request = request.header("X-API-User", user.to_string());
        }
        let response = request.send().await?;
        match response.status() {
            StatusCode::NO_CONTENT => Ok(Cancelled::Done),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND => {
                let body: ErrorBody = response.json().await?;
                Ok(Cancelled::Refused(body.message.unwrap_or_else(|| {
                    "Seerr refused to cancel the request".into()
                })))
            }
            status => {
                response.error_for_status()?;
                anyhow::bail!("unexpected status {status} from Seerr")
            }
        }
    }
}

/// "Title (2022)", or just the title without a usable date
pub fn with_year(title: &str, date: Option<&str>) -> String {
    date.and_then(|d| d.get(..4))
        .map_or_else(|| title.to_owned(), |year| format!("{title} ({year})"))
}

/// Percent-encodes everything but unreserved characters. Seerr rejects a query containing any
/// reserved character, even `+` for a space, which rules out form encoding.
fn encode(s: &str) -> String {
    s.bytes()
        .fold(String::with_capacity(s.len()), |mut out, b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(b));
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
            out
        })
}

/// Jellyfin user ids without dashes and in lowercase, so either spelling of a GUID matches
pub fn normalize(id: &str) -> String {
    id.chars()
        .filter(|&c| c != '-')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_results() {
        let search: Search = serde_json::from_str(
            r#"{"results": [
                {"id": 95396, "mediaType": "tv", "name": "Severance", "firstAirDate": "2022-02-17"},
                {"id": 5072, "mediaType": "movie", "title": "Severance", "releaseDate": ""},
                {"id": 18891, "mediaType": "person", "name": "Joan Severance"}
            ]}"#,
        )
        .unwrap();
        let labels: Vec<_> = search.results.iter().map(SearchResult::label).collect();
        assert_eq!(labels, ["Severance (2022)", "Severance", ""]);
        let media: Vec<_> = search.results.iter().map(SearchResult::media).collect();
        assert_eq!(
            media,
            [Some(Media::Tv(95396)), Some(Media::Movie(5072)), None]
        );
    }

    #[test]
    fn query_encoding() {
        assert_eq!(encode("the office's"), "the%20office%27s");
        assert_eq!(encode("amélie!"), "am%C3%A9lie%21");
        assert_eq!(encode("a-b.c_d~e"), "a-b.c_d~e");
    }

    #[test]
    fn cancellable() {
        let show: Details = serde_json::from_str(
            r#"{
                "name": "Show",
                "seasons": [
                    {"seasonNumber": 1, "episodeCount": 10},
                    {"seasonNumber": 2, "episodeCount": 10},
                    {"seasonNumber": 3, "episodeCount": 10}
                ],
                "mediaInfo": {
                    "status": 4,
                    "seasons": [
                        {"seasonNumber": 1, "status": 5},
                        {"seasonNumber": 2, "status": 3},
                        {"seasonNumber": 3, "status": 2}
                    ],
                    "requests": [
                        {"id": 1, "status": 5, "is4k": false, "requestedBy": {"id": 5},
                         "seasons": [{"seasonNumber": 1}]},
                        {"id": 2, "status": 2, "is4k": false, "requestedBy": {"id": 5},
                         "seasons": [{"seasonNumber": 2}]},
                        {"id": 3, "status": 1, "is4k": false, "requestedBy": {"id": 8},
                         "seasons": [{"seasonNumber": 3}]},
                        {"id": 4, "status": 3, "is4k": false, "requestedBy": {"id": 5},
                         "seasons": [{"seasonNumber": 3}]}
                    ]
                }
            }"#,
        )
        .unwrap();
        let ids = |c: Vec<Cancellable>| c.iter().map(|c| c.id).collect::<Vec<_>>();
        // Not season 1, which is downloaded, or the declined one
        assert_eq!(ids(show.cancellable(Some(5), false)), [2]);
        assert_eq!(ids(show.cancellable(Some(8), false)), [3]);
        assert!(show.cancellable(None, false).is_empty());
        // Admins get anyone's, but still nothing downloaded
        let all = show.cancellable(None, true);
        assert_eq!(
            all,
            [
                Cancellable {
                    id: 2,
                    sent: true,
                    seasons: vec![2],
                },
                Cancellable {
                    id: 3,
                    sent: false,
                    seasons: vec![3],
                },
            ]
        );
    }

    #[test]
    fn permissions() {
        let user = |permissions| Requester { id: 1, permissions };
        // tb: MANAGE_REQUESTS, REQUEST_ADVANCED and others
        assert!(user(336_617_648).can_bypass_quota());
        assert!(user(336_617_648).can_pick_profile());
        // REQUEST and views only
        assert!(!user(69_222_432).can_bypass_quota());
        assert!(!user(69_222_432).can_pick_profile());
        assert!(user(REQUEST_ADVANCED).can_pick_profile());
        assert!(!user(REQUEST_ADVANCED).can_bypass_quota());
        assert!(user(ADMIN).can_bypass_quota());
    }

    #[test]
    fn media_round_trip() {
        for media in [Media::Movie(693_134), Media::Tv(95396)] {
            assert_eq!(media.to_string().parse(), Ok(media));
        }
        assert!("Severance".parse::<Media>().is_err());
        assert!("person:1".parse::<Media>().is_err());
    }

    #[test]
    fn season_status() {
        let show: Details = serde_json::from_str(
            r#"{
                "name": "Nobody Wants This",
                "firstAirDate": "2024-09-26",
                "seasons": [
                    {"seasonNumber": 0, "episodeCount": 2},
                    {"seasonNumber": 1, "episodeCount": 10},
                    {"seasonNumber": 2, "episodeCount": 10},
                    {"seasonNumber": 3, "episodeCount": 10},
                    {"seasonNumber": 4, "episodeCount": 0}
                ],
                "mediaInfo": {
                    "status": 4,
                    "seasons": [
                        {"seasonNumber": 1, "status": 1},
                        {"seasonNumber": 2, "status": 1},
                        {"seasonNumber": 3, "status": 5}
                    ],
                    "requests": [
                        {"id": 1, "status": 1, "is4k": false, "seasons": [{"seasonNumber": 2}]},
                        {"id": 2, "status": 3, "is4k": false, "seasons": [{"seasonNumber": 1}]},
                        {"id": 3, "status": 5, "is4k": false, "seasons": [{"seasonNumber": 3}]}
                    ]
                }
            }"#,
        )
        .unwrap();
        assert_eq!(show.title, "Nobody Wants This");
        assert_eq!(show.season_numbers().collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(show.season_status(1), Status::Missing);
        assert_eq!(show.season_status(2), Status::Pending);
        assert_eq!(show.season_status(3), Status::Available);
        assert_eq!(show.status(), Status::PartiallyAvailable);
    }
}
