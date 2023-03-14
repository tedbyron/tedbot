use anyhow::Result;
use poise::command;

use crate::Context;

/// Ping the bot
#[command(slash_command)]
pub async fn ping(ctx: Context<'_>) -> Result<()> {
    ctx.say("Pong!").await?;
    Ok(())
}

// #[command(slash_command)]
// pub async fn name(ctx: Context<'_>, #[description = ""] param: Type) -> Result<()> {
//     Ok(())
// }
