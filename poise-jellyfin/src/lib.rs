//! A Jellyfin media server for [poise] Discord bots: slash commands for requesting media through
//! Seerr (`/request`, `/requests`), seeing who's streaming (`/streams`), ErsatzTV's live TV
//! guide (`/tv`), and admin tools (`/restart`, `/refresh-users`), plus Seerr's webhook, which
//! posts new and available requests in Discord.
//!
//! The bot's data (poise's `U`) holds a [`MediaServer`] and implements `AsRef<MediaServer>`
//! (`MediaServer` itself already does), and its error (`E`) converts from [`Error`]:
//!
//! ```ignore
//! // Shut down for the service manager to start the bot again; `None` can't restart
//! let restart = RestartBot::new(|shards| async move { shards.shutdown_all().await });
//! let server = MediaServer::new(&cfg.server, admins, http, Some(restart))?;
//! let webhook = poise_jellyfin::Listener::bind(&cfg.server).await?;
//! let options = FrameworkOptions {
//!     commands: poise_jellyfin::commands(),
//!     // Request posts' buttons outlive any command
//!     event_handler: |ctx, event| Box::pin(poise_jellyfin::handle_event(ctx, event)),
//!     ..FrameworkOptions::default()
//! };
//! // Once Discord is connected, in `setup`:
//! if let Some(webhook) = webhook {
//!     webhook.spawn(ctx.http.clone(), server.clone());
//! }
//! ```
//!
//! The commands are only for Discord users who put their Discord id in their Seerr notification
//! settings; anyone else is told how to link. That's a check, so `skip_checks_for_owners` isn't
//! supported: check it's off before starting the bot. [`handle_event`] also returns
//! [`Error::SkipChecksForOwners`] on `Ready` if it's set, but poise only hands that to `on_error`,
//! so it doesn't stop the bot.

mod announce;
mod arr;
mod commands;
mod config;
mod error;
mod ersatztv;
mod jellyfin;
mod seerr;
mod webhook;

use std::{fmt, ops::Deref, sync::Arc, time::Duration};

use futures::future::BoxFuture;
use poise::serenity_prelude::{self as serenity, ShardManager, UserId};

pub use config::{
    Config, ErsatzTvConfig, JellyfinConfig, Radarr4kConfig, RadarrConfig, SeerrConfig,
    Sonarr4kConfig, SonarrConfig, WebhookConfig,
};
pub use error::{Error, Result};
pub use webhook::Listener;

/// The media server's services and what's been looked up from them. Clones share it, so give one
/// to the webhook and keep one in the bot's data.
#[derive(Clone, Debug)]
pub struct MediaServer(Arc<Shared>);

/// What [`MediaServer`]'s clones share
#[derive(Debug)]
pub struct Shared {
    /// One HTTP client for all upstream services, so connections are pooled
    http: reqwest::Client,
    /// ErsatzTV's XMLTV guide, refetched at most once a minute
    guide: ersatztv::GuideCache,
    /// Jellyfin connection info
    jellyfin: jellyfin::Jellyfin,
    /// Whether Jellyfin has the Moonfin plugin
    moonfin: bool,
    /// Restarts Jellyfin for `/restart`, if configured; else Jellyfin restarts itself
    jellyfin_restart: Option<Vec<String>>,
    /// Restarts the bot for `/restart`, if it can be
    restart_bot: Option<RestartBot>,
    /// Seerr connection info
    seerr: seerr::Seerr,
    /// Radarr connection info, if configured
    radarr: Option<arr::Arr>,
    /// Sonarr connection info, if configured
    sonarr: Option<arr::Arr>,
    /// 4K Radarr connection info, if configured
    radarr_4k: Option<arr::Arr>,
    /// 4K Sonarr connection info, if configured
    sonarr_4k: Option<arr::Arr>,
    /// Users who count as admins
    admins: Vec<UserId>,
    /// Seerr users' Discord accounts in each server, as looked up lately
    members: commands::requests::Members,
    /// The default servers' quality profiles, as fetched lately
    profiles: commands::requests::ProfileCache,
}

impl MediaServer {
    /// Connects to nothing yet; services are asked as commands need them. `admins` can restart
    /// services, approve and cancel anyone's requests, and see what's streaming, and are pinged
    /// for requests waiting for approval. Give `http` a timeout: requests wait as long as it
    /// allows. `/restart` only offers to restart the bot with `restart_bot`. Fails if an API key
    /// is empty or the restart command is.
    pub fn new(
        cfg: &Config,
        admins: Vec<UserId>,
        http: reqwest::Client,
        restart_bot: Option<RestartBot>,
    ) -> Result<Self> {
        if cfg.jellyfin.api_key.is_empty() {
            return Err(Error::Config("jellyfin.api_key is empty"));
        }
        if cfg.seerr.api_key.is_empty() {
            return Err(Error::Config("seerr.api_key is empty"));
        }
        if cfg
            .jellyfin
            .restart_command
            .as_ref()
            .is_some_and(Vec::is_empty)
        {
            return Err(Error::Config("jellyfin.restart_command is empty"));
        }
        let arr = |url: &String, api_key: &String| arr::Arr {
            url: url.clone(),
            api_key: api_key.clone(),
        };
        Ok(Self(Arc::new(Shared {
            guide: ersatztv::GuideCache::new(
                http.clone(),
                cfg.ersatztv.url.clone(),
                Duration::from_secs(60),
            ),
            jellyfin: jellyfin::Jellyfin {
                url: cfg.jellyfin.url.clone(),
                api_key: cfg.jellyfin.api_key.clone(),
            },
            moonfin: cfg.jellyfin.moonfin,
            jellyfin_restart: cfg.jellyfin.restart_command.clone(),
            restart_bot,
            seerr: seerr::Seerr {
                url: cfg.seerr.url.clone(),
                api_key: cfg.seerr.api_key.clone(),
                public_url: cfg.seerr.public_url.clone(),
                links: seerr::Links::default(),
                titles: seerr::Titles::default(),
            },
            radarr: cfg.radarr.as_ref().map(|c| arr(&c.url, &c.api_key)),
            sonarr: cfg.sonarr.as_ref().map(|c| arr(&c.url, &c.api_key)),
            radarr_4k: cfg.radarr_4k.as_ref().map(|c| arr(&c.url, &c.api_key)),
            sonarr_4k: cfg.sonarr_4k.as_ref().map(|c| arr(&c.url, &c.api_key)),
            http,
            admins,
            members: commands::requests::Members::default(),
            profiles: commands::requests::ProfileCache::default(),
        })))
    }
}

/// How the bot restarts itself, for `/restart`. It's handed poise's shard manager, and should
/// get the bot running again, e.g. by shutting the shards down for a service manager to restart
/// the process.
pub struct RestartBot(Box<dyn Fn(Arc<ShardManager>) -> BoxFuture<'static, ()> + Send + Sync>);

impl RestartBot {
    /// Restarts with `restart`
    pub fn new<F, Fut>(restart: F) -> Self
    where
        F: Fn(Arc<ShardManager>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        Self(Box::new(move |shards| Box::pin(restart(shards))))
    }
}

impl fmt::Debug for RestartBot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RestartBot")
    }
}

impl Deref for MediaServer {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.0
    }
}

impl AsRef<Self> for MediaServer {
    fn as_ref(&self) -> &Self {
        self
    }
}

/// The bot's data (poise's `U`): anything that holds a [`MediaServer`]
pub trait Data: AsRef<MediaServer> + Send + Sync + 'static {
    /// The media server
    fn server(&self) -> &MediaServer {
        self.as_ref()
    }
}

impl<T: AsRef<MediaServer> + Send + Sync + 'static> Data for T {}

type Context<'a, U, E> = poise::Context<'a, U, E>;

/// Every command, for `FrameworkOptions::commands`
pub fn commands<U: Data, E: From<Error> + Send + Sync + 'static>() -> Vec<poise::Command<U, E>> {
    vec![
        commands::refresh_users::refresh_users(),
        commands::request::request(),
        commands::requests::requests(),
        commands::restart::restart(),
        commands::streams::streams(),
        commands::tv::tv(),
    ]
}

/// Handles presses on request posts, whose buttons outlive any command. Call it from poise's
/// `event_handler` with every event; it ignores the rest.
///
/// Returns [`Error::SkipChecksForOwners`] on `Ready` if `skip_checks_for_owners` is set, since
/// owners would skip the Seerr link check the commands rely on. Poise only passes that to
/// `on_error`, so also check before starting the bot.
pub async fn handle_event<U: Data, E: From<Error>>(
    framework: poise::FrameworkContext<'_, U, E>,
    event: &serenity::FullEvent,
) -> Result<(), E> {
    match event {
        serenity::FullEvent::Ready { .. } if framework.options.skip_checks_for_owners => {
            return Err(Error::SkipChecksForOwners.into());
        }
        serenity::FullEvent::InteractionCreate {
            interaction: serenity::Interaction::Component(press),
        } => {
            commands::requests::post_press(
                framework.serenity_context,
                framework.user_data.server(),
                press,
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}
