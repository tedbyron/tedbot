//! Slash commands. Each one is registered in `main.rs` (`FrameworkOptions.commands`).
//!
//! A command's `///` doc comment and its parameters' `#[description]`s are what Discord shows in
//! the command picker, so keep them short and user-facing.

pub mod streams;
pub mod tv;

use crate::Context;

/// Whether the invoker is one of the configured admin users or has the configured admin role.
/// Discord's Administrator permission doesn't count, since the server's admins aren't necessarily
/// the media server's.
async fn is_admin(ctx: Context<'_>) -> bool {
    let data = ctx.data();
    if data.admin_users.contains(&ctx.author().id) {
        return true;
    }
    let Some(role) = data.admin_role else {
        return false;
    };
    ctx.author_member()
        .await
        .is_some_and(|member| member.roles.contains(&role))
}

/// Backslash-escapes Discord markdown characters so titles show literally
fn escape(s: &str) -> String {
    s.chars()
        .fold(String::with_capacity(s.len()), |mut out, c| {
            if matches!(c, '\\' | '*' | '_' | '~' | '`' | '|') {
                out.push('\\');
            }
            out.push(c);
            out
        })
}
