use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::player::{Player, PlayerId};
use crate::strategy::{Build, ReplacementGroup};
use crate::team::{FantasyTeam, TeamId};

pub fn load_players(path: &str) -> Result<Vec<Player>, csv::Error> {
    let mut reader = csv::Reader::from_path(path)?;

    let mut players = Vec::new();

    for result in reader.deserialize::<Player>() {
        let player = result?;
        players.push(player);
    }

    Ok(players)
}

pub fn load_teams(path: &str) -> Result<Vec<FantasyTeam>, csv::Error> {
    let mut reader = csv::Reader::from_path(path)?;

    let mut teams = Vec::new();

    for result in reader.deserialize::<FantasyTeam>() {
        let team = result?;
        teams.push(team);
    }

    Ok(teams)
}
pub fn validate_data(
    players: &[Player],
    teams: &[FantasyTeam],
    builds: &[Build],
    replacements: &[ReplacementGroup],
) -> Result<()> {
    // Player IDs must be globally unique.
    let mut player_ids = HashSet::new();

    for player in players {
        if !player_ids.insert(player.id) {
            bail!(
                "duplicate player ID {:?} for player {:?}",
                player.id,
                player.name,
            );
        }
    }

    // Team IDs must be globally unique.
    let mut team_ids: HashSet<TeamId> = HashSet::new();

    for team in teams {
        if !team_ids.insert(team.id) {
            bail!("duplicate team ID {:?} for team {:?}", team.id, team.name,);
        }
    }

    // Validate build IDs and all player references inside builds.
    let mut build_ids = HashSet::new();

    for build in builds {
        if !build_ids.insert(build.id.as_str()) {
            bail!("duplicate build ID {:?}", build.id);
        }

        let mut required_players = HashSet::new();

        for player_id in &build.required_players {
            if !player_ids.contains(player_id) {
                bail!(
                    "build {:?} required_players references unknown player {:?}",
                    build.id,
                    player_id,
                );
            }

            if !required_players.insert(*player_id) {
                bail!(
                    "build {:?} contains duplicate required player {:?}",
                    build.id,
                    player_id,
                );
            }
        }

        let mut target_players = HashSet::new();

        for player_id in &build.target_players {
            if !player_ids.contains(player_id) {
                bail!(
                    "build {:?} target_players references unknown player {:?}",
                    build.id,
                    player_id,
                );
            }

            if !target_players.insert(*player_id) {
                bail!(
                    "build {:?} contains duplicate target player {:?}",
                    build.id,
                    player_id,
                );
            }
        }
    }

    // Track which matrix owns each primary player.
    // One player should not trigger two different matrices.
    let mut primary_player_owners: HashMap<PlayerId, &str> = HashMap::new();

    let mut replacement_ids = HashSet::new();

    for replacement in replacements {
        if !replacement_ids.insert(replacement.id.as_str()) {
            bail!("duplicate replacement ID {:?}", replacement.id,);
        }

        let mut primary_players = HashSet::new();

        for player_id in &replacement.primary_players {
            if !player_ids.contains(player_id) {
                bail!(
                    "replacement {:?} primary_players references unknown player {:?}",
                    replacement.id,
                    player_id,
                );
            }

            if !primary_players.insert(*player_id) {
                bail!(
                    "replacement {:?} contains duplicate primary player {:?}",
                    replacement.id,
                    player_id,
                );
            }

            if let Some(existing_replacement) = primary_player_owners.get(player_id) {
                bail!(
                    "player {:?} is a primary player in both replacement {:?} and replacement {:?}",
                    player_id,
                    existing_replacement,
                    replacement.id,
                );
            }

            primary_player_owners.insert(*player_id, replacement.id.as_str());
        }

        let mut alternatives = HashSet::new();

        for alternative in &replacement.alternatives {
            let player_id = alternative.player_id;

            if !player_ids.contains(&player_id) {
                bail!(
                    "replacement {:?} alternative references unknown player {:?}",
                    replacement.id,
                    player_id,
                );
            }

            if !alternatives.insert(player_id) {
                bail!(
                    "replacement {:?} contains duplicate alternative player {:?}",
                    replacement.id,
                    player_id,
                );
            }

            if primary_players.contains(&player_id) {
                bail!(
                    "replacement {:?} includes primary player {:?} among its own alternatives",
                    replacement.id,
                    player_id,
                );
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::{load_builds, load_replacements};
    #[test]
    fn project_data_is_coherent() -> anyhow::Result<()> {
        let players = load_players("data/players.csv")?;
        let teams = load_teams("data/teams.csv")?;
        let builds = load_builds("data/builds.toml")?;
        let replacements = load_replacements("data/replacements.toml")?;

        validate_data(&players, &teams, &builds, &replacements)
    }
    #[test]
    fn duplicate_player_ids_are_rejected() {
        let players = vec![
            Player {
                id: PlayerId(0),
                name: String::from("Bird"),
                position: String::from("SF"),
                projected_value: 50,
                short_name: Some(String::from("Bird")),
            },
            Player {
                id: PlayerId(0),
                name: String::from("Larry"),
                position: String::from("SF"),
                projected_value: 50,
                short_name: Some(String::from("Larry")),
            },
        ];

        let result = validate_data(&players, &[], &[], &[]);

        assert!(result.is_err());

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("duplicate player ID")
        );
    }
}

pub fn save_players(path: impl AsRef<Path>, players: &[Player]) -> Result<()> {
    let path = path.as_ref();

    // players.csv -> players.csv.tmp
    let temporary_path = path.with_extension("csv.tmp");

    let save_result = (|| -> Result<()> {
        let mut writer = csv::WriterBuilder::new()
            .has_headers(true)
            .from_path(&temporary_path)
            .with_context(|| {
                format!(
                    "failed to create temporary player file: {}",
                    temporary_path.display(),
                )
            })?;

        for player in players {
            writer
                .serialize(player)
                .with_context(|| format!("failed to serialize player {:?}", player.name,))?;
        }

        writer.flush().with_context(|| {
            format!("failed to flush player file: {}", temporary_path.display(),)
        })?;

        // Close the file before renaming it.
        drop(writer);

        fs::rename(&temporary_path, path).with_context(|| {
            format!(
                "failed to replace {} with saved player data",
                path.display(),
            )
        })?;

        Ok(())
    })();

    // Do not leave a broken temporary file behind.
    if save_result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }

    save_result
}
