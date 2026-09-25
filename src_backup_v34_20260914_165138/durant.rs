use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
};

use anyhow::{Result, bail};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    player::PlayerId,
    stats::{PlayerNineCatStats, PlayerWeeklyStats, StatsBundle},
};

/// BirdBoard requires a minimum number of active scoring periods so tiny
/// samples do not enter the static model. Missing zero-game weeks are not
/// inserted into the weekly cache, so this is an active-week filter.
pub const MIN_ACTIVE_WEEKS: usize = 10;

/// Optional hand-maintained per-game projections for players whose current
/// season sample is missing or not representative (returning injured players,
/// rookies, major role changes, etc.). These rows are scored by DURANT but are
/// deliberately excluded from fitting the historical reference environment.
pub const MANUAL_OVERRIDES_PATH: &str = "data/projections/manual_overrides.csv";

#[derive(Debug, Clone, Deserialize)]
struct ManualProjectionOverride {
    #[serde(default = "default_true")]
    enabled: bool,
    player_id: PlayerId,
    player_name: String,
    #[allow(dead_code)]
    #[serde(default)]
    team: String,
    #[allow(dead_code)]
    #[serde(default)]
    minutes_pg: f64,
    fgm_pg: f64,
    fga_pg: f64,
    ftm_pg: f64,
    fta_pg: f64,
    threes_pg: f64,
    points_pg: f64,
    rebounds_pg: f64,
    assists_pg: f64,
    steals_pg: f64,
    blocks_pg: f64,
    turnovers_pg: f64,
    #[allow(dead_code)]
    #[serde(default)]
    note: String,
    #[allow(dead_code)]
    #[serde(default)]
    source: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CountingParameters {
    /// Mean of player-level weekly means across the reference population Q.
    pub mean: f64,
    /// Player-to-player standard deviation of weekly means.
    pub sigma: f64,
    /// RMS week-to-week standard deviation across players.
    pub tau: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PercentageParameters {
    /// Mean weekly attempt volume across Q.
    pub mean_attempts: f64,
    /// Attempt-weighted composite shooting percentage across Q.
    pub mean_rate: f64,
    /// Player-to-player standard deviation of volume-adjusted shooting impact.
    pub sigma_rate: f64,
    /// RMS week-to-week standard deviation of volume-weighted percentage impact.
    pub tau_rate: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DurantParameters {
    pub points: CountingParameters,
    pub threes: CountingParameters,
    pub rebounds: CountingParameters,
    pub assists: CountingParameters,
    pub steals: CountingParameters,
    pub blocks: CountingParameters,
    pub turnovers: CountingParameters,
    pub field_goal: PercentageParameters,
    pub free_throw: PercentageParameters,
}

#[derive(Debug, Clone)]
pub struct DurantScore {
    pub player_id: PlayerId,
    pub player_name: String,

    pub field_goal: f64,
    pub free_throw: f64,
    pub threes: f64,
    pub points: f64,
    pub rebounds: f64,
    pub assists: f64,
    pub steals: f64,
    pub blocks: f64,
    pub turnovers: f64,

    pub total: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicDurantScore {
    pub player_id: PlayerId,
    pub player_name: String,

    /// Candidate's X-score contributions, ordered as:
    /// FG%, FT%, 3PM, PTS, REB, AST, STL, BLK, TO.
    pub x_scores: [f64; 9],

    /// Immediate category win probabilities after adding only this candidate.
    pub category_win_probabilities: [f64; 9],

    /// Immediate expected number of categories won.
    pub expected_categories: f64,

    /// Immediate probability of winning at least five of nine categories.
    pub matchup_win_probability: f64,

    /// Immediate increase in matchup win probability versus the current roster.
    pub marginal_matchup_win_probability: f64,

    /// Best deterministic continuation found after filling the remaining
    /// roster spots from the currently available player pool.
    pub projected_matchup_win_probability: f64,
    pub projected_expected_categories: f64,
    pub projected_category_win_probabilities: [f64; 9],

    /// Human-readable identity of the projected completed roster. This is
    /// derived from the final category win probabilities, not directly from j.
    pub build_name: String,
    /// Human-readable interpretation of the best future category-weight vector j.
    /// A zero weight can mean either a true punt or a category that is already
    /// strong enough to coast; j_name distinguishes those cases.
    pub j_name: String,
    /// Best future category-weight vector j, in DYNAMIC_CATEGORY_NAMES order.
    pub j_weights: [f64; 9],
    /// Runner-up j strategies from the same search, best first. These make
    /// strategy flexibility/rigidity observable instead of hiding it behind
    /// a single argmax.
    pub j_alternatives: Vec<StrategyAlternative>,
    /// Difference between the best and second-best projected matchup win
    /// probabilities. Small means many strategies are essentially tied.
    pub j_margin: f64,

    /// Greedy future players selected under the best j. These are diagnostics
    /// now and can later power the Strategy UI / draft explanation panel.
    pub projected_future_players: Vec<PlayerId>,
    pub projected_future_player_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyAlternative {
    pub j_name: String,
    pub j_weights: [f64; 9],
    pub projected_matchup_win_probability: f64,
    pub projected_expected_categories: f64,
    pub build_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurantRosterPlan {
    pub current_matchup_win_probability: f64,
    pub current_expected_categories: f64,
    pub current_category_win_probabilities: [f64; 9],

    pub projected_matchup_win_probability: f64,
    pub projected_expected_categories: f64,
    pub projected_category_win_probabilities: [f64; 9],

    pub build_name: String,
    pub j_name: String,
    pub j_weights: [f64; 9],
    pub j_alternatives: Vec<StrategyAlternative>,
    pub j_margin: f64,

    pub projected_future_players: Vec<PlayerId>,
    pub projected_future_player_names: Vec<String>,
    pub projected_future_spend: u16,
    pub projected_budget_left: u16,
}

/// Auction settings. BirdBoard currently assumes integer-dollar bids.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AuctionConfig {
    /// Starting budget for each fantasy team, normally $200.
    pub starting_budget: u16,
    /// Minimum legal bid / amount that must be reserved for every open slot.
    pub minimum_bid: u16,
    /// Number of bisection steps used for H-buy / H-pass indifference pricing.
    /// Eight steps is enough to resolve a $200 auction to the nearest dollar.
    pub fair_price_iterations: u8,
}

impl Default for AuctionConfig {
    fn default() -> Self {
        Self {
            starting_budget: 200,
            minimum_bid: 1,
            fair_price_iterations: 8,
        }
    }
}

/// Snapshot of the remaining auction economy. This is BirdBoard's practical
/// counterpart to Rosenof's replacement level R and dollar-value term D.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketEconomySnapshot {
    pub remaining_roster_slots: usize,
    pub remaining_league_dollars: u32,
    pub reserved_minimum_dollars: u32,
    pub discretionary_dollars: u32,

    /// G-score of the best player expected to remain undrafted if every open
    /// roster slot were filled from the current available pool.
    pub replacement_g_score: f64,
    /// Smoothed generic replacement X-vector around the draft/undrafted cutoff.
    pub replacement_x_scores: [f64; 9],

    /// Total positive G-score above replacement among players expected to be
    /// drafted from the current pool.
    pub total_g_above_replacement: f64,
    /// Statistical above-replacement G value represented by one discretionary
    /// auction dollar. This is the direct analogue of the paper's D scale.
    pub g_above_replacement_per_dollar: f64,
    pub dollars_per_g_above_replacement: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuctionDurantScore {
    pub player_id: PlayerId,
    pub player_name: String,

    /// Generic market estimate from the remaining G-score economy.
    pub market_price_exact: f64,
    pub market_price: u16,
    /// Price actually used for the market-price BUY diagnostics. This only
    /// differs from market_price when the generic market estimate exceeds our
    /// current legal maximum bid.
    pub evaluated_market_price: u16,
    /// Team-specific H-score indifference price: the highest integer bid where
    /// BUY is still at least as good as PASS under the current rollout model.
    pub fair_price: u16,
    pub expected_edge: i16,

    /// Best completed-roster H-score if the nominated player disappears and we
    /// keep all of our money.
    pub pass_projected_matchup_win_probability: f64,
    /// Best completed-roster H-score if we buy at the generic market estimate.
    pub market_price_projected_matchup_win_probability: f64,
    /// H-score at the reported fair price (normally just above PASS).
    pub fair_price_projected_matchup_win_probability: f64,

    /// Strategy/build diagnostics at the generic market price.
    pub build_name: String,
    pub j_name: String,
    pub j_weights: [f64; 9],
    pub projected_category_win_probabilities: [f64; 9],
    pub projected_future_players: Vec<PlayerId>,
    pub projected_future_player_names: Vec<String>,
    pub projected_future_spend: u16,
    pub projected_budget_left: u16,
}

#[derive(Debug, Clone)]
pub struct MarketAdvantageScore {
    pub player_id: PlayerId,
    pub player_name: String,
    pub market_price: u16,

    /// True when FINAL/build fields were produced by a future-roster rollout.
    /// The broad Live board intentionally computes only cheap NOW ΔH; a small
    /// shortlist gets the coarse future projection and Enter keeps the exact
    /// rational-market calculation.
    pub has_projected_finish: bool,

    /// Current partial-roster H before adding this candidate.
    pub current_matchup_win_probability: f64,
    pub current_category_win_probabilities: [f64; 9],

    /// Immediate H after adding only this candidate, before any future roster
    /// construction. This is the "how much does the player move me right now?"
    /// diagnostic from the original Dynamic DURANT experiments.
    pub immediate_matchup_win_probability: f64,
    pub marginal_immediate_matchup_win_probability: f64,
    pub immediate_category_win_probabilities: [f64; 9],

    /// Difference between buying this player at the current market estimate
    /// and letting somebody else buy him after optimally completing the roster:
    /// H_buy(market) - H_pass.
    pub marginal_projected_matchup_win_probability: f64,
    pub buy_projected_matchup_win_probability: f64,
    pub pass_projected_matchup_win_probability: f64,

    pub pass_projected_category_win_probabilities: [f64; 9],
    pub buy_projected_category_win_probabilities: [f64; 9],

    pub build_name: String,
    pub j_name: String,
    pub j_weights: [f64; 9],
    pub projected_future_players: Vec<PlayerId>,
    pub projected_future_player_names: Vec<String>,
    pub projected_future_spend: u16,
    pub projected_budget_left: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuctionAnalysis {
    pub economy: MarketEconomySnapshot,
    pub scores: Vec<AuctionDurantScore>,
}

#[derive(Debug, Clone)]
pub struct MarketValue {
    pub player_id: PlayerId,
    pub market_price_exact: f64,
    pub market_price: u16,
}

#[derive(Debug, Clone)]
pub struct MarketBoard {
    pub economy: MarketEconomySnapshot,
    pub values: Vec<MarketValue>,
}

impl MarketBoard {
    pub fn empty() -> Self {
        Self {
            economy: MarketEconomySnapshot {
                remaining_roster_slots: 0,
                remaining_league_dollars: 0,
                reserved_minimum_dollars: 0,
                discretionary_dollars: 0,
                replacement_g_score: 0.0,
                replacement_x_scores: [0.0; 9],
                total_g_above_replacement: 0.0,
                g_above_replacement_per_dollar: 0.0,
                dollars_per_g_above_replacement: 0.0,
            },
            values: Vec::new(),
        }
    }

    pub fn value_for(&self, player_id: &PlayerId) -> Option<&MarketValue> {
        self.values
            .iter()
            .find(|value| &value.player_id == player_id)
    }
}

#[derive(Debug, Clone)]
pub struct DurantModel {
    /// Number of players in Q. For a 13-team, 13-player league this is 169.
    pub reference_size: usize,
    /// Number of players whose performances count for one fantasy team.
    pub team_size: usize,
    /// Q, selected with conventional nine-category Z-score as in the paper.
    pub reference_players: Vec<PlayerId>,
    /// Fitted category-level parameters. These are also useful later for
    /// X-score / H-score calculations.
    pub parameters: DurantParameters,
    /// Player-to-player X-score variance across Q, ordered as:
    /// FG%, FT%, 3PM, PTS, REB, AST, STL, BLK, TO. This is the
    /// X_sigma^2 term used by Rosenof's dynamic matchup model.
    pub x_variance: [f64; 9],
    /// All players with a usable per-game projection/stat line, sorted
    /// best-to-worst by aggregate G-score. Weekly-history eligibility is only
    /// used to fit the reference environment and tau; it must not gate scoring.
    pub scores: Vec<DurantScore>,
    /// Historical stats cache directory, reused for persistent dynamic-search
    /// results so expensive j searches only need to be done once per state.
    cache_dir: PathBuf,
    /// Fingerprint of the fitted model used to invalidate stale dynamic caches.
    model_fingerprint: u64,
}

impl DurantModel {
    pub fn empty(league_teams: usize, roster_size: usize) -> Self {
        let reference_size = league_teams.saturating_mul(roster_size);

        Self {
            reference_size,
            team_size: roster_size,
            reference_players: Vec::new(),
            parameters: DurantParameters::default(),
            x_variance: [0.0; 9],
            scores: Vec::new(),
            cache_dir: PathBuf::new(),
            model_fingerprint: 0,
        }
    }

    /// Fit a static G-score model from historical BirdBoard statistics.
    ///
    /// `league_teams * roster_size` determines Q.
    pub fn from_stats(
        stats: &StatsBundle,
        league_teams: usize,
        roster_size: usize,
    ) -> Result<Self> {
        if league_teams == 0 {
            bail!("Durant requires at least one fantasy team");
        }

        if roster_size == 0 {
            bail!("Durant requires at least one rostered player per team");
        }

        let reference_size = league_teams * roster_size;

        let mut manual_overrides = load_manual_projection_overrides(MANUAL_OVERRIDES_PATH)?;
        canonicalize_manual_projection_overrides(&mut manual_overrides, stats);
        let override_ids = manual_overrides
            .iter()
            .map(|projection| projection.player_id.clone())
            .collect::<HashSet<_>>();

        let weekly_by_player = group_weekly(&stats.weekly);

        // Manual projections must not alter the historical environment used to
        // fit mu/sigma/tau. If an overridden player also has historical weekly
        // rows, exclude that player from Q and replace only their final score.
        let eligible_ids = weekly_by_player
            .iter()
            .filter_map(|(player_id, weeks)| {
                (weeks.len() >= MIN_ACTIVE_WEEKS && !override_ids.contains(player_id))
                    .then_some(player_id.clone())
            })
            .collect::<HashSet<_>>();

        if eligible_ids.len() < reference_size {
            bail!(
                "only {} non-overridden players have at least {} active weeks; Durant needs {} for Q",
                eligible_ids.len(),
                MIN_ACTIVE_WEEKS,
                reference_size
            );
        }

        let reference_players =
            select_reference_population(&stats.players, &eligible_ids, reference_size)?;

        let parameters = fit_parameters(&reference_players, &weekly_by_player)?;

        // Weekly game logs serve ONE job here: fit the historical environment
        // (mu/sigma/tau) and estimate the common active-week game volume.
        //
        // They must NOT decide which NBA players are allowed to receive a
        // DURANT score. A star can have fewer than MIN_ACTIVE_WEEKS, be absent
        // from the 220-player weekly fetch, or have incomplete PBP logs and
        // still have a perfectly usable per-game projection/stat line.
        //
        // Score every player in the projection/stat pool on the same projected
        // weekly scale. This also makes historical seeds and manual overrides
        // obey the same scoring rule.
        let games_per_active_week =
            mean_active_games_per_week(&reference_players, &weekly_by_player);

        let mut scores = stats
            .players
            .iter()
            .map(|player| score_stat_projection(player, games_per_active_week, parameters))
            .collect::<Vec<_>>();

        // Manual projections replace only the player's projected mean line.
        // They remain excluded from fitting the historical reference
        // environment above, so user edits cannot move mu/sigma/tau.
        for projection in &manual_overrides {
            scores.retain(|score| score.player_id != projection.player_id);
            scores.push(score_manual_projection(
                projection,
                games_per_active_week,
                parameters,
            ));
        }

        scores.sort_by(|a, b| b.total.total_cmp(&a.total));

        let x_variance = fit_x_variance(&reference_players, &scores, parameters);
        let model_fingerprint = fingerprint_model(&scores, parameters, reference_size, roster_size);

        Ok(Self {
            reference_size,
            team_size: roster_size,
            reference_players,
            parameters,
            x_variance,
            scores,
            cache_dir: stats.cache_dir.clone(),
            model_fingerprint,
        })
    }

    pub fn team_size(&self) -> usize {
        self.team_size
    }

    pub fn score_for(&self, player_id: &PlayerId) -> Option<&DurantScore> {
        self.scores
            .iter()
            .find(|score| &score.player_id == player_id)
    }

    /// Cheap generic market board for the current auction state. This does not
    /// run any j-search or H-score rollout.
    pub fn market_board(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        config: AuctionConfig,
    ) -> MarketBoard {
        let totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let economy = self.estimate_market_economy_from_totals(candidates, None, totals, config);

        let mut values = candidates
            .iter()
            .filter_map(|player_id| {
                Some(MarketValue {
                    player_id: player_id.clone(),
                    market_price_exact: *economy.exact_prices.get(player_id)?,
                    market_price: *economy.prices.get(player_id)?,
                })
            })
            .collect::<Vec<_>>();

        values.sort_by(|a, b| b.market_price_exact.total_cmp(&a.market_price_exact));

        MarketBoard {
            economy: economy.snapshot,
            values,
        }
    }

    /// Immediate roster-fit signal used by the live Big Board. This is the
    /// candidate's change in matchup win probability before any future rollout.
    pub fn immediate_fit(&self, own_roster: &[PlayerId], candidate: &PlayerId) -> Option<f64> {
        let candidate_x = self.x_score_for(candidate)?;
        let own_x = self.aggregate_x_scores(own_roster);
        let generic_opponent = [([0.0; 9], 0usize)];
        let before = self.evaluate_dynamic_state(own_x, &generic_opponent);
        let after = self.evaluate_dynamic_state(add_vectors(own_x, candidate_x), &generic_opponent);
        Some(after.matchup_win_probability - before.matchup_win_probability)
    }

    /// Aggregate X profile for roster scouting. Higher is better in every
    /// category, including turnovers.
    pub fn roster_x_profile(&self, roster: &[PlayerId]) -> [f64; 9] {
        self.aggregate_x_scores(roster)
    }

    /// Return the player's dynamic X-score vector. Higher is better in every
    /// component, including turnovers, where the sign has already been
    /// reversed by the static score.
    pub fn x_score_for(&self, player_id: &PlayerId) -> Option<[f64; 9]> {
        self.score_for(player_id)
            .map(|score| score_to_x_vector(score, self.parameters))
    }

    fn dynamic_cache_key(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> u64 {
        let mut hasher = DefaultHasher::new();
        DYNAMIC_SEARCH_VERSION.hash(&mut hasher);
        self.model_fingerprint.hash(&mut hasher);
        self.reference_size.hash(&mut hasher);
        self.team_size.hash(&mut hasher);

        own_roster.len().hash(&mut hasher);
        for player_id in own_roster {
            player_id.hash(&mut hasher);
        }

        opponent_rosters.len().hash(&mut hasher);
        for roster in opponent_rosters {
            roster.len().hash(&mut hasher);
            for player_id in roster {
                player_id.hash(&mut hasher);
            }
        }

        candidates.len().hash(&mut hasher);
        for player_id in candidates {
            player_id.hash(&mut hasher);
        }

        hasher.finish()
    }

    fn dynamic_cache_path(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Option<PathBuf> {
        if self.cache_dir.as_os_str().is_empty() {
            return None;
        }

        let key = self.dynamic_cache_key(own_roster, opponent_rosters, candidates);
        Some(
            self.cache_dir
                .join("durant")
                .join(format!("dynamic_v{DYNAMIC_SEARCH_VERSION}_{key:016x}.json")),
        )
    }

    fn load_dynamic_cache(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Option<Vec<DynamicDurantScore>> {
        let path = self.dynamic_cache_path(own_roster, opponent_rosters, candidates)?;
        let json = fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    fn save_dynamic_cache(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
        scores: &[DynamicDurantScore],
    ) {
        let Some(path) = self.dynamic_cache_path(own_roster, opponent_rosters, candidates) else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };

        if fs::create_dir_all(parent).is_err() {
            return;
        }

        let Ok(json) = serde_json::to_string(scores) else {
            return;
        };

        let _ = fs::write(path, json);
    }

    fn overnight_dynamic_cache_key(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> u64 {
        let mut hasher = DefaultHasher::new();
        OVERNIGHT_DYNAMIC_SEARCH_VERSION.hash(&mut hasher);
        self.model_fingerprint.hash(&mut hasher);
        self.reference_size.hash(&mut hasher);
        self.team_size.hash(&mut hasher);
        OVERNIGHT_STRATEGY_COUNT.hash(&mut hasher);

        own_roster.len().hash(&mut hasher);
        for player_id in own_roster {
            player_id.hash(&mut hasher);
        }

        opponent_rosters.len().hash(&mut hasher);
        for roster in opponent_rosters {
            roster.len().hash(&mut hasher);
            for player_id in roster {
                player_id.hash(&mut hasher);
            }
        }

        candidates.len().hash(&mut hasher);
        for player_id in candidates {
            player_id.hash(&mut hasher);
        }

        hasher.finish()
    }

    fn overnight_dynamic_cache_path(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Option<PathBuf> {
        if self.cache_dir.as_os_str().is_empty() {
            return None;
        }

        let key = self.overnight_dynamic_cache_key(own_roster, opponent_rosters, candidates);

        Some(self.cache_dir.join("durant").join(format!(
            "overnight_dynamic_v{OVERNIGHT_DYNAMIC_SEARCH_VERSION}_{key:016x}.json"
        )))
    }

    fn load_overnight_dynamic_cache(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Option<Vec<DynamicDurantScore>> {
        let path = self.overnight_dynamic_cache_path(own_roster, opponent_rosters, candidates)?;
        let json = fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    fn save_overnight_dynamic_cache(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
        scores: &[DynamicDurantScore],
    ) {
        let Some(path) =
            self.overnight_dynamic_cache_path(own_roster, opponent_rosters, candidates)
        else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };

        if fs::create_dir_all(parent).is_err() {
            return;
        }

        let Ok(json) = serde_json::to_string(scores) else {
            return;
        };

        let _ = fs::write(path, json);
    }

    fn deep_dynamic_cache_key(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> u64 {
        let mut hasher = DefaultHasher::new();
        DEEP_DYNAMIC_SEARCH_VERSION.hash(&mut hasher);
        self.model_fingerprint.hash(&mut hasher);
        self.reference_size.hash(&mut hasher);
        self.team_size.hash(&mut hasher);
        DEEP_STRATEGY_COUNT.hash(&mut hasher);

        own_roster.len().hash(&mut hasher);
        for player_id in own_roster {
            player_id.hash(&mut hasher);
        }

        opponent_rosters.len().hash(&mut hasher);
        for roster in opponent_rosters {
            roster.len().hash(&mut hasher);
            for player_id in roster {
                player_id.hash(&mut hasher);
            }
        }

        candidates.len().hash(&mut hasher);
        for player_id in candidates {
            player_id.hash(&mut hasher);
        }

        hasher.finish()
    }

    fn deep_dynamic_cache_path(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Option<PathBuf> {
        if self.cache_dir.as_os_str().is_empty() {
            return None;
        }

        let key = self.deep_dynamic_cache_key(own_roster, opponent_rosters, candidates);

        Some(self.cache_dir.join("durant").join(format!(
            "deep_dynamic_v{DEEP_DYNAMIC_SEARCH_VERSION}_{key:016x}.json"
        )))
    }

    fn load_deep_dynamic_cache(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Option<Vec<DynamicDurantScore>> {
        let path = self.deep_dynamic_cache_path(own_roster, opponent_rosters, candidates)?;
        let json = fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    fn save_deep_dynamic_cache(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
        scores: &[DynamicDurantScore],
    ) {
        let Some(path) = self.deep_dynamic_cache_path(own_roster, opponent_rosters, candidates)
        else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };

        if fs::create_dir_all(parent).is_err() {
            return;
        }

        let Ok(json) = serde_json::to_string(scores) else {
            return;
        };

        let _ = fs::write(path, json);
    }

    fn auction_j_census_cache_key(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
    ) -> u64 {
        let mut hasher = DefaultHasher::new();
        AUCTION_J_CENSUS_VERSION.hash(&mut hasher);
        self.model_fingerprint.hash(&mut hasher);
        self.reference_size.hash(&mut hasher);
        self.team_size.hash(&mut hasher);
        DEEP_STRATEGY_COUNT.hash(&mut hasher);
        own_budget_remaining.hash(&mut hasher);

        own_roster.len().hash(&mut hasher);
        for player_id in own_roster {
            player_id.hash(&mut hasher);
        }

        opponent_rosters.len().hash(&mut hasher);
        for roster in opponent_rosters {
            roster.len().hash(&mut hasher);
            for player_id in roster {
                player_id.hash(&mut hasher);
            }
        }

        opponent_budgets_remaining.len().hash(&mut hasher);
        for budget in opponent_budgets_remaining {
            budget.hash(&mut hasher);
        }

        candidates.len().hash(&mut hasher);
        for player_id in candidates {
            player_id.hash(&mut hasher);
        }

        hasher.finish()
    }

    fn auction_j_census_cache_path(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
    ) -> Option<PathBuf> {
        if self.cache_dir.as_os_str().is_empty() {
            return None;
        }
        let key = self.auction_j_census_cache_key(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
        );
        Some(self.cache_dir.join("durant").join(format!(
            "auction_j_census_v{AUCTION_J_CENSUS_VERSION}_{key:016x}.json"
        )))
    }

    fn load_auction_j_census_cache(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
    ) -> Option<Vec<DynamicDurantScore>> {
        let path = self.auction_j_census_cache_path(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
        )?;
        let json = fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    fn save_auction_j_census_cache(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        scores: &[DynamicDurantScore],
    ) {
        let Some(path) = self.auction_j_census_cache_path(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
        ) else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(json) = serde_json::to_string(scores) else {
            return;
        };
        let _ = fs::write(path, json);
    }

    /// Rank candidates by a roster-dependent Most-Categories H-score and a
    /// deterministic future-roster rollout.
    ///
    /// For every candidate, BirdBoard:
    /// 1. adds the candidate to the known roster,
    /// 2. tries a library of interpretable category-weight vectors j,
    /// 3. greedily fills the remaining roster slots from the available pool,
    /// 4. evaluates the completed roster against the current/generic opponents,
    /// 5. keeps the j with the highest projected matchup-win probability.
    ///

    /// Number of future-strategy j vectors in the normal research search.
    pub fn strategy_count(&self) -> usize {
        rollout_strategies().len()
    }

    /// Number of j vectors in the deliberately larger overnight search.
    pub fn overnight_strategy_count(&self) -> usize {
        OVERNIGHT_STRATEGY_COUNT
    }

    pub fn deep_strategy_count(&self) -> usize {
        DEEP_STRATEGY_COUNT
    }

    /// Standard 2,620-j dynamic search used by the normal research/debug path.
    pub fn dynamic_scores(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Vec<DynamicDurantScore> {
        if let Some(cached) = self.load_dynamic_cache(own_roster, opponent_rosters, candidates) {
            return cached;
        }

        let strategies = rollout_strategies();
        let results = self.compute_dynamic_scores_with_strategies(
            own_roster,
            opponent_rosters,
            candidates,
            &strategies,
        );

        self.save_dynamic_cache(own_roster, opponent_rosters, candidates, &results);
        results
    }

    /// Large offline search intended for the overnight strategy census.
    ///
    /// It uses a separate cache namespace, so normal BirdBoard runs continue
    /// using the small 2,620-j research search and never pay this cost.
    pub fn dynamic_scores_overnight(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Vec<DynamicDurantScore> {
        if let Some(cached) =
            self.load_overnight_dynamic_cache(own_roster, opponent_rosters, candidates)
        {
            return cached;
        }

        let strategies = overnight_rollout_strategies();
        let results = self.compute_dynamic_scores_with_strategies(
            own_roster,
            opponent_rosters,
            candidates,
            &strategies,
        );

        self.save_overnight_dynamic_cache(own_roster, opponent_rosters, candidates, &results);
        results
    }

    pub fn dynamic_scores_deep(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Vec<DynamicDurantScore> {
        if let Some(cached) = self.load_deep_dynamic_cache(own_roster, opponent_rosters, candidates)
        {
            return cached;
        }

        let strategies = deep_rollout_strategies();
        let results = self.compute_dynamic_scores_with_strategies(
            own_roster,
            opponent_rosters,
            candidates,
            &strategies,
        );

        self.save_deep_dynamic_cache(own_roster, opponent_rosters, candidates, &results);
        results
    }

    /// Full offline j search under the current auction model.
    ///
    /// For every candidate this buys the player at the current generic market
    /// price, fills the remaining roster under the real remaining budget, keeps
    /// the $1 reserve for later slots, and evaluates the best three j vectors.
    /// The rollout uses current auction semantics: there is no draft-turn stride.
    pub fn auction_dynamic_scores_deep(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
    ) -> Vec<DynamicDurantScore> {
        if let Some(cached) = self.load_auction_j_census_cache(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
        ) {
            return cached;
        }

        let strategies = deep_rollout_strategies();
        let config = AuctionConfig::default();
        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        if own_open_slots == 0 || strategies.is_empty() {
            return Vec::new();
        }

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let economy =
            self.estimate_market_economy_from_totals(candidates, None, market_totals, config);
        let max_bid = max_legal_bid(own_budget_remaining, own_open_slots, config.minimum_bid);

        // Precompute the candidate-specific auction state ONCE. The old v21
        // kernel built one enormous 212,941 x N ranking matrix and then did
        // millions of temporary allocations inside the candidate loop. That
        // was both RAM-heavy and catastrophically cache-unfriendly.
        let contexts = candidates
            .par_iter()
            .filter_map(|candidate| {
                if own_roster.iter().any(|player_id| player_id == candidate) {
                    return None;
                }

                let static_score = self.score_for(candidate)?;
                let market_price = economy
                    .prices
                    .get(candidate)
                    .copied()
                    .unwrap_or(config.minimum_bid);
                if market_price < config.minimum_bid || market_price > max_bid {
                    return None;
                }

                let candidate_x = score_to_x_vector(static_score, self.parameters);
                let with_candidate = add_vectors(own_sum, candidate_x);
                let immediate = self.evaluate_dynamic_state(with_candidate, &opponent_sums);
                let budget_after = own_budget_remaining.saturating_sub(market_price);
                let future_slots = own_open_slots.saturating_sub(1);

                let post_purchase_totals = AuctionMarketTotals {
                    remaining_slots: market_totals.remaining_slots.saturating_sub(1),
                    remaining_dollars: market_totals
                        .remaining_dollars
                        .saturating_sub(market_price as u32),
                    max_legal_market_bid: market_totals.max_legal_market_bid,
                };
                let post_purchase_economy = self.estimate_market_economy_from_totals(
                    candidates,
                    Some(candidate),
                    post_purchase_totals,
                    config,
                );

                Some(AuctionCensusCandidate {
                    player_id: candidate.clone(),
                    player_name: static_score.player_name.clone(),
                    x_scores: candidate_x,
                    immediate,
                    with_candidate,
                    budget_after,
                    future_slots,
                    economy: post_purchase_economy,
                })
            })
            .collect::<Vec<_>>();

        // Keep only the top three rollouts for each candidate while streaming
        // the 212,941 strategies in modest chunks. Peak ranking memory is now
        // O(CHUNK_SIZE * players), not O(all_strategies * players).
        let mut best_by_candidate = vec![Vec::<AuctionRolloutResult>::new(); contexts.len()];

        for strategy_chunk in strategies.chunks(AUCTION_J_CHUNK_SIZE) {
            let strategy_rankings = self.rank_available_by_strategy(candidates, strategy_chunk);

            let chunk_best = contexts
                .par_iter()
                .map(|context| {
                    self.top_auction_rollouts(
                        Some(&context.player_id),
                        context.with_candidate,
                        context.future_slots,
                        context.budget_after,
                        strategy_chunk,
                        &strategy_rankings,
                        &context.economy,
                        &opponent_sums,
                        config,
                        3,
                    )
                })
                .collect::<Vec<_>>();

            for (global_best, local_best) in
                best_by_candidate.iter_mut().zip(chunk_best.into_iter())
            {
                global_best.extend(local_best);
                global_best.sort_by(|a, b| {
                    b.evaluation
                        .matchup_win_probability
                        .total_cmp(&a.evaluation.matchup_win_probability)
                        .then_with(|| {
                            b.evaluation
                                .expected_categories
                                .total_cmp(&a.evaluation.expected_categories)
                        })
                        .then_with(|| a.future_spend.cmp(&b.future_spend))
                });
                global_best.truncate(3);
            }
        }

        let mut scores = contexts
            .into_iter()
            .zip(best_by_candidate.into_iter())
            .filter_map(|(context, mut rollouts)| {
                if rollouts.is_empty() {
                    return None;
                }

                let best = rollouts.remove(0);
                let second_probability = rollouts
                    .first()
                    .map(|result| result.evaluation.matchup_win_probability)
                    .unwrap_or(best.evaluation.matchup_win_probability);

                let alternatives = rollouts
                    .iter()
                    .map(|result| StrategyAlternative {
                        j_name: describe_strategy(
                            &result.strategy,
                            result.evaluation.category_win_probabilities,
                        ),
                        j_weights: result.strategy.weights,
                        projected_matchup_win_probability: result
                            .evaluation
                            .matchup_win_probability,
                        projected_expected_categories: result.evaluation.expected_categories,
                        build_name: describe_projected_build(
                            result.evaluation.category_win_probabilities,
                        ),
                    })
                    .collect::<Vec<_>>();

                Some(DynamicDurantScore {
                    player_id: context.player_id,
                    player_name: context.player_name,
                    x_scores: context.x_scores,
                    category_win_probabilities: context.immediate.category_win_probabilities,
                    expected_categories: context.immediate.expected_categories,
                    matchup_win_probability: context.immediate.matchup_win_probability,
                    marginal_matchup_win_probability: context.immediate.matchup_win_probability
                        - current.matchup_win_probability,
                    projected_matchup_win_probability: best.evaluation.matchup_win_probability,
                    projected_expected_categories: best.evaluation.expected_categories,
                    projected_category_win_probabilities: best
                        .evaluation
                        .category_win_probabilities,
                    build_name: describe_projected_build(
                        best.evaluation.category_win_probabilities,
                    ),
                    j_name: describe_strategy(
                        &best.strategy,
                        best.evaluation.category_win_probabilities,
                    ),
                    j_weights: best.strategy.weights,
                    j_alternatives: alternatives,
                    j_margin: best.evaluation.matchup_win_probability - second_probability,
                    projected_future_players: best.future_players,
                    projected_future_player_names: best.future_player_names,
                })
            })
            .collect::<Vec<_>>();

        scores.sort_by(|a, b| {
            b.projected_matchup_win_probability
                .total_cmp(&a.projected_matchup_win_probability)
                .then_with(|| {
                    b.projected_expected_categories
                        .total_cmp(&a.projected_expected_categories)
                })
        });

        self.save_auction_j_census_cache(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            &scores,
        );
        scores
    }

    /// Small, memory-bounded auction-aware j search for fast model checks.
    ///
    /// This deliberately uses the standard 2,620-vector j grid and may evaluate
    /// only a shortlist of candidate decisions, while `available_candidates`
    /// remains the full future player pool.  It has the same no-stride,
    /// budget-constrained semantics as the overnight auction search.
    pub fn auction_dynamic_scores_quick(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        available_candidates: &[PlayerId],
        evaluated_candidates: &[PlayerId],
    ) -> Vec<DynamicDurantScore> {
        let strategies = rollout_strategies();
        let config = AuctionConfig::default();
        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        if own_open_slots == 0 || strategies.is_empty() || evaluated_candidates.is_empty() {
            return Vec::new();
        }

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let economy = self.estimate_market_economy_from_totals(
            available_candidates,
            None,
            market_totals,
            config,
        );
        let max_bid = max_legal_bid(own_budget_remaining, own_open_slots, config.minimum_bid);

        // Only the requested shortlist gets a candidate-specific market state.
        // The full available pool is still used to rank and fill future slots.
        let contexts = evaluated_candidates
            .iter()
            .filter_map(|candidate| {
                if own_roster.iter().any(|player_id| player_id == candidate) {
                    return None;
                }
                let static_score = self.score_for(candidate)?;
                let market_price = economy
                    .prices
                    .get(candidate)
                    .copied()
                    .unwrap_or(config.minimum_bid);
                if market_price < config.minimum_bid || market_price > max_bid {
                    return None;
                }

                let candidate_x = score_to_x_vector(static_score, self.parameters);
                let with_candidate = add_vectors(own_sum, candidate_x);
                let immediate = self.evaluate_dynamic_state(with_candidate, &opponent_sums);
                let budget_after = own_budget_remaining.saturating_sub(market_price);
                let future_slots = own_open_slots.saturating_sub(1);
                let post_purchase_totals = AuctionMarketTotals {
                    remaining_slots: market_totals.remaining_slots.saturating_sub(1),
                    remaining_dollars: market_totals
                        .remaining_dollars
                        .saturating_sub(market_price as u32),
                    max_legal_market_bid: market_totals.max_legal_market_bid,
                };
                let post_purchase_economy = self.estimate_market_economy_from_totals(
                    available_candidates,
                    Some(candidate),
                    post_purchase_totals,
                    config,
                );

                Some(AuctionCensusCandidate {
                    player_id: candidate.clone(),
                    player_name: static_score.player_name.clone(),
                    x_scores: candidate_x,
                    immediate,
                    with_candidate,
                    budget_after,
                    future_slots,
                    economy: post_purchase_economy,
                })
            })
            .collect::<Vec<_>>();

        // 2,620 x ~244 is small enough to materialize once, and this is far
        // cheaper than repeatedly sorting the player pool for every decision.
        let strategy_rankings = self.rank_available_by_strategy(available_candidates, &strategies);

        let mut scores = contexts
            .par_iter()
            .filter_map(|context| {
                let mut best = Vec::<AuctionRolloutResult>::with_capacity(4);

                for (strategy, ranked_players) in strategies.iter().zip(strategy_rankings.iter()) {
                    let result = self.auction_rollout_summary(
                        Some(&context.player_id),
                        context.with_candidate,
                        context.future_slots,
                        context.budget_after,
                        strategy,
                        ranked_players,
                        &context.economy,
                        &opponent_sums,
                        config,
                    )?;

                    best.push(result);
                    best.sort_by(|a, b| {
                        b.evaluation
                            .matchup_win_probability
                            .total_cmp(&a.evaluation.matchup_win_probability)
                            .then_with(|| {
                                b.evaluation
                                    .expected_categories
                                    .total_cmp(&a.evaluation.expected_categories)
                            })
                            .then_with(|| a.future_spend.cmp(&b.future_spend))
                    });
                    best.truncate(3);
                }

                if best.is_empty() {
                    return None;
                }
                let winner = best.remove(0);
                let second_probability = best
                    .first()
                    .map(|r| r.evaluation.matchup_win_probability)
                    .unwrap_or(winner.evaluation.matchup_win_probability);

                let alternatives = best
                    .iter()
                    .map(|r| StrategyAlternative {
                        j_name: describe_strategy(
                            &r.strategy,
                            r.evaluation.category_win_probabilities,
                        ),
                        j_weights: r.strategy.weights,
                        projected_matchup_win_probability: r.evaluation.matchup_win_probability,
                        projected_expected_categories: r.evaluation.expected_categories,
                        build_name: describe_projected_build(
                            r.evaluation.category_win_probabilities,
                        ),
                    })
                    .collect::<Vec<_>>();

                Some(DynamicDurantScore {
                    player_id: context.player_id.clone(),
                    player_name: context.player_name.clone(),
                    x_scores: context.x_scores,
                    category_win_probabilities: context.immediate.category_win_probabilities,
                    expected_categories: context.immediate.expected_categories,
                    matchup_win_probability: context.immediate.matchup_win_probability,
                    marginal_matchup_win_probability: context.immediate.matchup_win_probability
                        - current.matchup_win_probability,
                    projected_matchup_win_probability: winner.evaluation.matchup_win_probability,
                    projected_expected_categories: winner.evaluation.expected_categories,
                    projected_category_win_probabilities: winner
                        .evaluation
                        .category_win_probabilities,
                    build_name: describe_projected_build(
                        winner.evaluation.category_win_probabilities,
                    ),
                    j_name: describe_strategy(
                        &winner.strategy,
                        winner.evaluation.category_win_probabilities,
                    ),
                    j_weights: winner.strategy.weights,
                    j_alternatives: alternatives,
                    j_margin: winner.evaluation.matchup_win_probability - second_probability,
                    // The quick census is for learning the j vocabulary, not
                    // plan presentation, so avoid allocating millions of path
                    // strings while searching.
                    projected_future_players: Vec::new(),
                    projected_future_player_names: Vec::new(),
                })
            })
            .collect::<Vec<_>>();

        scores.sort_by(|a, b| {
            b.projected_matchup_win_probability
                .total_cmp(&a.projected_matchup_win_probability)
                .then_with(|| {
                    b.projected_expected_categories
                        .total_cmp(&a.projected_expected_categories)
                })
        });

        scores
    }

    /// Medium-sized auction-aware j search for model evaluation.
    ///
    /// Uses the 30,178-vector overnight strategy space, but streams it in
    /// 4,096-vector chunks so ranking memory stays bounded. Only the requested
    /// candidate shortlist is scored; every rollout still has access to the
    /// complete remaining player pool.
    pub fn auction_dynamic_scores_medium(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        available_candidates: &[PlayerId],
        evaluated_candidates: &[PlayerId],
    ) -> Vec<DynamicDurantScore> {
        let strategies = overnight_rollout_strategies();
        let config = AuctionConfig::default();
        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        if own_open_slots == 0 || strategies.is_empty() || evaluated_candidates.is_empty() {
            return Vec::new();
        }

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let economy = self.estimate_market_economy_from_totals(
            available_candidates,
            None,
            market_totals,
            config,
        );
        let max_bid = max_legal_bid(own_budget_remaining, own_open_slots, config.minimum_bid);

        // Candidate-specific auction state is independent of j, so build it
        // once before streaming the larger strategy library.
        let contexts = evaluated_candidates
            .iter()
            .filter_map(|candidate| {
                if own_roster.iter().any(|player_id| player_id == candidate) {
                    return None;
                }

                let static_score = self.score_for(candidate)?;
                let market_price = economy
                    .prices
                    .get(candidate)
                    .copied()
                    .unwrap_or(config.minimum_bid);
                if market_price < config.minimum_bid || market_price > max_bid {
                    return None;
                }

                let candidate_x = score_to_x_vector(static_score, self.parameters);
                let with_candidate = add_vectors(own_sum, candidate_x);
                let immediate = self.evaluate_dynamic_state(with_candidate, &opponent_sums);
                let budget_after = own_budget_remaining.saturating_sub(market_price);
                let future_slots = own_open_slots.saturating_sub(1);
                let post_purchase_totals = AuctionMarketTotals {
                    remaining_slots: market_totals.remaining_slots.saturating_sub(1),
                    remaining_dollars: market_totals
                        .remaining_dollars
                        .saturating_sub(market_price as u32),
                    max_legal_market_bid: market_totals.max_legal_market_bid,
                };
                let post_purchase_economy = self.estimate_market_economy_from_totals(
                    available_candidates,
                    Some(candidate),
                    post_purchase_totals,
                    config,
                );

                Some(AuctionCensusCandidate {
                    player_id: candidate.clone(),
                    player_name: static_score.player_name.clone(),
                    x_scores: candidate_x,
                    immediate,
                    with_candidate,
                    budget_after,
                    future_slots,
                    economy: post_purchase_economy,
                })
            })
            .collect::<Vec<_>>();

        let mut best_by_candidate = vec![Vec::<AuctionRolloutResult>::new(); contexts.len()];

        for strategy_chunk in strategies.chunks(AUCTION_J_CHUNK_SIZE) {
            let strategy_rankings =
                self.rank_available_by_strategy(available_candidates, strategy_chunk);

            let chunk_best = contexts
                .par_iter()
                .map(|context| {
                    self.top_auction_rollouts(
                        Some(&context.player_id),
                        context.with_candidate,
                        context.future_slots,
                        context.budget_after,
                        strategy_chunk,
                        &strategy_rankings,
                        &context.economy,
                        &opponent_sums,
                        config,
                        3,
                    )
                })
                .collect::<Vec<_>>();

            for (global_best, local_best) in
                best_by_candidate.iter_mut().zip(chunk_best.into_iter())
            {
                global_best.extend(local_best);
                global_best.sort_by(|a, b| {
                    b.evaluation
                        .matchup_win_probability
                        .total_cmp(&a.evaluation.matchup_win_probability)
                        .then_with(|| {
                            b.evaluation
                                .expected_categories
                                .total_cmp(&a.evaluation.expected_categories)
                        })
                        .then_with(|| a.future_spend.cmp(&b.future_spend))
                });
                global_best.truncate(3);
            }
        }

        let mut scores = contexts
            .into_iter()
            .zip(best_by_candidate.into_iter())
            .filter_map(|(context, mut rollouts)| {
                if rollouts.is_empty() {
                    return None;
                }

                let best = rollouts.remove(0);
                let second_probability = rollouts
                    .first()
                    .map(|result| result.evaluation.matchup_win_probability)
                    .unwrap_or(best.evaluation.matchup_win_probability);

                let alternatives = rollouts
                    .iter()
                    .map(|result| StrategyAlternative {
                        j_name: describe_strategy(
                            &result.strategy,
                            result.evaluation.category_win_probabilities,
                        ),
                        j_weights: result.strategy.weights,
                        projected_matchup_win_probability: result
                            .evaluation
                            .matchup_win_probability,
                        projected_expected_categories: result.evaluation.expected_categories,
                        build_name: describe_projected_build(
                            result.evaluation.category_win_probabilities,
                        ),
                    })
                    .collect::<Vec<_>>();

                Some(DynamicDurantScore {
                    player_id: context.player_id,
                    player_name: context.player_name,
                    x_scores: context.x_scores,
                    category_win_probabilities: context.immediate.category_win_probabilities,
                    expected_categories: context.immediate.expected_categories,
                    matchup_win_probability: context.immediate.matchup_win_probability,
                    marginal_matchup_win_probability: context.immediate.matchup_win_probability
                        - current.matchup_win_probability,
                    projected_matchup_win_probability: best.evaluation.matchup_win_probability,
                    projected_expected_categories: best.evaluation.expected_categories,
                    projected_category_win_probabilities: best
                        .evaluation
                        .category_win_probabilities,
                    build_name: describe_projected_build(
                        best.evaluation.category_win_probabilities,
                    ),
                    j_name: describe_strategy(
                        &best.strategy,
                        best.evaluation.category_win_probabilities,
                    ),
                    j_weights: best.strategy.weights,
                    j_alternatives: alternatives,
                    j_margin: best.evaluation.matchup_win_probability - second_probability,
                    // Research bank extraction only needs the winning j and H.
                    projected_future_players: Vec::new(),
                    projected_future_player_names: Vec::new(),
                })
            })
            .collect::<Vec<_>>();

        scores.sort_by(|a, b| {
            b.projected_matchup_win_probability
                .total_cmp(&a.projected_matchup_win_probability)
                .then_with(|| {
                    b.projected_expected_categories
                        .total_cmp(&a.projected_expected_categories)
                })
        });

        scores
    }

    /// Score the same dynamic H-rollout problem using an externally supplied
    /// strategy bank. This is the bridge from the offline census to live
    /// BirdBoard: the bank can contain ~100-300 learned j vectors instead of
    /// re-searching the 212,941-vector research space.
    pub fn dynamic_scores_with_strategy_weights(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
        strategy_weights: &[[f64; 9]],
    ) -> Vec<DynamicDurantScore> {
        let strategies = strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();

        self.compute_dynamic_scores_with_strategies(
            own_roster,
            opponent_rosters,
            candidates,
            &strategies,
        )
    }

    /// Runtime team-building board.
    ///
    /// `available_candidates` is the full future pool used to complete rosters.
    /// `evaluated_candidates` is the (usually top-200 + curated) visible board.
    ///
    /// Unlike auction ΔH this does not price the candidate. It answers the same
    /// question as the original Dynamic DURANT debug board:
    ///
    ///     "If this player is on my roster, how good a completed team can I build?"
    pub fn dynamic_scores_for_candidates_with_strategy_weights(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        available_candidates: &[PlayerId],
        evaluated_candidates: &[PlayerId],
        strategy_weights: &[[f64; 9]],
    ) -> Vec<DynamicDurantScore> {
        let strategies = strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();

        self.compute_dynamic_scores_for_candidates_with_strategies(
            own_roster,
            opponent_rosters,
            available_candidates,
            evaluated_candidates,
            &strategies,
        )
    }

    /// Complete the current roster without forcing a candidate and without
    /// auction prices. This gives the baseline H used for the live board's
    /// post-pick ΔH:
    ///
    ///     ΔH(player) = H(best completion with player) - H(best completion if we pass)
    ///
    /// Future targets are chosen directly from the strategy ranking; auctions
    /// have no turn-order stride.
    pub fn team_building_plan_with_strategy_weights(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        available_candidates: &[PlayerId],
        strategy_weights: &[[f64; 9]],
    ) -> Option<DurantRosterPlan> {
        if own_roster.len() > self.team_size || strategy_weights.is_empty() {
            return None;
        }

        let strategies = strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();
        let strategy_rankings = self.rank_available_by_strategy(available_candidates, &strategies);

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let future_slots = self.team_size.saturating_sub(own_roster.len());

        let mut rollouts = self.top_rollouts_from_state(
            own_sum,
            future_slots,
            &strategies,
            &strategy_rankings,
            &opponent_sums,
            3,
        );
        if rollouts.is_empty() {
            return None;
        }

        let best = rollouts.remove(0);
        let second_probability = rollouts
            .first()
            .map(|result| result.evaluation.matchup_win_probability)
            .unwrap_or(best.evaluation.matchup_win_probability);

        let alternatives = rollouts
            .iter()
            .map(|result| StrategyAlternative {
                j_name: describe_strategy(
                    &result.strategy,
                    result.evaluation.category_win_probabilities,
                ),
                j_weights: result.strategy.weights,
                projected_matchup_win_probability: result.evaluation.matchup_win_probability,
                projected_expected_categories: result.evaluation.expected_categories,
                build_name: describe_projected_build(result.evaluation.category_win_probabilities),
            })
            .collect::<Vec<_>>();

        Some(DurantRosterPlan {
            current_matchup_win_probability: current.matchup_win_probability,
            current_expected_categories: current.expected_categories,
            current_category_win_probabilities: current.category_win_probabilities,
            projected_matchup_win_probability: best.evaluation.matchup_win_probability,
            projected_expected_categories: best.evaluation.expected_categories,
            projected_category_win_probabilities: best.evaluation.category_win_probabilities,
            build_name: describe_projected_build(best.evaluation.category_win_probabilities),
            j_name: describe_strategy(&best.strategy, best.evaluation.category_win_probabilities),
            j_weights: best.strategy.weights,
            j_alternatives: alternatives,
            j_margin: best.evaluation.matchup_win_probability - second_probability,
            projected_future_players: best.future_players,
            projected_future_player_names: best.future_player_names,
            projected_future_spend: 0,
            projected_budget_left: 0,
        })
    }

    /// Cheap stage-selection helper used by the adaptive MID strategy bank.
    ///
    /// It ranks the supplied global vocabulary under the same static auction
    /// economy used by Strategy's coarse pass and returns the best current
    /// directions. No opponent H-indifference prices are solved here.
    pub fn coarse_strategy_seed_weights(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
        keep: usize,
    ) -> Vec<[f64; 9]> {
        if own_roster.len() > self.team_size || strategy_weights.is_empty() || keep == 0 {
            return Vec::new();
        }

        let strategies = strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();
        let strategy_rankings = self.rank_available_by_strategy(candidates, &strategies);

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();
        let own_sum = self.aggregate_x_scores(own_roster);
        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let static_economy =
            self.estimate_market_economy_from_totals(candidates, None, market_totals, config);

        self.top_intentional_auction_rollouts(
            own_roster.len(),
            own_sum,
            own_open_slots,
            own_budget_remaining,
            &strategies,
            &strategy_rankings,
            &static_economy,
            &opponent_sums,
            config,
            keep,
        )
        .into_iter()
        .map(|result| result.strategy.weights)
        .collect()
    }

    /// Complete the current roster under an externally supplied strategy bank.
    ///
    /// Live Strategy uses a two-stage search:
    ///
    /// 1. Search the ENTIRE supplied j bank against the cheap static G/$ market.
    ///    This identifies a small set of genuinely promising team builds without
    ///    first solving opponent-specific bids for the whole undrafted pool.
    /// 2. Collect only the future players targeted by the top coarse builds,
    ///    solve rational opponent prices for that focused set, then re-evaluate
    ///    and re-rank those same finalist strategies.
    ///
    /// The opponent bid solver still sees the full remaining player pool when
    /// completing a roster. Only the set of players whose clearing price is
    /// solved exactly is restricted. This keeps Strategy responsive while
    /// retaining opponent-aware prices where they can actually change the plan.
    pub fn roster_plan_with_strategy_weights(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
    ) -> Option<DurantRosterPlan> {
        self.roster_plan_with_search_and_pricing_weights(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            strategy_weights,
            strategy_weights,
            config,
        )
    }

    /// Validation-only bridge for the v30 Strategy environment.
    ///
    /// `search_strategy_weights` controls the j vocabulary being tested while
    /// `pricing_strategy_weights` is held fixed. This lets the validation ask
    /// only whether the runtime j bank approximates the deep 212,941-j search,
    /// without confounding that answer with a different opponent-pricing core.
    pub(crate) fn roster_plan_validation_with_strategy_weights(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        search_strategy_weights: &[[f64; 9]],
        pricing_strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
    ) -> Option<DurantRosterPlan> {
        self.roster_plan_with_search_and_pricing_weights(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            search_strategy_weights,
            pricing_strategy_weights,
            config,
        )
    }

    fn roster_plan_with_search_and_pricing_weights(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        search_strategy_weights: &[[f64; 9]],
        pricing_strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
    ) -> Option<DurantRosterPlan> {
        if own_roster.len() > self.team_size || search_strategy_weights.is_empty() {
            return None;
        }

        let strategies = search_strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();
        let strategy_rankings = self.rank_available_by_strategy(candidates, &strategies);

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let own_open_slots = self.team_size.saturating_sub(own_roster.len());

        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );

        // Stage 1: cheap global search. The whole supplied j search space and
        // available player pool participate, but no opponent H-indifference
        // price is solved yet.
        let static_economy =
            self.estimate_market_economy_from_totals(candidates, None, market_totals, config);
        let coarse = self.top_intentional_auction_rollouts(
            own_roster.len(),
            own_sum,
            own_open_slots,
            own_budget_remaining,
            &strategies,
            &strategy_rankings,
            &static_economy,
            &opponent_sums,
            config,
            3,
        );
        if coarse.is_empty() {
            return None;
        }

        // Runtime passes the same bank for search and pricing. Reuse the
        // already-built rankings in that normal path so v31 does not add any
        // live Strategy work merely to support offline validation.
        if std::ptr::eq(search_strategy_weights, pricing_strategy_weights) {
            return self.refine_strategy_plan_from_coarse(
                current,
                own_roster.len(),
                own_sum,
                own_open_slots,
                own_budget_remaining,
                opponent_rosters,
                opponent_budgets_remaining,
                candidates,
                market_totals,
                coarse,
                &strategies,
                &strategy_rankings,
                &opponent_sums,
                config,
            );
        }

        let pricing_strategies = pricing_strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();
        let pricing_rankings = self.rank_available_by_strategy(candidates, &pricing_strategies);

        self.refine_strategy_plan_from_coarse(
            current,
            own_roster.len(),
            own_sum,
            own_open_slots,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            market_totals,
            coarse,
            &pricing_strategies,
            &pricing_rankings,
            &opponent_sums,
            config,
        )
    }

    /// Validation oracle for the current auction environment.
    ///
    /// Builds ONE full opponent-aware rational market for every remaining
    /// player using the runtime pricing bank, then searches both:
    ///
    ///   A. all 212,941 deep j vectors
    ///   B. the supplied runtime j bank
    ///
    /// against that exact same fixed market.  This restores the superset
    /// property needed for a meaningful bank-regret test: the pricing
    /// environment no longer depends on which three strategies happened to
    /// survive a preceding coarse shortlist.
    pub(crate) fn roster_plan_validation_common_market_pair(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        bank_strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
    ) -> Option<(DurantRosterPlan, DurantRosterPlan)> {
        self.roster_plan_validation_common_market_pair_with_pricing(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            bank_strategy_weights,
            bank_strategy_weights,
            config,
        )
    }

    /// Same common-market oracle, but decouples the SEARCH vocabulary from the
    /// compact opponent-PRICING vocabulary.  Stage-aware validation uses this
    /// so a 1,000-vector EARLY/MID search can be tested without also changing
    /// the opponent clearing-price model.
    pub(crate) fn roster_plan_validation_common_market_pair_with_pricing(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        search_strategy_weights: &[[f64; 9]],
        pricing_strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
    ) -> Option<(DurantRosterPlan, DurantRosterPlan)> {
        if own_roster.len() > self.team_size
            || search_strategy_weights.is_empty()
            || pricing_strategy_weights.is_empty()
        {
            return None;
        }

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );

        // Pricing is fixed for BOTH search spaces. We pay the expensive
        // all-player rational-market cost once because this is offline work.
        let pricing_strategies = pricing_strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();
        let pricing_rankings = self.rank_available_by_strategy(candidates, &pricing_strategies);
        let common_economy = self.rational_opponent_market_economy(
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            market_totals,
            &pricing_strategies,
            &pricing_rankings,
            config,
        );

        // Restricted/stage-aware search under the common market.
        let search_strategies = search_strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();
        let search_rankings = self.rank_available_by_strategy(candidates, &search_strategies);
        let bank_rollouts = self.top_intentional_auction_rollouts(
            own_roster.len(),
            own_sum,
            own_open_slots,
            own_budget_remaining,
            &search_strategies,
            &search_rankings,
            &common_economy,
            &opponent_sums,
            config,
            3,
        );

        // Full 212,941-j search under THE SAME common market. Stream in chunks
        // so the ranking matrix never becomes enormous.
        let deep_strategies = deep_rollout_strategies();
        let mut deep_rollouts = Vec::<AuctionRolloutResult>::new();
        for strategy_chunk in deep_strategies.chunks(AUCTION_J_CHUNK_SIZE) {
            let rankings = self.rank_available_by_strategy(candidates, strategy_chunk);
            let mut chunk_best = self.top_intentional_auction_rollouts(
                own_roster.len(),
                own_sum,
                own_open_slots,
                own_budget_remaining,
                strategy_chunk,
                &rankings,
                &common_economy,
                &opponent_sums,
                config,
                3,
            );
            deep_rollouts.append(&mut chunk_best);
            sort_and_truncate_auction_rollouts(&mut deep_rollouts, 3);
        }

        let deep_plan = self.validation_plan_from_rollouts(current, deep_rollouts)?;
        let bank_plan = self.validation_plan_from_rollouts(current, bank_rollouts)?;
        Some((deep_plan, bank_plan))
    }

    fn validation_plan_from_rollouts(
        &self,
        current: DynamicStateEvaluation,
        mut rollouts: Vec<AuctionRolloutResult>,
    ) -> Option<DurantRosterPlan> {
        if rollouts.is_empty() {
            return None;
        }
        sort_and_truncate_auction_rollouts(&mut rollouts, 3);
        let best = rollouts.remove(0);
        let second_probability = rollouts
            .first()
            .map(|result| result.evaluation.matchup_win_probability)
            .unwrap_or(best.evaluation.matchup_win_probability);
        let alternatives = rollouts
            .iter()
            .map(|result| StrategyAlternative {
                j_name: describe_strategy(
                    &result.strategy,
                    result.evaluation.category_win_probabilities,
                ),
                j_weights: result.strategy.weights,
                projected_matchup_win_probability: result.evaluation.matchup_win_probability,
                projected_expected_categories: result.evaluation.expected_categories,
                build_name: describe_projected_build(result.evaluation.category_win_probabilities),
            })
            .collect::<Vec<_>>();

        Some(DurantRosterPlan {
            current_matchup_win_probability: current.matchup_win_probability,
            current_expected_categories: current.expected_categories,
            current_category_win_probabilities: current.category_win_probabilities,
            projected_matchup_win_probability: best.evaluation.matchup_win_probability,
            projected_expected_categories: best.evaluation.expected_categories,
            projected_category_win_probabilities: best.evaluation.category_win_probabilities,
            build_name: describe_projected_build(best.evaluation.category_win_probabilities),
            j_name: describe_strategy(&best.strategy, best.evaluation.category_win_probabilities),
            j_weights: best.strategy.weights,
            j_alternatives: alternatives,
            j_margin: best.evaluation.matchup_win_probability - second_probability,
            projected_future_players: best.future_players,
            projected_future_player_names: best.future_player_names,
            projected_future_spend: best.future_spend,
            projected_budget_left: best.budget_left,
        })
    }

    /// Full 212,941-j Strategy search under the CURRENT v30 auction/planning
    /// environment. This is intentionally an offline validation path.
    ///
    /// The deep j search is streamed in chunks so we never materialize a
    /// 212,941 x N player-ranking matrix. Opponent pricing is deliberately held
    /// to the caller-supplied runtime pricing bank, matching the live model and
    /// isolating the approximation error of the j search vocabulary itself.
    pub(crate) fn roster_plan_validation_deep(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        pricing_strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
    ) -> Option<DurantRosterPlan> {
        if own_roster.len() > self.team_size || pricing_strategy_weights.is_empty() {
            return None;
        }

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let static_economy =
            self.estimate_market_economy_from_totals(candidates, None, market_totals, config);

        let deep_strategies = deep_rollout_strategies();
        let mut coarse = Vec::<AuctionRolloutResult>::new();

        for strategy_chunk in deep_strategies.chunks(AUCTION_J_CHUNK_SIZE) {
            let strategy_rankings = self.rank_available_by_strategy(candidates, strategy_chunk);
            let mut chunk_best = self.top_intentional_auction_rollouts(
                own_roster.len(),
                own_sum,
                own_open_slots,
                own_budget_remaining,
                strategy_chunk,
                &strategy_rankings,
                &static_economy,
                &opponent_sums,
                config,
                3,
            );
            coarse.append(&mut chunk_best);
            sort_and_truncate_auction_rollouts(&mut coarse, 3);
        }

        if coarse.is_empty() {
            return None;
        }

        let pricing_strategies = pricing_strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();
        let pricing_rankings = self.rank_available_by_strategy(candidates, &pricing_strategies);

        self.refine_strategy_plan_from_coarse(
            current,
            own_roster.len(),
            own_sum,
            own_open_slots,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            market_totals,
            coarse,
            &pricing_strategies,
            &pricing_rankings,
            &opponent_sums,
            config,
        )
    }

    fn refine_strategy_plan_from_coarse(
        &self,
        current: DynamicStateEvaluation,
        roster_size_at_start: usize,
        own_sum: [f64; 9],
        own_open_slots: usize,
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        market_totals: AuctionMarketTotals,
        coarse: Vec<AuctionRolloutResult>,
        pricing_strategies: &[RolloutStrategy],
        pricing_rankings: &[Vec<PlayerId>],
        opponent_sums: &[([f64; 9], usize)],
        config: AuctionConfig,
    ) -> Option<DurantRosterPlan> {
        if coarse.is_empty() {
            return None;
        }

        // Only players actually requested by one of the three best coarse
        // builds get an expensive opponent-specific market price.
        let mut focus_seen = HashSet::<PlayerId>::new();
        let mut focus_players = Vec::<PlayerId>::new();
        for rollout in &coarse {
            for player_id in &rollout.future_players {
                if focus_seen.insert(player_id.clone()) {
                    focus_players.push(player_id.clone());
                }
            }
        }

        let focused_economy = self.focused_rational_opponent_market_economy(
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            &focus_players,
            market_totals,
            pricing_strategies,
            pricing_rankings,
            config,
        );

        // Re-score the same finalist strategies under the focused rational
        // market. Because all three are refined before sorting, j margin and
        // the displayed alternatives compare like with like.
        let finalist_strategies = coarse
            .iter()
            .map(|result| result.strategy.clone())
            .collect::<Vec<_>>();
        let finalist_rankings = self.rank_available_by_strategy(candidates, &finalist_strategies);
        let mut refined = self.top_intentional_auction_rollouts(
            roster_size_at_start,
            own_sum,
            own_open_slots,
            own_budget_remaining,
            &finalist_strategies,
            &finalist_rankings,
            &focused_economy,
            opponent_sums,
            config,
            3,
        );
        if refined.is_empty() {
            refined = coarse;
        }

        let best = refined.remove(0);
        let second_probability = refined
            .first()
            .map(|result| result.evaluation.matchup_win_probability)
            .unwrap_or(best.evaluation.matchup_win_probability);
        let alternatives = refined
            .iter()
            .map(|result| StrategyAlternative {
                j_name: describe_strategy(
                    &result.strategy,
                    result.evaluation.category_win_probabilities,
                ),
                j_weights: result.strategy.weights,
                projected_matchup_win_probability: result.evaluation.matchup_win_probability,
                projected_expected_categories: result.evaluation.expected_categories,
                build_name: describe_projected_build(result.evaluation.category_win_probabilities),
            })
            .collect::<Vec<_>>();

        Some(DurantRosterPlan {
            current_matchup_win_probability: current.matchup_win_probability,
            current_expected_categories: current.expected_categories,
            current_category_win_probabilities: current.category_win_probabilities,
            projected_matchup_win_probability: best.evaluation.matchup_win_probability,
            projected_expected_categories: best.evaluation.expected_categories,
            projected_category_win_probabilities: best.evaluation.category_win_probabilities,
            build_name: describe_projected_build(best.evaluation.category_win_probabilities),
            j_name: describe_strategy(&best.strategy, best.evaluation.category_win_probabilities),
            j_weights: best.strategy.weights,
            j_alternatives: alternatives,
            j_margin: best.evaluation.matchup_win_probability - second_probability,
            projected_future_players: best.future_players,
            projected_future_player_names: best.future_player_names,
            projected_future_spend: best.future_spend,
            projected_budget_left: best.budget_left,
        })
    }

    /// Search supplied strategies under one already-built market economy while
    /// respecting the live-draft assumption that only the first 10 total roster
    /// spots are intentional purchases. Remaining slots are generic $1 fliers.
    fn top_intentional_auction_rollouts(
        &self,
        roster_size_at_start: usize,
        starting_x: [f64; 9],
        future_slots: usize,
        starting_budget: u16,
        strategies: &[RolloutStrategy],
        strategy_rankings: &[Vec<PlayerId>],
        economy: &MarketEconomy,
        opponent_sums: &[([f64; 9], usize)],
        config: AuctionConfig,
        keep: usize,
    ) -> Vec<AuctionRolloutResult> {
        if keep == 0 {
            return Vec::new();
        }

        if future_slots == 0 || strategies.is_empty() {
            return vec![AuctionRolloutResult {
                strategy: RolloutStrategy::balanced(),
                evaluation: self.evaluate_dynamic_state(starting_x, opponent_sums),
                future_players: Vec::new(),
                future_player_names: Vec::new(),
                future_spend: 0,
                budget_left: starting_budget,
            }];
        }

        let minimum_required = (future_slots as u32).saturating_mul(config.minimum_bid as u32);
        if (starting_budget as u32) < minimum_required {
            return Vec::new();
        }

        let intentional_target = INTENTIONAL_ROSTER_SLOTS.min(self.team_size);
        let intentional_remaining = intentional_target
            .saturating_sub(roster_size_at_start)
            .min(future_slots);

        // One rollout is independent of every other j.  The old v33 code ran
        // this loop serially even during the 212,941-vector offline oracle.
        // Keep tiny pricing/runtime searches serial to avoid Rayon overhead,
        // but use all Rayon workers for larger bank/deep searches.
        let evaluate = |strategy: &RolloutStrategy, ranked_players: &Vec<PlayerId>| {
            let mut completed_x = starting_x;
            let mut budget = starting_budget;
            let mut future_spend = 0u16;
            let mut future_players = Vec::<PlayerId>::with_capacity(intentional_remaining);
            let mut future_player_names = Vec::<String>::with_capacity(future_slots);
            let mut cursor = 0usize;

            for intentional_index in 0..intentional_remaining {
                let slots_after_this_purchase = future_slots.saturating_sub(intentional_index + 1);
                let reserve_after =
                    (slots_after_this_purchase as u32).saturating_mul(config.minimum_bid as u32);
                let max_spend = (budget as u32)
                    .saturating_sub(reserve_after)
                    .min(u16::MAX as u32) as u16;

                let mut chosen: Option<(&PlayerId, [f64; 9], u16)> = None;
                while cursor < ranked_players.len() {
                    let player_id = &ranked_players[cursor];
                    cursor += 1;

                    let Some(x) = self.x_score_for(player_id) else {
                        continue;
                    };
                    let price = economy
                        .prices
                        .get(player_id)
                        .copied()
                        .unwrap_or(config.minimum_bid);

                    if price <= max_spend {
                        chosen = Some((player_id, x, price));
                        break;
                    }
                    // Available budget headroom cannot increase later in this
                    // rollout, so an unaffordable target can be skipped forever.
                }

                let Some((player_id, x, price)) = chosen else {
                    break;
                };

                completed_x = add_vectors(completed_x, x);
                budget = budget.saturating_sub(price);
                future_spend = future_spend.saturating_add(price);
                future_players.push(player_id.clone());
                if let Some(score) = self.score_for(player_id) {
                    future_player_names.push(format!("{} (${price})", score.player_name));
                }
            }

            // Any unfilled intentional slot plus the normal 10->13 tail is a
            // conservative minimum-bid replacement/flier.
            let flier_count = future_slots.saturating_sub(future_players.len());
            for _ in 0..flier_count {
                if budget < config.minimum_bid {
                    break;
                }
                completed_x = add_vectors(completed_x, economy.snapshot.replacement_x_scores);
                budget = budget.saturating_sub(config.minimum_bid);
                future_spend = future_spend.saturating_add(config.minimum_bid);
                future_player_names.push(format!("Flier / replacement (${})", config.minimum_bid));
            }

            AuctionRolloutResult {
                strategy: strategy.clone(),
                evaluation: self.evaluate_dynamic_state(completed_x, opponent_sums),
                future_players,
                future_player_names,
                future_spend,
                budget_left: budget,
            }
        };

        const PARALLEL_ROLLOUT_THRESHOLD: usize = 256;
        if strategies.len() >= PARALLEL_ROLLOUT_THRESHOLD {
            let mut best = strategies
                .par_iter()
                .zip(strategy_rankings.par_iter())
                .map(|(strategy, ranked_players)| evaluate(strategy, ranked_players))
                .fold(
                    || Vec::<AuctionRolloutResult>::with_capacity(keep + 1),
                    |mut local_best, result| {
                        local_best.push(result);
                        sort_and_truncate_auction_rollouts(&mut local_best, keep);
                        local_best
                    },
                )
                .reduce(
                    || Vec::<AuctionRolloutResult>::with_capacity(keep + 1),
                    |mut left, mut right| {
                        left.append(&mut right);
                        sort_and_truncate_auction_rollouts(&mut left, keep);
                        left
                    },
                );
            sort_and_truncate_auction_rollouts(&mut best, keep);
            return best;
        }

        let mut best = Vec::<AuctionRolloutResult>::with_capacity(keep + 1);
        for (strategy, ranked_players) in strategies.iter().zip(strategy_rankings.iter()) {
            best.push(evaluate(strategy, ranked_players));
            sort_and_truncate_auction_rollouts(&mut best, keep);
        }
        best
    }

    /// Shared H-score rollout engine. The strategy library is the only thing
    /// that differs between the normal and overnight searches.
    fn compute_dynamic_scores_with_strategies(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
        strategies: &[RolloutStrategy],
    ) -> Vec<DynamicDurantScore> {
        self.compute_dynamic_scores_for_candidates_with_strategies(
            own_roster,
            opponent_rosters,
            candidates,
            candidates,
            strategies,
        )
    }

    fn compute_dynamic_scores_for_candidates_with_strategies(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        available_candidates: &[PlayerId],
        evaluated_candidates: &[PlayerId],
        strategies: &[RolloutStrategy],
    ) -> Vec<DynamicDurantScore> {
        let own_sum = self.aggregate_x_scores(own_roster);

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };

        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let baseline = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let strategy_rankings = self.rank_available_by_strategy(available_candidates, strategies);

        let owned = own_roster.iter().cloned().collect::<HashSet<_>>();
        let future_slots_after_candidate = self
            .team_size
            .saturating_sub(own_roster.len().saturating_add(1));

        let mut results = evaluated_candidates
            .par_iter()
            .filter(|player_id| !owned.contains(*player_id))
            .filter_map(|player_id| {
                let static_score = self.score_for(player_id)?;
                let candidate_x = score_to_x_vector(static_score, self.parameters);
                let with_candidate = add_vectors(own_sum, candidate_x);
                let immediate = self.evaluate_dynamic_state(with_candidate, &opponent_sums);

                let mut rollouts = self.top_rollouts_for_candidate(
                    player_id,
                    with_candidate,
                    future_slots_after_candidate,
                    strategies,
                    &strategy_rankings,
                    &opponent_sums,
                    3,
                );

                if rollouts.is_empty() {
                    return None;
                }

                let rollout = rollouts.remove(0);
                let second_probability = rollouts
                    .first()
                    .map(|result| result.evaluation.matchup_win_probability)
                    .unwrap_or(rollout.evaluation.matchup_win_probability);

                let alternatives = rollouts
                    .iter()
                    .map(|result| StrategyAlternative {
                        j_name: describe_strategy(
                            &result.strategy,
                            result.evaluation.category_win_probabilities,
                        ),
                        j_weights: result.strategy.weights,
                        projected_matchup_win_probability: result
                            .evaluation
                            .matchup_win_probability,
                        projected_expected_categories: result.evaluation.expected_categories,
                        build_name: describe_projected_build(
                            result.evaluation.category_win_probabilities,
                        ),
                    })
                    .collect::<Vec<_>>();

                Some(DynamicDurantScore {
                    player_id: player_id.clone(),
                    player_name: static_score.player_name.clone(),
                    x_scores: candidate_x,
                    category_win_probabilities: immediate.category_win_probabilities,
                    expected_categories: immediate.expected_categories,
                    matchup_win_probability: immediate.matchup_win_probability,
                    marginal_matchup_win_probability: immediate.matchup_win_probability
                        - baseline.matchup_win_probability,
                    projected_matchup_win_probability: rollout.evaluation.matchup_win_probability,
                    projected_expected_categories: rollout.evaluation.expected_categories,
                    projected_category_win_probabilities: rollout
                        .evaluation
                        .category_win_probabilities,
                    build_name: describe_projected_build(
                        rollout.evaluation.category_win_probabilities,
                    ),
                    j_name: describe_strategy(
                        &rollout.strategy,
                        rollout.evaluation.category_win_probabilities,
                    ),
                    j_weights: rollout.strategy.weights,
                    j_alternatives: alternatives,
                    j_margin: rollout.evaluation.matchup_win_probability - second_probability,
                    projected_future_players: rollout.future_players,
                    projected_future_player_names: rollout.future_player_names,
                })
            })
            .collect::<Vec<_>>();

        results.sort_by(|a, b| {
            b.projected_matchup_win_probability
                .total_cmp(&a.projected_matchup_win_probability)
                .then_with(|| {
                    b.matchup_win_probability
                        .total_cmp(&a.matchup_win_probability)
                })
                .then_with(|| b.expected_categories.total_cmp(&a.expected_categories))
        });

        results
    }

    fn auction_cache_key(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        config: AuctionConfig,
    ) -> u64 {
        let mut hasher = DefaultHasher::new();
        AUCTION_SEARCH_VERSION.hash(&mut hasher);
        self.model_fingerprint.hash(&mut hasher);
        self.reference_size.hash(&mut hasher);
        self.team_size.hash(&mut hasher);
        own_budget_remaining.hash(&mut hasher);
        config.starting_budget.hash(&mut hasher);
        config.minimum_bid.hash(&mut hasher);
        config.fair_price_iterations.hash(&mut hasher);

        own_roster.len().hash(&mut hasher);
        for player_id in own_roster {
            player_id.hash(&mut hasher);
        }

        opponent_rosters.len().hash(&mut hasher);
        for roster in opponent_rosters {
            roster.len().hash(&mut hasher);
            for player_id in roster {
                player_id.hash(&mut hasher);
            }
        }

        opponent_budgets_remaining.hash(&mut hasher);
        candidates.len().hash(&mut hasher);
        for player_id in candidates {
            player_id.hash(&mut hasher);
        }

        hasher.finish()
    }

    fn auction_cache_path(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        config: AuctionConfig,
    ) -> Option<PathBuf> {
        if self.cache_dir.as_os_str().is_empty() {
            return None;
        }

        let key = self.auction_cache_key(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            config,
        );

        Some(
            self.cache_dir
                .join("durant")
                .join(format!("auction_v{AUCTION_SEARCH_VERSION}_{key:016x}.json")),
        )
    }

    fn load_auction_cache(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        config: AuctionConfig,
    ) -> Option<AuctionAnalysis> {
        let path = self.auction_cache_path(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            config,
        )?;
        let json = fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    fn save_auction_cache(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        config: AuctionConfig,
        analysis: &AuctionAnalysis,
    ) {
        let Some(path) = self.auction_cache_path(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            config,
        ) else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(json) = serde_json::to_string(analysis) else {
            return;
        };
        let _ = fs::write(path, json);
    }

    fn auction_candidate_cache_path(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        config: AuctionConfig,
        candidate: &PlayerId,
    ) -> Option<PathBuf> {
        if self.cache_dir.as_os_str().is_empty() {
            return None;
        }

        let state_key = self.auction_cache_key(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            config,
        );
        let mut hasher = DefaultHasher::new();
        state_key.hash(&mut hasher);
        candidate.hash(&mut hasher);
        let candidate_key = hasher.finish();

        Some(self.cache_dir.join("durant").join(format!(
            "auction_v{AUCTION_SEARCH_VERSION}_{state_key:016x}_{candidate_key:016x}.json"
        )))
    }

    fn load_auction_candidate_cache(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        config: AuctionConfig,
        candidate: &PlayerId,
    ) -> Option<AuctionDurantScore> {
        let path = self.auction_candidate_cache_path(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            config,
            candidate,
        )?;
        let json = fs::read_to_string(path).ok()?;
        serde_json::from_str(&json).ok()
    }

    fn save_auction_candidate_cache(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        config: AuctionConfig,
        candidate: &PlayerId,
        score: &AuctionDurantScore,
    ) {
        let Some(path) = self.auction_candidate_cache_path(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            config,
            candidate,
        ) else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(json) = serde_json::to_string(score) else {
            return;
        };
        let _ = fs::write(path, json);
    }

    /// Estimate the current generic auction market, then compute a roster-
    /// specific H-score indifference price for every candidate.
    ///
    /// `candidates` is the currently available player pool. Missing opponent
    /// roster/budget entries are treated as empty/full-budget teams, which is
    /// useful for pre-draft analysis.
    pub fn auction_analysis(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        config: AuctionConfig,
    ) -> Result<AuctionAnalysis> {
        if config.starting_budget == 0 {
            bail!("auction starting budget must be positive");
        }
        if config.minimum_bid == 0 {
            bail!("auction minimum bid must be positive");
        }
        if own_roster.len() > self.team_size {
            bail!("own roster has more players than Durant team_size");
        }

        if let Some(cached) = self.load_auction_cache(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            config,
        ) {
            return Ok(cached);
        }

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let strategies = rollout_strategies();
        let strategy_rankings = self.rank_available_by_strategy(candidates, &strategies);
        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let economy =
            self.estimate_market_economy_from_totals(candidates, None, market_totals, config);

        let own_sum = self.aggregate_x_scores(own_roster);
        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        let owned = own_roster.iter().cloned().collect::<HashSet<_>>();

        let mut scores = candidates
            .par_iter()
            .filter(|player_id| !owned.contains(*player_id))
            .filter_map(|candidate| {
                let Some(static_score) = self.score_for(candidate) else {
                    return None;
                };
                if own_open_slots == 0 {
                    return None;
                }

                if let Some(cached_score) = self.load_auction_candidate_cache(
                    own_roster,
                    own_budget_remaining,
                    opponent_rosters,
                    opponent_budgets_remaining,
                    candidates,
                    config,
                    candidate,
                ) {
                    return Some(cached_score);
                }

                let expected_sale_price = economy
                    .prices
                    .get(candidate)
                    .copied()
                    .unwrap_or(config.minimum_bid);
                let pass_totals = AuctionMarketTotals {
                    remaining_slots: market_totals.remaining_slots.saturating_sub(1),
                    remaining_dollars: market_totals
                        .remaining_dollars
                        .saturating_sub(expected_sale_price as u32),
                    max_legal_market_bid: market_totals.max_legal_market_bid,
                };
                let pass_economy = self.estimate_market_economy_from_totals(
                    candidates,
                    Some(candidate),
                    pass_totals,
                    config,
                );
                let pass = self
                    .top_auction_rollouts(
                        Some(candidate),
                        own_sum,
                        own_open_slots,
                        own_budget_remaining,
                        &strategies,
                        &strategy_rankings,
                        &pass_economy,
                        &opponent_sums,
                        config,
                        1,
                    )
                    .into_iter()
                    .next();
                let Some(pass) = pass else {
                    return None;
                };
                let pass_h = pass.evaluation.matchup_win_probability;

                let max_bid =
                    max_legal_bid(own_budget_remaining, own_open_slots, config.minimum_bid);
                if max_bid < config.minimum_bid {
                    return None;
                }

                let market_exact = economy
                    .exact_prices
                    .get(candidate)
                    .copied()
                    .unwrap_or(config.minimum_bid as f64);
                let market_price = economy
                    .prices
                    .get(candidate)
                    .copied()
                    .unwrap_or(config.minimum_bid);
                let analysis_price = market_price.clamp(config.minimum_bid, max_bid);

                let mut buy_cache = HashMap::<u16, AuctionRolloutResult>::new();
                let mut evaluate_buy = |price: u16| -> Option<AuctionRolloutResult> {
                    if let Some(result) = buy_cache.get(&price) {
                        return Some(result.clone());
                    }
                    let result = self.best_buy_rollout(
                        candidate,
                        static_score,
                        own_sum,
                        own_open_slots,
                        own_budget_remaining,
                        price,
                        &strategies,
                        &strategy_rankings,
                        candidates,
                        market_totals,
                        None,
                        &opponent_sums,
                        config,
                    )?;
                    buy_cache.insert(price, result.clone());
                    Some(result)
                };

                let Some(market_rollout) = evaluate_buy(analysis_price) else {
                    return None;
                };

                let minimum_price = config.minimum_bid;
                let cheap_h = evaluate_buy(minimum_price)
                    .map(|r| r.evaluation.matchup_win_probability)
                    .unwrap_or(0.0);

                let fair_price = if cheap_h + H_INDIFFERENCE_EPSILON < pass_h {
                    0
                } else {
                    let max_h = evaluate_buy(max_bid)
                        .map(|r| r.evaluation.matchup_win_probability)
                        .unwrap_or(0.0);

                    if max_h + H_INDIFFERENCE_EPSILON >= pass_h {
                        max_bid
                    } else {
                        let mut low = minimum_price;
                        let mut high = max_bid;
                        let mut steps = 0u8;

                        while low + 1 < high && steps < config.fair_price_iterations {
                            let mid = low + (high - low) / 2;
                            let mid_h = evaluate_buy(mid)
                                .map(|r| r.evaluation.matchup_win_probability)
                                .unwrap_or(0.0);

                            if mid_h + H_INDIFFERENCE_EPSILON >= pass_h {
                                low = mid;
                            } else {
                                high = mid;
                            }
                            steps += 1;
                        }

                        // Small local verification protects us from tiny non-
                        // monotonicities introduced by the deterministic greedy
                        // rollout without turning pricing into a full $1 scan.
                        let start = low.saturating_sub(FAIR_PRICE_LOCAL_RADIUS);
                        let end = high.saturating_add(FAIR_PRICE_LOCAL_RADIUS).min(max_bid);
                        let mut best_good = low;
                        for price in start.max(minimum_price)..=end {
                            let h = evaluate_buy(price)
                                .map(|r| r.evaluation.matchup_win_probability)
                                .unwrap_or(0.0);
                            if h + H_INDIFFERENCE_EPSILON >= pass_h {
                                best_good = best_good.max(price);
                            }
                        }
                        best_good
                    }
                };

                let fair_rollout = if fair_price == 0 {
                    market_rollout.clone()
                } else {
                    evaluate_buy(fair_price).unwrap_or_else(|| market_rollout.clone())
                };

                let expected_edge_i32 = fair_price as i32 - market_price as i32;
                let expected_edge =
                    expected_edge_i32.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

                let auction_score = AuctionDurantScore {
                    player_id: candidate.clone(),
                    player_name: static_score.player_name.clone(),
                    market_price_exact: market_exact,
                    market_price,
                    evaluated_market_price: analysis_price,
                    fair_price,
                    expected_edge,
                    pass_projected_matchup_win_probability: pass_h,
                    market_price_projected_matchup_win_probability: market_rollout
                        .evaluation
                        .matchup_win_probability,
                    fair_price_projected_matchup_win_probability: if fair_price == 0 {
                        pass_h
                    } else {
                        fair_rollout.evaluation.matchup_win_probability
                    },
                    build_name: describe_projected_build(
                        market_rollout.evaluation.category_win_probabilities,
                    ),
                    j_name: describe_strategy(
                        &market_rollout.strategy,
                        market_rollout.evaluation.category_win_probabilities,
                    ),
                    j_weights: market_rollout.strategy.weights,
                    projected_category_win_probabilities: market_rollout
                        .evaluation
                        .category_win_probabilities,
                    projected_future_players: market_rollout.future_players,
                    projected_future_player_names: market_rollout.future_player_names,
                    projected_future_spend: market_rollout.future_spend,
                    projected_budget_left: market_rollout.budget_left,
                };

                self.save_auction_candidate_cache(
                    own_roster,
                    own_budget_remaining,
                    opponent_rosters,
                    opponent_budgets_remaining,
                    candidates,
                    config,
                    candidate,
                    &auction_score,
                );
                Some(auction_score)
            })
            .collect::<Vec<_>>();

        scores.sort_by(|a, b| {
            b.expected_edge
                .cmp(&a.expected_edge)
                .then_with(|| b.fair_price.cmp(&a.fair_price))
                .then_with(|| {
                    b.market_price_projected_matchup_win_probability
                        .total_cmp(&a.market_price_projected_matchup_win_probability)
                })
        });

        let analysis = AuctionAnalysis {
            economy: economy.snapshot.clone(),
            scores,
        };
        self.save_auction_cache(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            config,
            &analysis,
        );
        Ok(analysis)
    }

    /// Cheap Live-board signal for the full visible player pool.
    ///
    /// This intentionally does NO strategy search and NO rational-opponent bid
    /// solve. It evaluates only current H and H after adding the candidate, and
    /// reuses the generic static market price already computed for Preparation.
    /// A small NOW-ΔH shortlist can then be upgraded by
    /// `coarse_market_advantage_scores_with_strategy_weights`.
    pub fn immediate_market_advantage_scores(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        evaluated_candidates: &[PlayerId],
        market_board: &MarketBoard,
    ) -> Vec<MarketAdvantageScore> {
        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let market_prices = market_board
            .values
            .iter()
            .map(|value| (value.player_id.clone(), value.market_price))
            .collect::<HashMap<_, _>>();

        evaluated_candidates
            .par_iter()
            .filter_map(|candidate| {
                if own_roster.iter().any(|player_id| player_id == candidate) {
                    return None;
                }

                let static_score = self.score_for(candidate)?;
                let candidate_x = score_to_x_vector(static_score, self.parameters);
                let immediate =
                    self.evaluate_dynamic_state(add_vectors(own_sum, candidate_x), &opponent_sums);
                let market_price = market_prices.get(candidate).copied().unwrap_or(1);

                Some(MarketAdvantageScore {
                    player_id: candidate.clone(),
                    player_name: static_score.player_name.clone(),
                    market_price,
                    has_projected_finish: false,
                    current_matchup_win_probability: current.matchup_win_probability,
                    current_category_win_probabilities: current.category_win_probabilities,
                    immediate_matchup_win_probability: immediate.matchup_win_probability,
                    marginal_immediate_matchup_win_probability: immediate.matchup_win_probability
                        - current.matchup_win_probability,
                    immediate_category_win_probabilities: immediate.category_win_probabilities,
                    // Deliberately neutral placeholders. Callers must check
                    // has_projected_finish before presenting FINAL/build data.
                    marginal_projected_matchup_win_probability: 0.0,
                    buy_projected_matchup_win_probability: immediate.matchup_win_probability,
                    pass_projected_matchup_win_probability: current.matchup_win_probability,
                    pass_projected_category_win_probabilities: current.category_win_probabilities,
                    buy_projected_category_win_probabilities: immediate.category_win_probabilities,
                    build_name: "shortlist projection pending".to_string(),
                    j_name: "NOW only".to_string(),
                    j_weights: [0.0; 9],
                    projected_future_players: Vec::new(),
                    projected_future_player_names: Vec::new(),
                    projected_future_spend: 0,
                    projected_budget_left: 0,
                })
            })
            .collect()
    }

    /// Fast future-roster projection for a small Live-board shortlist.
    ///
    /// Unlike the exact nomination path, this uses the generic static auction
    /// market to choose PASS/BUY strategies and to price future targets. It
    /// still searches the caller-supplied strategy bank, still lets every
    /// shortlisted decision complete from the FULL remaining player pool, and
    /// preserves the 10 intentional + 3 flier roster model. The expensive
    /// rational-opponent market and sequential rational repricing are reserved
    /// for the single-player Enter calculation.
    pub fn coarse_market_advantage_scores_with_strategy_weights(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        available_candidates: &[PlayerId],
        evaluated_candidates: &[PlayerId],
        strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
    ) -> Result<Vec<MarketAdvantageScore>> {
        if config.starting_budget == 0 {
            bail!("auction starting budget must be positive");
        }
        if config.minimum_bid == 0 {
            bail!("auction minimum bid must be positive");
        }
        if own_roster.len() > self.team_size {
            bail!("own roster has more players than Durant team_size");
        }
        if strategy_weights.is_empty() || evaluated_candidates.is_empty() {
            return Ok(Vec::new());
        }

        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        if own_open_slots == 0 {
            return Ok(Vec::new());
        }

        let strategies = strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();
        let strategy_rankings = self.rank_available_by_strategy(available_candidates, &strategies);

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let economy = self.estimate_market_economy_from_totals(
            available_candidates,
            None,
            market_totals,
            config,
        );
        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let max_bid = max_legal_bid(own_budget_remaining, own_open_slots, config.minimum_bid);
        if max_bid < config.minimum_bid {
            return Ok(Vec::new());
        }

        let scores = evaluated_candidates
            .par_iter()
            .filter_map(|candidate| {
                if own_roster.iter().any(|player_id| player_id == candidate) {
                    return None;
                }

                let static_score = self.score_for(candidate)?;
                let market_price = economy
                    .prices
                    .get(candidate)
                    .copied()
                    .unwrap_or(config.minimum_bid);
                if market_price > max_bid {
                    return None;
                }
                let analysis_price = market_price.max(config.minimum_bid);

                let candidate_x = score_to_x_vector(static_score, self.parameters);
                let immediate =
                    self.evaluate_dynamic_state(add_vectors(own_sum, candidate_x), &opponent_sums);

                let pass_totals = AuctionMarketTotals {
                    remaining_slots: market_totals.remaining_slots.saturating_sub(1),
                    remaining_dollars: market_totals
                        .remaining_dollars
                        .saturating_sub(market_price as u32),
                    max_legal_market_bid: market_totals.max_legal_market_bid,
                };
                let pass_economy = self.estimate_market_economy_from_totals(
                    available_candidates,
                    Some(candidate),
                    pass_totals,
                    config,
                );
                let coarse_pass = self
                    .top_auction_rollouts(
                        Some(candidate),
                        own_sum,
                        own_open_slots,
                        own_budget_remaining,
                        &strategies,
                        &strategy_rankings,
                        &pass_economy,
                        &opponent_sums,
                        config,
                        1,
                    )
                    .into_iter()
                    .next()?;

                let coarse_buy = self.best_buy_rollout(
                    candidate,
                    static_score,
                    own_sum,
                    own_open_slots,
                    own_budget_remaining,
                    analysis_price,
                    &strategies,
                    &strategy_rankings,
                    available_candidates,
                    market_totals,
                    None,
                    &opponent_sums,
                    config,
                )?;

                let pass = self.refine_rollout_with_static_future_market(
                    own_sum,
                    own_roster.len(),
                    own_open_slots,
                    own_budget_remaining,
                    &coarse_pass.strategy,
                    std::slice::from_ref(candidate),
                    available_candidates,
                    pass_totals,
                    &opponent_sums,
                    config,
                );

                let buy_start_x = add_vectors(own_sum, candidate_x);
                let buy_budget = own_budget_remaining.saturating_sub(analysis_price);
                let buy_future_slots = own_open_slots.saturating_sub(1);
                let buy_totals = AuctionMarketTotals {
                    remaining_slots: market_totals.remaining_slots.saturating_sub(1),
                    remaining_dollars: market_totals
                        .remaining_dollars
                        .saturating_sub(analysis_price as u32),
                    max_legal_market_bid: market_totals.max_legal_market_bid,
                };
                let buy = self.refine_rollout_with_static_future_market(
                    buy_start_x,
                    own_roster.len().saturating_add(1),
                    buy_future_slots,
                    buy_budget,
                    &coarse_buy.strategy,
                    std::slice::from_ref(candidate),
                    available_candidates,
                    buy_totals,
                    &opponent_sums,
                    config,
                );

                Some(MarketAdvantageScore {
                    player_id: candidate.clone(),
                    player_name: static_score.player_name.clone(),
                    market_price,
                    has_projected_finish: true,
                    current_matchup_win_probability: current.matchup_win_probability,
                    current_category_win_probabilities: current.category_win_probabilities,
                    immediate_matchup_win_probability: immediate.matchup_win_probability,
                    marginal_immediate_matchup_win_probability: immediate.matchup_win_probability
                        - current.matchup_win_probability,
                    immediate_category_win_probabilities: immediate.category_win_probabilities,
                    marginal_projected_matchup_win_probability: buy
                        .evaluation
                        .matchup_win_probability
                        - pass.evaluation.matchup_win_probability,
                    buy_projected_matchup_win_probability: buy.evaluation.matchup_win_probability,
                    pass_projected_matchup_win_probability: pass.evaluation.matchup_win_probability,
                    pass_projected_category_win_probabilities: pass
                        .evaluation
                        .category_win_probabilities,
                    buy_projected_category_win_probabilities: buy
                        .evaluation
                        .category_win_probabilities,
                    build_name: describe_projected_build(buy.evaluation.category_win_probabilities),
                    j_name: describe_strategy(
                        &buy.strategy,
                        buy.evaluation.category_win_probabilities,
                    ),
                    j_weights: buy.strategy.weights,
                    projected_future_players: buy.future_players,
                    projected_future_player_names: buy.future_player_names,
                    projected_future_spend: buy.future_spend,
                    projected_budget_left: buy.budget_left,
                })
            })
            .collect::<Vec<_>>();

        Ok(scores)
    }

    /// Live-board auction signal using rational opponent bids plus a sequential
    /// future-market refinement. The first 10 roster spots are intentional;
    /// the final 3 are modeled as minimum-bid fliers.
    ///
    /// `available_candidates` is the full future player pool used by rollouts.
    /// `evaluated_candidates` can be a smaller league-depth shortlist for the
    /// visible live board.
    pub fn market_advantage_scores_with_strategy_weights(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        available_candidates: &[PlayerId],
        evaluated_candidates: &[PlayerId],
        strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
    ) -> Result<Vec<MarketAdvantageScore>> {
        if config.starting_budget == 0 {
            bail!("auction starting budget must be positive");
        }
        if config.minimum_bid == 0 {
            bail!("auction minimum bid must be positive");
        }
        if own_roster.len() > self.team_size {
            bail!("own roster has more players than Durant team_size");
        }
        if strategy_weights.is_empty() {
            return Ok(Vec::new());
        }

        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        if own_open_slots == 0 {
            return Ok(Vec::new());
        }

        let strategies = strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();

        // This is the expensive ordering work. Build it once for the entire
        // board rather than once per candidate.
        let strategy_rankings = self.rank_available_by_strategy(available_candidates, &strategies);

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let economy = self.rational_opponent_market_economy(
            opponent_rosters,
            opponent_budgets_remaining,
            available_candidates,
            market_totals,
            &strategies,
            &strategy_rankings,
            config,
        );
        let own_sum = self.aggregate_x_scores(own_roster);
        let current = self.evaluate_dynamic_state(own_sum, &opponent_sums);
        let max_bid = max_legal_bid(own_budget_remaining, own_open_slots, config.minimum_bid);
        if max_bid < config.minimum_bid {
            return Ok(Vec::new());
        }

        let mut scores = evaluated_candidates
            .par_iter()
            .filter_map(|candidate| {
                if own_roster.iter().any(|player_id| player_id == candidate) {
                    return None;
                }

                let static_score = self.score_for(candidate)?;
                let market_price = economy
                    .prices
                    .get(candidate)
                    .copied()
                    .unwrap_or(config.minimum_bid);
                if market_price > max_bid {
                    return None;
                }
                let analysis_price = market_price.max(config.minimum_bid);

                let candidate_x = score_to_x_vector(static_score, self.parameters);
                let with_candidate = add_vectors(own_sum, candidate_x);
                let immediate = self.evaluate_dynamic_state(with_candidate, &opponent_sums);

                let pass_totals = AuctionMarketTotals {
                    remaining_slots: market_totals.remaining_slots.saturating_sub(1),
                    remaining_dollars: market_totals
                        .remaining_dollars
                        .saturating_sub(market_price as u32),
                    max_legal_market_bid: market_totals.max_legal_market_bid,
                };
                let pass_economy =
                    self.competition_economy_after_sale(&economy, candidate, pass_totals, config);

                let coarse_pass = self
                    .top_auction_rollouts(
                        Some(candidate),
                        own_sum,
                        own_open_slots,
                        own_budget_remaining,
                        &strategies,
                        &strategy_rankings,
                        &pass_economy,
                        &opponent_sums,
                        config,
                        1,
                    )
                    .into_iter()
                    .next()?;

                let coarse_buy = self.best_buy_rollout(
                    candidate,
                    static_score,
                    own_sum,
                    own_open_slots,
                    own_budget_remaining,
                    analysis_price,
                    &strategies,
                    &strategy_rankings,
                    available_candidates,
                    market_totals,
                    Some(&economy),
                    &opponent_sums,
                    config,
                )?;

                let pass = self.refine_rollout_with_dynamic_future_market(
                    own_sum,
                    own_roster.len(),
                    own_open_slots,
                    own_budget_remaining,
                    &coarse_pass.strategy,
                    std::slice::from_ref(candidate),
                    available_candidates,
                    opponent_rosters,
                    opponent_budgets_remaining,
                    pass_totals,
                    &strategies,
                    &opponent_sums,
                    config,
                );

                let candidate_x = score_to_x_vector(static_score, self.parameters);
                let buy_start_x = add_vectors(own_sum, candidate_x);
                let buy_budget = own_budget_remaining.saturating_sub(analysis_price);
                let buy_future_slots = own_open_slots.saturating_sub(1);
                let buy_totals = AuctionMarketTotals {
                    remaining_slots: market_totals.remaining_slots.saturating_sub(1),
                    remaining_dollars: market_totals
                        .remaining_dollars
                        .saturating_sub(analysis_price as u32),
                    max_legal_market_bid: market_totals.max_legal_market_bid,
                };
                let buy = self.refine_rollout_with_dynamic_future_market(
                    buy_start_x,
                    own_roster.len().saturating_add(1),
                    buy_future_slots,
                    buy_budget,
                    &coarse_buy.strategy,
                    std::slice::from_ref(candidate),
                    available_candidates,
                    opponent_rosters,
                    opponent_budgets_remaining,
                    buy_totals,
                    &strategies,
                    &opponent_sums,
                    config,
                );

                Some(MarketAdvantageScore {
                    player_id: candidate.clone(),
                    player_name: static_score.player_name.clone(),
                    market_price,
                    has_projected_finish: true,
                    current_matchup_win_probability: current.matchup_win_probability,
                    current_category_win_probabilities: current.category_win_probabilities,
                    immediate_matchup_win_probability: immediate.matchup_win_probability,
                    marginal_immediate_matchup_win_probability: immediate.matchup_win_probability
                        - current.matchup_win_probability,
                    immediate_category_win_probabilities: immediate.category_win_probabilities,
                    marginal_projected_matchup_win_probability: buy
                        .evaluation
                        .matchup_win_probability
                        - pass.evaluation.matchup_win_probability,
                    buy_projected_matchup_win_probability: buy.evaluation.matchup_win_probability,
                    pass_projected_matchup_win_probability: pass.evaluation.matchup_win_probability,
                    pass_projected_category_win_probabilities: pass
                        .evaluation
                        .category_win_probabilities,
                    buy_projected_category_win_probabilities: buy
                        .evaluation
                        .category_win_probabilities,
                    build_name: describe_projected_build(buy.evaluation.category_win_probabilities),
                    j_name: describe_strategy(
                        &buy.strategy,
                        buy.evaluation.category_win_probabilities,
                    ),
                    j_weights: buy.strategy.weights,
                    projected_future_players: buy.future_players,
                    projected_future_player_names: buy.future_player_names,
                    projected_future_spend: buy.future_spend,
                    projected_budget_left: buy.budget_left,
                })
            })
            .collect::<Vec<_>>();

        scores.sort_by(|a, b| {
            b.marginal_projected_matchup_win_probability
                .total_cmp(&a.marginal_projected_matchup_win_probability)
                .then_with(|| {
                    b.buy_projected_matchup_win_probability
                        .total_cmp(&a.buy_projected_matchup_win_probability)
                })
                .then_with(|| b.market_price.cmp(&a.market_price))
        });

        Ok(scores)
    }

    /// Exact auction decision for one nominated player using a caller-supplied
    /// strategy bank. Unlike `auction_analysis`, this does not score the whole
    /// board and does not touch the legacy 2,620-j auction cache.
    pub fn auction_candidate_analysis_with_strategy_weights(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        candidate: &PlayerId,
        strategy_weights: &[[f64; 9]],
        config: AuctionConfig,
    ) -> Result<Option<AuctionDurantScore>> {
        if config.starting_budget == 0 {
            bail!("auction starting budget must be positive");
        }
        if config.minimum_bid == 0 {
            bail!("auction minimum bid must be positive");
        }
        if own_roster.len() > self.team_size {
            bail!("own roster has more players than Durant team_size");
        }
        if strategy_weights.is_empty() {
            return Ok(None);
        }
        if own_roster.iter().any(|player_id| player_id == candidate) {
            return Ok(None);
        }

        let Some(static_score) = self.score_for(candidate) else {
            return Ok(None);
        };
        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        if own_open_slots == 0 {
            return Ok(None);
        }

        let strategies = strategy_weights
            .iter()
            .copied()
            .map(|weights| RolloutStrategy { weights })
            .collect::<Vec<_>>();
        let strategy_rankings = self.rank_available_by_strategy(candidates, &strategies);

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };
        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        // A nomination only needs an opponent-specific clearing price for the
        // nominated player.  v27/v28 rebuilt the rational market for every
        // remaining player here, which made Enter progressively slower as
        // opponent rosters diverged.  Keep the cheap static G/$ market for
        // future completion prices and overwrite only this candidate with the
        // exact live competition price.
        let mut economy =
            self.estimate_market_economy_from_totals(candidates, None, market_totals, config);
        if let Some((exact_price, rounded_price)) = self.rational_opponent_price_for_candidate(
            opponent_rosters,
            opponent_budgets_remaining,
            candidates,
            candidate,
            market_totals,
            &strategies,
            &strategy_rankings,
            config,
        ) {
            economy.exact_prices.insert(candidate.clone(), exact_price);
            economy.prices.insert(candidate.clone(), rounded_price);
        }
        let own_sum = self.aggregate_x_scores(own_roster);

        let expected_sale_price = economy
            .prices
            .get(candidate)
            .copied()
            .unwrap_or(config.minimum_bid);
        let pass_totals = AuctionMarketTotals {
            remaining_slots: market_totals.remaining_slots.saturating_sub(1),
            remaining_dollars: market_totals
                .remaining_dollars
                .saturating_sub(expected_sale_price as u32),
            max_legal_market_bid: market_totals.max_legal_market_bid,
        };
        let pass_economy =
            self.competition_economy_after_sale(&economy, candidate, pass_totals, config);
        let pass = self
            .top_auction_rollouts(
                Some(candidate),
                own_sum,
                own_open_slots,
                own_budget_remaining,
                &strategies,
                &strategy_rankings,
                &pass_economy,
                &opponent_sums,
                config,
                1,
            )
            .into_iter()
            .next();
        let Some(pass) = pass else {
            return Ok(None);
        };
        let pass_h = pass.evaluation.matchup_win_probability;

        let max_bid = max_legal_bid(own_budget_remaining, own_open_slots, config.minimum_bid);
        if max_bid < config.minimum_bid {
            return Ok(None);
        }

        let market_exact = economy
            .exact_prices
            .get(candidate)
            .copied()
            .unwrap_or(config.minimum_bid as f64);
        let market_price = economy
            .prices
            .get(candidate)
            .copied()
            .unwrap_or(config.minimum_bid);
        let analysis_price = market_price.clamp(config.minimum_bid, max_bid);

        let mut buy_cache = HashMap::<u16, AuctionRolloutResult>::new();
        let mut evaluate_buy = |price: u16| -> Option<AuctionRolloutResult> {
            if let Some(result) = buy_cache.get(&price) {
                return Some(result.clone());
            }
            let result = self.best_buy_rollout(
                candidate,
                static_score,
                own_sum,
                own_open_slots,
                own_budget_remaining,
                price,
                &strategies,
                &strategy_rankings,
                candidates,
                market_totals,
                Some(&economy),
                &opponent_sums,
                config,
            )?;
            buy_cache.insert(price, result.clone());
            Some(result)
        };

        let Some(market_rollout) = evaluate_buy(analysis_price) else {
            return Ok(None);
        };

        let minimum_price = config.minimum_bid;
        let cheap_h = evaluate_buy(minimum_price)
            .map(|result| result.evaluation.matchup_win_probability)
            .unwrap_or(0.0);

        let fair_price = if cheap_h + H_INDIFFERENCE_EPSILON < pass_h {
            0
        } else {
            let max_h = evaluate_buy(max_bid)
                .map(|result| result.evaluation.matchup_win_probability)
                .unwrap_or(0.0);

            if max_h + H_INDIFFERENCE_EPSILON >= pass_h {
                max_bid
            } else {
                let mut low = minimum_price;
                let mut high = max_bid;
                let mut steps = 0u8;

                while low + 1 < high && steps < config.fair_price_iterations {
                    let mid = low + (high - low) / 2;
                    let mid_h = evaluate_buy(mid)
                        .map(|result| result.evaluation.matchup_win_probability)
                        .unwrap_or(0.0);

                    if mid_h + H_INDIFFERENCE_EPSILON >= pass_h {
                        low = mid;
                    } else {
                        high = mid;
                    }
                    steps += 1;
                }

                let start = low.saturating_sub(FAIR_PRICE_LOCAL_RADIUS);
                let end = high.saturating_add(FAIR_PRICE_LOCAL_RADIUS).min(max_bid);
                let mut best_good = low;
                for price in start.max(minimum_price)..=end {
                    let h = evaluate_buy(price)
                        .map(|result| result.evaluation.matchup_win_probability)
                        .unwrap_or(0.0);
                    if h + H_INDIFFERENCE_EPSILON >= pass_h {
                        best_good = best_good.max(price);
                    }
                }
                best_good
            }
        };

        let fair_rollout = if fair_price == 0 {
            market_rollout.clone()
        } else {
            evaluate_buy(fair_price).unwrap_or_else(|| market_rollout.clone())
        };

        let expected_edge_i32 = fair_price as i32 - market_price as i32;
        let expected_edge = expected_edge_i32.clamp(i16::MIN as i32, i16::MAX as i32) as i16;

        Ok(Some(AuctionDurantScore {
            player_id: candidate.clone(),
            player_name: static_score.player_name.clone(),
            market_price_exact: market_exact,
            market_price,
            evaluated_market_price: analysis_price,
            fair_price,
            expected_edge,
            pass_projected_matchup_win_probability: pass_h,
            market_price_projected_matchup_win_probability: market_rollout
                .evaluation
                .matchup_win_probability,
            fair_price_projected_matchup_win_probability: if fair_price == 0 {
                pass_h
            } else {
                fair_rollout.evaluation.matchup_win_probability
            },
            build_name: describe_projected_build(
                market_rollout.evaluation.category_win_probabilities,
            ),
            j_name: describe_strategy(
                &market_rollout.strategy,
                market_rollout.evaluation.category_win_probabilities,
            ),
            j_weights: market_rollout.strategy.weights,
            projected_category_win_probabilities: market_rollout
                .evaluation
                .category_win_probabilities,
            projected_future_players: market_rollout.future_players,
            projected_future_player_names: market_rollout.future_player_names,
            projected_future_spend: market_rollout.future_spend,
            projected_budget_left: market_rollout.budget_left,
        }))
    }

    /// Diagnostic helper for inspecting H_buy(price) around a candidate's
    /// reported fair value. The expensive strategy library/rankings are built
    /// once and reused for every requested price.
    pub fn auction_price_curve(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        candidate: &PlayerId,
        prices: &[u16],
        config: AuctionConfig,
    ) -> Result<Vec<(u16, f64)>> {
        if own_roster.len() > self.team_size {
            bail!("own roster has more players than Durant team_size");
        }

        let Some(candidate_score) = self.score_for(candidate) else {
            return Ok(Vec::new());
        };

        let own_open_slots = self.team_size.saturating_sub(own_roster.len());
        if own_open_slots == 0 {
            return Ok(Vec::new());
        }

        let generic_opponent = vec![Vec::<PlayerId>::new()];
        let opponents = if opponent_rosters.is_empty() {
            generic_opponent.as_slice()
        } else {
            opponent_rosters
        };

        let opponent_sums = opponents
            .iter()
            .map(|roster| (self.aggregate_x_scores(roster), roster.len()))
            .collect::<Vec<_>>();

        let strategies = rollout_strategies();
        let strategy_rankings = self.rank_available_by_strategy(candidates, &strategies);
        let market_totals = self.auction_market_totals(
            own_roster,
            own_budget_remaining,
            opponent_rosters,
            opponent_budgets_remaining,
            config,
        );
        let own_sum = self.aggregate_x_scores(own_roster);

        let mut curve = Vec::with_capacity(prices.len());

        for &price in prices {
            if let Some(result) = self.best_buy_rollout(
                candidate,
                candidate_score,
                own_sum,
                own_open_slots,
                own_budget_remaining,
                price,
                &strategies,
                &strategy_rankings,
                candidates,
                market_totals,
                None,
                &opponent_sums,
                config,
            ) {
                curve.push((price, result.evaluation.matchup_win_probability));
            }
        }

        Ok(curve)
    }

    fn auction_market_totals(
        &self,
        own_roster: &[PlayerId],
        own_budget_remaining: u16,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        config: AuctionConfig,
    ) -> AuctionMarketTotals {
        let league_teams = self.league_teams();
        let opponent_count = league_teams.saturating_sub(1);
        let own_open = self.team_size.saturating_sub(own_roster.len());

        let mut remaining_slots = own_open;
        let mut remaining_dollars = own_budget_remaining as u32;
        let mut max_legal_market_bid =
            max_legal_bid(own_budget_remaining, own_open, config.minimum_bid);

        for index in 0..opponent_count {
            let roster_len = opponent_rosters
                .get(index)
                .map(|roster| roster.len())
                .unwrap_or(0)
                .min(self.team_size);
            let open = self.team_size.saturating_sub(roster_len);
            let budget = opponent_budgets_remaining
                .get(index)
                .copied()
                .unwrap_or(config.starting_budget);

            remaining_slots = remaining_slots.saturating_add(open);
            remaining_dollars = remaining_dollars.saturating_add(budget as u32);
            max_legal_market_bid =
                max_legal_market_bid.max(max_legal_bid(budget, open, config.minimum_bid));
        }

        AuctionMarketTotals {
            remaining_slots,
            remaining_dollars,
            max_legal_market_bid,
        }
    }

    /// Rational clearing price for ONE nominated player.
    ///
    /// This is the focused version of `rational_opponent_market_economy` used
    /// by the interactive nomination panel.  The full-market routine is still
    /// useful for bulk analysis, but solving every remaining player before an
    /// Enter key result is unnecessary: only the nominated player's current
    /// clearing price is opponent-specific.  Future completion prices are
    /// supplied by the static G/$ market in the caller.
    fn rational_opponent_price_for_candidate(
        &self,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        candidate: &PlayerId,
        market_totals: AuctionMarketTotals,
        strategies: &[RolloutStrategy],
        strategy_rankings: &[Vec<PlayerId>],
        config: AuctionConfig,
    ) -> Option<(f64, u16)> {
        let static_score = self.score_for(candidate)?;
        let static_economy =
            self.estimate_market_economy_from_totals(candidates, None, market_totals, config);

        let fallback = || {
            let exact = static_economy
                .exact_prices
                .get(candidate)
                .copied()
                .unwrap_or(config.minimum_bid as f64);
            let rounded = static_economy
                .prices
                .get(candidate)
                .copied()
                .unwrap_or(config.minimum_bid);
            (exact, rounded)
        };

        let pricing_count = strategies
            .len()
            .min(strategy_rankings.len())
            .min(COMPETITION_PRICING_STRATEGY_LIMIT);
        if pricing_count == 0 {
            return Some(fallback());
        }
        let pricing_strategies = &strategies[..pricing_count];
        let pricing_rankings = &strategy_rankings[..pricing_count];

        let team_count = opponent_rosters.len().max(opponent_budgets_remaining.len());
        let mut unique_states = Vec::<(Vec<PlayerId>, u16)>::new();
        for index in 0..team_count {
            let mut roster = opponent_rosters.get(index).cloned().unwrap_or_default();
            roster.sort_by(|a, b| a.0.cmp(&b.0));
            let budget = opponent_budgets_remaining
                .get(index)
                .copied()
                .unwrap_or(config.starting_budget);

            if !unique_states.iter().any(|(known_roster, known_budget)| {
                *known_budget == budget && *known_roster == roster
            }) {
                unique_states.push((roster, budget));
            }
        }
        if unique_states.is_empty() {
            return Some(fallback());
        }

        let candidate_x = score_to_x_vector(static_score, self.parameters);
        let generic_field = [([0.0; 9], 0usize)];
        let mut ordered_states = unique_states
            .iter()
            .filter_map(|(roster, budget)| {
                let open_slots = self.team_size.saturating_sub(roster.len());
                if open_slots == 0 {
                    return None;
                }

                let legal_max = max_legal_bid(*budget, open_slots, config.minimum_bid);
                if legal_max < config.minimum_bid {
                    return None;
                }

                let roster_x = self.aggregate_x_scores(roster);
                let before = self.evaluate_dynamic_state(roster_x, &generic_field);
                let after =
                    self.evaluate_dynamic_state(add_vectors(roster_x, candidate_x), &generic_field);
                Some((
                    after.matchup_win_probability - before.matchup_win_probability,
                    legal_max,
                    roster.as_slice(),
                    *budget,
                ))
            })
            .collect::<Vec<_>>();

        ordered_states.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

        let mut strongest_fair_bid = 0u16;
        for (_, legal_max, roster, budget) in ordered_states {
            if legal_max <= strongest_fair_bid {
                continue;
            }

            let fair = self.rational_bid_for_team(
                roster,
                budget,
                candidate,
                static_score,
                candidates,
                market_totals,
                &static_economy,
                pricing_strategies,
                pricing_rankings,
                config,
            );
            strongest_fair_bid = strongest_fair_bid.max(fair);
        }

        let required_to_win = if strongest_fair_bid == 0 {
            config.minimum_bid
        } else {
            strongest_fair_bid.saturating_add(1)
        };
        Some((required_to_win as f64, required_to_win))
    }

    /// Build a mostly-static market, overriding prices only for players that
    /// appear in the top coarse Strategy plans.
    ///
    /// Each focused player's opponent bid calculation still uses ALL remaining
    /// candidates as future alternatives. Restricting `focus_candidates` only
    /// avoids solving expensive H-indifference prices for irrelevant players.
    fn focused_rational_opponent_market_economy(
        &self,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        all_candidates: &[PlayerId],
        focus_candidates: &[PlayerId],
        market_totals: AuctionMarketTotals,
        strategies: &[RolloutStrategy],
        strategy_rankings: &[Vec<PlayerId>],
        config: AuctionConfig,
    ) -> MarketEconomy {
        let mut economy =
            self.estimate_market_economy_from_totals(all_candidates, None, market_totals, config);
        if focus_candidates.is_empty() {
            return economy;
        }

        // Strategy is a planning screen, not an exact nomination quote. A
        // compact pricing core is enough once the global j search has already
        // selected the relevant targets.
        let pricing_count = strategies
            .len()
            .min(strategy_rankings.len())
            .min(FUTURE_REPRICE_STRATEGY_LIMIT);
        if pricing_count == 0 {
            return economy;
        }
        let pricing_strategies = &strategies[..pricing_count];
        let pricing_rankings = &strategy_rankings[..pricing_count];

        let team_count = opponent_rosters.len().max(opponent_budgets_remaining.len());
        let mut unique_states = Vec::<(Vec<PlayerId>, u16)>::new();
        for index in 0..team_count {
            let mut roster = opponent_rosters.get(index).cloned().unwrap_or_default();
            roster.sort_by(|a, b| a.0.cmp(&b.0));
            let budget = opponent_budgets_remaining
                .get(index)
                .copied()
                .unwrap_or(config.starting_budget);

            if !unique_states.iter().any(|(known_roster, known_budget)| {
                *known_budget == budget && *known_roster == roster
            }) {
                unique_states.push((roster, budget));
            }
        }
        if unique_states.is_empty() {
            return economy;
        }

        let solved = focus_candidates
            .par_iter()
            .filter_map(|candidate| {
                let candidate_score = self.score_for(candidate)?;
                let candidate_x = score_to_x_vector(candidate_score, self.parameters);
                let generic_field = [([0.0; 9], 0usize)];

                let mut ordered_states = unique_states
                    .iter()
                    .filter_map(|(roster, budget)| {
                        let open_slots = self.team_size.saturating_sub(roster.len());
                        if open_slots == 0 {
                            return None;
                        }
                        let legal_max = max_legal_bid(*budget, open_slots, config.minimum_bid);
                        if legal_max < config.minimum_bid {
                            return None;
                        }

                        let roster_x = self.aggregate_x_scores(roster);
                        let before = self.evaluate_dynamic_state(roster_x, &generic_field);
                        let after = self.evaluate_dynamic_state(
                            add_vectors(roster_x, candidate_x),
                            &generic_field,
                        );

                        Some((
                            after.matchup_win_probability - before.matchup_win_probability,
                            legal_max,
                            roster.as_slice(),
                            *budget,
                        ))
                    })
                    .collect::<Vec<_>>();

                ordered_states.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

                let mut strongest_fair_bid = 0u16;
                for (_, legal_max, roster, budget) in ordered_states {
                    if legal_max <= strongest_fair_bid {
                        continue;
                    }

                    let fair = self.rational_bid_for_team(
                        roster,
                        budget,
                        candidate,
                        candidate_score,
                        all_candidates,
                        market_totals,
                        &economy,
                        pricing_strategies,
                        pricing_rankings,
                        config,
                    );
                    strongest_fair_bid = strongest_fair_bid.max(fair);
                }

                let required_to_win = if strongest_fair_bid == 0 {
                    config.minimum_bid
                } else {
                    strongest_fair_bid.saturating_add(1)
                };
                Some((candidate.clone(), required_to_win))
            })
            .collect::<Vec<_>>();

        for (player_id, price) in solved {
            economy.exact_prices.insert(player_id.clone(), price as f64);
            economy.prices.insert(player_id, price);
        }
        economy
    }

    /// Build a live competition market from actual opponent-specific rational
    /// max bids.
    ///
    /// For every remaining player and every DISTINCT opponent state we solve
    /// the same H-indifference problem used by BirdBoard's exact Max Bid:
    ///
    ///     fair_bid = max price where H(BUY) >= H(PASS)
    ///
    /// Future completion inside this pricing calculation uses the Preparation
    /// static market as the neutral price prior. This deliberately avoids the
    /// circular problem "competition price depends on competition price".
    ///
    /// The live acquisition price is then:
    ///
    ///     1 + strongest opponent fair bid
    ///
    /// i.e. the first whole-dollar bid that beats the strongest competing
    /// manager. Preparation MARKET remains completely unchanged.
    fn rational_opponent_market_economy(
        &self,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidates: &[PlayerId],
        market_totals: AuctionMarketTotals,
        strategies: &[RolloutStrategy],
        strategy_rankings: &[Vec<PlayerId>],
        config: AuctionConfig,
    ) -> MarketEconomy {
        // Neutral prior used ONLY while asking how much an opponent can
        // rationally afford to pay. It is the same static G/$ market shown in
        // Preparation and therefore gives the bid solver a non-circular future
        // price environment.
        let static_economy =
            self.estimate_market_economy_from_totals(candidates, None, market_totals, config);

        let pricing_count = strategies
            .len()
            .min(strategy_rankings.len())
            .min(COMPETITION_PRICING_STRATEGY_LIMIT);
        if pricing_count == 0 {
            return static_economy;
        }
        let pricing_strategies = &strategies[..pricing_count];
        let pricing_rankings = &strategy_rankings[..pricing_count];

        // Many teams are identical early in the draft (empty roster, $200).
        // Solve each unique state once. This is exact for the max-bid market
        // because duplicate states necessarily produce the same fair bid.
        let team_count = opponent_rosters.len().max(opponent_budgets_remaining.len());

        let mut unique_states = Vec::<(Vec<PlayerId>, u16)>::new();
        for index in 0..team_count {
            let mut roster = opponent_rosters.get(index).cloned().unwrap_or_default();
            roster.sort_by(|a, b| a.0.cmp(&b.0));
            let budget = opponent_budgets_remaining
                .get(index)
                .copied()
                .unwrap_or(config.starting_budget);

            if !unique_states.iter().any(|(known_roster, known_budget)| {
                *known_budget == budget && *known_roster == roster
            }) {
                unique_states.push((roster, budget));
            }
        }

        // No explicit opponents means there is no competition information.
        if unique_states.is_empty() {
            return static_economy;
        }

        let exact_and_rounded = candidates
            .par_iter()
            .filter_map(|candidate| {
                let static_score = self.score_for(candidate)?;
                let mut strongest_fair_bid = 0u16;

                // Evaluate likely strongest bidders first so the legal-max
                // pruning below becomes useful as quickly as possible.
                let candidate_x = score_to_x_vector(static_score, self.parameters);
                let generic_field = [([0.0; 9], 0usize)];

                let mut ordered_states = unique_states
                    .iter()
                    .filter_map(|(roster, budget)| {
                        let open_slots = self.team_size.saturating_sub(roster.len());
                        if open_slots == 0 {
                            return None;
                        }
                        let legal_max = max_legal_bid(*budget, open_slots, config.minimum_bid);
                        if legal_max < config.minimum_bid {
                            return None;
                        }

                        let roster_x = self.aggregate_x_scores(roster);
                        let before = self.evaluate_dynamic_state(roster_x, &generic_field);
                        let after = self.evaluate_dynamic_state(
                            add_vectors(roster_x, candidate_x),
                            &generic_field,
                        );
                        let immediate_gain =
                            after.matchup_win_probability - before.matchup_win_probability;

                        Some((immediate_gain, legal_max, roster.as_slice(), *budget))
                    })
                    .collect::<Vec<_>>();

                ordered_states.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

                for (_, legal_max, roster, budget) in ordered_states {
                    // This manager cannot beat the best fair bid already found,
                    // so an expensive H search cannot change the clearing price.
                    if legal_max <= strongest_fair_bid {
                        continue;
                    }

                    let fair = self.rational_bid_for_team(
                        roster,
                        budget,
                        candidate,
                        static_score,
                        candidates,
                        market_totals,
                        &static_economy,
                        pricing_strategies,
                        pricing_rankings,
                        config,
                    );
                    strongest_fair_bid = strongest_fair_bid.max(fair);
                }

                let required_to_win = if strongest_fair_bid == 0 {
                    config.minimum_bid
                } else {
                    strongest_fair_bid.saturating_add(1)
                };

                Some((candidate.clone(), required_to_win as f64, required_to_win))
            })
            .collect::<Vec<_>>();

        let mut exact_prices = HashMap::with_capacity(exact_and_rounded.len());
        let mut prices = HashMap::with_capacity(exact_and_rounded.len());
        for (player_id, exact, rounded) in exact_and_rounded {
            exact_prices.insert(player_id.clone(), exact);
            prices.insert(player_id, rounded);
        }

        MarketEconomy {
            // Replacement level is a property of the remaining player pool,
            // not of the bid-order statistic, so keep the static snapshot.
            snapshot: static_economy.snapshot,
            exact_prices,
            prices,
        }
    }

    /// Team-specific rational willingness to pay for one player.
    ///
    /// PASS assumes another manager buys the candidate at the neutral static
    /// market price. BUY is evaluated under the same static future-price prior,
    /// so the only thing being solved here is the bidder's own H indifference
    /// price. The result is therefore a true max bid rather than a transformed
    /// demand score.
    fn rational_bid_for_team(
        &self,
        roster: &[PlayerId],
        budget_remaining: u16,
        candidate: &PlayerId,
        candidate_score: &DurantScore,
        candidates: &[PlayerId],
        market_totals: AuctionMarketTotals,
        static_economy: &MarketEconomy,
        strategies: &[RolloutStrategy],
        strategy_rankings: &[Vec<PlayerId>],
        config: AuctionConfig,
    ) -> u16 {
        let open_slots = self.team_size.saturating_sub(roster.len());
        if open_slots == 0 {
            return 0;
        }

        let max_bid = max_legal_bid(budget_remaining, open_slots, config.minimum_bid);
        if max_bid < config.minimum_bid {
            return 0;
        }

        let team_sum = self.aggregate_x_scores(roster);
        let generic_field = [([0.0; 9], 0usize)];

        let expected_sale_price = static_economy
            .prices
            .get(candidate)
            .copied()
            .unwrap_or(config.minimum_bid);

        let pass_totals = AuctionMarketTotals {
            remaining_slots: market_totals.remaining_slots.saturating_sub(1),
            remaining_dollars: market_totals
                .remaining_dollars
                .saturating_sub(expected_sale_price as u32),
            max_legal_market_bid: market_totals.max_legal_market_bid,
        };
        let pass_economy = self.estimate_market_economy_from_totals(
            candidates,
            Some(candidate),
            pass_totals,
            config,
        );

        let Some(pass) = self
            .top_auction_rollouts(
                Some(candidate),
                team_sum,
                open_slots,
                budget_remaining,
                strategies,
                strategy_rankings,
                &pass_economy,
                &generic_field,
                config,
                1,
            )
            .into_iter()
            .next()
        else {
            return 0;
        };
        let pass_h = pass.evaluation.matchup_win_probability;

        let mut buy_cache = HashMap::<u16, AuctionRolloutResult>::new();
        let mut evaluate_buy = |price: u16| -> Option<AuctionRolloutResult> {
            if let Some(result) = buy_cache.get(&price) {
                return Some(result.clone());
            }

            let result = self.best_buy_rollout(
                candidate,
                candidate_score,
                team_sum,
                open_slots,
                budget_remaining,
                price,
                strategies,
                strategy_rankings,
                candidates,
                market_totals,
                None,
                &generic_field,
                config,
            )?;
            buy_cache.insert(price, result.clone());
            Some(result)
        };

        let minimum_price = config.minimum_bid;
        let cheap_h = evaluate_buy(minimum_price)
            .map(|result| result.evaluation.matchup_win_probability)
            .unwrap_or(0.0);
        if cheap_h + H_INDIFFERENCE_EPSILON < pass_h {
            return 0;
        }

        let max_h = evaluate_buy(max_bid)
            .map(|result| result.evaluation.matchup_win_probability)
            .unwrap_or(0.0);
        if max_h + H_INDIFFERENCE_EPSILON >= pass_h {
            return max_bid;
        }

        let mut low = minimum_price;
        let mut high = max_bid;
        let mut steps = 0u8;

        while low + 1 < high && steps < config.fair_price_iterations {
            let mid = low + (high - low) / 2;
            let mid_h = evaluate_buy(mid)
                .map(|result| result.evaluation.matchup_win_probability)
                .unwrap_or(0.0);

            if mid_h + H_INDIFFERENCE_EPSILON >= pass_h {
                low = mid;
            } else {
                high = mid;
            }
            steps += 1;
        }

        // The deterministic rollout has small local sawteeth. Keep the same
        // local verification used by the exact nomination Max Bid calculation.
        let start = low.saturating_sub(FAIR_PRICE_LOCAL_RADIUS);
        let end = high.saturating_add(FAIR_PRICE_LOCAL_RADIUS).min(max_bid);
        let mut best_good = low;
        for price in start.max(minimum_price)..=end {
            let h = evaluate_buy(price)
                .map(|result| result.evaluation.matchup_win_probability)
                .unwrap_or(0.0);
            if h + H_INDIFFERENCE_EPSILON >= pass_h {
                best_good = best_good.max(price);
            }
        }

        best_good
    }

    /// Reprice ONE future target after hypothetical purchases have removed
    /// alternatives from the market.
    ///
    /// This solves the same opponent H-indifference bid problem as the current
    /// LIVE market, but only for the player our selected j-plan wants next.
    /// That makes sequential future repricing practical without recursively
    /// rebuilding the entire 200+ player market after every hypothetical buy.
    fn rational_future_price_for_candidate(
        &self,
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        candidate: &PlayerId,
        candidates: &[PlayerId],
        market_totals: AuctionMarketTotals,
        strategies: &[RolloutStrategy],
        config: AuctionConfig,
    ) -> Option<u16> {
        let static_score = self.score_for(candidate)?;
        if candidates.is_empty() {
            return Some(config.minimum_bid);
        }

        let pricing_count = strategies.len().min(FUTURE_REPRICE_STRATEGY_LIMIT);
        if pricing_count == 0 {
            return Some(config.minimum_bid);
        }
        let pricing_strategies = &strategies[..pricing_count];
        let pricing_rankings = self.rank_available_by_strategy(candidates, pricing_strategies);

        let static_economy =
            self.estimate_market_economy_from_totals(candidates, None, market_totals, config);

        let team_count = opponent_rosters.len().max(opponent_budgets_remaining.len());

        let mut unique_states = Vec::<(Vec<PlayerId>, u16)>::new();
        for index in 0..team_count {
            let mut roster = opponent_rosters.get(index).cloned().unwrap_or_default();
            roster.sort_by(|a, b| a.0.cmp(&b.0));
            let budget = opponent_budgets_remaining
                .get(index)
                .copied()
                .unwrap_or(config.starting_budget);

            if !unique_states.iter().any(|(known_roster, known_budget)| {
                *known_budget == budget && *known_roster == roster
            }) {
                unique_states.push((roster, budget));
            }
        }

        if unique_states.is_empty() {
            return static_economy
                .prices
                .get(candidate)
                .copied()
                .or(Some(config.minimum_bid));
        }

        let candidate_x = score_to_x_vector(static_score, self.parameters);
        let generic_field = [([0.0; 9], 0usize)];

        let mut ordered_states = unique_states
            .iter()
            .filter_map(|(roster, budget)| {
                let open_slots = self.team_size.saturating_sub(roster.len());
                if open_slots == 0 {
                    return None;
                }

                let legal_max = max_legal_bid(*budget, open_slots, config.minimum_bid);
                if legal_max < config.minimum_bid {
                    return None;
                }

                let roster_x = self.aggregate_x_scores(roster);
                let before = self.evaluate_dynamic_state(roster_x, &generic_field);
                let after =
                    self.evaluate_dynamic_state(add_vectors(roster_x, candidate_x), &generic_field);

                Some((
                    after.matchup_win_probability - before.matchup_win_probability,
                    legal_max,
                    roster.as_slice(),
                    *budget,
                ))
            })
            .collect::<Vec<_>>();

        ordered_states.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

        let mut strongest_fair_bid = 0u16;
        for (_, legal_max, roster, budget) in ordered_states {
            if legal_max <= strongest_fair_bid {
                continue;
            }

            let fair = self.rational_bid_for_team(
                roster,
                budget,
                candidate,
                static_score,
                candidates,
                market_totals,
                &static_economy,
                pricing_strategies,
                &pricing_rankings,
                config,
            );

            strongest_fair_bid = strongest_fair_bid.max(fair);
        }

        Some(if strongest_fair_bid == 0 {
            config.minimum_bid
        } else {
            strongest_fair_bid.saturating_add(1)
        })
    }

    /// Refine one selected j-plan against the cheap static auction market.
    ///
    /// This is the Live-board shortlist version of the dynamic future-market
    /// refinement below. It sequentially removes purchases and updates the
    /// generic G/$ market, but never solves opponent-specific H-indifference
    /// bids. That keeps the broad Draft screen responsive while preserving the
    /// 10 intentional roster spots + minimum-bid flier tail.
    fn refine_rollout_with_static_future_market(
        &self,
        starting_x: [f64; 9],
        roster_size_at_start: usize,
        future_slots: usize,
        starting_budget: u16,
        strategy: &RolloutStrategy,
        initially_excluded: &[PlayerId],
        available_candidates: &[PlayerId],
        mut market_totals: AuctionMarketTotals,
        opponent_sums: &[([f64; 9], usize)],
        config: AuctionConfig,
    ) -> AuctionRolloutResult {
        let mut completed_x = starting_x;
        let mut budget = starting_budget;
        let mut future_spend = 0u16;
        let mut future_players = Vec::<PlayerId>::with_capacity(future_slots);
        let mut future_player_names = Vec::<String>::with_capacity(future_slots);
        let mut unavailable = initially_excluded.iter().cloned().collect::<HashSet<_>>();

        let intentional_target = INTENTIONAL_ROSTER_SLOTS.min(self.team_size);
        let intentional_remaining = intentional_target
            .saturating_sub(roster_size_at_start)
            .min(future_slots);
        let flier_slots = future_slots.saturating_sub(intentional_remaining);

        for intentional_index in 0..intentional_remaining {
            let slots_after_this_purchase = future_slots.saturating_sub(intentional_index + 1);
            let reserve_after =
                (slots_after_this_purchase as u32).saturating_mul(config.minimum_bid as u32);
            let max_spend = (budget as u32)
                .saturating_sub(reserve_after)
                .min(u16::MAX as u32) as u16;

            let remaining_candidates = available_candidates
                .iter()
                .filter(|player_id| !unavailable.contains(*player_id))
                .cloned()
                .collect::<Vec<_>>();
            if remaining_candidates.is_empty() {
                break;
            }
            let economy = self.estimate_market_economy_from_totals(
                &remaining_candidates,
                None,
                market_totals,
                config,
            );

            let mut ranked = remaining_candidates
                .iter()
                .filter_map(|player_id| {
                    let x = self.x_score_for(player_id)?;
                    Some((player_id.clone(), x, weighted_x_score(x, strategy.weights)))
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|a, b| b.2.total_cmp(&a.2));

            let chosen = ranked.into_iter().find_map(|(player_id, x, _)| {
                let price = economy
                    .prices
                    .get(&player_id)
                    .copied()
                    .unwrap_or(config.minimum_bid);
                (price <= max_spend).then_some((player_id, x, price))
            });

            let Some((player_id, x, price)) = chosen else {
                break;
            };

            completed_x = add_vectors(completed_x, x);
            budget = budget.saturating_sub(price);
            future_spend = future_spend.saturating_add(price);
            unavailable.insert(player_id.clone());
            if let Some(score) = self.score_for(&player_id) {
                future_player_names.push(format!("{} (${price})", score.player_name));
            }
            future_players.push(player_id);
            market_totals.remaining_slots = market_totals.remaining_slots.saturating_sub(1);
            market_totals.remaining_dollars =
                market_totals.remaining_dollars.saturating_sub(price as u32);
        }

        let filled_intentional = future_players.len();
        let unresolved_intentional = intentional_remaining.saturating_sub(filled_intentional);
        let total_fliers = flier_slots.saturating_add(unresolved_intentional);

        for _ in 0..total_fliers {
            if budget < config.minimum_bid {
                break;
            }
            let remaining_candidates = available_candidates
                .iter()
                .filter(|player_id| !unavailable.contains(*player_id))
                .cloned()
                .collect::<Vec<_>>();
            let replacement_x = self
                .estimate_market_economy_from_totals(
                    &remaining_candidates,
                    None,
                    market_totals,
                    config,
                )
                .snapshot
                .replacement_x_scores;
            completed_x = add_vectors(completed_x, replacement_x);
            budget = budget.saturating_sub(config.minimum_bid);
            future_spend = future_spend.saturating_add(config.minimum_bid);
            future_player_names.push(format!("Flier / replacement (${})", config.minimum_bid));
            market_totals.remaining_slots = market_totals.remaining_slots.saturating_sub(1);
            market_totals.remaining_dollars = market_totals
                .remaining_dollars
                .saturating_sub(config.minimum_bid as u32);
        }

        AuctionRolloutResult {
            strategy: strategy.clone(),
            evaluation: self.evaluate_dynamic_state(completed_x, opponent_sums),
            future_players,
            future_player_names,
            future_spend,
            budget_left: budget,
        }
    }

    /// Refine one already-selected j-plan with sequential competition repricing.
    ///
    /// The broad strategy search still runs against the current-state rational
    /// market. Once it finds the best j for a candidate, this pass walks that
    /// exact plan forward and recalculates the opponent clearing price for the
    /// NEXT desired player after each hypothetical purchase.
    ///
    /// This captures the important auction feedback:
    ///
    ///     buy Jokic -> elite alternatives disappear -> opponents' rational
    ///     bids for SGA/Wemby/etc. can change before our next purchase.
    ///
    /// Only the first 10 total roster spots are intentional. Any remaining
    /// spots are modeled conservatively as generic $1 fliers.
    fn refine_rollout_with_dynamic_future_market(
        &self,
        starting_x: [f64; 9],
        roster_size_at_start: usize,
        future_slots: usize,
        starting_budget: u16,
        strategy: &RolloutStrategy,
        initially_excluded: &[PlayerId],
        available_candidates: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        opponent_budgets_remaining: &[u16],
        mut market_totals: AuctionMarketTotals,
        pricing_strategies: &[RolloutStrategy],
        opponent_sums: &[([f64; 9], usize)],
        config: AuctionConfig,
    ) -> AuctionRolloutResult {
        let mut completed_x = starting_x;
        let mut budget = starting_budget;
        let mut future_spend = 0u16;
        let mut future_players = Vec::<PlayerId>::with_capacity(future_slots);
        let mut future_player_names = Vec::<String>::with_capacity(future_slots);

        let mut unavailable = initially_excluded.iter().cloned().collect::<HashSet<_>>();

        let intentional_target = INTENTIONAL_ROSTER_SLOTS.min(self.team_size);
        let intentional_remaining = intentional_target
            .saturating_sub(roster_size_at_start)
            .min(future_slots);
        let flier_slots = future_slots.saturating_sub(intentional_remaining);

        for intentional_index in 0..intentional_remaining {
            let slots_after_this_purchase = future_slots.saturating_sub(intentional_index + 1);
            let reserve_after =
                (slots_after_this_purchase as u32).saturating_mul(config.minimum_bid as u32);
            let max_spend = (budget as u32)
                .saturating_sub(reserve_after)
                .min(u16::MAX as u32) as u16;

            let mut ranked = available_candidates
                .iter()
                .filter(|player_id| !unavailable.contains(*player_id))
                .filter_map(|player_id| {
                    let x = self.x_score_for(player_id)?;
                    Some((player_id.clone(), x, weighted_x_score(x, strategy.weights)))
                })
                .collect::<Vec<_>>();
            ranked.sort_by(|a, b| b.2.total_cmp(&a.2));

            let mut chosen: Option<(PlayerId, [f64; 9], u16)> = None;

            // Reprice only players we actually consider buying, in j-order.
            // If the first target is now too expensive, continue down the plan.
            for (player_id, x, _) in ranked {
                let remaining_candidates = available_candidates
                    .iter()
                    .filter(|candidate_id| {
                        !unavailable.contains(*candidate_id) && **candidate_id != player_id
                    })
                    .cloned()
                    .chain(std::iter::once(player_id.clone()))
                    .collect::<Vec<_>>();

                let Some(price) = self.rational_future_price_for_candidate(
                    opponent_rosters,
                    opponent_budgets_remaining,
                    &player_id,
                    &remaining_candidates,
                    market_totals,
                    pricing_strategies,
                    config,
                ) else {
                    continue;
                };

                if price <= max_spend {
                    chosen = Some((player_id, x, price));
                    break;
                }
            }

            let Some((player_id, x, price)) = chosen else {
                // If no intentional target is affordable, stop pretending we
                // can force another meaningful auction win. The remaining
                // places become minimum-bid fliers.
                break;
            };

            completed_x = add_vectors(completed_x, x);
            budget = budget.saturating_sub(price);
            future_spend = future_spend.saturating_add(price);
            unavailable.insert(player_id.clone());

            if let Some(score) = self.score_for(&player_id) {
                future_player_names.push(format!("{} (${price})", score.player_name));
            }
            future_players.push(player_id);

            market_totals.remaining_slots = market_totals.remaining_slots.saturating_sub(1);
            market_totals.remaining_dollars =
                market_totals.remaining_dollars.saturating_sub(price as u32);
        }

        // Everything after the intentional core is deliberately conservative.
        // These are roster fliers, not optimized category-engineering pieces.
        let filled_intentional = future_players.len();
        let unresolved_intentional = intentional_remaining.saturating_sub(filled_intentional);
        let total_fliers = flier_slots.saturating_add(unresolved_intentional);

        for _ in 0..total_fliers {
            if budget < config.minimum_bid {
                break;
            }
            completed_x = add_vectors(
                completed_x,
                self.estimate_market_economy_from_totals(
                    available_candidates,
                    None,
                    market_totals,
                    config,
                )
                .snapshot
                .replacement_x_scores,
            );
            budget = budget.saturating_sub(config.minimum_bid);
            future_spend = future_spend.saturating_add(config.minimum_bid);
            future_player_names.push(format!("Flier / replacement (${})", config.minimum_bid));
        }

        AuctionRolloutResult {
            strategy: strategy.clone(),
            evaluation: self.evaluate_dynamic_state(completed_x, opponent_sums),
            future_players,
            future_player_names,
            future_spend,
            budget_left: budget,
        }
    }

    /// Reuse an already-calibrated live competition market after one
    /// hypothetical sale. Actual opponent max bids are recalculated whenever
    /// the REAL draft state changes; inside one projected path we avoid a
    /// recursive market-within-market solve.
    fn competition_economy_after_sale(
        &self,
        base: &MarketEconomy,
        sold_player: &PlayerId,
        totals: AuctionMarketTotals,
        config: AuctionConfig,
    ) -> MarketEconomy {
        let mut economy = base.clone();
        economy.exact_prices.remove(sold_player);
        economy.prices.remove(sold_player);

        economy.snapshot.remaining_roster_slots = totals.remaining_slots;
        economy.snapshot.remaining_league_dollars = totals.remaining_dollars;
        economy.snapshot.reserved_minimum_dollars =
            (totals.remaining_slots as u32).saturating_mul(config.minimum_bid as u32);
        economy.snapshot.discretionary_dollars = totals
            .remaining_dollars
            .saturating_sub(economy.snapshot.reserved_minimum_dollars);

        economy
    }

    fn estimate_market_economy_from_totals(
        &self,
        candidates: &[PlayerId],
        excluded_player: Option<&PlayerId>,
        totals: AuctionMarketTotals,
        config: AuctionConfig,
    ) -> MarketEconomy {
        let mut available = candidates
            .iter()
            .filter(|player_id| excluded_player != Some(*player_id))
            .filter_map(|player_id| {
                let score = self.score_for(player_id)?;
                Some((player_id.clone(), score.total))
            })
            .collect::<Vec<_>>();
        available.sort_by(|a, b| b.1.total_cmp(&a.1));

        let draftable_count = totals.remaining_slots.min(available.len());
        let replacement_g_score = if available.len() > draftable_count {
            available[draftable_count].1
        } else {
            available.last().map(|(_, g)| *g).unwrap_or(0.0)
        };

        let cutoff = draftable_count.min(available.len());
        let replacement_band = self.league_teams().max(1);
        let band_start = cutoff.saturating_sub(replacement_band / 2);
        let band_end = (band_start + replacement_band).min(available.len());
        let mut replacement_x_scores = [0.0; 9];
        let mut replacement_count = 0usize;
        for (player_id, _) in &available[band_start..band_end] {
            if let Some(x) = self.x_score_for(player_id) {
                replacement_x_scores = add_vectors(replacement_x_scores, x);
                replacement_count += 1;
            }
        }
        if replacement_count > 0 {
            for value in &mut replacement_x_scores {
                *value /= replacement_count as f64;
            }
        }

        let reserved_minimum_dollars =
            (totals.remaining_slots as u32).saturating_mul(config.minimum_bid as u32);
        let discretionary_dollars = totals
            .remaining_dollars
            .saturating_sub(reserved_minimum_dollars);

        let total_g_above_replacement = available
            .iter()
            .take(draftable_count)
            .map(|(_, g)| (g - replacement_g_score).max(0.0))
            .sum::<f64>();

        let g_above_replacement_per_dollar = if discretionary_dollars == 0 {
            0.0
        } else {
            total_g_above_replacement / discretionary_dollars as f64
        };
        let dollars_per_g_above_replacement = if total_g_above_replacement <= f64::EPSILON {
            0.0
        } else {
            discretionary_dollars as f64 / total_g_above_replacement
        };

        let mut exact_prices = HashMap::new();
        let mut prices = HashMap::new();
        let minimum = config.minimum_bid as f64;
        let market_cap = totals.max_legal_market_bid.max(config.minimum_bid) as f64;

        for (rank, (player_id, g)) in available.iter().enumerate() {
            let exact = if rank < draftable_count && total_g_above_replacement > f64::EPSILON {
                let weight = (g - replacement_g_score).max(0.0);
                (minimum + discretionary_dollars as f64 * weight / total_g_above_replacement)
                    .clamp(minimum, market_cap)
            } else {
                minimum
            };

            exact_prices.insert(player_id.clone(), exact);
            prices.insert(
                player_id.clone(),
                exact
                    .round()
                    .clamp(config.minimum_bid as f64, u16::MAX as f64) as u16,
            );
        }

        MarketEconomy {
            snapshot: MarketEconomySnapshot {
                remaining_roster_slots: totals.remaining_slots,
                remaining_league_dollars: totals.remaining_dollars,
                reserved_minimum_dollars,
                discretionary_dollars,
                replacement_g_score,
                replacement_x_scores,
                total_g_above_replacement,
                g_above_replacement_per_dollar,
                dollars_per_g_above_replacement,
            },
            exact_prices,
            prices,
        }
    }

    fn best_buy_rollout(
        &self,
        candidate: &PlayerId,
        candidate_score: &DurantScore,
        own_sum: [f64; 9],
        own_open_slots: usize,
        own_budget_remaining: u16,
        candidate_price: u16,
        strategies: &[RolloutStrategy],
        strategy_rankings: &[Vec<PlayerId>],
        candidates: &[PlayerId],
        market_totals: AuctionMarketTotals,
        competition_pricing: Option<&MarketEconomy>,
        opponent_sums: &[([f64; 9], usize)],
        config: AuctionConfig,
    ) -> Option<AuctionRolloutResult> {
        if own_open_slots == 0 {
            return None;
        }
        let max_bid = max_legal_bid(own_budget_remaining, own_open_slots, config.minimum_bid);
        if candidate_price < config.minimum_bid || candidate_price > max_bid {
            return None;
        }

        let candidate_x = score_to_x_vector(candidate_score, self.parameters);
        let with_candidate = add_vectors(own_sum, candidate_x);
        let budget_after = own_budget_remaining.saturating_sub(candidate_price);
        let future_slots = own_open_slots.saturating_sub(1);
        let post_purchase_totals = AuctionMarketTotals {
            remaining_slots: market_totals.remaining_slots.saturating_sub(1),
            remaining_dollars: market_totals
                .remaining_dollars
                .saturating_sub(candidate_price as u32),
            max_legal_market_bid: market_totals.max_legal_market_bid,
        };
        let post_purchase_economy = if let Some(competition_economy) = competition_pricing {
            self.competition_economy_after_sale(
                competition_economy,
                candidate,
                post_purchase_totals,
                config,
            )
        } else {
            self.estimate_market_economy_from_totals(
                candidates,
                Some(candidate),
                post_purchase_totals,
                config,
            )
        };

        self.top_auction_rollouts(
            Some(candidate),
            with_candidate,
            future_slots,
            budget_after,
            strategies,
            strategy_rankings,
            &post_purchase_economy,
            opponent_sums,
            config,
            1,
        )
        .into_iter()
        .next()
    }

    /// Evaluate one auction rollout without retaining its player path. This is
    /// used by the quick j census to keep RAM and allocator churn tiny.
    fn auction_rollout_summary(
        &self,
        excluded_player: Option<&PlayerId>,
        starting_x: [f64; 9],
        future_slots: usize,
        starting_budget: u16,
        strategy: &RolloutStrategy,
        ranked_players: &[PlayerId],
        economy: &MarketEconomy,
        opponent_sums: &[([f64; 9], usize)],
        config: AuctionConfig,
    ) -> Option<AuctionRolloutResult> {
        let minimum_required = (future_slots as u32).saturating_mul(config.minimum_bid as u32);
        if (starting_budget as u32) < minimum_required {
            return None;
        }

        let mut completed_x = starting_x;
        let mut budget = starting_budget;
        let mut future_spend = 0u16;
        let mut cursor = 0usize;

        for slot in 0..future_slots {
            let slots_after = future_slots.saturating_sub(slot + 1);
            let reserve_after = (slots_after as u32).saturating_mul(config.minimum_bid as u32);
            let max_spend = (budget as u32)
                .saturating_sub(reserve_after)
                .min(u16::MAX as u32) as u16;

            let mut chosen: Option<([f64; 9], u16)> = None;
            while cursor < ranked_players.len() {
                let player_id = &ranked_players[cursor];
                cursor += 1;

                if excluded_player == Some(player_id) {
                    continue;
                }
                let Some(x) = self.x_score_for(player_id) else {
                    continue;
                };
                let price = economy
                    .prices
                    .get(player_id)
                    .copied()
                    .unwrap_or(config.minimum_bid);
                if price <= max_spend {
                    chosen = Some((x, price));
                    break;
                }
                // Budget headroom never increases during a completion, so an
                // unaffordable player cannot become affordable later.
            }

            match chosen {
                Some((x, price)) => {
                    completed_x = add_vectors(completed_x, x);
                    budget = budget.saturating_sub(price);
                    future_spend = future_spend.saturating_add(price);
                }
                None => {
                    if budget < config.minimum_bid {
                        return None;
                    }
                    completed_x = add_vectors(completed_x, economy.snapshot.replacement_x_scores);
                    budget = budget.saturating_sub(config.minimum_bid);
                    future_spend = future_spend.saturating_add(config.minimum_bid);
                }
            }
        }

        Some(AuctionRolloutResult {
            strategy: strategy.clone(),
            evaluation: self.evaluate_dynamic_state(completed_x, opponent_sums),
            future_players: Vec::new(),
            future_player_names: Vec::new(),
            future_spend,
            budget_left: budget,
        })
    }

    fn top_auction_rollouts(
        &self,
        excluded_player: Option<&PlayerId>,
        starting_x: [f64; 9],
        future_slots: usize,
        starting_budget: u16,
        strategies: &[RolloutStrategy],
        strategy_rankings: &[Vec<PlayerId>],
        economy: &MarketEconomy,
        opponent_sums: &[([f64; 9], usize)],
        config: AuctionConfig,
        keep: usize,
    ) -> Vec<AuctionRolloutResult> {
        if keep == 0 {
            return Vec::new();
        }

        if future_slots == 0 || strategies.is_empty() {
            return vec![AuctionRolloutResult {
                strategy: RolloutStrategy::balanced(),
                evaluation: self.evaluate_dynamic_state(starting_x, opponent_sums),
                future_players: Vec::new(),
                future_player_names: Vec::new(),
                future_spend: 0,
                budget_left: starting_budget,
            }];
        }

        let minimum_required = (future_slots as u32).saturating_mul(config.minimum_bid as u32);
        if (starting_budget as u32) < minimum_required {
            return Vec::new();
        }

        let mut best = Vec::<AuctionRolloutResult>::with_capacity(keep + 1);

        for (strategy, ranked_players) in strategies.iter().zip(strategy_rankings.iter()) {
            let mut completed_x = starting_x;
            let mut budget = starting_budget;
            let mut future_spend = 0u16;
            let mut future_players = Vec::with_capacity(future_slots);
            let mut future_player_names = Vec::with_capacity(future_slots);
            let mut cursor = 0usize;

            for slot in 0..future_slots {
                let slots_after = future_slots.saturating_sub(slot + 1);
                let reserve_after = (slots_after as u32).saturating_mul(config.minimum_bid as u32);
                let max_spend = (budget as u32)
                    .saturating_sub(reserve_after)
                    .min(u16::MAX as u32) as u16;

                let mut chosen: Option<(&PlayerId, [f64; 9], u16)> = None;

                while cursor < ranked_players.len() {
                    let player_id = &ranked_players[cursor];
                    cursor += 1;

                    if excluded_player == Some(player_id) {
                        continue;
                    }

                    let Some(x) = self.x_score_for(player_id) else {
                        continue;
                    };

                    let price = economy
                        .prices
                        .get(player_id)
                        .copied()
                        .unwrap_or(config.minimum_bid);

                    if price <= max_spend {
                        chosen = Some((player_id, x, price));
                        break;
                    }
                    // max_spend can only stay flat or decrease from here, so a
                    // player unaffordable now will never become affordable later.
                }

                let Some((player_id, x, price)) = chosen else {
                    if budget < config.minimum_bid {
                        break;
                    }
                    completed_x = add_vectors(completed_x, economy.snapshot.replacement_x_scores);
                    budget = budget.saturating_sub(config.minimum_bid);
                    future_spend = future_spend.saturating_add(config.minimum_bid);
                    future_player_names.push("Replacement-level".to_string());
                    continue;
                };

                completed_x = add_vectors(completed_x, x);
                budget = budget.saturating_sub(price);
                future_spend = future_spend.saturating_add(price);
                future_players.push(player_id.clone());
                if let Some(score) = self.score_for(player_id) {
                    future_player_names.push(format!("{} (${price})", score.player_name));
                }
            }

            let result = AuctionRolloutResult {
                strategy: strategy.clone(),
                evaluation: self.evaluate_dynamic_state(completed_x, opponent_sums),
                future_players,
                future_player_names,
                future_spend,
                budget_left: budget,
            };

            best.push(result);
            best.sort_by(|a, b| {
                b.evaluation
                    .matchup_win_probability
                    .total_cmp(&a.evaluation.matchup_win_probability)
                    .then_with(|| {
                        b.evaluation
                            .expected_categories
                            .total_cmp(&a.evaluation.expected_categories)
                    })
                    .then_with(|| a.future_spend.cmp(&b.future_spend))
            });
            best.truncate(keep);
        }

        best
    }

    fn rank_available_by_strategy(
        &self,
        candidates: &[PlayerId],
        strategies: &[RolloutStrategy],
    ) -> Vec<Vec<PlayerId>> {
        strategies
            .par_iter()
            .map(|strategy| {
                let mut ranked = candidates
                    .iter()
                    .filter_map(|player_id| {
                        let x = self.x_score_for(player_id)?;
                        Some((player_id.clone(), weighted_x_score(x, strategy.weights)))
                    })
                    .collect::<Vec<_>>();

                ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
                ranked.into_iter().map(|(player_id, _)| player_id).collect()
            })
            .collect()
    }

    fn top_rollouts_from_state(
        &self,
        starting_x: [f64; 9],
        future_slots: usize,
        strategies: &[RolloutStrategy],
        strategy_rankings: &[Vec<PlayerId>],
        opponent_sums: &[([f64; 9], usize)],
        keep: usize,
    ) -> Vec<RolloutResult> {
        if keep == 0 {
            return Vec::new();
        }

        if future_slots == 0 || strategies.is_empty() {
            return vec![RolloutResult {
                strategy: RolloutStrategy::balanced(),
                evaluation: self.evaluate_dynamic_state(starting_x, opponent_sums),
                future_players: Vec::new(),
                future_player_names: Vec::new(),
            }];
        }

        let mut best = Vec::<RolloutResult>::with_capacity(keep + 1);

        for (strategy, ranked_players) in strategies.iter().zip(strategy_rankings.iter()) {
            let mut completed_x = starting_x;
            let mut future_players = Vec::with_capacity(future_slots);
            let mut future_player_names = Vec::with_capacity(future_slots);

            let valid_ranked = ranked_players
                .iter()
                .filter(|player_id| self.x_score_for(player_id).is_some())
                .collect::<Vec<_>>();

            let mut used_indices = HashSet::new();

            for _ in 0..future_slots {
                if valid_ranked.is_empty() {
                    break;
                }

                // Auction semantics: there is no draft turn / 13-team stride.
                // Take the highest-ranked remaining target for this strategy.
                let Some(index) = (0..valid_ranked.len())
                    .find(|candidate_index| !used_indices.contains(candidate_index))
                else {
                    break;
                };

                used_indices.insert(index);
                let player_id = valid_ranked[index];
                let Some(x) = self.x_score_for(player_id) else {
                    continue;
                };

                completed_x = add_vectors(completed_x, x);
                future_players.push(player_id.clone());

                if let Some(score) = self.score_for(player_id) {
                    future_player_names.push(score.player_name.clone());
                }
            }

            let result = RolloutResult {
                strategy: strategy.clone(),
                evaluation: self.evaluate_dynamic_state(completed_x, opponent_sums),
                future_players,
                future_player_names,
            };

            best.push(result);
            best.sort_by(|a, b| {
                b.evaluation
                    .matchup_win_probability
                    .total_cmp(&a.evaluation.matchup_win_probability)
                    .then_with(|| {
                        b.evaluation
                            .expected_categories
                            .total_cmp(&a.evaluation.expected_categories)
                    })
            });
            best.truncate(keep);
        }

        best
    }

    fn top_rollouts_for_candidate(
        &self,
        candidate: &PlayerId,
        with_candidate: [f64; 9],
        future_slots: usize,
        strategies: &[RolloutStrategy],
        strategy_rankings: &[Vec<PlayerId>],
        opponent_sums: &[([f64; 9], usize)],
        keep: usize,
    ) -> Vec<RolloutResult> {
        if keep == 0 {
            return Vec::new();
        }

        if future_slots == 0 || strategies.is_empty() {
            return vec![RolloutResult {
                strategy: RolloutStrategy::balanced(),
                evaluation: self.evaluate_dynamic_state(with_candidate, opponent_sums),
                future_players: Vec::new(),
                future_player_names: Vec::new(),
            }];
        }

        let mut best = Vec::<RolloutResult>::with_capacity(keep + 1);

        for (strategy, ranked_players) in strategies.iter().zip(strategy_rankings.iter()) {
            let mut completed_x = with_candidate;
            let mut future_players = Vec::with_capacity(future_slots);
            let mut future_player_names = Vec::with_capacity(future_slots);

            let valid_ranked = ranked_players
                .iter()
                .filter(|player_id| *player_id != candidate)
                .filter(|player_id| self.x_score_for(player_id).is_some())
                .collect::<Vec<_>>();

            let mut used_indices = HashSet::new();

            for _ in 0..future_slots {
                if valid_ranked.is_empty() {
                    break;
                }

                // Auction semantics: there is no turn-order stride. Take the
                // highest-ranked remaining target for this strategy.
                let Some(index) = (0..valid_ranked.len())
                    .find(|candidate_index| !used_indices.contains(candidate_index))
                else {
                    break;
                };

                used_indices.insert(index);
                let player_id = valid_ranked[index];
                let Some(x) = self.x_score_for(player_id) else {
                    continue;
                };

                completed_x = add_vectors(completed_x, x);
                future_players.push(player_id.clone());

                if let Some(score) = self.score_for(player_id) {
                    future_player_names.push(score.player_name.clone());
                }
            }

            let result = RolloutResult {
                strategy: strategy.clone(),
                evaluation: self.evaluate_dynamic_state(completed_x, opponent_sums),
                future_players,
                future_player_names,
            };

            best.push(result);
            best.sort_by(|a, b| {
                b.evaluation
                    .matchup_win_probability
                    .total_cmp(&a.evaluation.matchup_win_probability)
                    .then_with(|| {
                        b.evaluation
                            .expected_categories
                            .total_cmp(&a.evaluation.expected_categories)
                    })
            });
            best.truncate(keep);
        }

        best
    }

    fn league_teams(&self) -> usize {
        if self.team_size == 0 {
            1
        } else {
            (self.reference_size / self.team_size).max(1)
        }
    }

    fn aggregate_x_scores(&self, roster: &[PlayerId]) -> [f64; 9] {
        roster.iter().fold([0.0; 9], |acc, player_id| {
            match self.x_score_for(player_id) {
                Some(x) => add_vectors(acc, x),
                None => acc,
            }
        })
    }

    fn evaluate_dynamic_state(
        &self,
        own_x: [f64; 9],
        opponent_sums: &[([f64; 9], usize)],
    ) -> DynamicStateEvaluation {
        let mut average_category_probabilities = [0.0; 9];
        let mut average_expected_categories = 0.0;
        let mut average_matchup_probability = 0.0;

        for (opponent_x, known_opponent_players) in opponent_sums {
            let unknown_opponent_slots = self
                .team_size
                .saturating_sub((*known_opponent_players).min(self.team_size));

            let mut category_probabilities = [0.0; 9];

            for category in 0..9 {
                let mean_difference = own_x[category] - opponent_x[category];

                // In the X-score basis, one full roster contributes variance
                // N for each team from scoring-period noise. Rosenof adds
                // player-to-player variance for the opponent's unknown future
                // picks, assumed random around their expected means.
                let variance = 2.0 * self.team_size as f64
                    + unknown_opponent_slots as f64 * self.x_variance[category];

                let std_dev = variance.max(f64::EPSILON).sqrt();
                category_probabilities[category] = normal_cdf(mean_difference / std_dev);
                average_category_probabilities[category] +=
                    category_probabilities[category] / opponent_sums.len() as f64;
            }

            let expected_categories = category_probabilities.iter().sum::<f64>();
            let matchup_probability = most_categories_probability(category_probabilities);

            average_expected_categories += expected_categories / opponent_sums.len() as f64;
            average_matchup_probability += matchup_probability / opponent_sums.len() as f64;
        }

        DynamicStateEvaluation {
            category_win_probabilities: average_category_probabilities,
            expected_categories: average_expected_categories,
            matchup_win_probability: average_matchup_probability,
        }
    }
}

fn fingerprint_model(
    scores: &[DurantScore],
    parameters: DurantParameters,
    reference_size: usize,
    team_size: usize,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    reference_size.hash(&mut hasher);
    team_size.hash(&mut hasher);

    for score in scores {
        score.player_id.hash(&mut hasher);
        for value in [
            score.field_goal,
            score.free_throw,
            score.threes,
            score.points,
            score.rebounds,
            score.assists,
            score.steals,
            score.blocks,
            score.turnovers,
            score.total,
        ] {
            value.to_bits().hash(&mut hasher);
        }
    }

    for value in [
        parameters.points.mean,
        parameters.points.sigma,
        parameters.points.tau,
        parameters.threes.mean,
        parameters.threes.sigma,
        parameters.threes.tau,
        parameters.rebounds.mean,
        parameters.rebounds.sigma,
        parameters.rebounds.tau,
        parameters.assists.mean,
        parameters.assists.sigma,
        parameters.assists.tau,
        parameters.steals.mean,
        parameters.steals.sigma,
        parameters.steals.tau,
        parameters.blocks.mean,
        parameters.blocks.sigma,
        parameters.blocks.tau,
        parameters.turnovers.mean,
        parameters.turnovers.sigma,
        parameters.turnovers.tau,
        parameters.field_goal.mean_attempts,
        parameters.field_goal.mean_rate,
        parameters.field_goal.sigma_rate,
        parameters.field_goal.tau_rate,
        parameters.free_throw.mean_attempts,
        parameters.free_throw.mean_rate,
        parameters.free_throw.sigma_rate,
        parameters.free_throw.tau_rate,
    ] {
        value.to_bits().hash(&mut hasher);
    }

    hasher.finish()
}

// -----------------------------------------------------------------------------
// Manual projection overrides
// -----------------------------------------------------------------------------

fn load_manual_projection_overrides(path: &str) -> Result<Vec<ManualProjectionOverride>> {
    let path = std::path::Path::new(path);

    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut reader = csv::ReaderBuilder::new()
        .trim(csv::Trim::All)
        .from_path(path)?;

    let mut projections = Vec::new();
    let mut seen_ids = HashSet::new();

    for row in reader.deserialize::<ManualProjectionOverride>() {
        let projection = row?;

        if !projection.enabled {
            continue;
        }

        validate_manual_projection(&projection)?;

        if !seen_ids.insert(projection.player_id.clone()) {
            bail!(
                "duplicate player_id {} in {}",
                projection.player_id.0,
                path.display()
            );
        }

        projections.push(projection);
    }

    Ok(projections)
}

fn normalized_player_name(name: &str) -> String {
    name.chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

fn canonicalize_manual_projection_overrides(
    projections: &mut Vec<ManualProjectionOverride>,
    stats: &StatsBundle,
) {
    let canonical_by_name = stats
        .players
        .iter()
        .map(|player| {
            (
                normalized_player_name(&player.player_name),
                player.player_id.clone(),
            )
        })
        .collect::<HashMap<_, _>>();

    // Canonicalize legacy IDs by player name, then collapse duplicate rows.
    // Last row wins, matching normal override-file expectations.
    let mut deduped = Vec::<ManualProjectionOverride>::with_capacity(projections.len());
    let mut index_by_id = HashMap::<PlayerId, usize>::new();

    for mut projection in projections.drain(..) {
        if let Some(canonical_id) =
            canonical_by_name.get(&normalized_player_name(&projection.player_name))
        {
            projection.player_id = canonical_id.clone();
        }

        if let Some(index) = index_by_id.get(&projection.player_id).copied() {
            deduped[index] = projection;
        } else {
            index_by_id.insert(projection.player_id.clone(), deduped.len());
            deduped.push(projection);
        }
    }

    *projections = deduped;
}

fn validate_manual_projection(projection: &ManualProjectionOverride) -> Result<()> {
    if projection.player_id.0.trim().is_empty() {
        bail!("manual DURANT projection has an empty player_id");
    }

    if projection.player_name.trim().is_empty() {
        bail!(
            "manual DURANT projection {} has an empty player_name",
            projection.player_id.0
        );
    }

    for (name, value) in [
        ("fgm_pg", projection.fgm_pg),
        ("fga_pg", projection.fga_pg),
        ("ftm_pg", projection.ftm_pg),
        ("fta_pg", projection.fta_pg),
        ("threes_pg", projection.threes_pg),
        ("points_pg", projection.points_pg),
        ("rebounds_pg", projection.rebounds_pg),
        ("assists_pg", projection.assists_pg),
        ("steals_pg", projection.steals_pg),
        ("blocks_pg", projection.blocks_pg),
        ("turnovers_pg", projection.turnovers_pg),
    ] {
        if !value.is_finite() || value < 0.0 {
            bail!(
                "manual DURANT projection {} has invalid {}={}",
                projection.player_name,
                name,
                value
            );
        }
    }

    if projection.fgm_pg > projection.fga_pg + f64::EPSILON {
        bail!(
            "manual DURANT projection {} has FGM > FGA",
            projection.player_name
        );
    }

    if projection.ftm_pg > projection.fta_pg + f64::EPSILON {
        bail!(
            "manual DURANT projection {} has FTM > FTA",
            projection.player_name
        );
    }

    Ok(())
}

fn mean_active_games_per_week(
    reference_players: &[PlayerId],
    weekly_by_player: &HashMap<PlayerId, Vec<&PlayerWeeklyStats>>,
) -> f64 {
    let games = reference_players
        .iter()
        .filter_map(|player_id| weekly_by_player.get(player_id))
        .flat_map(|weeks| weeks.iter())
        .map(|week| week.games as f64)
        .collect::<Vec<_>>();

    if games.is_empty() { 3.5 } else { mean(&games) }
}

fn score_stat_projection(
    player: &PlayerNineCatStats,
    games_per_active_week: f64,
    parameters: DurantParameters,
) -> DurantScore {
    let weekly = |per_game: f64| per_game * games_per_active_week;

    let field_goal = projected_percentage_score(
        weekly(player.fgm_pg),
        weekly(player.fga_pg),
        parameters.field_goal,
    );
    let free_throw = projected_percentage_score(
        weekly(player.ftm_pg),
        weekly(player.fta_pg),
        parameters.free_throw,
    );
    let threes = counting_score(weekly(player.threes_pg), parameters.threes, false);
    let points = counting_score(weekly(player.points_pg), parameters.points, false);
    let rebounds = counting_score(weekly(player.rebounds_pg), parameters.rebounds, false);
    let assists = counting_score(weekly(player.assists_pg), parameters.assists, false);
    let steals = counting_score(weekly(player.steals_pg), parameters.steals, false);
    let blocks = counting_score(weekly(player.blocks_pg), parameters.blocks, false);
    let turnovers = counting_score(weekly(player.turnovers_pg), parameters.turnovers, true);

    DurantScore {
        player_id: player.player_id.clone(),
        player_name: player.player_name.clone(),
        field_goal,
        free_throw,
        threes,
        points,
        rebounds,
        assists,
        steals,
        blocks,
        turnovers,
        total: field_goal
            + free_throw
            + threes
            + points
            + rebounds
            + assists
            + steals
            + blocks
            + turnovers,
    }
}

fn score_manual_projection(
    projection: &ManualProjectionOverride,
    games_per_active_week: f64,
    parameters: DurantParameters,
) -> DurantScore {
    let weekly = |per_game: f64| per_game * games_per_active_week;

    let field_goal = projected_percentage_score(
        weekly(projection.fgm_pg),
        weekly(projection.fga_pg),
        parameters.field_goal,
    );
    let free_throw = projected_percentage_score(
        weekly(projection.ftm_pg),
        weekly(projection.fta_pg),
        parameters.free_throw,
    );
    let threes = counting_score(weekly(projection.threes_pg), parameters.threes, false);
    let points = counting_score(weekly(projection.points_pg), parameters.points, false);
    let rebounds = counting_score(weekly(projection.rebounds_pg), parameters.rebounds, false);
    let assists = counting_score(weekly(projection.assists_pg), parameters.assists, false);
    let steals = counting_score(weekly(projection.steals_pg), parameters.steals, false);
    let blocks = counting_score(weekly(projection.blocks_pg), parameters.blocks, false);
    let turnovers = counting_score(weekly(projection.turnovers_pg), parameters.turnovers, true);

    DurantScore {
        player_id: projection.player_id.clone(),
        player_name: projection.player_name.clone(),
        field_goal,
        free_throw,
        threes,
        points,
        rebounds,
        assists,
        steals,
        blocks,
        turnovers,
        total: field_goal
            + free_throw
            + threes
            + points
            + rebounds
            + assists
            + steals
            + blocks
            + turnovers,
    }
}

fn projected_percentage_score(
    made_per_week: f64,
    attempts_per_week: f64,
    parameters: PercentageParameters,
) -> f64 {
    if attempts_per_week <= f64::EPSILON || parameters.mean_attempts <= f64::EPSILON {
        return 0.0;
    }

    let player_rate = made_per_week / attempts_per_week;
    let numerator =
        (attempts_per_week / parameters.mean_attempts) * (player_rate - parameters.mean_rate);
    let denominator = (parameters.sigma_rate.powi(2) + parameters.tau_rate.powi(2)).sqrt();

    if denominator <= f64::EPSILON {
        0.0
    } else {
        numerator / denominator
    }
}

// -----------------------------------------------------------------------------
// Reference population Q
// -----------------------------------------------------------------------------

/// Rosenof selects Q with conventional Z-scores, then evaluates G-scores inside
/// that fantasy-relevant population. We do the same. Percentage-category
/// selection uses the usual volume-adjusted shooting impact.
fn select_reference_population(
    players: &[PlayerNineCatStats],
    eligible_ids: &HashSet<PlayerId>,
    reference_size: usize,
) -> Result<Vec<PlayerId>> {
    // Use the broad season population to define the ordinary Z-score scale,
    // while avoiding tiny-sample players who can distort the selector.
    let selector_pool = players
        .iter()
        .filter(|player| player.games >= 10)
        .collect::<Vec<_>>();

    if selector_pool.len() < reference_size {
        bail!(
            "only {} season-stat players are available for a {}-player reference population",
            selector_pool.len(),
            reference_size
        );
    }

    let fg_baseline = ratio(
        selector_pool.iter().map(|player| player.fgm_total).sum(),
        selector_pool.iter().map(|player| player.fga_total).sum(),
    );
    let ft_baseline = ratio(
        selector_pool.iter().map(|player| player.ftm_total).sum(),
        selector_pool.iter().map(|player| player.fta_total).sum(),
    );

    let vectors = selector_pool
        .iter()
        .map(|player| selector_vector(player, fg_baseline, ft_baseline))
        .collect::<Vec<_>>();

    let means = array_means(&vectors);
    let std_devs = array_stddevs(&vectors, means);

    let mut ranked = selector_pool
        .into_iter()
        .map(|player| {
            let values = selector_vector(player, fg_baseline, ft_baseline);
            let score = values
                .iter()
                .zip(means.iter())
                .zip(std_devs.iter())
                .map(|((&value, &mean), &std_dev)| standardized(value, mean, std_dev))
                .sum::<f64>();

            (player.player_id.clone(), score)
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    let reference_players = ranked
        .into_iter()
        .filter_map(|(player_id, _)| eligible_ids.contains(&player_id).then_some(player_id))
        .take(reference_size)
        .collect::<Vec<_>>();

    if reference_players.len() < reference_size {
        bail!(
            "only {} Z-ranked players also have enough weekly observations; need {}",
            reference_players.len(),
            reference_size
        );
    }

    Ok(reference_players)
}

/// Order: PTS, 3PM, REB, AST, STL, BLK, inverted TO, FG impact, FT impact.
fn selector_vector(player: &PlayerNineCatStats, fg_baseline: f64, ft_baseline: f64) -> [f64; 9] {
    [
        player.points_pg,
        player.threes_pg,
        player.rebounds_pg,
        player.assists_pg,
        player.steals_pg,
        player.blocks_pg,
        -player.turnovers_pg,
        (player.fg_pct - fg_baseline) * player.fga_pg,
        (player.ft_pct - ft_baseline) * player.fta_pg,
    ]
}

// -----------------------------------------------------------------------------
// Fit G-score parameters
// -----------------------------------------------------------------------------

fn fit_parameters(
    reference_players: &[PlayerId],
    weekly_by_player: &HashMap<PlayerId, Vec<&PlayerWeeklyStats>>,
) -> Result<DurantParameters> {
    Ok(DurantParameters {
        points: fit_counting(reference_players, weekly_by_player, |week| week.points)?,
        threes: fit_counting(reference_players, weekly_by_player, |week| week.threes)?,
        rebounds: fit_counting(reference_players, weekly_by_player, |week| week.rebounds)?,
        assists: fit_counting(reference_players, weekly_by_player, |week| week.assists)?,
        steals: fit_counting(reference_players, weekly_by_player, |week| week.steals)?,
        blocks: fit_counting(reference_players, weekly_by_player, |week| week.blocks)?,
        turnovers: fit_counting(reference_players, weekly_by_player, |week| week.turnovers)?,
        field_goal: fit_percentage(
            reference_players,
            weekly_by_player,
            |week| week.fgm,
            |week| week.fga,
        )?,
        free_throw: fit_percentage(
            reference_players,
            weekly_by_player,
            |week| week.ftm,
            |week| week.fta,
        )?,
    })
}

fn fit_counting<F>(
    reference_players: &[PlayerId],
    weekly_by_player: &HashMap<PlayerId, Vec<&PlayerWeeklyStats>>,
    value: F,
) -> Result<CountingParameters>
where
    F: Fn(&PlayerWeeklyStats) -> f64 + Copy,
{
    let mut player_means = Vec::with_capacity(reference_players.len());
    let mut player_taus = Vec::with_capacity(reference_players.len());

    for player_id in reference_players {
        let weeks = weekly_by_player.get(player_id).ok_or_else(|| {
            anyhow::anyhow!("reference player {} has no weekly rows", player_id.0)
        })?;

        let values = weeks.iter().map(|week| value(week)).collect::<Vec<_>>();
        let player_mean = mean(&values);
        let player_tau = population_stddev(&values, player_mean);

        player_means.push(player_mean);
        player_taus.push(player_tau);
    }

    let global_mean = mean(&player_means);
    let sigma = population_stddev(&player_means, global_mean);
    let tau = rms(&player_taus);

    Ok(CountingParameters {
        mean: global_mean,
        sigma,
        tau,
    })
}

fn fit_percentage<FM, FA>(
    reference_players: &[PlayerId],
    weekly_by_player: &HashMap<PlayerId, Vec<&PlayerWeeklyStats>>,
    made: FM,
    attempts: FA,
) -> Result<PercentageParameters>
where
    FM: Fn(&PlayerWeeklyStats) -> f64 + Copy,
    FA: Fn(&PlayerWeeklyStats) -> f64 + Copy,
{
    // Rosenof's notation:
    // a_mu_p = player's mean weekly attempts
    // r_mu_p = player's aggregate success rate
    // a_mu   = mean of a_mu_p across Q
    // r_mu   = attempt-weighted aggregate success rate across Q
    let mut player_mean_attempts = Vec::with_capacity(reference_players.len());
    let mut player_rates = Vec::with_capacity(reference_players.len());

    for player_id in reference_players {
        let weeks = weekly_by_player.get(player_id).ok_or_else(|| {
            anyhow::anyhow!("reference player {} has no weekly rows", player_id.0)
        })?;

        let total_made = weeks.iter().map(|week| made(week)).sum::<f64>();
        let total_attempts = weeks.iter().map(|week| attempts(week)).sum::<f64>();
        let mean_attempts = total_attempts / weeks.len() as f64;

        player_mean_attempts.push(mean_attempts);
        player_rates.push(ratio(total_made, total_attempts));
    }

    let mean_attempts = mean(&player_mean_attempts);

    if mean_attempts <= f64::EPSILON {
        return Ok(PercentageParameters::default());
    }

    let weighted_makes = player_mean_attempts
        .iter()
        .zip(player_rates.iter())
        .map(|(&attempts, &rate)| attempts * rate)
        .sum::<f64>();
    let total_weight = player_mean_attempts.iter().sum::<f64>();
    let mean_rate = ratio(weighted_makes, total_weight);

    // r_sigma is the player-to-player standard deviation of
    // (a_mu_p / a_mu) * (r_mu_p - r_mu), not of raw shooting percentages.
    let player_impacts = player_mean_attempts
        .iter()
        .zip(player_rates.iter())
        .map(|(&player_attempts, &player_rate)| {
            (player_attempts / mean_attempts) * (player_rate - mean_rate)
        })
        .collect::<Vec<_>>();
    let sigma_rate = population_stddev(&player_impacts, mean(&player_impacts));

    // r_tau_p is the week-to-week standard deviation of
    // (a_np / a_mu_p) * (r_np - r_mu_p) for each player. r_tau is the RMS
    // of those player-level deviations across Q.
    let mut player_taus = Vec::with_capacity(reference_players.len());

    for ((player_id, &player_mean_attempts), &player_rate) in reference_players
        .iter()
        .zip(player_mean_attempts.iter())
        .zip(player_rates.iter())
    {
        let weeks = weekly_by_player
            .get(player_id)
            .expect("reference player was already validated");

        if player_mean_attempts <= f64::EPSILON {
            player_taus.push(0.0);
            continue;
        }

        let weekly_impacts = weeks
            .iter()
            .map(|week| {
                let week_attempts = attempts(week);

                if week_attempts <= f64::EPSILON {
                    0.0
                } else {
                    let week_rate = made(week) / week_attempts;
                    (week_attempts / player_mean_attempts) * (week_rate - player_rate)
                }
            })
            .collect::<Vec<_>>();

        // By construction the theoretical center is zero. Using zero here
        // follows the paper's definition directly rather than re-centering the
        // finite sample a second time.
        player_taus.push(population_stddev(&weekly_impacts, 0.0));
    }

    let tau_rate = rms(&player_taus);

    Ok(PercentageParameters {
        mean_attempts,
        mean_rate,
        sigma_rate,
        tau_rate,
    })
}

// -----------------------------------------------------------------------------
// Player scoring
// -----------------------------------------------------------------------------

fn score_player(
    player: &PlayerNineCatStats,
    weeks: &[&PlayerWeeklyStats],
    parameters: DurantParameters,
) -> DurantScore {
    let points = counting_score(
        weekly_mean(weeks, |week| week.points),
        parameters.points,
        false,
    );
    let threes = counting_score(
        weekly_mean(weeks, |week| week.threes),
        parameters.threes,
        false,
    );
    let rebounds = counting_score(
        weekly_mean(weeks, |week| week.rebounds),
        parameters.rebounds,
        false,
    );
    let assists = counting_score(
        weekly_mean(weeks, |week| week.assists),
        parameters.assists,
        false,
    );
    let steals = counting_score(
        weekly_mean(weeks, |week| week.steals),
        parameters.steals,
        false,
    );
    let blocks = counting_score(
        weekly_mean(weeks, |week| week.blocks),
        parameters.blocks,
        false,
    );
    let turnovers = counting_score(
        weekly_mean(weeks, |week| week.turnovers),
        parameters.turnovers,
        true,
    );

    let field_goal = percentage_score(
        weeks,
        parameters.field_goal,
        |week| week.fgm,
        |week| week.fga,
    );
    let free_throw = percentage_score(
        weeks,
        parameters.free_throw,
        |week| week.ftm,
        |week| week.fta,
    );

    let total = field_goal
        + free_throw
        + threes
        + points
        + rebounds
        + assists
        + steals
        + blocks
        + turnovers;

    DurantScore {
        player_id: player.player_id.clone(),
        player_name: player.player_name.clone(),
        field_goal,
        free_throw,
        threes,
        points,
        rebounds,
        assists,
        steals,
        blocks,
        turnovers,
        total,
    }
}

fn counting_score(player_mean: f64, parameters: CountingParameters, lower_is_better: bool) -> f64 {
    let denominator = (parameters.sigma.powi(2) + parameters.tau.powi(2)).sqrt();

    if denominator <= f64::EPSILON {
        return 0.0;
    }

    let difference = if lower_is_better {
        parameters.mean - player_mean
    } else {
        player_mean - parameters.mean
    };

    difference / denominator
}

fn percentage_score<FM, FA>(
    weeks: &[&PlayerWeeklyStats],
    parameters: PercentageParameters,
    made: FM,
    attempts: FA,
) -> f64
where
    FM: Fn(&PlayerWeeklyStats) -> f64,
    FA: Fn(&PlayerWeeklyStats) -> f64,
{
    if weeks.is_empty() || parameters.mean_attempts <= f64::EPSILON {
        return 0.0;
    }

    let total_made = weeks.iter().map(|week| made(week)).sum::<f64>();
    let total_attempts = weeks.iter().map(|week| attempts(week)).sum::<f64>();

    if total_attempts <= f64::EPSILON {
        return 0.0;
    }

    let player_mean_attempts = total_attempts / weeks.len() as f64;
    let player_rate = total_made / total_attempts;

    let numerator =
        (player_mean_attempts / parameters.mean_attempts) * (player_rate - parameters.mean_rate);
    let denominator = (parameters.sigma_rate.powi(2) + parameters.tau_rate.powi(2)).sqrt();

    if denominator <= f64::EPSILON {
        0.0
    } else {
        numerator / denominator
    }
}

// -----------------------------------------------------------------------------
// Deterministic future-roster rollout / build identity
// -----------------------------------------------------------------------------

fn sort_and_truncate_auction_rollouts(rollouts: &mut Vec<AuctionRolloutResult>, keep: usize) {
    rollouts.sort_by(|a, b| {
        b.evaluation
            .matchup_win_probability
            .total_cmp(&a.evaluation.matchup_win_probability)
            .then_with(|| {
                b.evaluation
                    .expected_categories
                    .total_cmp(&a.evaluation.expected_categories)
            })
            .then_with(|| a.future_spend.cmp(&b.future_spend))
    });
    rollouts.truncate(keep);
}

const DYNAMIC_SEARCH_VERSION: u32 = 5;
const OVERNIGHT_DYNAMIC_SEARCH_VERSION: u32 = 2;
const OVERNIGHT_STRATEGY_COUNT: usize = 30_178;
const DEEP_DYNAMIC_SEARCH_VERSION: u32 = 2;
const DEEP_STRATEGY_COUNT: usize = 212_941;
const AUCTION_SEARCH_VERSION: u32 = 3;
const AUCTION_J_CENSUS_VERSION: u32 = 2;
const AUCTION_J_CHUNK_SIZE: usize = 4_096;
const H_INDIFFERENCE_EPSILON: f64 = 1e-9;
const FAIR_PRICE_LOCAL_RADIUS: u16 = 3;
/// Opponent clearing-price simulation only needs a compact, high-coverage
/// strategy core. The live team itself still uses the entire loaded bank.
const COMPETITION_PRICING_STRATEGY_LIMIT: usize = 64;
/// In a 13-man roster we model the first 10 spots as intentional auction
/// purchases. The final 3 are generic $1 fliers. This matches the user's
/// league behavior and keeps projected-market repricing tractable.
const INTENTIONAL_ROSTER_SLOTS: usize = 10;
/// Future-path repricing is a refinement pass, so it uses a smaller
/// opponent pricing core than the current-state LIVE market.
const FUTURE_REPRICE_STRATEGY_LIMIT: usize = 16;
const MAX_NON_NEUTRAL_WEIGHTS: usize = 3;
const J_WEIGHT_LEVELS: [f64; 4] = [1.0, 0.0, 0.5, 1.5];

#[derive(Debug, Clone)]
struct RolloutStrategy {
    weights: [f64; 9],
}

impl RolloutStrategy {
    fn balanced() -> Self {
        Self { weights: [1.0; 9] }
    }
}

#[derive(Debug, Clone)]
struct RolloutResult {
    strategy: RolloutStrategy,
    evaluation: DynamicStateEvaluation,
    future_players: Vec<PlayerId>,
    future_player_names: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct AuctionMarketTotals {
    remaining_slots: usize,
    remaining_dollars: u32,
    max_legal_market_bid: u16,
}

#[derive(Debug, Clone)]
struct MarketEconomy {
    snapshot: MarketEconomySnapshot,
    exact_prices: HashMap<PlayerId, f64>,
    prices: HashMap<PlayerId, u16>,
}

#[derive(Debug, Clone)]
struct AuctionRolloutResult {
    strategy: RolloutStrategy,
    evaluation: DynamicStateEvaluation,
    future_players: Vec<PlayerId>,
    future_player_names: Vec<String>,
    future_spend: u16,
    budget_left: u16,
}

#[derive(Debug, Clone)]
struct AuctionCensusCandidate {
    player_id: PlayerId,
    player_name: String,
    x_scores: [f64; 9],
    immediate: DynamicStateEvaluation,
    with_candidate: [f64; 9],
    budget_after: u16,
    future_slots: usize,
    economy: MarketEconomy,
}

/// Search a denser but still tractable j-space. Every category can be neutral
/// (1.0), ignored/coasted (0.0), de-emphasized (0.5), or pushed (1.5), with
/// up to three non-neutral categories at once. This produces 2,620 strategies
/// and, unlike the original 91-vector library, includes mixed plans such as
/// coast FT%, de-emphasize TO, and push REB in the same j.
fn rollout_strategies() -> Vec<RolloutStrategy> {
    let total_codes = J_WEIGHT_LEVELS.len().pow(9);
    let mut strategies = Vec::with_capacity(2620);

    for mut code in 0..total_codes {
        let mut weights = [1.0; 9];
        let mut changed = 0usize;

        for weight in &mut weights {
            let digit = code % J_WEIGHT_LEVELS.len();
            code /= J_WEIGHT_LEVELS.len();
            *weight = J_WEIGHT_LEVELS[digit];
            if digit != 0 {
                changed += 1;
            }
        }

        if changed <= MAX_NON_NEUTRAL_WEIGHTS {
            strategies.push(RolloutStrategy { weights });
        }
    }

    strategies
}

/// Large offline strategy library targeted at roughly one night of compute on
/// the user's 8-thread machine.
///
/// It deliberately spends the extra budget in two different directions:
///
/// 1. Fine resolution for one-to-three category adjustments:
///    0, .25, .5, .75, 1, 1.25, 1.5
/// 2. Coarse structural exploration with exactly four non-neutral categories:
///    0, .5, 1.5
/// 3. A small set of stronger 2.0 pushes when only one or two categories move.
///
/// The baseline weight is fixed at 1.0, which also removes the uninteresting
/// overall positive-scale degree of freedom from j.
fn overnight_rollout_strategies() -> Vec<RolloutStrategy> {
    const FINE_NON_NEUTRAL: [f64; 6] = [0.0, 0.25, 0.5, 0.75, 1.25, 1.5];
    const COARSE_NON_NEUTRAL: [f64; 3] = [0.0, 0.5, 1.5];
    const STRONG_NON_NEUTRAL: [f64; 7] = [0.0, 0.25, 0.5, 0.75, 1.25, 1.5, 2.0];

    let mut strategies = Vec::with_capacity(OVERNIGHT_STRATEGY_COUNT);
    let mut seen = HashSet::<[i16; 9]>::with_capacity(OVERNIGHT_STRATEGY_COUNT);

    push_strategy_if_new([1.0; 9], &mut strategies, &mut seen);

    for changed in 1..=3 {
        generate_exact_strategy_family(changed, &FINE_NON_NEUTRAL, &mut strategies, &mut seen);
    }

    generate_exact_strategy_family(4, &COARSE_NON_NEUTRAL, &mut strategies, &mut seen);

    for changed in 1..=2 {
        generate_exact_strategy_family(changed, &STRONG_NON_NEUTRAL, &mut strategies, &mut seen);
    }

    debug_assert_eq!(strategies.len(), OVERNIGHT_STRATEGY_COUNT);
    strategies
}

fn deep_rollout_strategies() -> Vec<RolloutStrategy> {
    const COARSE_LEVELS: [f64; 4] = [1.0, 0.0, 0.5, 1.5];
    const FINE_NON_NEUTRAL: [f64; 6] = [0.0, 0.25, 0.5, 0.75, 1.25, 1.5];
    const FOUR_REFINED_NON_NEUTRAL: [f64; 4] = [0.0, 0.25, 0.5, 0.75];

    let mut strategies = Vec::with_capacity(DEEP_STRATEGY_COUNT);
    let mut seen = HashSet::<[i16; 9]>::with_capacity(DEEP_STRATEGY_COUNT);

    // Broad structural search over all coarse j-vectors with <= 6
    // non-neutral categories.
    let total_codes = COARSE_LEVELS.len().pow(9);
    for mut code in 0..total_codes {
        let mut weights = [1.0; 9];
        let mut changed = 0usize;

        for weight in &mut weights {
            let digit = code % COARSE_LEVELS.len();
            code /= COARSE_LEVELS.len();
            *weight = COARSE_LEVELS[digit];

            if digit != 0 {
                changed += 1;
            }
        }

        if changed <= 6 {
            push_strategy_if_new(weights, &mut strategies, &mut seen);
        }
    }

    // Keep the successful fine-resolution family from the 30k pilot.
    for changed in 1..=3 {
        generate_exact_strategy_family(changed, &FINE_NON_NEUTRAL, &mut strategies, &mut seen);
    }

    // Refine the exact-4 surface. The pilot strongly preferred the
    // four-category boundary, so spend extra resolution here.
    generate_exact_strategy_family(4, &FOUR_REFINED_NON_NEUTRAL, &mut strategies, &mut seen);

    // Probe whether the same boundary effect continues beyond six
    // changed categories.
    generate_seven_category_probe(&mut strategies, &mut seen);

    // Add exact-5 quarter-step refinement where the 30k census suggested
    // extra resolution can matter. The all-{0,.5} subset is already covered
    // by the coarse <=6 family, so only genuinely new vectors survive.
    const FIVE_REFINED_NON_NEUTRAL: [f64; 3] = [0.0, 0.5, 0.75];
    generate_exact_strategy_family(5, &FIVE_REFINED_NON_NEUTRAL, &mut strategies, &mut seen);

    // Final high-dimensional boundary probe: exactly eight categories move.
    // One is pushed to either 1.25 or 1.5; the other seven are 0 or .5.
    generate_eight_category_probe(&mut strategies, &mut seen);

    debug_assert_eq!(strategies.len(), DEEP_STRATEGY_COUNT);
    strategies
}

fn generate_seven_category_probe(
    strategies: &mut Vec<RolloutStrategy>,
    seen: &mut HashSet<[i16; 9]>,
) {
    for neutral_a in 0..9 {
        for neutral_b in (neutral_a + 1)..9 {
            let active = (0..9)
                .filter(|category| *category != neutral_a && *category != neutral_b)
                .collect::<Vec<_>>();

            for pushed_offset in 0..active.len() {
                let pushed_category = active[pushed_offset];

                for mask in 0usize..(1usize << 6) {
                    let mut weights = [1.0; 9];
                    weights[pushed_category] = 1.5;

                    let mut bit = 0usize;
                    for &category in &active {
                        if category == pushed_category {
                            continue;
                        }

                        weights[category] = if ((mask >> bit) & 1) == 0 { 0.0 } else { 0.5 };
                        bit += 1;
                    }

                    push_strategy_if_new(weights, strategies, seen);
                }
            }
        }
    }
}

fn generate_eight_category_probe(
    strategies: &mut Vec<RolloutStrategy>,
    seen: &mut HashSet<[i16; 9]>,
) {
    const PUSH_LEVELS: [f64; 2] = [1.25, 1.5];

    for neutral_category in 0..9 {
        let active = (0..9)
            .filter(|category| *category != neutral_category)
            .collect::<Vec<_>>();

        for &pushed_category in &active {
            for &push_weight in &PUSH_LEVELS {
                for mask in 0usize..(1usize << 7) {
                    let mut weights = [1.0; 9];
                    weights[pushed_category] = push_weight;

                    let mut bit = 0usize;
                    for &category in &active {
                        if category == pushed_category {
                            continue;
                        }

                        weights[category] = if ((mask >> bit) & 1) == 0 { 0.0 } else { 0.5 };
                        bit += 1;
                    }

                    push_strategy_if_new(weights, strategies, seen);
                }
            }
        }
    }
}

fn generate_exact_strategy_family(
    changed: usize,
    non_neutral_levels: &[f64],
    strategies: &mut Vec<RolloutStrategy>,
    seen: &mut HashSet<[i16; 9]>,
) {
    let mut weights = [1.0; 9];

    generate_exact_strategy_family_recursive(
        0,
        changed,
        non_neutral_levels,
        &mut weights,
        strategies,
        seen,
    );
}

fn generate_exact_strategy_family_recursive(
    category_start: usize,
    remaining: usize,
    non_neutral_levels: &[f64],
    weights: &mut [f64; 9],
    strategies: &mut Vec<RolloutStrategy>,
    seen: &mut HashSet<[i16; 9]>,
) {
    if remaining == 0 {
        push_strategy_if_new(*weights, strategies, seen);
        return;
    }

    if category_start >= weights.len() || remaining > weights.len() - category_start {
        return;
    }

    let last_start = weights.len() - remaining;

    for category in category_start..=last_start {
        for &weight in non_neutral_levels {
            weights[category] = weight;

            generate_exact_strategy_family_recursive(
                category + 1,
                remaining - 1,
                non_neutral_levels,
                weights,
                strategies,
                seen,
            );
        }

        weights[category] = 1.0;
    }
}

fn push_strategy_if_new(
    weights: [f64; 9],
    strategies: &mut Vec<RolloutStrategy>,
    seen: &mut HashSet<[i16; 9]>,
) {
    let key = weights.map(|weight| (weight * 100.0).round() as i16);

    if seen.insert(key) {
        strategies.push(RolloutStrategy { weights });
    }
}

fn max_legal_bid(budget: u16, open_slots: usize, minimum_bid: u16) -> u16 {
    if open_slots == 0 {
        return 0;
    }

    let reserve_for_other_slots =
        (open_slots.saturating_sub(1) as u32).saturating_mul(minimum_bid as u32);
    (budget as u32)
        .saturating_sub(reserve_for_other_slots)
        .min(u16::MAX as u32) as u16
}

fn weighted_x_score(x_scores: [f64; 9], weights: [f64; 9]) -> f64 {
    x_scores
        .iter()
        .zip(weights.iter())
        .map(|(&x, &weight)| x * weight)
        .sum()
}

/// Describe what j is asking future picks to do. A zero weight is not
/// automatically a punt: if the completed roster already has a high win
/// probability in that category, j is simply choosing not to spend more draft
/// capital there ("Coast").
fn describe_strategy(strategy: &RolloutStrategy, projected_probabilities: [f64; 9]) -> String {
    let mut coast = Vec::new();
    let mut punt = Vec::new();
    let mut de_emphasize = Vec::new();
    let mut push = Vec::new();

    for index in 0..9 {
        let weight = strategy.weights[index];
        let name = DYNAMIC_CATEGORY_NAMES[index];
        let probability = projected_probabilities[index];

        if weight <= f64::EPSILON {
            if probability >= 0.65 {
                coast.push(name);
            } else if probability <= 0.35 {
                punt.push(name);
            } else {
                de_emphasize.push(name);
            }
        } else if weight < 1.0 - f64::EPSILON {
            de_emphasize.push(name);
        } else if weight > 1.0 + f64::EPSILON {
            push.push(name);
        }
    }

    let mut parts = Vec::new();
    if !coast.is_empty() {
        parts.push(format!("Coast {}", coast.join(" + ")));
    }
    if !punt.is_empty() {
        parts.push(format!("Punt {}", punt.join(" + ")));
    }
    if !de_emphasize.is_empty() {
        parts.push(format!("De-emphasize {}", de_emphasize.join(" + ")));
    }
    if !push.is_empty() {
        parts.push(format!("Push {}", push.join(" + ")));
    }

    if parts.is_empty() {
        "Balanced".to_string()
    } else {
        parts.join(" / ")
    }
}

/// Name the projected roster from the categories it is actually expected to
/// lose/win. This is deliberately separate from j: j describes future marginal
/// priorities, while build_name describes the resulting team identity.
fn describe_projected_build(probabilities: [f64; 9]) -> String {
    let mut ranked = (0..9)
        .map(|index| (index, probabilities[index]))
        .collect::<Vec<_>>();
    ranked.sort_by(|a, b| a.1.total_cmp(&b.1));

    let punts = ranked
        .iter()
        .filter(|(_, probability)| *probability <= 0.35)
        .map(|(index, _)| *index)
        .collect::<Vec<_>>();

    if !punts.is_empty() {
        let shown = punts
            .iter()
            .take(2)
            .map(|&index| DYNAMIC_CATEGORY_NAMES[index])
            .collect::<Vec<_>>()
            .join(" + ");

        return if punts.len() > 2 {
            format!("Punt {shown} (+{})", punts.len() - 2)
        } else {
            format!("Punt {shown}")
        };
    }

    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    let strengths = ranked
        .iter()
        .filter(|(_, probability)| *probability >= 0.65)
        .take(2)
        .map(|(index, _)| DYNAMIC_CATEGORY_NAMES[*index])
        .collect::<Vec<_>>();

    if strengths.len() >= 2 {
        format!("{} + {} Core", strengths[0], strengths[1])
    } else if strengths.len() == 1 {
        format!("{} Core", strengths[0])
    } else {
        "Balanced".to_string()
    }
}

// -----------------------------------------------------------------------------
// Dynamic H-score core
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct DynamicStateEvaluation {
    category_win_probabilities: [f64; 9],
    expected_categories: f64,
    matchup_win_probability: f64,
}

/// Category order used by the dynamic model:
/// FG%, FT%, 3PM, PTS, REB, AST, STL, BLK, TO.
pub const DYNAMIC_CATEGORY_NAMES: [&str; 9] =
    ["FG%", "FT%", "3PM", "PTS", "REB", "AST", "STL", "BLK", "TO"];

fn score_to_x_vector(score: &DurantScore, parameters: DurantParameters) -> [f64; 9] {
    [
        g_to_x_percentage(score.field_goal, parameters.field_goal),
        g_to_x_percentage(score.free_throw, parameters.free_throw),
        g_to_x_counting(score.threes, parameters.threes),
        g_to_x_counting(score.points, parameters.points),
        g_to_x_counting(score.rebounds, parameters.rebounds),
        g_to_x_counting(score.assists, parameters.assists),
        g_to_x_counting(score.steals, parameters.steals),
        g_to_x_counting(score.blocks, parameters.blocks),
        g_to_x_counting(score.turnovers, parameters.turnovers),
    ]
}

fn g_to_x_counting(g_score: f64, parameters: CountingParameters) -> f64 {
    let denominator = (parameters.tau.powi(2) + parameters.sigma.powi(2)).sqrt();

    if parameters.tau <= f64::EPSILON || denominator <= f64::EPSILON {
        0.0
    } else {
        // G = X * tau / sqrt(tau^2 + sigma^2).
        g_score * denominator / parameters.tau
    }
}

fn g_to_x_percentage(g_score: f64, parameters: PercentageParameters) -> f64 {
    let denominator = (parameters.tau_rate.powi(2) + parameters.sigma_rate.powi(2)).sqrt();

    if parameters.tau_rate <= f64::EPSILON || denominator <= f64::EPSILON {
        0.0
    } else {
        g_score * denominator / parameters.tau_rate
    }
}

fn fit_x_variance(
    reference_players: &[PlayerId],
    scores: &[DurantScore],
    parameters: DurantParameters,
) -> [f64; 9] {
    let reference_set = reference_players.iter().cloned().collect::<HashSet<_>>();
    let vectors = scores
        .iter()
        .filter(|score| reference_set.contains(&score.player_id))
        .map(|score| score_to_x_vector(score, parameters))
        .collect::<Vec<_>>();

    if vectors.is_empty() {
        return [0.0; 9];
    }

    let means = array_means(&vectors);
    let std_devs = array_stddevs(&vectors, means);
    std_devs.map(|std_dev| std_dev.powi(2))
}

fn add_vectors(left: [f64; 9], right: [f64; 9]) -> [f64; 9] {
    let mut result = [0.0; 9];

    for index in 0..9 {
        result[index] = left[index] + right[index];
    }

    result
}

/// Probability of winning at least five of nine independent categories.
/// This is a Poisson-binomial DP equivalent to enumerating the 256 winning
/// scenarios in Rosenof's Most-Categories objective.
fn most_categories_probability(category_probabilities: [f64; 9]) -> f64 {
    let mut wins = [0.0; 10];
    wins[0] = 1.0;

    for (processed, probability) in category_probabilities.into_iter().enumerate() {
        let probability = probability.clamp(0.0, 1.0);

        for won in (0..=processed + 1).rev() {
            let lose_part = wins[won] * (1.0 - probability);
            let win_part = if won == 0 {
                0.0
            } else {
                wins[won - 1] * probability
            };

            wins[won] = lose_part + win_part;
        }
    }

    wins[5..=9].iter().sum()
}

/// Standard normal CDF. The approximation is more than sufficient for draft
/// ranking and avoids introducing a statistics crate solely for erf().
fn normal_cdf(value: f64) -> f64 {
    0.5 * (1.0 + erf_approx(value / std::f64::consts::SQRT_2))
}

fn erf_approx(value: f64) -> f64 {
    // Abramowitz & Stegun 7.1.26. Maximum error is about 1.5e-7.
    let sign = if value < 0.0 { -1.0 } else { 1.0 };
    let x = value.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);

    let polynomial = (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736)
        * t
        + 0.254829592)
        * t;

    sign * (1.0 - polynomial * (-x * x).exp())
}

// -----------------------------------------------------------------------------
// Math helpers
// -----------------------------------------------------------------------------

fn group_weekly(weekly: &[PlayerWeeklyStats]) -> HashMap<PlayerId, Vec<&PlayerWeeklyStats>> {
    let mut grouped: HashMap<PlayerId, Vec<&PlayerWeeklyStats>> = HashMap::new();

    for week in weekly {
        grouped
            .entry(week.player_id.clone())
            .or_default()
            .push(week);
    }

    grouped
}

fn weekly_mean<F>(weeks: &[&PlayerWeeklyStats], value: F) -> f64
where
    F: Fn(&PlayerWeeklyStats) -> f64,
{
    if weeks.is_empty() {
        return 0.0;
    }

    weeks.iter().map(|week| value(week)).sum::<f64>() / weeks.len() as f64
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn population_stddev(values: &[f64], center: f64) -> f64 {
    squared_mean_around(values, center).sqrt()
}

fn squared_mean_around(values: &[f64], center: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    values
        .iter()
        .map(|value| (value - center).powi(2))
        .sum::<f64>()
        / values.len() as f64
}

fn rms(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }

    (values.iter().map(|value| value.powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator <= f64::EPSILON {
        0.0
    } else {
        numerator / denominator
    }
}

fn standardized(value: f64, mean: f64, std_dev: f64) -> f64 {
    if std_dev <= f64::EPSILON {
        0.0
    } else {
        (value - mean) / std_dev
    }
}

fn array_means(values: &[[f64; 9]]) -> [f64; 9] {
    let mut sums = [0.0; 9];

    if values.is_empty() {
        return sums;
    }

    for vector in values {
        for (index, value) in vector.iter().enumerate() {
            sums[index] += value;
        }
    }

    let n = values.len() as f64;
    sums.map(|sum| sum / n)
}

fn array_stddevs(values: &[[f64; 9]], means: [f64; 9]) -> [f64; 9] {
    let mut sums = [0.0; 9];

    if values.is_empty() {
        return sums;
    }

    for vector in values {
        for index in 0..9 {
            sums[index] += (vector[index] - means[index]).powi(2);
        }
    }

    let n = values.len() as f64;
    sums.map(|sum| (sum / n).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn population_math_is_used() {
        let values = [1.0, 2.0, 3.0];
        let center = mean(&values);

        assert!((center - 2.0).abs() < 1e-12);
        assert!((population_stddev(&values, center) - (2.0_f64 / 3.0).sqrt()).abs() < 1e-12);
    }

    #[test]
    fn turnovers_are_reversed() {
        let parameters = CountingParameters {
            mean: 10.0,
            sigma: 2.0,
            tau: 1.0,
        };

        let low_turnovers = counting_score(8.0, parameters, true);
        let high_turnovers = counting_score(12.0, parameters, true);

        assert!(low_turnovers > 0.0);
        assert!(high_turnovers < 0.0);
    }

    #[test]
    fn percentage_sigma_uses_volume_adjusted_player_impact() {
        let mean_attempts = 10.0;
        let mean_rate = 0.5;
        let impacts = [
            (5.0 / mean_attempts) * (0.6 - mean_rate),
            (15.0 / mean_attempts) * (0.4 - mean_rate),
        ];

        let center = mean(&impacts);
        let sigma = population_stddev(&impacts, center);

        assert!(sigma > 0.0);
        assert!((impacts[0] - 0.05).abs() < 1e-12);
        assert!((impacts[1] + 0.15).abs() < 1e-12);
    }

    #[test]
    fn shooting_volume_scales_percentage_value() {
        let parameters = PercentageParameters {
            mean_attempts: 10.0,
            mean_rate: 0.5,
            sigma_rate: 0.05,
            tau_rate: 0.05,
        };

        let denominator = (parameters.sigma_rate.powi(2) + parameters.tau_rate.powi(2)).sqrt();
        let low_volume =
            (5.0 / parameters.mean_attempts) * (0.6 - parameters.mean_rate) / denominator;
        let high_volume =
            (10.0 / parameters.mean_attempts) * (0.6 - parameters.mean_rate) / denominator;

        assert!((high_volume - 2.0 * low_volume).abs() < 1e-12);
    }

    #[test]
    fn normal_cdf_is_symmetric() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-7);
        assert!((normal_cdf(1.0) + normal_cdf(-1.0) - 1.0).abs() < 1e-7);
    }

    #[test]
    fn nine_coinflip_categories_give_fifty_percent_matchup_odds() {
        let probability = most_categories_probability([0.5; 9]);
        assert!((probability - 0.5).abs() < 1e-12);
    }

    #[test]
    fn dominant_categories_raise_matchup_probability() {
        let neutral = most_categories_probability([0.5; 9]);
        let strong = most_categories_probability([0.7; 9]);

        assert!(strong > neutral);
    }

    #[test]
    fn rollout_strategy_library_is_dense_but_bounded() {
        let strategies = rollout_strategies();

        assert_eq!(strategies.len(), 2620);
        assert!(
            strategies
                .iter()
                .any(|strategy| strategy.weights == [1.0; 9])
        );

        let mut mixed = [1.0; 9];
        mixed[1] = 0.0;
        mixed[4] = 1.5;
        mixed[8] = 0.5;
        assert!(strategies.iter().any(|strategy| strategy.weights == mixed));

        assert!(strategies.iter().all(|strategy| {
            strategy
                .weights
                .iter()
                .filter(|&&weight| (weight - 1.0).abs() > f64::EPSILON)
                .count()
                <= MAX_NON_NEUTRAL_WEIGHTS
        }));
    }

    #[test]
    fn projected_build_is_named_from_actual_weak_categories() {
        let probabilities = [0.80, 0.73, 0.89, 0.93, 0.84, 0.83, 0.43, 0.71, 0.10];

        assert_eq!(describe_projected_build(probabilities), "Punt TO");
    }

    #[test]
    fn zero_j_weight_on_locked_categories_is_coasting_not_punting() {
        let strategy = RolloutStrategy {
            weights: [1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        };
        let probabilities = [0.50, 0.60, 0.89, 0.93, 0.55, 0.55, 0.50, 0.50, 0.50];

        assert_eq!(
            describe_strategy(&strategy, probabilities),
            "Coast 3PM + PTS"
        );
    }

    #[test]
    fn zero_j_weight_on_lost_categories_is_a_punt() {
        let strategy = RolloutStrategy {
            weights: [1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0],
        };
        let probabilities = [0.60, 0.18, 0.60, 0.70, 0.70, 0.70, 0.60, 0.70, 0.12];

        assert_eq!(describe_strategy(&strategy, probabilities), "Punt FT% + TO");
    }

    #[test]
    fn weighted_x_score_ignores_a_zero_weight_category() {
        let x = [10.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let mut weights = [1.0; 9];
        weights[0] = 0.0;

        assert!((weighted_x_score(x, weights) - 8.0).abs() < 1e-12);
    }

    #[test]
    fn auction_max_bid_reserves_one_dollar_for_every_other_slot() {
        assert_eq!(max_legal_bid(200, 13, 1), 188);
        assert_eq!(max_legal_bid(50, 3, 1), 48);
        assert_eq!(max_legal_bid(7, 1, 1), 7);
        assert_eq!(max_legal_bid(0, 0, 1), 0);
    }

    #[test]
    fn default_auction_config_matches_birdboard_league_budget() {
        let config = AuctionConfig::default();
        assert_eq!(config.starting_budget, 200);
        assert_eq!(config.minimum_bid, 1);
        assert_eq!(config.fair_price_iterations, 8);
    }
}
