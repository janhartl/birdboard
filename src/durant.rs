use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};

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
    /// All players with enough weekly observations to receive a static score,
    /// sorted best-to-worst by aggregate G-score.
    pub scores: Vec<DurantScore>,
}

impl DurantModel {
    pub fn empty(league_teams: usize, roster_size: usize) -> Self {
        let reference_size = league_teams.saturating_mul(roster_size);

        Self {
            reference_size,
            team_size: roster_size,
            reference_players: Vec::new(),
            parameters: DurantParameters::default(),
            scores: Vec::new(),
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

        Ok(Self {
            reference_size,
            team_size: roster_size,
            reference_players,
            parameters,
            scores,
        })
    }

    pub fn score_for(&self, player_id: &PlayerId) -> Option<&DurantScore> {
        self.scores
            .iter()
            .find(|score| &score.player_id == player_id)
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
}
