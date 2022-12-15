use std::time::Duration;

use anyhow::Result;
use poise::futures_util::StreamExt;
use poise::serenity_prelude::InteractionResponseType;
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

/// Create a survey
#[command(slash_command)]
pub async fn survey(
    ctx: Context<'_>,
    #[description = "Survey title"] title: String,
    #[description = "Survey duration in seconds (default: 1h)"] duration: Option<u64>,
    #[description = "Comma separated answers"] answers: String,
) -> Result<()> {
    _survey(ctx, title, duration, None, answers).await
}

/// Create a survey that accepts multiple responses
#[command(slash_command)]
pub async fn multisurvey(
    ctx: Context<'_>,
    #[description = "Survey title"] title: String,
    #[description = "Survey duration in seconds (default: 1h)"] duration: Option<u64>,
    #[description = "Maximum number of selected answers"] max: u64,
    #[description = "Comma separated answers"] answers: String,
) -> Result<()> {
    _survey(ctx, title, duration, Some(max), answers).await
}

async fn _survey(
    ctx: Context<'_>,
    title: String,
    duration: Option<u64>,
    max: Option<u64>,
    answers: String,
) -> Result<()> {
    let answers = answers.split(',').map(str::trim).collect::<Vec<_>>();

    let msg = ctx
        .send(|m| {
            m.content(&title).components(|c| {
                c.create_action_row(|row| {
                    row.create_select_menu(|menu| {
                        if let Some(max) = max {
                            menu.min_values(1);
                            menu.max_values(max);
                        }
                        menu.custom_id(&title);
                        menu.placeholder("Select an answer");
                        menu.options(|f| {
                            for answer in answers {
                                f.create_option(|o| o.label(answer).value(answer));
                            }
                            f
                        })
                    })
                })
            })
        })
        .await?;

    let mut interactions = msg
        .into_message()
        .await?
        .await_component_interactions(ctx)
        .timeout(Duration::from_secs(duration.unwrap_or(3600)))
        .build();

    while let Some(interaction) = interactions.next().await {
        interaction
            .create_interaction_response(ctx, |r| {
                r.kind(InteractionResponseType::UpdateMessage)
                    .interaction_response_data(|d| d.content(&interaction.data.values[0]))
            })
            .await?;
    }

    Ok(())
}
