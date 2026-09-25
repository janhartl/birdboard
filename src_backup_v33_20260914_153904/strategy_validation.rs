use crate::app::App;
use crate::durant::{AuctionConfig, DurantRosterPlan};
use crate::player::PlayerId;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

const VALIDATION_SCHEMA_VERSION: u32 = 3;
const CATEGORY_COUNT: usize = 9;
const OPPONENT_COUNT: usize = 12;

/// Clean holdouts for the CURRENT v30 Strategy environment.
///
/// None of these exact own-roster states appear in AUCTION_CENSUS_SCENARIOS.
/// The states deliberately span early (2-3), middle (4-7), and late (8-10)
/// intentional-roster construction. Opponent rosters/budgets are generated
/// deterministically from the current DURANT-ranked pool for each state.
const HOLDOUT_SCENARIOS: &[(&str, &[&str])] = &[
    (
        "EARLY 2 — MAXEY + JALEN J",
        &["Tyrese Maxey", "Jalen Johnson"],
    ),
    (
        "EARLY 3 — SCOTTIE + WHITE + DUREN",
        &["Scottie Barnes", "Derrick White", "Jalen Duren"],
    ),
    (
        "MID 4 — LUKA + BAM + TREY + WARE",
        &[
            "Luka Dončić",
            "Bam Adebayo",
            "Trey Murphy III",
            "Kel'el Ware",
        ],
    ),
    (
        "MID 5 — WEMBY + BANE + QUICKLEY + CAMARA + CLINGAN",
        &[
            "Victor Wembanyama",
            "Desmond Bane",
            "Immanuel Quickley",
            "Toumani Camara",
            "Donovan Clingan",
        ],
    ),
    (
        "MID 6 — SGA + MOBLEY + WHITE + GIDDEY + WARE + MATAS",
        &[
            "Shai Gilgeous-Alexander",
            "Evan Mobley",
            "Derrick White",
            "Josh Giddey",
            "Kel'el Ware",
            "Matas Buzelis",
        ],
    ),
    (
        "MID 7 — MAXEY + JALEN J + CHET + OG + DUREN + CAMARA + PRITCHARD",
        &[
            "Tyrese Maxey",
            "Jalen Johnson",
            "Chet Holmgren",
            "OG Anunoby",
            "Jalen Duren",
            "Toumani Camara",
            "Payton Pritchard",
        ],
    ),
    (
        "LATE 8 — CADE + KAT + BANE + WHITE + TREY + CLINGAN + CAMARA + WARE",
        &[
            "Cade Cunningham",
            "Karl-Anthony Towns",
            "Desmond Bane",
            "Derrick White",
            "Trey Murphy III",
            "Donovan Clingan",
            "Toumani Camara",
            "Kel'el Ware",
        ],
    ),
    (
        "LATE 9 — SCOTTIE + MITCHELL + MOBLEY + BANE + QUICKLEY + DUREN + MATAS + CAMARA + WARE",
        &[
            "Scottie Barnes",
            "Donovan Mitchell",
            "Evan Mobley",
            "Desmond Bane",
            "Immanuel Quickley",
            "Jalen Duren",
            "Matas Buzelis",
            "Toumani Camara",
            "Kel'el Ware",
        ],
    ),
    (
        "LATE 10 — LUKA + JALEN J + BAM + WHITE + OG + GIDDEY + CLINGAN + MATAS + CAMARA + WARE",
        &[
            "Luka Dončić",
            "Jalen Johnson",
            "Bam Adebayo",
            "Derrick White",
            "OG Anunoby",
            "Josh Giddey",
            "Donovan Clingan",
            "Matas Buzelis",
            "Toumani Camara",
            "Kel'el Ware",
        ],
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StrategyKey([i32; CATEGORY_COUNT]);

impl StrategyKey {
    fn from_weights(weights: [f64; CATEGORY_COUNT]) -> Self {
        Self(weights.map(|weight| (weight * 1000.0).round() as i32))
    }
}

#[derive(Debug, Clone)]
struct HoldoutState {
    name: String,
    own_roster: Vec<PlayerId>,
    own_roster_names: Vec<String>,
    own_budget: u16,
    opponent_rosters: Vec<Vec<PlayerId>>,
    opponent_budgets: Vec<u16>,
    candidates: Vec<PlayerId>,
    drafted_players: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationReport {
    schema_version: u32,
    environment: String,
    draft_season: String,
    runtime_bank_source: String,
    runtime_bank_profile: String,
    deep_strategy_count: usize,
    runtime_bank_strategy_count: usize,
    requested_holdout_scenarios: usize,
    completed_holdout_scenarios: usize,
    wall_time_seconds: f64,
    aggregate: AggregateSummary,
    scenarios: Vec<ScenarioSummary>,
    worst_h_differences: Vec<PlanComparison>,
}

#[derive(Debug, Clone, Serialize)]
struct ScenarioSummary {
    scenario: String,
    roster_size: usize,
    own_roster: Vec<String>,
    own_budget: u16,
    drafted_players: usize,
    available_players: usize,
    opponent_roster_sizes: Vec<usize>,
    opponent_budgets: Vec<u16>,
    common_market_seconds: f64,
    runtime_seconds: f64,
    comparison: PlanComparison,
}

#[derive(Debug, Clone, Serialize)]
struct AggregateSummary {
    states: usize,
    mean_bank_regret_pp: f64,
    median_bank_regret_pp: f64,
    p95_bank_regret_pp: f64,
    max_bank_regret_pp: f64,
    mean_bank_absolute_h_delta_pp: f64,
    p95_bank_absolute_h_delta_pp: f64,
    max_bank_absolute_h_delta_pp: f64,
    within_0_05_pp_pct: f64,
    within_0_10_pp_pct: f64,
    within_0_25_pp_pct: f64,
    within_0_50_pp_pct: f64,
    deep_winner_present_pct: f64,
    exact_j_match_pct: f64,
    build_match_pct: f64,
    mean_top3_j_overlap: f64,
    mean_future_player_jaccard_pct: f64,
    mean_max_category_probability_delta_pp: f64,
    max_category_probability_delta_pp: f64,
    bank_above_oracle_states: usize,
    mean_runtime_absolute_h_delta_pp: f64,
    p95_runtime_absolute_h_delta_pp: f64,
    max_runtime_absolute_h_delta_pp: f64,
    runtime_above_oracle_states: usize,
}

#[derive(Debug, Clone, Serialize)]
struct PlanComparison {
    scenario: String,
    roster_size: usize,
    oracle_h_pct: f64,
    bank_common_h_pct: f64,
    runtime_h_pct: f64,
    oracle_minus_bank_pp: f64,
    bank_regret_pp: f64,
    bank_absolute_h_delta_pp: f64,
    oracle_minus_runtime_pp: f64,
    runtime_absolute_h_delta_pp: f64,
    deep_winner_present: bool,
    exact_j_match: bool,
    build_match: bool,
    top3_j_overlap: usize,
    max_category_probability_delta_pp: f64,
    future_player_jaccard_pct: f64,
    future_spend_delta_dollars: i32,
    budget_left_delta_dollars: i32,
    oracle_j_name: String,
    bank_j_name: String,
    runtime_j_name: String,
    oracle_build: String,
    bank_build: String,
    runtime_build: String,
    oracle_j_weights: [f64; CATEGORY_COUNT],
    bank_j_weights: [f64; CATEGORY_COUNT],
    runtime_j_weights: [f64; CATEGORY_COUNT],
    oracle_future_players: Vec<String>,
    bank_future_players: Vec<String>,
    runtime_future_players: Vec<String>,
}

pub fn print_plan(app: &App) {
    let Some(bank) = app.strategy_bank.as_ref() else {
        println!("No runtime strategy bank is loaded.");
        return;
    };

    println!();
    println!("========================================================================");
    println!("CURRENT-AUCTION j-BANK VALIDATION (v32)");
    println!("========================================================================");
    println!("environment                : current H + budgets + 10 intentional slots");
    println!(
        "oracle search              : {} j vectors",
        app.durant.deep_strategy_count()
    );
    println!(
        "runtime bank               : {} j vectors",
        bank.weights.len()
    );
    println!(
        "runtime source             : {}",
        bank.source_path.display()
    );
    println!("runtime profile            : {}", bank.search_profile);
    println!("holdout states             : {}", HOLDOUT_SCENARIOS.len());
    println!("own-roster sizes           : 2 through 10 players");
    println!("opponents                  : 12 heterogeneous generated rosters/budgets");
    println!();
    println!("Primary bank test:");
    println!("  1. Build ONE opponent-aware rational market for every remaining player.");
    println!("  2. Search both the full 212,941-j space and the runtime bank against");
    println!("     that identical fixed market.");
    println!("  3. Report H regret of the restricted bank relative to the deep oracle.");
    println!();
    println!("Secondary runtime test:");
    println!("  Compare the actual fast v30 two-stage Strategy planner with the same");
    println!("  deep/common-market oracle. This measures bank + pricing-shortcut error.");
    println!();
    println!("Unlike v31, the oracle market does NOT depend on which top-3 strategies");
    println!("survive the coarse search, so bank-vs-oracle is a valid restricted-search test.");
}

pub fn validate_and_save(app: &App) -> Result<()> {
    let run_started = Instant::now();
    let bank = app
        .strategy_bank
        .as_ref()
        .context("no runtime strategy bank is loaded")?;
    if bank.weights.is_empty() {
        bail!("runtime strategy bank contains no strategies");
    }

    let bank_keys = bank
        .weights
        .iter()
        .copied()
        .map(StrategyKey::from_weights)
        .collect::<HashSet<_>>();

    let config = AuctionConfig::default();
    let base_candidates = app
        .players
        .iter()
        .filter(|player| app.durant.score_for(&player.id).is_some())
        .map(|player| player.id.clone())
        .collect::<Vec<_>>();
    if base_candidates.is_empty() {
        bail!("no DURANT-scored runtime players are available for validation");
    }

    let initial_prices = initial_market_prices(app, &base_candidates, config);

    println!();
    println!("========================================================================");
    println!("CURRENT AUCTION j-BANK HOLDOUT TEST — v32 CORRECTED ORACLE");
    println!("========================================================================");
    println!(
        "{} clean states | oracle={} | runtime bank={} | one common rational market/state",
        HOLDOUT_SCENARIOS.len(),
        app.durant.deep_strategy_count(),
        bank.weights.len(),
    );
    println!("NOTE: this is intentionally slower than v31 because all remaining players");
    println!("      receive opponent-aware prices once per holdout state.");

    let mut comparisons = Vec::<PlanComparison>::new();
    let mut scenarios = Vec::<ScenarioSummary>::new();

    for (index, (scenario_name, roster_names)) in HOLDOUT_SCENARIOS.iter().enumerate() {
        let Some(state) = build_holdout_state(
            app,
            scenario_name,
            roster_names,
            &base_candidates,
            &initial_prices,
            config,
        ) else {
            continue;
        };

        println!();
        println!(
            "  [{:>2}/{}] {}",
            index + 1,
            HOLDOUT_SCENARIOS.len(),
            state.name,
        );
        println!(
            "           own {} players / ${} | drafted {} | available {}",
            state.own_roster.len(),
            state.own_budget,
            state.drafted_players,
            state.candidates.len(),
        );

        let common_started = Instant::now();
        let common_pair = app.durant.roster_plan_validation_common_market_pair(
            &state.own_roster,
            state.own_budget,
            &state.opponent_rosters,
            &state.opponent_budgets,
            &state.candidates,
            &bank.weights,
            config,
        );
        let common_market_seconds = common_started.elapsed().as_secs_f64();

        let runtime_started = Instant::now();
        let runtime_plan = app.durant.roster_plan_validation_with_strategy_weights(
            &state.own_roster,
            state.own_budget,
            &state.opponent_rosters,
            &state.opponent_budgets,
            &state.candidates,
            &bank.weights,
            &bank.weights,
            config,
        );
        let runtime_seconds = runtime_started.elapsed().as_secs_f64();

        let (Some((oracle, bank_common)), Some(runtime_plan)) = (common_pair, runtime_plan) else {
            println!("           SKIPPED — planner returned no valid plan");
            continue;
        };

        let comparison = compare_plans(
            &state,
            &oracle,
            &bank_common,
            &runtime_plan,
            &bank_keys,
            app,
        );

        println!(
            "           common {:>7.2}s | runtime {:>6.2}s | H oracle {:>6.3}% | bank {:>6.3}% | live {:>6.3}%",
            common_market_seconds,
            runtime_seconds,
            comparison.oracle_h_pct,
            comparison.bank_common_h_pct,
            comparison.runtime_h_pct,
        );
        println!(
            "           BANK regret {:>6.3} pp | |ΔH| {:>6.3} pp | runtime |ΔH| {:>6.3} pp",
            comparison.bank_regret_pp,
            comparison.bank_absolute_h_delta_pp,
            comparison.runtime_absolute_h_delta_pp,
        );
        println!(
            "           j exact={} present={} top3-overlap={}/3 | build={} | future overlap {:>5.1}%",
            comparison.exact_j_match,
            comparison.deep_winner_present,
            comparison.top3_j_overlap,
            comparison.build_match,
            comparison.future_player_jaccard_pct,
        );

        scenarios.push(ScenarioSummary {
            scenario: state.name.clone(),
            roster_size: state.own_roster.len(),
            own_roster: state.own_roster_names.clone(),
            own_budget: state.own_budget,
            drafted_players: state.drafted_players,
            available_players: state.candidates.len(),
            opponent_roster_sizes: state.opponent_rosters.iter().map(Vec::len).collect(),
            opponent_budgets: state.opponent_budgets.clone(),
            common_market_seconds,
            runtime_seconds,
            comparison: comparison.clone(),
        });
        comparisons.push(comparison);
    }

    if comparisons.is_empty() {
        bail!("none of the current-environment holdout scenarios completed");
    }

    let aggregate = summarize(&comparisons);
    let mut worst = comparisons.clone();
    worst.sort_by(|a, b| {
        b.bank_absolute_h_delta_pp
            .total_cmp(&a.bank_absolute_h_delta_pp)
    });
    worst.truncate(12);

    let report = ValidationReport {
        schema_version: VALIDATION_SCHEMA_VERSION,
        environment: "current_auction_common_rational_market_v32".to_string(),
        draft_season: app.stats.draft_season.clone(),
        runtime_bank_source: bank.source_path.display().to_string(),
        runtime_bank_profile: bank.search_profile.clone(),
        deep_strategy_count: app.durant.deep_strategy_count(),
        runtime_bank_strategy_count: bank.weights.len(),
        requested_holdout_scenarios: HOLDOUT_SCENARIOS.len(),
        completed_holdout_scenarios: scenarios.len(),
        wall_time_seconds: run_started.elapsed().as_secs_f64(),
        aggregate,
        scenarios,
        worst_h_differences: worst,
    };

    let output_dir = PathBuf::from("data")
        .join("stats")
        .join(&app.stats.draft_season)
        .join("durant");
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let json_path = output_dir.join("strategy_bank_validation_current_v32.json");
    let json = serde_json::to_string_pretty(&report)?;
    fs::write(&json_path, json)
        .with_context(|| format!("failed to write {}", json_path.display()))?;

    let csv_path = output_dir.join("strategy_bank_validation_current_v32.csv");
    write_csv(&csv_path, &comparisons)?;

    println!();
    println!("SUMMARY — PRIMARY: j-BANK VOCABULARY UNDER COMMON MARKET");
    println!(
        "completed states      : {}/{}",
        report.completed_holdout_scenarios, report.requested_holdout_scenarios
    );
    println!(
        "mean bank regret      : {:.4} pp",
        report.aggregate.mean_bank_regret_pp
    );
    println!(
        "p95 / max regret      : {:.4} / {:.4} pp",
        report.aggregate.p95_bank_regret_pp, report.aggregate.max_bank_regret_pp
    );
    println!(
        "mean bank |ΔH|        : {:.4} pp",
        report.aggregate.mean_bank_absolute_h_delta_pp
    );
    println!(
        "p95 / max bank |ΔH|   : {:.4} / {:.4} pp",
        report.aggregate.p95_bank_absolute_h_delta_pp,
        report.aggregate.max_bank_absolute_h_delta_pp
    );
    println!(
        "within 0.10 pp        : {:.1}%",
        report.aggregate.within_0_10_pp_pct
    );
    println!(
        "within 0.25 pp        : {:.1}%",
        report.aggregate.within_0_25_pp_pct
    );
    println!(
        "deep winner in bank   : {:.1}%",
        report.aggregate.deep_winner_present_pct
    );
    println!(
        "exact j match         : {:.1}%",
        report.aggregate.exact_j_match_pct
    );
    println!(
        "build match           : {:.1}%",
        report.aggregate.build_match_pct
    );
    println!(
        "mean top-3 j overlap  : {:.2}/3",
        report.aggregate.mean_top3_j_overlap
    );
    println!(
        "mean future overlap   : {:.1}%",
        report.aggregate.mean_future_player_jaccard_pct
    );
    println!(
        "bank > oracle states  : {}",
        report.aggregate.bank_above_oracle_states
    );
    println!();
    println!("SECONDARY: ACTUAL v30 FAST PLANNER VS ORACLE");
    println!(
        "mean runtime |ΔH|     : {:.4} pp",
        report.aggregate.mean_runtime_absolute_h_delta_pp
    );
    println!(
        "p95 / max runtime     : {:.4} / {:.4} pp",
        report.aggregate.p95_runtime_absolute_h_delta_pp,
        report.aggregate.max_runtime_absolute_h_delta_pp
    );
    println!(
        "runtime > oracle      : {} states",
        report.aggregate.runtime_above_oracle_states
    );
    println!();
    println!("report                : {}", json_path.display());
    println!("scenario CSV          : {}", csv_path.display());
    println!(
        "wall time             : {:.2} min",
        report.wall_time_seconds / 60.0
    );

    Ok(())
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

fn build_holdout_state(
    app: &App,
    scenario_name: &str,
    roster_names: &[&str],
    base_candidates: &[PlayerId],
    initial_prices: &HashMap<PlayerId, u16>,
    config: AuctionConfig,
) -> Option<HoldoutState> {
    let mut own_roster = Vec::with_capacity(roster_names.len());
    let mut missing = Vec::new();
    for name in roster_names {
        match app
            .durant
            .scores
            .iter()
            .find(|score| score.player_name == *name)
        {
            Some(score) => own_roster.push(score.player_id.clone()),
            None => missing.push((*name).to_string()),
        }
    }
    if !missing.is_empty() {
        println!(
            "           SKIPPED {scenario_name} — missing DURANT player(s): {}",
            missing.join(", ")
        );
        return None;
    }

    let own_set = own_roster.iter().cloned().collect::<HashSet<_>>();
    let ranked_pool = base_candidates
        .iter()
        .filter(|player_id| !own_set.contains(*player_id))
        .cloned()
        .collect::<Vec<_>>();

    let target_sizes = opponent_target_sizes(own_roster.len(), app.durant.team_size());
    let mut opponent_rosters = vec![Vec::<PlayerId>::new(); OPPONENT_COUNT];
    let mut cursor = 0usize;
    let max_rounds = target_sizes.iter().copied().max().unwrap_or(0);

    // Snake allocation prevents one synthetic opponent from always receiving
    // the best player of each round while still making the state deterministic.
    for round in 0..max_rounds {
        if round % 2 == 0 {
            for team_index in 0..OPPONENT_COUNT {
                if round >= target_sizes[team_index] || cursor >= ranked_pool.len() {
                    continue;
                }
                opponent_rosters[team_index].push(ranked_pool[cursor].clone());
                cursor += 1;
            }
        } else {
            for team_index in (0..OPPONENT_COUNT).rev() {
                if round >= target_sizes[team_index] || cursor >= ranked_pool.len() {
                    continue;
                }
                opponent_rosters[team_index].push(ranked_pool[cursor].clone());
                cursor += 1;
            }
        }
    }

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

    let mut drafted = own_set;
    for roster in &opponent_rosters {
        drafted.extend(roster.iter().cloned());
    }
    let candidates = base_candidates
        .iter()
        .filter(|player_id| !drafted.contains(*player_id))
        .cloned()
        .collect::<Vec<_>>();

    let own_roster_names = own_roster
        .iter()
        .filter_map(|player_id| app.durant.score_for(player_id))
        .map(|score| score.player_name.clone())
        .collect::<Vec<_>>();

    Some(HoldoutState {
        name: scenario_name.to_string(),
        own_roster,
        own_roster_names,
        own_budget,
        opponent_rosters,
        opponent_budgets,
        candidates,
        drafted_players: drafted.len(),
    })
}

fn opponent_target_sizes(own_size: usize, team_size: usize) -> [usize; OPPONENT_COUNT] {
    const DELTAS: [i8; OPPONENT_COUNT] = [0, 1, -1, 0, 1, 0, -1, 1, 0, -1, 0, 1];
    let mut sizes = [0usize; OPPONENT_COUNT];
    for (index, delta) in DELTAS.iter().enumerate() {
        let size = (own_size as i16 + *delta as i16).clamp(0, team_size as i16) as usize;
        sizes[index] = size;
    }
    sizes
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

fn compare_plans(
    state: &HoldoutState,
    oracle: &DurantRosterPlan,
    bank: &DurantRosterPlan,
    runtime: &DurantRosterPlan,
    bank_keys: &HashSet<StrategyKey>,
    app: &App,
) -> PlanComparison {
    let oracle_key = StrategyKey::from_weights(oracle.j_weights);
    let bank_key = StrategyKey::from_weights(bank.j_weights);
    let oracle_minus_bank_pp =
        (oracle.projected_matchup_win_probability - bank.projected_matchup_win_probability) * 100.0;
    let bank_regret_pp = oracle_minus_bank_pp.max(0.0);
    let bank_absolute_h_delta_pp = oracle_minus_bank_pp.abs();
    let oracle_minus_runtime_pp = (oracle.projected_matchup_win_probability
        - runtime.projected_matchup_win_probability)
        * 100.0;
    let runtime_absolute_h_delta_pp = oracle_minus_runtime_pp.abs();

    let max_category_probability_delta_pp = oracle
        .projected_category_win_probabilities
        .iter()
        .zip(bank.projected_category_win_probabilities.iter())
        .map(|(a, b)| (a - b).abs() * 100.0)
        .fold(0.0_f64, f64::max);

    let oracle_top3 = plan_strategy_keys(oracle);
    let bank_top3 = plan_strategy_keys(bank);
    let top3_j_overlap = oracle_top3.intersection(&bank_top3).count();

    let oracle_future = oracle
        .projected_future_players
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let bank_future = bank
        .projected_future_players
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let union = oracle_future.union(&bank_future).count();
    let intersection = oracle_future.intersection(&bank_future).count();
    let future_player_jaccard_pct = if union == 0 {
        100.0
    } else {
        intersection as f64 / union as f64 * 100.0
    };

    let names = |players: &[PlayerId]| {
        players
            .iter()
            .map(|player_id| {
                app.durant
                    .score_for(player_id)
                    .map(|score| score.player_name.clone())
                    .unwrap_or_else(|| player_id.0.clone())
            })
            .collect::<Vec<_>>()
    };

    PlanComparison {
        scenario: state.name.clone(),
        roster_size: state.own_roster.len(),
        oracle_h_pct: oracle.projected_matchup_win_probability * 100.0,
        bank_common_h_pct: bank.projected_matchup_win_probability * 100.0,
        runtime_h_pct: runtime.projected_matchup_win_probability * 100.0,
        oracle_minus_bank_pp,
        bank_regret_pp,
        bank_absolute_h_delta_pp,
        oracle_minus_runtime_pp,
        runtime_absolute_h_delta_pp,
        deep_winner_present: bank_keys.contains(&oracle_key),
        exact_j_match: oracle_key == bank_key,
        build_match: oracle.build_name == bank.build_name,
        top3_j_overlap,
        max_category_probability_delta_pp,
        future_player_jaccard_pct,
        future_spend_delta_dollars: oracle.projected_future_spend as i32
            - bank.projected_future_spend as i32,
        budget_left_delta_dollars: oracle.projected_budget_left as i32
            - bank.projected_budget_left as i32,
        oracle_j_name: oracle.j_name.clone(),
        bank_j_name: bank.j_name.clone(),
        runtime_j_name: runtime.j_name.clone(),
        oracle_build: oracle.build_name.clone(),
        bank_build: bank.build_name.clone(),
        runtime_build: runtime.build_name.clone(),
        oracle_j_weights: oracle.j_weights,
        bank_j_weights: bank.j_weights,
        runtime_j_weights: runtime.j_weights,
        oracle_future_players: names(&oracle.projected_future_players),
        bank_future_players: names(&bank.projected_future_players),
        runtime_future_players: names(&runtime.projected_future_players),
    }
}

fn plan_strategy_keys(plan: &DurantRosterPlan) -> HashSet<StrategyKey> {
    let mut keys = HashSet::new();
    keys.insert(StrategyKey::from_weights(plan.j_weights));
    for alternative in &plan.j_alternatives {
        keys.insert(StrategyKey::from_weights(alternative.j_weights));
    }
    keys
}

fn summarize(rows: &[PlanComparison]) -> AggregateSummary {
    if rows.is_empty() {
        return AggregateSummary {
            states: 0,
            mean_bank_regret_pp: 0.0,
            median_bank_regret_pp: 0.0,
            p95_bank_regret_pp: 0.0,
            max_bank_regret_pp: 0.0,
            mean_bank_absolute_h_delta_pp: 0.0,
            p95_bank_absolute_h_delta_pp: 0.0,
            max_bank_absolute_h_delta_pp: 0.0,
            within_0_05_pp_pct: 0.0,
            within_0_10_pp_pct: 0.0,
            within_0_25_pp_pct: 0.0,
            within_0_50_pp_pct: 0.0,
            deep_winner_present_pct: 0.0,
            exact_j_match_pct: 0.0,
            build_match_pct: 0.0,
            mean_top3_j_overlap: 0.0,
            mean_future_player_jaccard_pct: 0.0,
            mean_max_category_probability_delta_pp: 0.0,
            max_category_probability_delta_pp: 0.0,
            bank_above_oracle_states: 0,
            mean_runtime_absolute_h_delta_pp: 0.0,
            p95_runtime_absolute_h_delta_pp: 0.0,
            max_runtime_absolute_h_delta_pp: 0.0,
            runtime_above_oracle_states: 0,
        };
    }

    let mut regrets = rows
        .iter()
        .map(|row| row.bank_regret_pp)
        .collect::<Vec<_>>();
    regrets.sort_by(f64::total_cmp);
    let mut absolute = rows
        .iter()
        .map(|row| row.bank_absolute_h_delta_pp)
        .collect::<Vec<_>>();
    absolute.sort_by(f64::total_cmp);
    let mut runtime_absolute = rows
        .iter()
        .map(|row| row.runtime_absolute_h_delta_pp)
        .collect::<Vec<_>>();
    runtime_absolute.sort_by(f64::total_cmp);

    let pct = |count: usize| count as f64 / rows.len() as f64 * 100.0;
    let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;

    AggregateSummary {
        states: rows.len(),
        mean_bank_regret_pp: mean(&regrets),
        median_bank_regret_pp: percentile(&regrets, 0.50),
        p95_bank_regret_pp: percentile(&regrets, 0.95),
        max_bank_regret_pp: *regrets.last().unwrap_or(&0.0),
        mean_bank_absolute_h_delta_pp: mean(&absolute),
        p95_bank_absolute_h_delta_pp: percentile(&absolute, 0.95),
        max_bank_absolute_h_delta_pp: *absolute.last().unwrap_or(&0.0),
        within_0_05_pp_pct: pct(rows
            .iter()
            .filter(|row| row.bank_absolute_h_delta_pp <= 0.05)
            .count()),
        within_0_10_pp_pct: pct(rows
            .iter()
            .filter(|row| row.bank_absolute_h_delta_pp <= 0.10)
            .count()),
        within_0_25_pp_pct: pct(rows
            .iter()
            .filter(|row| row.bank_absolute_h_delta_pp <= 0.25)
            .count()),
        within_0_50_pp_pct: pct(rows
            .iter()
            .filter(|row| row.bank_absolute_h_delta_pp <= 0.50)
            .count()),
        deep_winner_present_pct: pct(rows.iter().filter(|row| row.deep_winner_present).count()),
        exact_j_match_pct: pct(rows.iter().filter(|row| row.exact_j_match).count()),
        build_match_pct: pct(rows.iter().filter(|row| row.build_match).count()),
        mean_top3_j_overlap: rows
            .iter()
            .map(|row| row.top3_j_overlap as f64)
            .sum::<f64>()
            / rows.len() as f64,
        mean_future_player_jaccard_pct: rows
            .iter()
            .map(|row| row.future_player_jaccard_pct)
            .sum::<f64>()
            / rows.len() as f64,
        mean_max_category_probability_delta_pp: rows
            .iter()
            .map(|row| row.max_category_probability_delta_pp)
            .sum::<f64>()
            / rows.len() as f64,
        max_category_probability_delta_pp: rows
            .iter()
            .map(|row| row.max_category_probability_delta_pp)
            .fold(0.0_f64, f64::max),
        bank_above_oracle_states: rows
            .iter()
            .filter(|row| row.oracle_minus_bank_pp < -1.0e-9)
            .count(),
        mean_runtime_absolute_h_delta_pp: mean(&runtime_absolute),
        p95_runtime_absolute_h_delta_pp: percentile(&runtime_absolute, 0.95),
        max_runtime_absolute_h_delta_pp: *runtime_absolute.last().unwrap_or(&0.0),
        runtime_above_oracle_states: rows
            .iter()
            .filter(|row| row.oracle_minus_runtime_pp < -1.0e-9)
            .count(),
    }
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn write_csv(path: &PathBuf, rows: &[PlanComparison]) -> Result<()> {
    let mut output = String::from(
        "scenario,roster_size,oracle_h_pct,bank_common_h_pct,runtime_h_pct,oracle_minus_bank_pp,bank_regret_pp,bank_absolute_h_delta_pp,oracle_minus_runtime_pp,runtime_absolute_h_delta_pp,deep_winner_present,exact_j_match,build_match,top3_j_overlap,max_category_delta_pp,future_player_jaccard_pct,future_spend_delta,budget_left_delta,oracle_build,bank_build,runtime_build,oracle_j_name,bank_j_name,runtime_j_name,oracle_future_players,bank_future_players,runtime_future_players\n",
    );

    for row in rows {
        let fields = [
            csv_escape(&row.scenario),
            row.roster_size.to_string(),
            format!("{:.8}", row.oracle_h_pct),
            format!("{:.8}", row.bank_common_h_pct),
            format!("{:.8}", row.runtime_h_pct),
            format!("{:.8}", row.oracle_minus_bank_pp),
            format!("{:.8}", row.bank_regret_pp),
            format!("{:.8}", row.bank_absolute_h_delta_pp),
            format!("{:.8}", row.oracle_minus_runtime_pp),
            format!("{:.8}", row.runtime_absolute_h_delta_pp),
            row.deep_winner_present.to_string(),
            row.exact_j_match.to_string(),
            row.build_match.to_string(),
            row.top3_j_overlap.to_string(),
            format!("{:.8}", row.max_category_probability_delta_pp),
            format!("{:.8}", row.future_player_jaccard_pct),
            row.future_spend_delta_dollars.to_string(),
            row.budget_left_delta_dollars.to_string(),
            csv_escape(&row.oracle_build),
            csv_escape(&row.bank_build),
            csv_escape(&row.runtime_build),
            csv_escape(&row.oracle_j_name),
            csv_escape(&row.bank_j_name),
            csv_escape(&row.runtime_j_name),
            csv_escape(&row.oracle_future_players.join(" | ")),
            csv_escape(&row.bank_future_players.join(" | ")),
            csv_escape(&row.runtime_future_players.join(" | ")),
        ];
        output.push_str(&fields.join(","));
        output.push('\n');
    }

    fs::write(path, output).with_context(|| format!("failed to write {}", path.display()))
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('\"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}
