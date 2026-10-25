#![warn(clippy::all, clippy::nursery, rust_2018_idioms)]

use std::{env, fs, process, time::Duration};

use anyhow::{Context as _, Error, Result};
use poise::{
    CreateReply, FrameworkOptions,
    builtins::register_globally,
    serenity_prelude::{self as serenity, ActivityData, GatewayIntents, Permissions},
};
use tracing::{Level, error, info, warn};

mod commands;
mod config;
mod ersatztv;
mod jellyfin;

#[derive(Debug)]
pub struct Data {
    http: reqwest::Client,
    guide: ersatztv::GuideCache,
    jellyfin: Option<jellyfin::Jellyfin>,
}
type Context<'a> = poise::Context<'a, Data, Error>;
type Framework = poise::Framework<Data, Error>;
type FrameworkError<'a> = poise::FrameworkError<'a, Data, Error>;

#[tokio::main]
async fn main() {
    process::exit(match run().await {
        Ok(()) => 0,
        Err(e) => {
            error!("{e:#}");
            eprintln!("{e:#}");
            1
        }
    });
}

async fn run() -> Result<()> {
    let path = env::var("TEDBOT_CONFIG").unwrap_or_else(|_| "config.toml".into());
    let cfg = toml::from_str::<config::Config>(
        &fs::read_to_string(&path).with_context(|| format!("reading {path}"))?,
    )
    .with_context(|| format!("parsing {path}"))?;

    tracing_subscriber::fmt()
        .with_target(false)
        .with_max_level(
            cfg.log_level
                .as_deref()
                .and_then(|lvl| lvl.parse().ok())
                .unwrap_or(Level::INFO),
        )
        .init();

    let options = FrameworkOptions {
        commands: vec![
            commands::ping(),
            commands::channels::channel(),
            commands::channels::channels(),
        ],
        on_error: |e| Box::pin(on_error(e)),
        pre_command: |ctx| Box::pin(pre_command(ctx)),
        command_check: Some(|ctx| Box::pin(command_check(ctx))),
        ..FrameworkOptions::default()
    };

    let activity = cfg.discord.activity;
    let http = reqwest::Client::new();
    let data = Data {
        guide: ersatztv::GuideCache::new(http.clone(), cfg.ersatztv.url, Duration::from_secs(60)),
        http,
        jellyfin: cfg.jellyfin.map(|j| jellyfin::Jellyfin {
            url: j.url,
            api_key: j.api_key,
        }),
    };
    let framework = Framework::builder()
        .setup(move |ctx, ready, framework| Box::pin(setup(ctx, ready, framework, activity, data)))
        .options(options)
        .build();

    serenity::ClientBuilder::new(cfg.discord.token, GatewayIntents::non_privileged())
        .framework(framework)
        .await?
        .start_autosharded()
        .await?;

    Ok(())
}

async fn on_error(err: FrameworkError<'_>) {
    let reply = |content| CreateReply::default().ephemeral(true).content(content);
    if let poise::FrameworkError::Command { error, ctx, .. } = err {
        error!("{error:#}");
        drop(ctx.send(reply("Error executing command \u{1fae4}")).await);
    } else {
        error!("{err}");
        if let Some(ctx) = err.ctx() {
            drop(ctx.send(reply("An error occurred \u{1fae4}")).await);
        }
    }
}

#[tracing::instrument(skip_all)]
async fn pre_command(ctx: Context<'_>) {
    info!("{} used `{}`", ctx.author().name, ctx.invocation_string());
}

#[allow(clippy::unused_async)]
async fn command_check(ctx: Context<'_>) -> Result<bool> {
    Ok(!ctx.author().bot)
}

#[tracing::instrument(skip_all)]
async fn setup(
    ctx: &serenity::Context,
    ready: &serenity::Ready,
    framework: &Framework,
    cfg: Option<config::DiscordActivityConfig>,
    data: Data,
) -> Result<Data> {
    register_globally(ctx, &framework.options().commands).await?;
    clear_guild_commands(ctx, ready).await;

    info!(
        "{} available in {} servers",
        ready.user.tag(),
        ready.guilds.len()
    );
    info!("{}", invite_url(ready));

    if let Some(cfg) = &cfg {
        activity(ctx, cfg);
    }

    Ok(data)
}

/// Commands are registered globally; remove guild-scoped ones left over from older versions,
/// which Discord keeps showing until they're deleted.
async fn clear_guild_commands(ctx: &serenity::Context, ready: &serenity::Ready) {
    for guild in &ready.guilds {
        let stale = match guild.id.get_commands(ctx).await {
            Ok(commands) if commands.is_empty() => continue,
            Ok(commands) => commands,
            Err(e) => {
                warn!("Listing commands in guild {}: {e}", guild.id);
                continue;
            }
        };
        let names: Vec<&str> = stale.iter().map(|c| c.name.as_str()).collect();
        match guild.id.set_commands(ctx, Vec::new()).await {
            Ok(_) => info!(
                "Removed stale commands in guild {}: {}",
                guild.id,
                names.join(", ")
            ),
            Err(e) => warn!("Removing stale commands in guild {}: {e}", guild.id),
        }
    }
}

fn invite_url(ready: &serenity::Ready) -> String {
    let permissions = Permissions::ADD_REACTIONS
        | Permissions::READ_MESSAGE_HISTORY
        | Permissions::SEND_MESSAGES
        | Permissions::VIEW_CHANNEL;
    format!(
        "https://discord.com/oauth2/authorize?client_id={}&scope=bot+applications.commands&permissions={}",
        ready.application.id,
        permissions.bits()
    )
}

#[tracing::instrument(skip_all)]
fn activity(ctx: &serenity::Context, cfg: &config::DiscordActivityConfig) {
    let activity = match (cfg.r#type.as_deref(), cfg.name.as_deref()) {
        (Some("competing"), Some(name)) => Some(ActivityData::competing(name)),
        (Some("listening"), Some(name)) => Some(ActivityData::listening(name)),
        (Some("playing"), Some(name)) => Some(ActivityData::playing(name)),
        (Some("watching"), Some(name)) => Some(ActivityData::watching(name)),
        (Some("streaming"), Some(name)) => cfg.streaming_url.as_deref().map_or_else(
            || {
                warn!("Missing discord.activity.streaming_url");
                None
            },
            |url| {
                ActivityData::streaming(name, url)
                    .inspect_err(|e| warn!("Invalid discord.activity.streaming_url: {e}"))
                    .ok()
            },
        ),
        _ => None,
    };

    if let Some(activity) = activity {
        info!("{:?} {}", activity.kind, activity.name);
        ctx.set_activity(Some(activity));
    }
}
