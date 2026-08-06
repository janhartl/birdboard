use crate::player::PlayerId;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Build {
    pub id: String,
    pub title: String,
    pub required_players: Vec<PlayerId>,
    pub identity: String,
    pub sections: Vec<BuildSection>,
    pub target_players: Vec<PlayerId>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BuildSection {
    pub title: String,
    pub items: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BuildFile {
    builds: Vec<Build>,
}

#[derive(Serialize)]
struct BuildFileRef<'a> {
    builds: &'a [Build],
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReplacementGroup {
    pub id: String,
    pub title: String,
    pub primary_players: Vec<PlayerId>,
    pub left_label: String,
    pub right_label: String,
    pub alternatives: Vec<ReplacementOption>,
    pub rule: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReplacementOption {
    pub player_id: PlayerId,
    pub left: String,
    pub right: String,
}

#[derive(Debug, Deserialize)]
struct ReplacementFile {
    replacements: Vec<ReplacementGroup>,
}

#[derive(Serialize)]
struct ReplacementFileRef<'a> {
    replacements: &'a [ReplacementGroup],
}

pub fn load_builds(path: &str) -> Result<Vec<Build>> {
    let contents = fs::read_to_string(path)
        .with_context(|| format!("failed to read strategy file: {path}"))?;

    let build_file: BuildFile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse strategy file: {path}"))?;

    Ok(build_file.builds)
}

pub fn save_builds(path: impl AsRef<Path>, builds: &[Build]) -> Result<()> {
    let path = path.as_ref();
    let temporary_path = path.with_extension("toml.tmp");

    let contents =
        toml::to_string_pretty(&BuildFileRef { builds }).context("failed to serialize builds")?;

    fs::write(&temporary_path, contents).with_context(|| {
        format!(
            "failed to write temporary build file: {}",
            temporary_path.display(),
        )
    })?;

    fs::rename(&temporary_path, path)
        .with_context(|| format!("failed to replace {} with saved build data", path.display(),))?;

    Ok(())
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

pub fn save_replacements(path: impl AsRef<Path>, replacements: &[ReplacementGroup]) -> Result<()> {
    let path = path.as_ref();
    let temporary_path = path.with_extension("toml.tmp");

    let contents = toml::to_string_pretty(&ReplacementFileRef { replacements })
        .context("failed to serialize replacement matrices")?;

    fs::write(&temporary_path, contents).with_context(|| {
        format!(
            "failed to write temporary replacement file: {}",
            temporary_path.display(),
        )
    })?;

    fs::rename(&temporary_path, path).with_context(|| {
        format!(
            "failed to replace {} with saved replacement data",
            path.display(),
        )
    })?;

    Ok(())
}

pub fn replacement_for_player(
    replacements: &[ReplacementGroup],
    player_id: PlayerId,
) -> Option<&ReplacementGroup> {
    replacements
        .iter()
        .find(|replacement| replacement.primary_players.contains(&player_id))
}
