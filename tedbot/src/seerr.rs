//! Seerr API client. Only the parts the bot uses.

use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use poise::serenity_prelude::UserId;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

/// Where to reach Seerr and how to authenticate. The HTTP client is passed in per call so the bot
/// shares one connection pool.
#[derive(Clone, Debug)]
pub struct Seerr {
    pub url: String,
    pub api_key: String,
    /// Address people open in a browser, for links. `None` leaves links out.
    pub public_url: Option<String>,
    /// Who linked which Discord account, for [`Self::user_for`]
    pub links: Links,
}

/// How long [`Links`] are kept before they're fetched again
const LINKS_TTL: Duration = Duration::from_secs(60);

/// Seerr users by the Discord account they linked, as fetched at some instant
type Linked = (Instant, HashMap<UserId, Requester>);

/// The last [`Linked`] fetched, if any
#[derive(Clone, Debug, Default)]
pub struct Links(Arc<tokio::sync::Mutex<Option<Linked>>>);

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
/// Seerr's `REQUEST_4K` permission, for 4K movies and shows
const REQUEST_4K: u32 = 1024;
/// Seerr's `REQUEST_4K_MOVIE` permission
const REQUEST_4K_MOVIE: u32 = 2048;
/// Seerr's `REQUEST_4K_TV` permission
const REQUEST_4K_TV: u32 = 4096;

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

    /// Whether Seerr lets them request `media` in 4K
    pub const fn can_request_4k(self, media: Media) -> bool {
        self.has_any(match media {
            Media::Movie(_) => REQUEST_4K | REQUEST_4K_MOVIE,
            Media::Tv(_) => REQUEST_4K | REQUEST_4K_TV,
        })
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
    is_4k: bool,
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

/// The quality profiles on a Radarr or Sonarr server
#[derive(Clone, Debug, Default)]
pub struct Profiles {
    /// Seerr's id for the server, unique among the Radarrs or the Sonarrs
    pub server: u32,
    /// Whether it's the server for movies (Radarr) rather than shows (Sonarr)
    pub movies: bool,
    /// Whether it's a 4K server
    pub is_4k: bool,
    pub list: Vec<Profile>,
}

impl Profiles {
    /// Profile `id`'s name
    pub fn name(&self, id: u32) -> Option<&str> {
        self.list
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.as_str())
    }
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
    /// The 4K copy's
    #[serde(default)]
    pub status_4k: Status,
    /// Jellyfin page, once it's available
    pub media_url: Option<String>,
    pub media_url_4k: Option<String>,
    /// Jellyfin item id, once it's available
    pub jellyfin_media_id: Option<String>,
    pub jellyfin_media_id_4k: Option<String>,
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
    /// The 4K copy's
    #[serde(default)]
    pub status_4k: Status,
}

/// A request for a movie or some seasons of a show
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: u32,
    pub status: RequestStatus,
    pub is_4k: bool,
    /// What was requested. Absent when listed under the media itself.
    pub media: Option<RequestMedia>,
    /// When it was made, RFC 3339
    pub created_at: Option<String>,
    /// Who made it
    pub requested_by: Option<RequestedBy>,
    /// Who last changed its status: the requester if it was approved automatically, else
    /// whoever approved or declined it
    pub modified_by: Option<RequestedBy>,
    /// Requested seasons, shows only. Their statuses here are request statuses.
    #[serde(default)]
    pub seasons: Vec<RequestSeason>,
    /// Radarr or Sonarr server, `None` for the default
    pub server_id: Option<u32>,
    /// Quality profile, `None` for the server's default
    pub profile_id: Option<u32>,
    pub root_folder: Option<String>,
    /// Sonarr only
    pub language_profile_id: Option<u32>,
    pub tags: Option<Vec<u32>>,
}

/// The movie or show a request is for
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestMedia {
    /// TMDB id
    pub tmdb_id: u32,
    /// `movie` or `tv`
    pub media_type: String,
}

impl Request {
    /// The movie or show it's for, if Seerr said
    pub fn media(&self) -> Option<Media> {
        let media = self.media.as_ref()?;
        match media.media_type.as_str() {
            "movie" => Some(Media::Movie(media.tmdb_id)),
            "tv" => Some(Media::Tv(media.tmdb_id)),
            _ => None,
        }
    }

    /// Whether it was approved, or declined, by the requester's own permissions rather than by
    /// someone else
    pub fn automatic(&self) -> bool {
        let id = |by: &Option<RequestedBy>| by.as_ref().map(|by| by.id);
        id(&self.modified_by).is_some() && id(&self.modified_by) == id(&self.requested_by)
    }

    /// Requested season numbers, shows only
    pub fn season_numbers(&self) -> Vec<u32> {
        self.seasons.iter().map(|s| s.season_number).collect()
    }
}

/// A user a request refers to
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestedBy {
    /// Seerr's user id
    pub id: u32,
    pub display_name: Option<String>,
}

/// Response of `GET /api/v1/request`
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestList {
    page_info: PageInfo,
    results: Vec<Request>,
}

/// Paging of a list
#[derive(Debug, Deserialize)]
struct PageInfo {
    /// Total across every page
    results: u32,
}

/// One page of requests
#[derive(Debug)]
pub struct RequestPage {
    /// Requests matching, across every page
    pub total: u32,
    pub requests: Vec<Request>,
}

/// One season of a show request
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSeason {
    pub season_number: u32,
}

/// Where a movie or season stands. Seerr's unknown and deleted are both [`Self::Missing`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(from = "u8")]
pub enum Status {
    /// Not requested, or deleted since
    #[default]
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

impl MediaInfo {
    /// The Jellyfin page of the HD or 4K copy
    pub fn jellyfin_url(&self, is_4k: bool) -> Option<&str> {
        if is_4k {
            self.media_url_4k.as_deref()
        } else {
            self.media_url.as_deref()
        }
    }

    /// The HD or 4K copy's page in Moonfin Web, which the Moonfin plugin serves on the Jellyfin
    /// server that [`Self::jellyfin_url`] points to
    pub fn moonfin_url(&self, is_4k: bool) -> Option<String> {
        let (server, _) = self.jellyfin_url(is_4k)?.split_once("/web/")?;
        let id = if is_4k {
            &self.jellyfin_media_id_4k
        } else {
            &self.jellyfin_media_id
        };
        Some(format!("{server}/Moonfin/Web/item/{}", id.as_deref()?))
    }
}

impl Details {
    /// Where the movie, or the whole show, stands
    pub fn status(&self) -> Status {
        self.status_of(false)
    }

    /// Where the HD or 4K copy of the movie, or the whole show, stands
    pub fn status_of(&self, is_4k: bool) -> Status {
        self.media_info.as_ref().map_or(
            Status::Missing,
            |m| {
                if is_4k { m.status_4k } else { m.status }
            },
        )
    }

    /// Seasons that can be requested, in order: not specials, not empty
    pub fn season_numbers(&self) -> impl Iterator<Item = u32> {
        self.seasons
            .iter()
            .filter(|s| s.season_number > 0 && s.episode_count > 0)
            .map(|s| s.season_number)
    }

    /// Requests the viewer may cancel: ones waiting for approval or approved but not downloaded
    /// yet (in the copy they asked for, HD or 4K), not even in part. As `admin`, anyone's;
    /// otherwise only Seerr user `user`'s own. Declined requests are left out.
    pub fn cancellable(&self, user: Option<u32>, admin: bool) -> Vec<Cancellable> {
        let Some(info) = &self.media_info else {
            return Vec::new();
        };
        let downloaded = |s: Status| matches!(s, Status::Available | Status::PartiallyAvailable);
        info.requests
            .iter()
            .filter(|r| {
                matches!(r.status, RequestStatus::Pending | RequestStatus::Approved)
                    && (admin
                        || (user.is_some() && r.requested_by.as_ref().map(|by| by.id) == user))
            })
            .map(|r| Cancellable {
                id: r.id,
                sent: r.status == RequestStatus::Approved,
                is_4k: r.is_4k,
                seasons: r.seasons.iter().map(|s| s.season_number).collect(),
            })
            .filter(|c| {
                if self.seasons.is_empty() {
                    !downloaded(self.status_of(c.is_4k))
                } else {
                    !c.seasons
                        .iter()
                        .any(|&n| downloaded(self.season_status_of(n, c.is_4k)))
                }
            })
            .collect()
    }

    /// Whether the approved request covering season `n`, or the movie with `None`, was approved
    /// automatically rather than by someone else. `false` if no approved request covers it.
    pub fn auto_approved(&self, n: Option<u32>, is_4k: bool) -> bool {
        self.media_info
            .iter()
            .flat_map(|m| &m.requests)
            .filter(|r| r.is_4k == is_4k && r.status == RequestStatus::Approved)
            .find(|r| n.is_none_or(|n| r.seasons.iter().any(|s| s.season_number == n)))
            .is_some_and(Request::automatic)
    }

    /// Where what `request` asked for stands: the movie, or the request's seasons together, in
    /// HD or 4K as it asked
    pub fn request_status(&self, request: &Request) -> Status {
        if request.seasons.is_empty() {
            return self.status_of(request.is_4k);
        }
        let statuses: Vec<Status> = request
            .seasons
            .iter()
            .map(|s| self.season_status_of(s.season_number, request.is_4k))
            .collect();
        if statuses.iter().all(|&s| s == Status::Available) {
            Status::Available
        } else if statuses
            .iter()
            .any(|s| matches!(s, Status::Available | Status::PartiallyAvailable))
        {
            Status::PartiallyAvailable
        } else {
            statuses
                .into_iter()
                .max_by_key(|&s| s == Status::Processing)
                .unwrap_or(Status::Missing)
        }
    }

    /// Where season `n` stands. Like Seerr, a season in an open (not declined or completed)
    /// request counts as requested even if its own status hasn't caught up.
    pub fn season_status(&self, n: u32) -> Status {
        self.season_status_of(n, false)
    }

    /// Counts `seasons` of the HD or 4K version as downloaded where Seerr has them as partly
    /// downloaded
    pub fn mark_downloaded(&mut self, seasons: &[u32], is_4k: bool) {
        let Some(info) = &mut self.media_info else {
            return;
        };
        for season in info
            .seasons
            .iter_mut()
            .filter(|s| seasons.contains(&s.season_number))
        {
            let status = if is_4k {
                &mut season.status_4k
            } else {
                &mut season.status
            };
            if *status == Status::PartiallyAvailable {
                *status = Status::Available;
            }
        }
    }

    /// Where the HD or 4K copy of season `n` stands, as [`Self::season_status`]
    pub fn season_status_of(&self, n: u32, is_4k: bool) -> Status {
        let Some(info) = &self.media_info else {
            return Status::Missing;
        };
        let own = info
            .seasons
            .iter()
            .find(|s| s.season_number == n)
            .map_or(
                Status::Missing,
                |s| if is_4k { s.status_4k } else { s.status },
            );
        if own != Status::Missing {
            return own;
        }
        info.requests
            .iter()
            .filter(|r| {
                r.is_4k == is_4k
                    && !matches!(r.status, RequestStatus::Declined | RequestStatus::Completed)
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    is_4k: bool,
}

/// What to request beyond the movie or show itself
#[derive(Debug, Default)]
pub struct Options<'a> {
    /// Seasons, shows only
    pub seasons: &'a [u32],
    /// Quality profile on the default (HD or 4K) server, `None` for Seerr's choice
    pub profile: Option<u32>,
    /// Don't count it against the requester's quota
    pub ignore_quota: bool,
    /// Request the 4K copy, which goes to Seerr's default 4K server
    pub is_4k: bool,
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
    /// For the 4K copy, which has its own Radarr or Sonarr
    pub is_4k: bool,
    /// Requested seasons, shows only
    pub seasons: Vec<u32>,
}

/// What came of changing a request: cancelling, approving, declining or editing it
#[derive(Debug)]
pub enum Changed {
    Done,
    /// Seerr said no, e.g. because it's been approved since. The message is Seerr's.
    Refused(String),
}

/// Body of `PUT /api/v1/request/{id}`. Seerr sets every field from it, so the ones not being
/// changed are sent back as they were.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EditRequest<'a> {
    media_type: &'static str,
    server_id: Option<u32>,
    profile_id: Option<u32>,
    root_folder: Option<&'a str>,
    language_profile_id: Option<u32>,
    tags: Option<&'a [u32]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seasons: Option<&'a [u32]>,
}

/// What came of a request
#[derive(Debug)]
pub enum Requested {
    Made,
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
    pub async fn discord_id(&self, http: &reqwest::Client, id: u32) -> Result<Option<UserId>> {
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

    /// The Seerr user who put `discord` in their notification settings, if any. Every command
    /// and autocomplete asks, so who linked what is fetched at most once per [`LINKS_TTL`].
    pub async fn user_for(
        &self,
        http: &reqwest::Client,
        discord: UserId,
    ) -> Result<Option<Requester>> {
        let mut links = self.links.0.lock().await;
        if !links
            .as_ref()
            .is_some_and(|(at, _)| at.elapsed() < LINKS_TTL)
        {
            *links = Some((Instant::now(), self.fetch_links(http).await?));
        }
        Ok(links
            .as_ref()
            .and_then(|(_, linked)| linked.get(&discord).copied()))
    }

    /// Fetches who linked what now, whatever its age, returning how many Seerr users linked a
    /// Discord account
    pub async fn refresh_links(&self, http: &reqwest::Client) -> Result<usize> {
        let mut links = self.links.0.lock().await;
        let fetched = self.fetch_links(http).await?;
        let count = fetched.len();
        *links = Some((Instant::now(), fetched));
        drop(links);
        Ok(count)
    }

    /// Seerr users by the Discord account they linked
    async fn fetch_links(&self, http: &reqwest::Client) -> Result<HashMap<UserId, Requester>> {
        let mut fetched = HashMap::new();
        for user in self.users(http).await? {
            if let Some(id) = self.discord_id(http, user.id).await? {
                fetched.entry(id).or_insert(Requester {
                    id: user.id,
                    permissions: user.permissions,
                });
            }
        }
        Ok(fetched)
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

    /// Quality profiles on the default HD or 4K server for `media`, which is where a request
    /// without a server goes. `None` if there's no such server.
    pub async fn profiles(
        &self,
        http: &reqwest::Client,
        media: Media,
        is_4k: bool,
    ) -> Result<Option<Profiles>> {
        let service = media.service();
        let servers: Vec<Server> = self
            .get(http, &format!("/service/{service}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let Some(server) = servers.iter().find(|s| s.is_default && s.is_4k == is_4k) else {
            return Ok(None);
        };
        let details: ServerDetails = self
            .get(http, &format!("/service/{service}/{}", server.id))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(Some(Profiles {
            server: server.id,
            movies: matches!(media, Media::Movie(_)),
            is_4k,
            list: details.profiles,
        }))
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
            is_4k: options.is_4k,
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
            StatusCode::CREATED => Ok(Requested::Made),
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
    ) -> Result<Changed> {
        let mut request = http
            .delete(self.api(&format!("/request/{id}")))
            .header("X-Api-Key", &self.api_key);
        if let Some(user) = user {
            request = request.header("X-API-User", user.to_string());
        }
        changed(
            request.send().await?,
            StatusCode::NO_CONTENT,
            "Seerr refused to cancel the request",
        )
        .await
    }

    /// Requests matching Seerr's `filter` (`all`, `pending`, `processing`, `available`, ...),
    /// newest first, skipping `skip` and taking up to `take`. Only Seerr user `user`'s with
    /// `Some`.
    pub async fn requests(
        &self,
        http: &reqwest::Client,
        filter: &str,
        user: Option<u32>,
        skip: usize,
        take: usize,
    ) -> Result<RequestPage> {
        let mut query = vec![
            ("filter", filter.to_owned()),
            ("sort", "added".into()),
            ("skip", skip.to_string()),
            ("take", take.to_string()),
        ];
        if let Some(user) = user {
            query.push(("requestedBy", user.to_string()));
        }
        let list: RequestList = self
            .get(http, "/request")
            .query(&query)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(RequestPage {
            total: list.page_info.results,
            requests: list.results,
        })
    }

    /// Request `id`, `None` if it's been deleted
    pub async fn request_by_id(&self, http: &reqwest::Client, id: u32) -> Result<Option<Request>> {
        let response = self.get(http, &format!("/request/{id}")).send().await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(response.error_for_status()?.json().await?))
    }

    /// Approves or declines pending request `id` as Seerr's admin, so the caller has to have
    /// checked who may
    pub async fn review(&self, http: &reqwest::Client, id: u32, approve: bool) -> Result<Changed> {
        let action = if approve { "approve" } else { "decline" };
        let response = http
            .post(self.api(&format!("/request/{id}/{action}")))
            .header("X-Api-Key", &self.api_key)
            .send()
            .await?;
        changed(
            response,
            StatusCode::OK,
            "Seerr refused to change the request",
        )
        .await
    }

    /// Changes pending `request`'s quality profile and, for shows, seasons, as Seerr's admin, so
    /// the caller has to have checked who may. Everything else stays as it was.
    pub async fn edit(
        &self,
        http: &reqwest::Client,
        request: &Request,
        profile: Option<u32>,
        seasons: &[u32],
    ) -> Result<Changed> {
        let media_type = match request.media() {
            Some(Media::Movie(_)) => "movie",
            Some(Media::Tv(_)) => "tv",
            None => anyhow::bail!("request {} isn't for a movie or show", request.id),
        };
        let body = EditRequest {
            media_type,
            server_id: request.server_id,
            profile_id: profile,
            root_folder: request.root_folder.as_deref(),
            language_profile_id: request.language_profile_id,
            tags: request.tags.as_deref(),
            seasons: (media_type == "tv").then_some(seasons),
        };
        let response = http
            .put(self.api(&format!("/request/{}", request.id)))
            .header("X-Api-Key", &self.api_key)
            .json(&body)
            .send()
            .await?;
        changed(
            response,
            StatusCode::OK,
            "Seerr refused to change the request",
        )
        .await
    }
}

/// Reads a response to changing a request: `ok` is success, and refusals carry Seerr's message,
/// or `fallback` without one. Seerr answers 202 when an edit leaves no seasons to request.
async fn changed(response: reqwest::Response, ok: StatusCode, fallback: &str) -> Result<Changed> {
    match response.status() {
        status if status == ok => Ok(Changed::Done),
        StatusCode::ACCEPTED
        | StatusCode::UNAUTHORIZED
        | StatusCode::FORBIDDEN
        | StatusCode::NOT_FOUND
        | StatusCode::CONFLICT => {
            let body: ErrorBody = response.json().await.unwrap_or(ErrorBody { message: None });
            Ok(Changed::Refused(
                body.message.unwrap_or_else(|| fallback.into()),
            ))
        }
        status => {
            response.error_for_status()?;
            anyhow::bail!("unexpected status {status} from Seerr")
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
    fn moonfin_url() {
        let info: MediaInfo = serde_json::from_str(
            r##"{
                "status": 5,
                "mediaUrl": "https://stream.example.com/web/index.html#!/details?id=abc&context=home",
                "jellyfinMediaId": "abc"
            }"##,
        )
        .unwrap();
        assert_eq!(
            info.moonfin_url(false).as_deref(),
            Some("https://stream.example.com/Moonfin/Web/item/abc")
        );
        assert_eq!(info.moonfin_url(true), None);
        let uhd: MediaInfo = serde_json::from_str(
            r##"{
                "status": 1,
                "status4k": 5,
                "mediaUrl4k": "https://stream.example.com/web/index.html#!/details?id=def",
                "jellyfinMediaId4k": "def"
            }"##,
        )
        .unwrap();
        assert_eq!(uhd.status_4k, Status::Available);
        assert_eq!(
            uhd.moonfin_url(true).as_deref(),
            Some("https://stream.example.com/Moonfin/Web/item/def")
        );
        let missing: MediaInfo = serde_json::from_str(r#"{"status": 3}"#).unwrap();
        assert_eq!(missing.moonfin_url(false), None);
    }

    #[test]
    fn request_status_4k() {
        let movie: Details = serde_json::from_str(
            r#"{
                "title": "Dune: Part Two",
                "mediaInfo": {"status": 1, "status4k": 5, "requests": []}
            }"#,
        )
        .unwrap();
        let request = |is_4k| Request {
            is_4k,
            ..serde_json::from_str(r#"{"id": 1, "status": 5, "is4k": false}"#).unwrap()
        };
        assert_eq!(movie.request_status(&request(true)), Status::Available);
        assert_eq!(movie.request_status(&request(false)), Status::Missing);
    }

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
                        {"seasonNumber": 1, "status": 5, "status4k": 3},
                        {"seasonNumber": 2, "status": 3, "status4k": 5},
                        {"seasonNumber": 3, "status": 2}
                    ],
                    "requests": [
                        {"id": 1, "status": 5, "is4k": false, "requestedBy": {"id": 5},
                         "seasons": [{"seasonNumber": 1}]},
                        {"id": 5, "status": 2, "is4k": true, "requestedBy": {"id": 5},
                         "seasons": [{"seasonNumber": 1}]},
                        {"id": 6, "status": 2, "is4k": true, "requestedBy": {"id": 5},
                         "seasons": [{"seasonNumber": 2}]},
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
        // Not season 1, which is downloaded, or the declined one. 4K goes by the 4K copy: season
        // 1 isn't downloaded in 4K, season 2 is.
        assert_eq!(ids(show.cancellable(Some(5), false)), [5, 2]);
        assert_eq!(ids(show.cancellable(Some(8), false)), [3]);
        assert!(show.cancellable(None, false).is_empty());
        // Admins get anyone's, but still nothing downloaded
        let all = show.cancellable(None, true);
        assert_eq!(
            all,
            [
                Cancellable {
                    id: 5,
                    sent: true,
                    is_4k: true,
                    seasons: vec![1],
                },
                Cancellable {
                    id: 2,
                    sent: true,
                    is_4k: false,
                    seasons: vec![2],
                },
                Cancellable {
                    id: 3,
                    sent: false,
                    is_4k: false,
                    seasons: vec![3],
                },
            ]
        );
    }

    #[test]
    fn request_list() {
        let list: RequestList = serde_json::from_str(
            r#"{
                "pageInfo": {"pages": 79, "pageSize": 1, "results": 236, "page": 1},
                "results": [{
                    "id": 255, "status": 2, "is4k": false, "serverId": null, "profileId": 7,
                    "rootFolder": null, "languageProfileId": null, "tags": null,
                    "createdAt": "2026-10-25T23:18:09.000Z", "type": "tv",
                    "media": {"tmdbId": 1920, "tvdbId": 70533, "mediaType": "tv", "status": 3},
                    "requestedBy": {"id": 8, "displayName": "someone", "settings": null},
                    "modifiedBy": {"id": 1, "displayName": "admin"},
                    "seasons": [{"id": 70, "seasonNumber": 1, "status": 2},
                                {"id": 71, "seasonNumber": 2, "status": 2}]
                }]
            }"#,
        )
        .unwrap();
        assert_eq!(list.page_info.results, 236);
        let request = &list.results[0];
        assert_eq!(request.media(), Some(Media::Tv(1920)));
        assert_eq!(request.season_numbers(), [1, 2]);
        assert_eq!(request.profile_id, Some(7));
        assert!(!request.automatic());

        let show: Details = serde_json::from_str(
            r#"{
                "name": "Show",
                "mediaInfo": {
                    "status": 4,
                    "seasons": [
                        {"seasonNumber": 1, "status": 5},
                        {"seasonNumber": 2, "status": 3},
                        {"seasonNumber": 3, "status": 5}
                    ]
                }
            }"#,
        )
        .unwrap();
        assert_eq!(show.request_status(request), Status::PartiallyAvailable);
        let only = |n| Request {
            seasons: vec![RequestSeason { season_number: n }],
            ..serde_json::from_str(r#"{"id": 1, "status": 2, "is4k": false}"#).unwrap()
        };
        assert_eq!(show.request_status(&only(2)), Status::Processing);
        assert_eq!(show.request_status(&only(3)), Status::Available);
    }

    #[test]
    fn auto_approved() {
        let show: Details = serde_json::from_str(
            r#"{
                "name": "Show",
                "mediaInfo": {
                    "status": 3,
                    "requests": [
                        {"id": 1, "status": 2, "is4k": false, "requestedBy": {"id": 5},
                         "modifiedBy": {"id": 5}, "seasons": [{"seasonNumber": 1}]},
                        {"id": 2, "status": 2, "is4k": false, "requestedBy": {"id": 8},
                         "modifiedBy": {"id": 1}, "seasons": [{"seasonNumber": 2}]},
                        {"id": 3, "status": 1, "is4k": false, "requestedBy": {"id": 8},
                         "seasons": [{"seasonNumber": 3}]}
                    ]
                }
            }"#,
        )
        .unwrap();
        assert!(show.auto_approved(Some(1), false));
        assert!(!show.auto_approved(Some(2), false));
        assert!(!show.auto_approved(Some(3), false));
    }

    #[test]
    fn permissions() {
        let user = |permissions| Requester { id: 1, permissions };
        // tb: MANAGE_REQUESTS, REQUEST_ADVANCED and others
        assert!(user(336_617_648).can_bypass_quota());
        // REQUEST and views only
        assert!(!user(69_222_432).can_bypass_quota());
        // REQUEST_ADVANCED
        assert!(!user(8192).can_bypass_quota());
        assert!(user(ADMIN).can_bypass_quota());
        assert!(user(REQUEST_4K_TV).can_request_4k(Media::Tv(1)));
        assert!(!user(REQUEST_4K_TV).can_request_4k(Media::Movie(1)));
        assert!(user(REQUEST_4K).can_request_4k(Media::Movie(1)));
        assert!(!user(69_222_432).can_request_4k(Media::Movie(1)));
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
