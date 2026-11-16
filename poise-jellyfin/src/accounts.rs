//! Posts in `admin_channels` when a Jellyfin account is disabled, by an admin or
//! after too many failed sign-ins, and when it's enabled again. Jellyfin doesn't say, so its
//! accounts are checked every [`POLL_EVERY`]. Like Alertmanager's posts: red while disabled,
//! green once enabled.

use std::{collections::HashMap, net::IpAddr, sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use poise::serenity_prelude::{
    Colour, CreateAllowedMentions, CreateEmbed, CreateMessage, Http, Timestamp,
};
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use crate::{
    MediaServer,
    announce::requester,
    commands::escape,
    error::chain,
    jellyfin::{Activity, User},
    seerr::normalize,
};

/// How often accounts are checked
const POLL_EVERY: Duration = Duration::from_secs(15 * 60);
/// How many of the activity log's newest entries are searched for why an account was disabled
const ACTIVITY: u32 = 100;
/// Alertmanager's colour for firing alerts
const DISABLED: Colour = Colour::new(0x0099_2d22);
/// Alertmanager's colour for resolved alerts
const ENABLED: Colour = Colour::new(0x002e_cc71);

/// Posts in `admin_channels` when a Jellyfin account is disabled or enabled again.
///
/// Checks in the background for as long as the bot runs; call it once Discord is connected. Does
/// nothing without any channels. Changes while the bot isn't running aren't posted.
pub fn watch_accounts(discord: Arc<Http>, data: MediaServer) {
    if data.channels.admin_channels.is_empty() {
        return;
    }
    tokio::spawn(async move { watch(&discord, &data).await });
}

/// [`watch_accounts`]' loop
async fn watch(discord: &Http, data: &MediaServer) {
    let mut interval = tokio::time::interval(POLL_EVERY);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Whether each account was disabled, by id, as last seen
    let mut seen: Option<HashMap<String, bool>> = None;
    let mut failing = false;
    loop {
        interval.tick().await;
        let users = match data.jellyfin.users(&data.http).await {
            Ok(users) => users,
            Err(e) => {
                // Once per outage, not every minute of it
                if !failing {
                    warn!("Fetching Jellyfin's accounts: {}", chain(&e));
                    failing = true;
                }
                continue;
            }
        };
        if failing {
            info!("Fetching Jellyfin's accounts works again");
            failing = false;
        }
        if let Some(seen) = &seen {
            for user in &users {
                if seen
                    .get(&user.id)
                    .is_some_and(|&was| was != user.policy.is_disabled)
                {
                    post(discord, data, user).await;
                }
            }
        }
        seen = Some(
            users
                .into_iter()
                .map(|u| (u.id, u.policy.is_disabled))
                .collect(),
        );
    }
}

/// Posts that `user` was disabled or enabled again, whichever they are now, with their Discord
/// account where they linked one in Seerr and are in the channel's server
async fn post(discord: &Http, data: &MediaServer, user: &User) {
    let disabled = user.policy.is_disabled;
    info!(
        "Jellyfin account {} was {}",
        user.name,
        if disabled { "disabled" } else { "enabled" }
    );
    let name = escape(&user.name);
    let mut embed = CreateEmbed::new().timestamp(Timestamp::now());
    if disabled {
        embed = embed
            .title(format!("{name}'s Jellyfin account was disabled"))
            .colour(DISABLED);
        let why = match data.jellyfin.activity(&data.http, ACTIVITY).await {
            Ok(entries) => lockout(&entries, user, Utc::now()),
            Err(e) => {
                warn!("Fetching Jellyfin's activity log: {}", chain(&e));
                None
            }
        };
        if let Some(why) = why {
            embed = embed.description(why);
        }
    } else {
        embed = embed
            .title(format!("{name}'s Jellyfin account is enabled again"))
            .colour(ENABLED);
    }

    let linked = data
        .seerr
        .discord_ids(&data.http, &[&user.id])
        .await
        .inspect_err(|e| warn!("Fetching Discord ids from Seerr: {}", chain(&e)))
        .ok()
        .and_then(|ids| ids.get(&normalize(&user.id)).copied());
    for &channel in &data.channels.admin_channels {
        let mut embed = embed.clone();
        if let Some(mention) = requester(discord, channel, linked).await {
            embed = embed.field("Discord", mention, true);
        }
        let message = CreateMessage::new()
            .embed(embed)
            .allowed_mentions(CreateAllowedMentions::new());
        if let Err(e) = channel.send_message(discord, message).await {
            warn!("Posting in channel {channel}: {e}");
        }
    }
}

/// "Locked out after 5 failed sign-ins, the last from 1.2.3.4", if `entries` (newest first) say
/// Jellyfin locked `user` out within the last two checks before `now`
fn lockout(entries: &[Activity], user: &User, now: DateTime<Utc>) -> Option<String> {
    let window = TimeDelta::from_std(POLL_EVERY * 2).ok()?;
    let id = normalize(&user.id);
    let at = entries
        .iter()
        .filter(|a| a.r#type == "UserLockedOut")
        .filter(|a| a.user_id.as_deref().map(normalize).as_ref() == Some(&id))
        .find_map(Activity::date)
        .filter(|&at| now - at <= window)?;
    // The failed sign-in that locked them out is logged at the same moment. It names the user,
    // but in the server's language, so it's matched by time instead.
    let ip = entries
        .iter()
        .filter(|a| a.r#type == "AuthenticationFailed")
        .filter(|a| {
            a.date()
                .is_some_and(|d| (d - at).abs() <= TimeDelta::seconds(2))
        })
        .find_map(|a| {
            a.short_overview
                .as_deref()?
                .rsplit(' ')
                .next()?
                .parse::<IpAddr>()
                .ok()
        });
    let attempts = match user.policy.login_attempts_before_lockout {
        Some(n) if n > 0 => format!("{n} failed sign-ins"),
        _ => "too many failed sign-ins".into(),
    };
    Some(ip.map_or_else(
        || format!("Locked out after {attempts}"),
        |ip| format!("Locked out after {attempts}, the last from {ip}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_out() {
        let entries: Vec<Activity> = serde_json::from_str(
            r#"[
                {"Type": "AuthenticationFailed", "Date": "2026-11-16T00:59:04.387362Z",
                 "UserId": "00000000000000000000000000000000",
                 "ShortOverview": "IP address: 159.26.99.225"},
                {"Type": "UserLockedOut", "Date": "2026-11-16T00:59:04.386524Z",
                 "UserId": "2610ece98f304b6d8e7958f6d2b55589"},
                {"Type": "AuthenticationFailed", "Date": "2026-11-16T00:58:00Z",
                 "ShortOverview": "IP address: 192.168.2.1"}
            ]"#,
        )
        .unwrap();
        let user: User = serde_json::from_str(
            r#"{"Id": "2610ece9-8f30-4b6d-8e79-58f6d2b55589", "Name": "th",
                "Policy": {"IsDisabled": true, "LoginAttemptsBeforeLockout": 5}}"#,
        )
        .unwrap();
        let at = |s| DateTime::parse_from_rfc3339(s).unwrap().to_utc();

        assert_eq!(
            lockout(&entries, &user, at("2026-11-16T01:00:00Z")).as_deref(),
            Some("Locked out after 5 failed sign-ins, the last from 159.26.99.225")
        );
        // Too long ago to be why they're disabled now
        assert_eq!(lockout(&entries, &user, at("2026-11-16T01:40:00Z")), None);
        // Only failed sign-ins
        assert_eq!(
            lockout(&entries[2..], &user, at("2026-11-16T01:00:00Z")),
            None
        );
    }
}
