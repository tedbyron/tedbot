//! Event handlers.

use std::env;

use serenity::async_trait;
use serenity::client::{Context, EventHandler};
use serenity::model::channel::Message;
use serenity::model::oauth2::OAuth2Scope;
use serenity::model::prelude::{Activity, Ready};
use serenity::model::Permissions;

pub struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.content == "order up" {
            if let Err(e) = msg
                .channel_id
                .say(&ctx.http, "<:galleyboy:915674675684712509>")
                .await
            {
                tracing::error!("{:?}", e);
            }
        }
    }
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::debug!(guilds = ?ready.guilds);
        tracing::info!("Bot logged in as {}", ready.user.tag());

        // Bot user activity.
        let activity = match (
            env::var("TEDBOT_ACTIVITY_TYPE"),
            env::var("TEDBOT_ACTIVITY_NAME"),
        ) {
            (Ok(type_), Ok(name)) => {
                let activity_type = if let type_ @ ("competing" | "listening" | "playing"
                | "streaming" | "watching") = type_.as_str()
                {
                    type_
                } else {
                    tracing::warn!("Invalid TEDBOT_ACTIVITY_TYPE env var");
                    ""
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
