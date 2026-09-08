use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    thread::sleep,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::player::PlayerId;

const NBA_STATS_URL: &str = concat!("https://", "api.server.nbaapi.com", "/api/playertotals");
const PBP_TOTALS_URL: &str = concat!("https://", "api.pbpstats.com", "/get-totals/nba");
const PBP_GAME_LOGS_URL: &str = concat!("https://", "api.pbpstats.com", "/get-game-logs/nba");

const SCHEMA_VERSION: u32 = 3;

// July is a natural rollover point for a fantasy-draft application.
// Jul 2026 -> 2026-27 draft season
// Jan 2027 -> still 2026-27 draft season
const SEASON_ROLLOVER_MONTH: u32 = 7;

const STATS_FILE: &str = "player_9cat.csv";
const WEEKLY_FILE: &str = "player_weekly.csv";
const METADATA_FILE: &str = "metadata.json";

// Weekly game logs are only needed to estimate the scoring-period variance used
// by the Rosenof/Durant valuation layer. Fetch a generous fantasy-relevant
// buffer rather than every NBA player; durant.rs can later choose the exact
// 169-player reference population for a 13 x 13 league.
const WEEKLY_REFERENCE_POOL_SIZE: usize = 220;
const MIN_REQUIRED_WEEKLY_PLAYERS: usize = 169;
const MIN_SELECTOR_GAMES: u32 = 10;
const MIN_SELECTOR_MINUTES_PG: f64 = 12.0;
const PBP_REQUEST_PAUSE_MS: u64 = 250;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerWeeklyStats {
    pub player_id: PlayerId,
    pub player_name: String,
    pub week: u32,
    pub games: u32,

    pub fgm: f64,
    pub fga: f64,
    pub ftm: f64,
    pub fta: f64,

    pub threes: f64,
    pub points: f64,
    pub rebounds: f64,
    pub assists: f64,
    pub steals: f64,
    pub blocks: f64,
    pub turnovers: f64,
}

#[derive(Debug)]
pub struct StatsBundle {
    pub draft_season: String,
    pub source_season: String,
    pub cache_dir: PathBuf,
    pub players: Vec<PlayerNineCatStats>,
    pub weekly: Vec<PlayerWeeklyStats>,
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

    let (players, weekly) = if cache_is_valid(&cache_dir, &season) {
        (
            load_cached_stats(&cache_dir)?,
            load_cached_weekly_stats(&cache_dir)?,
        )
    } else {
        let players = fetch_nba_stats(&season.source_season)?;

        if players.is_empty() {
            bail!(
                "NBA returned no player statistics for {}",
                season.source_season
            );
        }

        let weekly = fetch_weekly_stats(&season.source_season, &players)?;

        if weekly.is_empty() {
            bail!(
                "NBA returned no weekly player statistics for {}",
                season.source_season
            );
        }

        write_cache(&cache_dir, &season, &players, &weekly)?;

        (players, weekly)
    };

    Ok(StatsBundle {
        draft_season: season.draft_season,
        source_season: season.source_season,
        cache_dir,
        players,
        weekly,
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
    weekly_provider: String,
    weekly_endpoint: String,
    generated_at_utc: String,
    player_count: usize,
    weekly_count: usize,
}

fn cache_is_valid(cache_dir: &Path, season: &SeasonContext) -> bool {
    let csv_path = cache_dir.join(STATS_FILE);
    let weekly_path = cache_dir.join(WEEKLY_FILE);
    let metadata_path = cache_dir.join(METADATA_FILE);

    if !csv_path.exists() || !weekly_path.exists() || !metadata_path.exists() {
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
        && metadata.weekly_count > 0
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

fn load_cached_weekly_stats(cache_dir: &Path) -> Result<Vec<PlayerWeeklyStats>> {
    let path = cache_dir.join(WEEKLY_FILE);

    let mut reader =
        csv::Reader::from_path(&path).with_context(|| format!("opening {}", path.display()))?;

    let weekly = reader
        .deserialize()
        .collect::<std::result::Result<Vec<PlayerWeeklyStats>, _>>()
        .with_context(|| format!("reading {}", path.display()))?;

    Ok(weekly)
}

fn write_cache(
    cache_dir: &Path,
    season: &SeasonContext,
    players: &[PlayerNineCatStats],
    weekly: &[PlayerWeeklyStats],
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

    let weekly_path = cache_dir.join(WEEKLY_FILE);
    let weekly_tmp = cache_dir.join(format!("{WEEKLY_FILE}.tmp"));

    {
        let mut writer = csv::Writer::from_path(&weekly_tmp)?;

        for row in weekly {
            writer.serialize(row)?;
        }

        writer.flush()?;
    }

    fs::rename(&weekly_tmp, &weekly_path)?;

    let metadata = CacheMetadata {
        schema_version: SCHEMA_VERSION,
        draft_season: season.draft_season.clone(),
        source_season: season.source_season.clone(),
        season_type: "Regular Season".to_string(),
        per_mode: "Totals".to_string(),
        provider: "nbaapi.com".to_string(),
        endpoint: NBA_STATS_URL.to_string(),
        weekly_provider: "pbpstats.com".to_string(),
        weekly_endpoint: PBP_GAME_LOGS_URL.to_string(),
        generated_at_utc: Utc::now().to_rfc3339(),
        player_count: players.len(),
        weekly_count: weekly.len(),
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
// PBP Stats weekly game-log provider
// -----------------------------------------------------------------------------

#[derive(Debug)]
struct PbpPlayerRef {
    entity_id: String,
    name: String,
}

#[derive(Debug)]
struct RawGameLog {
    player_id: PlayerId,
    player_name: String,
    game_date: NaiveDate,
    fgm: f64,
    fga: f64,
    ftm: f64,
    fta: f64,
    threes: f64,
    points: f64,
    rebounds: f64,
    assists: f64,
    steals: f64,
    blocks: f64,
    turnovers: f64,
}

#[derive(Debug, Default)]
struct WeeklyAccumulator {
    player_name: String,
    games: u32,
    fgm: f64,
    fga: f64,
    ftm: f64,
    fta: f64,
    threes: f64,
    points: f64,
    rebounds: f64,
    assists: f64,
    steals: f64,
    blocks: f64,
    turnovers: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct SelectorMeans {
    points: f64,
    threes: f64,
    rebounds: f64,
    assists: f64,
    steals: f64,
    blocks: f64,
    turnovers: f64,
    fg_impact: f64,
    ft_impact: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct SelectorStdDevs {
    points: f64,
    threes: f64,
    rebounds: f64,
    assists: f64,
    steals: f64,
    blocks: f64,
    turnovers: f64,
    fg_impact: f64,
    ft_impact: f64,
}

fn fetch_weekly_stats(
    source_season: &str,
    players: &[PlayerNineCatStats],
) -> Result<Vec<PlayerWeeklyStats>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36")
        .build()
        .context("building PBP Stats HTTP client")?;

    let selected = select_weekly_reference_players(players);

    if selected.len() < MIN_REQUIRED_WEEKLY_PLAYERS {
        bail!(
            "only {} players qualified for weekly-data selection; need at least {}",
            selected.len(),
            MIN_REQUIRED_WEEKLY_PLAYERS
        );
    }

    let pbp_players = fetch_pbp_player_index(&client, source_season)?;
    let matched = match_pbp_players(&selected, &pbp_players);

    if matched.len() < MIN_REQUIRED_WEEKLY_PLAYERS {
        bail!(
            "matched only {} fantasy-relevant players to PBP Stats; need at least {}",
            matched.len(),
            MIN_REQUIRED_WEEKLY_PLAYERS
        );
    }

    eprintln!(
        "Fetching {} PBP Stats player game logs for {} (first run only)...",
        matched.len(),
        source_season
    );

    let mut raw_logs = Vec::new();
    let mut successful_players = 0usize;

    for (index, (player, entity_id)) in matched.iter().enumerate() {
        match request_pbp_game_logs_with_retry(&client, source_season, entity_id, player) {
            Ok(mut logs) if !logs.is_empty() => {
                successful_players += 1;
                raw_logs.append(&mut logs);
            }
            Ok(_) => {
                eprintln!(
                    "warning: PBP Stats returned no game logs for {} ({})",
                    player.player_name, entity_id
                );
            }
            Err(error) => {
                eprintln!(
                    "warning: skipping PBP Stats game logs for {} ({}): {:#}",
                    player.player_name, entity_id, error
                );
            }
        }

        if (index + 1) % 25 == 0 || index + 1 == matched.len() {
            eprintln!(
                "  weekly data: {}/{} players checked, {} successful",
                index + 1,
                matched.len(),
                successful_players
            );
        }

        if index + 1 < matched.len() {
            sleep(Duration::from_millis(PBP_REQUEST_PAUSE_MS));
        }
    }

    if successful_players < MIN_REQUIRED_WEEKLY_PLAYERS {
        bail!(
            "PBP Stats produced usable game logs for only {} players; need at least {}",
            successful_players,
            MIN_REQUIRED_WEEKLY_PLAYERS
        );
    }

    aggregate_game_logs(raw_logs)
}

fn fetch_pbp_player_index(client: &Client, source_season: &str) -> Result<Vec<PbpPlayerRef>> {
    let params = [
        ("Season", source_season),
        ("SeasonType", "Regular Season"),
        ("Type", "Player"),
    ];

    let value =
        request_json_with_retry(client, PBP_TOTALS_URL, &params, "PBP Stats player totals")?;

    let rows = value
        .get("multi_row_table_data")
        .and_then(Value::as_array)
        .context("PBP Stats totals response is missing multi_row_table_data")?;

    let mut players = Vec::with_capacity(rows.len());

    for row in rows {
        let Some(name) = row.get("Name").and_then(Value::as_str) else {
            continue;
        };

        let Some(entity_id) = value_as_id(row.get("EntityId")) else {
            continue;
        };

        players.push(PbpPlayerRef {
            entity_id,
            name: name.to_owned(),
        });
    }

    if players.is_empty() {
        bail!("PBP Stats returned no player IDs for {source_season}");
    }

    Ok(players)
}

fn match_pbp_players<'a>(
    selected: &[&'a PlayerNineCatStats],
    pbp_players: &[PbpPlayerRef],
) -> Vec<(&'a PlayerNineCatStats, String)> {
    let exact: HashMap<String, &PbpPlayerRef> = pbp_players
        .iter()
        .map(|player| (name_key(&player.name), player))
        .collect();

    let mut fallback: HashMap<(char, String), Vec<&PbpPlayerRef>> = HashMap::new();

    for player in pbp_players {
        if let Some(key) = first_initial_surname_key(&player.name) {
            fallback.entry(key).or_default().push(player);
        }
    }

    let mut matched = Vec::with_capacity(selected.len());

    for player in selected {
        if let Some(pbp) = exact.get(&name_key(&player.player_name)) {
            matched.push((*player, pbp.entity_id.clone()));
            continue;
        }

        if let Some(key) = first_initial_surname_key(&player.player_name) {
            if let Some(candidates) = fallback.get(&key) {
                if candidates.len() == 1 {
                    matched.push((*player, candidates[0].entity_id.clone()));
                    continue;
                }
            }
        }

        eprintln!(
            "warning: could not match {} to a PBP Stats player ID",
            player.player_name
        );
    }

    matched
}

fn request_pbp_game_logs_with_retry(
    client: &Client,
    source_season: &str,
    entity_id: &str,
    player: &PlayerNineCatStats,
) -> Result<Vec<RawGameLog>> {
    let params = [
        ("Season", source_season),
        ("SeasonType", "Regular Season"),
        ("EntityId", entity_id),
        ("EntityType", "Player"),
    ];

    let value = request_json_with_retry(client, PBP_GAME_LOGS_URL, &params, "PBP Stats game logs")?;

    if value.get("error").is_some() {
        return Ok(Vec::new());
    }

    let rows = value
        .get("multi_row_table_data")
        .and_then(Value::as_array)
        .context("PBP Stats game-log response is missing multi_row_table_data")?;

    rows.iter()
        .map(|row| parse_pbp_game_log(row, player))
        .collect()
}

fn request_json_with_retry(
    client: &Client,
    url: &str,
    params: &[(&str, &str)],
    description: &str,
) -> Result<Value> {
    let mut last_error = None;

    for attempt in 1..=3 {
        let result = client
            .get(url)
            .query(params)
            .header("Accept", "application/json")
            .send()
            .with_context(|| format!("requesting {description}"))
            .and_then(|response| {
                response
                    .error_for_status()
                    .with_context(|| format!("{description} returned an HTTP error"))
            })
            .and_then(|response| {
                response
                    .json::<Value>()
                    .with_context(|| format!("decoding {description}"))
            });

        match result {
            Ok(value) => return Ok(value),
            Err(error) => {
                last_error = Some(error);

                if attempt < 3 {
                    sleep(Duration::from_secs(attempt as u64));
                }
            }
        }
    }

    Err(last_error.context("request failed without an error")?)
}

fn parse_pbp_game_log(row: &Value, player: &PlayerNineCatStats) -> Result<RawGameLog> {
    let date_text = row
        .get("Date")
        .and_then(Value::as_str)
        .context("PBP Stats game log is missing Date")?;

    let fg2m = value_number_or_zero(row.get("FG2M"));
    let fg2a = value_number_or_zero(row.get("FG2A"));
    let fg3m = value_number_or_zero(row.get("FG3M"));
    let fg3a = value_number_or_zero(row.get("FG3A"));

    Ok(RawGameLog {
        player_id: player.player_id.clone(),
        player_name: player.player_name.clone(),
        game_date: parse_pbp_date(date_text)?,
        fgm: fg2m + fg3m,
        fga: fg2a + fg3a,
        ftm: value_number_or_zero(row.get("FtPoints")),
        fta: value_number_or_zero(row.get("FTA")),
        threes: fg3m,
        points: value_number_or_zero(row.get("Points")),
        rebounds: value_number_or_zero(row.get("Rebounds")),
        assists: value_number_or_zero(row.get("Assists")),
        steals: value_number_or_zero(row.get("Steals")),
        blocks: value_number_or_zero(row.get("Blocks")),
        turnovers: value_number_or_zero(row.get("Turnovers")),
    })
}

fn aggregate_game_logs(raw_logs: Vec<RawGameLog>) -> Result<Vec<PlayerWeeklyStats>> {
    if raw_logs.is_empty() {
        return Ok(Vec::new());
    }

    let season_first_game = raw_logs
        .iter()
        .map(|log| log.game_date)
        .min()
        .context("PBP Stats game logs had no dates")?;

    // Fantasy scoring periods are treated as Monday-Sunday. We intentionally
    // only store weeks in which a player appeared. durant.rs can later decide
    // whether missing weeks should count as zero-production injury weeks.
    let week_zero = season_first_game
        - ChronoDuration::days(season_first_game.weekday().num_days_from_monday() as i64);

    let mut grouped: HashMap<(PlayerId, u32), WeeklyAccumulator> = HashMap::new();

    for log in raw_logs {
        let week = ((log.game_date - week_zero).num_days() / 7) as u32 + 1;

        let entry = grouped
            .entry((log.player_id, week))
            .or_insert_with(|| WeeklyAccumulator {
                player_name: log.player_name.clone(),
                ..WeeklyAccumulator::default()
            });

        entry.games += 1;
        entry.fgm += log.fgm;
        entry.fga += log.fga;
        entry.ftm += log.ftm;
        entry.fta += log.fta;
        entry.threes += log.threes;
        entry.points += log.points;
        entry.rebounds += log.rebounds;
        entry.assists += log.assists;
        entry.steals += log.steals;
        entry.blocks += log.blocks;
        entry.turnovers += log.turnovers;
    }

    let mut weekly = grouped
        .into_iter()
        .map(|((player_id, week), row)| PlayerWeeklyStats {
            player_id,
            player_name: row.player_name,
            week,
            games: row.games,
            fgm: row.fgm,
            fga: row.fga,
            ftm: row.ftm,
            fta: row.fta,
            threes: row.threes,
            points: row.points,
            rebounds: row.rebounds,
            assists: row.assists,
            steals: row.steals,
            blocks: row.blocks,
            turnovers: row.turnovers,
        })
        .collect::<Vec<_>>();

    weekly.sort_by(|a, b| {
        a.player_name
            .cmp(&b.player_name)
            .then_with(|| a.week.cmp(&b.week))
    });

    Ok(weekly)
}

fn select_weekly_reference_players(players: &[PlayerNineCatStats]) -> Vec<&PlayerNineCatStats> {
    let candidates = players
        .iter()
        .filter(|player| {
            player.games >= MIN_SELECTOR_GAMES && player.minutes_pg >= MIN_SELECTOR_MINUTES_PG
        })
        .collect::<Vec<_>>();

    if candidates.is_empty() {
        return Vec::new();
    }

    let fg_baseline = percentage(
        candidates.iter().map(|player| player.fgm_total).sum(),
        candidates.iter().map(|player| player.fga_total).sum(),
    );
    let ft_baseline = percentage(
        candidates.iter().map(|player| player.ftm_total).sum(),
        candidates.iter().map(|player| player.fta_total).sum(),
    );

    let means = selector_means(&candidates, fg_baseline, ft_baseline);
    let std_devs = selector_std_devs(&candidates, fg_baseline, ft_baseline, means);

    let mut scored = candidates
        .into_iter()
        .map(|player| {
            let fg_impact = (player.fg_pct - fg_baseline) * player.fga_pg;
            let ft_impact = (player.ft_pct - ft_baseline) * player.fta_pg;

            let score = z(player.points_pg, means.points, std_devs.points)
                + z(player.threes_pg, means.threes, std_devs.threes)
                + z(player.rebounds_pg, means.rebounds, std_devs.rebounds)
                + z(player.assists_pg, means.assists, std_devs.assists)
                + z(player.steals_pg, means.steals, std_devs.steals)
                + z(player.blocks_pg, means.blocks, std_devs.blocks)
                + z(-player.turnovers_pg, means.turnovers, std_devs.turnovers)
                + z(fg_impact, means.fg_impact, std_devs.fg_impact)
                + z(ft_impact, means.ft_impact, std_devs.ft_impact);

            (player, score)
        })
        .collect::<Vec<_>>();

    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(WEEKLY_REFERENCE_POOL_SIZE.min(scored.len()));

    scored.into_iter().map(|(player, _)| player).collect()
}

fn selector_means(
    players: &[&PlayerNineCatStats],
    fg_baseline: f64,
    ft_baseline: f64,
) -> SelectorMeans {
    let n = players.len() as f64;

    players
        .iter()
        .fold(SelectorMeans::default(), |mut acc, player| {
            acc.points += player.points_pg / n;
            acc.threes += player.threes_pg / n;
            acc.rebounds += player.rebounds_pg / n;
            acc.assists += player.assists_pg / n;
            acc.steals += player.steals_pg / n;
            acc.blocks += player.blocks_pg / n;
            acc.turnovers += -player.turnovers_pg / n;
            acc.fg_impact += (player.fg_pct - fg_baseline) * player.fga_pg / n;
            acc.ft_impact += (player.ft_pct - ft_baseline) * player.fta_pg / n;
            acc
        })
}

fn selector_std_devs(
    players: &[&PlayerNineCatStats],
    fg_baseline: f64,
    ft_baseline: f64,
    means: SelectorMeans,
) -> SelectorStdDevs {
    let n = players.len() as f64;

    let sums = players
        .iter()
        .fold(SelectorStdDevs::default(), |mut acc, player| {
            let fg_impact = (player.fg_pct - fg_baseline) * player.fga_pg;
            let ft_impact = (player.ft_pct - ft_baseline) * player.fta_pg;

            acc.points += (player.points_pg - means.points).powi(2);
            acc.threes += (player.threes_pg - means.threes).powi(2);
            acc.rebounds += (player.rebounds_pg - means.rebounds).powi(2);
            acc.assists += (player.assists_pg - means.assists).powi(2);
            acc.steals += (player.steals_pg - means.steals).powi(2);
            acc.blocks += (player.blocks_pg - means.blocks).powi(2);
            acc.turnovers += (-player.turnovers_pg - means.turnovers).powi(2);
            acc.fg_impact += (fg_impact - means.fg_impact).powi(2);
            acc.ft_impact += (ft_impact - means.ft_impact).powi(2);
            acc
        });

    SelectorStdDevs {
        points: (sums.points / n).sqrt(),
        threes: (sums.threes / n).sqrt(),
        rebounds: (sums.rebounds / n).sqrt(),
        assists: (sums.assists / n).sqrt(),
        steals: (sums.steals / n).sqrt(),
        blocks: (sums.blocks / n).sqrt(),
        turnovers: (sums.turnovers / n).sqrt(),
        fg_impact: (sums.fg_impact / n).sqrt(),
        ft_impact: (sums.ft_impact / n).sqrt(),
    }
}

fn z(value: f64, mean: f64, std_dev: f64) -> f64 {
    if std_dev <= f64::EPSILON {
        0.0
    } else {
        (value - mean) / std_dev
    }
}

fn parse_pbp_date(value: &str) -> Result<NaiveDate> {
    const FORMATS: &[&str] = &[
        "%Y-%m-%d",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%SZ",
        "%m/%d/%Y",
        "%b %d, %Y",
        "%b %e, %Y",
    ];

    for format in FORMATS {
        if let Ok(date) = NaiveDate::parse_from_str(value, format) {
            return Ok(date);
        }
    }

    // Also accept ISO timestamps with fractional seconds/time-zone suffixes by
    // reading their YYYY-MM-DD prefix.
    if let Some(prefix) = value.get(0..10) {
        if let Ok(date) = NaiveDate::parse_from_str(prefix, "%Y-%m-%d") {
            return Ok(date);
        }
    }

    bail!("unrecognized PBP Stats game date: {value}")
}

fn value_number_or_zero(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(Value::String(text)) => text.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

fn value_as_id(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn first_initial_surname_key(name: &str) -> Option<(char, String)> {
    let mut tokens = name
        .split_whitespace()
        .map(name_key)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();

    while matches!(
        tokens.last().map(String::as_str),
        Some("jr" | "sr" | "ii" | "iii" | "iv" | "v")
    ) {
        tokens.pop();
    }

    let first = tokens.first()?.chars().next()?;
    let surname = tokens.last()?.clone();

    Some((first, surname))
}

fn name_key(name: &str) -> String {
    let mut key = String::new();

    for character in name.chars().flat_map(char::to_lowercase) {
        let folded = match character {
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => 'a',
            'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => 'c',
            'ď' | 'đ' | 'ð' => 'd',
            'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => 'e',
            'ĝ' | 'ğ' | 'ġ' | 'ģ' => 'g',
            'ĥ' | 'ħ' => 'h',
            'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => 'i',
            'ĵ' => 'j',
            'ķ' => 'k',
            'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => 'l',
            'ñ' | 'ń' | 'ņ' | 'ň' | 'ŋ' => 'n',
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' => 'o',
            'ŕ' | 'ŗ' | 'ř' => 'r',
            'ś' | 'ŝ' | 'ş' | 'š' => 's',
            'ţ' | 'ť' | 'ŧ' => 't',
            'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => 'u',
            'ŵ' => 'w',
            'ý' | 'ÿ' | 'ŷ' => 'y',
            'ź' | 'ż' | 'ž' => 'z',
            other => other,
        };

        if folded.is_ascii_alphanumeric() {
            key.push(folded);
        }
    }

    key
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

    if let Some(total) = rows
        .iter()
        .find(|player| player.team == "TOT" || player.team.ends_with("TM"))
    {
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

    #[test]
    fn pbp_name_keys_handle_accents_and_suffixes() {
        assert_eq!(name_key("Nikola Jokić"), "nikolajokic");
        assert_eq!(
            first_initial_surname_key("Gary Trent Jr."),
            Some(('g', "trent".to_string()))
        );
    }

    #[test]
    fn parses_pbp_iso_timestamp_prefix() {
        assert_eq!(
            parse_pbp_date("2025-10-21T00:00:00.000Z").unwrap(),
            NaiveDate::from_ymd_opt(2025, 10, 21).unwrap()
        );
    }
}
