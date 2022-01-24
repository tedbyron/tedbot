//! Wordle score result parser.

// TODO: Check message day is ±1

use nom::bytes::complete::tag;
use nom::character::complete::{char, digit1, line_ending, multispace1, one_of, space1};
use nom::combinator::{map, map_res, opt};
use nom::multi::count;
use nom::sequence::{terminated, tuple};
use nom::IResult;

use super::model::{Grid, Letter, Score};

pub fn parse(input: &str) -> IResult<&str, Score> {
    let (input, (day, tries)) = line1(input)?;
    let (input, grid) = grid(input, tries)?;

    Ok((input, Score { day, tries, grid }))
}

fn line1(input: &str) -> IResult<&str, (u32, u8)> {
    let (input, _) = terminated(tag("Wordle"), space1)(input)?;
    let (input, day) = map_res(terminated(digit1, space1), str::parse)(input)?;
    let (input, tries) = map_res(
        terminated(digit1, tuple((char('/'), digit1, multispace1))),
        str::parse,
    )(input)?;

    Ok((input, (day, tries)))
}

fn grid(input: &str, tries: u8) -> IResult<&str, Grid> {
    let letter = map(one_of("\u{1f7e9}\u{1f7e8}\u{2b1b}\u{2b1c}"), letter);
    let row = terminated(count(letter, 5), opt(line_ending));
    let (input, grid) = count(row, usize::from(tries))(input)?;

    Ok((input, grid))
}

const fn letter(letter: char) -> Letter {
    match letter {
        '\u{1f7e9}' => Letter::Correct,
        '\u{1f7e8}' => Letter::Partial,
        '\u{2b1b}' | '\u{2b1c}' => Letter::Incorrect,
        _ => unreachable!(),
    }
}
