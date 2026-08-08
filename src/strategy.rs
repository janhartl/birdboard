use crate::player::PlayerId;

use anyhow::{Context, Result};
use serde::Deserialize;
use std::fs;

#[derive(Debug, Deserialize)]
pub struct Build {
    pub id: String,
    pub title: String,
    pub required_players: Vec<PlayerId>,
    pub text: String,
    pub target_players: Vec<PlayerId>,
}

#[derive(Debug, Deserialize)]
struct BuildFile {
    builds: Vec<Build>,
}

#[derive(Debug, Deserialize)]
pub struct ReplacementGroup {
    pub id: String,
    pub title: String,
    pub primary_players: Vec<PlayerId>,
    pub left_label: String,
    pub right_label: String,
    pub alternatives: Vec<ReplacementOption>,
    pub rule: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReplacementOption {
    pub player_id: PlayerId,
    pub left: String,
    pub right: String,
}

#[derive(Debug, Deserialize)]
struct ReplacementFile {
    replacements: Vec<ReplacementGroup>,
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

pub fn load_replacements(path: &str) -> Result<Vec<ReplacementGroup>> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read replacement file: {path}"))?;

    let replacement_file: ReplacementFile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse replacement file: {path}"))?;

    Ok(replacement_file.replacements)
}

pub fn replacement_for_player(
    replacements: &[ReplacementGroup],
    player_id: PlayerId,
) -> Option<&ReplacementGroup> {
    replacements
        .iter()
        .find(|replacement| replacement.primary_players.contains(&player_id))
}
