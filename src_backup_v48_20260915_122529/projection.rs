use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::player::PlayerId;
use crate::stats::StatsBundle;

pub const MANUAL_OVERRIDES_PATH: &str = "data/projections/manual_overrides.csv";

#[derive(Debug, Clone)]
pub struct PlayerProjection {
    pub player_id: PlayerId,
    pub player_name: String,
    pub team: String,
    pub minutes_pg: f64,
    pub fgm_pg: f64,
    pub fga_pg: f64,
    pub ftm_pg: f64,
    pub fta_pg: f64,
    pub threes_pg: f64,
    pub points_pg: f64,
    pub rebounds_pg: f64,
    pub assists_pg: f64,
    pub steals_pg: f64,
    pub blocks_pg: f64,
    pub turnovers_pg: f64,
    pub is_manual_override: bool,
}

impl PlayerProjection {
    pub fn fg_pct(&self) -> f64 {
        if self.fga_pg <= f64::EPSILON {
            0.0
        } else {
            self.fgm_pg / self.fga_pg
        }
    }

    pub fn ft_pct(&self) -> f64 {
        if self.fta_pg <= f64::EPSILON {
            0.0
        } else {
            self.ftm_pg / self.fta_pg
        }
    }
}

#[derive(Debug)]
pub struct ProjectionBook {
    entries: HashMap<PlayerId, PlayerProjection>,
    override_path: PathBuf,
}

impl ProjectionBook {
    pub fn empty() -> Self {
        Self {
            entries: HashMap::new(),
            override_path: PathBuf::from(MANUAL_OVERRIDES_PATH),
        }
    }

    pub fn from_stats(stats: &StatsBundle) -> Result<Self> {
        let mut entries = stats
            .players
            .iter()
            .map(|player| {
                (
                    player.player_id.clone(),
                    PlayerProjection {
                        player_id: player.player_id.clone(),
                        player_name: player.player_name.clone(),
                        team: player.team.clone(),
                        minutes_pg: player.minutes_pg,
                        fgm_pg: player.fgm_pg,
                        fga_pg: player.fga_pg,
                        ftm_pg: player.ftm_pg,
                        fta_pg: player.fta_pg,
                        threes_pg: player.threes_pg,
                        points_pg: player.points_pg,
                        rebounds_pg: player.rebounds_pg,
                        assists_pg: player.assists_pg,
                        steals_pg: player.steals_pg,
                        blocks_pg: player.blocks_pg,
                        turnovers_pg: player.turnovers_pg,
                        is_manual_override: false,
                    },
                )
            })
            .collect::<HashMap<_, _>>();

        let override_path = PathBuf::from(MANUAL_OVERRIDES_PATH);
        let canonical_by_name = stats
            .players
            .iter()
            .map(|player| {
                (
                    normalized_player_name(&player.player_name),
                    player.player_id.clone(),
                )
            })
            .collect::<HashMap<_, _>>();

        // Older overrides may still carry the legacy ID that players.csv used
        // before the stats pipeline adopted canonical player IDs. Resolve by
        // full name so an override replaces the historical seed instead of
        // creating a second Tatum/Haliburton/etc.
        for mut row in load_override_rows(&override_path)? {
            if !row.enabled {
                continue;
            }

            if let Some(canonical_id) =
                canonical_by_name.get(&normalized_player_name(&row.player_name))
            {
                row.player_id = canonical_id.clone();
            }

            validate_override_row(&row)?;
            entries.insert(row.player_id.clone(), row.into_projection());
        }

        Ok(Self {
            entries,
            override_path,
        })
    }

    pub fn for_player(&self, player_id: &PlayerId) -> Option<&PlayerProjection> {
        self.entries.get(player_id)
    }

    pub fn save_manual_override(&mut self, projection: &PlayerProjection) -> Result<()> {
        validate_projection(projection)?;

        let mut rows = load_override_rows(&self.override_path)?;
        let replacement = OverrideRow::from_projection(projection);
        let replacement_name = normalized_player_name(&replacement.player_name);

        // Also collapse legacy-ID duplicates by player name while saving.
        rows.retain(|row| {
            row.player_id != projection.player_id
                && normalized_player_name(&row.player_name) != replacement_name
        });
        rows.push(replacement);

        if let Some(parent) = self.override_path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let mut writer = csv::Writer::from_path(&self.override_path)
            .with_context(|| format!("failed to create {}", self.override_path.display()))?;

        for row in &rows {
            writer.serialize(row)?;
        }
        writer.flush()?;

        let mut stored = projection.clone();
        stored.is_manual_override = true;
        self.entries.insert(stored.player_id.clone(), stored);

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OverrideRow {
    #[serde(default = "default_true")]
    enabled: bool,
    player_id: PlayerId,
    player_name: String,
    #[serde(default)]
    team: String,
    #[serde(default)]
    minutes_pg: f64,
    fgm_pg: f64,
    fga_pg: f64,
    ftm_pg: f64,
    fta_pg: f64,
    threes_pg: f64,
    points_pg: f64,
    rebounds_pg: f64,
    assists_pg: f64,
    steals_pg: f64,
    blocks_pg: f64,
    turnovers_pg: f64,
    #[serde(default)]
    note: String,
    #[serde(default)]
    source: String,
}

impl OverrideRow {
    fn into_projection(self) -> PlayerProjection {
        PlayerProjection {
            player_id: self.player_id,
            player_name: self.player_name,
            team: self.team,
            minutes_pg: self.minutes_pg,
            fgm_pg: self.fgm_pg,
            fga_pg: self.fga_pg,
            ftm_pg: self.ftm_pg,
            fta_pg: self.fta_pg,
            threes_pg: self.threes_pg,
            points_pg: self.points_pg,
            rebounds_pg: self.rebounds_pg,
            assists_pg: self.assists_pg,
            steals_pg: self.steals_pg,
            blocks_pg: self.blocks_pg,
            turnovers_pg: self.turnovers_pg,
            is_manual_override: true,
        }
    }

    fn from_projection(projection: &PlayerProjection) -> Self {
        Self {
            enabled: true,
            player_id: projection.player_id.clone(),
            player_name: projection.player_name.clone(),
            team: projection.team.clone(),
            minutes_pg: projection.minutes_pg,
            fgm_pg: projection.fgm_pg,
            fga_pg: projection.fga_pg,
            ftm_pg: projection.ftm_pg,
            fta_pg: projection.fta_pg,
            threes_pg: projection.threes_pg,
            points_pg: projection.points_pg,
            rebounds_pg: projection.rebounds_pg,
            assists_pg: projection.assists_pg,
            steals_pg: projection.steals_pg,
            blocks_pg: projection.blocks_pg,
            turnovers_pg: projection.turnovers_pg,
            note: "Edited in BirdBoard".to_string(),
            source: "BirdBoard manual override".to_string(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn normalized_player_name(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn load_override_rows(path: &Path) -> Result<Vec<OverrideRow>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)?;

    reader
        .deserialize::<OverrideRow>()
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn validate_override_row(row: &OverrideRow) -> Result<()> {
    validate_projection(&row.clone().into_projection())
}

fn validate_projection(projection: &PlayerProjection) -> Result<()> {
    if projection.player_id.0.trim().is_empty() {
        bail!("projection has an empty player id");
    }

    for (name, value) in [
        ("minutes", projection.minutes_pg),
        ("FGM", projection.fgm_pg),
        ("FGA", projection.fga_pg),
        ("FTM", projection.ftm_pg),
        ("FTA", projection.fta_pg),
        ("3PM", projection.threes_pg),
        ("PTS", projection.points_pg),
        ("REB", projection.rebounds_pg),
        ("AST", projection.assists_pg),
        ("STL", projection.steals_pg),
        ("BLK", projection.blocks_pg),
        ("TO", projection.turnovers_pg),
    ] {
        if !value.is_finite() || value < 0.0 {
            bail!(
                "{} has invalid {} projection: {}",
                projection.player_name,
                name,
                value
            );
        }
    }

    if projection.fgm_pg > projection.fga_pg + f64::EPSILON {
        bail!("{} has FGM > FGA", projection.player_name);
    }

    if projection.ftm_pg > projection.fta_pg + f64::EPSILON {
        bail!("{} has FTM > FTA", projection.player_name);
    }

    Ok(())
}
