use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct TeamId(pub u32);

#[derive(Debug, Clone, Deserialize)]
pub struct FantasyTeam {
    pub id: TeamId,
    pub name: String,
    pub budget: u8,
}
