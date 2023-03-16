use std::time::Instant;

use anyhow::Result;
use async_openai::types::{
    ChatCompletionRequestMessageArgs, CreateChatCompletionRequestArgs, Role,
};
use poise::serenity_prelude::Color;
use poise::{command, ChoiceParameter};

use crate::{Context, Data};

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
}

/// Order something...
#[command(slash_command)]
pub async fn order(ctx: Context<'_>, #[description = "Menu item"] item: OrderItem) -> Result<()> {
    match item {
        OrderItem::GalleyBoy => {
            ctx.say("<:galleyboy:915674675684712509>").await?;
        }
    }
    Ok(())
}

macro_rules! build_message {
    ($role:expr, $content:expr $(,)?) => {
        ChatCompletionRequestMessageArgs::default()
            .role($role)
            .content($content)
            .build()?
    };
}

/// Generate bad advice using OpenAI's gpt-3.5-turbo model
#[command(slash_command)]
pub async fn badadvice(
    ctx: Context<'_>,
    #[description = "What do you want advice about?"] prompt: String,
) -> Result<()> {
    let Data { openai_client } = ctx.data();

    // initial response and timer
    let handle = ctx
        .send(|m| {
            m.content("Loading...").embed(|e| {
                e.title(&prompt).color(Color::ROHRKATZE_BLUE).author(|a| {
                    a.name(&ctx.author().name).icon_url(
                        ctx.author()
                            .avatar_url()
                            .unwrap_or_else(|| ctx.author().default_avatar_url()),
                    )
                })
            })
        })
        .await?;
    let timer = Instant::now();

    // build and send request
    let req = CreateChatCompletionRequestArgs::default()
        .model("gpt-3.5-turbo")
        .messages([
            build_message!(
                Role::System,
                "You are a horrible life coach.
                You are giving bad advice to a client.
                You can only give funny, crazy, or absurd life advice.",
            ),
            build_message!(Role::User, format!("Give me bad advice about {prompt}.")),
        ])
        .build()?;
    let res = openai_client.chat().create(req).await?;
    let elapsed = timer.elapsed().as_millis();
    let s = elapsed / 1000;
    let ms = elapsed % 1000;

    // update response with result
    handle
        .edit(ctx, |m| {
            m.content(format!("Done in {s}s {ms}ms"))
                .embed(|e| {
                    e.title(&prompt).color(Color::ROHRKATZE_BLUE).author(|a| {
                        a.name(&ctx.author().name).icon_url(
                            ctx.author()
                                .avatar_url()
                                .unwrap_or_else(|| ctx.author().default_avatar_url()),
                        )
                    })
                })
                .embed(|e| {
                    e.color(Color::FOOYOO)
                        .description(&res.choices[0].message.content)
                })
        })
        .await?;

    Ok(())
}
