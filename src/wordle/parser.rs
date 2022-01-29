//! Wordle score result parser.

// TODO: Check message day is ±1

use nom::bytes::complete::tag;
use nom::character::complete::{char, digit1, line_ending, multispace1, one_of, satisfy, space1};
use nom::combinator::{map, map_res, opt};
use nom::multi::count;
use nom::sequence::{pair, terminated};
use nom::IResult;

use super::model::{Grid, Letter, Score};

pub fn parse(input: &str) -> IResult<&str, Score> {
    let (input, (day, success, tries, hard_mode)) = header(input)?;
    let (input, grid) = grid(input, tries)?;

    Ok((
        input,
        Score {
            day,
            success,
            tries,
            hard_mode,
            grid,
        },
    ))
}

fn header(input: &str) -> IResult<&str, (u32, bool, u8, bool)> {
    let (input, _) = terminated(tag("Wordle"), space1)(input)?;
    let (input, day) = map_res(terminated(digit1, space1), str::parse)(input)?;
    let (input, (success, tries)) = map(
        terminated(
            satisfy(|c| c.is_ascii_digit() || c == 'X'),
            pair(char('/'), digit1),
        ),
        is_success,
    )(input)?;
    let (input, hard_mode) = map(terminated(opt(char('*')), multispace1), is_hard_mode)(input)?;

    Ok((input, (day, success, tries, hard_mode)))
}

fn grid(input: &str, tries: u8) -> IResult<&str, Grid> {
    let letter = map(one_of("\u{1f7e9}\u{1f7e8}\u{2b1b}\u{2b1c}"), letter);
    let row = terminated(count(letter, 5), opt(line_ending));
    let (input, grid) = count(row, usize::from(tries))(input)?;

    Ok((input, grid))
}

#[allow(clippy::cast_possible_truncation)]
fn is_success(tries: char) -> (bool, u8) {
    match tries.to_digit(10) {
        Some(n) => (true, n as u8),
        None => (false, 6),
    }
}

const fn is_hard_mode(has_asterisk: Option<char>) -> bool {
    has_asterisk.is_some()
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
    use super::Letter::{Correct, Incorrect, Partial};
    use super::{parse, Score};

    #[test]
    fn wordle_loss() {
        let input = "Wordle 213 X/6

⬜⬜⬜⬜🟨
⬜⬜🟨⬜🟨
🟩🟩🟩⬜⬜
🟩🟩🟩⬜⬜
🟩🟩🟩⬜⬜
🟩🟩🟩⬜⬜
this was a hard one";
        let output = parse(input);
        let expected = Ok((
            "this was a hard one",
            Score {
                day: 213,
                success: false,
                tries: 6,
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
⬛⬛⬛⬛⬛
⬛🟨⬛🟨⬛
🟩🟨🟩⬛⬛
🟩🟩🟩🟩🟩";
        let output = parse(input);
        let expected = Ok((
            "",
            Score {
                day: 224,
                success: true,
                tries: 4,
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
