use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(transparent)]
pub struct PlayerId(pub u32);

#[derive(Debug, Clone, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,
    pub position: String,
    pub projected_value: u8,
}
