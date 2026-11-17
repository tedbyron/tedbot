//! `/help`: what each of the bot's commands is for, from their doc comments. Lists every command
//! the bot registered, not just this crate's, but only those the invoker can use there
//! ([`usable`]).

use poise::{
    Command, CreateReply, command,
    serenity_prelude::{CreateEmbed, CreateEmbedFooter},
};

use super::{defer, in_guild, is_admin};
use crate::{Context, Data, Error, Result};

/// What each command is for
#[command(check = "super::linked", slash_command)]
pub async fn help<U: Data, E: From<Error>>(ctx: Context<'_, U, E>) -> Result<(), E> {
    help_run(ctx).await.map_err(E::from)
}

/// [`help()`], failing with [`Error`](crate::Error)
async fn help_run<U: Data, E: From<Error>>(ctx: Context<'_, U, E>) -> Result<()> {
    defer(ctx).await?;
    let lines: Vec<String> = ctx
        .framework()
        .options()
        .commands
        .iter()
        .filter(|command| command.name != ctx.command().name && usable(ctx, command))
        .map(|command| {
            let mut usage = format!("/{}", command.name);
            for param in &command.parameters {
                let (open, close) = if param.required {
                    ('<', '>')
                } else {
                    ('[', ']')
                };
                usage = format!("{usage} {open}{}{close}", param.name);
            }
            let about = command.help_text.as_deref().map_or_else(
                || {
                    command
                        .description
                        .as_deref()
                        .unwrap_or_default()
                        .to_owned()
                },
                unwrap,
            );
            format!("`{usage}`: {about}")
        })
        .collect();
    let embed = CreateEmbed::new()
        .title("Commands")
        .description(lines.join("\n"))
        .footer(CreateEmbedFooter::new("<…> is required, […] is optional"));
    ctx.send(CreateReply::default().embed(embed)).await?;
    Ok(())
}

/// Whether the invoker can use `command` here, so `/help` only lists those. Admin-only commands
/// (with `default_member_permissions`) are for [`is_admin`]'s admins, which the commands check
/// themselves; Discord may also hide them from admins without the permission, unless the server's
/// Integrations settings give them access.
fn usable<U: Data, E: From<Error>>(ctx: Context<'_, U, E>, command: &Command<U, E>) -> bool {
    let in_server = in_guild(ctx);
    let owner = ctx.framework().options().owners.contains(&ctx.author().id);
    let permissions = match ctx {
        Context::Application(app) => app
            .interaction
            .member
            .as_ref()
            .and_then(|member| member.permissions),
        Context::Prefix(_) => None,
    };
    command.slash_action.is_some()
        && !command.hide_in_help
        && (command.default_member_permissions.is_empty() || is_admin(ctx))
        && (!command.owners_only || owner)
        && !(command.guild_only && !in_server)
        && !(command.dm_only && in_server)
        && (command.required_permissions.is_empty()
            || !in_server
            || permissions.is_some_and(|have| have.contains(command.required_permissions)))
}

/// Joins a doc comment's wrapped lines, keeping blank lines between paragraphs
fn unwrap(text: &str) -> String {
    text.split("\n\n")
        .map(|paragraph| {
            paragraph
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwrap_joins_lines() {
        assert_eq!(unwrap("one\ntwo\n\nthree\nfour"), "one two\n\nthree four");
    }
}
