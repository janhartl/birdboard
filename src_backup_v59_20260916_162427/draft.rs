use crate::player::PlayerId;
use crate::team::TeamId;

#[derive(Debug, Clone)]
pub struct DraftPick {
    pub player_id: PlayerId,
    pub team_id: TeamId,
    pub price: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DraftError {
    InvalidPlayer,
    InvalidTeam,
    InsufficientFunds,
    PlayerAlreadyDrafted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftMode {
    BrowsingPlayers,
    RecordingDraft,
    SearchingPlayer,
}
