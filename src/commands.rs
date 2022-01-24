//! Bot commands.

use serenity::client::Context;
use serenity::model::id::GuildId;
use serenity::model::interactions::application_command::ApplicationCommandOptionType;

use crate::util::TraceResult;

/// Register a command with a guild.
pub async fn register_guild(ctx: &Context, guild_id: GuildId) {
    guild_id
        .set_application_commands(&ctx.http, |commands| {
            commands
                .create_application_command(|cmd| {
                    cmd.name("ping").description("Replies with \"pong\".")
                })
                .create_application_command(|cmd| {
                    cmd.name("order").description("Order a galley boy.")
                })
                .create_application_command(|cmd| {
                    cmd.name("wordle")
                        .description("Show wordle stats.")
                        .create_option(|opt| {
                            opt.name("user")
                                .description("Stats for a specific user.")
                                .kind(ApplicationCommandOptionType::User)
                        })
                })
                .create_application_command(|cmd| {
                    cmd.name("wordle-init")
                        .description("Initialize wordle score tracking. Loads previous messages containing wordle scores.")
                        .create_option(|opt| {
                                opt.name("channel")
                                    .description("The channel to initialize the leaderboard in.")
                                    .kind(ApplicationCommandOptionType::Channel)
                                    .required(true)
                            })
                    })
                .create_application_command(|cmd| {
                    cmd.name("wordle-uninit")
                        .description("Remove wordle score tracking from the server.")
                        .default_permission(false)
                    // TODO
                })
        })
        .await
        .trace_err();
}
