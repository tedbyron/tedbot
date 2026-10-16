use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use poise::{
    CreateReply, command,
    serenity_prelude::{
        ActionRowComponent, AutocompleteChoice, ButtonStyle, ComponentInteractionCollector,
        CreateActionRow, CreateAttachment, CreateAutocompleteResponse, CreateButton, CreateEmbed,
        CreateEmbedFooter, CreateInputText, CreateInteractionResponse,
        CreateInteractionResponseMessage, CreateModal, EditMessage, InputTextStyle,
        ModalInteractionCollector, ReactionType,
    },
};
use tracing::warn;

use crate::{
    Context, Data,
    ersatztv::{Guide, Program},
};

const NAV_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const POSTER: &str = "poster.jpg";
const POSTER_HEIGHT: u32 = 160;
const KEYPAD_MAX: usize = 12;
const PAGE_SIZE: usize = 12;

/// What's on a live TV channel right now
#[command(slash_command)]
pub async fn live(
    ctx: Context<'_>,
    #[description = "Channel number or name"]
    #[autocomplete = "autocomplete_channel"]
    channel: Option<String>,
) -> Result<()> {
    navigate(ctx, Mode::Channel, channel.as_deref()).await
}

/// What's on every live TV channel right now
#[command(slash_command, rename = "live-all")]
pub async fn live_all(
    ctx: Context<'_>,
    #[description = "Start at the page with this channel"]
    #[autocomplete = "autocomplete_channel"]
    channel: Option<String>,
) -> Result<()> {
    navigate(ctx, Mode::Grid, channel.as_deref()).await
}

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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// One channel per card, `index` is the channel.
    Channel,
    /// A page of channels, `index` is the page.
    Grid,
}

enum Target<'a> {
    Index(usize),
    Find(&'a str),
}

async fn navigate(ctx: Context<'_>, mode: Mode, query: Option<&str>) -> Result<()> {
    ctx.defer().await?;

    let data = ctx.data();
    let id = ctx.id().to_string();
    let modal_id = format!("{id}-modal");
    let target = query.map_or(Target::Index(0), Target::Find);

    let Some(mut screen) = Screen::build(data, mode, target).await? else {
        let content = query.map_or_else(
            || "No live TV channels right now".to_owned(),
            |q| format!("No channel matches \"{}\"", escape(q)),
        );
        ctx.say(content).await?;
        return Ok(());
    };

    let mut reply = CreateReply::default()
        .components(screen.components(&id))
        .embed(std::mem::take(&mut screen.embed));
    if let Some(poster) = screen.poster.take() {
        reply = reply.attachment(poster);
    }
    let mut message = ctx.send(reply).await?.into_message().await?;

    loop {
        tokio::select! {
            press = ComponentInteractionCollector::new(ctx)
                .message_id(message.id)
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
                let Some(next) = Screen::build(data, mode, Target::Index(index)).await? else {
                    break;
                };
                screen = next;
                press.create_response(ctx, screen.update(&id)).await?;
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
                let found = Screen::build(data, mode, Target::Find(&query)).await?;
                let response = found.map_or_else(
                    || {
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content(format!("No channel matches \"{}\"", escape(&query)))
                                .ephemeral(true),
                        )
                    },
                    |next| {
                        screen = next;
                        screen.update(&id)
                    },
                );
                submit.create_response(ctx, response).await?;
            }
        }
    }

    message
        .edit(ctx, EditMessage::new().components(vec![]))
        .await?;
    Ok(())
}

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

struct Screen {
    mode: Mode,
    embed: CreateEmbed,
    poster: Option<CreateAttachment>,
    index: usize,
    count: usize,
    /// Channel numbers for the keypad, empty when there are too many channels for one.
    numbers: Vec<String>,
}

impl Screen {
    /// `None` when there are no channels or `Target::Find` matches nothing.
    async fn build(data: &Data, mode: Mode, target: Target<'_>) -> Result<Option<Self>> {
        let guide = data.guide.get().await?;
        let channels = guide.channels.len();
        if channels == 0 {
            return Ok(None);
        }
        let found = match target {
            Target::Index(position) => Err(position),
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
                    .description(description);
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
            Mode::Grid => {
                let pages = channels.div_ceil(PAGE_SIZE);
                let page = found.map_or_else(|i| i % pages, |i| i / PAGE_SIZE);
                let mut embed = guide
                    .channels
                    .iter()
                    .skip(page * PAGE_SIZE)
                    .take(PAGE_SIZE)
                    .fold(CreateEmbed::new().title("Live TV"), |embed, channel| {
                        let value = match guide.on_at(channel, &now) {
                            (Some(p), _) => format!("{}\n{}", label(p, None), times(p)),
                            (None, _) => "\u{2014}".into(),
                        };
                        embed.field(channel.title(), value, true)
                    });
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

    fn update(&mut self, id: &str) -> CreateInteractionResponse {
        CreateInteractionResponse::UpdateMessage(
            CreateInteractionResponseMessage::new()
                .components(self.components(id))
                .embed(std::mem::take(&mut self.embed))
                .files(self.poster.take()),
        )
    }

    /// A keypad of channel numbers in rows of 3 when there are few channels, then ◀ 🔄 ▶,
    /// with ⌨️ to type a channel when there are too many for a keypad.
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

async fn director(data: &Data, p: &Program) -> Option<String> {
    if p.episode().is_some() {
        return None;
    }
    let jellyfin = data.jellyfin.as_ref()?;
    let id = p.jellyfin_item_id()?;
    match jellyfin.directors(&data.http, id).await {
        Ok(directors) => directors.first().map(|name| surname(name)),
        Err(e) => {
            warn!("Fetching directors for {id}: {e:#}");
            None
        }
    }
}

/// Keycap emoji for 0-10 so number buttons match the width of the emoji nav buttons.
fn keycap(number: &str) -> Option<ReactionType> {
    match number {
        "10" => Some('\u{1f51f}'.into()),
        n if n.len() == 1 && n.chars().all(|c| c.is_ascii_digit()) => {
            Some(ReactionType::Unicode(format!("{n}\u{fe0f}\u{20e3}")))
        }
        _ => None,
    }
}

/// "Guillermo del Toro" -> "del Toro".
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

/// **Title** S01 E02, or **Title** (Director, 1995) for movies.
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

fn times(p: &Program) -> String {
    format!("{} \u{2013} {}", time(&p.start), time(&p.stop))
}

fn time<Tz: TimeZone>(t: &DateTime<Tz>) -> String {
    format!("<t:{}:t>", t.timestamp())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surnames() {
        assert_eq!(surname("Flim Florp"), "Florp");
        assert_eq!(surname("Zib del Wobble"), "del Wobble");
        assert_eq!(surname("Quaz"), "Quaz");
    }
}
