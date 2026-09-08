use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{
    player::PlayerId,
    stats::{PlayerNineCatStats, PlayerWeeklyStats, StatsBundle},
};

/// BirdBoard requires a minimum number of active scoring periods so tiny
/// samples do not enter the static model. Missing zero-game weeks are not
/// inserted into the weekly cache, so this is an active-week filter.
pub const MIN_ACTIVE_WEEKS: usize = 10;

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
    /// All players with enough weekly observations to receive a static score,
    /// sorted best-to-worst by aggregate G-score.
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

        let weekly_by_player = group_weekly(&stats.weekly);

        let eligible_ids = weekly_by_player
            .iter()
            .filter_map(|(player_id, weeks)| {
                (weeks.len() >= MIN_ACTIVE_WEEKS).then_some(player_id.clone())
            })
            .collect::<HashSet<_>>();

        if eligible_ids.len() < reference_size {
            bail!(
                "only {} players have at least {} active weeks; Durant needs {} for Q",
                eligible_ids.len(),
                MIN_ACTIVE_WEEKS,
                reference_size
            );
        }

        let reference_players =
            select_reference_population(&stats.players, &eligible_ids, reference_size)?;

        let parameters = fit_parameters(&reference_players, &weekly_by_player)?;

        let mut scores = stats
            .players
            .iter()
            .filter(|player| eligible_ids.contains(&player.player_id))
            .filter_map(|player| {
                weekly_by_player
                    .get(&player.player_id)
                    .map(|weeks| score_player(player, weeks, parameters))
            })
            .collect::<Vec<_>>();

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

    pub fn score_for(&self, player_id: &PlayerId) -> Option<&DurantScore> {
        self.scores
            .iter()
            .find(|score| &score.player_id == player_id)
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
    /// This is intentionally a practical BirdBoard approximation to H0's
    /// future-pick optimizer X_delta(j): we use the real remaining player pool
    /// instead of approximating future players as a multivariate Gaussian.
    pub fn dynamic_scores(
        &self,
        own_roster: &[PlayerId],
        opponent_rosters: &[Vec<PlayerId>],
        candidates: &[PlayerId],
    ) -> Vec<DynamicDurantScore> {
        if let Some(cached) = self.load_dynamic_cache(own_roster, opponent_rosters, candidates) {
            return cached;
        }

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
        let strategies = rollout_strategies();
        let strategy_rankings = self.rank_available_by_strategy(candidates, &strategies);

        let owned = own_roster.iter().cloned().collect::<HashSet<_>>();
        let future_slots_after_candidate = self
            .team_size
            .saturating_sub(own_roster.len().saturating_add(1));

        let mut results = candidates
            .iter()
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
                    &strategies,
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

        self.save_dynamic_cache(own_roster, opponent_rosters, candidates, &results);
        results
    }

    fn rank_available_by_strategy(
        &self,
        candidates: &[PlayerId],
        strategies: &[RolloutStrategy],
    ) -> Vec<Vec<PlayerId>> {
        strategies
            .iter()
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
        let market_stride = self.league_teams().max(1);

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

            for slot in 0..future_slots {
                if valid_ranked.is_empty() {
                    break;
                }

                let target_index = ((slot + 1) * market_stride).saturating_sub(1);
                let mut index = target_index.min(valid_ranked.len() - 1);

                while used_indices.contains(&index) && index + 1 < valid_ranked.len() {
                    index += 1;
                }

                if used_indices.contains(&index) {
                    if let Some(fallback) = (0..valid_ranked.len())
                        .find(|candidate_index| !used_indices.contains(candidate_index))
                    {
                        index = fallback;
                    } else {
                        break;
                    }
                }

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

const DYNAMIC_SEARCH_VERSION: u32 = 4;
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
}
