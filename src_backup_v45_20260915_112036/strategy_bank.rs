use crate::app::App;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

const BANK_SCHEMA_VERSION: u32 = 3;
const MAX_LIVE_STRATEGIES: usize = 300;
const COVERAGE_TARGET: f64 = 0.99;
const NICHE_MIN_WINS: usize = 2;
const NICHE_MIN_AVG_MARGIN: f64 = 0.005; // 0.5 percentage points.

const CATEGORY_ORDER: [&str; 9] = ["FG%", "FT%", "3PM", "PTS", "REB", "AST", "STL", "BLK", "TO"];

const CENSUS_SCENARIOS: &[(&str, &[&str])] = &[
    ("EMPTY", &[]),
    ("JOKIC", &["Nikola Jokić"]),
    ("WEMBY", &["Victor Wembanyama"]),
    ("SGA", &["Shai Gilgeous-Alexander"]),
    ("LUKA", &["Luka Dončić"]),
    ("GIANNIS", &["Giannis Antetokounmpo"]),
    ("TATUM", &["Jayson Tatum"]),
    ("HALIBURTON", &["Tyrese Haliburton"]),
    ("SCOTTIE", &["Scottie Barnes"]),
    ("MAXEY", &["Tyrese Maxey"]),
    (
        "GIANNIS + GOBERT",
        &["Giannis Antetokounmpo", "Rudy Gobert"],
    ),
    ("LUKA + CLINGAN", &["Luka Dončić", "Donovan Clingan"]),
    ("JOKIC + MATAS", &["Nikola Jokić", "Matas Buzelis"]),
    ("SGA + MAXEY", &["Shai Gilgeous-Alexander", "Tyrese Maxey"]),
    ("WEMBY + TATUM", &["Victor Wembanyama", "Jayson Tatum"]),
];

const AUCTION_CENSUS_SCENARIOS: &[(&str, &[&str])] = &[
    ("EMPTY", &[]),
    ("JOKIC", &["Nikola Jokić"]),
    ("WEMBY", &["Victor Wembanyama"]),
    ("LUKA", &["Luka Dončić"]),
    ("GIANNIS", &["Giannis Antetokounmpo"]),
    ("MAXEY", &["Tyrese Maxey"]),
    (
        "GIANNIS + GOBERT",
        &["Giannis Antetokounmpo", "Rudy Gobert"],
    ),
    ("LUKA + CLINGAN", &["Luka Dončić", "Donovan Clingan"]),
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

// Fast calibration set for the corrected no-stride auction model.  It is not
// intended to be the final learned bank; it exists so we can inspect sensible
// plans in minutes before committing another full-night search.
const AUCTION_QUICK_SCENARIOS: &[(&str, &[&str])] = &[
    ("EMPTY", &[]),
    ("JOKIC", &["Nikola Jokić"]),
    ("LUKA", &["Luka Dončić"]),
    (
        "GIANNIS + GOBERT",
        &["Giannis Antetokounmpo", "Rudy Gobert"],
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
];

const AUCTION_QUICK_DECISION_LIMIT: usize = 64;

// Larger calibration search for evaluating BirdBoard with a strategy bank that
// is materially richer than the 2,620-vector quick pass, without paying the
// full 212,941-vector overnight cost.
const AUCTION_MEDIUM_SCENARIOS: &[(&str, &[&str])] = &[
    ("EMPTY", &[]),
    ("JOKIC", &["Nikola Jokić"]),
    ("WEMBY", &["Victor Wembanyama"]),
    ("LUKA", &["Luka Dončić"]),
    (
        "GIANNIS + GOBERT",
        &["Giannis Antetokounmpo", "Rudy Gobert"],
    ),
    ("LUKA + CLINGAN", &["Luka Dončić", "Donovan Clingan"]),
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

const AUCTION_MEDIUM_DECISION_LIMIT: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct StrategyKey([i32; 9]);

impl StrategyKey {
    fn from_weights(weights: [f64; 9]) -> Self {
        Self(weights.map(|weight| (weight * 1000.0).round() as i32))
    }
}

#[derive(Debug, Default)]
struct CensusEntry {
    weights: [f64; 9],
    best_wins: usize,
    top_three_appearances: usize,
    best_margin_sum: f64,
    best_h_sum: f64,
    scenario_hits: HashSet<String>,
    examples: Vec<String>,
}

impl CensusEntry {
    fn average_margin(&self) -> f64 {
        if self.best_wins == 0 {
            0.0
        } else {
            self.best_margin_sum / self.best_wins as f64
        }
    }

    fn average_h(&self) -> f64 {
        if self.best_wins == 0 {
            0.0
        } else {
            self.best_h_sum / self.best_wins as f64
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyBankFile {
    pub schema_version: u32,
    pub search_profile: String,
    pub draft_season: String,
    pub category_order: [String; 9],
    pub searched_strategy_count: usize,
    pub scenario_count: usize,
    pub best_observations: usize,
    pub top_three_observations: usize,
    pub distinct_best: usize,
    pub distinct_top_three: usize,
    pub selection: StrategyBankSelection,
    pub strategies: Vec<StrategyBankEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyBankSelection {
    pub max_live_strategies: usize,
    pub coverage_target: f64,
    pub niche_min_wins: usize,
    pub niche_min_average_margin_pp: f64,
    pub selected_count: usize,
    pub selected_best_coverage: f64,
    pub core_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyBankEntry {
    pub bank_rank: usize,
    pub weights: [f64; 9],
    pub best_wins: usize,
    pub top_three_appearances: usize,
    pub state_count: usize,
    pub average_margin_pp: f64,
    pub average_h_pct: f64,
    pub best_decision_share: f64,
    pub selection_reasons: Vec<String>,
    pub examples: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RuntimeStrategyBank {
    /// Compact legacy/v30 bank. This remains the late-draft vocabulary and the
    /// fixed opponent-pricing core used by the offline oracle/trainer.
    pub source_path: PathBuf,
    pub search_profile: String,
    pub weights: Vec<[f64; 9]>,
    /// Optional broad bank learned specifically for 0-4 player auction states.
    /// If it has not been trained yet, EARLY/MID safely fall back to `weights`.
    pub early_source_path: Option<PathBuf>,
    pub early_search_profile: Option<String>,
    pub early_weights: Vec<[f64; 9]>,
}

impl RuntimeStrategyBank {
    pub fn len(&self) -> usize {
        self.weights.len()
    }

    pub fn is_empty(&self) -> bool {
        self.weights.is_empty()
    }

    pub fn early_weights_or_fallback(&self) -> &[[f64; 9]] {
        if self.early_weights.is_empty() {
            &self.weights
        } else {
            &self.early_weights
        }
    }

    pub fn early_source_or_fallback(&self) -> &PathBuf {
        self.early_source_path.as_ref().unwrap_or(&self.source_path)
    }

    pub fn early_profile_or_fallback(&self) -> &str {
        self.early_search_profile
            .as_deref()
            .unwrap_or(self.search_profile.as_str())
    }
}

/// Load the strategy vocabulary used by live BirdBoard. Runtime code is
/// deliberately agnostic about how the bank was produced or how many j
/// vectors it contains. A canonical `strategy_bank_live.json` can replace
/// research output later without any Rust changes; until then we fall back to
/// the current deep-search bank.
pub fn load_runtime_bank(draft_season: &str) -> Result<Option<RuntimeStrategyBank>> {
    let base = PathBuf::from("data")
        .join("stats")
        .join(draft_season)
        .join("durant");

    let candidates = [
        base.join("strategy_bank_live.json"),
        base.join("strategy_bank_auction.json"),
        base.join("strategy_bank_auction_medium.json"),
        base.join("strategy_bank_auction_quick.json"),
        base.join("strategy_bank_deep.json"),
        base.join("strategy_bank.json"),
    ];

    let Some(path) = candidates.into_iter().find(|path| path.exists()) else {
        return Ok(None);
    };

    let json = fs::read_to_string(&path)
        .with_context(|| format!("failed to read runtime strategy bank {}", path.display()))?;
    let bank: StrategyBankFile = serde_json::from_str(&json)
        .with_context(|| format!("failed to parse runtime strategy bank {}", path.display()))?;

    if bank.draft_season != draft_season {
        anyhow::bail!(
            "runtime strategy bank season {} does not match loaded season {}",
            bank.draft_season,
            draft_season
        );
    }

    let category_order = bank
        .category_order
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    if category_order.as_slice() != CATEGORY_ORDER.as_slice() {
        anyhow::bail!(
            "runtime strategy bank category order {:?} does not match DURANT {:?}",
            category_order,
            CATEGORY_ORDER
        );
    }

    let weights = bank
        .strategies
        .into_iter()
        .map(|entry| entry.weights)
        .collect::<Vec<_>>();
    if weights.is_empty() {
        anyhow::bail!(
            "runtime strategy bank {} contains no strategies",
            path.display()
        );
    }

    let early_path = base.join("strategy_bank_stage_early.json");
    let (early_source_path, early_search_profile, early_weights) = if early_path.exists() {
        let early_json = fs::read_to_string(&early_path).with_context(|| {
            format!(
                "failed to read early strategy bank {}",
                early_path.display()
            )
        })?;
        let early_bank: StrategyBankFile =
            serde_json::from_str(&early_json).with_context(|| {
                format!(
                    "failed to parse early strategy bank {}",
                    early_path.display()
                )
            })?;

        if early_bank.draft_season != draft_season {
            anyhow::bail!(
                "early strategy bank season {} does not match loaded season {}",
                early_bank.draft_season,
                draft_season
            );
        }
        let early_order = early_bank
            .category_order
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if early_order.as_slice() != CATEGORY_ORDER.as_slice() {
            anyhow::bail!(
                "early strategy bank category order {:?} does not match DURANT {:?}",
                early_order,
                CATEGORY_ORDER
            );
        }
        let early_profile = early_bank.search_profile.clone();
        let early = early_bank
            .strategies
            .into_iter()
            .map(|entry| entry.weights)
            .collect::<Vec<_>>();
        if early.is_empty() {
            anyhow::bail!(
                "early strategy bank {} contains no strategies",
                early_path.display()
            );
        }
        (Some(early_path), Some(early_profile), early)
    } else {
        (None, None, Vec::new())
    };

    Ok(Some(RuntimeStrategyBank {
        source_path: path,
        search_profile: bank.search_profile,
        weights,
        early_source_path,
        early_search_profile,
        early_weights,
    }))
}

#[derive(Debug, Clone, Copy)]
enum SearchProfile {
    Standard,
    Overnight,
    Deep,
    AuctionDeep,
    AuctionMedium,
    AuctionQuick,
}

impl SearchProfile {
    fn label(self) -> &'static str {
        match self {
            Self::Standard => "standard_2620",
            Self::Overnight => "overnight_fine3_coarse4_strong2",
            Self::Deep => "deep_coarse6_fine3_refined4_probe7",
            Self::AuctionDeep => "auction_no_stride_budget_deep_212941",
            Self::AuctionMedium => "auction_no_stride_budget_medium_30178",
            Self::AuctionQuick => "auction_no_stride_budget_quick_2620",
        }
    }

    fn strategy_count(self, app: &App) -> usize {
        match self {
            Self::Standard => app.durant.strategy_count(),
            Self::Overnight => app.durant.overnight_strategy_count(),
            Self::Deep => app.durant.deep_strategy_count(),
            Self::AuctionDeep => app.durant.deep_strategy_count(),
            Self::AuctionMedium => app.durant.overnight_strategy_count(),
            Self::AuctionQuick => app.durant.strategy_count(),
        }
    }

    fn score(
        self,
        app: &App,
        own_roster: &[crate::player::PlayerId],
        candidates: &[crate::player::PlayerId],
    ) -> Vec<crate::durant::DynamicDurantScore> {
        match self {
            Self::Standard => app.durant.dynamic_scores(own_roster, &[], candidates),
            Self::Overnight => app
                .durant
                .dynamic_scores_overnight(own_roster, &[], candidates),
            Self::Deep => app.durant.dynamic_scores_deep(own_roster, &[], candidates),
            Self::AuctionDeep | Self::AuctionMedium | Self::AuctionQuick => {
                use crate::durant::AuctionConfig;

                let config = AuctionConfig::default();
                let opponent_count = app.teams.len().saturating_sub(1);
                let opponent_rosters = vec![Vec::new(); opponent_count];
                let opponent_budgets = vec![config.starting_budget; opponent_count];

                // Give scenario rosters a reproducible remaining budget by
                // charging their players the neutral empty-board market price.
                let all_players = app
                    .durant
                    .scores
                    .iter()
                    .map(|score| score.player_id.clone())
                    .collect::<Vec<_>>();
                let neutral_market = app.durant.market_board(
                    &[],
                    config.starting_budget,
                    &opponent_rosters,
                    &opponent_budgets,
                    &all_players,
                    config,
                );
                let spent = own_roster
                    .iter()
                    .filter_map(|player_id| neutral_market.value_for(player_id))
                    .map(|value| value.market_price as u32)
                    .sum::<u32>();
                let open_slots = app.durant.team_size().saturating_sub(own_roster.len());
                let minimum_needed = (open_slots as u32).saturating_mul(config.minimum_bid as u32);
                let remaining = (config.starting_budget as u32).saturating_sub(spent);
                if remaining < minimum_needed {
                    return Vec::new();
                }
                let own_budget = remaining.min(u16::MAX as u32) as u16;

                match self {
                    Self::AuctionDeep => app.durant.auction_dynamic_scores_deep(
                        own_roster,
                        own_budget,
                        &opponent_rosters,
                        &opponent_budgets,
                        candidates,
                    ),
                    Self::AuctionMedium => {
                        let evaluated = candidates
                            .iter()
                            .take(AUCTION_MEDIUM_DECISION_LIMIT)
                            .cloned()
                            .collect::<Vec<_>>();

                        app.durant.auction_dynamic_scores_medium(
                            own_roster,
                            own_budget,
                            &opponent_rosters,
                            &opponent_budgets,
                            candidates,
                            &evaluated,
                        )
                    }
                    Self::AuctionQuick => {
                        // Learn only from the most relevant candidate decisions,
                        // but let every rollout buy from the full remaining pool.
                        let evaluated = candidates
                            .iter()
                            .take(AUCTION_QUICK_DECISION_LIMIT)
                            .cloned()
                            .collect::<Vec<_>>();

                        app.durant.auction_dynamic_scores_quick(
                            own_roster,
                            own_budget,
                            &opponent_rosters,
                            &opponent_budgets,
                            candidates,
                            &evaluated,
                        )
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    fn scenarios(self) -> &'static [(&'static str, &'static [&'static str])] {
        match self {
            Self::AuctionDeep => AUCTION_CENSUS_SCENARIOS,
            Self::AuctionMedium => AUCTION_MEDIUM_SCENARIOS,
            Self::AuctionQuick => AUCTION_QUICK_SCENARIOS,
            _ => CENSUS_SCENARIOS,
        }
    }

    fn output_suffix(self) -> &'static str {
        match self {
            Self::Standard => "",
            Self::Overnight => "_overnight",
            Self::Deep => "_deep",
            Self::AuctionDeep => "_auction",
            Self::AuctionMedium => "_auction_medium",
            Self::AuctionQuick => "_auction_quick",
        }
    }
}

pub fn print_overnight_plan(app: &App) {
    let fine_three = 19_495usize;
    let coarse_four = 10_206usize;
    let extra_strong = 477usize;

    println!();
    println!("========================================================================");
    println!("DURANT OVERNIGHT j SEARCH");
    println!("========================================================================");
    println!("fine 1-3 category strategies : {fine_three}");
    println!("coarse 4-category strategies : {coarse_four}");
    println!("extra 2.0 push probes        : {extra_strong}");
    println!(
        "unique j vectors             : {}",
        app.durant.overnight_strategy_count()
    );
    println!("representative roster states : {}", CENSUS_SCENARIOS.len());
    println!("live-bank cap                : {MAX_LIVE_STRATEGIES}");
    println!(
        "coverage target              : {:.1}%",
        COVERAGE_TARGET * 100.0
    );
    println!();
    println!("Normal BirdBoard remains on the 2,620-j search.");
    println!("Overnight results use a separate cache and separate output files.");
}

pub fn print_deep_plan(app: &App) {
    println!();
    println!("========================================================================");
    println!("DURANT FULL-NIGHT j SEARCH");
    println!("========================================================================");
    println!("coarse <=6 categories       : 104680");
    println!("fine 1-3 categories         : +16875 unique");
    println!("refined exact-4 surface     : +30240 unique");
    println!("7-category boundary probe   : +16128");
    println!("refined exact-5 surface     : +26586");
    println!("8-category boundary probe   : +18432");
    println!(
        "unique j vectors            : {}",
        app.durant.deep_strategy_count()
    );
    println!("representative roster states: {}", CENSUS_SCENARIOS.len());
    println!("live-bank cap               : {MAX_LIVE_STRATEGIES}");
    println!(
        "coverage target             : {:.1}%",
        COVERAGE_TARGET * 100.0
    );
    println!();
    println!("Sized to ~10 hours from the measured 85-minute / 30,178-j release run.");
    println!("Normal and 30k searches keep separate caches and outputs.");
}

pub fn print_auction_relearn_plan(app: &App) {
    println!();
    println!("========================================================================");
    println!("DURANT AUCTION-AWARE OVERNIGHT j RELEARN");
    println!("========================================================================");
    println!(
        "j vectors                    : {}",
        app.durant.deep_strategy_count()
    );
    println!(
        "representative roster states : {}",
        AUCTION_CENSUS_SCENARIOS.len()
    );
    println!("roster sizes covered          : 0 through 7");
    println!("live-bank cap                 : {MAX_LIVE_STRATEGIES}");
    println!(
        "coverage target               : {:.1}%",
        COVERAGE_TARGET * 100.0
    );
    println!();
    println!("Objective: BUY each candidate at current market, then maximize completed-roster H");
    println!("under the current $200 no-stride auction model.");
    println!("Search is resumable per roster state in its own cache namespace.");
    println!("Expected wall time on the 8-thread machine: roughly 8-11 hours.");
    println!("Outputs: strategy_bank_auction.json and strategy_census_auction.csv");
}

pub fn print_auction_medium_plan(app: &App) {
    println!();
    println!("========================================================================");
    println!("DURANT MEDIUM AUCTION j CALIBRATION");
    println!("========================================================================");
    println!(
        "j vectors                    : {}",
        app.durant.overnight_strategy_count()
    );
    println!(
        "representative roster states : {}",
        AUCTION_MEDIUM_SCENARIOS.len()
    );
    println!("candidate decisions / state  : <= {AUCTION_MEDIUM_DECISION_LIMIT}");
    println!("future player pool            : full remaining DURANT pool");
    println!("auction model                 : no stride, $200 budget, $1 slot reserve");
    println!("memory model                  : 4,096-j ranking chunks");
    println!();
    println!("Objective: maximize completed-roster H after BUY @ market price.");
    println!("Strategy space: the 30,178-vector fine/coarse overnight library.");
    println!("This bank is intended for serious BirdBoard evaluation before the full deep search.");
    println!("Expected wall time from the measured quick run: roughly 5-15 minutes.");
    println!("Outputs: strategy_bank_auction_medium.json + strategy_census_auction_medium.csv");
}

pub fn print_auction_quick_plan(app: &App) {
    println!();
    println!("========================================================================");
    println!("DURANT QUICK AUCTION j CALIBRATION");
    println!("========================================================================");
    println!(
        "j vectors                    : {}",
        app.durant.strategy_count()
    );
    println!(
        "representative roster states : {}",
        AUCTION_QUICK_SCENARIOS.len()
    );
    println!("candidate decisions / state  : <= {AUCTION_QUICK_DECISION_LIMIT}");
    println!("future player pool            : full remaining DURANT pool");
    println!("auction model                 : no stride, $200 budget, $1 slot reserve");
    println!();
    println!("Objective: maximize completed-roster H after BUY @ market price.");
    println!("There is no draft-turn / 13-player availability heuristic.");
    println!("This is a calibration bank for plan inspection, not the final overnight bank.");
    println!("Target wall time: comfortably under 30 minutes on the 8-thread machine.");
    println!("Outputs: strategy_bank_auction_quick.json + strategy_census_auction_quick.csv");
}

pub fn build_and_save(app: &App) -> Result<()> {
    build_and_save_profile(app, SearchProfile::Standard)
}

pub fn build_overnight_and_save(app: &App) -> Result<()> {
    build_and_save_profile(app, SearchProfile::Overnight)
}

pub fn build_deep_and_save(app: &App) -> Result<()> {
    build_and_save_profile(app, SearchProfile::Deep)
}

pub fn build_auction_and_save(app: &App) -> Result<()> {
    build_and_save_profile(app, SearchProfile::AuctionDeep)
}

pub fn build_auction_medium_and_save(app: &App) -> Result<()> {
    build_and_save_profile(app, SearchProfile::AuctionMedium)
}

pub fn build_auction_quick_and_save(app: &App) -> Result<()> {
    build_and_save_profile(app, SearchProfile::AuctionQuick)
}

fn build_and_save_profile(app: &App, profile: SearchProfile) -> Result<()> {
    let run_started = Instant::now();
    let mut census = HashMap::<StrategyKey, CensusEntry>::new();
    let mut total_best_observations = 0usize;
    let mut total_top_three_observations = 0usize;

    println!();
    println!("========================================================================");
    println!("DURANT STRATEGY-BANK EXTRACTION");
    println!("========================================================================");
    let scenarios = profile.scenarios();

    println!(
        "{} representative states | {} j vectors | {}",
        scenarios.len(),
        profile.strategy_count(app),
        profile.label(),
    );

    let all_candidates = app
        .durant
        .scores
        .iter()
        .map(|score| score.player_id.clone())
        .collect::<Vec<_>>();

    for (scenario_index, (scenario_name, roster_names)) in scenarios.iter().enumerate() {
        let scenario_started = Instant::now();
        println!(
            "  [{:>2}/{}] {:<20} starting...",
            scenario_index + 1,
            scenarios.len(),
            scenario_name,
        );
        let mut missing = Vec::new();

        let own_roster = roster_names
            .iter()
            .filter_map(|name| {
                let score = app
                    .durant
                    .scores
                    .iter()
                    .find(|score| score.player_name == *name);

                match score {
                    Some(score) => Some(score.player_id.clone()),
                    None => {
                        missing.push((*name).to_string());
                        None
                    }
                }
            })
            .collect::<Vec<_>>();

        if !missing.is_empty() {
            println!(
                "  {:<20} SKIPPED — missing DURANT player(s): {}",
                scenario_name,
                missing.join(", "),
            );
            continue;
        }

        let owned = own_roster.iter().cloned().collect::<HashSet<_>>();

        let candidates = all_candidates
            .iter()
            .filter(|player_id| !owned.contains(*player_id))
            .cloned()
            .collect::<Vec<_>>();

        let scores = profile.score(app, &own_roster, &candidates);

        println!(
            "             {:<20} {:>4} decisions | {:>6.1} min",
            scenario_name,
            scores.len(),
            scenario_started.elapsed().as_secs_f64() / 60.0,
        );

        for score in scores {
            total_best_observations += 1;

            let best_key = StrategyKey::from_weights(score.j_weights);
            let entry = census.entry(best_key).or_default();

            if entry.best_wins == 0 && entry.top_three_appearances == 0 {
                entry.weights = score.j_weights;
            }

            entry.best_wins += 1;
            entry.top_three_appearances += 1;
            entry.best_margin_sum += score.j_margin;
            entry.best_h_sum += score.projected_matchup_win_probability;
            entry.scenario_hits.insert((*scenario_name).to_string());

            if entry.examples.len() < 3 {
                entry
                    .examples
                    .push(format!("{scenario_name} / {}", score.player_name));
            }

            total_top_three_observations += 1;

            for alternative in &score.j_alternatives {
                let key = StrategyKey::from_weights(alternative.j_weights);
                let alt_entry = census.entry(key).or_default();

                if alt_entry.best_wins == 0 && alt_entry.top_three_appearances == 0 {
                    alt_entry.weights = alternative.j_weights;
                }

                alt_entry.top_three_appearances += 1;
                alt_entry.scenario_hits.insert((*scenario_name).to_string());
                total_top_three_observations += 1;
            }
        }
    }

    let mut rows = census.into_iter().collect::<Vec<_>>();

    rows.sort_by(|a, b| {
        b.1.best_wins
            .cmp(&a.1.best_wins)
            .then_with(|| b.1.top_three_appearances.cmp(&a.1.top_three_appearances))
            .then_with(|| b.1.average_margin().total_cmp(&a.1.average_margin()))
    });

    let distinct_best = rows.iter().filter(|(_, entry)| entry.best_wins > 0).count();

    let distinct_top_three = rows
        .iter()
        .filter(|(_, entry)| entry.top_three_appearances > 0)
        .count();

    let mut selected = HashSet::<StrategyKey>::new();
    let mut reasons = HashMap::<StrategyKey, HashSet<String>>::new();
    let mut covered_best = 0usize;
    let mut core_count = 0usize;

    // 1. Coverage core: most frequent winners until the requested share of
    // best-j decisions is covered.
    for (key, entry) in &rows {
        if entry.best_wins == 0 || selected.len() >= MAX_LIVE_STRATEGIES {
            continue;
        }

        selected.insert(*key);
        reasons
            .entry(*key)
            .or_default()
            .insert("coverage_core".to_string());
        covered_best += entry.best_wins;
        core_count += 1;

        let coverage = if total_best_observations == 0 {
            0.0
        } else {
            covered_best as f64 / total_best_observations as f64
        };

        if coverage >= COVERAGE_TARGET {
            break;
        }
    }

    // 2. High-conviction niche additions: low-frequency strategies that win
    // by a meaningful H margin in the states where they matter.
    let mut niche = rows
        .iter()
        .filter(|(_, entry)| {
            entry.best_wins >= NICHE_MIN_WINS && entry.average_margin() >= NICHE_MIN_AVG_MARGIN
        })
        .collect::<Vec<_>>();

    niche.sort_by(|a, b| {
        b.1.average_margin()
            .total_cmp(&a.1.average_margin())
            .then_with(|| b.1.best_wins.cmp(&a.1.best_wins))
            .then_with(|| b.1.top_three_appearances.cmp(&a.1.top_three_appearances))
    });

    for (key, _) in niche {
        if selected.contains(key) {
            reasons
                .entry(*key)
                .or_default()
                .insert("high_margin_niche".to_string());
            continue;
        }

        if selected.len() >= MAX_LIVE_STRATEGIES {
            break;
        }

        selected.insert(*key);
        reasons
            .entry(*key)
            .or_default()
            .insert("high_margin_niche".to_string());
    }

    // 3. Top-3 insurance: use any remaining capacity for strategies that
    // repeatedly challenged the winner. With the current 2,620-j census this
    // should retain every strategy that ever appeared in the top three.
    for (key, entry) in &rows {
        if entry.top_three_appearances == 0 {
            continue;
        }

        if selected.contains(key) {
            reasons
                .entry(*key)
                .or_default()
                .insert("top3_competitive".to_string());
            continue;
        }

        if selected.len() >= MAX_LIVE_STRATEGIES {
            break;
        }

        selected.insert(*key);
        reasons
            .entry(*key)
            .or_default()
            .insert("top3_competitive".to_string());
    }

    let selected_best_wins = rows
        .iter()
        .filter(|(key, _)| selected.contains(key))
        .map(|(_, entry)| entry.best_wins)
        .sum::<usize>();

    let selected_best_coverage = if total_best_observations == 0 {
        0.0
    } else {
        selected_best_wins as f64 / total_best_observations as f64
    };

    let mut selected_rows = rows
        .iter()
        .filter(|(key, _)| selected.contains(key))
        .collect::<Vec<_>>();

    selected_rows.sort_by(|a, b| {
        b.1.best_wins
            .cmp(&a.1.best_wins)
            .then_with(|| b.1.top_three_appearances.cmp(&a.1.top_three_appearances))
            .then_with(|| b.1.average_margin().total_cmp(&a.1.average_margin()))
    });

    let strategies = selected_rows
        .iter()
        .enumerate()
        .map(|(index, (key, entry))| {
            let mut selection_reasons = reasons
                .get(key)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .collect::<Vec<_>>();
            selection_reasons.sort();

            StrategyBankEntry {
                bank_rank: index + 1,
                weights: entry.weights,
                best_wins: entry.best_wins,
                top_three_appearances: entry.top_three_appearances,
                state_count: entry.scenario_hits.len(),
                average_margin_pp: entry.average_margin() * 100.0,
                average_h_pct: entry.average_h() * 100.0,
                best_decision_share: if total_best_observations == 0 {
                    0.0
                } else {
                    entry.best_wins as f64 / total_best_observations as f64
                },
                selection_reasons,
                examples: entry.examples.clone(),
            }
        })
        .collect::<Vec<_>>();

    let bank = StrategyBankFile {
        schema_version: BANK_SCHEMA_VERSION,
        search_profile: profile.label().to_string(),
        draft_season: app.stats.draft_season.clone(),
        category_order: CATEGORY_ORDER.map(str::to_string),
        searched_strategy_count: profile.strategy_count(app),
        scenario_count: scenarios.len(),
        best_observations: total_best_observations,
        top_three_observations: total_top_three_observations,
        distinct_best,
        distinct_top_three,
        selection: StrategyBankSelection {
            max_live_strategies: MAX_LIVE_STRATEGIES,
            coverage_target: COVERAGE_TARGET,
            niche_min_wins: NICHE_MIN_WINS,
            niche_min_average_margin_pp: NICHE_MIN_AVG_MARGIN * 100.0,
            selected_count: strategies.len(),
            selected_best_coverage,
            core_count,
        },
        strategies,
    };

    let output_dir = PathBuf::from("data")
        .join("stats")
        .join(&app.stats.draft_season)
        .join("durant");

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create {}", output_dir.display()))?;

    let json_path = output_dir.join(format!("strategy_bank{}.json", profile.output_suffix()));
    let json = serde_json::to_string_pretty(&bank)?;
    fs::write(&json_path, json)
        .with_context(|| format!("failed to write {}", json_path.display()))?;

    let census_path = output_dir.join(format!("strategy_census{}.csv", profile.output_suffix()));
    write_census_csv(
        &census_path,
        &rows,
        &selected,
        &reasons,
        total_best_observations,
    )?;

    println!();
    println!("SUMMARY");
    println!("searched j vectors  : {}", bank.searched_strategy_count);
    println!("best-j observations : {}", bank.best_observations);
    println!("top-3 observations  : {}", bank.top_three_observations);
    println!("distinct best j     : {}", bank.distinct_best);
    println!("distinct top-3 j    : {}", bank.distinct_top_three);
    println!("coverage core       : {}", bank.selection.core_count);
    println!("bank size           : {}", bank.selection.selected_count);
    println!(
        "best-j coverage     : {:.2}%",
        bank.selection.selected_best_coverage * 100.0,
    );
    println!(
        "wall time           : {:.2} hours",
        run_started.elapsed().as_secs_f64() / 3600.0,
    );
    println!();
    println!("saved {}", json_path.display());
    println!("saved {}", census_path.display());

    if distinct_top_three > MAX_LIVE_STRATEGIES {
        println!();
        println!(
            "NOTE: {} strategies appeared in the top three, so the live bank was capped at {}.",
            distinct_top_three, MAX_LIVE_STRATEGIES,
        );
        println!(
            "The full census is preserved in {} for tomorrow's pruning review.",
            census_path.display(),
        );
    }

    Ok(())
}

fn write_census_csv(
    path: &std::path::Path,
    rows: &[(StrategyKey, CensusEntry)],
    selected: &HashSet<StrategyKey>,
    reasons: &HashMap<StrategyKey, HashSet<String>>,
    total_best_observations: usize,
) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)
        .with_context(|| format!("failed to create {}", path.display()))?;

    writer.write_record([
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
        "best_wins",
        "top_three_appearances",
        "state_count",
        "average_margin_pp",
        "average_h_pct",
        "best_decision_share_pct",
        "selected",
        "selection_reasons",
        "examples",
    ])?;

    for (index, (key, entry)) in rows.iter().enumerate() {
        let mut selection_reasons = reasons
            .get(key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect::<Vec<_>>();
        selection_reasons.sort();

        let best_share = if total_best_observations == 0 {
            0.0
        } else {
            entry.best_wins as f64 / total_best_observations as f64 * 100.0
        };

        let mut record = Vec::with_capacity(19);
        record.push((index + 1).to_string());

        for weight in entry.weights {
            record.push(format_weight(weight));
        }

        record.push(entry.best_wins.to_string());
        record.push(entry.top_three_appearances.to_string());
        record.push(entry.scenario_hits.len().to_string());
        record.push(format!("{:.6}", entry.average_margin() * 100.0));
        record.push(format!("{:.6}", entry.average_h() * 100.0));
        record.push(format!("{best_share:.6}"));
        record.push(selected.contains(key).to_string());
        record.push(selection_reasons.join("|"));
        record.push(entry.examples.join(" | "));

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
