//! Slash commands. Each one is registered in `main.rs` (`FrameworkOptions.commands`).
//!
//! A command's `///` doc comment and its parameters' `#[description]`s are what Discord shows in
//! the command picker, so keep them short and user-facing.

pub mod request;
pub mod requests;
pub mod restart;
pub mod streams;
pub mod tv;

use anyhow::{Context as _, Result};
use poise::serenity_prelude::{Builder as _, EditInteractionResponse, UserId};

use crate::{Context, Data, seerr::Requester};

/// The invoker's Seerr user, as found by `command_check` in `main.rs`, which only lets linked
/// users through
async fn seerr_user(ctx: Context<'_>) -> Result<Requester> {
    ctx.invocation_data::<Requester>()
        .await
        .map(|user| *user)
        .context("Invoker's Seerr user missing")
}

/// Whether the invoker is one of the configured admins, in any server or DM. Discord's
/// Administrator permission doesn't count, since the server's admins aren't necessarily the media
/// server's.
fn is_admin(ctx: Context<'_>) -> bool {
    is_admin_user(ctx.data(), ctx.author().id)
}

/// Whether `user` is one of the configured admins
fn is_admin_user(data: &Data, user: UserId) -> bool {
    data.admins.contains(&user)
}

/// Backslash-escapes Discord markdown characters so titles show literally
pub fn escape(s: &str) -> String {
    s.chars()
        .fold(String::with_capacity(s.len()), |mut out, c| {
            if matches!(c, '\\' | '*' | '_' | '~' | '`' | '|') {
                out.push('\\');
            }
            out.push(c);
            out
        })
}

/// Removes the buttons from a command's ephemeral reply once its collector times out, keeping the
/// rest. Only an unexpired interaction token can edit an ephemeral reply, so this uses the newest
/// press's, or the command's own if nothing was pressed. Not poise's `ReplyHandle::edit`, which
/// always sends `embeds` and so would clear the embed, leaving a message Discord refuses as empty.
async fn remove_buttons(ctx: Context<'_>, last: Option<String>) -> Result<()> {
    let token = match (last, ctx) {
        (Some(token), _) => token,
        (None, Context::Application(app)) => app.interaction.token.clone(),
        // Prefix commands aren't registered
        (None, Context::Prefix(_)) => return Ok(()),
    };
    EditInteractionResponse::new()
        .components(vec![])
        .execute(ctx, &token)
        .await?;
    Ok(())
}
