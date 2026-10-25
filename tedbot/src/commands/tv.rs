//! `/tv`: what's on ErsatzTV's live TV channels, as a list of every channel or one channel's card.
//!
//! Both views reply ephemerally with an embed and navigation buttons, then wait for button presses for
//! [`NAV_TIMEOUT`] before removing the buttons. Each press rebuilds the [`Screen`] from the guide
//! and edits the same message in place.

use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use poise::{
    CreateReply, command,
    serenity_prelude::{
        ActionRowComponent, AutocompleteChoice, Builder as _, ButtonStyle,
        ComponentInteractionCollector, CreateActionRow, CreateAttachment,
        CreateAutocompleteResponse, CreateButton, CreateEmbed, CreateEmbedFooter, CreateInputText,
        CreateInteractionResponse, CreateInteractionResponseFollowup, CreateModal, EditAttachments,
        EditInteractionResponse, InputTextStyle, ModalInteractionCollector, ReactionType,
        Timestamp,
    },
};
use tracing::warn;

use super::escape;
use crate::{
    Context, Data,
    ersatztv::{Guide, Program},
};

/// How long buttons keep working after the last press. An ephemeral reply can only be edited with
/// an interaction token, which expires after 15 minutes, so this stays under that to remove the
/// buttons at the end.
const NAV_TIMEOUT: Duration = Duration::from_secs(14 * 60);
/// Attachment file name the embed thumbnail refers to (`attachment://poster.jpg`)
const POSTER: &str = "poster.jpg";
/// Thumbnail height in pixels
const POSTER_HEIGHT: u32 = 160;
/// Most channels that get a keypad; beyond this, ⌨️ opens a jump-to-channel modal instead
const KEYPAD_MAX: usize = 12;
/// Channels per page of the channel list. Discord allows at most 25 fields per embed.
const PAGE_SIZE: usize = 12;

/// What's on live TV right now
#[command(slash_command)]
pub async fn tv(
    ctx: Context<'_>,
    #[description = "Channel number or name"]
    #[autocomplete = "autocomplete_channel"]
    channel: Option<String>,
) -> Result<()> {
    let mode = if channel.is_some() {
        Mode::Channel
    } else {
        Mode::List
    };
    navigate(ctx, mode, channel).await
}

/// Suggests up to 25 channels (Discord's limit) by title, filling in the channel number so
/// [`crate::ersatztv::Guide::find`] matches it exactly. An unreachable guide gives no suggestions
/// rather than an error.
async fn autocomplete_channel(ctx: Context<'_>, partial: &str) -> CreateAutocompleteResponse {
    let choices = match ctx.data().guide.get().await {
        Ok(guide) => guide
            .search(partial, 25)
            .into_iter()
            .map(|c| {
                let title: String = c.title().chars().take(100).collect();
                let value = c.number().map_or_else(|| title.clone(), str::to_owned);
                AutocompleteChoice::new(title, value)
            })
            .collect(),
        Err(e) => {
            warn!("Fetching guide for autocomplete: {e:#}");
            Vec::new()
        }
    };
    CreateAutocompleteResponse::new().set_choices(choices)
}

/// Which view is showing, and so how [`Screen::index`] is read
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One channel per card, `index` is the channel
    Channel,
    /// A page of channels listed top to bottom, `index` is the page
    List,
}

/// What to show next
enum Target {
    /// A channel or page by position. Wraps around, so ◀ on the first goes to the last.
    Index(usize),
    /// The channel matching what the user typed (see [`crate::ersatztv::Guide::find`]), or in
    /// list mode, the page containing it
    Find(String),
}

/// Sends the first screen, then handles button presses and jump-modal submits until
/// [`NAV_TIMEOUT`] passes with no interaction, and finally removes the buttons.
///
/// Only presses on this reply are collected. Button and modal ids are prefixed with the
/// invocation id so concurrent invocations' modals don't collide.
async fn navigate(ctx: Context<'_>, mode: Mode, query: Option<String>) -> Result<()> {
    ctx.defer_ephemeral().await?;

    let data = ctx.data();
    let id = ctx.id().to_string();
    let modal_id = format!("{id}-modal");
    let target = query.map_or(Target::Index(0), Target::Find);

    let Some(mut screen) = Screen::build(data, mode, &target).await? else {
        let content = match &target {
            Target::Find(q) => no_match(q),
            Target::Index(_) => "No live TV channels right now".to_owned(),
        };
        ctx.say(content).await?;
        return Ok(());
    };

    let mut reply = CreateReply::default()
        .components(screen.components(&id))
        .embed(std::mem::take(&mut screen.embed));
    if let Some(poster) = screen.poster.take() {
        reply = reply.attachment(poster);
    }
    let handle = ctx.send(reply).await?;
    let message = handle.message().await?.id;
    let mut last = None;

    loop {
        // Acknowledge first: rebuilding can refetch the guide and posters, which may take longer
        // than the 3 seconds Discord allows before an interaction fails
        let (token, target) = tokio::select! {
            press = ComponentInteractionCollector::new(ctx)
                .message_id(message)
                .timeout(NAV_TIMEOUT)
                .into_future() => {
                let Some(press) = press else { break };
                let pressed = press.data.custom_id.as_str();
                let index = match pressed.strip_prefix(id.as_str()) {
                    Some("-next") => screen.index + 1,
                    Some("-prev") => screen.index + screen.count - 1,
                    Some("-refresh") => screen.index,
                    Some("-jump") => {
                        press.create_response(ctx, jump_modal(&modal_id)).await?;
                        continue;
                    }
                    Some(rest) => match rest.strip_prefix("-ch-").and_then(|i| i.parse().ok()) {
                        Some(i) => i,
                        None => continue,
                    },
                    None => continue,
                };
                press.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
                (press.token, Target::Index(index))
            }
            submit = ModalInteractionCollector::new(ctx)
                .custom_ids(vec![modal_id.clone()])
                .timeout(NAV_TIMEOUT)
                .into_future() => {
                let Some(submit) = submit else { break };
                let query = submit
                    .data
                    .components
                    .iter()
                    .flat_map(|row| &row.components)
                    .find_map(|c| match c {
                        ActionRowComponent::InputText(input) => input.value.clone(),
                        _ => None,
                    })
                    .unwrap_or_default();
                submit.create_response(ctx, CreateInteractionResponse::Acknowledge).await?;
                (submit.token, Target::Find(query))
            }
        };

        last = Some(token.clone());
        let notice = match Screen::build(data, mode, &target).await {
            Ok(Some(next)) => {
                screen = next;
                screen.edit(&id).execute(ctx, &token).await?;
                continue;
            }
            Ok(None) => match &target {
                Target::Find(q) => no_match(q),
                Target::Index(_) => break,
            },
            Err(e) => {
                warn!("Updating live TV: {e:#}");
                "Couldn't load the TV guide, try again in a bit \u{1fae4}".to_owned()
            }
        };
        CreateInteractionResponseFollowup::new()
            .content(notice)
            .ephemeral(true)
            .execute(ctx, (None, &token))
            .await?;
    }

    // The newest token is the only one guaranteed not to have expired
    match last {
        Some(token) => {
            EditInteractionResponse::new()
                .components(vec![])
                .execute(ctx, &token)
                .await?;
        }
        None => {
            handle
                .edit(ctx, CreateReply::default().components(vec![]))
                .await?;
        }
    }
    Ok(())
}

/// Reply when [`Target::Find`] matches nothing
fn no_match(query: &str) -> String {
    format!("No channel matches \"{}\"", escape(query))
}

/// The ⌨️ jump-to-channel popup, with one text box for a channel number or name
fn jump_modal(modal_id: &str) -> CreateInteractionResponse {
    CreateInteractionResponse::Modal(CreateModal::new(modal_id, "Jump to channel").components(
        vec![
            CreateActionRow::InputText(
                CreateInputText::new(InputTextStyle::Short, "Channel number or name", "query")
                    .max_length(100),
            ),
        ],
    ))
}

/// One rendered view: the embed and poster to show, plus where we are so the buttons know where
/// to go next
struct Screen {
    mode: Mode,
    /// Taken (left empty) when sent
    embed: CreateEmbed,
    /// Current program's poster, channel view only. Taken when sent.
    poster: Option<CreateAttachment>,
    /// The channel or page shown
    index: usize,
    /// Number of channels or pages, for wrapping ◀ ▶
    count: usize,
    /// Channel numbers for the keypad, empty when there are too many channels for one
    numbers: Vec<String>,
}

impl Screen {
    /// Renders `target` from the current guide. For a channel, also fetches the poster and the
    /// movie director, best effort: failures are logged and the card is shown without them.
    ///
    /// `None` when there are no channels or `Target::Find` matches nothing. Errors only if the
    /// guide can't be fetched.
    async fn build(data: &Data, mode: Mode, target: &Target) -> Result<Option<Self>> {
        let guide = data.guide.get().await?;
        let channels = guide.channels.len();
        if channels == 0 {
            return Ok(None);
        }
        let found = match target {
            &Target::Index(position) => Err(position),
            Target::Find(query) => match guide.find(query) {
                Some(channel) => Ok(channel),
                None => return Ok(None),
            },
        };
        let now = Utc::now();

        match mode {
            Mode::Channel => {
                let index = found.unwrap_or_else(|i| i % channels);
                let channel = &guide.channels[index];
                let (current, next) = guide.on_at(channel, &now);

                let mut description = match current {
                    Some(p) => format!(
                        "{}\n{}",
                        label(p, director(data, p).await.as_deref()),
                        times(p)
                    ),
                    None => "Nothing on".into(),
                };
                if let Some(p) = next {
                    let label = label(p, director(data, p).await.as_deref());
                    description = format!("{description}\n\nNext: {label}");
                }

                let mut embed = CreateEmbed::new()
                    .title(channel.title())
                    .description(description)
                    .timestamp(Timestamp::now());
                let mut poster = None;
                if let Some(url) = current.and_then(|p| p.poster_url(POSTER_HEIGHT)) {
                    match Guide::fetch_image(&data.http, &url).await {
                        Ok(bytes) => {
                            embed = embed.thumbnail(format!("attachment://{POSTER}"));
                            poster = Some(CreateAttachment::bytes(bytes, POSTER));
                        }
                        Err(e) => warn!("Fetching poster {url}: {e:#}"),
                    }
                }

                let numbers = if channels <= KEYPAD_MAX {
                    guide
                        .channels
                        .iter()
                        .enumerate()
                        .map(|(i, c)| {
                            c.number()
                                .map_or_else(|| (i + 1).to_string(), str::to_owned)
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                Ok(Some(Self {
                    mode,
                    embed,
                    poster,
                    index,
                    count: channels,
                    numbers,
                }))
            }
            Mode::List => {
                let pages = channels.div_ceil(PAGE_SIZE);
                let page = found.map_or_else(|i| i % pages, |i| i / PAGE_SIZE);
                let mut embed = guide
                    .channels
                    .iter()
                    .skip(page * PAGE_SIZE)
                    .take(PAGE_SIZE)
                    .fold(
                        CreateEmbed::new().timestamp(Timestamp::now()),
                        |embed, channel| {
                            let value = match guide.on_at(channel, &now) {
                                (Some(p), _) => format!("{} \u{b7} {}", label(p, None), times(p)),
                                (None, _) => "\u{2014}".into(),
                            };
                            embed.field(channel.title(), value, false)
                        },
                    );
                if pages > 1 {
                    embed = embed.footer(CreateEmbedFooter::new(format!(
                        "Page {} of {pages}",
                        page + 1
                    )));
                }

                Ok(Some(Self {
                    mode,
                    embed,
                    poster: None,
                    index: page,
                    count: pages,
                    numbers: Vec::new(),
                }))
            }
        }
    }

    /// Replaces the message's embed, buttons and poster, dropping the old poster
    fn edit(&mut self, id: &str) -> EditInteractionResponse {
        let attachments = self
            .poster
            .take()
            .into_iter()
            .fold(EditAttachments::new(), EditAttachments::add);
        EditInteractionResponse::new()
            .components(self.components(id))
            .embed(std::mem::take(&mut self.embed))
            .attachments(attachments)
    }

    /// A keypad of channel numbers in rows of 3 when there are few channels, then ◀ 🔄 ▶,
    /// with ⌨️ to type a channel when there are too many for a keypad
    fn components(&self, id: &str) -> Vec<CreateActionRow> {
        let button = |suffix: &str| CreateButton::new(format!("{id}-{suffix}"));
        let mut rows = Vec::new();

        if self.mode == Mode::Channel && self.count > 1 && !self.numbers.is_empty() {
            let keycaps: Option<Vec<ReactionType>> =
                self.numbers.iter().map(|n| keycap(n)).collect();
            let mut keys: Vec<CreateButton> = self
                .numbers
                .iter()
                .enumerate()
                .map(|(i, number)| {
                    let key = button(&format!("ch-{i}"));
                    match &keycaps {
                        Some(keycaps) => key.emoji(keycaps[i].clone()),
                        None => key.label(number),
                    }
                    .style(if i == self.index {
                        ButtonStyle::Primary
                    } else {
                        ButtonStyle::Secondary
                    })
                })
                .collect();
            while !keys.len().is_multiple_of(3) {
                keys.push(
                    button(&format!("pad-{}", keys.len()))
                        .label("\u{200b}")
                        .style(ButtonStyle::Secondary)
                        .disabled(true),
                );
            }
            rows.extend(
                keys.chunks(3)
                    .map(|row| CreateActionRow::Buttons(row.to_vec())),
            );
        }

        let refresh = button("refresh")
            .emoji('\u{1f504}')
            .style(ButtonStyle::Secondary);
        let nav = if self.count > 1 {
            let mut nav = vec![
                button("prev")
                    .emoji('\u{25c0}')
                    .style(ButtonStyle::Secondary),
            ];
            if rows.is_empty() {
                nav.push(
                    button("jump")
                        .emoji('\u{2328}')
                        .style(ButtonStyle::Secondary),
                );
            }
            nav.push(refresh);
            nav.push(
                button("next")
                    .emoji('\u{25b6}')
                    .style(ButtonStyle::Secondary),
            );
            nav
        } else {
            vec![refresh]
        };
        rows.push(CreateActionRow::Buttons(nav));
        rows
    }
}

/// Surname of a movie's first director, from Jellyfin. `None` for episodes, or if the lookup
/// fails.
async fn director(data: &Data, p: &Program) -> Option<String> {
    if p.episode().is_some() {
        return None;
    }
    let id = p.jellyfin_item_id()?;
    match data.jellyfin.directors(&data.http, id).await {
        Ok(directors) => directors.first().map(|name| surname(name)),
        Err(e) => {
            warn!("Fetching directors for {id}: {e:#}");
            None
        }
    }
}

/// Keycap emoji for 0-10 so number buttons match the width of the emoji nav buttons. `None` for
/// other numbers, in which case the whole keypad falls back to text labels.
fn keycap(number: &str) -> Option<ReactionType> {
    match number {
        "10" => Some('\u{1f51f}'.into()),
        n if n.len() == 1 && n.chars().all(|c| c.is_ascii_digit()) => {
            Some(ReactionType::Unicode(format!("{n}\u{fe0f}\u{20e3}")))
        }
        _ => None,
    }
}

/// The last name plus any lowercase particles before it: "Guillermo del Toro" -> "del Toro"
fn surname(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let Some((_, rest)) = words.split_last() else {
        return name.to_owned();
    };
    let particles = rest
        .iter()
        .rev()
        .take_while(|w| w.starts_with(char::is_lowercase))
        .count();
    words[rest.len() - particles..].join(" ")
}

/// **Title** S01 E02, or **Title** (Director, 1995) for movies
fn label(p: &Program, director: Option<&str>) -> String {
    let title = format!("**{}**", escape(&p.title.value));
    if let Some(episode) = p.episode() {
        return format!("{title} {episode}");
    }
    let credits: Vec<&str> = director.into_iter().chain(p.date.as_deref()).collect();
    if credits.is_empty() {
        title
    } else {
        format!("{title} ({})", credits.join(", "))
    }
}

/// "7:00 PM – 8:30 PM" in each viewer's own time zone
fn times(p: &Program) -> String {
    format!("{} \u{2013} {}", time(&p.start), time(&p.stop))
}

/// A Discord timestamp tag, which each client renders as a short time in its local zone
fn time<Tz: TimeZone>(t: &DateTime<Tz>) -> String {
    format!("<t:{}:t>", t.timestamp())
}
