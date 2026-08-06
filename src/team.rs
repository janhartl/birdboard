use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(transparent)]
pub struct TeamId(pub u32);

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FantasyTeam {
    pub id: TeamId,
    pub name: String,
    pub budget: u8,

    #[serde(default)]
    pub is_user: bool,
}
