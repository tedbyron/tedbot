//! `/refresh-users`: looks up again which Discord account each Seerr user linked, and whether
//! they're in each server, so someone who just linked shows as a mention without restarting
//! the bot. Admins only; like `/restart`, Discord hides it from members without Administrator
//! unless the server's Integrations settings give admins access.

use poise::command;
use tracing::{info, warn};

use super::is_admin;
use crate::{Context, Data, Error, Result, error::chain};

/// Find Discord accounts newly linked in Seerr
#[command(
    check = "super::linked",
    slash_command,
    rename = "refresh-users",
    ephemeral,
    default_member_permissions = "ADMINISTRATOR"
)]
pub(crate) async fn refresh_users<U: Data, E: From<Error>>(
    ctx: Context<'_, U, E>,
) -> Result<(), E> {
    refresh_users_run(ctx).await.map_err(E::from)
}

/// [`refresh_users()`], failing with [`Error`](crate::Error)
async fn refresh_users_run<U: Data, E: From<Error>>(ctx: Context<'_, U, E>) -> Result<()> {
    if !is_admin(ctx) {
        ctx.say("Only admins can refresh users").await?;
        return Ok(());
    }
    ctx.defer_ephemeral().await?;
    let data = ctx.data().server();
    data.members.clear();
    let reply = match data.seerr.refresh_links(&data.http).await {
        Ok(count) => {
            info!("{} refreshed users: {count} linked", ctx.author().name);
            match count {
                1 => "1 Seerr user has a Discord account linked".to_owned(),
                n => format!("{n} Seerr users have a Discord account linked"),
            }
        }
        Err(e) => {
            warn!("Refreshing Seerr users: {}", chain(&e));
            "Couldn't reach Seerr, try again in a bit \u{1fae4}".to_owned()
        }
    };
    ctx.say(reply).await?;
    Ok(())
}
