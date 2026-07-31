use crate::player::PlayerId;

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;

#[derive(Debug, Deserialize)]
pub struct Build {
    pub id: String,
    pub title: String,
    pub required_players: Vec<PlayerId>,
    pub identity: String,
    pub sections: Vec<BuildSection>,
    pub target_players: Vec<PlayerId>,
}

#[derive(Debug, Deserialize)]
pub struct BuildSection {
    pub title: String,
    pub items: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BuildFile {
    builds: Vec<Build>,
}

pub fn load_builds(path: &str) -> Result<Vec<Build>> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read strategy file: {path}"))?;

    let build_file: BuildFile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse strategy file: {path}"))?;

    Ok(build_file.builds)
}

pub fn active_build<F>(builds: &[Build], owns_player: F) -> Option<&Build>
where
    F: Fn(PlayerId) -> bool,
{
    builds
        .iter()
        .filter(|build| {
            build
                .required_players
                .iter()
                .all(|player_id| owns_player(*player_id))
        })
        .max_by_key(|build| build.required_players.len())
}
