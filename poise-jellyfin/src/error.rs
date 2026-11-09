//! What can go wrong. Messages leave out their source, so print the chain to see it, e.g. with
//! anyhow's `{:#}`.

use std::{fmt, io};

use poise::serenity_prelude as serenity;

/// `Result` with [`Error`] by default
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a command, the webhook, or setting up the media server failed
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A config value is invalid
    #[error("{0}")]
    Config(&'static str),
    /// The webhook's port is in use, or can't be bound
    #[error("binding localhost:{port} for the webhook")]
    Bind {
        /// The webhook's port
        port: u16,
        /// Why it couldn't be bound
        source: io::Error,
    },
    /// Poise's `skip_checks_for_owners` is set, which the commands don't support
    #[error("skip_checks_for_owners isn't supported: owners would skip the Seerr link check")]
    SkipChecksForOwners,
    /// Discord's API failed. Boxed, since it's large.
    #[error("Discord's API failed")]
    Discord(#[source] Box<serenity::Error>),
    /// A request to Jellyfin, Seerr, Radarr, Sonarr or ErsatzTV failed
    #[error("request failed")]
    Http(#[from] reqwest::Error),
    /// A response wasn't the JSON expected
    #[error("parsing a response")]
    Json(#[from] serde_json::Error),
    /// ErsatzTV's guide isn't the XMLTV expected
    #[error("parsing ErsatzTV's guide")]
    Guide(#[from] quick_xml::DeError),
    /// A service answered with something the bot doesn't handle
    #[error("{service}: {message}")]
    Unexpected {
        /// e.g. `Seerr`
        service: &'static str,
        /// What it did
        message: String,
    },
    /// Radarr or Sonarr failed while taking a cancelled request out
    #[error("{service}")]
    Arr {
        /// `Radarr`, `Sonarr`, `4K Radarr` or `4K Sonarr`
        service: &'static str,
        /// How it failed
        source: Box<Self>,
    },
    /// The Radarr or Sonarr a cancelled request went to isn't configured
    #[error("{0} isn't configured")]
    NotConfigured(&'static str),
    /// Jellyfin's restart command couldn't be run
    #[error("running {command}")]
    Spawn {
        /// The command, its arguments joined by spaces
        command: String,
        /// Why it couldn't be run
        source: io::Error,
    },
    /// Jellyfin's restart command failed
    #[error("{command}: {stderr}")]
    Command {
        /// The command, its arguments joined by spaces
        command: String,
        /// What it printed to stderr
        stderr: String,
    },
    /// A request couldn't be posted in any of its channels
    #[error("couldn't post in any channel")]
    NotPosted,
    /// A fetch shared with other callers failed
    #[error(transparent)]
    Shared(#[from] std::sync::Arc<Self>),
    /// A bug: something the bot expected to have wasn't there
    #[error("{0} missing")]
    Missing(&'static str),
}

impl From<serenity::Error> for Error {
    fn from(e: serenity::Error) -> Self {
        Self::Discord(Box::new(e))
    }
}

impl Error {
    /// [`Error::Unexpected`] from Seerr
    pub(crate) fn seerr(message: impl Into<String>) -> Self {
        Self::Unexpected {
            service: "Seerr",
            message: message.into(),
        }
    }
}

/// Shows `e` and its sources, for logging: "request failed: error sending request: ..."
pub fn chain(e: &(impl std::error::Error + ?Sized)) -> impl fmt::Display + '_ {
    Chain(e)
}

struct Chain<'a, E: ?Sized>(&'a E);

impl<E: std::error::Error + ?Sized> fmt::Display for Chain<'_, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(e) = source {
            write!(f, ": {e}")?;
            source = e.source();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains() {
        let e = Error::Bind {
            port: 8080,
            source: io::Error::other("in use"),
        };
        assert_eq!(
            chain(&e).to_string(),
            "binding localhost:8080 for the webhook: in use"
        );
    }
}
