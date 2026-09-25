use crate::app::App;
use crate::durant::{DYNAMIC_CATEGORY_NAMES, DurantModel, MIN_ACTIVE_WEEKS};
use crate::player::PlayerId;
use crate::stats::{PlayerNineCatStats, PlayerWeeklyStats, StatsBundle};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

const VALIDATION_SCHEMA_VERSION: u32 = 1;
const TEAM_SIZE: usize = 13;
const CATEGORY_COUNT: usize = 9;
const CALIBRATION_BINS: usize = 10;
const MIN_HOLDOUT_WEEKS: usize = 4;
const DEFAULT_SAMPLES_PER_MODE: usize = 20_000;
const DEFAULT_SEED: u64 = 0xB1D0_B04D_2026_0035;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValidationMode {
    ActiveWeek,
    Calendar,
}

impl ValidationMode {
    fn label(self) -> &'static str {
        match self {
            Self::ActiveWeek => "ACTIVE-WEEK",
            Self::Calendar => "CALENDAR",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::ActiveWeek => {
                "Both rosters are sampled only from Q players with an observed row in the held-out week. This isolates H/category calibration conditional on players being active."
            }
            Self::Calendar => {
                "Rosters are sampled from Q before weekly availability is known; a missing weekly row contributes zero production. This is a practical schedule/availability diagnostic, not a pure H-core test."
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ValidationReport {
    schema_version: u32,
    experiment: String,
    source_season: String,
    draft_season: String,
    team_size: usize,
    reference_size: usize,
    train_weeks: Vec<u32>,
    holdout_weeks: Vec<u32>,
    train_week_count: usize,
    holdout_week_count: usize,
    training_weekly_rows: usize,
    holdout_weekly_rows: usize,
    samples_requested_per_mode: usize,
    seed: u64,
    wall_time_seconds: f64,
    limitations: Vec<String>,
    modes: Vec<ModeSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct ModeSummary {
    mode: String,
    description: String,
    requested_samples: usize,
    completed_samples: usize,
    skipped_samples: usize,
    average_active_pool_size: f64,
    matchup_tie_rate: f64,
    category_tie_rates: [f64; CATEGORY_COUNT],
    h: CalibrationSummary,
    categories: Vec<CategoryCalibrationSummary>,
    outcome_correlation_matrix: [[f64; CATEGORY_COUNT]; CATEGORY_COUNT],
    strongest_outcome_correlations: Vec<CorrelationPair>,
}

#[derive(Debug, Clone, Serialize)]
struct CategoryCalibrationSummary {
    category: String,
    calibration: CalibrationSummary,
}

#[derive(Debug, Clone, Serialize)]
struct CalibrationSummary {
    count: usize,
    mean_prediction: f64,
    observed_rate: f64,
    brier_score: f64,
    expected_calibration_error: f64,
    maximum_bin_calibration_error: f64,
    prediction_stddev: f64,
    bins: Vec<CalibrationBinSummary>,
}

#[derive(Debug, Clone, Serialize)]
struct CalibrationBinSummary {
    lower: f64,
    upper: f64,
    count: usize,
    mean_prediction: f64,
    observed_rate: f64,
    calibration_error: f64,
}

#[derive(Debug, Clone, Serialize)]
struct CorrelationPair {
    category_a: String,
    category_b: String,
    correlation: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct BinAccumulator {
    count: usize,
    prediction_sum: f64,
    observed_sum: f64,
}

#[derive(Debug, Clone)]
struct CalibrationAccumulator {
    count: usize,
    prediction_sum: f64,
    prediction_sq_sum: f64,
    observed_sum: f64,
    squared_error_sum: f64,
    bins: [BinAccumulator; CALIBRATION_BINS],
}

impl Default for CalibrationAccumulator {
    fn default() -> Self {
        Self {
            count: 0,
            prediction_sum: 0.0,
            prediction_sq_sum: 0.0,
            observed_sum: 0.0,
            squared_error_sum: 0.0,
            bins: [BinAccumulator::default(); CALIBRATION_BINS],
        }
    }
}

impl CalibrationAccumulator {
    fn push(&mut self, prediction: f64, observed: f64) {
        let prediction = prediction.clamp(0.0, 1.0);
        let observed = observed.clamp(0.0, 1.0);
        let bin =
            ((prediction * CALIBRATION_BINS as f64).floor() as usize).min(CALIBRATION_BINS - 1);

        self.count += 1;
        self.prediction_sum += prediction;
        self.prediction_sq_sum += prediction * prediction;
        self.observed_sum += observed;
        self.squared_error_sum += (prediction - observed).powi(2);
        self.bins[bin].count += 1;
        self.bins[bin].prediction_sum += prediction;
        self.bins[bin].observed_sum += observed;
    }

    fn summary(&self) -> CalibrationSummary {
        if self.count == 0 {
            return CalibrationSummary {
                count: 0,
                mean_prediction: 0.0,
                observed_rate: 0.0,
                brier_score: 0.0,
                expected_calibration_error: 0.0,
                maximum_bin_calibration_error: 0.0,
                prediction_stddev: 0.0,
                bins: Vec::new(),
            };
        }

        let n = self.count as f64;
        let mean_prediction = self.prediction_sum / n;
        let observed_rate = self.observed_sum / n;
        let variance = (self.prediction_sq_sum / n - mean_prediction.powi(2)).max(0.0);
        let mut expected_calibration_error = 0.0;
        let mut maximum_bin_calibration_error: f64 = 0.0;
        let mut bins = Vec::with_capacity(CALIBRATION_BINS);

        for index in 0..CALIBRATION_BINS {
            let lower = index as f64 / CALIBRATION_BINS as f64;
            let upper = (index + 1) as f64 / CALIBRATION_BINS as f64;
            let bin = self.bins[index];
            if bin.count == 0 {
                bins.push(CalibrationBinSummary {
                    lower,
                    upper,
                    count: 0,
                    mean_prediction: 0.0,
                    observed_rate: 0.0,
                    calibration_error: 0.0,
                });
                continue;
            }

            let bin_n = bin.count as f64;
            let bin_prediction = bin.prediction_sum / bin_n;
            let bin_observed = bin.observed_sum / bin_n;
            let error = (bin_prediction - bin_observed).abs();
            expected_calibration_error += error * bin_n / n;
            maximum_bin_calibration_error = maximum_bin_calibration_error.max(error);

            bins.push(CalibrationBinSummary {
                lower,
                upper,
                count: bin.count,
                mean_prediction: bin_prediction,
                observed_rate: bin_observed,
                calibration_error: error,
            });
        }

        CalibrationSummary {
            count: self.count,
            mean_prediction,
            observed_rate,
            brier_score: self.squared_error_sum / n,
            expected_calibration_error,
            maximum_bin_calibration_error,
            prediction_stddev: variance.sqrt(),
            bins,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct CorrelationAccumulator {
    count: usize,
    sums: [f64; CATEGORY_COUNT],
    square_sums: [f64; CATEGORY_COUNT],
    cross_sums: [[f64; CATEGORY_COUNT]; CATEGORY_COUNT],
}

impl CorrelationAccumulator {
    fn push(&mut self, values: [f64; CATEGORY_COUNT]) {
        self.count += 1;
        for i in 0..CATEGORY_COUNT {
            self.sums[i] += values[i];
            self.square_sums[i] += values[i] * values[i];
            for j in 0..CATEGORY_COUNT {
                self.cross_sums[i][j] += values[i] * values[j];
            }
        }
    }

    fn matrix(&self) -> [[f64; CATEGORY_COUNT]; CATEGORY_COUNT] {
        let mut matrix = [[0.0; CATEGORY_COUNT]; CATEGORY_COUNT];
        if self.count == 0 {
            return matrix;
        }

        let n = self.count as f64;
        for i in 0..CATEGORY_COUNT {
            let mean_i = self.sums[i] / n;
            let variance_i = (self.square_sums[i] / n - mean_i * mean_i).max(0.0);
            for j in 0..CATEGORY_COUNT {
                let mean_j = self.sums[j] / n;
                let variance_j = (self.square_sums[j] / n - mean_j * mean_j).max(0.0);
                let denominator = (variance_i * variance_j).sqrt();
                matrix[i][j] = if denominator <= f64::EPSILON {
                    if i == j { 1.0 } else { 0.0 }
                } else {
                    (self.cross_sums[i][j] / n - mean_i * mean_j) / denominator
                };
            }
        }
        matrix
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct WeeklyTotals {
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

impl WeeklyTotals {
    fn add_row(&mut self, row: &PlayerWeeklyStats) {
        self.fgm += row.fgm;
        self.fga += row.fga;
        self.ftm += row.ftm;
        self.fta += row.fta;
        self.threes += row.threes;
        self.points += row.points;
        self.rebounds += row.rebounds;
        self.assists += row.assists;
        self.steals += row.steals;
        self.blocks += row.blocks;
        self.turnovers += row.turnovers;
    }

    fn fg_pct(self) -> f64 {
        ratio(self.fgm, self.fga)
    }

    fn ft_pct(self) -> f64 {
        ratio(self.ftm, self.fta)
    }
}

#[derive(Debug, Clone)]
struct ActualMatchup {
    category_outcomes: [f64; CATEGORY_COUNT],
    matchup_outcome: f64,
    matchup_tied: bool,
    category_tied: [bool; CATEGORY_COUNT],
}

#[derive(Debug, Clone)]
struct ModeAccumulator {
    mode: ValidationMode,
    requested: usize,
    completed: usize,
    skipped: usize,
    pool_size_sum: usize,
    matchup_ties: usize,
    category_ties: [usize; CATEGORY_COUNT],
    h: CalibrationAccumulator,
    categories: [CalibrationAccumulator; CATEGORY_COUNT],
    correlations: CorrelationAccumulator,
}

impl ModeAccumulator {
    fn new(mode: ValidationMode, requested: usize) -> Self {
        Self {
            mode,
            requested,
            completed: 0,
            skipped: 0,
            pool_size_sum: 0,
            matchup_ties: 0,
            category_ties: [0; CATEGORY_COUNT],
            h: CalibrationAccumulator::default(),
            categories: std::array::from_fn(|_| CalibrationAccumulator::default()),
            correlations: CorrelationAccumulator::default(),
        }
    }

    fn push(
        &mut self,
        pool_size: usize,
        predicted_categories: [f64; CATEGORY_COUNT],
        predicted_h: f64,
        actual: &ActualMatchup,
    ) {
        self.completed += 1;
        self.pool_size_sum += pool_size;
        self.matchup_ties += if actual.matchup_tied { 1 } else { 0 };
        self.h.push(predicted_h, actual.matchup_outcome);

        for category in 0..CATEGORY_COUNT {
            self.categories[category].push(
                predicted_categories[category],
                actual.category_outcomes[category],
            );
            self.category_ties[category] += if actual.category_tied[category] { 1 } else { 0 };
        }
        self.correlations.push(actual.category_outcomes);
    }

    fn summary(&self) -> ModeSummary {
        let matrix = self.correlations.matrix();
        let mut pairs = Vec::new();
        for i in 0..CATEGORY_COUNT {
            for j in (i + 1)..CATEGORY_COUNT {
                pairs.push(CorrelationPair {
                    category_a: DYNAMIC_CATEGORY_NAMES[i].to_string(),
                    category_b: DYNAMIC_CATEGORY_NAMES[j].to_string(),
                    correlation: matrix[i][j],
                });
            }
        }
        pairs.sort_by(|a, b| b.correlation.abs().total_cmp(&a.correlation.abs()));
        pairs.truncate(12);

        let completed = self.completed.max(1) as f64;
        ModeSummary {
            mode: self.mode.label().to_string(),
            description: self.mode.description().to_string(),
            requested_samples: self.requested,
            completed_samples: self.completed,
            skipped_samples: self.skipped,
            average_active_pool_size: self.pool_size_sum as f64 / completed,
            matchup_tie_rate: self.matchup_ties as f64 / completed,
            category_tie_rates: self.category_ties.map(|count| count as f64 / completed),
            h: self.h.summary(),
            categories: (0..CATEGORY_COUNT)
                .map(|index| CategoryCalibrationSummary {
                    category: DYNAMIC_CATEGORY_NAMES[index].to_string(),
                    calibration: self.categories[index].summary(),
                })
                .collect(),
            outcome_correlation_matrix: matrix,
            strongest_outcome_correlations: pairs,
        }
    }
}

#[derive(Debug, Clone)]
struct HistoricalSetup {
    model: DurantModel,
    train_weeks: Vec<u32>,
    holdout_weeks: Vec<u32>,
    training_rows: Vec<PlayerWeeklyStats>,
    holdout_rows: Vec<PlayerWeeklyStats>,
}

#[derive(Debug, Clone)]
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    fn index(&mut self, upper_exclusive: usize) -> usize {
        if upper_exclusive <= 1 {
            0
        } else {
            (self.next_u64() as usize) % upper_exclusive
        }
    }
}

pub fn print_plan(app: &App) {
    let samples = requested_samples();
    let weeks = unique_weeks(&app.stats.weekly);
    println!();
    println!("========================================================================");
    println!("H CALIBRATION — TEMPORAL HOLDOUT — v35");
    println!("========================================================================");
    println!("source season          : {}", app.stats.source_season);
    println!("available weekly rows  : {}", app.stats.weekly.len());
    println!("available week labels  : {:?}", weeks);
    println!("team size              : {}", TEAM_SIZE);
    println!("reference population Q : {}", app.teams.len() * TEAM_SIZE);
    println!("samples / mode         : {}", samples);
    println!("seed                    : {}", requested_seed());
    println!();
    println!("Method:");
    println!("  1. choose the earliest temporal split that can fit a full historical Q");
    println!(
        "     with >= {} active training weeks/player, leaving >= {} unseen weeks;",
        MIN_ACTIVE_WEEKS, MIN_HOLDOUT_WEEKS
    );
    println!("  2. rebuild DURANT only from PRE-split weekly data; current manual");
    println!("     2026-27 projection overrides are disabled;");
    println!("  3. freeze player X values and mu/sigma/tau, then predict roster-vs-roster");
    println!("     category probabilities + H on later unseen weeks;");
    println!("  4. compare probabilities to actual held-out category/matchup outcomes;");
    println!("  5. measure observed category-outcome correlations to stress-test H's");
    println!("     independence assumption.");
    println!();
    println!("Two diagnostics are emitted:");
    println!("  ACTIVE-WEEK : pure H/category test conditional on sampled players playing");
    println!("  CALENDAR    : pre-week rosters; missing weekly row = zero production");
    println!();
    println!("Important limitation:");
    println!("  the cached weekly pool (~220 players) was itself selected using full-season");
    println!("  source statistics. Held-out WEEKLY VALUES do not leak into the fit, but the");
    println!("  outer player universe has mild look-ahead selection.");
    println!("  Also, thousands of sampled roster matchups reuse a small number of held-out");
    println!("  NBA weeks; treat calibration rates as diagnostics, not 20,000 independent trials.");
    println!();
    println!(
        "Outputs go to data/stats/{}/durant/h_validation_v35_*",
        app.stats.draft_season
    );
}

pub fn validate_and_save(app: &App) -> Result<()> {
    let started = Instant::now();
    let samples = requested_samples();
    let seed = requested_seed();
    let setup = build_historical_setup(app)?;

    println!();
    println!("========================================================================");
    println!("H CALIBRATION — TEMPORAL HOLDOUT — v35");
    println!("========================================================================");
    println!(
        "train weeks {:?} | holdout weeks {:?} | Q={} | {} samples/mode",
        setup.train_weeks, setup.holdout_weeks, setup.model.reference_size, samples,
    );
    println!(
        "training rows {} | holdout rows {} | current manual overrides DISABLED",
        setup.training_rows.len(),
        setup.holdout_rows.len(),
    );

    let rows_by_week = weekly_rows_by_week(&setup.holdout_rows);
    let output_dir = app.stats.cache_dir.join("durant");
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("creating {}", output_dir.display()))?;
    let raw_path = output_dir.join("h_validation_v35_samples.csv");
    let mut raw_writer = csv::Writer::from_path(&raw_path)
        .with_context(|| format!("creating {}", raw_path.display()))?;
    write_raw_header(&mut raw_writer)?;

    println!("running ACTIVE-WEEK calibration ...");
    let mut active = run_mode(
        ValidationMode::ActiveWeek,
        &setup,
        &rows_by_week,
        samples,
        seed ^ 0xA11C_E001,
        &mut raw_writer,
    )?;
    println!("running CALENDAR diagnostic ...");
    let mut calendar = run_mode(
        ValidationMode::Calendar,
        &setup,
        &rows_by_week,
        samples,
        seed ^ 0xCA1E_0001,
        &mut raw_writer,
    )?;
    raw_writer.flush()?;

    // Explicitly account for attempts that could not be formed. This is rare
    // but makes the report honest if a held-out week has too few active Q rows.
    active.skipped = active.requested.saturating_sub(active.completed);
    calendar.skipped = calendar.requested.saturating_sub(calendar.completed);

    let mode_summaries = vec![active.summary(), calendar.summary()];
    let report = ValidationReport {
        schema_version: VALIDATION_SCHEMA_VERSION,
        experiment: "out-of-sample H/category calibration using temporal weekly holdout".to_string(),
        source_season: app.stats.source_season.clone(),
        draft_season: app.stats.draft_season.clone(),
        team_size: TEAM_SIZE,
        reference_size: setup.model.reference_size,
        train_weeks: setup.train_weeks.clone(),
        holdout_weeks: setup.holdout_weeks.clone(),
        train_week_count: setup.train_weeks.len(),
        holdout_week_count: setup.holdout_weeks.len(),
        training_weekly_rows: setup.training_rows.len(),
        holdout_weekly_rows: setup.holdout_rows.len(),
        samples_requested_per_mode: samples,
        seed,
        wall_time_seconds: started.elapsed().as_secs_f64(),
        limitations: vec![
            "The cached weekly history covers a fantasy-relevant ~220-player pool that was selected using full-season source statistics. The temporal split prevents held-out weekly values from entering H fitting, but the outer candidate universe itself has mild look-ahead selection.".to_string(),
            "ACTIVE-WEEK conditions on sampled players having an observed row, so it intentionally does not measure injury/zero-game risk. CALENDAR includes that missing-row risk but can also reflect any gaps in the weekly provider.".to_string(),
            "Random 13-player rosters from historical Q are a calibration experiment, not reconstructions of actual ESPN league rosters or waiver behavior.".to_string(),
            "The requested Monte Carlo matchup count is much larger than the number of independent held-out NBA weeks. Samples repeatedly recombine the same held-out player-week observations, so calibration point estimates are useful but naive sample-count confidence intervals would be overconfident.".to_string(),
        ],
        modes: mode_summaries,
    };

    let json_path = output_dir.join("h_validation_v35_summary.json");
    fs::write(&json_path, serde_json::to_string_pretty(&report)?)
        .with_context(|| format!("writing {}", json_path.display()))?;

    let bins_path = output_dir.join("h_validation_v35_calibration.csv");
    write_calibration_csv(&bins_path, &report)?;
    let correlation_path = output_dir.join("h_validation_v35_correlations.csv");
    write_correlation_csv(&correlation_path, &report)?;

    print_summary(&report);
    println!();
    println!("report       : {}", json_path.display());
    println!("calibration  : {}", bins_path.display());
    println!("correlations : {}", correlation_path.display());
    println!("raw samples  : {}", raw_path.display());
    println!("wall time    : {:.2}s", started.elapsed().as_secs_f64());

    Ok(())
}

fn build_historical_setup(app: &App) -> Result<HistoricalSetup> {
    let weeks = unique_weeks(&app.stats.weekly);
    if weeks.len() < MIN_ACTIVE_WEEKS + MIN_HOLDOUT_WEEKS {
        bail!(
            "only {} week labels are available; H validation needs at least {}",
            weeks.len(),
            MIN_ACTIVE_WEEKS + MIN_HOLDOUT_WEEKS
        );
    }

    let league_teams = app.teams.len();
    if league_teams == 0 {
        bail!("H validation needs at least one fantasy team");
    }

    let earliest_cut = ((weeks.len() as f64) * 0.65).ceil() as usize;
    let earliest_cut = earliest_cut.max(MIN_ACTIVE_WEEKS + 1);
    let latest_cut = weeks.len().saturating_sub(MIN_HOLDOUT_WEEKS);
    let mut last_error = None;

    for cut in earliest_cut..=latest_cut {
        let train_weeks = weeks[..cut].to_vec();
        let holdout_weeks = weeks[cut..].to_vec();
        let train_set = train_weeks.iter().copied().collect::<HashSet<_>>();
        let holdout_set = holdout_weeks.iter().copied().collect::<HashSet<_>>();
        let training_rows = app
            .stats
            .weekly
            .iter()
            .filter(|row| train_set.contains(&row.week))
            .cloned()
            .collect::<Vec<_>>();
        let holdout_rows = app
            .stats
            .weekly
            .iter()
            .filter(|row| holdout_set.contains(&row.week))
            .cloned()
            .collect::<Vec<_>>();
        let training_players = aggregate_players_from_weekly(&app.stats.players, &training_rows);
        let training_bundle = StatsBundle {
            draft_season: format!("{}-h-train", app.stats.draft_season),
            source_season: app.stats.source_season.clone(),
            cache_dir: app.stats.cache_dir.clone(),
            players: training_players,
            weekly: training_rows.clone(),
        };

        match DurantModel::from_historical_stats_for_validation(
            &training_bundle,
            league_teams,
            TEAM_SIZE,
        ) {
            Ok(model) => {
                return Ok(HistoricalSetup {
                    model,
                    train_weeks,
                    holdout_weeks,
                    training_rows,
                    holdout_rows,
                });
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }

    bail!(
        "could not construct a leakage-free historical DURANT model while leaving at least {} holdout weeks; last error: {}",
        MIN_HOLDOUT_WEEKS,
        last_error.unwrap_or_else(|| "unknown".to_string())
    )
}

fn aggregate_players_from_weekly(
    originals: &[PlayerNineCatStats],
    weekly: &[PlayerWeeklyStats],
) -> Vec<PlayerNineCatStats> {
    #[derive(Debug, Clone, Default)]
    struct Aggregate {
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

    let original_by_id = originals
        .iter()
        .map(|player| (player.player_id.clone(), player))
        .collect::<HashMap<_, _>>();
    let mut aggregates = HashMap::<PlayerId, Aggregate>::new();

    for row in weekly {
        let aggregate = aggregates.entry(row.player_id.clone()).or_default();
        aggregate.games += row.games;
        aggregate.fgm += row.fgm;
        aggregate.fga += row.fga;
        aggregate.ftm += row.ftm;
        aggregate.fta += row.fta;
        aggregate.threes += row.threes;
        aggregate.points += row.points;
        aggregate.rebounds += row.rebounds;
        aggregate.assists += row.assists;
        aggregate.steals += row.steals;
        aggregate.blocks += row.blocks;
        aggregate.turnovers += row.turnovers;
    }

    aggregates
        .into_iter()
        .filter_map(|(player_id, aggregate)| {
            let original = original_by_id.get(&player_id)?;
            if aggregate.games == 0 {
                return None;
            }
            let games = aggregate.games as f64;
            Some(PlayerNineCatStats {
                player_id,
                player_name: original.player_name.clone(),
                team: original.team.clone(),
                games: aggregate.games,
                minutes_pg: original.minutes_pg,
                fgm_pg: aggregate.fgm / games,
                fga_pg: aggregate.fga / games,
                fg_pct: ratio(aggregate.fgm, aggregate.fga),
                ftm_pg: aggregate.ftm / games,
                fta_pg: aggregate.fta / games,
                ft_pct: ratio(aggregate.ftm, aggregate.fta),
                fgm_total: aggregate.fgm,
                fga_total: aggregate.fga,
                ftm_total: aggregate.ftm,
                fta_total: aggregate.fta,
                threes_pg: aggregate.threes / games,
                points_pg: aggregate.points / games,
                rebounds_pg: aggregate.rebounds / games,
                assists_pg: aggregate.assists / games,
                steals_pg: aggregate.steals / games,
                blocks_pg: aggregate.blocks / games,
                turnovers_pg: aggregate.turnovers / games,
            })
        })
        .collect()
}

fn run_mode(
    mode: ValidationMode,
    setup: &HistoricalSetup,
    rows_by_week: &HashMap<u32, HashMap<PlayerId, PlayerWeeklyStats>>,
    requested: usize,
    seed: u64,
    raw_writer: &mut csv::Writer<std::fs::File>,
) -> Result<ModeAccumulator> {
    let mut accumulator = ModeAccumulator::new(mode, requested);
    let mut rng = XorShift64::new(seed);
    let reference = &setup.model.reference_players;
    let needed = TEAM_SIZE * 2;

    for sample_index in 0..requested {
        let week = setup.holdout_weeks[sample_index % setup.holdout_weeks.len()];
        let Some(week_rows) = rows_by_week.get(&week) else {
            continue;
        };

        let pool = match mode {
            ValidationMode::ActiveWeek => reference
                .iter()
                .filter(|player_id| week_rows.contains_key(*player_id))
                .cloned()
                .collect::<Vec<_>>(),
            ValidationMode::Calendar => reference.to_vec(),
        };

        if pool.len() < needed {
            continue;
        }

        let Some((own_roster, opponent_roster)) = sample_disjoint_rosters(&pool, &mut rng) else {
            continue;
        };
        let Some((predicted_categories, predicted_h)) = setup
            .model
            .validation_complete_matchup_prediction(&own_roster, &opponent_roster)
        else {
            continue;
        };

        let actual = actual_matchup(&own_roster, &opponent_roster, week_rows);
        accumulator.push(pool.len(), predicted_categories, predicted_h, &actual);
        write_raw_sample(
            raw_writer,
            mode,
            week,
            predicted_h,
            &predicted_categories,
            &actual,
        )?;
    }

    Ok(accumulator)
}

fn sample_disjoint_rosters(
    pool: &[PlayerId],
    rng: &mut XorShift64,
) -> Option<(Vec<PlayerId>, Vec<PlayerId>)> {
    let needed = TEAM_SIZE * 2;
    if pool.len() < needed {
        return None;
    }

    let mut indices = (0..pool.len()).collect::<Vec<_>>();
    for i in 0..needed {
        let j = i + rng.index(pool.len() - i);
        indices.swap(i, j);
    }

    let own = indices[..TEAM_SIZE]
        .iter()
        .map(|&index| pool[index].clone())
        .collect::<Vec<_>>();
    let opponent = indices[TEAM_SIZE..needed]
        .iter()
        .map(|&index| pool[index].clone())
        .collect::<Vec<_>>();
    Some((own, opponent))
}

fn actual_matchup(
    own_roster: &[PlayerId],
    opponent_roster: &[PlayerId],
    week_rows: &HashMap<PlayerId, PlayerWeeklyStats>,
) -> ActualMatchup {
    let own = aggregate_actual(own_roster, week_rows);
    let opponent = aggregate_actual(opponent_roster, week_rows);

    let own_values = [
        own.fg_pct(),
        own.ft_pct(),
        own.threes,
        own.points,
        own.rebounds,
        own.assists,
        own.steals,
        own.blocks,
        own.turnovers,
    ];
    let opponent_values = [
        opponent.fg_pct(),
        opponent.ft_pct(),
        opponent.threes,
        opponent.points,
        opponent.rebounds,
        opponent.assists,
        opponent.steals,
        opponent.blocks,
        opponent.turnovers,
    ];

    let mut category_outcomes = [0.0; CATEGORY_COUNT];
    let mut category_tied = [false; CATEGORY_COUNT];
    let mut wins = 0usize;
    let mut losses = 0usize;

    for category in 0..CATEGORY_COUNT {
        let ordering = if category == 8 {
            compare_lower_is_better(own_values[category], opponent_values[category])
        } else {
            compare_higher_is_better(own_values[category], opponent_values[category])
        };

        category_outcomes[category] = match ordering {
            1 => {
                wins += 1;
                1.0
            }
            -1 => {
                losses += 1;
                0.0
            }
            _ => {
                category_tied[category] = true;
                0.5
            }
        };
    }

    let (matchup_outcome, matchup_tied) = if wins > losses {
        (1.0, false)
    } else if losses > wins {
        (0.0, false)
    } else {
        (0.5, true)
    };

    ActualMatchup {
        category_outcomes,
        matchup_outcome,
        matchup_tied,
        category_tied,
    }
}

fn aggregate_actual(
    roster: &[PlayerId],
    week_rows: &HashMap<PlayerId, PlayerWeeklyStats>,
) -> WeeklyTotals {
    let mut totals = WeeklyTotals::default();
    for player_id in roster {
        if let Some(row) = week_rows.get(player_id) {
            totals.add_row(row);
        }
    }
    totals
}

fn compare_higher_is_better(left: f64, right: f64) -> i8 {
    if (left - right).abs() <= 1e-12 {
        0
    } else if left > right {
        1
    } else {
        -1
    }
}

fn compare_lower_is_better(left: f64, right: f64) -> i8 {
    compare_higher_is_better(right, left)
}

fn weekly_rows_by_week(
    rows: &[PlayerWeeklyStats],
) -> HashMap<u32, HashMap<PlayerId, PlayerWeeklyStats>> {
    let mut by_week = HashMap::new();
    for row in rows {
        by_week
            .entry(row.week)
            .or_insert_with(HashMap::new)
            .insert(row.player_id.clone(), row.clone());
    }
    by_week
}

fn unique_weeks(rows: &[PlayerWeeklyStats]) -> Vec<u32> {
    let mut weeks = rows.iter().map(|row| row.week).collect::<Vec<_>>();
    weeks.sort_unstable();
    weeks.dedup();
    weeks
}

fn requested_samples() -> usize {
    std::env::var("BIRDBOARD_H_VALIDATION_SAMPLES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&value| value >= 100)
        .unwrap_or(DEFAULT_SAMPLES_PER_MODE)
}

fn requested_seed() -> u64 {
    std::env::var("BIRDBOARD_H_VALIDATION_SEED")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SEED)
}

fn write_raw_header(writer: &mut csv::Writer<std::fs::File>) -> Result<()> {
    let mut header = vec![
        "mode".to_string(),
        "week".to_string(),
        "predicted_h".to_string(),
        "actual_matchup".to_string(),
        "matchup_tied".to_string(),
    ];
    for category in DYNAMIC_CATEGORY_NAMES {
        header.push(format!("pred_{category}"));
    }
    for category in DYNAMIC_CATEGORY_NAMES {
        header.push(format!("actual_{category}"));
    }
    writer.write_record(header)?;
    Ok(())
}

fn write_raw_sample(
    writer: &mut csv::Writer<std::fs::File>,
    mode: ValidationMode,
    week: u32,
    predicted_h: f64,
    predicted_categories: &[f64; CATEGORY_COUNT],
    actual: &ActualMatchup,
) -> Result<()> {
    let mut row = vec![
        mode.label().to_string(),
        week.to_string(),
        format!("{predicted_h:.8}"),
        format!("{:.1}", actual.matchup_outcome),
        actual.matchup_tied.to_string(),
    ];
    row.extend(
        predicted_categories
            .iter()
            .map(|value| format!("{value:.8}")),
    );
    row.extend(
        actual
            .category_outcomes
            .iter()
            .map(|value| format!("{value:.1}")),
    );
    writer.write_record(row)?;
    Ok(())
}

fn write_calibration_csv(path: &PathBuf, report: &ValidationReport) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record([
        "mode",
        "target",
        "bin_lower",
        "bin_upper",
        "count",
        "mean_prediction",
        "observed_rate",
        "calibration_error",
    ])?;

    for mode in &report.modes {
        write_bins(&mut writer, &mode.mode, "H", &mode.h)?;
        for category in &mode.categories {
            write_bins(
                &mut writer,
                &mode.mode,
                &category.category,
                &category.calibration,
            )?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn write_bins(
    writer: &mut csv::Writer<std::fs::File>,
    mode: &str,
    target: &str,
    summary: &CalibrationSummary,
) -> Result<()> {
    for bin in &summary.bins {
        writer.write_record([
            mode.to_string(),
            target.to_string(),
            format!("{:.2}", bin.lower),
            format!("{:.2}", bin.upper),
            bin.count.to_string(),
            format!("{:.8}", bin.mean_prediction),
            format!("{:.8}", bin.observed_rate),
            format!("{:.8}", bin.calibration_error),
        ])?;
    }
    Ok(())
}

fn write_correlation_csv(path: &PathBuf, report: &ValidationReport) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record(["mode", "category_a", "category_b", "correlation"])?;
    for mode in &report.modes {
        for i in 0..CATEGORY_COUNT {
            for j in 0..CATEGORY_COUNT {
                writer.write_record([
                    mode.mode.clone(),
                    DYNAMIC_CATEGORY_NAMES[i].to_string(),
                    DYNAMIC_CATEGORY_NAMES[j].to_string(),
                    format!("{:.8}", mode.outcome_correlation_matrix[i][j]),
                ])?;
            }
        }
    }
    writer.flush()?;
    Ok(())
}

fn print_summary(report: &ValidationReport) {
    for mode in &report.modes {
        println!();
        println!("{}", mode.mode);
        println!("  {}", mode.description);
        println!(
            "  samples {}/{} | avg pool {:.1} | matchup ties {:.2}%",
            mode.completed_samples,
            mode.requested_samples,
            mode.average_active_pool_size,
            mode.matchup_tie_rate * 100.0,
        );
        println!(
            "  H mean predicted {:6.2}% | observed {:6.2}% | Brier {:.4} | ECE {:5.2} pp | max-bin {:5.2} pp | spread {:5.2} pp",
            mode.h.mean_prediction * 100.0,
            mode.h.observed_rate * 100.0,
            mode.h.brier_score,
            mode.h.expected_calibration_error * 100.0,
            mode.h.maximum_bin_calibration_error * 100.0,
            mode.h.prediction_stddev * 100.0,
        );
        println!("  category calibration:");
        println!("    CAT    pred    actual   ECE     Brier   tie");
        for (index, category) in mode.categories.iter().enumerate() {
            println!(
                "    {:<4} {:6.2}% {:7.2}% {:6.2}pp  {:.4}  {:5.2}%",
                category.category,
                category.calibration.mean_prediction * 100.0,
                category.calibration.observed_rate * 100.0,
                category.calibration.expected_calibration_error * 100.0,
                category.calibration.brier_score,
                mode.category_tie_rates[index] * 100.0,
            );
        }
        println!("  strongest observed category-outcome correlations:");
        for pair in mode.strongest_outcome_correlations.iter().take(8) {
            println!(
                "    {:<4} / {:<4}  {:+.3}",
                pair.category_a, pair.category_b, pair.correlation
            );
        }
        println!("  H calibration bins:");
        for bin in &mode.h.bins {
            if bin.count == 0 {
                continue;
            }
            println!(
                "    {:>2.0}–{:>3.0}%  n={:>5}  pred={:6.2}%  actual={:6.2}%  Δ={:+6.2}pp",
                bin.lower * 100.0,
                bin.upper * 100.0,
                bin.count,
                bin.mean_prediction * 100.0,
                bin.observed_rate * 100.0,
                (bin.observed_rate - bin.mean_prediction) * 100.0,
            );
        }
    }
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator <= f64::EPSILON {
        0.0
    } else {
        numerator / denominator
    }
}
