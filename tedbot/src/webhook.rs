//! Seerr's webhook. Seerr only has one, so the bot takes it and passes each notification on to
//! each of `seerr.webhook.forward`, answering Seerr with the first failed response, else the
//! first one.
//!
//! New and available requests are posted in Discord ([`announce`]), and request posts are kept
//! up to date as Seerr says their requests change. Seerr doesn't say when a show is partly
//! downloaded or a download starts, so open requests' posts are also rebuilt every
//! [`SWEEP_EVERY`], starting with those found in the requests channels at startup.
//!
//! Listens on localhost only. Point Seerr's webhook URL at `http://localhost:<port>/seerr`.
//! `GET /health` answers 200, for uptime probes; it's only served once Discord is connected.

use std::{
    collections::{HashMap, HashSet},
    net::Ipv4Addr,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context as _, Result};
use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse as _, Response},
    routing::{get, post},
};
use futures::future::join_all;
use poise::serenity_prelude::{ChannelId, GetMessages, Http, MessageId, UserId};
use serde::Deserialize;
use tokio::{net::TcpListener, time::MissedTickBehavior};
use tracing::{error, info, warn};

use crate::{
    Data,
    announce::{self, Event, Targets},
    commands::requests::{post_request, refresh_post},
};

/// How often open requests' posts are rebuilt
const SWEEP_EVERY: Duration = Duration::from_secs(15 * 60);
/// How many of each requests channel's latest messages are searched for posts at startup
const HISTORY: usize = 1000;

/// The bound listener, waiting for the Discord connection before serving
#[derive(Debug)]
pub struct Listener {
    listener: TcpListener,
    forward: Vec<String>,
    targets: Targets,
}

/// What a notification handler needs
struct Hook {
    discord: Arc<Http>,
    data: Arc<Data>,
    /// Where to pass notifications on to
    forward: Vec<String>,
    targets: Targets,
    /// Requests posted, since Seerr can say the same thing more than once
    posted: Mutex<HashSet<(Event, u32)>>,
    /// Each open request's posts in the requests channels
    posts: Mutex<HashMap<u32, Vec<(ChannelId, MessageId)>>>,
}

/// The parts of a notification the bot reads. Seerr's default payload and Moonfin's both have
/// them.
#[derive(Debug, Deserialize)]
struct Notification {
    /// e.g. `MEDIA_AVAILABLE`
    notification_type: String,
    /// `null` for notifications about something other than a request
    request: Option<NotificationRequest>,
}

#[derive(Debug, Deserialize)]
struct NotificationRequest {
    /// The request's id, as a string
    request_id: String,
}

impl Listener {
    /// Binds localhost:`port`, so a port in use fails at startup
    pub async fn bind(port: u16, forward: Vec<String>, targets: Targets) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .with_context(|| format!("binding localhost:{port} for the webhook"))?;
        Ok(Self {
            listener,
            forward,
            targets,
        })
    }

    /// Serves in the background for as long as the bot runs
    pub fn spawn(self, discord: Arc<Http>, data: Arc<Data>) {
        let hook = Arc::new(Hook {
            discord,
            data,
            forward: self.forward,
            targets: self.targets,
            posted: Mutex::default(),
            posts: Mutex::default(),
        });
        if !hook.targets.requests.is_empty() {
            let hook = hook.clone();
            tokio::spawn(async move { hook.sweep().await });
        }
        let app = Router::new()
            .route("/seerr", post(notify))
            .route("/health", get(|| async { StatusCode::OK }))
            .with_state(hook);
        if let Ok(addr) = self.listener.local_addr() {
            info!("Listening for Seerr's webhook on {addr}");
        }
        tokio::spawn(async move {
            if let Err(e) = axum::serve(self.listener, app).await {
                error!("Webhook listener stopped: {e}");
            }
        });
    }
}

/// The posts by `me` in the last [`HISTORY`] messages of `channel`, by request
async fn posts_in(discord: &Http, channel: ChannelId, me: UserId) -> Result<Vec<(u32, MessageId)>> {
    let mut found = Vec::new();
    let mut before = None;
    for _ in 0..HISTORY.div_ceil(100) {
        let mut page = GetMessages::new().limit(100);
        if let Some(id) = before {
            page = page.before(id);
        }
        let messages = channel.messages(discord, page).await?;
        before = messages.last().map(|m| m.id);
        found.extend(
            messages
                .iter()
                .filter(|m| m.author.id == me)
                .filter_map(|m| Some((post_request(m)?, m.id))),
        );
        if messages.len() < 100 {
            break;
        }
    }
    Ok(found)
}

/// Handles a notification from Seerr, then forwards it
async fn notify(State(hook): State<Arc<Hook>>, headers: HeaderMap, body: Bytes) -> Response {
    match serde_json::from_slice::<Notification>(&body) {
        Ok(Notification {
            notification_type,
            request: Some(request),
        }) => match request.request_id.parse() {
            Ok(id) => {
                let hook = hook.clone();
                tokio::spawn(async move { hook.handle(&notification_type, id).await });
            }
            Err(_) => warn!("Seerr webhook has request id {:?}", request.request_id),
        },
        Ok(Notification {
            notification_type, ..
        }) => info!("Seerr sent {notification_type}"),
        Err(e) => warn!("Reading Seerr's webhook: {e}"),
    }
    hook.forward(&headers, body).await
}

impl Hook {
    /// Acts on a notification of type `kind` about request `id`
    async fn handle(&self, kind: &str, id: u32) {
        info!("Seerr sent {kind} for request {id}");
        match kind {
            "MEDIA_PENDING" | "MEDIA_AUTO_APPROVED" => self.announce(Event::Requested, id).await,
            "MEDIA_AVAILABLE" => {
                tokio::join!(self.announce(Event::Available, id), self.refresh(id));
            }
            "MEDIA_APPROVED" | "MEDIA_DECLINED" | "MEDIA_FAILED" => {
                self.refresh(id).await;
            }
            _ => {}
        }
    }

    /// Rebuilds request `id`'s posts in the requests channels. Returns whether it may still
    /// change, which it's taken to if any post couldn't be rebuilt.
    async fn refresh(&self, id: u32) -> bool {
        let posts = self.posts.lock().unwrap().get(&id).cloned();
        let mut open = false;
        for (channel, message) in posts.into_iter().flatten() {
            match refresh_post(&self.data, &self.discord, channel, message, id).await {
                Ok(still) => open |= still,
                Err(e) => {
                    warn!("Refreshing request {id}'s post in channel {channel}: {e:#}");
                    open = true;
                }
            }
        }
        open
    }

    /// Finds the posts in the requests channels, then rebuilds open requests' posts every
    /// [`SWEEP_EVERY`], forgetting the rest
    async fn sweep(&self) {
        self.find_posts().await;
        let mut every = tokio::time::interval(SWEEP_EVERY);
        every.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            let ids: Vec<u32> = self.posts.lock().unwrap().keys().copied().collect();
            for id in ids {
                if !self.refresh(id).await {
                    self.posts.lock().unwrap().remove(&id);
                }
            }
        }
    }

    /// Adds the bot's posts in the last [`HISTORY`] messages of each requests channel
    async fn find_posts(&self) {
        let me = match self.discord.get_current_user().await {
            Ok(user) => user.id,
            Err(e) => {
                warn!("Looking up the bot's own user: {e}");
                return;
            }
        };
        for &channel in &self.targets.requests {
            let found = match posts_in(&self.discord, channel, me).await {
                Ok(found) => found,
                Err(e) => {
                    warn!("Finding request posts in channel {channel}: {e:#}");
                    continue;
                }
            };
            info!("Found {} request posts in channel {channel}", found.len());
            let mut posts = self.posts.lock().unwrap();
            for (id, message) in found {
                let list = posts.entry(id).or_default();
                if !list.contains(&(channel, message)) {
                    list.push((channel, message));
                }
            }
            drop(posts);
        }
    }

    /// Posts `event` for request `id`, if it hasn't been already
    async fn announce(&self, event: Event, id: u32) {
        let key = (event, id);
        if self.targets.channels(event).is_empty() || !self.posted.lock().unwrap().insert(key) {
            return;
        }
        let request = match self.data.seerr.request_by_id(&self.data.http, id).await {
            Ok(Some(request)) => request,
            Ok(None) => return,
            Err(e) => {
                warn!("Fetching request {id}: {e:#}");
                self.posted.lock().unwrap().remove(&key);
                return;
            }
        };
        match announce::post(&self.discord, &self.data, &self.targets, event, &request).await {
            Ok(posts) if event == Event::Requested => {
                self.posts
                    .lock()
                    .unwrap()
                    .entry(id)
                    .or_default()
                    .extend(posts);
            }
            Ok(_) => {}
            Err(e) => {
                warn!("Posting request {id} ({event:?}): {e:#}");
                self.posted.lock().unwrap().remove(&key);
            }
        }
    }

    /// Passes the notification on to each of [`Self::forward`] at once. Returns the first
    /// failed response, else the first one, or 204 if there's nowhere to forward to.
    async fn forward(&self, headers: &HeaderMap, body: Bytes) -> Response {
        let mut responses = join_all(
            self.forward
                .iter()
                .map(|url| self.forward_to(url, headers, body.clone())),
        )
        .await;
        let Some(i) = responses
            .iter()
            .position(|r| !r.status().is_success())
            .or_else(|| (!responses.is_empty()).then_some(0))
        else {
            return StatusCode::NO_CONTENT.into_response();
        };
        responses.swap_remove(i)
    }

    /// Passes the notification on to `url` and returns its response, or 502 if it couldn't be
    /// reached
    async fn forward_to(&self, url: &str, headers: &HeaderMap, body: Bytes) -> Response {
        // The query may hold a secret
        let shown = url.split_once('?').map_or(url, |(path, _)| path);
        let mut request = self.data.http.post(url).body(body);
        for name in [header::CONTENT_TYPE, header::AUTHORIZATION] {
            if let Some(value) = headers.get(&name) {
                request = request.header(name, value);
            }
        }
        let response = match request.send().await {
            Ok(response) => response,
            Err(e) => {
                warn!("Forwarding Seerr's webhook to {shown}: {}", e.without_url());
                return StatusCode::BAD_GATEWAY.into_response();
            }
        };
        let status = response.status();
        let content_type = response.headers().get(header::CONTENT_TYPE).cloned();
        let body = match response.bytes().await {
            Ok(body) => body,
            Err(e) => {
                warn!("Reading {shown}'s webhook response: {}", e.without_url());
                return StatusCode::BAD_GATEWAY.into_response();
            }
        };
        if !status.is_success() {
            warn!("Forwarded webhook to {shown} got {status}");
        }
        let mut response = (status, body).into_response();
        if let Some(content_type) = content_type {
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, content_type);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{ersatztv::GuideCache, jellyfin::Jellyfin, seerr::Seerr};

    /// The Authorization header and body the upstream got
    type Seen = Arc<Mutex<Option<(String, Bytes)>>>;

    #[test]
    fn moonfin_payload() {
        let body = r#"{
            "notification_type": "MEDIA_AVAILABLE",
            "subject": "Kwaidan (1965)",
            "media": {"media_type": "movie", "tmdbId": "30959"},
            "request": {"request_id": "234", "requestedBy_username": "ted"},
            "issue": null,
            "comment": null,
            "extra": []
        }"#;
        let n: Notification = serde_json::from_str(body).unwrap();
        assert_eq!(n.notification_type, "MEDIA_AVAILABLE");
        assert_eq!(n.request.unwrap().request_id, "234");
        let n: Notification =
            serde_json::from_str(r#"{"notification_type": "TEST_NOTIFICATION", "request": null}"#)
                .unwrap();
        assert!(n.request.is_none());
    }

    #[tokio::test]
    async fn forwards() {
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let upstream_addr = upstream.local_addr().unwrap();
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route("/ok", post(|| async { StatusCode::OK }))
            .route(
                "/hook",
                post(
                    |State(seen): State<Seen>, headers: HeaderMap, body: Bytes| async move {
                        let auth = headers[header::AUTHORIZATION].to_str().unwrap().to_owned();
                        *seen.lock().unwrap() = Some((auth, body));
                        (StatusCode::UNAUTHORIZED, "nope")
                    },
                ),
            )
            .with_state(seen.clone());
        tokio::spawn(async move { axum::serve(upstream, app).await });

        let listener = Listener::bind(
            0,
            vec![
                format!("http://{upstream_addr}/ok"),
                format!("http://{upstream_addr}/hook"),
            ],
            Targets::default(),
        )
        .await
        .unwrap();
        let addr = listener.listener.local_addr().unwrap();
        let http = reqwest::Client::new();
        let unused = || "http://localhost:1".to_owned();
        let data = Data {
            guide: GuideCache::new(http.clone(), unused(), Duration::ZERO),
            http,
            jellyfin: Jellyfin {
                url: unused(),
                api_key: String::new(),
                moonfin: false,
            },
            seerr: Seerr {
                url: unused(),
                api_key: String::new(),
                public_url: None,
            },
            radarr: None,
            sonarr: None,
            radarr_4k: None,
            sonarr_4k: None,
            admin_role: None,
        };
        listener.spawn(Arc::new(Http::new("")), Arc::new(data));

        let body = r#"{"notification_type": "MEDIA_PENDING", "request": {"request_id": "1"}}"#;
        let response = reqwest::Client::new()
            .post(format!("http://{addr}/seerr"))
            .header(header::AUTHORIZATION, "secret")
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(response.text().await.unwrap(), "nope");
        let (auth, forwarded) = seen.lock().unwrap().take().unwrap();
        assert_eq!(auth, "secret");
        assert_eq!(forwarded, body.as_bytes());

        let health = reqwest::get(format!("http://{addr}/health")).await.unwrap();
        assert_eq!(health.status(), StatusCode::OK);
    }
}
