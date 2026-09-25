use crate::player::PlayerId;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const EQUILIBRIUM_MARKET_FILENAME: &str = "equilibrium_prices.csv";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquilibriumMarketRow {
    pub player_id: String,
    pub player_name: String,
    /// Independent rational-bid clearing value used as the live market demand
    /// weight.  Normal BirdBoard rescales these relative weights to the actual
    /// room's remaining discretionary dollars after every real pick.
    pub equilibrium_price: f64,
    pub mean_price: f64,
    pub median_price: f64,
    pub p25_price: f64,
    pub p75_price: f64,
    pub sale_rate: f64,
    pub observations: usize,
    pub rooms: usize,
}

#[derive(Debug, Clone)]
pub struct EquilibriumMarket {
    pub season: String,
    pub rows: Vec<EquilibriumMarketRow>,
    pub prices: HashMap<PlayerId, f64>,
}

pub fn path_for_season(season: &str) -> PathBuf {
    Path::new("data")
        .join("market")
        .join(season)
        .join(EQUILIBRIUM_MARKET_FILENAME)
}

pub fn load_for_season(season: &str) -> Result<Option<EquilibriumMarket>> {
    let path = path_for_season(season);
    if !path.exists() {
        return Ok(None);
    }

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(&path)
        .with_context(|| format!("failed to open equilibrium market {}", path.display()))?;

    let mut rows = Vec::<EquilibriumMarketRow>::new();
    let mut prices = HashMap::<PlayerId, f64>::new();
    for row in reader.deserialize::<EquilibriumMarketRow>() {
        let row = row.with_context(|| {
            format!(
                "failed to parse equilibrium market row in {}",
                path.display()
            )
        })?;

        if row.player_id.trim().is_empty() {
            bail!(
                "equilibrium market {} contains an empty player_id",
                path.display()
            );
        }
        if !row.equilibrium_price.is_finite() || row.equilibrium_price < 1.0 {
            bail!(
                "equilibrium market {} contains invalid price {} for {}",
                path.display(),
                row.equilibrium_price,
                row.player_id
            );
        }

        let player_id = PlayerId(row.player_id.clone());
        if prices.insert(player_id, row.equilibrium_price).is_some() {
            bail!(
                "equilibrium market {} contains duplicate player_id {}",
                path.display(),
                row.player_id
            );
        }
        rows.push(row);
    }

    if rows.is_empty() {
        return Ok(None);
    }

    Ok(Some(EquilibriumMarket {
        season: season.to_string(),
        rows,
        prices,
    }))
}

pub fn save_for_season(season: &str, rows: &[EquilibriumMarketRow]) -> Result<PathBuf> {
    let path = path_for_season(season);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let mut writer = csv::WriterBuilder::new()
        .has_headers(true)
        .from_path(&path)
        .with_context(|| format!("failed to create equilibrium market {}", path.display()))?;
    for row in rows {
        writer.serialize(row)?;
    }
    writer.flush()?;
    Ok(path)
}
