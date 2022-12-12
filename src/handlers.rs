use std::env;

use poise::serenity_prelude::oauth::Scope;
use poise::serenity_prelude::{Activity, Context, Permissions, Ready};
use tracing::{debug, error, info, warn};

use crate::{Data, Framework, FrameworkError, Result};

#[tracing::instrument(skip_all)]
pub async fn command_check(ctx: crate::Context<'_>) -> Result<bool> {
    Ok(!ctx.author().bot)
}

#[tracing::instrument(skip_all)]
pub async fn setup(ctx: &Context, ready: &Ready, framework: &Framework) -> Result<Data> {
    debug!(guilds = ?ready.guilds);
    info!("Logged in as {}", ready.user.tag());

    invite_url(&ctx, &ready).await;
    set_activity(&ctx).await;

    Ok(Data {
        tag: ready.user.tag(),
    })
}

#[tracing::instrument(skip_all)]
async fn invite_url(ctx: &Context, ready: &Ready) {
    match ready
        .user
        .invite_url_with_oauth2_scopes(
            &ctx,
            Permissions::ADD_REACTIONS
                | Permissions::SEND_MESSAGES
                | Permissions::SEND_MESSAGES_IN_THREADS
                | Permissions::VIEW_CHANNEL,
            &[Scope::Bot, Scope::ApplicationsCommands],
        )
        .await
    {
        Ok(url) => info!("{}", url),
        _ => warn!("Could not create a bot invite URL"),
    }
}

#[tracing::instrument(skip_all)]
async fn set_activity(ctx: &Context) {
    let activity = match (
        env::var("TEDBOT_ACTIVITY_TYPE"),
        env::var("TEDBOT_ACTIVITY_NAME"),
    ) {
        (Ok(type_), Ok(name)) => match type_.as_str() {
            "competing" => Some(Activity::competing(name)),
            "listening" => Some(Activity::listening(name)),
            "playing" => Some(Activity::playing(name)),
            "streaming" => match env::var("TEDBOT_ACTIVITY_STREAMING") {
                Ok(streaming) => Some(Activity::streaming(name, streaming)),
                _ => {
                    warn!("Missing TEDBOT_ACTIVITY_STREAMING env var");
                    None
                }
            },
            "watching" => Some(Activity::watching(name)),
            _ => None,
        },
        _ => None,
    };

    if let Some(activity) = activity {
        ctx.set_activity(activity).await;
    }
}

#[tracing::instrument(skip_all)]
async fn err_handler(err: FrameworkError<'_>) {
    error!("{:?}", err);
}
