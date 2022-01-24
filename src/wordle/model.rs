//! Wordle score model.

#[derive(Debug, Clone, bincode::Encode, bincode::Decode)]
pub struct Score {
    pub day: u32,
    pub tries: u8,
    pub grid: Grid,
}

pub type Grid = Vec<Vec<Letter>>;

#[derive(Debug, Clone, Copy, bincode::Encode, bincode::Decode)]
pub enum Letter {
    Correct,
    Partial,
    Incorrect,
}
