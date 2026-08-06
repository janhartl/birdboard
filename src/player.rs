use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(transparent)]
pub struct PlayerId(pub u32);

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Player {
    pub id: PlayerId,
    pub name: String,

    #[serde(default)]
    pub short_name: Option<String>,

    pub position: String,
    pub projected_value: u8,
}

impl Player {
    pub fn display_name(&self) -> &str {
        self.short_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(&self.name)
    }
}
