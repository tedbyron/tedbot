//! Wordle score model.

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, bincode::Encode, bincode::Decode)]
pub struct Score {
    pub day: u32,
    pub success: bool,
    pub tries: u8,
    pub hard_mode: bool,
    pub grid: Grid,
}

pub type Grid = Vec<Vec<Letter>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, bincode::Encode, bincode::Decode)]
pub enum Letter {
    Correct,
    Partial,
    Incorrect,
}
