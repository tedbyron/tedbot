//! Slash commands, all listed in [`crate::commands()`]. Each checks the invoker linked their
//! Discord account in Seerr first ([`linked`]).
//!
//! A command's `///` doc comment and its parameters' `#[description]`s are what Discord shows in
//! the command picker, so keep them short and user-facing.

pub mod refresh_users;
pub mod request;
pub mod requests;
pub mod restart;
pub mod streams;
pub mod tv;

use anyhow::{Context as _, Result};
use poise::{
    CreateReply,
    serenity_prelude::{Builder as _, EditInteractionResponse, UserId},
};

use crate::{BotData, BotError, Context, MediaServer, seerr::Requester};

/// Every command's check: ignores other bots, and users who haven't linked their Discord account
/// in Seerr, who are told how to. Commands get the invoker's Seerr user from [`seerr_user`].
async fn linked<U: BotData, E: BotError>(ctx: Context<'_, U, E>) -> Result<bool, E> {
    if ctx.author().bot {
        return Ok(false);
    }
    let data = ctx.data().server();
    let Some(user) = data.seerr.user_for(&data.http, ctx.author().id).await? else {
        let autocomplete = matches!(
            ctx,
            poise::Context::Application(app)
                if app.interaction_type == poise::CommandInteractionType::Autocomplete
        );
        if !autocomplete {
            let name = ctx.cache().current_user().name.clone();
            let mut content = format!(
                "To use {name}, add your Discord user ID to your Seerr notification settings"
            );
            if let Some(url) = &data.seerr.public_url {
                content = format!(
                    "{content}: {}/profile/settings/notifications/discord",
                    url.trim_end_matches('/')
                );
            }
            ctx.send(CreateReply::default().ephemeral(true).content(content))
                .await
                .map_err(anyhow::Error::from)?;
        }
        return Ok(false);
    };
    ctx.set_invocation_data(user).await;
    Ok(true)
}

/// The invoker's Seerr user, as found by [`linked`], which only lets linked users through. Missing
/// if poise skipped the check, which `skip_checks_for_owners` does.
async fn seerr_user<U: BotData, E: BotError>(ctx: Context<'_, U, E>) -> Result<Requester> {
    ctx.invocation_data::<Requester>()
        .await
        .map(|user| *user)
        .context("Invoker's Seerr user missing; is skip_checks_for_owners set?")
}

/// Whether the invoker is one of the configured admins, in any server or DM. Discord's
/// Administrator permission doesn't count, since the server's admins aren't necessarily the media
/// server's.
fn is_admin<U: BotData, E: BotError>(ctx: Context<'_, U, E>) -> bool {
    is_admin_user(ctx.data().server(), ctx.author().id)
}

/// Whether `user` is one of the configured admins
pub fn is_admin_user(data: &MediaServer, user: UserId) -> bool {
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
async fn remove_buttons<U: BotData, E: BotError>(
    ctx: Context<'_, U, E>,
    last: Option<String>,
) -> Result<()> {
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
