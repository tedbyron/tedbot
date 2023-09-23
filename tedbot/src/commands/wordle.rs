use std::{collections::HashSet, sync::LazyLock, time::Instant};

use anyhow::Result;
use chrono::{DateTime, Duration, TimeZone, Utc};
use nom::{
    bytes::complete::tag,
    character::complete::{char, digit1, line_ending, multispace1, one_of, satisfy, space1},
    combinator::{map, map_res, opt},
    multi::count,
    sequence::{pair, terminated},
    IResult,
};
use poise::{command, futures_util::StreamExt, serenity_prelude::GuildChannel};

use crate::Context;

static WORDLE_DAY1: LazyLock<DateTime<Utc>> =
    LazyLock::new(|| Utc.with_ymd_and_hms(2021, 6, 19, 0, 0, 0).unwrap());

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TimestampedScore {
    pub timestamp: i64,
    pub score: Score,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, serde::Serialize, serde::Deserialize)]
struct Score {
    pub day: u32,
    pub success: bool,
    pub guesses: u8,
    pub hard_mode: bool,
    pub grid: Grid,
}

type Grid = Vec<Vec<Letter>>;

#[derive(Debug, PartialEq, Eq, PartialOrd, serde::Serialize, serde::Deserialize)]
pub enum Letter {
    Correct,
    Partial,
    Incorrect,
}

#[command(slash_command, rename = "wordle load")]
pub async fn load(
    ctx: Context<'_>,
    #[channel_types("Text")]
    #[description = "Provide a channel, or the current one will be used"]
    channel: Option<GuildChannel>,
) -> Result<()> {
    let channel_id = channel.map_or(ctx.channel_id(), |c| c.id);
    let mut unique_users = HashSet::<u64>::new();
    let mut score_count = 0;
    let mut msg_count = 0;

    let msg = ctx
        .say(format!("Loading wordle scores from <#{}>...", channel_id.0))
        .await?;

    let timer = Instant::now();

    let mut messages = channel_id.messages_iter(&ctx).boxed();
    while let Some(Ok(msg)) = messages.next().await {
        if *msg.timestamp < *WORDLE_DAY1 {
            break;
        }

        msg_count += 1;

        if let Ok((_, score)) = parse(&msg.content) {
            let timestamp = msg.timestamp.timestamp();
            if !wordle_day_ok(score.day, timestamp) {
                continue;
            }
        }
    }

    let user_count = unique_users.len();
    let timer_duration = timer.elapsed().as_secs();
    let timer_minutes = timer_duration / 60;
    let timer_seconds = timer_duration % 60;
    let duration = if timer_minutes > 0 {
        format!("{}m {}s", timer_minutes, timer_seconds)
    } else {
        format!("{}s", timer_seconds)
    };

    if score_count == 0 {
        msg.edit(ctx, |m| {
            m.content(format!(
                "Parsed {msg_count} messages in {duration}
No scores to add or update in <#{channel_id}>",
            ))
        })
        .await?;
    } else {
        msg.edit(ctx, |m| {
            m.content(format!(
                "Parsed {msg_count} messages in {duration}
Loaded {score_count} score{scores_plural} from {user_count} user{users_plural} in <#{channel_id}>",
                scores_plural = if score_count == 1 { "" } else { "s" },
                users_plural = if user_count == 1 { "" } else { "s" },
            ))
        })
        .await?;
    }

    Ok(())
}

fn wordle_day_ok(day: u32, msg_timestamp: i64) -> bool {
    let score_datetime = *WORDLE_DAY1 + Duration::days(i64::from(day - 1));
    let msg_datetime = Utc.timestamp_opt(msg_timestamp, 0).unwrap();

    score_datetime >= msg_datetime - Duration::days(1)
        || score_datetime > msg_datetime + Duration::days(1)
}

fn parse(input: &str) -> IResult<&str, Score> {
    let (input, (day, success, guesses, hard_mode)) = header(input)?;
    let (input, grid) = grid(input, guesses)?;

    Ok((
        input,
        Score {
            day,
            success,
            guesses,
            hard_mode,
            grid,
        },
    ))
}

fn header(input: &str) -> IResult<&str, (u32, bool, u8, bool)> {
    let (input, _) = terminated(tag("Wordle"), space1)(input)?;
    let (input, day) = map_res(terminated(digit1, space1), str::parse)(input)?;
    let (input, (success, guesses)) = map(
        terminated(
            satisfy(|c| c.is_ascii_digit() || c == 'X'),
            pair(char('/'), digit1),
        ),
        is_success,
    )(input)?;
    let (input, hard_mode) = map(terminated(opt(char('*')), multispace1), is_hard_mode)(input)?;

    Ok((input, (day, success, guesses, hard_mode)))
}

const fn is_success(guesses: char) -> (bool, u8) {
    match guesses.to_digit(10) {
        Some(n) => (true, n as u8),
        None => (false, 6),
    }
}

const fn is_hard_mode(has_asterisk: Option<char>) -> bool {
    has_asterisk.is_some()
}

fn grid(input: &str, guesses: u8) -> IResult<&str, Grid> {
    let letter = map(one_of("\u{1f7e9}\u{1f7e8}\u{2b1b}\u{2b1c}"), letter);
    let row = terminated(count(letter, 5), opt(line_ending));

    count(row, usize::from(guesses))(input)
}

const fn letter(letter: char) -> Letter {
    match letter {
        '\u{1f7e9}' => Letter::Correct,
        '\u{1f7e8}' => Letter::Partial,
        '\u{2b1b}' | '\u{2b1c}' => Letter::Incorrect,
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod test {
    use super::{
        parse,
        Letter::{Correct, Incorrect, Partial},
        Score,
    };

    #[test]
    fn wordle_loss() {
        let input = "Wordle 213 X/6

\u{2b1c}\u{2b1c}\u{2b1c}\u{2b1c}\u{1f7e8}
\u{2b1c}\u{2b1c}\u{1f7e8}\u{2b1c}\u{1f7e8}
\u{1f7e9}\u{1f7e9}\u{1f7e9}\u{2b1c}\u{2b1c}
\u{1f7e9}\u{1f7e9}\u{1f7e9}\u{2b1c}\u{2b1c}
\u{1f7e9}\u{1f7e9}\u{1f7e9}\u{2b1c}\u{2b1c}
\u{1f7e9}\u{1f7e9}\u{1f7e9}\u{2b1c}\u{2b1c}
this was a hard one";
        let output = parse(input);
        let expected = Ok((
            "this was a hard one",
            Score {
                day: 213,
                success: false,
                guesses: 6,
                hard_mode: false,
                grid: vec![
                    vec![Incorrect, Incorrect, Incorrect, Incorrect, Partial],
                    vec![Incorrect, Incorrect, Partial, Incorrect, Partial],
                    vec![Correct, Correct, Correct, Incorrect, Incorrect],
                    vec![Correct, Correct, Correct, Incorrect, Incorrect],
                    vec![Correct, Correct, Correct, Incorrect, Incorrect],
                    vec![Correct, Correct, Correct, Incorrect, Incorrect],
                ],
            },
        ));

        assert_eq!(output, expected);
    }

    #[test]
    fn wordle_win_hard_mode() {
        let input = "Wordle 224 4/6*
\u{2b1b}\u{2b1b}\u{2b1b}\u{2b1b}\u{2b1b}
\u{2b1b}\u{1f7e8}\u{2b1b}\u{1f7e8}\u{2b1b}
\u{1f7e9}\u{1f7e8}\u{1f7e9}\u{2b1b}\u{2b1b}
\u{1f7e9}\u{1f7e9}\u{1f7e9}\u{1f7e9}\u{1f7e9}";
        let output = parse(input);
        let expected = Ok((
            "",
            Score {
                day: 224,
                success: true,
                guesses: 4,
                hard_mode: true,
                grid: vec![
                    vec![Incorrect, Incorrect, Incorrect, Incorrect, Incorrect],
                    vec![Incorrect, Partial, Incorrect, Partial, Incorrect],
                    vec![Correct, Partial, Correct, Incorrect, Incorrect],
                    vec![Correct, Correct, Correct, Correct, Correct],
                ],
            },
        ));

        assert_eq!(output, expected);
    }
}
