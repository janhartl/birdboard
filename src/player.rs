use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Player {
    pub name: String,
    pub position: String,
    pub projected_value: u8,
}
