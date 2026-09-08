use std::{
    fs,
    path::{Path, PathBuf},
    thread::sleep,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Local, NaiveDate, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::player::PlayerId;

const NBA_STATS_URL: &str = concat!("https://", "api.server.nbaapi.com", "/api/playertotals");

const SCHEMA_VERSION: u32 = 1;

// July is a natural rollover point for a fantasy-draft application.
// Jul 2026 -> 2026-27 draft season
// Jan 2027 -> still 2026-27 draft season
const SEASON_ROLLOVER_MONTH: u32 = 7;

const STATS_FILE: &str = "player_9cat.csv";
const METADATA_FILE: &str = "metadata.json";

// -----------------------------------------------------------------------------
// Public types
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeasonContext {
    pub draft_season: String,
    pub source_season: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerNineCatStats {
    pub player_id: PlayerId,
    pub player_name: String,
    pub team: String,
    pub games: u32,

    pub minutes_pg: f64,

    // Percentage categories
    //
    // Keep BOTH percentage and shooting volume.
    // This is important for Durant/H2H calculations.
    pub fgm_pg: f64,
    pub fga_pg: f64,
    pub fg_pct: f64,

    pub ftm_pg: f64,
    pub fta_pg: f64,
    pub ft_pct: f64,

    // Makes/attempts from the complete season are also preserved.
    pub fgm_total: f64,
    pub fga_total: f64,
    pub ftm_total: f64,
    pub fta_total: f64,

    // Counting categories
    pub threes_pg: f64,
    pub points_pg: f64,
    pub rebounds_pg: f64,
    pub assists_pg: f64,
    pub steals_pg: f64,
    pub blocks_pg: f64,
    pub turnovers_pg: f64,
}

#[derive(Debug)]
pub struct StatsBundle {
    pub draft_season: String,
    pub source_season: String,
    pub cache_dir: PathBuf,
    pub players: Vec<PlayerNineCatStats>,
}

// -----------------------------------------------------------------------------
// Public entry point
// -----------------------------------------------------------------------------

pub fn load_or_fetch() -> Result<StatsBundle> {
    let today = Local::now().date_naive();
    let season = season_context_for(today);

    let stats_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join("stats");

    let cache_dir = stats_root.join(&season.draft_season);

    let players = if cache_is_valid(&cache_dir, &season) {
        load_cached_stats(&cache_dir)?
    } else {
        let players = fetch_nba_stats(&season.source_season)?;

        if players.is_empty() {
            bail!(
                "NBA returned no player statistics for {}",
                season.source_season
            );
        }

        write_cache(&cache_dir, &season, &players)?;

        players
    };

    Ok(StatsBundle {
        draft_season: season.draft_season,
        source_season: season.source_season,
        cache_dir,
        players,
    })
}

// -----------------------------------------------------------------------------
// Season detection
// -----------------------------------------------------------------------------

pub fn season_context_for(date: NaiveDate) -> SeasonContext {
    let draft_start_year = if date.month() >= SEASON_ROLLOVER_MONTH {
        date.year()
    } else {
        date.year() - 1
    };

    let source_start_year = draft_start_year - 1;

    SeasonContext {
        draft_season: season_string(draft_start_year),
        source_season: season_string(source_start_year),
    }
}

fn season_string(start_year: i32) -> String {
    format!("{}-{:02}", start_year, (start_year + 1) % 100)
}

// -----------------------------------------------------------------------------
// Cache
// -----------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct CacheMetadata {
    schema_version: u32,
    draft_season: String,
    source_season: String,
    season_type: String,
    per_mode: String,
    provider: String,
    endpoint: String,
    generated_at_utc: String,
    player_count: usize,
}

fn cache_is_valid(cache_dir: &Path, season: &SeasonContext) -> bool {
    let csv_path = cache_dir.join(STATS_FILE);
    let metadata_path = cache_dir.join(METADATA_FILE);

    if !csv_path.exists() || !metadata_path.exists() {
        return false;
    }

    let Ok(metadata_string) = fs::read_to_string(metadata_path) else {
        return false;
    };

    let Ok(metadata) = serde_json::from_str::<CacheMetadata>(&metadata_string) else {
        return false;
    };

    metadata.schema_version == SCHEMA_VERSION
        && metadata.draft_season == season.draft_season
        && metadata.source_season == season.source_season
        && metadata.player_count > 0
}

fn load_cached_stats(cache_dir: &Path) -> Result<Vec<PlayerNineCatStats>> {
    let path = cache_dir.join(STATS_FILE);

    let mut reader =
        csv::Reader::from_path(&path).with_context(|| format!("opening {}", path.display()))?;

    let players = reader
        .deserialize()
        .collect::<std::result::Result<Vec<PlayerNineCatStats>, _>>()
        .with_context(|| format!("reading {}", path.display()))?;

    Ok(players)
}

fn write_cache(
    cache_dir: &Path,
    season: &SeasonContext,
    players: &[PlayerNineCatStats],
) -> Result<()> {
    fs::create_dir_all(cache_dir)
        .with_context(|| format!("creating stats directory {}", cache_dir.display()))?;

    // Write to temporary files first so an interrupted download/write
    // cannot leave a cache that appears complete.
    let csv_path = cache_dir.join(STATS_FILE);
    let csv_tmp = cache_dir.join(format!("{STATS_FILE}.tmp"));

    {
        let mut writer = csv::Writer::from_path(&csv_tmp)?;

        for player in players {
            writer.serialize(player)?;
        }

        writer.flush()?;
    }

    fs::rename(&csv_tmp, &csv_path)?;

    let metadata = CacheMetadata {
        schema_version: SCHEMA_VERSION,
        draft_season: season.draft_season.clone(),
        source_season: season.source_season.clone(),
        season_type: "Regular Season".to_string(),
        per_mode: "Totals".to_string(),
        provider: "nbaapi.com".to_string(),
        endpoint: NBA_STATS_URL.to_string(),
        generated_at_utc: Utc::now().to_rfc3339(),
        player_count: players.len(),
    };

    let metadata_path = cache_dir.join(METADATA_FILE);
    let metadata_tmp = cache_dir.join(format!("{METADATA_FILE}.tmp"));

    let contents = serde_json::to_vec_pretty(&metadata)?;

    fs::write(&metadata_tmp, contents)?;
    fs::rename(&metadata_tmp, &metadata_path)?;

    Ok(())
}

// -----------------------------------------------------------------------------
// NBA stats provider
// -----------------------------------------------------------------------------

fn fetch_nba_stats(source_season: &str) -> Result<Vec<PlayerNineCatStats>> {
    let season = season_end_year(source_season)?;

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("building NBA statistics HTTP client")?;

    let mut page = 1;
    let mut raw_players = Vec::new();

    loop {
        let response = request_with_retry(&client, season, page)?;

        let total_pages = response.pagination.pages;

        raw_players.extend(
            response
                .data
                .into_iter()
                .filter(|player| !player.is_playoff),
        );

        if page >= total_pages {
            break;
        }

        page += 1;
    }

    normalize_response(raw_players)
}

fn request_with_retry(client: &Client, season: i32, page: u32) -> Result<ApiResponse> {
    let mut last_error = None;

    for attempt in 1..=3 {
        match request_nba_stats(client, season, page) {
            Ok(response) => return Ok(response),

            Err(error) => {
                last_error = Some(error);

                if attempt < 3 {
                    sleep(Duration::from_secs(attempt * 2));
                }
            }
        }
    }

    Err(last_error.unwrap())
}

fn request_nba_stats(client: &Client, season: i32, page: u32) -> Result<ApiResponse> {
    let season = season.to_string();
    let page = page.to_string();

    let params = [
        ("season", season.as_str()),
        ("isPlayoff", "false"),
        ("page", page.as_str()),
        ("pageSize", "100"),
    ];

    client
        .get(NBA_STATS_URL)
        .query(&params)
        .send()
        .context("requesting NBA statistics")?
        .error_for_status()
        .context("NBA statistics provider returned an HTTP error")?
        .json::<ApiResponse>()
        .context("decoding NBA statistics response")
}

// -----------------------------------------------------------------------------
// Provider response
// -----------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ApiResponse {
    data: Vec<ApiPlayer>,
    pagination: Pagination,
}

#[derive(Debug, Deserialize)]
struct Pagination {
    pages: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiPlayer {
    player_id: String,
    player_name: String,

    games: u32,

    // Despite its name this provider currently returns
    // season-total minutes here.
    minutes_pg: f64,

    field_goals: f64,
    field_attempts: f64,

    three_fg: f64,

    ft: f64,
    ft_attempts: f64,

    total_rb: f64,
    assists: f64,
    steals: f64,
    blocks: f64,
    turnovers: f64,
    points: f64,

    team: String,
    is_playoff: bool,
}

// -----------------------------------------------------------------------------
// Provider -> BirdBoard normalization
// -----------------------------------------------------------------------------

fn normalize_response(rows: Vec<ApiPlayer>) -> Result<Vec<PlayerNineCatStats>> {
    use std::collections::HashMap;

    let mut grouped: HashMap<String, Vec<ApiPlayer>> = HashMap::new();

    for player in rows {
        grouped
            .entry(player.player_id.clone())
            .or_default()
            .push(player);
    }

    let mut players = Vec::with_capacity(grouped.len());

    for (_, rows) in grouped {
        let player = combine_player_rows(rows);

        if player.games == 0 {
            continue;
        }

        players.push(PlayerNineCatStats {
            player_id: PlayerId(player.player_id),

            player_name: player.player_name,

            team: player.team,

            games: player.games,

            minutes_pg: per_game(player.minutes_pg, player.games),

            fgm_pg: per_game(player.field_goals, player.games),

            fga_pg: per_game(player.field_attempts, player.games),

            fg_pct: percentage(player.field_goals, player.field_attempts),

            ftm_pg: per_game(player.ft, player.games),

            fta_pg: per_game(player.ft_attempts, player.games),

            ft_pct: percentage(player.ft, player.ft_attempts),

            fgm_total: player.field_goals,
            fga_total: player.field_attempts,

            ftm_total: player.ft,
            fta_total: player.ft_attempts,

            threes_pg: per_game(player.three_fg, player.games),

            points_pg: per_game(player.points, player.games),

            rebounds_pg: per_game(player.total_rb, player.games),

            assists_pg: per_game(player.assists, player.games),

            steals_pg: per_game(player.steals, player.games),

            blocks_pg: per_game(player.blocks, player.games),

            turnovers_pg: per_game(player.turnovers, player.games),
        });
    }

    players.sort_by(|a, b| a.player_name.cmp(&b.player_name));

    Ok(players)
}

// -----------------------------------------------------------------------------
// Handle traded players
// -----------------------------------------------------------------------------

fn combine_player_rows(rows: Vec<ApiPlayer>) -> ApiPlayer {
    // Basketball-Reference style datasets generally provide
    // a TOT row for players who changed teams.
    //
    // If that exists, it already contains the correct
    // season totals and we should use it.

    if let Some(total) = rows.iter().find(|player| player.team == "TOT") {
        return total.clone();
    }

    if rows.len() == 1 {
        return rows.into_iter().next().unwrap();
    }

    // Defensive fallback:
    // If a traded player has only individual-team rows,
    // combine them ourselves.

    let mut iter = rows.into_iter();

    let mut total = iter.next().unwrap();

    total.team = "TOT".to_string();

    for player in iter {
        total.games += player.games;

        total.minutes_pg += player.minutes_pg;

        total.field_goals += player.field_goals;
        total.field_attempts += player.field_attempts;

        total.three_fg += player.three_fg;

        total.ft += player.ft;
        total.ft_attempts += player.ft_attempts;

        total.total_rb += player.total_rb;

        total.assists += player.assists;
        total.steals += player.steals;
        total.blocks += player.blocks;
        total.turnovers += player.turnovers;
        total.points += player.points;
    }

    total
}

// -----------------------------------------------------------------------------
// Season format conversion
// -----------------------------------------------------------------------------

fn season_end_year(season: &str) -> Result<i32> {
    let start_year = season
        .get(0..4)
        .context("invalid NBA season format")?
        .parse::<i32>()
        .context("invalid NBA season year")?;

    Ok(start_year + 1)
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

fn per_game(total: f64, games: u32) -> f64 {
    if games == 0 {
        0.0
    } else {
        total / games as f64
    }
}

fn percentage(made: f64, attempts: f64) -> f64 {
    if attempts == 0.0 {
        0.0
    } else {
        made / attempts
    }
}

fn number(row: &[Value], index: usize, column: &str) -> Result<f64> {
    row.get(index)
        .and_then(Value::as_f64)
        .with_context(|| format!("invalid numeric value in NBA column {column}"))
}

fn integer(row: &[Value], index: usize, column: &str) -> Result<u64> {
    let value = row
        .get(index)
        .with_context(|| format!("missing NBA column value {column}"))?;

    if let Some(value) = value.as_u64() {
        return Ok(value);
    }

    if let Some(value) = value.as_f64() {
        return Ok(value as u64);
    }

    bail!("invalid integer value in NBA column {column}")
}

fn text(row: &[Value], index: usize, column: &str) -> Result<String> {
    row.get(index)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .with_context(|| format!("invalid text value in NBA column {column}"))
}

fn text_or_empty(row: &[Value], index: usize) -> String {
    row.get(index)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_season_to_api_year() {
        assert_eq!(season_end_year("2025-26").unwrap(), 2026);

        assert_eq!(season_end_year("2026-27").unwrap(), 2027);
    }

    #[test]
    fn september_2026_uses_2025_26_as_source() {
        let date = NaiveDate::from_ymd_opt(2026, 9, 7).unwrap();

        let context = season_context_for(date);

        assert_eq!(context.draft_season, "2026-27");

        assert_eq!(context.source_season, "2025-26");
    }

    #[test]
    fn january_2027_is_still_2026_27_season() {
        let date = NaiveDate::from_ymd_opt(2027, 1, 15).unwrap();

        let context = season_context_for(date);

        assert_eq!(context.draft_season, "2026-27");

        assert_eq!(context.source_season, "2025-26");
    }

    #[test]
    fn july_rolls_into_new_draft_season() {
        let date = NaiveDate::from_ymd_opt(2027, 7, 1).unwrap();

        let context = season_context_for(date);

        assert_eq!(context.draft_season, "2027-28");

        assert_eq!(context.source_season, "2026-27");
    }

    #[test]
    fn percentage_uses_volume() {
        assert_eq!(percentage(40.0, 100.0), 0.4);
    }
}
