use std::time::Duration;

use anyhow::Result;
use poise::futures_util::StreamExt;
use poise::serenity_prelude::{Color, CreateComponents, InteractionResponseType};
use poise::{command, ChoiceParameter};
use rand::Rng;

use crate::Context;

/// Ping the bot
#[command(slash_command)]
pub async fn ping(ctx: Context<'_>) -> Result<()> {
    ctx.say("Pong!").await?;
    Ok(())
}

#[derive(Debug, ChoiceParameter)]
pub enum OrderItem {
    #[name = "galley boy"]
    GalleyBoy,
    #[name = "bing chilling"]
    BingChilling,
}

const BING_CHILLINGS: &[&str] = &[
    "https://www.youtube.com/watch?v=BMvqvnyGtGo",
    "https://www.youtube.com/watch?v=vE-kqcNh-bo",
    "https://www.youtube.com/watch?v=rhfVXoEhd1c",
];

/// Order something...
#[command(slash_command)]
pub async fn order(ctx: Context<'_>, #[description = "Menu item"] item: OrderItem) -> Result<()> {
    match item {
        OrderItem::GalleyBoy => {
            ctx.say("<:galleyboy:915674675684712509>").await?;
        }
        OrderItem::BingChilling => {
            let i = rand::thread_rng().gen_range(0..BING_CHILLINGS.len());
            ctx.say(BING_CHILLINGS[i]).await?;
        }
    }
    Ok(())
}

/// Create a poll
#[command(slash_command)]
pub async fn poll(
    ctx: Context<'_>,
    #[description = "Poll title"] title: String,
    #[description = "Poll duration in seconds (default: 1h)"] duration: Option<u64>,
    #[description = "Comma separated answers"] answers: String,
) -> Result<()> {
    _poll(ctx, title, duration, None, answers).await
}

/// Create a poll that accepts multiple responses
#[command(slash_command)]
pub async fn multipoll(
    ctx: Context<'_>,
    #[description = "Poll title"] title: String,
    #[description = "Poll duration in seconds (default: 1h)"] duration: Option<u64>,
    #[description = "Maximum number of selected answers"] max: u64,
    #[description = "Comma separated answers"] answers: String,
) -> Result<()> {
    _poll(ctx, title, duration, Some(max), answers).await
}

async fn _poll(
    ctx: Context<'_>,
    title: String,
    duration: Option<u64>,
    max: Option<u64>,
    answers: String,
) -> Result<()> {
    let answers = answers
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();
    let duration = Duration::from_secs(duration.unwrap_or(3600));

    let m = ctx
        .send(|m| {
            m.embed(|e| {
                e.title(&title)
                    .author(|a| {
                        a.name(&ctx.author().name).icon_url(
                            ctx.author()
                                .avatar_url()
                                .unwrap_or_else(|| ctx.author().default_avatar_url()),
                        )
                    })
                    .colour(Color::ROHRKATZE_BLUE)
            })
            .components(|c| {
                c.create_action_row(|row| {
                    row.create_button(|b| b.custom_id("Answer").emoji('\u{1f4e2}').label("Answer"))
                })
            })
        })
        .await?;

    let mut interactions = m
        .into_message()
        .await?
        .await_component_interactions(ctx)
        .timeout(duration)
        .build();

    while let Some(interaction) = interactions.next().await {
        if interaction.data.custom_id == "Answer" {
            interaction
                .create_interaction_response(ctx, |m| {
                    m.interaction_response_data(|c| {
                        c.ephemeral(true).components(|c| {
                            c.create_action_row(|r| {
                                r.create_select_menu(|menu| {
                                    if let Some(max) = max {
                                        menu.min_values(1);
                                        menu.max_values(max);
                                    }
                                    menu.custom_id(&title);
                                    menu.placeholder("Select an answer");
                                    menu.options(|f| {
                                        for answer in &answers {
                                            f.create_option(|o| o.label(answer).value(answer));
                                        }
                                        f
                                    })
                                })
                            })
                        })
                    })
                })
                .await?;

            let msg = interaction.get_interaction_response(ctx).await?;

            match msg
                .await_component_interaction(ctx)
                .timeout(Duration::from_secs(300))
                .await
            {
                Some(interaction) => {
                    println!("{:?}", interaction.data);
                    interaction
                        .create_interaction_response(ctx, |m| {
                            m.kind(InteractionResponseType::UpdateMessage)
                                .interaction_response_data(|c| {
                                    c.ephemeral(true)
                                        .content("Response recorded")
                                        .set_components(CreateComponents::default())
                                })
                        })
                        .await?
                }
                None => {
                    msg.reply(ctx, "Timed out").await?;
                }
            }
        }
    }

    Ok(())
}
