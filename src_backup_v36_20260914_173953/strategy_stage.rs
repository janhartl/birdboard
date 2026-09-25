use crate::app::App;
use crate::durant::AuctionConfig;
use crate::player::PlayerId;
use crate::strategy_bank::{
    RuntimeStrategyBank, StrategyBankEntry, StrategyBankFile, StrategyBankSelection,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

const CATEGORY_COUNT: usize = 9;
const OPPONENT_COUNT: usize = 12;
const EARLY_LAST_ROSTER_SIZE: usize = 4;
const MID_LAST_ROSTER_SIZE: usize = 7;
const MID_SEED_COUNT: usize = 24;
const MID_GLOBAL_ESCAPE_COUNT: usize = 200;
const MID_MAX_STRATEGIES: usize = 1_200;
const EARLY_BANK_MAX_STRATEGIES: usize = 1_500;
const EARLY_TRAINING_TARGET_STATES: usize = 600;
const TRAINING_SCHEMA_VERSION: u32 = 1;
const DEFAULT_TRAINING_HOURS: f64 = 5.75;
const CATEGORY_ORDER: [&str; 9] = ["FG%", "FT%", "3PM", "PTS", "REB", "AST", "STL", "BLK", "TO"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeStrategyStage {
    EarlyGlobal,
    MidAdaptive,
    LateCompact,
}

impl RuntimeStrategyStage {
    pub fn for_roster_size(roster_size: usize) -> Self {
        if roster_size <= EARLY_LAST_ROSTER_SIZE {
            Self::EarlyGlobal
        } else if roster_size <= MID_LAST_ROSTER_SIZE {
            Self::MidAdaptive
        } else {
            Self::LateCompact
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::EarlyGlobal => "EARLY-global",
            Self::MidAdaptive => "MID-local",
            Self::LateCompact => "LATE-compact",
        }
    }
}

/// Build the mid-draft local strategy cloud around strategies that already look
/// promising for the CURRENT roster.  This is deliberately not a separate
/// lookup bank: it is a local refinement of the broad early vocabulary.
///
/// Each seed gets single-coordinate +/- .25 and +/- .50 probes (clamped to the
/// deep-search [0, 1.5] range).  We also retain an evenly sampled global escape
/// set so the optimizer can abandon a local basin if the auction changes.
pub fn build_mid_adaptive_weights(
    global_weights: &[[f64; CATEGORY_COUNT]],
    seed_weights: &[[f64; CATEGORY_COUNT]],
) -> Vec<[f64; CATEGORY_COUNT]> {
    let mut output = Vec::<[f64; CATEGORY_COUNT]>::with_capacity(MID_MAX_STRATEGIES);
    let mut seen = HashSet::<[i16; CATEGORY_COUNT]>::with_capacity(MID_MAX_STRATEGIES * 2);

    push_weight_unique(&mut output, &mut seen, [1.0; CATEGORY_COUNT]);

    const DELTAS: [f64; 4] = [-0.50, -0.25, 0.25, 0.50];
    for seed in seed_weights.iter().take(MID_SEED_COUNT) {
        if output.len() >= MID_MAX_STRATEGIES {
            break;
        }
        push_weight_unique(&mut output, &mut seen, *seed);
        for category in 0..CATEGORY_COUNT {
            for delta in DELTAS {
                if output.len() >= MID_MAX_STRATEGIES {
                    break;
                }
                let mut local = *seed;
                local[category] = quantize_quarter((local[category] + delta).clamp(0.0, 1.5));
                push_weight_unique(&mut output, &mut seen, local);
            }
        }
    }

    // Keep roughly 20% broad/global coverage. Evenly spaced sampling is
    // deterministic and avoids depending on file ordering beyond the fact that
    // learned banks are already ranked by usefulness.
    let global_take = MID_GLOBAL_ESCAPE_COUNT.min(global_weights.len());
    if global_take > 0 {
        for sample in 0..global_take {
            if output.len() >= MID_MAX_STRATEGIES {
                break;
            }
            let index = sample * global_weights.len() / global_take;
            push_weight_unique(
                &mut output,
                &mut seen,
                global_weights[index.min(global_weights.len() - 1)],
            );
        }
    }

    // If local deduplication left capacity, keep filling from the ranked global
    // bank rather than inventing increasingly exotic perturbations.
    for &weights in global_weights {
        if output.len() >= MID_MAX_STRATEGIES {
            break;
        }
        push_weight_unique(&mut output, &mut seen, weights);
    }

    output
}

fn push_weight_unique(
    output: &mut Vec<[f64; CATEGORY_COUNT]>,
    seen: &mut HashSet<[i16; CATEGORY_COUNT]>,
    weights: [f64; CATEGORY_COUNT],
) {
    if output.len() >= MID_MAX_STRATEGIES {
        return;
    }
    let key = weights.map(|weight| (weight * 100.0).round() as i16);
    if seen.insert(key) {
        output.push(weights);
    }
}

fn quantize_quarter(value: f64) -> f64 {
    (value * 4.0).round() / 4.0
}

pub fn print_stage_relearn_plan(app: &App) {
    let hours = requested_training_hours();
    println!();
    println!("========================================================================");
    println!("STAGE-AWARE j RELEARN — v34");
    println!("========================================================================");
    println!(
        "deep oracle vectors          : {}",
        app.durant.deep_strategy_count()
    );
    println!("synthetic early states       : {EARLY_TRAINING_TARGET_STATES}");
    println!("early roster sizes           : 0 through {EARLY_LAST_ROSTER_SIZE}");
    println!("early bank cap               : {EARLY_BANK_MAX_STRATEGIES}");
    println!("mid seeds                    : {MID_SEED_COUNT}");
    println!("mid adaptive cap             : {MID_MAX_STRATEGIES}");
    println!("mid global escape vectors    : {MID_GLOBAL_ESCAPE_COUNT}");
    println!(
        "late transition              : {}+ owned players",
        MID_LAST_ROSTER_SIZE + 1
    );
    println!("training budget this run     : {hours:.2} hours");
    println!(
        "Rayon worker threads         : {}",
        rayon::current_num_threads()
    );
    println!();
    println!("EARLY: deep/current-auction winners learned from heterogeneous states.");
    println!("MID  : dynamically refine the best current EARLY directions; no fixed mid bank.");
    println!("LATE : preserve the existing compact v30 runtime bank.");
    println!();
    println!("Training checkpoints after every state and is safe to rerun.");
    println!("Set BIRDBOARD_J_TRAIN_HOURS to change the per-run compute budget.");
}

pub fn train_stage_early_bank(app: &App) -> Result<()> {
    let Some(runtime_bank) = app.strategy_bank.as_ref() else {
        bail!("a compact runtime bank is required as the fixed opponent-pricing vocabulary");
    };
    if runtime_bank.weights.is_empty() {
        bail!("compact runtime strategy bank is empty");
    }

    let config = AuctionConfig::default();
    let output_dir = PathBuf::from("data")
        .join("stats")
        .join(&app.stats.draft_season)
        .join("durant");
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let checkpoint_path = output_dir.join("strategy_stage_early_training_checkpoint.json");
    let bank_path = output_dir.join("strategy_bank_stage_early.json");
    let census_path = output_dir.join("strategy_stage_early_census.csv");

    let mut checkpoint = load_checkpoint(&checkpoint_path, &app.stats.draft_season)?;
    let completed = checkpoint
        .observations
        .iter()
        .map(|observation| observation.state_index)
        .collect::<HashSet<_>>();

    let base_candidates = app
        .durant
        .scores
        .iter()
        .map(|score| score.player_id.clone())
        .collect::<Vec<_>>();
    if base_candidates.is_empty() {
        bail!("no DURANT players available for stage-aware training");
    }
    let initial_prices = initial_market_prices(app, &base_candidates, config);

    let max_hours = requested_training_hours();
    let started = Instant::now();

    println!();
    println!("========================================================================");
    println!("STAGE-AWARE EARLY j TRAINING — v34");
    println!("========================================================================");
    println!(
        "{} / {} states already checkpointed | deep={} | pricing bank={}",
        completed.len(),
        EARLY_TRAINING_TARGET_STATES,
        app.durant.deep_strategy_count(),
        runtime_bank.weights.len(),
    );
    println!("per-run compute budget: {max_hours:.2} hours");
    println!("Rayon worker threads: {}", rayon::current_num_threads());
    println!("deep j rollouts: parallel for searches >= 256 vectors");

    for state_index in 0..EARLY_TRAINING_TARGET_STATES {
        if completed.contains(&state_index) {
            continue;
        }
        if started.elapsed().as_secs_f64() >= max_hours * 3600.0 {
            println!("training budget reached; finalizing from completed checkpoints");
            break;
        }

        let state =
            build_training_state(app, state_index, &base_candidates, &initial_prices, config);
        let state_started = Instant::now();
        let Some((oracle, _restricted)) = app.durant.roster_plan_validation_common_market_pair(
            &state.own_roster,
            state.own_budget,
            &state.opponent_rosters,
            &state.opponent_budgets,
            &state.candidates,
            &runtime_bank.weights,
            config,
        ) else {
            println!("  [{:>3}] skipped — no valid oracle plan", state_index + 1);
            continue;
        };

        let mut top = Vec::<StageStrategyObservation>::with_capacity(3);
        top.push(StageStrategyObservation {
            rank: 1,
            weights: oracle.j_weights,
            h_pct: oracle.projected_matchup_win_probability * 100.0,
            build: oracle.build_name.clone(),
        });
        for (index, alternative) in oracle.j_alternatives.iter().take(2).enumerate() {
            top.push(StageStrategyObservation {
                rank: index + 2,
                weights: alternative.j_weights,
                h_pct: alternative.projected_matchup_win_probability * 100.0,
                build: alternative.build_name.clone(),
            });
        }

        let own_names = state
            .own_roster
            .iter()
            .filter_map(|player_id| app.durant.score_for(player_id))
            .map(|score| score.player_name.clone())
            .collect::<Vec<_>>();

        checkpoint.observations.push(StageTrainingObservation {
            state_index,
            roster_size: state.own_roster.len(),
            own_roster: own_names,
            own_budget: state.own_budget,
            drafted_players: state.drafted_players,
            strategies: top,
        });
        checkpoint.observations.sort_by_key(|row| row.state_index);
        save_checkpoint(&checkpoint_path, &checkpoint)?;

        println!(
            "  [{:>3}/{EARLY_TRAINING_TARGET_STATES}] roster={} drafted={} H={:>6.2}% | {:>5.1}s | checkpointed",
            state_index + 1,
            state.own_roster.len(),
            state.drafted_players,
            oracle.projected_matchup_win_probability * 100.0,
            state_started.elapsed().as_secs_f64(),
        );
    }

    let bank = build_early_bank_from_checkpoint(app, &checkpoint, runtime_bank)?;
    let json = serde_json::to_string_pretty(&bank)?;
    fs::write(&bank_path, json)
        .with_context(|| format!("failed to write {}", bank_path.display()))?;
    write_stage_census(&census_path, &checkpoint)?;

    println!();
    println!("SUMMARY");
    println!("checkpointed states : {}", checkpoint.observations.len());
    println!("early bank size     : {}", bank.strategies.len());
    println!("saved               : {}", bank_path.display());
    println!("census              : {}", census_path.display());
    println!("checkpoint          : {}", checkpoint_path.display());
    println!(
        "elapsed this run    : {:.2} hours",
        started.elapsed().as_secs_f64() / 3600.0
    );
    println!();
    println!("Restart BirdBoard after training so the new EARLY bank is loaded.");

    Ok(())
}

pub fn print_stage_validation_plan(app: &App) {
    let Some(bank) = app.strategy_bank.as_ref() else {
        println!("No runtime strategy bank loaded.");
        return;
    };
    println!();
    println!("========================================================================");
    println!("STAGE-AWARE j HOLDOUT VALIDATION — v34");
    println!("========================================================================");
    println!("deep oracle vectors : {}", app.durant.deep_strategy_count());
    println!("compact pricing core: {}", bank.weights.len());
    println!(
        "early bank          : {}",
        bank.early_weights_or_fallback().len()
    );
    println!("holdout roster sizes: 0 through 10");
    println!("EARLY 0-4 / MID adaptive 5-7 / LATE compact 8-10");
    println!();
    println!("Both oracle and stage-aware search use ONE common opponent-aware market");
    println!("priced by the compact bank. Only the j search vocabulary changes.");
}

pub fn validate_stage_aware_bank(app: &App) -> Result<()> {
    let bank = app
        .strategy_bank
        .as_ref()
        .context("no runtime strategy bank loaded")?;
    if bank.early_weights.is_empty() {
        bail!("no learned EARLY bank found; run --stage-j-train and restart BirdBoard first");
    }

    let config = AuctionConfig::default();
    let base_candidates = app
        .durant
        .scores
        .iter()
        .map(|score| score.player_id.clone())
        .collect::<Vec<_>>();
    let initial_prices = initial_market_prices(app, &base_candidates, config);
    let started = Instant::now();

    println!();
    println!("========================================================================");
    println!("STAGE-AWARE j HOLDOUT TEST — v34");
    println!("========================================================================");
    println!("11 unseen synthetic states | common compact pricing core | deep oracle");

    let mut regrets = Vec::<f64>::new();
    let mut stage_rows = Vec::<(usize, String, usize, f64)>::new();

    for roster_size in 0usize..=10 {
        let state_index = 10_000 + roster_size * 137;
        let state = build_synthetic_state(
            app,
            state_index,
            roster_size,
            roster_size.saturating_add(1).min(10),
            &base_candidates,
            &initial_prices,
            config,
        );
        let stage = RuntimeStrategyStage::for_roster_size(roster_size);
        let search_weights = stage_weights_for_state(app, bank, &state, stage, config);
        let state_started = Instant::now();
        let Some((oracle, restricted)) = app
            .durant
            .roster_plan_validation_common_market_pair_with_pricing(
                &state.own_roster,
                state.own_budget,
                &state.opponent_rosters,
                &state.opponent_budgets,
                &state.candidates,
                &search_weights,
                &bank.weights,
                config,
            )
        else {
            println!("  roster {roster_size:>2}: skipped — no valid plan");
            continue;
        };

        let regret = ((oracle.projected_matchup_win_probability
            - restricted.projected_matchup_win_probability)
            * 100.0)
            .max(0.0);
        regrets.push(regret);
        stage_rows.push((
            roster_size,
            stage.label().to_string(),
            search_weights.len(),
            regret,
        ));
        println!(
            "  roster {roster_size:>2} | {:<12} {:>4} j | oracle {:>6.3}% vs stage {:>6.3}% | regret {:>6.3} pp | {:>5.1}s",
            stage.label(),
            search_weights.len(),
            oracle.projected_matchup_win_probability * 100.0,
            restricted.projected_matchup_win_probability * 100.0,
            regret,
            state_started.elapsed().as_secs_f64(),
        );
    }

    if regrets.is_empty() {
        bail!("no stage-aware validation states completed");
    }
    regrets.sort_by(|a, b| a.total_cmp(b));
    let mean = regrets.iter().sum::<f64>() / regrets.len() as f64;
    let max = regrets.last().copied().unwrap_or(0.0);
    let p95_index = ((regrets.len() - 1) as f64 * 0.95).round() as usize;
    let p95 = regrets[p95_index.min(regrets.len() - 1)];
    let within_025 = regrets.iter().filter(|value| **value <= 0.25).count() as f64
        / regrets.len() as f64
        * 100.0;

    let output_dir = PathBuf::from("data")
        .join("stats")
        .join(&app.stats.draft_season)
        .join("durant");
    fs::create_dir_all(&output_dir)?;
    let csv_path = output_dir.join("strategy_stage_validation_v34.csv");
    let mut writer = csv::Writer::from_path(&csv_path)?;
    writer.write_record(["roster_size", "stage", "strategy_count", "regret_pp"])?;
    for (roster_size, stage, strategy_count, regret) in stage_rows {
        writer.write_record([
            roster_size.to_string(),
            stage,
            strategy_count.to_string(),
            format!("{regret:.6}"),
        ])?;
    }
    writer.flush()?;

    println!();
    println!("SUMMARY");
    println!("mean regret      : {mean:.4} pp");
    println!("p95 / max regret : {p95:.4} / {max:.4} pp");
    println!("within 0.25 pp   : {within_025:.1}%");
    println!(
        "wall time        : {:.2} min",
        started.elapsed().as_secs_f64() / 60.0
    );
    println!("report           : {}", csv_path.display());

    Ok(())
}

fn stage_weights_for_state(
    app: &App,
    bank: &RuntimeStrategyBank,
    state: &TrainingState,
    stage: RuntimeStrategyStage,
    config: AuctionConfig,
) -> Vec<[f64; CATEGORY_COUNT]> {
    match stage {
        RuntimeStrategyStage::EarlyGlobal => bank.early_weights_or_fallback().to_vec(),
        RuntimeStrategyStage::LateCompact => bank.weights.clone(),
        RuntimeStrategyStage::MidAdaptive => {
            let global = bank.early_weights_or_fallback();
            let seeds = app.durant.coarse_strategy_seed_weights(
                &state.own_roster,
                state.own_budget,
                &state.opponent_rosters,
                &state.opponent_budgets,
                &state.candidates,
                global,
                config,
                MID_SEED_COUNT,
            );
            build_mid_adaptive_weights(global, &seeds)
        }
    }
}

#[derive(Debug, Clone)]
struct TrainingState {
    own_roster: Vec<PlayerId>,
    own_budget: u16,
    opponent_rosters: Vec<Vec<PlayerId>>,
    opponent_budgets: Vec<u16>,
    candidates: Vec<PlayerId>,
    drafted_players: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StageTrainingCheckpoint {
    schema_version: u32,
    draft_season: String,
    target_states: usize,
    observations: Vec<StageTrainingObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StageTrainingObservation {
    state_index: usize,
    roster_size: usize,
    own_roster: Vec<String>,
    own_budget: u16,
    drafted_players: usize,
    strategies: Vec<StageStrategyObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StageStrategyObservation {
    rank: usize,
    weights: [f64; CATEGORY_COUNT],
    h_pct: f64,
    build: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StrategyKey([i16; CATEGORY_COUNT]);

impl StrategyKey {
    fn from_weights(weights: [f64; CATEGORY_COUNT]) -> Self {
        Self(weights.map(|weight| (weight * 100.0).round() as i16))
    }
}

#[derive(Debug, Default)]
struct LearnedEntry {
    weights: [f64; CATEGORY_COUNT],
    wins: usize,
    top3: usize,
    h_sum: f64,
    roster_sizes: HashSet<usize>,
    examples: Vec<String>,
}

fn load_checkpoint(path: &std::path::Path, draft_season: &str) -> Result<StageTrainingCheckpoint> {
    if !path.exists() {
        return Ok(StageTrainingCheckpoint {
            schema_version: TRAINING_SCHEMA_VERSION,
            draft_season: draft_season.to_string(),
            target_states: EARLY_TRAINING_TARGET_STATES,
            observations: Vec::new(),
        });
    }

    let json =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let checkpoint: StageTrainingCheckpoint = serde_json::from_str(&json)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if checkpoint.draft_season != draft_season {
        bail!(
            "stage-training checkpoint season {} does not match {}",
            checkpoint.draft_season,
            draft_season
        );
    }
    Ok(checkpoint)
}

fn save_checkpoint(path: &std::path::Path, checkpoint: &StageTrainingCheckpoint) -> Result<()> {
    let json = serde_json::to_string_pretty(checkpoint)?;
    fs::write(path, json).with_context(|| format!("failed to write {}", path.display()))
}

fn requested_training_hours() -> f64 {
    std::env::var("BIRDBOARD_J_TRAIN_HOURS")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_TRAINING_HOURS)
}

fn build_training_state(
    app: &App,
    state_index: usize,
    base_candidates: &[PlayerId],
    initial_prices: &HashMap<PlayerId, u16>,
    config: AuctionConfig,
) -> TrainingState {
    let own_target = state_index % (EARLY_LAST_ROSTER_SIZE + 1);
    build_synthetic_state(
        app,
        state_index,
        own_target,
        EARLY_LAST_ROSTER_SIZE,
        base_candidates,
        initial_prices,
        config,
    )
}

fn build_synthetic_state(
    app: &App,
    state_index: usize,
    own_target: usize,
    opponent_size_cap: usize,
    base_candidates: &[PlayerId],
    initial_prices: &HashMap<PlayerId, u16>,
    config: AuctionConfig,
) -> TrainingState {
    let own_target = own_target.min(app.durant.team_size());
    let opponent_size_cap = opponent_size_cap.min(app.durant.team_size());
    let mut target_sizes = [0usize; OPPONENT_COUNT + 1];
    target_sizes[0] = own_target;
    const DELTAS: [i8; OPPONENT_COUNT] = [0, 1, -1, 0, 1, 0, -1, 1, 0, -1, 0, 1];
    for opponent in 0..OPPONENT_COUNT {
        let rotated = DELTAS[(opponent + state_index) % OPPONENT_COUNT];
        target_sizes[opponent + 1] =
            (own_target as i16 + rotated as i16).clamp(0, opponent_size_cap as i16) as usize;
    }

    let mut pool = base_candidates.to_vec();
    let mut rosters = vec![Vec::<PlayerId>::new(); OPPONENT_COUNT + 1];
    let mut rng = SplitMix64::new(0xB17D_B04D_5EED_0001u64 ^ state_index as u64);

    // Force broad coverage of leading players as an own-roster anchor whenever
    // the state has at least one player. The rest of the state remains random.
    if own_target > 0 && !pool.is_empty() {
        let anchor_span = pool.len().min(96);
        let anchor_index = (state_index / (EARLY_LAST_ROSTER_SIZE + 1)) % anchor_span;
        rosters[0].push(pool.remove(anchor_index));
    }

    let max_rounds = target_sizes.iter().copied().max().unwrap_or(0);
    for round in 0..max_rounds {
        let mut team_order = (0..=OPPONENT_COUNT).collect::<Vec<_>>();
        let rotation = (state_index + round * 3) % team_order.len();
        team_order.rotate_left(rotation);
        if (state_index + round) % 2 == 1 {
            team_order.reverse();
        }

        for team in team_order {
            if rosters[team].len() >= target_sizes[team] || pool.is_empty() {
                continue;
            }
            let window = pool.len().min(24);
            let pick_index = (rng.next_u64() as usize) % window;
            rosters[team].push(pool.remove(pick_index));
        }
    }

    let own_roster = rosters.remove(0);
    let opponent_rosters = rosters;
    let own_budget = remaining_budget_from_initial_prices(
        &own_roster,
        initial_prices,
        app.durant.team_size(),
        config,
    );
    let opponent_budgets = opponent_rosters
        .iter()
        .map(|roster| {
            remaining_budget_from_initial_prices(
                roster,
                initial_prices,
                app.durant.team_size(),
                config,
            )
        })
        .collect::<Vec<_>>();

    let mut drafted = own_roster.iter().cloned().collect::<HashSet<_>>();
    for roster in &opponent_rosters {
        drafted.extend(roster.iter().cloned());
    }
    let candidates = base_candidates
        .iter()
        .filter(|player_id| !drafted.contains(*player_id))
        .cloned()
        .collect::<Vec<_>>();

    TrainingState {
        own_roster,
        own_budget,
        opponent_rosters,
        opponent_budgets,
        candidates,
        drafted_players: drafted.len(),
    }
}

fn initial_market_prices(
    app: &App,
    candidates: &[PlayerId],
    config: AuctionConfig,
) -> HashMap<PlayerId, u16> {
    let opponent_rosters = vec![Vec::<PlayerId>::new(); OPPONENT_COUNT];
    let opponent_budgets = vec![config.starting_budget; OPPONENT_COUNT];
    app.durant
        .market_board(
            &[],
            config.starting_budget,
            &opponent_rosters,
            &opponent_budgets,
            candidates,
            config,
        )
        .values
        .into_iter()
        .map(|value| (value.player_id, value.market_price))
        .collect()
}

fn remaining_budget_from_initial_prices(
    roster: &[PlayerId],
    initial_prices: &HashMap<PlayerId, u16>,
    team_size: usize,
    config: AuctionConfig,
) -> u16 {
    let open_slots = team_size.saturating_sub(roster.len());
    let reserve = (open_slots as u32).saturating_mul(config.minimum_bid as u32);
    let max_spend = (config.starting_budget as u32).saturating_sub(reserve);
    let modeled_spend = roster
        .iter()
        .map(|player_id| {
            initial_prices
                .get(player_id)
                .copied()
                .unwrap_or(config.minimum_bid) as u32
        })
        .sum::<u32>()
        .min(max_spend);

    (config.starting_budget as u32)
        .saturating_sub(modeled_spend)
        .min(u16::MAX as u32) as u16
}

fn build_early_bank_from_checkpoint(
    app: &App,
    checkpoint: &StageTrainingCheckpoint,
    runtime_bank: &RuntimeStrategyBank,
) -> Result<StrategyBankFile> {
    let mut census = HashMap::<StrategyKey, LearnedEntry>::new();

    for observation in &checkpoint.observations {
        for strategy in &observation.strategies {
            let key = StrategyKey::from_weights(strategy.weights);
            let entry = census.entry(key).or_default();
            entry.weights = strategy.weights;
            entry.top3 += 1;
            entry.h_sum += strategy.h_pct / 100.0;
            entry.roster_sizes.insert(observation.roster_size);
            if strategy.rank == 1 {
                entry.wins += 1;
            }
            if entry.examples.len() < 3 {
                let roster = if observation.own_roster.is_empty() {
                    "EMPTY".to_string()
                } else {
                    observation.own_roster.join(" + ")
                };
                entry
                    .examples
                    .push(format!("r{} / {}", observation.roster_size, roster));
            }
        }
    }

    // Always retain the balanced strategy and then use the legacy compact bank
    // as a final global safety net if the deep observations do not fill the cap.
    let balanced_key = StrategyKey::from_weights([1.0; CATEGORY_COUNT]);
    census.entry(balanced_key).or_insert_with(|| LearnedEntry {
        weights: [1.0; CATEGORY_COUNT],
        ..LearnedEntry::default()
    });

    let mut rows = census.into_iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        b.1.wins
            .cmp(&a.1.wins)
            .then_with(|| b.1.top3.cmp(&a.1.top3))
            .then_with(|| b.1.roster_sizes.len().cmp(&a.1.roster_sizes.len()))
    });

    let mut selected = Vec::<LearnedEntry>::new();
    let mut seen = HashSet::<StrategyKey>::new();
    for (key, entry) in rows {
        if selected.len() >= EARLY_BANK_MAX_STRATEGIES {
            break;
        }
        if seen.insert(key) {
            selected.push(entry);
        }
    }

    for &weights in &runtime_bank.weights {
        if selected.len() >= EARLY_BANK_MAX_STRATEGIES {
            break;
        }
        let key = StrategyKey::from_weights(weights);
        if seen.insert(key) {
            selected.push(LearnedEntry {
                weights,
                ..LearnedEntry::default()
            });
        }
    }

    let total_wins = checkpoint.observations.len().max(1);
    let strategies = selected
        .into_iter()
        .enumerate()
        .map(|(index, entry)| StrategyBankEntry {
            bank_rank: index + 1,
            weights: entry.weights,
            best_wins: entry.wins,
            top_three_appearances: entry.top3,
            state_count: entry.roster_sizes.len(),
            average_margin_pp: 0.0,
            average_h_pct: if entry.top3 == 0 {
                0.0
            } else {
                entry.h_sum / entry.top3 as f64 * 100.0
            },
            best_decision_share: entry.wins as f64 / total_wins as f64,
            selection_reasons: if entry.wins > 0 {
                vec!["deep_early_winner".to_string()]
            } else if entry.top3 > 0 {
                vec!["deep_early_top3".to_string()]
            } else {
                vec!["legacy_global_escape".to_string()]
            },
            examples: entry.examples,
        })
        .collect::<Vec<_>>();

    let selected_wins = strategies
        .iter()
        .map(|entry| entry.best_wins)
        .sum::<usize>();
    Ok(StrategyBankFile {
        schema_version: 4,
        search_profile: format!(
            "stage_early_deep_current_auction_{}_states",
            checkpoint.observations.len()
        ),
        draft_season: app.stats.draft_season.clone(),
        category_order: CATEGORY_ORDER.map(str::to_string),
        searched_strategy_count: app.durant.deep_strategy_count(),
        scenario_count: checkpoint.observations.len(),
        best_observations: checkpoint.observations.len(),
        top_three_observations: checkpoint
            .observations
            .iter()
            .map(|row| row.strategies.len())
            .sum(),
        distinct_best: strategies
            .iter()
            .filter(|entry| entry.best_wins > 0)
            .count(),
        distinct_top_three: strategies
            .iter()
            .filter(|entry| entry.top_three_appearances > 0)
            .count(),
        selection: StrategyBankSelection {
            max_live_strategies: EARLY_BANK_MAX_STRATEGIES,
            coverage_target: 1.0,
            niche_min_wins: 0,
            niche_min_average_margin_pp: 0.0,
            selected_count: strategies.len(),
            selected_best_coverage: selected_wins as f64 / total_wins as f64,
            core_count: strategies
                .iter()
                .filter(|entry| entry.best_wins > 0)
                .count(),
        },
        strategies,
    })
}

fn write_stage_census(path: &std::path::Path, checkpoint: &StageTrainingCheckpoint) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)
        .with_context(|| format!("failed to create {}", path.display()))?;
    writer.write_record([
        "state_index",
        "roster_size",
        "own_budget",
        "drafted_players",
        "rank",
        "fg_pct",
        "ft_pct",
        "threes",
        "points",
        "rebounds",
        "assists",
        "steals",
        "blocks",
        "turnovers",
        "h_pct",
        "build",
        "own_roster",
    ])?;

    for observation in &checkpoint.observations {
        for strategy in &observation.strategies {
            let mut row = vec![
                observation.state_index.to_string(),
                observation.roster_size.to_string(),
                observation.own_budget.to_string(),
                observation.drafted_players.to_string(),
                strategy.rank.to_string(),
            ];
            row.extend(strategy.weights.iter().map(|weight| format!("{weight:.2}")));
            row.push(format!("{:.6}", strategy.h_pct));
            row.push(strategy.build.clone());
            row.push(observation.own_roster.join(" + "));
            writer.write_record(row)?;
        }
    }
    writer.flush()?;
    Ok(())
}

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }
}
