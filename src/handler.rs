//! Event handlers.

use std::collections::HashSet;
use std::env;
use std::str::FromStr;
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use serenity::async_trait;
use serenity::client::{Context, EventHandler};
use serenity::futures::StreamExt;
use serenity::model::channel::{Message, ReactionType};
use serenity::model::gateway::{Activity, Ready};
use serenity::model::guild;
#[allow(deprecated)]
use serenity::model::guild::GuildStatus;
use serenity::model::interactions::application_command::{
    ApplicationCommandInteraction, ApplicationCommandInteractionDataOption,
    ApplicationCommandInteractionDataOptionValue,
};
use serenity::model::interactions::{Interaction, InteractionResponseType};
use serenity::prelude::*;

use crate::codec;
use crate::util::TraceResult;
use crate::wordle::{self, TimestampedScore};

lazy_static::lazy_static! {
    static ref WORDLE_1: DateTime<Utc> = Utc.ymd(2021, 6, 20).and_hms(0, 0, 0);
}

pub struct Handler {
    pub db: sled::Db,
}

pub struct BotName;
impl TypeMapKey for BotName {
    type Value = Arc<str>;
}

impl Handler {
    #[tracing::instrument(skip_all)]
    async fn wordle_load(&self, ctx: &Context, cmd: ApplicationCommandInteraction) {
        let channel_id = match cmd.data.options.get(0) {
            Some(ApplicationCommandInteractionDataOption {
                resolved: Some(ApplicationCommandInteractionDataOptionValue::Channel(partial)),
                ..
            }) => partial.id,
            _ => cmd.channel_id,
        };

        let mut unique_users: HashSet<u64> = HashSet::new();
        let mut score_count = 0;

        res_str(
            ctx,
            &cmd,
            format!("Loading wordle scores from <#{}>...", channel_id),
        )
        .await;

        let mut messages = channel_id.messages_iter(&ctx).boxed();
        while let Some(Ok(msg)) = messages.next().await {
            if msg.timestamp < *WORDLE_1 {
                break;
            }

            if let Ok((_, score)) = wordle::parse(&msg.content) {
                let author_id = codec::encode(msg.author.id.0).unwrap();
                let day = codec::encode(score.day).unwrap();
                let timestamp = msg.timestamp.timestamp();
                let score = codec::encode(TimestampedScore { timestamp, score }).unwrap();
                let tree = self.db.open_tree(author_id).unwrap();

                match tree.get(&day).unwrap() {
                    None => {
                        tree.insert(day, score).unwrap();
                        unique_users.insert(msg.author.id.0);
                        score_count += 1;
                    }
                    Some(prev_slice) => {
                        let (prev, _) = codec::decode::<TimestampedScore>(&prev_slice).unwrap();
                        if timestamp < prev.timestamp {
                            tree.insert(day, score).unwrap();
                            unique_users.insert(msg.author.id.0);
                            score_count += 1;
                        }
                    }
                }
            }
        }

        cmd.channel_id
            .say(
                &ctx.http,
                if score_count == 0 {
                    format!("No scores to add or update in <#{}>", channel_id)
                } else {
                    format!(
                        "Loaded {} scores from {} users in <#{}>",
                        score_count,
                        unique_users.len(),
                        channel_id
                    )
                },
            )
            .await
            .unwrap();
    }
}

#[async_trait]
impl EventHandler for Handler {
    #[tracing::instrument(skip_all)]
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::ApplicationCommand(cmd) = interaction {
            match cmd.data.name.as_str() {
                "order-up" => res_str(&ctx, &cmd, "<:galleyboy:915674675684712509>").await,
                "thank" => res_str(&ctx, &cmd, "you're welcome").await,
                "wordle-load" => self.wordle_load(&ctx, cmd).await,
                // "wordle" => wordle(&ctx, cmd).await,
                _ => res_str(&ctx, &cmd, "unimplemented").await,
            }
        }
    }

    #[tracing::instrument(skip_all)]
    async fn message(&self, ctx: Context, msg: Message) {
        if let Ok((_, _score)) = wordle::parse(&msg.content) {
            msg.react(
                &ctx.http,
                ReactionType::from_str("<:galleyboy:915674675684712509>").unwrap(),
            )
            .await
            .trace_err();
        }

        if msg.author.bot {
            return;
        }
    }

    #[allow(deprecated)]
    #[tracing::instrument(skip_all)]
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::debug!(guilds = ?ready.guilds);
        tracing::info!("Logged in as {}", ready.user.tag());

        ctx.dnd().await;

        // Insert bot name into global state.
        {
            let mut data = ctx.data.write().await;
            data.insert::<BotName>(ready.user.name.clone().into());
        }

        invite_url(&ctx, &ready).await;

        // Attach slash commands to guilds.
        // FIX: GuildStatus will be deprecated in serenity 12.
        let guild_ids = ready.guilds.iter().map(|status| match status {
            GuildStatus::OnlinePartialGuild(guild::PartialGuild { id, .. })
            | GuildStatus::OnlineGuild(guild::Guild { id, .. })
            | GuildStatus::Offline(guild::GuildUnavailable { id, .. }) => id,
            _ => unreachable!(),
        });
        for &guild_id in guild_ids {
            crate::commands::register_guild(&ctx, guild_id).await;
        }

        set_activity(&ctx).await;
        ctx.online().await;
    }
}

/// Generate an invite URL to add the bot to servers.
#[tracing::instrument(skip_all)]
async fn invite_url(ctx: &Context, ready: &Ready) {
    if let Ok(url) = ready
        .user
        .invite_url_with_oauth2_scopes(&ctx.http, *crate::PERMISSIONS, crate::SCOPES)
        .await
    {
        tracing::info!("{}", url);
    } else {
        tracing::warn!("Could not create a bot invite URL");
    }
}

/// Set the bot activity based on environment variables.
#[tracing::instrument(skip_all)]
async fn set_activity(ctx: &Context) {
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
}

#[tracing::instrument(skip_all)]
async fn res_str<T>(ctx: &Context, cmd: &ApplicationCommandInteraction, content: T)
where
    T: ToString + Send,
{
    cmd.create_interaction_response(&ctx.http, |res| {
        res.kind(InteractionResponseType::ChannelMessageWithSource)
            .interaction_response_data(|msg| msg.content(content))
    })
    .await
    .trace_err();
}
