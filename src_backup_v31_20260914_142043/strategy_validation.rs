use crate::app::App;
use crate::durant::{AuctionConfig, DurantRosterPlan};
use crate::player::PlayerId;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

const VALIDATION_SCHEMA_VERSION: u32 = 2;
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
    deep_seconds: f64,
    bank_seconds: f64,
    comparison: PlanComparison,
}

#[derive(Debug, Clone, Serialize)]
struct AggregateSummary {
    states: usize,
    mean_regret_pp: f64,
    median_regret_pp: f64,
    p95_regret_pp: f64,
    max_regret_pp: f64,
    mean_absolute_h_delta_pp: f64,
    p95_absolute_h_delta_pp: f64,
    max_absolute_h_delta_pp: f64,
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
    bank_above_deep_states: usize,
}

#[derive(Debug, Clone, Serialize)]
struct PlanComparison {
    scenario: String,
    roster_size: usize,
    deep_h_pct: f64,
    bank_h_pct: f64,
    deep_minus_bank_pp: f64,
    regret_pp: f64,
    absolute_h_delta_pp: f64,
    deep_winner_present: bool,
    exact_j_match: bool,
    build_match: bool,
    top3_j_overlap: usize,
    max_category_probability_delta_pp: f64,
    future_player_jaccard_pct: f64,
    future_spend_delta_dollars: i32,
    budget_left_delta_dollars: i32,
    deep_j_name: String,
    bank_j_name: String,
    deep_build: String,
    bank_build: String,
    deep_j_weights: [f64; CATEGORY_COUNT],
    bank_j_weights: [f64; CATEGORY_COUNT],
    deep_future_players: Vec<String>,
    bank_future_players: Vec<String>,
}

pub fn print_plan(app: &App) {
    let Some(bank) = app.strategy_bank.as_ref() else {
        println!("No runtime strategy bank is loaded.");
        return;
    };

    println!();
    println!("========================================================================");
    println!("CURRENT-ENVIRONMENT j-BANK VALIDATION (v31)");
    println!("========================================================================");
    println!("environment                : v30 two-stage Strategy planner");
    println!(
        "deep reference             : {} j vectors",
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
    println!("Both sides use the SAME current opponent-pricing core.");
    println!("Only the searched j vocabulary changes:");
    println!("  A. full 212,941-j deep search");
    println!("  B. currently loaded runtime bank");
    println!();
    println!("Primary metric: H regret = max(H_deep - H_bank, 0), percentage points.");
    println!("Also reports absolute H delta, top-3 j overlap, category deltas,");
    println!("future-player overlap, spend/budget differences, and build agreement.");
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
    println!("CURRENT v30 STRATEGY j-BANK HOLDOUT TEST");
    println!("========================================================================");
    println!(
        "{} clean states | deep={} | runtime bank={} | pricing core held fixed",
        HOLDOUT_SCENARIOS.len(),
        app.durant.deep_strategy_count(),
        bank.weights.len(),
    );

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

        let deep_started = Instant::now();
        let deep = app.durant.roster_plan_validation_deep(
            &state.own_roster,
            state.own_budget,
            &state.opponent_rosters,
            &state.opponent_budgets,
            &state.candidates,
            &bank.weights,
            config,
        );
        let deep_seconds = deep_started.elapsed().as_secs_f64();

        let bank_started = Instant::now();
        let bank_plan = app.durant.roster_plan_validation_with_strategy_weights(
            &state.own_roster,
            state.own_budget,
            &state.opponent_rosters,
            &state.opponent_budgets,
            &state.candidates,
            &bank.weights,
            &bank.weights,
            config,
        );
        let bank_seconds = bank_started.elapsed().as_secs_f64();

        let (Some(deep), Some(bank_plan)) = (deep, bank_plan) else {
            println!("           SKIPPED — planner returned no valid plan");
            continue;
        };

        let comparison = compare_plans(&state, &deep, &bank_plan, &bank_keys, app);

        println!(
            "           deep {:>7.2}s | bank {:>6.2}s | H {:>6.3}% vs {:>6.3}% | regret {:>6.3} pp | |ΔH| {:>6.3} pp",
            deep_seconds,
            bank_seconds,
            comparison.deep_h_pct,
            comparison.bank_h_pct,
            comparison.regret_pp,
            comparison.absolute_h_delta_pp,
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
            deep_seconds,
            bank_seconds,
            comparison: comparison.clone(),
        });
        comparisons.push(comparison);
    }

    if comparisons.is_empty() {
        bail!("none of the current-environment holdout scenarios completed");
    }

    let aggregate = summarize(&comparisons);
    let mut worst = comparisons.clone();
    worst.sort_by(|a, b| b.absolute_h_delta_pp.total_cmp(&a.absolute_h_delta_pp));
    worst.truncate(12);

    let report = ValidationReport {
        schema_version: VALIDATION_SCHEMA_VERSION,
        environment: "v30_two_stage_strategy_current_auction".to_string(),
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

    let json_path = output_dir.join("strategy_bank_validation_current_v31.json");
    let json = serde_json::to_string_pretty(&report)?;
    fs::write(&json_path, json)
        .with_context(|| format!("failed to write {}", json_path.display()))?;

    let csv_path = output_dir.join("strategy_bank_validation_current_v31.csv");
    write_csv(&csv_path, &comparisons)?;

    println!();
    println!("SUMMARY");
    println!(
        "completed states      : {}/{}",
        report.completed_holdout_scenarios, report.requested_holdout_scenarios
    );
    println!(
        "mean regret           : {:.4} pp",
        report.aggregate.mean_regret_pp
    );
    println!(
        "p95 / max regret      : {:.4} / {:.4} pp",
        report.aggregate.p95_regret_pp, report.aggregate.max_regret_pp
    );
    println!(
        "mean |ΔH|             : {:.4} pp",
        report.aggregate.mean_absolute_h_delta_pp
    );
    println!(
        "p95 / max |ΔH|        : {:.4} / {:.4} pp",
        report.aggregate.p95_absolute_h_delta_pp, report.aggregate.max_absolute_h_delta_pp
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
        "bank > deep states    : {}",
        report.aggregate.bank_above_deep_states
    );
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
    deep: &DurantRosterPlan,
    bank: &DurantRosterPlan,
    bank_keys: &HashSet<StrategyKey>,
    app: &App,
) -> PlanComparison {
    let deep_key = StrategyKey::from_weights(deep.j_weights);
    let bank_key = StrategyKey::from_weights(bank.j_weights);
    let deep_minus_bank_pp =
        (deep.projected_matchup_win_probability - bank.projected_matchup_win_probability) * 100.0;
    let regret_pp = deep_minus_bank_pp.max(0.0);
    let absolute_h_delta_pp = deep_minus_bank_pp.abs();

    let max_category_probability_delta_pp = deep
        .projected_category_win_probabilities
        .iter()
        .zip(bank.projected_category_win_probabilities.iter())
        .map(|(a, b)| (a - b).abs() * 100.0)
        .fold(0.0_f64, f64::max);

    let deep_top3 = plan_strategy_keys(deep);
    let bank_top3 = plan_strategy_keys(bank);
    let top3_j_overlap = deep_top3.intersection(&bank_top3).count();

    let deep_future = deep
        .projected_future_players
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let bank_future = bank
        .projected_future_players
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let union = deep_future.union(&bank_future).count();
    let intersection = deep_future.intersection(&bank_future).count();
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
        deep_h_pct: deep.projected_matchup_win_probability * 100.0,
        bank_h_pct: bank.projected_matchup_win_probability * 100.0,
        deep_minus_bank_pp,
        regret_pp,
        absolute_h_delta_pp,
        deep_winner_present: bank_keys.contains(&deep_key),
        exact_j_match: deep_key == bank_key,
        build_match: deep.build_name == bank.build_name,
        top3_j_overlap,
        max_category_probability_delta_pp,
        future_player_jaccard_pct,
        future_spend_delta_dollars: deep.projected_future_spend as i32
            - bank.projected_future_spend as i32,
        budget_left_delta_dollars: deep.projected_budget_left as i32
            - bank.projected_budget_left as i32,
        deep_j_name: deep.j_name.clone(),
        bank_j_name: bank.j_name.clone(),
        deep_build: deep.build_name.clone(),
        bank_build: bank.build_name.clone(),
        deep_j_weights: deep.j_weights,
        bank_j_weights: bank.j_weights,
        deep_future_players: names(&deep.projected_future_players),
        bank_future_players: names(&bank.projected_future_players),
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
            mean_regret_pp: 0.0,
            median_regret_pp: 0.0,
            p95_regret_pp: 0.0,
            max_regret_pp: 0.0,
            mean_absolute_h_delta_pp: 0.0,
            p95_absolute_h_delta_pp: 0.0,
            max_absolute_h_delta_pp: 0.0,
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
            bank_above_deep_states: 0,
        };
    }

    let mut regrets = rows.iter().map(|row| row.regret_pp).collect::<Vec<_>>();
    regrets.sort_by(f64::total_cmp);
    let mut absolute = rows
        .iter()
        .map(|row| row.absolute_h_delta_pp)
        .collect::<Vec<_>>();
    absolute.sort_by(f64::total_cmp);

    let pct = |count: usize| count as f64 / rows.len() as f64 * 100.0;
    let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;

    AggregateSummary {
        states: rows.len(),
        mean_regret_pp: mean(&regrets),
        median_regret_pp: percentile(&regrets, 0.50),
        p95_regret_pp: percentile(&regrets, 0.95),
        max_regret_pp: *regrets.last().unwrap_or(&0.0),
        mean_absolute_h_delta_pp: mean(&absolute),
        p95_absolute_h_delta_pp: percentile(&absolute, 0.95),
        max_absolute_h_delta_pp: *absolute.last().unwrap_or(&0.0),
        within_0_05_pp_pct: pct(rows
            .iter()
            .filter(|row| row.absolute_h_delta_pp <= 0.05)
            .count()),
        within_0_10_pp_pct: pct(rows
            .iter()
            .filter(|row| row.absolute_h_delta_pp <= 0.10)
            .count()),
        within_0_25_pp_pct: pct(rows
            .iter()
            .filter(|row| row.absolute_h_delta_pp <= 0.25)
            .count()),
        within_0_50_pp_pct: pct(rows
            .iter()
            .filter(|row| row.absolute_h_delta_pp <= 0.50)
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
        bank_above_deep_states: rows
            .iter()
            .filter(|row| row.deep_minus_bank_pp < 0.0)
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
        "scenario,roster_size,deep_h_pct,bank_h_pct,deep_minus_bank_pp,regret_pp,absolute_h_delta_pp,deep_winner_present,exact_j_match,build_match,top3_j_overlap,max_category_delta_pp,future_player_jaccard_pct,future_spend_delta,budget_left_delta,deep_build,bank_build,deep_j_name,bank_j_name,deep_future_players,bank_future_players\n",
    );

    for row in rows {
        let fields = [
            csv_escape(&row.scenario),
            row.roster_size.to_string(),
            format!("{:.8}", row.deep_h_pct),
            format!("{:.8}", row.bank_h_pct),
            format!("{:.8}", row.deep_minus_bank_pp),
            format!("{:.8}", row.regret_pp),
            format!("{:.8}", row.absolute_h_delta_pp),
            row.deep_winner_present.to_string(),
            row.exact_j_match.to_string(),
            row.build_match.to_string(),
            row.top3_j_overlap.to_string(),
            format!("{:.8}", row.max_category_probability_delta_pp),
            format!("{:.8}", row.future_player_jaccard_pct),
            row.future_spend_delta_dollars.to_string(),
            row.budget_left_delta_dollars.to_string(),
            csv_escape(&row.deep_build),
            csv_escape(&row.bank_build),
            csv_escape(&row.deep_j_name),
            csv_escape(&row.bank_j_name),
            csv_escape(&row.deep_future_players.join(" | ")),
            csv_escape(&row.bank_future_players.join(" | ")),
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
