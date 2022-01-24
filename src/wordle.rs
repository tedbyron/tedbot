//! Wordle score result parser.

// TODO: Check message day is ±1

use nom::bytes::complete::tag;
use nom::character::complete::{char, digit1, multispace1, space1};
use nom::combinator::map_res;
use nom::sequence::{terminated, tuple};
use nom::IResult;

#[derive(Debug, Clone)]
pub struct Score {
    day: u32,
    tries: u8,
    grid: Grid,
}

type Grid = Vec<[Letter; 5]>;

#[derive(Debug, Clone, Copy)]
enum Letter {
    Correct,
    Partial,
    Incorrect,
}

pub fn parse(input: &'static str) -> crate::Result<Score> {
    let (input, (day, tries)) = parse_line1(input)?;
    let (input, grid) = parse_grid(input)?;

    Ok(Score { day, tries, grid })
}

fn parse_line1(input: &str) -> IResult<&str, (u32, u8)> {
    let (input, _) = terminated(tag("Wordle"), space1)(input)?;
    let (input, day) = map_res(terminated(digit1, space1), str::parse::<u32>)(input)?;
    let (input, tries) = map_res(
        terminated(digit1, tuple((char('/'), digit1, multispace1))),
        str::parse::<u8>,
    )(input)?;

    Ok((input, (day, tries)))
}

fn parse_grid(input: &str) -> IResult<&str, Grid> {
    Ok((input, vec![[Letter::Correct; 5]]))
}
