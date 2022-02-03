//! Wordle score model.

use chrono::{DateTime, Duration, Utc};

#[derive(Debug, bincode::Encode, bincode::Decode)]
pub struct TimestampedScore {
    pub timestamp: i64,
    pub score: Score,
}

#[derive(Debug, PartialEq, PartialOrd, bincode::Encode, bincode::Decode)]
pub struct Score {
    pub day: u32,
    pub success: bool,
    pub guesses: u8,
    pub hard_mode: bool,
    pub grid: Grid,
}

// Uses `Vec`s to simplify parsing.
// FIX: https://docs.rs/nom/*/nom/multi/fn.fill.html
pub type Grid = Vec<Vec<Letter>>;

#[derive(Debug, PartialEq, PartialOrd, bincode::Encode, bincode::Decode)]
pub enum Letter {
    Correct,
    Partial,
    Incorrect,
}

pub fn day_to_datetime(day: u32) -> DateTime<Utc> {
    *crate::WORDLE_DAY1 + Duration::days(i64::from(day - 1))
}
