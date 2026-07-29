#[derive(Debug, Clone)]
pub struct DraftPick {
    pub player_index: usize,
    pub team_index: usize,
    pub price: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DraftError {
    InvalidPlayer,
    InvalidTeam,
    InsufficientFunds,
    PlayerAlreadyDrafted,
}
