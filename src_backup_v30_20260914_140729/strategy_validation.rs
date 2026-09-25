use crate::app::App;
use crate::durant::DynamicDurantScore;
use crate::player::PlayerId;
use crate::strategy_bank::StrategyBankFile;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

const VALIDATION_SCHEMA_VERSION: u32 = 1;
const CATEGORY_COUNT: usize = 9;

/// These states were deliberately NOT used in the 15-state strategy census.
/// They also stress a different regime: the training census used 0-2 owned
/// players, while these holdouts contain 3-7 owned players and therefore test
/// whether the learned j vocabulary generalizes into realistic mid-draft
/// roster shapes.
const HOLDOUT_SCENARIOS: &[(&str, &[&str])] = &[
    (
        "CADE + MOBLEY + WHITE",
        &["Cade Cunningham", "Evan Mobley", "Derrick White"],
    ),
    (
        "CURRY + BAM + JALEN J",
        &["Stephen Curry", "Bam Adebayo", "Jalen Johnson"],
    ),
    (
        "ANT + CHET + LAMELO + OG",
        &[
            "Anthony Edwards",
            "Chet Holmgren",
            "LaMelo Ball",
            "OG Anunoby",
        ],
    ),
    (
        "MITCHELL + DUREN + BANE + FRANZ",
        &[
            "Donovan Mitchell",
            "Jalen Duren",
            "Desmond Bane",
            "Franz Wagner",
        ],
    ),
    (
        "FOX + KAT + AMEN + WHITE + CLAXTON",
        &[
            "De'Aaron Fox",
            "Karl-Anthony Towns",
            "Amen Thompson",
            "Derrick White",
            "Nic Claxton",
        ],
    ),
    (
        "BRUNSON + MOBLEY + TREY + DYSON + ZUBAC",
        &[
            "Jalen Brunson",
            "Evan Mobley",
            "Trey Murphy III",
            "Dyson Daniels",
            "Ivica Zubac",
        ],
    ),
    (
        "CURRY + JALEN J + CHET + OG + GIDDEY + WARE",
        &[
            "Stephen Curry",
            "Jalen Johnson",
            "Chet Holmgren",
            "OG Anunoby",
            "Josh Giddey",
            "Kel'el Ware",
        ],
    ),
    (
        "CADE + BAM + BANE + JJJ + QUICKLEY + OKONGWU + CAMARA",
        &[
            "Cade Cunningham",
            "Bam Adebayo",
            "Desmond Bane",
            "Jaren Jackson Jr.",
            "Immanuel Quickley",
            "Onyeka Okongwu",
            "Toumani Camara",
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

#[derive(Debug, Clone, Serialize)]
struct ValidationReport {
    schema_version: u32,
    draft_season: String,
    bank_path: String,
    deep_strategy_count: usize,
    full_bank_strategy_count: usize,
    core_bank_strategy_count: usize,
    requested_holdout_scenarios: usize,
    completed_holdout_scenarios: usize,
    total_decisions: usize,
    wall_time_seconds: f64,
    full_bank: AggregateSummary,
    coverage_core: AggregateSummary,
    scenarios: Vec<ScenarioSummary>,
    worst_full_bank_decisions: Vec<DecisionComparison>,
    worst_core_decisions: Vec<DecisionComparison>,
}

#[derive(Debug, Clone, Serialize)]
struct ScenarioSummary {
    scenario: String,
    roster: Vec<String>,
    roster_size: usize,
    decisions: usize,
    deep_seconds: f64,
    full_bank_seconds: f64,
    core_bank_seconds: f64,
    full_bank: AggregateSummary,
    coverage_core: AggregateSummary,
}

#[derive(Debug, Clone, Serialize)]
struct AggregateSummary {
    decisions: usize,
    mean_regret_pp: f64,
    median_regret_pp: f64,
    p95_regret_pp: f64,
    max_regret_pp: f64,
    within_0_05_pp_pct: f64,
    within_0_10_pp_pct: f64,
    within_0_25_pp_pct: f64,
    within_0_50_pp_pct: f64,
    deep_winner_present_pct: f64,
    exact_j_match_pct: f64,
    build_match_pct: f64,
    mean_max_category_probability_delta_pp: f64,
    max_category_probability_delta_pp: f64,
}

#[derive(Debug, Clone, Serialize)]
struct DecisionComparison {
    scenario: String,
    roster_size: usize,
    roster: String,
    candidate: String,
    bank_profile: String,
    deep_h_pct: f64,
    bank_h_pct: f64,
    regret_pp: f64,
    deep_winner_present: bool,
    exact_j_match: bool,
    build_match: bool,
    max_category_probability_delta_pp: f64,
    deep_j_name: String,
    bank_j_name: String,
    deep_build: String,
    bank_build: String,
    deep_j_weights: [f64; CATEGORY_COUNT],
    bank_j_weights: [f64; CATEGORY_COUNT],
}

pub fn print_plan(app: &App) {
    let bank_path = bank_path(app);

    println!();
    println!("========================================================================");
    println!("DURANT STRATEGY-BANK HOLDOUT VALIDATION");
    println!("========================================================================");
    println!(
        "deep search               : {} j vectors",
        app.durant.deep_strategy_count()
    );
    println!("holdout roster states     : {}", HOLDOUT_SCENARIOS.len());
    println!("training roster sizes     : 0-2 players");
    println!("holdout roster sizes      : 3-7 players");
    println!("bank input                : {}", bank_path.display());
    println!();
    println!("For every available candidate in every holdout state this compares:");
    println!("  1. the full 212,941-j deep search (reference)");
    println!("  2. the complete learned bank (currently 260 strategies)");
    println!("  3. the 99%-coverage core (currently 102 strategies)");
    println!();
    println!("Primary metric: H regret = H_deep - H_bank, in percentage points.");
    println!("Deep-state results use the existing resumable deep cache namespace.");
}

pub fn validate_and_save(app: &App) -> Result<()> {
    let run_started = Instant::now();
    let bank_path = bank_path(app);
    let bank = load_bank(&bank_path)?;

    if bank.draft_season != app.stats.draft_season {
        bail!(
            "strategy bank season {} does not match loaded season {}",
            bank.draft_season,
            app.stats.draft_season,
        );
    }

    if bank.strategies.is_empty() {
        bail!("strategy bank contains no strategies");
    }

    let full_bank_weights = bank
        .strategies
        .iter()
        .map(|entry| entry.weights)
        .collect::<Vec<_>>();

    let core_bank_weights = bank
        .strategies
        .iter()
        .filter(|entry| {
            entry
                .selection_reasons
                .iter()
                .any(|reason| reason == "coverage_core")
        })
        .map(|entry| entry.weights)
        .collect::<Vec<_>>();

    if core_bank_weights.is_empty() {
        bail!("strategy bank contains no coverage_core strategies");
    }

    let full_bank_keys = full_bank_weights
        .iter()
        .copied()
        .map(StrategyKey::from_weights)
        .collect::<HashSet<_>>();

    let core_bank_keys = core_bank_weights
        .iter()
        .copied()
        .map(StrategyKey::from_weights)
        .collect::<HashSet<_>>();

    println!();
    println!("========================================================================");
    println!("DURANT HOLDOUT GENERALIZATION TEST");
    println!("========================================================================");
    println!(
        "{} unseen states | deep={} | full bank={} | core={}",
        HOLDOUT_SCENARIOS.len(),
        app.durant.deep_strategy_count(),
        full_bank_weights.len(),
        core_bank_weights.len(),
    );

    let all_candidates = app
        .durant
        .scores
        .iter()
        .map(|score| score.player_id.clone())
        .collect::<Vec<_>>();

    let mut all_full_comparisons = Vec::<DecisionComparison>::new();
    let mut all_core_comparisons = Vec::<DecisionComparison>::new();
    let mut scenario_summaries = Vec::<ScenarioSummary>::new();

    for (scenario_index, (scenario_name, roster_names)) in HOLDOUT_SCENARIOS.iter().enumerate() {
        let Some(own_roster) = resolve_roster(app, scenario_name, roster_names) else {
            continue;
        };

        let owned = own_roster.iter().cloned().collect::<HashSet<_>>();
        let candidates = all_candidates
            .iter()
            .filter(|player_id| !owned.contains(*player_id))
            .cloned()
            .collect::<Vec<_>>();

        println!(
            "  [{:>2}/{}] {:<54} {} players / {} decisions",
            scenario_index + 1,
            HOLDOUT_SCENARIOS.len(),
            scenario_name,
            own_roster.len(),
            candidates.len(),
        );

        let deep_started = Instant::now();
        let deep_scores = app
            .durant
            .dynamic_scores_deep(&own_roster, &[], &candidates);
        let deep_seconds = deep_started.elapsed().as_secs_f64();

        let full_bank_started = Instant::now();
        let full_bank_scores = app.durant.dynamic_scores_with_strategy_weights(
            &own_roster,
            &[],
            &candidates,
            &full_bank_weights,
        );
        let full_bank_seconds = full_bank_started.elapsed().as_secs_f64();

        let core_bank_started = Instant::now();
        let core_bank_scores = app.durant.dynamic_scores_with_strategy_weights(
            &own_roster,
            &[],
            &candidates,
            &core_bank_weights,
        );
        let core_bank_seconds = core_bank_started.elapsed().as_secs_f64();

        let full_map = scores_by_player(full_bank_scores);
        let core_map = scores_by_player(core_bank_scores);
        let roster_label = roster_names.join(" + ");

        let mut scenario_full = Vec::<DecisionComparison>::new();
        let mut scenario_core = Vec::<DecisionComparison>::new();

        for deep_score in &deep_scores {
            let full_score = full_map.get(&deep_score.player_id).with_context(|| {
                format!(
                    "full bank returned no score for {} in {scenario_name}",
                    deep_score.player_name
                )
            })?;

            let core_score = core_map.get(&deep_score.player_id).with_context(|| {
                format!(
                    "coverage core returned no score for {} in {scenario_name}",
                    deep_score.player_name
                )
            })?;

            scenario_full.push(compare_decision(
                scenario_name,
                own_roster.len(),
                &roster_label,
                "full_bank",
                deep_score,
                full_score,
                &full_bank_keys,
            ));

            scenario_core.push(compare_decision(
                scenario_name,
                own_roster.len(),
                &roster_label,
                "coverage_core",
                deep_score,
                core_score,
                &core_bank_keys,
            ));
        }

        let full_summary = summarize(&scenario_full);
        let core_summary = summarize(&scenario_core);

        println!(
            "           deep {:>5.1} min | full mean/p95/max {:>6.3}/{:>6.3}/{:>6.3} pp | core {:>6.3}/{:>6.3}/{:>6.3} pp",
            deep_seconds / 60.0,
            full_summary.mean_regret_pp,
            full_summary.p95_regret_pp,
            full_summary.max_regret_pp,
            core_summary.mean_regret_pp,
            core_summary.p95_regret_pp,
            core_summary.max_regret_pp,
        );

        scenario_summaries.push(ScenarioSummary {
            scenario: (*scenario_name).to_string(),
            roster: roster_names
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            roster_size: own_roster.len(),
            decisions: deep_scores.len(),
            deep_seconds,
            full_bank_seconds,
            core_bank_seconds,
            full_bank: full_summary,
            coverage_core: core_summary,
        });

        all_full_comparisons.extend(scenario_full);
        all_core_comparisons.extend(scenario_core);
    }

    if scenario_summaries.is_empty() {
        bail!("none of the holdout scenarios could be resolved");
    }

    let full_summary = summarize(&all_full_comparisons);
    let core_summary = summarize(&all_core_comparisons);

    let mut worst_full = all_full_comparisons.clone();
    worst_full.sort_by(|a, b| b.regret_pp.total_cmp(&a.regret_pp));
    worst_full.truncate(25);

    let mut worst_core = all_core_comparisons.clone();
    worst_core.sort_by(|a, b| b.regret_pp.total_cmp(&a.regret_pp));
    worst_core.truncate(25);

    let report = ValidationReport {
        schema_version: VALIDATION_SCHEMA_VERSION,
        draft_season: app.stats.draft_season.clone(),
        bank_path: bank_path.display().to_string(),
        deep_strategy_count: app.durant.deep_strategy_count(),
        full_bank_strategy_count: full_bank_weights.len(),
        core_bank_strategy_count: core_bank_weights.len(),
        requested_holdout_scenarios: HOLDOUT_SCENARIOS.len(),
        completed_holdout_scenarios: scenario_summaries.len(),
        total_decisions: all_full_comparisons.len(),
        wall_time_seconds: run_started.elapsed().as_secs_f64(),
        full_bank: full_summary,
        coverage_core: core_summary,
        scenarios: scenario_summaries,
        worst_full_bank_decisions: worst_full,
        worst_core_decisions: worst_core,
    };

    let output_dir = PathBuf::from("data")
        .join("stats")
        .join(&app.stats.draft_season)
        .join("durant");
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let json_path = output_dir.join("strategy_bank_validation_deep.json");
    let json = serde_json::to_string_pretty(&report)?;
    fs::write(&json_path, json)
        .with_context(|| format!("failed to write {}", json_path.display()))?;

    let csv_path = output_dir.join("strategy_bank_validation_deep.csv");
    write_comparison_csv(&csv_path, &all_full_comparisons, &all_core_comparisons)?;

    println!();
    println!("SUMMARY");
    println!(
        "completed states     : {}/{}",
        report.completed_holdout_scenarios, report.requested_holdout_scenarios
    );
    println!("candidate decisions  : {}", report.total_decisions);
    println!();
    print_summary(
        "FULL BANK",
        report.full_bank_strategy_count,
        &report.full_bank,
    );
    println!();
    print_summary(
        "99% CORE",
        report.core_bank_strategy_count,
        &report.coverage_core,
    );
    println!();
    println!("report               : {}", json_path.display());
    println!("decision CSV         : {}", csv_path.display());
    println!(
        "wall time            : {:.2} hours",
        report.wall_time_seconds / 3600.0
    );

    Ok(())
}

fn bank_path(app: &App) -> PathBuf {
    PathBuf::from("data")
        .join("stats")
        .join(&app.stats.draft_season)
        .join("durant")
        .join("strategy_bank_deep.json")
}

fn load_bank(path: &PathBuf) -> Result<StrategyBankFile> {
    let json =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    serde_json::from_str(&json).with_context(|| format!("failed to parse {}", path.display()))
}

fn resolve_roster(app: &App, scenario_name: &str, roster_names: &[&str]) -> Option<Vec<PlayerId>> {
    let mut roster = Vec::with_capacity(roster_names.len());
    let mut missing = Vec::new();

    for name in roster_names {
        match app
            .durant
            .scores
            .iter()
            .find(|score| score.player_name == *name)
        {
            Some(score) => roster.push(score.player_id.clone()),
            None => missing.push((*name).to_string()),
        }
    }

    if missing.is_empty() {
        Some(roster)
    } else {
        println!(
            "           SKIPPED {scenario_name} — missing DURANT player(s): {}",
            missing.join(", ")
        );
        None
    }
}

fn scores_by_player(scores: Vec<DynamicDurantScore>) -> HashMap<PlayerId, DynamicDurantScore> {
    scores
        .into_iter()
        .map(|score| (score.player_id.clone(), score))
        .collect()
}

fn compare_decision(
    scenario: &str,
    roster_size: usize,
    roster: &str,
    bank_profile: &str,
    deep: &DynamicDurantScore,
    bank: &DynamicDurantScore,
    bank_keys: &HashSet<StrategyKey>,
) -> DecisionComparison {
    let deep_key = StrategyKey::from_weights(deep.j_weights);
    let bank_key = StrategyKey::from_weights(bank.j_weights);

    let regret_pp = ((deep.projected_matchup_win_probability
        - bank.projected_matchup_win_probability)
        .max(0.0))
        * 100.0;

    let max_category_probability_delta_pp = deep
        .projected_category_win_probabilities
        .iter()
        .zip(bank.projected_category_win_probabilities.iter())
        .map(|(deep_probability, bank_probability)| {
            (deep_probability - bank_probability).abs() * 100.0
        })
        .fold(0.0_f64, f64::max);

    DecisionComparison {
        scenario: scenario.to_string(),
        roster_size,
        roster: roster.to_string(),
        candidate: deep.player_name.clone(),
        bank_profile: bank_profile.to_string(),
        deep_h_pct: deep.projected_matchup_win_probability * 100.0,
        bank_h_pct: bank.projected_matchup_win_probability * 100.0,
        regret_pp,
        deep_winner_present: bank_keys.contains(&deep_key),
        exact_j_match: deep_key == bank_key,
        build_match: deep.build_name == bank.build_name,
        max_category_probability_delta_pp,
        deep_j_name: deep.j_name.clone(),
        bank_j_name: bank.j_name.clone(),
        deep_build: deep.build_name.clone(),
        bank_build: bank.build_name.clone(),
        deep_j_weights: deep.j_weights,
        bank_j_weights: bank.j_weights,
    }
}

fn summarize(rows: &[DecisionComparison]) -> AggregateSummary {
    if rows.is_empty() {
        return AggregateSummary {
            decisions: 0,
            mean_regret_pp: 0.0,
            median_regret_pp: 0.0,
            p95_regret_pp: 0.0,
            max_regret_pp: 0.0,
            within_0_05_pp_pct: 0.0,
            within_0_10_pp_pct: 0.0,
            within_0_25_pp_pct: 0.0,
            within_0_50_pp_pct: 0.0,
            deep_winner_present_pct: 0.0,
            exact_j_match_pct: 0.0,
            build_match_pct: 0.0,
            mean_max_category_probability_delta_pp: 0.0,
            max_category_probability_delta_pp: 0.0,
        };
    }

    let mut regrets = rows.iter().map(|row| row.regret_pp).collect::<Vec<_>>();
    regrets.sort_by(|a, b| a.total_cmp(b));

    let n = rows.len();
    let mean_regret_pp = regrets.iter().sum::<f64>() / n as f64;
    let mean_max_category_probability_delta_pp = rows
        .iter()
        .map(|row| row.max_category_probability_delta_pp)
        .sum::<f64>()
        / n as f64;

    AggregateSummary {
        decisions: n,
        mean_regret_pp,
        median_regret_pp: percentile(&regrets, 0.50),
        p95_regret_pp: percentile(&regrets, 0.95),
        max_regret_pp: regrets.last().copied().unwrap_or(0.0),
        within_0_05_pp_pct: pct(rows, |row| row.regret_pp <= 0.05),
        within_0_10_pp_pct: pct(rows, |row| row.regret_pp <= 0.10),
        within_0_25_pp_pct: pct(rows, |row| row.regret_pp <= 0.25),
        within_0_50_pp_pct: pct(rows, |row| row.regret_pp <= 0.50),
        deep_winner_present_pct: pct(rows, |row| row.deep_winner_present),
        exact_j_match_pct: pct(rows, |row| row.exact_j_match),
        build_match_pct: pct(rows, |row| row.build_match),
        mean_max_category_probability_delta_pp,
        max_category_probability_delta_pp: rows
            .iter()
            .map(|row| row.max_category_probability_delta_pp)
            .fold(0.0_f64, f64::max),
    }
}

fn percentile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }

    let index = ((sorted.len() - 1) as f64 * quantile).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn pct<F>(rows: &[DecisionComparison], predicate: F) -> f64
where
    F: Fn(&DecisionComparison) -> bool,
{
    if rows.is_empty() {
        return 0.0;
    }

    let count = rows.iter().filter(|row| predicate(row)).count();
    count as f64 / rows.len() as f64 * 100.0
}

fn print_summary(label: &str, strategy_count: usize, summary: &AggregateSummary) {
    println!("{label} ({strategy_count} strategies)");
    println!(
        "  H regret mean / median / p95 / max : {:.4} / {:.4} / {:.4} / {:.4} pp",
        summary.mean_regret_pp,
        summary.median_regret_pp,
        summary.p95_regret_pp,
        summary.max_regret_pp,
    );
    println!(
        "  within 0.05 / 0.10 / 0.25 / 0.50pp : {:.1}% / {:.1}% / {:.1}% / {:.1}%",
        summary.within_0_05_pp_pct,
        summary.within_0_10_pp_pct,
        summary.within_0_25_pp_pct,
        summary.within_0_50_pp_pct,
    );
    println!(
        "  deep winner present / exact j match : {:.1}% / {:.1}%",
        summary.deep_winner_present_pct, summary.exact_j_match_pct,
    );
    println!(
        "  build match                          : {:.1}%",
        summary.build_match_pct,
    );
    println!(
        "  mean / max category-prob delta       : {:.4} / {:.4} pp",
        summary.mean_max_category_probability_delta_pp, summary.max_category_probability_delta_pp,
    );
}

fn write_comparison_csv(
    path: &PathBuf,
    full_rows: &[DecisionComparison],
    core_rows: &[DecisionComparison],
) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)
        .with_context(|| format!("failed to create {}", path.display()))?;

    let mut header = vec![
        "scenario".to_string(),
        "roster_size".to_string(),
        "roster".to_string(),
        "candidate".to_string(),
        "bank_profile".to_string(),
        "deep_h_pct".to_string(),
        "bank_h_pct".to_string(),
        "regret_pp".to_string(),
        "deep_winner_present".to_string(),
        "exact_j_match".to_string(),
        "build_match".to_string(),
        "max_category_probability_delta_pp".to_string(),
        "deep_j_name".to_string(),
        "bank_j_name".to_string(),
        "deep_build".to_string(),
        "bank_build".to_string(),
    ];

    for prefix in ["deep_j", "bank_j"] {
        for category in ["fg", "ft", "3pm", "pts", "reb", "ast", "stl", "blk", "to"] {
            header.push(format!("{prefix}_{category}"));
        }
    }

    writer.write_record(header)?;

    for row in full_rows.iter().chain(core_rows.iter()) {
        let mut record = vec![
            row.scenario.clone(),
            row.roster_size.to_string(),
            row.roster.clone(),
            row.candidate.clone(),
            row.bank_profile.clone(),
            format!("{:.8}", row.deep_h_pct),
            format!("{:.8}", row.bank_h_pct),
            format!("{:.8}", row.regret_pp),
            row.deep_winner_present.to_string(),
            row.exact_j_match.to_string(),
            row.build_match.to_string(),
            format!("{:.8}", row.max_category_probability_delta_pp),
            row.deep_j_name.clone(),
            row.bank_j_name.clone(),
            row.deep_build.clone(),
            row.bank_build.clone(),
        ];

        for weight in row.deep_j_weights {
            record.push(format_weight(weight));
        }
        for weight in row.bank_j_weights {
            record.push(format_weight(weight));
        }

        writer.write_record(record)?;
    }

    writer.flush()?;
    Ok(())
}

fn format_weight(weight: f64) -> String {
    if (weight - weight.round()).abs() < 1e-9 {
        format!("{weight:.0}")
    } else {
        format!("{weight:.3}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}
