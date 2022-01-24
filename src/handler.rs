#![allow(clippy::unreadable_literal)]
//! Event handlers.

use std::env;

use serenity::async_trait;
use serenity::client::{Context, EventHandler};
use serenity::model::channel::Message;
use serenity::model::gateway::{Activity, Ready};
use serenity::model::guild::GuildStatus;
use serenity::model::oauth2::OAuth2Scope;
use serenity::model::Permissions;

use crate::{wordle, TraceErr};

pub struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        if msg.content == "ping" {
            msg.channel_id.say(&ctx.http, "pong").await.trace_err();
            return;
        }

        if msg.content == "order up" {
            msg.channel_id
                .say(&ctx.http, "<:galleyboy:915674675684712509>")
                .await
                .trace_err();
            return;
        }

        if let Ok((_, score)) = wordle::parse(&msg.content) {
            msg.channel_id
                .say(&ctx.http, format!("```rust\n{:#?}\n```", score))
                .await
                .trace_err();
            return;
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::debug!(guilds = ?ready.guilds);
        tracing::info!(
            "Logged in as {} into {} guilds",
            ready.user.tag(),
            ready
                .guilds
                .iter()
                .filter(|&g| !matches!(g, &GuildStatus::Offline(_)))
                .count()
        );

        // Bot user activity.
        let activity = match (
            env::var("TEDBOT_ACTIVITY_TYPE"),
            env::var("TEDBOT_ACTIVITY_NAME"),
        ) {
            (Ok(type_), Ok(name)) => {
                let type_str = type_.as_str();
                let activity_type = match type_str {
                    "competing" | "listening" | "playing" | "streaming" | "watching" => type_str,
                    _ => {
                        tracing::warn!("Invalid TEDBOT_ACTIVITY_TYPE env var");
                        ""
                    }
                };

                match activity_type {
                    "competing" => Some(Activity::competing(name)),
                    "listening" => Some(Activity::listening(name)),
                    "playing" => Some(Activity::playing(name)),
                    "streaming" => {
                        if let Ok(streaming) = env::var("TEDBOT_ACTIVITY_STREAMING") {
                            Some(Activity::streaming(name, streaming))
                        } else {
                            tracing::warn!("Missing TEDBOT_ACTIVITY_STREAMING env var");
                            None
                        }
                    }
                    "watching" => Some(Activity::watching(name)),
                    _ => None,
                }
            }
            _ => None,
        };

        if let Some(activity) = activity {
            ctx.set_activity(activity).await;
        }

        let permissions = Permissions::READ_MESSAGES
            | Permissions::READ_MESSAGE_HISTORY
            | Permissions::SEND_MESSAGES;
        let scopes = &[OAuth2Scope::Bot, OAuth2Scope::ApplicationsCommands];

        if let Ok(url) = ready
            .user
            .invite_url_with_oauth2_scopes(ctx.http, permissions, scopes)
            .await
        {
            tracing::info!("{}", url);
        } else {
            tracing::warn!("Could not create a bot invite URL");
        }
    }
}
