//! Event handlers.

use serenity::async_trait;
use serenity::client::{Context, EventHandler};
use serenity::model::channel::Message;
use serenity::model::prelude::Ready;

pub struct Handler;

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.content == "!ping" {
            if let Err(e) = msg.channel_id.say(&ctx.http, "pong!").await {
                tracing::error!("{:?}", e);
            }
        }
    }
    async fn ready(&self, _: Context, ready: Ready) {
        tracing::debug!(guilds = ?ready.guilds);
        tracing::info!("Bot logged in as {}", ready.user.tag());
    }
}
