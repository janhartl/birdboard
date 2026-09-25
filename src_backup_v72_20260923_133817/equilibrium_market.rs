use crate::player::PlayerId;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub const EQUILIBRIUM_MARKET_FILENAME: &str = "equilibrium_prices.csv";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquilibriumMarketRow {
    pub player_id: String,
    pub player_name: String,
    /// Unconditional synthetic clearing value used as the live market weight:
    /// $1 + sale_rate * (conditional_mean_sale - $1).
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
        prices.insert(
            PlayerId(row.player_id.clone()),
            row.equilibrium_price.max(1.0),
        );
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
