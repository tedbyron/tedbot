use anyhow::Result;
use poise::command;

use crate::Context;

pub mod live;

/// Ping the bot
#[command(slash_command)]
pub async fn ping(ctx: Context<'_>) -> Result<()> {
    ctx.say("Pong!").await?;
    Ok(())
}
