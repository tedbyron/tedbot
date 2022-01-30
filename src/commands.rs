//! Bot commands.

use serenity::client::Context;
use serenity::model::id::GuildId;
use serenity::model::interactions::application_command::ApplicationCommandOptionType;

use crate::handler::BotName;
use crate::util::TraceResult;

/// Register a command with a guild.
// #[tracing::instrument(skip_all)]
pub async fn register_guild(ctx: &Context, guild_id: GuildId) {
    let name = {
        let lock = ctx.data.read().await;
        lock.get::<BotName>()
            .expect("Expected BotName in TypeMap")
            .clone()
    };

    guild_id
        .set_application_commands(&ctx.http, |commands| {
            commands
                .create_application_command(|cmd| {
                    cmd.name("order-up")
                        .description("There's a fresh galley boy waiting for you")
                })
                .create_application_command(|cmd| {
                    cmd.name("thank").description(format!("Thank {name}"))
                })
                .create_application_command(|cmd| {
                    cmd.name("wordle")
                        .description("Show wordle stats")
                        .create_option(|opt| {
                            opt.name("user")
                                .description("Select a specific user")
                                .kind(ApplicationCommandOptionType::User)
                        })
                        .create_option(|opt| {
                            opt.name("day")
                                .description("Select a day (e.g. 123 or 1/1/2021)")
                                .kind(ApplicationCommandOptionType::String)
                        })
                        .create_option(|opt| {
                            opt.name("hard")
                                .description("Select hard mode scores")
                                .kind(ApplicationCommandOptionType::Boolean)
                        })
                })
                .create_application_command(|cmd| {
                    cmd.name("wordle-load")
                        .description("Load wordle scores from the current channel")
                        .create_option(|opt| {
                            opt.name("channel")
                                .description("Specify a different channel to load scores from")
                                .kind(ApplicationCommandOptionType::Channel)
                        })
                })
        })
        .await
        .or_trace();
}
