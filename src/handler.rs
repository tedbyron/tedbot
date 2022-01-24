//! Event handlers.

use std::env;

use serenity::async_trait;
use serenity::client::{Context, EventHandler};
use serenity::model::channel::Message;
use serenity::model::gateway::{Activity, Ready};
use serenity::model::guild;
#[allow(deprecated)]
use serenity::model::guild::GuildStatus;
use serenity::model::interactions::application_command::ApplicationCommandInteraction;
use serenity::model::interactions::{Interaction, InteractionResponseType};
use serenity::model::oauth2::OAuth2Scope;
use serenity::model::Permissions;

use crate::util::TraceResult;
// use crate::wordle;

pub struct Handler {
    pub db: sled::Db,
}

#[async_trait]
impl EventHandler for Handler {
    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::ApplicationCommand(cmd) = interaction {
            match cmd.data.name.as_str() {
                "ping" => res_str(&ctx, cmd, "pong").await,
                "order" => res_str(&ctx, cmd, "<:galleyboy:915674675684712509>").await,
                _ => res_str(&ctx, cmd, "unimplemented").await,
            }
        }
    }

    async fn message(&self, _ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        // if let Ok((_, score)) = wordle::parse(&msg.content) {
        //     msg.channel_id
        //         .say(&ctx.http, format!("```rust\n{:#?}\n```", score))
        //         .await
        //         .trace_err();
        //     return;
        // }
    }

    #[allow(deprecated)]
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::debug!(guilds = ?ready.guilds);
        tracing::info!("Logged in as {}", ready.user.tag());

        ctx.dnd().await;

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

        // Global slash command.
        // ApplicationCommand::create_global_application_command(&ctx.http, |command| {
        //     command
        //         .name("tedbot_global")
        //         .description("Does stuff")
        // })
        // .await
        // .trace_err();

        set_activity(&ctx).await;
        ctx.online().await;
    }
}

/// Generate an invite URL to add the bot to servers.
async fn invite_url(ctx: &Context, ready: &Ready) {
    let permissions =
        Permissions::READ_MESSAGES | Permissions::READ_MESSAGE_HISTORY | Permissions::SEND_MESSAGES;
    let scopes = &[OAuth2Scope::Bot, OAuth2Scope::ApplicationsCommands];

    if let Ok(url) = ready
        .user
        .invite_url_with_oauth2_scopes(&ctx.http, permissions, scopes)
        .await
    {
        tracing::info!("{}", url);
    } else {
        tracing::warn!("Could not create a bot invite URL");
    }
}

/// Set the bot activity based on environment variables.
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

async fn res_str(ctx: &Context, cmd: ApplicationCommandInteraction, content: &str) {
    cmd.create_interaction_response(&ctx.http, |res| {
        res.kind(InteractionResponseType::ChannelMessageWithSource)
            .interaction_response_data(|msg| msg.content(content))
    })
    .await
    .trace_err();
}
