use std::time::Instant;

use anyhow::Result;
use async_openai::types::{
    ChatCompletionRequestMessageArgs, CreateChatCompletionRequestArgs, Role,
};
use poise::{command, serenity_prelude::Color};

use crate::{Context, Data};

macro_rules! build_message {
    ($role:expr, $content:expr $(,)?) => {
        ChatCompletionRequestMessageArgs::default()
            .role($role)
            .content($content)
            .build()?
    };
}

/// Generate bad advice using OpenAI's gpt-3.5-turbo model
#[command(slash_command, rename = "bad advice")]
pub async fn bad_advice(
    ctx: Context<'_>,
    #[description = "What do you want advice about?"] prompt: String,
) -> Result<()> {
    let Data { openai_client } = ctx.data();

    // defer response and start timer
    ctx.defer().await?;
    let timer = Instant::now();

    // build and send request
    let request = CreateChatCompletionRequestArgs::default()
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
    let response = openai_client.chat().create(request).await?;
    let elapsed = timer.elapsed().as_millis();
    let s = elapsed / 1000;
    let ms = elapsed % 1000;

    // update response with result
    ctx.send(|m| {
        m.content(format!("Done in {s}s {ms}ms"))
            .embed(|e| {
                e.title(&prompt)
                    .color(Color::ROHRKATZE_BLUE)
                    .author(|a| a.name(&ctx.author().name).icon_url(ctx.author().face()))
            })
            .embed(|e| {
                e.color(Color::FOOYOO)
                    .description(response.choices[0].message.content.as_deref().unwrap_or(""))
            })
    })
    .await?;

    Ok(())
}
