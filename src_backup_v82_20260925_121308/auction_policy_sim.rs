use crate::app::App;
use crate::durant::{AuctionConfig, DurantModel, MarketBoard};
use crate::equilibrium_market::{self, EquilibriumMarketRow};
use crate::player::PlayerId;
use crate::stats;
use crate::strategy_stage::{RuntimeStrategyStage, build_mid_adaptive_weights};
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};

const TEAM_COUNT: usize = 13;
const ROSTER_SIZE: usize = 13;
const STARTING_BUDGET: u16 = 200;
const MIN_BID: u16 = 1;
const FINAL_SHORTLIST: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserPolicy {
    DurantValue,
    NowDeltaH,
    FinalDeltaH,
    TeamStrategy,
}

impl UserPolicy {
    const ALL: [Self; 4] = [
        Self::DurantValue,
        Self::NowDeltaH,
        Self::FinalDeltaH,
        Self::TeamStrategy,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::DurantValue => "DURANT/$ control",
            Self::NowDeltaH => "NOW delta-H",
            Self::FinalDeltaH => "FINAL delta-H",
            Self::TeamStrategy => "TEAM strategy",
        }
    }
}

#[derive(Clone)]
struct SimState {
    rosters: Vec<Vec<PlayerId>>,
    prices: Vec<Vec<u16>>,
    budgets: Vec<u16>,
    available: HashSet<PlayerId>,
}

impl SimState {
    fn new(candidate_pool: &[PlayerId]) -> Self {
        Self {
            rosters: vec![Vec::new(); TEAM_COUNT],
            prices: vec![Vec::new(); TEAM_COUNT],
            budgets: vec![STARTING_BUDGET; TEAM_COUNT],
            available: candidate_pool.iter().cloned().collect(),
        }
    }

    fn open_slots(&self, team: usize) -> usize {
        ROSTER_SIZE.saturating_sub(self.rosters[team].len())
    }

    fn max_legal_bid(&self, team: usize) -> u16 {
        let open = self.open_slots(team);
        if open == 0 {
            return 0;
        }
        self.budgets[team].saturating_sub(MIN_BID.saturating_mul(open.saturating_sub(1) as u16))
    }

    fn buy(&mut self, team: usize, player_id: PlayerId, price: u16) -> Result<()> {
        if !self.available.remove(&player_id) {
            bail!("attempted to draft an unavailable player: {:?}", player_id);
        }
        if self.rosters[team].len() >= ROSTER_SIZE {
            bail!("team {team} is already full");
        }
        if price > self.max_legal_bid(team) || price > self.budgets[team] {
            bail!("illegal simulated price ${price} for team {team}");
        }
        self.budgets[team] -= price;
        self.rosters[team].push(player_id);
        self.prices[team].push(price);
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct LeagueResult {
    user_rank: usize,
    user_avg_h: f64,
    user_avg_categories: f64,
    user_category_win_probabilities: [f64; 9],
    user_budget_left: u16,
    user_avg_durant: f64,
    user_one_dollar_slots: usize,
}

struct SimOutcome {
    result: LeagueResult,
    state: SimState,
}

#[derive(Default)]
struct Aggregate {
    runs: usize,
    wins: usize,
    top3: usize,
    rank_sum: f64,
    h_sum: f64,
    cats_sum: f64,
    budget_sum: f64,
    durant_sum: f64,
    one_dollar_sum: f64,
}

impl Aggregate {
    fn push(&mut self, result: LeagueResult) {
        self.runs += 1;
        self.wins += usize::from(result.user_rank == 1);
        self.top3 += usize::from(result.user_rank <= 3);
        self.rank_sum += result.user_rank as f64;
        self.h_sum += result.user_avg_h;
        self.cats_sum += result.user_avg_categories;
        self.budget_sum += result.user_budget_left as f64;
        self.durant_sum += result.user_avg_durant;
        self.one_dollar_sum += result.user_one_dollar_slots as f64;
    }
}

/// Tiny deterministic RNG so the simulation test needs no new dependency.
struct SimRng(u64);

impl SimRng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    fn unit(&mut self) -> f64 {
        let bits = self.next_u64() >> 11;
        bits as f64 / ((1u64 << 53) as f64)
    }

    fn choose_top_index(&mut self, len: usize) -> usize {
        if len <= 1 {
            return 0;
        }
        let roll = self.unit();
        let requested = if roll < 0.70 {
            0
        } else if roll < 0.86 {
            1
        } else if roll < 0.96 {
            2
        } else {
            3
        };
        requested.min(len - 1)
    }
}

#[derive(Debug)]
struct EquilibriumRoomOutcome {
    sales: Vec<(PlayerId, u16)>,
    budgets_left: Vec<u16>,
}

/// Build BirdBoard's independent synthetic auction market.
///
/// v75 intentionally drops the recursive price fixed point.  Each synthetic
/// room is a complete 13 x 13 auction with heterogeneous strategy-bank
/// managers.  On every nomination, all active managers solve BirdBoard's real
/// H-indifference reservation-bid problem for the CURRENT roster/budget state.
/// Future BUY/PASS completion uses only the transparent legacy replacement/G-$
/// market as a fixed opportunity-cost approximation.  ESPN is never read.
///
/// The price saved for a player is the unconditional expected clearing weight:
///
///     $1 + sale_rate * (conditional_mean_sale_price - $1)
///
/// so players who rarely make a rational 169-player room naturally collapse
/// toward the minimum bid.  Normal BirdBoard later rescales this demand shape
/// to the real room's remaining discretionary dollars after every pick.
#[test]
#[ignore = "expensive one-time independent 13x13 synthetic auction census"]
fn build_independent_equilibrium_market() -> Result<()> {
    let rooms = std::env::var("BIRDBOARD_MARKET_ROOMS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(120)
        .max(20);
    let pool_size = std::env::var("BIRDBOARD_MARKET_PLAYERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(260)
        .max(TEAM_COUNT * ROSTER_SIZE);

    let stats = stats::load_or_fetch()?;
    let mut app = App::new(stats)?;

    // The independent synthetic-market census must be reproducible from the
    // underlying basketball model alone. App::new normally loads a previously
    // saved equilibrium market for production use; if we leave that attached,
    // a second census run silently uses the first run's synthetic prices as its
    // future-price prior and reintroduces the recursive feedback v75 was meant
    // to remove. Force the entire census (including BUY/PASS sub-rollouts) onto
    // the transparent legacy replacement/G-$ opportunity-cost prior.
    app.durant.set_equilibrium_prices(None);

    let bank = app
        .strategy_bank
        .as_ref()
        .context("synthetic market build requires a runtime strategy bank")?;
    let strategy_weights = bank.early_weights_or_fallback();
    if strategy_weights.is_empty() {
        bail!("runtime strategy bank contains no strategy weights");
    }

    let all_players = app
        .durant
        .scores
        .iter()
        .map(|score| score.player_id.clone())
        .collect::<Vec<_>>();
    let candidate_pool = all_players
        .iter()
        .take(pool_size.min(all_players.len()))
        .cloned()
        .collect::<Vec<_>>();
    if candidate_pool.len() < TEAM_COUNT * ROSTER_SIZE {
        bail!(
            "synthetic market pool has only {} players; need at least {}",
            candidate_pool.len(),
            TEAM_COUNT * ROSTER_SIZE
        );
    }

    println!("\nBirdBoard independent synthetic auction market v77");
    println!(
        "{} rooms x {} teams x {} roster slots; ${} per team",
        rooms, TEAM_COUNT, ROSTER_SIZE, STARTING_BUDGET
    );
    println!(
        "{} actively simulated players; {} total BirdBoard players saved (unsimulated tail = $1)",
        candidate_pool.len(),
        all_players.len()
    );
    println!("strategy profiles available: {}", strategy_weights.len());
    println!(
        "bids: current-state H-indifference max bids; future opportunity cost uses ONLY the legacy replacement/G-$ market"
    );
    println!(
        "saved synthetic/equilibrium prices are explicitly ignored while building this census"
    );
    println!(
        "clearing: rotating nominations, second-highest rational max bid + $1; ESPN inputs: NONE\n"
    );

    let started = std::time::Instant::now();
    let model = &app.durant;
    let completed = AtomicUsize::new(0);
    let progress_step = (rooms / 10).max(1);
    let outcomes = (0..rooms)
        .into_par_iter()
        .map(|room| {
            let seed = 0x51A7_2027u64.wrapping_add((room as u64).wrapping_mul(104_729));
            let result = simulate_independent_equilibrium_room(
                model,
                &candidate_pool,
                strategy_weights,
                seed,
            );
            let done = completed.fetch_add(1, Ordering::Relaxed) + 1;
            if done % progress_step == 0 || done == rooms {
                println!(
                    "  completed {}/{} rooms ({:.1}s elapsed)",
                    done,
                    rooms,
                    started.elapsed().as_secs_f64()
                );
            }
            result
        })
        .collect::<Vec<_>>();

    let mut sales_by_player = candidate_pool
        .iter()
        .cloned()
        .map(|player_id| (player_id, Vec::<u16>::new()))
        .collect::<HashMap<_, _>>();
    let mut total_left = 0u64;
    let mut max_left = 0u16;
    let mut completed_rooms = 0usize;
    for outcome in outcomes {
        let outcome = outcome?;
        completed_rooms += 1;
        for (player_id, price) in outcome.sales {
            if let Some(prices) = sales_by_player.get_mut(&player_id) {
                prices.push(price);
            }
        }
        for budget in outcome.budgets_left {
            total_left += budget as u64;
            max_left = max_left.max(budget);
        }
    }

    let simulated_ids = candidate_pool.iter().cloned().collect::<HashSet<_>>();
    let mut rows = Vec::<EquilibriumMarketRow>::with_capacity(all_players.len());
    for player_id in &all_players {
        let mut prices = sales_by_player.remove(player_id).unwrap_or_default();
        prices.sort_unstable();
        let observations = prices.len();
        let sale_rate = if simulated_ids.contains(player_id) {
            observations as f64 / completed_rooms.max(1) as f64
        } else {
            0.0
        };
        let (mean_price, median_price, p25_price, p75_price) = if prices.is_empty() {
            (1.0, 1.0, 1.0, 1.0)
        } else {
            let mean = prices.iter().map(|price| *price as f64).sum::<f64>() / prices.len() as f64;
            (
                mean,
                quantile_sorted_u16(&prices, 0.50),
                quantile_sorted_u16(&prices, 0.25),
                quantile_sorted_u16(&prices, 0.75),
            )
        };
        let equilibrium_price = 1.0 + sale_rate * (mean_price - 1.0).max(0.0);
        rows.push(EquilibriumMarketRow {
            player_id: player_id.0.clone(),
            player_name: player_display_name(&app, player_id),
            equilibrium_price,
            mean_price,
            median_price,
            p25_price,
            p75_price,
            sale_rate,
            observations,
            rooms: completed_rooms,
        });
    }
    rows.sort_by(|left, right| {
        right
            .equilibrium_price
            .total_cmp(&left.equilibrium_price)
            .then_with(|| right.median_price.total_cmp(&left.median_price))
            .then_with(|| left.player_name.cmp(&right.player_name))
    });

    let path = equilibrium_market::save_for_season(&app.stats.draft_season, &rows)?;
    let team_samples = completed_rooms * TEAM_COUNT;
    println!(
        "finished {} rooms in {:.1}s",
        completed_rooms,
        started.elapsed().as_secs_f64()
    );
    println!("Saved independent market to {}", path.display());
    println!(
        "auction sanity: avg money left/team ${:.2}, max left ${}",
        total_left as f64 / team_samples.max(1) as f64,
        max_left
    );
    println!("\nTOP 30 INDEPENDENT SYNTHETIC CLEARING PRICES");
    println!(
        "{:<3} {:<25} {:>7} {:>7} {:>7} {:>11} {:>9}",
        "#", "player", "market", "median", "mean", "IQR", "sale rate"
    );
    for (index, row) in rows.iter().take(30).enumerate() {
        println!(
            "{:>2}. {:<25} ${:>5.1} ${:>5.1} ${:>5.1}  ${:>3.0}-${:<3.0} {:>8.1}%",
            index + 1,
            row.player_name,
            row.equilibrium_price,
            row.median_price,
            row.mean_price,
            row.p25_price,
            row.p75_price,
            row.sale_rate * 100.0,
        );
    }
    println!(
        "\nNo fixed point and no ESPN fitting: these are empirical sale distributions from complete rational-bid synthetic auctions."
    );
    Ok(())
}

fn representative_strategy_profiles(weights: &[[f64; 9]], requested: usize) -> Vec<[f64; 9]> {
    let count = requested.min(weights.len()).max(1);
    if count >= weights.len() {
        return weights.to_vec();
    }
    if count == 1 {
        return vec![weights[0]];
    }

    // Evenly span the learned bank rather than taking its first N entries.
    // No ESPN information enters this selection.
    (0..count)
        .map(|index| {
            let source = index * (weights.len() - 1) / (count - 1);
            weights[source]
        })
        .collect()
}

fn sampled_room_profile_sets(profile_count: usize, rooms: usize) -> Vec<Vec<usize>> {
    (0..rooms)
        .map(|room| {
            let mut rng =
                SimRng::new(0xA11C_E5E1_2027u64.wrapping_add((room as u64).wrapping_mul(104_729)));
            let mut indices = (0..profile_count).collect::<Vec<_>>();
            for i in (1..indices.len()).rev() {
                let j = (rng.next_u64() as usize) % (i + 1);
                indices.swap(i, j);
            }
            indices.truncate(TEAM_COUNT.min(indices.len()));
            indices
        })
        .collect()
}

fn second_price_from_profile_bids(bids: &[u16], profile_set: &[usize]) -> u16 {
    let mut room_bids = profile_set
        .iter()
        .filter_map(|index| bids.get(*index).copied())
        .collect::<Vec<_>>();
    if room_bids.is_empty() {
        return MIN_BID;
    }
    room_bids.sort_unstable_by(|left, right| right.cmp(left));
    let winner_max = room_bids[0];
    if winner_max < MIN_BID {
        return MIN_BID;
    }
    let second_max = room_bids.get(1).copied().unwrap_or(0);
    winner_max
        .min(second_max.saturating_add(1).max(MIN_BID))
        .max(MIN_BID)
}

fn simulate_independent_equilibrium_room(
    model: &DurantModel,
    candidate_pool: &[PlayerId],
    strategy_weights: &[[f64; 9]],
    seed: u64,
) -> Result<EquilibriumRoomOutcome> {
    let mut state = SimState::new(candidate_pool);
    let team_styles = (0..TEAM_COUNT)
        .map(|team| {
            let index = (seed as usize)
                .wrapping_add(team.wrapping_mul(97))
                .wrapping_add(team.wrapping_mul(team).wrapping_mul(13))
                % strategy_weights.len();
            strategy_weights[index]
        })
        .collect::<Vec<_>>();
    let mut utility_cache = vec![None::<HashMap<PlayerId, f64>>; TEAM_COUNT];
    let mut rng = SimRng::new(seed ^ 0x1A2B_3C4D_5E6F_7788);
    let mut sales = Vec::<(PlayerId, u16)>::with_capacity(TEAM_COUNT * ROSTER_SIZE);
    let config = AuctionConfig::default();

    for sale_index in 0..(TEAM_COUNT * ROSTER_SIZE) {
        let start = sale_index % TEAM_COUNT;
        let nominator = (0..TEAM_COUNT)
            .map(|offset| (start + offset) % TEAM_COUNT)
            .find(|team| state.open_slots(*team) > 0)
            .context("synthetic auction has no active nominator")?;

        // Nomination choice is deliberately cheap.  Managers nominate from the
        // top of a roster-dependent H/category utility list; actual PRICE is
        // determined below by full H-indifference reservation bids.
        for team in 0..TEAM_COUNT {
            if state.open_slots(team) == 0 || utility_cache[team].is_some() {
                continue;
            }
            utility_cache[team] = Some(equilibrium_team_utilities(
                model,
                candidate_pool,
                &state.rosters[team],
                team_styles[team],
            ));
        }

        let nominator_schedule = equilibrium_bid_schedule(
            utility_cache[nominator]
                .as_ref()
                .context("missing nominator utility cache")?,
            &state.available,
            state.budgets[nominator],
            state.open_slots(nominator),
        );
        if nominator_schedule.is_empty() {
            bail!("nominator {nominator} has no available player to nominate");
        }
        let nominee_index = rng.choose_top_index(nominator_schedule.len().min(4));
        let nominee = nominator_schedule[nominee_index].0.clone();

        let available_candidates = candidate_pool
            .iter()
            .filter(|player_id| state.available.contains(*player_id))
            .cloned()
            .collect::<Vec<_>>();

        let mut max_bids = model.synthetic_room_reservation_bids_with_strategy_weights(
            &state.rosters,
            &state.budgets,
            &nominee,
            &available_candidates,
            &team_styles,
            config,
        );
        if max_bids.len() != TEAM_COUNT {
            bail!(
                "synthetic bid solver returned {} bids for {} teams",
                max_bids.len(),
                TEAM_COUNT
            );
        }

        // A nomination itself carries the minimum opening bid.  If every
        // rational bidder would pass even at $1, the nominator still acquires
        // the player for $1 rather than leaving the auction state undefined.
        if state.open_slots(nominator) > 0 {
            max_bids[nominator] = max_bids[nominator]
                .max(MIN_BID)
                .min(state.max_legal_bid(nominator));
        }

        let mut bids = (0..TEAM_COUNT)
            .filter(|team| state.open_slots(*team) > 0)
            .map(|team| {
                (
                    team,
                    max_bids[team].min(state.max_legal_bid(team)),
                    stable_player_team_hash(seed, team, &nominee),
                )
            })
            .collect::<Vec<_>>();
        bids.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.2.cmp(&left.2)));
        if bids.is_empty() {
            bail!("no active synthetic bidder for nominee {:?}", nominee);
        }

        let (winner, winner_max, _) = bids[0];
        let second_max = bids.get(1).map(|entry| entry.1).unwrap_or(0);
        let sale_price = if winner_max < MIN_BID {
            MIN_BID
        } else {
            winner_max
                .min(second_max.saturating_add(1).max(MIN_BID))
                .min(state.max_legal_bid(winner))
        };

        state.buy(winner, nominee.clone(), sale_price)?;
        sales.push((nominee, sale_price));

        // Only the winning manager's roster changed.  Everyone else's cheap
        // nomination utility ranking remains valid; availability is filtered
        // when the next nomination is chosen.
        utility_cache[winner] = None;
    }

    for team in 0..TEAM_COUNT {
        if state.rosters[team].len() != ROSTER_SIZE {
            bail!(
                "synthetic room ended with team {team} at {}/{} players",
                state.rosters[team].len(),
                ROSTER_SIZE
            );
        }
    }

    Ok(EquilibriumRoomOutcome {
        sales,
        budgets_left: state.budgets,
    })
}

fn equilibrium_team_utilities(
    model: &DurantModel,
    candidate_pool: &[PlayerId],
    roster: &[PlayerId],
    weights: [f64; 9],
) -> HashMap<PlayerId, f64> {
    let mut rows = candidate_pool
        .iter()
        .filter_map(|player_id| {
            let score = model.score_for(player_id)?;
            let category_g = [
                score.field_goal,
                score.free_throw,
                score.threes,
                score.points,
                score.rebounds,
                score.assists,
                score.steals,
                score.blocks,
                score.turnovers,
            ];
            let strategy_value = category_g
                .iter()
                .zip(weights.iter())
                .map(|(value, weight)| value * weight)
                .sum::<f64>();
            let immediate_h = model.immediate_fit(roster, player_id).unwrap_or(0.0);
            Some((player_id.clone(), immediate_h, strategy_value))
        })
        .collect::<Vec<_>>();

    let n = rows.len();
    if n <= 1 {
        return rows
            .drain(..)
            .map(|(player_id, _, _)| (player_id, 1.0))
            .collect();
    }

    let mut fit_order = (0..n).collect::<Vec<_>>();
    fit_order.sort_by(|left, right| {
        rows[*right]
            .1
            .total_cmp(&rows[*left].1)
            .then_with(|| rows[*left].0.0.cmp(&rows[*right].0.0))
    });
    let mut strategy_order = (0..n).collect::<Vec<_>>();
    strategy_order.sort_by(|left, right| {
        rows[*right]
            .2
            .total_cmp(&rows[*left].2)
            .then_with(|| rows[*left].0.0.cmp(&rows[*right].0.0))
    });

    let mut fit_percentile = vec![0.0; n];
    let mut strategy_percentile = vec![0.0; n];
    let denominator = (n - 1) as f64;
    for (rank, index) in fit_order.into_iter().enumerate() {
        fit_percentile[index] = 1.0 - rank as f64 / denominator;
    }
    for (rank, index) in strategy_order.into_iter().enumerate() {
        strategy_percentile[index] = 1.0 - rank as f64 / denominator;
    }

    rows.into_iter()
        .enumerate()
        .map(|(index, (player_id, _, _))| {
            // Equal-weight rank aggregation avoids fitting any arbitrary scale
            // between probability-point H and category-G units.
            (
                player_id,
                fit_percentile[index] + strategy_percentile[index],
            )
        })
        .collect()
}

fn equilibrium_bid_schedule(
    utilities: &HashMap<PlayerId, f64>,
    available: &HashSet<PlayerId>,
    budget: u16,
    open_slots: usize,
) -> Vec<(PlayerId, u16)> {
    if open_slots == 0 || available.is_empty() {
        return Vec::new();
    }

    let mut ranked = available
        .iter()
        .filter_map(|player_id| {
            utilities
                .get(player_id)
                .map(|utility| (player_id.clone(), *utility))
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.0.cmp(&right.0.0))
    });
    if ranked.is_empty() {
        return Vec::new();
    }

    let target_count = open_slots.min(ranked.len());
    let floor = ranked
        .get(target_count)
        .map(|(_, utility)| *utility)
        .unwrap_or_else(|| {
            ranked
                .last()
                .map(|(_, utility)| *utility - 1e-6)
                .unwrap_or(0.0)
        });
    let surpluses = ranked
        .iter()
        .take(target_count)
        .map(|(_, utility)| (*utility - floor).max(1e-6))
        .collect::<Vec<_>>();
    let total_surplus = surpluses.iter().sum::<f64>();

    let reserve = MIN_BID.saturating_mul(open_slots as u16);
    let discretionary = budget.saturating_sub(reserve) as f64;
    let legal_max =
        budget.saturating_sub(MIN_BID.saturating_mul(open_slots.saturating_sub(1) as u16));

    ranked
        .into_iter()
        .enumerate()
        .map(|(rank, (player_id, _))| {
            let bid = if rank < target_count && discretionary > 0.0 {
                let share = if total_surplus <= f64::EPSILON {
                    1.0 / target_count.max(1) as f64
                } else {
                    surpluses[rank] / total_surplus
                };
                MIN_BID.saturating_add((discretionary * share).round() as u16)
            } else {
                MIN_BID
            };
            (player_id, bid.clamp(MIN_BID, legal_max.max(MIN_BID)))
        })
        .collect()
}

fn quantile_sorted_u16(values: &[u16], q: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    if values.len() == 1 {
        return values[0] as f64;
    }
    let position = q.clamp(0.0, 1.0) * (values.len() - 1) as f64;
    let low = position.floor() as usize;
    let high = position.ceil() as usize;
    if low == high {
        values[low] as f64
    } else {
        let fraction = position - low as f64;
        values[low] as f64 * (1.0 - fraction) + values[high] as f64 * fraction
    }
}

#[test]
#[ignore = "slow simulation; run explicitly in --release mode"]
fn auction_policy_simulation() -> Result<()> {
    let runs = std::env::var("BIRDBOARD_SIM_RUNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(8)
        .max(1);

    let stats = stats::load_or_fetch()?;
    let app = App::new(stats)?;
    let bank = app
        .strategy_bank
        .as_ref()
        .context("simulation requires a runtime strategy bank")?;

    // Stable, fantasy-relevant universe. We deliberately retain a buffer above
    // 13 x 13 so auction choices do not collapse to exactly 169 names.
    let candidate_pool = app
        .durant
        .scores
        .iter()
        .take(220)
        .map(|score| score.player_id.clone())
        .collect::<Vec<_>>();
    if candidate_pool.len() < TEAM_COUNT * ROSTER_SIZE {
        bail!(
            "simulation needs at least {} scored players, found {}",
            TEAM_COUNT * ROSTER_SIZE,
            candidate_pool.len()
        );
    }

    let global_weights = bank.early_weights_or_fallback();
    if global_weights.is_empty() {
        bail!("runtime strategy bank contains no weights");
    }

    println!("\nBirdBoard policy simulation");
    println!(
        "{} teams x {} players, $200 budget, market-price acquisition approximation",
        TEAM_COUNT, ROSTER_SIZE
    );
    println!(
        "{} matched auction environments per policy; opponent bots use heterogeneous strategy/value drafting",
        runs
    );
    println!(
        "NOTE: this is a policy-selection test, not yet a bidding/nominations simulator. Final ranking uses BirdBoard H.\n"
    );

    let mut aggregates = HashMap::<&'static str, Aggregate>::new();

    for run in 0..runs {
        let seed = 0xB1AD_B04Du64.wrapping_add((run as u64).wrapping_mul(104_729));
        print!("run {:>2}/{runs}: ", run + 1);

        for policy in UserPolicy::ALL {
            let outcome = simulate_one_auction(
                &app,
                &candidate_pool,
                global_weights,
                bank.weights.as_slice(),
                policy,
                seed,
            )?;
            let result = outcome.result;
            aggregates.entry(policy.label()).or_default().push(result);
            print!(
                "{} rank #{}, H {:.1}%  |  ",
                policy.label(),
                result.user_rank,
                result.user_avg_h * 100.0
            );
        }
        println!();
    }

    println!("\nSUMMARY");
    println!(
        "{:<18} {:>8} {:>8} {:>9} {:>10} {:>10} {:>9} {:>11} {:>8}",
        "policy",
        "#1 rate",
        "top-3",
        "avg rank",
        "avg H",
        "avg cats",
        "$ left",
        "avg DURANT",
        "$1 slots"
    );
    for policy in UserPolicy::ALL {
        let summary = aggregates
            .get(policy.label())
            .context("missing policy aggregate")?;
        let n = summary.runs as f64;
        println!(
            "{:<18} {:>7.1}% {:>7.1}% {:>9.2} {:>9.2}% {:>10.3} {:>9.1} {:>11.3} {:>8.2}",
            policy.label(),
            summary.wins as f64 * 100.0 / n,
            summary.top3 as f64 * 100.0 / n,
            summary.rank_sum / n,
            summary.h_sum * 100.0 / n,
            summary.cats_sum / n,
            summary.budget_sum / n,
            summary.durant_sum / n,
            summary.one_dollar_sum / n,
        );
    }

    println!(
        "\nInterpretation: compare the three BirdBoard policies primarily by paired avg rank / avg H. "
    );
    println!(
        "If one policy is clearly better here, the next validation step is a true nomination+bidding simulator and an out-of-sample weekly-stat backtest."
    );

    Ok(())
}

fn simulate_one_auction(
    app: &App,
    candidate_pool: &[PlayerId],
    global_weights: &[[f64; 9]],
    compact_weights: &[[f64; 9]],
    policy: UserPolicy,
    seed: u64,
) -> Result<SimOutcome> {
    let mut state = SimState::new(candidate_pool);
    let mut rng = SimRng::new(seed);

    // Give every opponent a stable but different strategic personality for the
    // whole auction. The same seed/personalities are reused for every user
    // policy, which makes comparisons substantially less noisy.
    let opponent_styles = (0..TEAM_COUNT)
        .map(|team| {
            let index =
                ((seed as usize).wrapping_add(team.wrapping_mul(97))) % global_weights.len();
            global_weights[index]
        })
        .collect::<Vec<_>>();

    for _round in 0..ROSTER_SIZE {
        for team in 0..TEAM_COUNT {
            if state.open_slots(team) == 0 {
                continue;
            }

            let market = market_for_team(app, candidate_pool, &state, team);
            let choice = if team == 0 {
                choose_user_player(
                    app,
                    candidate_pool,
                    &state,
                    &market,
                    policy,
                    global_weights,
                    compact_weights,
                )
            } else {
                choose_reasonable_opponent_player(
                    app,
                    candidate_pool,
                    &state,
                    &market,
                    team,
                    opponent_styles[team],
                    &mut rng,
                )
            }
            .with_context(|| format!("no legal player for team {team}"))?;

            let market_price = market_price_for(&market, &choice).unwrap_or(MIN_BID);
            let price = market_price.clamp(MIN_BID, state.max_legal_bid(team));
            state.buy(team, choice, price)?;
        }
    }

    let result = score_final_league(app, &state)?;
    Ok(SimOutcome { result, state })
}

fn market_for_team(
    app: &App,
    candidate_pool: &[PlayerId],
    state: &SimState,
    team: usize,
) -> MarketBoard {
    let candidates = candidate_pool
        .iter()
        .filter(|player_id| state.available.contains(*player_id))
        .cloned()
        .collect::<Vec<_>>();
    let opponent_rosters = state
        .rosters
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != team)
        .map(|(_, roster)| roster.clone())
        .collect::<Vec<_>>();
    let opponent_budgets = state
        .budgets
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != team)
        .map(|(_, budget)| *budget)
        .collect::<Vec<_>>();

    if let Some(equilibrium) = &app.equilibrium_market {
        app.durant.market_board_with_equilibrium_prices(
            &state.rosters[team],
            state.budgets[team],
            &opponent_rosters,
            &opponent_budgets,
            &candidates,
            &equilibrium.prices,
            AuctionConfig::default(),
        )
    } else {
        app.durant.market_board(
            &state.rosters[team],
            state.budgets[team],
            &opponent_rosters,
            &opponent_budgets,
            &candidates,
            AuctionConfig::default(),
        )
    }
}

fn market_price_for(market: &MarketBoard, player_id: &PlayerId) -> Option<u16> {
    market
        .values
        .iter()
        .find(|value| &value.player_id == player_id)
        .map(|value| value.market_price)
}

fn legal_candidates(
    candidate_pool: &[PlayerId],
    state: &SimState,
    team: usize,
    market: &MarketBoard,
) -> Vec<PlayerId> {
    let max_bid = state.max_legal_bid(team);
    candidate_pool
        .iter()
        .filter(|player_id| state.available.contains(*player_id))
        .filter(|player_id| {
            market_price_for(market, player_id)
                .map(|price| price <= max_bid)
                .unwrap_or(false)
        })
        .cloned()
        .collect()
}

fn opponent_views(state: &SimState, team: usize) -> (Vec<Vec<PlayerId>>, Vec<u16>) {
    let rosters = state
        .rosters
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != team)
        .map(|(_, roster)| roster.clone())
        .collect::<Vec<_>>();
    let budgets = state
        .budgets
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != team)
        .map(|(_, budget)| *budget)
        .collect::<Vec<_>>();
    (rosters, budgets)
}

fn choose_user_player(
    app: &App,
    candidate_pool: &[PlayerId],
    state: &SimState,
    market: &MarketBoard,
    policy: UserPolicy,
    global_weights: &[[f64; 9]],
    compact_weights: &[[f64; 9]],
) -> Option<PlayerId> {
    let team = 0;
    let legal = legal_candidates(candidate_pool, state, team, market);
    if legal.is_empty() {
        return None;
    }

    match policy {
        UserPolicy::DurantValue => legal.into_iter().max_by(|left, right| {
            let left_score = value_score(app, market, left);
            let right_score = value_score(app, market, right);
            left_score.total_cmp(&right_score)
        }),
        UserPolicy::NowDeltaH => {
            let (opponent_rosters, _) = opponent_views(state, team);
            app.durant
                .immediate_market_advantage_scores(
                    &state.rosters[team],
                    &opponent_rosters,
                    &legal,
                    market,
                )
                .into_iter()
                .max_by(|left, right| {
                    left.marginal_immediate_matchup_win_probability
                        .total_cmp(&right.marginal_immediate_matchup_win_probability)
                        .then_with(|| {
                            left.immediate_matchup_win_probability
                                .total_cmp(&right.immediate_matchup_win_probability)
                        })
                })
                .map(|score| score.player_id)
        }
        UserPolicy::FinalDeltaH => {
            let (opponent_rosters, opponent_budgets) = opponent_views(state, team);
            let weights = runtime_weights_for_state(
                app,
                &state.rosters[team],
                state.budgets[team],
                &opponent_rosters,
                &opponent_budgets,
                &legal,
                global_weights,
                compact_weights,
            );
            if weights.is_empty() {
                return choose_user_player(
                    app,
                    candidate_pool,
                    state,
                    market,
                    UserPolicy::NowDeltaH,
                    global_weights,
                    compact_weights,
                );
            }

            let mut immediate = app.durant.immediate_market_advantage_scores(
                &state.rosters[team],
                &opponent_rosters,
                &legal,
                market,
            );
            immediate.sort_by(|left, right| {
                right
                    .marginal_immediate_matchup_win_probability
                    .total_cmp(&left.marginal_immediate_matchup_win_probability)
            });
            let shortlist = immediate
                .iter()
                .take(FINAL_SHORTLIST)
                .map(|score| score.player_id.clone())
                .collect::<Vec<_>>();

            app.durant
                .coarse_market_advantage_scores_with_strategy_weights(
                    &state.rosters[team],
                    state.budgets[team],
                    &opponent_rosters,
                    &opponent_budgets,
                    &legal,
                    &shortlist,
                    &weights,
                    AuctionConfig::default(),
                )
                .ok()?
                .into_iter()
                .filter(|score| score.has_projected_finish)
                .max_by(|left, right| {
                    left.marginal_projected_matchup_win_probability
                        .total_cmp(&right.marginal_projected_matchup_win_probability)
                        .then_with(|| {
                            left.buy_projected_matchup_win_probability
                                .total_cmp(&right.buy_projected_matchup_win_probability)
                        })
                })
                .map(|score| score.player_id)
                .or_else(|| {
                    choose_user_player(
                        app,
                        candidate_pool,
                        state,
                        market,
                        UserPolicy::NowDeltaH,
                        global_weights,
                        compact_weights,
                    )
                })
        }
        UserPolicy::TeamStrategy => {
            let (opponent_rosters, opponent_budgets) = opponent_views(state, team);
            let weights = runtime_weights_for_state(
                app,
                &state.rosters[team],
                state.budgets[team],
                &opponent_rosters,
                &opponent_budgets,
                &legal,
                global_weights,
                compact_weights,
            );
            let plan = app.durant.roster_plan_with_strategy_weights(
                &state.rosters[team],
                state.budgets[team],
                &opponent_rosters,
                &opponent_budgets,
                &legal,
                &weights,
                AuctionConfig::default(),
            );

            plan.and_then(|plan| {
                plan.projected_future_players.into_iter().find(|player_id| {
                    state.available.contains(player_id)
                        && market_price_for(market, player_id)
                            .map(|price| price <= state.max_legal_bid(team))
                            .unwrap_or(false)
                })
            })
            .or_else(|| {
                choose_user_player(
                    app,
                    candidate_pool,
                    state,
                    market,
                    UserPolicy::NowDeltaH,
                    global_weights,
                    compact_weights,
                )
            })
        }
    }
}

fn runtime_weights_for_state(
    app: &App,
    own_roster: &[PlayerId],
    own_budget: u16,
    opponent_rosters: &[Vec<PlayerId>],
    opponent_budgets: &[u16],
    candidates: &[PlayerId],
    global_weights: &[[f64; 9]],
    compact_weights: &[[f64; 9]],
) -> Vec<[f64; 9]> {
    runtime_weights_for_model(
        &app.durant,
        own_roster,
        own_budget,
        opponent_rosters,
        opponent_budgets,
        candidates,
        global_weights,
        compact_weights,
    )
}

fn runtime_weights_for_model(
    model: &DurantModel,
    own_roster: &[PlayerId],
    own_budget: u16,
    opponent_rosters: &[Vec<PlayerId>],
    opponent_budgets: &[u16],
    candidates: &[PlayerId],
    global_weights: &[[f64; 9]],
    compact_weights: &[[f64; 9]],
) -> Vec<[f64; 9]> {
    match RuntimeStrategyStage::for_roster_size(own_roster.len()) {
        RuntimeStrategyStage::EarlyGlobal => global_weights.to_vec(),
        RuntimeStrategyStage::LateCompact => compact_weights.to_vec(),
        RuntimeStrategyStage::MidAdaptive => {
            let seeds = model.coarse_strategy_seed_weights(
                own_roster,
                own_budget,
                opponent_rosters,
                opponent_budgets,
                candidates,
                global_weights,
                AuctionConfig::default(),
                24,
            );
            build_mid_adaptive_weights(global_weights, &seeds)
        }
    }
}

fn choose_reasonable_opponent_player(
    app: &App,
    candidate_pool: &[PlayerId],
    state: &SimState,
    market: &MarketBoard,
    team: usize,
    strategy: [f64; 9],
    rng: &mut SimRng,
) -> Option<PlayerId> {
    let mut ranked = legal_candidates(candidate_pool, state, team, market)
        .into_iter()
        .filter_map(|player_id| {
            let x = app.durant.x_score_for(&player_id)?;
            let static_score = app.durant.score_for(&player_id)?.total;
            let price = market_price_for(market, &player_id)? as f64;
            let weighted_g = x
                .iter()
                .zip(strategy.iter())
                .map(|(value, weight)| value * weight)
                .sum::<f64>();

            // Bots are intentionally competent but not omniscient: category
            // direction + overall player quality + price discipline.
            let jitter = (rng.unit() - 0.5) * 0.30;
            let score = weighted_g + 0.25 * static_score - 0.025 * price + jitter;
            Some((player_id, score))
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|left, right| right.1.total_cmp(&left.1));
    let top_len = ranked.len().min(4);
    if top_len == 0 {
        return None;
    }
    let index = rng.choose_top_index(top_len);
    Some(ranked[index].0.clone())
}

fn value_score(app: &App, market: &MarketBoard, player_id: &PlayerId) -> f64 {
    let durant = app
        .durant
        .score_for(player_id)
        .map(|score| score.total)
        .unwrap_or(f64::NEG_INFINITY);
    let price = market_price_for(market, player_id).unwrap_or(MIN_BID) as f64;
    durant - 0.025 * price
}

fn score_final_league(app: &App, state: &SimState) -> Result<LeagueResult> {
    if state
        .rosters
        .iter()
        .any(|roster| roster.len() != ROSTER_SIZE)
    {
        bail!("simulation ended with an incomplete roster");
    }

    let mut avg_h = [0.0f64; TEAM_COUNT];
    let mut avg_categories = [0.0f64; TEAM_COUNT];
    let mut category_sums = [[0.0f64; 9]; TEAM_COUNT];

    for team in 0..TEAM_COUNT {
        let mut h_sum = 0.0;
        let mut cats_sum = 0.0;
        for opponent in 0..TEAM_COUNT {
            if opponent == team {
                continue;
            }
            let (category_probabilities, h) = app
                .durant
                .validation_complete_matchup_prediction(
                    &state.rosters[team],
                    &state.rosters[opponent],
                )
                .with_context(|| {
                    format!("unable to evaluate final matchup {team} vs {opponent}")
                })?;
            h_sum += h;
            cats_sum += category_probabilities.iter().sum::<f64>();
            for (index, probability) in category_probabilities.iter().enumerate() {
                category_sums[team][index] += probability;
            }
        }
        avg_h[team] = h_sum / (TEAM_COUNT - 1) as f64;
        avg_categories[team] = cats_sum / (TEAM_COUNT - 1) as f64;
    }

    let user_h = avg_h[0];
    let user_rank = 1 + avg_h
        .iter()
        .skip(1)
        .filter(|&&opponent_h| opponent_h > user_h)
        .count();

    let user_avg_durant = state.rosters[0]
        .iter()
        .filter_map(|player_id| app.durant.score_for(player_id).map(|score| score.total))
        .sum::<f64>()
        / ROSTER_SIZE as f64;
    let user_one_dollar_slots = state.prices[0]
        .iter()
        .filter(|&&price| price == MIN_BID)
        .count();

    Ok(LeagueResult {
        user_rank,
        user_avg_h: user_h,
        user_avg_categories: avg_categories[0],
        user_category_win_probabilities: category_sums[0].map(|sum| sum / (TEAM_COUNT - 1) as f64),
        user_budget_left: state.budgets[0],
        user_avg_durant,
        user_one_dollar_slots,
    })
}

#[test]
#[ignore = "slow diagnostic; run explicitly in --release mode"]
fn auction_policy_roster_diagnostic() -> Result<()> {
    let stats = stats::load_or_fetch()?;
    let app = App::new(stats)?;
    let bank = app
        .strategy_bank
        .as_ref()
        .context("simulation requires a runtime strategy bank")?;

    let candidate_pool = app
        .durant
        .scores
        .iter()
        .take(220)
        .map(|score| score.player_id.clone())
        .collect::<Vec<_>>();
    if candidate_pool.len() < TEAM_COUNT * ROSTER_SIZE {
        bail!(
            "simulation needs at least {} scored players, found {}",
            TEAM_COUNT * ROSTER_SIZE,
            candidate_pool.len()
        );
    }

    let global_weights = bank.early_weights_or_fallback();
    if global_weights.is_empty() {
        bail!("runtime strategy bank contains no weights");
    }

    let seed = 0xB1AD_B04Du64;
    println!("\nBirdBoard representative roster diagnostic");
    println!(
        "One matched auction environment; every policy sees the same opponent personalities/random seed.\n"
    );

    for policy in UserPolicy::ALL {
        let outcome = simulate_one_auction(
            &app,
            &candidate_pool,
            global_weights,
            bank.weights.as_slice(),
            policy,
            seed,
        )?;
        print_policy_roster(&app, policy, &outcome);
    }

    Ok(())
}

fn print_policy_roster(app: &App, policy: UserPolicy, outcome: &SimOutcome) {
    let result = outcome.result;
    let roster = &outcome.state.rosters[0];
    let prices = &outcome.state.prices[0];
    let spent = STARTING_BUDGET.saturating_sub(outcome.state.budgets[0]);

    println!("============================================================");
    println!("{}", policy.label());
    println!(
        "rank #{} | H {:.1}% | expected cats {:.3} | spent ${} | left ${} | avg DURANT {:.3} | $1 slots {}",
        result.user_rank,
        result.user_avg_h * 100.0,
        result.user_avg_categories,
        spent,
        result.user_budget_left,
        result.user_avg_durant,
        result.user_one_dollar_slots
    );
    println!("\nPICKS");
    for (index, (player_id, price)) in roster.iter().zip(prices.iter()).enumerate() {
        let name = player_display_name(app, player_id);
        let durant = app
            .durant
            .score_for(player_id)
            .map(|score| format!("{:+.3}", score.total))
            .unwrap_or_else(|| "—".to_string());
        println!(
            "{:>2}. {:<26} ${:>3}   DURANT {}",
            index + 1,
            name,
            price,
            durant
        );
    }

    println!("\nCATEGORY WIN PROBABILITIES vs the 12 simulated opponents");
    let labels = ["FG%", "FT%", "3PM", "PTS", "REB", "AST", "STL", "BLK", "TO"];
    for (label, probability) in labels
        .iter()
        .zip(result.user_category_win_probabilities.iter())
    {
        println!("  {:<4} {:>5.1}%", label, probability * 100.0);
    }
    println!();
}

fn player_display_name(app: &App, player_id: &PlayerId) -> String {
    app.players
        .iter()
        .find(|player| &player.id == player_id)
        .map(|player| player.display_name().to_string())
        .unwrap_or_else(|| player_id.0.clone())
}

#[derive(Debug, Clone)]
struct EspnAuctionEntry {
    full_name: String,
    average_price: f64,
}

#[derive(Debug, Clone, Copy, Default)]
struct RealWeekScore {
    wins: usize,
    losses: usize,
    ties: usize,
    rank: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct ActualWeeklyTotals {
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

impl ActualWeeklyTotals {
    fn add(&mut self, row: &crate::stats::PlayerWeeklyStats) {
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
        if self.fga <= f64::EPSILON {
            0.0
        } else {
            self.fgm / self.fga
        }
    }

    fn ft_pct(self) -> f64 {
        if self.fta <= f64::EPSILON {
            0.0
        } else {
            self.ftm / self.fta
        }
    }
}

#[test]
#[ignore = "network + slow strategy + five-week realized-stat backtest"]
fn espn_market_bots_vs_birdboard_strategy() -> Result<()> {
    let stats = stats::load_or_fetch()?;
    let app = App::new(stats)?;
    let bank = app
        .strategy_bank
        .as_ref()
        .context("ESPN simulation requires a runtime strategy bank")?;

    println!("\nESPN market-bot room vs BirdBoard TEAM strategy");
    println!("Fetching current ESPN average salary-cap prices...");

    let espn_entries = fetch_current_espn_auction_values()?;
    println!(
        "ESPN returned {} players with auction averages.",
        espn_entries.len()
    );

    let espn_prices = match_espn_prices_to_birdboard(&app, &espn_entries);
    println!(
        "Matched {} ESPN auction prices to BirdBoard player IDs.",
        espn_prices.len()
    );

    let weekly_ids = app
        .stats
        .weekly
        .iter()
        .map(|row| row.player_id.clone())
        .collect::<HashSet<_>>();

    // Keep this test genuinely scoreable by the historical weekly file. A
    // current ESPN player with no source-season weekly row is excluded rather
    // than silently receiving five weeks of zero production.
    let candidate_pool = app
        .durant
        .scores
        .iter()
        .map(|score| score.player_id.clone())
        .filter(|player_id| weekly_ids.contains(player_id))
        .filter(|player_id| espn_prices.contains_key(player_id))
        .take(240)
        .collect::<Vec<_>>();

    if candidate_pool.len() < TEAM_COUNT * ROSTER_SIZE {
        bail!(
            "only {} players overlap BirdBoard + ESPN auction averages + weekly history; need at least {}",
            candidate_pool.len(),
            TEAM_COUNT * ROSTER_SIZE
        );
    }

    println!(
        "Using {} players with BOTH current ESPN price data and source-season weekly history.",
        candidate_pool.len()
    );
    print_espn_price_sample(&app, &candidate_pool, &espn_prices);

    let global_weights = bank.early_weights_or_fallback();
    if global_weights.is_empty() {
        bail!("runtime strategy bank contains no weights");
    }

    let seed = 0xE5A1_B04Du64;
    let state = simulate_espn_price_room(
        &app,
        &candidate_pool,
        &espn_prices,
        global_weights,
        bank.weights.as_slice(),
        seed,
    )?;

    println!("\nDRAFTED ROSTERS");
    println!("BirdBoard strategy roster:");
    print_sim_roster(&app, &state, 0);
    println!("\nRepresentative ESPN-price bot roster:");
    print_sim_roster(&app, &state, 1);

    let weeks = choose_five_random_scoreable_weeks(&app, &state, seed)?;
    println!(
        "\nFive deterministic random source-season weeks: {:?}",
        weeks
    );
    println!(
        "Missing player-week row = zero production, matching a calendar-week fantasy matchup."
    );

    let rows_by_week = weekly_rows_by_week_for_sim(&app.stats.weekly);
    let mut birdboard_match_wins = 0usize;
    let mut birdboard_match_losses = 0usize;
    let mut birdboard_match_ties = 0usize;
    let mut birdboard_week_wins = 0usize;

    println!("\nREALIZED WEEKLY RESULTS");
    println!("week   BB record vs 12 bots   league rank   weekly winner");
    for week in weeks {
        let week_rows = rows_by_week
            .get(&week)
            .with_context(|| format!("missing weekly rows for week {week}"))?;
        let results = score_realized_week(&state, week_rows);
        let bb = results[0];
        birdboard_match_wins += bb.wins;
        birdboard_match_losses += bb.losses;
        birdboard_match_ties += bb.ties;
        if bb.rank == 1 {
            birdboard_week_wins += 1;
        }
        let winner = results
            .iter()
            .enumerate()
            .min_by_key(|(_, result)| result.rank)
            .map(|(index, _)| index)
            .unwrap_or(0);
        println!(
            "{:>4}   {:>2}-{:>2}-{:>2}               #{:<2}          {}",
            week,
            bb.wins,
            bb.losses,
            bb.ties,
            bb.rank,
            if winner == 0 { "BirdBoard" } else { "ESPN bot" }
        );
    }

    let decided = birdboard_match_wins + birdboard_match_losses + birdboard_match_ties;
    let bb_points = birdboard_match_wins as f64 + 0.5 * birdboard_match_ties as f64;
    let bb_rate = if decided == 0 {
        0.0
    } else {
        bb_points / decided as f64
    };

    println!("\nSUMMARY");
    println!(
        "BirdBoard TEAM strategy: {} weekly league wins / 5",
        birdboard_week_wins
    );
    println!(
        "Head-to-head vs ESPN-price bots across 5 weeks: {}-{}-{} ({:.1}% matchup points)",
        birdboard_match_wins,
        birdboard_match_losses,
        birdboard_match_ties,
        bb_rate * 100.0
    );
    println!(
        "ESPN-price field produced the weekly #1 team in {} / 5 weeks.",
        5usize.saturating_sub(birdboard_week_wins)
    );
    println!("\nImportant: ESPN does not publish its proprietary mock-bot decision algorithm. ");
    println!(
        "These 12 opponents therefore use ESPN's CURRENT average auction prices as their valuation model, "
    );
    println!(
        "with small drafting noise. This is a market-model test, not a literal reverse-engineering of ESPN bots."
    );
    println!(
        "The five weeks come from BirdBoard's source-season weekly cache, so this is useful realized-week validation, "
    );
    println!(
        "but it is not perfectly out-of-sample because the same source season contributes to BirdBoard's projections."
    );

    Ok(())
}

fn fetch_current_espn_auction_values() -> Result<Vec<EspnAuctionEntry>> {
    let client = Client::builder()
        .user_agent("Mozilla/5.0 BirdBoard validation")
        .build()
        .context("unable to build ESPN HTTP client")?;

    let current_season = std::env::var("BIRDBOARD_ESPN_SEASON")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .or_else(|| {
            client
                .get("https://lm-api-reads.fantasy.espn.com/apis/v3/games/fba")
                .send()
                .ok()?
                .json::<Value>()
                .ok()?
                .get("currentSeasonId")?
                .as_u64()
                .map(|value| value as u32)
        })
        .unwrap_or(2027);

    let url = format!(
        "https://lm-api-reads.fantasy.espn.com/apis/v3/games/fba/seasons/{current_season}/segments/0/leaguedefaults/3"
    );
    let filter = json!({
        "players": {
            "limit": 500,
            "offset": 0,
            "sortAverageAuction": {
                "sortAsc": false,
                "sortPriority": 1
            }
        }
    });

    let response = client
        .get(&url)
        .query(&[("view", "kona_player_info")])
        .header("x-fantasy-filter", filter.to_string())
        .send()
        .with_context(|| format!("unable to fetch ESPN fantasy player data from {url}"))?
        .error_for_status()
        .context("ESPN fantasy player-data request returned an error status")?;

    let payload: Value = response
        .json()
        .context("unable to decode ESPN fantasy player-data JSON")?;
    let rows = payload
        .get("players")
        .and_then(Value::as_array)
        .context("ESPN response does not contain players[]")?;

    let mut entries = Vec::new();
    for row in rows {
        let Some(player) = row.get("player") else {
            continue;
        };
        let Some(full_name) = player.get("fullName").and_then(Value::as_str) else {
            continue;
        };
        let Some(average_price) = player
            .get("ownership")
            .and_then(|ownership| ownership.get("auctionValueAverage"))
            .and_then(Value::as_f64)
        else {
            continue;
        };
        if average_price <= 0.0 {
            continue;
        }
        entries.push(EspnAuctionEntry {
            full_name: full_name.to_string(),
            average_price,
        });
    }

    if entries.len() < TEAM_COUNT * ROSTER_SIZE {
        bail!(
            "ESPN returned only {} positive auction averages for season {}; try setting BIRDBOARD_ESPN_SEASON explicitly",
            entries.len(),
            current_season
        );
    }

    Ok(entries)
}

fn match_espn_prices_to_birdboard(
    app: &App,
    entries: &[EspnAuctionEntry],
) -> HashMap<PlayerId, f64> {
    let mut exact = HashMap::<String, PlayerId>::new();
    let mut first_last = HashMap::<String, Vec<PlayerId>>::new();

    for player in &app.stats.players {
        exact.insert(
            normalize_person_name(&player.player_name),
            player.player_id.clone(),
        );
        first_last
            .entry(first_initial_last_key(&player.player_name))
            .or_default()
            .push(player.player_id.clone());
    }
    for player in &app.players {
        exact
            .entry(normalize_person_name(player.display_name()))
            .or_insert_with(|| player.id.clone());
        first_last
            .entry(first_initial_last_key(player.display_name()))
            .or_default()
            .push(player.id.clone());
    }

    let mut matched = HashMap::new();
    for entry in entries {
        let normalized = normalize_person_name(&entry.full_name);
        let player_id = exact.get(&normalized).cloned().or_else(|| {
            let key = first_initial_last_key(&entry.full_name);
            first_last.get(&key).and_then(|ids| {
                let mut unique = ids.clone();
                unique.sort_by(|left, right| left.0.cmp(&right.0));
                unique.dedup();
                if unique.len() == 1 {
                    unique.into_iter().next()
                } else {
                    None
                }
            })
        });
        if let Some(player_id) = player_id {
            matched.insert(player_id, entry.average_price.max(1.0));
        }
    }
    matched
}

fn normalize_person_name(name: &str) -> String {
    name.chars()
        .flat_map(|ch| match ch {
            'ć' | 'č' | 'Ć' | 'Č' => "c".chars().collect::<Vec<_>>(),
            'š' | 'Š' => "s".chars().collect::<Vec<_>>(),
            'ž' | 'Ž' => "z".chars().collect::<Vec<_>>(),
            'đ' | 'Đ' => "d".chars().collect::<Vec<_>>(),
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => {
                "e".chars().collect::<Vec<_>>()
            }
            'á' | 'à' | 'â' | 'ä' | 'Á' | 'À' | 'Â' | 'Ä' => {
                "a".chars().collect::<Vec<_>>()
            }
            'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => {
                "i".chars().collect::<Vec<_>>()
            }
            'ó' | 'ò' | 'ô' | 'ö' | 'Ó' | 'Ò' | 'Ô' | 'Ö' => {
                "o".chars().collect::<Vec<_>>()
            }
            'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => {
                "u".chars().collect::<Vec<_>>()
            }
            'ñ' | 'Ñ' => "n".chars().collect::<Vec<_>>(),
            _ => vec![ch],
        })
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect::<String>()
        .replace("jr", "")
        .replace("iii", "")
        .replace("ii", "")
}

fn first_initial_last_key(name: &str) -> String {
    let clean = name
        .replace('.', " ")
        .replace('-', " ")
        .replace('’', "'")
        .replace('\'', " ");
    let parts = clean
        .split_whitespace()
        .filter(|part| {
            !matches!(
                part.to_ascii_lowercase().as_str(),
                "jr" | "ii" | "iii" | "iv"
            )
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return String::new();
    }
    let first = parts[0].chars().next().unwrap_or_default();
    let last = parts[parts.len() - 1];
    normalize_person_name(&format!("{first}{last}"))
}

fn espn_price_for(prices: &HashMap<PlayerId, f64>, player_id: &PlayerId) -> Option<u16> {
    prices
        .get(player_id)
        .copied()
        .map(|value| value.round().clamp(1.0, STARTING_BUDGET as f64) as u16)
}

fn espn_market_for_team(
    app: &App,
    candidate_pool: &[PlayerId],
    state: &SimState,
    team: usize,
    prices: &HashMap<PlayerId, f64>,
) -> MarketBoard {
    let mut market = market_for_team(app, candidate_pool, state, team);
    for value in &mut market.values {
        if let Some(price) = prices.get(&value.player_id).copied() {
            value.market_price_exact = price.max(1.0);
            value.market_price = price.round().clamp(1.0, STARTING_BUDGET as f64) as u16;
        }
    }
    market
}

fn simulate_espn_price_room(
    app: &App,
    candidate_pool: &[PlayerId],
    prices: &HashMap<PlayerId, f64>,
    global_weights: &[[f64; 9]],
    compact_weights: &[[f64; 9]],
    seed: u64,
) -> Result<SimState> {
    let mut state = SimState::new(candidate_pool);
    let mut rng = SimRng::new(seed);

    for _round in 0..ROSTER_SIZE {
        // Rotate first buyer by round to avoid permanently privileging team 0.
        let start_team = (_round + (seed as usize % TEAM_COUNT)) % TEAM_COUNT;
        for offset in 0..TEAM_COUNT {
            let team = (start_team + offset) % TEAM_COUNT;
            if state.open_slots(team) == 0 {
                continue;
            }

            let market = espn_market_for_team(app, candidate_pool, &state, team, prices);
            let choice = if team == 0 {
                choose_user_player(
                    app,
                    candidate_pool,
                    &state,
                    &market,
                    UserPolicy::TeamStrategy,
                    global_weights,
                    compact_weights,
                )
            } else {
                choose_espn_value_bot(candidate_pool, &state, team, prices, &mut rng)
            }
            .with_context(|| format!("no ESPN-room legal player for team {team}"))?;

            let price = espn_price_for(prices, &choice)
                .unwrap_or(MIN_BID)
                .clamp(MIN_BID, state.max_legal_bid(team));
            state.buy(team, choice, price)?;
        }
    }

    Ok(state)
}

fn choose_espn_value_bot(
    candidate_pool: &[PlayerId],
    state: &SimState,
    team: usize,
    prices: &HashMap<PlayerId, f64>,
    rng: &mut SimRng,
) -> Option<PlayerId> {
    let max_bid = state.max_legal_bid(team);
    let mut ranked = candidate_pool
        .iter()
        .filter(|player_id| state.available.contains(*player_id))
        .filter_map(|player_id| {
            let price = prices.get(player_id).copied()?;
            if price.round() as u16 > max_bid {
                return None;
            }
            // ESPN average auction price is the bot's valuation signal. Small
            // room noise prevents twelve bots from being clones while keeping
            // them anchored tightly to ESPN's market ordering.
            let jitter = (rng.unit() - 0.5) * 5.0;
            Some((player_id.clone(), price + jitter))
        })
        .collect::<Vec<_>>();

    ranked.sort_by(|left, right| right.1.total_cmp(&left.1));
    let top_len = ranked.len().min(5);
    if top_len == 0 {
        return None;
    }
    let index = rng.choose_top_index(top_len);
    Some(ranked[index].0.clone())
}

fn print_espn_price_sample(
    app: &App,
    candidate_pool: &[PlayerId],
    prices: &HashMap<PlayerId, f64>,
) {
    println!("\nESPN PRICE SAMPLE");
    for player_id in candidate_pool.iter().take(12) {
        if let Some(price) = prices.get(player_id) {
            println!(
                "  {:<26} ${:>5.1}",
                player_display_name(app, player_id),
                price
            );
        }
    }
}

fn print_sim_roster(app: &App, state: &SimState, team: usize) {
    for (index, (player_id, price)) in state.rosters[team]
        .iter()
        .zip(state.prices[team].iter())
        .enumerate()
    {
        println!(
            "  {:>2}. {:<26} ${:>3}",
            index + 1,
            player_display_name(app, player_id),
            price
        );
    }
    println!("  left: ${}", state.budgets[team]);
}

fn weekly_rows_by_week_for_sim(
    rows: &[crate::stats::PlayerWeeklyStats],
) -> HashMap<u32, HashMap<PlayerId, crate::stats::PlayerWeeklyStats>> {
    let mut by_week = HashMap::new();
    for row in rows {
        by_week
            .entry(row.week)
            .or_insert_with(HashMap::new)
            .insert(row.player_id.clone(), row.clone());
    }
    by_week
}

fn choose_five_random_scoreable_weeks(app: &App, state: &SimState, seed: u64) -> Result<Vec<u32>> {
    let by_week = weekly_rows_by_week_for_sim(&app.stats.weekly);
    let roster_ids = state
        .rosters
        .iter()
        .flat_map(|roster| roster.iter().cloned())
        .collect::<HashSet<_>>();

    let mut weeks = by_week
        .iter()
        .filter_map(|(&week, rows)| {
            let coverage = roster_ids
                .iter()
                .filter(|id| rows.contains_key(*id))
                .count();
            let ratio = coverage as f64 / roster_ids.len().max(1) as f64;
            (ratio >= 0.70).then_some(week)
        })
        .collect::<Vec<_>>();
    weeks.sort_unstable();

    if weeks.len() < 5 {
        bail!(
            "only {} source-season weeks have >=70% coverage of the drafted rosters; need 5",
            weeks.len()
        );
    }

    let mut rng = SimRng::new(seed ^ 0x51EE_5EED);
    for index in (1..weeks.len()).rev() {
        let swap_with = (rng.next_u64() as usize) % (index + 1);
        weeks.swap(index, swap_with);
    }
    weeks.truncate(5);
    weeks.sort_unstable();
    Ok(weeks)
}

fn score_realized_week(
    state: &SimState,
    week_rows: &HashMap<PlayerId, crate::stats::PlayerWeeklyStats>,
) -> [RealWeekScore; TEAM_COUNT] {
    let totals = std::array::from_fn::<ActualWeeklyTotals, TEAM_COUNT, _>(|team| {
        let mut total = ActualWeeklyTotals::default();
        for player_id in &state.rosters[team] {
            if let Some(row) = week_rows.get(player_id) {
                total.add(row);
            }
        }
        total
    });

    let mut scores = [RealWeekScore::default(); TEAM_COUNT];
    let mut matchup_points = [0.0f64; TEAM_COUNT];

    for left in 0..TEAM_COUNT {
        for right in (left + 1)..TEAM_COUNT {
            let outcome = actual_roster_matchup(totals[left], totals[right]);
            match outcome {
                1 => {
                    scores[left].wins += 1;
                    scores[right].losses += 1;
                    matchup_points[left] += 1.0;
                }
                -1 => {
                    scores[right].wins += 1;
                    scores[left].losses += 1;
                    matchup_points[right] += 1.0;
                }
                _ => {
                    scores[left].ties += 1;
                    scores[right].ties += 1;
                    matchup_points[left] += 0.5;
                    matchup_points[right] += 0.5;
                }
            }
        }
    }

    for team in 0..TEAM_COUNT {
        scores[team].rank = 1 + matchup_points
            .iter()
            .enumerate()
            .filter(|(other, points)| *other != team && **points > matchup_points[team])
            .count();
    }
    scores
}

fn actual_roster_matchup(left: ActualWeeklyTotals, right: ActualWeeklyTotals) -> i8 {
    let left_values = [
        left.fg_pct(),
        left.ft_pct(),
        left.threes,
        left.points,
        left.rebounds,
        left.assists,
        left.steals,
        left.blocks,
        left.turnovers,
    ];
    let right_values = [
        right.fg_pct(),
        right.ft_pct(),
        right.threes,
        right.points,
        right.rebounds,
        right.assists,
        right.steals,
        right.blocks,
        right.turnovers,
    ];

    let mut left_wins = 0usize;
    let mut right_wins = 0usize;
    for category in 0..9 {
        let ordering = if category == 8 {
            compare_real_number(right_values[category], left_values[category])
        } else {
            compare_real_number(left_values[category], right_values[category])
        };
        if ordering > 0 {
            left_wins += 1;
        } else if ordering < 0 {
            right_wins += 1;
        }
    }

    if left_wins > right_wins {
        1
    } else if right_wins > left_wins {
        -1
    } else {
        0
    }
}

fn compare_real_number(left: f64, right: f64) -> i8 {
    if (left - right).abs() <= 1e-12 {
        0
    } else if left > right {
        1
    } else {
        -1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompetitiveBirdBoardBidMode {
    /// TEAM strategy and Max Bid both use ESPN prices only as the expected
    /// future-price environment.  Actual willingness to pay is still solved by
    /// H-indifference and can be above or below the market estimate.
    EspnClearingMarket,
    /// Same TEAM strategy and H-indifference Max Bid algorithm, but future
    /// opportunity costs use BirdBoard's independent synthetic price prior.
    IndependentEquilibriumMarket,
}

impl CompetitiveBirdBoardBidMode {
    const ALL: [Self; 2] = [Self::EspnClearingMarket, Self::IndependentEquilibriumMarket];

    fn label(self) -> &'static str {
        match self {
            Self::EspnClearingMarket => "TEAM @ ESPN market",
            Self::IndependentEquilibriumMarket => "TEAM @ BirdBoard market",
        }
    }
}

#[derive(Default)]
struct CompetitiveWeeklyAggregate {
    rooms: usize,
    weekly_samples: usize,
    weekly_wins: usize,
    weekly_rank_sum: f64,
    matchup_wins: usize,
    matchup_losses: usize,
    matchup_ties: usize,
    bb_budget_left_sum: f64,
    field_budget_left_sum: f64,
    max_field_budget_left: u16,
    total_room_spend_sum: f64,
}

impl CompetitiveWeeklyAggregate {
    fn push_room_budget(&mut self, state: &SimState) {
        self.rooms += 1;
        self.bb_budget_left_sum += state.budgets[0] as f64;
        let field_count = TEAM_COUNT.saturating_sub(1).max(1);
        self.field_budget_left_sum +=
            state.budgets.iter().skip(1).copied().sum::<u16>() as f64 / field_count as f64;
        self.max_field_budget_left = self
            .max_field_budget_left
            .max(state.budgets.iter().skip(1).copied().max().unwrap_or(0));
        self.total_room_spend_sum += (TEAM_COUNT as u16 * STARTING_BUDGET
            - state.budgets.iter().copied().sum::<u16>())
            as f64;
    }

    fn push_week(&mut self, score: RealWeekScore) {
        self.weekly_samples += 1;
        self.weekly_wins += usize::from(score.rank == 1);
        self.weekly_rank_sum += score.rank as f64;
        self.matchup_wins += score.wins;
        self.matchup_losses += score.losses;
        self.matchup_ties += score.ties;
    }
}

#[test]
#[ignore = "network + independent-market comparison + competitive auction + realized weekly backtest"]
fn independent_equilibrium_market_vs_espn() -> Result<()> {
    let rooms = std::env::var("BIRDBOARD_ESPN_ROOMS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(10)
        .max(1);

    let stats = stats::load_or_fetch()?;
    let app = App::new(stats)?;
    let equilibrium = app.equilibrium_market.as_ref().with_context(|| {
        format!(
            "no independent synthetic market found; first run `BIRDBOARD_MARKET_ROOMS=120 BIRDBOARD_MARKET_PLAYERS=260 cargo test --release build_independent_equilibrium_market -- --ignored --nocapture` (expected {})",
            equilibrium_market::path_for_season(&app.stats.draft_season).display()
        )
    })?;
    let bank = app
        .strategy_bank
        .as_ref()
        .context("competitive ESPN simulation requires a runtime strategy bank")?;

    println!("\nIndependent BirdBoard synthetic market vs ESPN");
    println!(
        "Loaded {} independent synthetic-market prices from {}.",
        equilibrium.prices.len(),
        equilibrium_market::path_for_season(&app.stats.draft_season).display()
    );
    println!("Fetching CURRENT ESPN average salary-cap prices for validation only...");
    let espn_entries = fetch_current_espn_auction_values()?;
    let espn_prices = match_espn_prices_to_birdboard(&app, &espn_entries);
    println!(
        "ESPN returned {} auction averages; {} matched BirdBoard IDs.",
        espn_entries.len(),
        espn_prices.len()
    );

    print_equilibrium_vs_espn_comparison(&app, &espn_prices);

    let weekly_ids = app
        .stats
        .weekly
        .iter()
        .map(|row| row.player_id.clone())
        .collect::<HashSet<_>>();
    let candidate_pool = app
        .durant
        .scores
        .iter()
        .map(|score| score.player_id.clone())
        .filter(|player_id| weekly_ids.contains(player_id))
        .filter(|player_id| espn_prices.contains_key(player_id))
        .filter(|player_id| equilibrium.prices.contains_key(player_id))
        .take(240)
        .collect::<Vec<_>>();

    if candidate_pool.len() < TEAM_COUNT * ROSTER_SIZE {
        bail!(
            "only {} players overlap BirdBoard equilibrium + ESPN + weekly history; need at least {}",
            candidate_pool.len(),
            TEAM_COUNT * ROSTER_SIZE
        );
    }

    let global_weights = bank.early_weights_or_fallback();
    if global_weights.is_empty() {
        bail!("runtime strategy bank contains no weights");
    }

    print_espn_room_calibration(&app, &candidate_pool, &espn_prices, global_weights)?;

    let rows_by_week = weekly_rows_by_week_for_sim(&app.stats.weekly);
    println!(
        "\nUsing {} players with independent prices + ESPN prices + weekly history.",
        candidate_pool.len()
    );
    println!(
        "Auction model: rotating nominations, all teams submit legal max bids, winner pays second-highest max + $1."
    );
    println!(
        "ESPN opponents use current ESPN averages + relative live room inflation anchored to 1.0 + team-specific finite-roster budget pressure that is zero at the opening state and reacts to both the bot's own roster progress and the room's remaining competitive runway."
    );
    println!(
        "Both variants use the SAME H-indifference Max Bid rule. They differ only in the expected future-price prior used by TEAM planning and BUY-vs-PASS opportunity cost: ESPN vs BirdBoard synthetic market."
    );
    println!(
        "Roster completion v77: all 13 slots are real opportunity-cost slots; first 10 get expensive strategy search, remaining slots use cheap greedy real-player completion (replacement only if necessary)."
    );
    println!(
        "Auction semantics v82: MARKET predicts future cost; ESPN opening dollars remain absolute; opponent bots add only endogenous stranded-cash pressure from their own remaining cash/slots plus the room's shrinking competitive runway; Max Bid is H-indifference, target membership never caps bidding, off-plan bargains may be bought, and every nomination carries the nominator's mandatory $1 opening bid."
    );
    println!(
        "Each room is scored on five deterministic random realized source-season weeks; {} rooms = {} weekly league outcomes.\n",
        rooms,
        rooms * 5
    );

    let mut aggregates = HashMap::<&'static str, CompetitiveWeeklyAggregate>::new();

    for room in 0..rooms {
        let seed = 0xC0A7_2027u64.wrapping_add((room as u64).wrapping_mul(104_729));
        let validation_weeks =
            choose_five_pool_calendar_weeks(&app, &candidate_pool, seed ^ 0xA11C_7100u64)?;
        println!("ROOM {:>2}/{rooms} weeks {:?}", room + 1, validation_weeks);

        for mode in CompetitiveBirdBoardBidMode::ALL {
            let state = simulate_competitive_espn_room(
                &app,
                &candidate_pool,
                &espn_prices,
                global_weights,
                bank.weights.as_slice(),
                mode,
                seed,
            )?;
            validate_competitive_room(&state)?;

            if room == 0 {
                print_competitive_birdboard_roster(&app, &state, mode);
            }

            let total_left = state.budgets.iter().copied().sum::<u16>();
            let field_count = TEAM_COUNT.saturating_sub(1).max(1);
            let field_avg_left =
                state.budgets.iter().skip(1).copied().sum::<u16>() as f64 / field_count as f64;
            let field_max_left = state.budgets.iter().skip(1).copied().max().unwrap_or(0);
            let spend = TEAM_COUNT as u16 * STARTING_BUDGET - total_left;

            let aggregate = aggregates.entry(mode.label()).or_default();
            aggregate.push_room_budget(&state);

            let mut room_rank_sum = 0usize;
            let mut room_wins = 0usize;
            for week in &validation_weeks {
                let week_rows = rows_by_week
                    .get(week)
                    .with_context(|| format!("missing weekly rows for week {week}"))?;
                let realized = score_realized_week(&state, week_rows);
                room_rank_sum += realized[0].rank;
                room_wins += usize::from(realized[0].rank == 1);
                aggregate.push_week(realized[0]);
            }

            println!(
                "  {:<22} weekly #1 {:>1}/5 | avg rank {:>4.1} | BB left ${:>3} | field avg left ${:>4.1} | field max ${:>3} | spend ${}",
                mode.label(),
                room_wins,
                room_rank_sum as f64 / validation_weeks.len() as f64,
                state.budgets[0],
                field_avg_left,
                field_max_left,
                spend,
            );
        }
    }

    println!("\nREALIZED-WEEK COMPETITIVE AUCTION SUMMARY");
    println!(
        "{:<22} {:>10} {:>10} {:>12} {:>10} {:>12} {:>12} {:>10}",
        "BirdBoard mode",
        "week #1",
        "avg rank",
        "H2H points",
        "BB $left",
        "field $left",
        "field max",
        "room spend"
    );

    for mode in CompetitiveBirdBoardBidMode::ALL {
        let summary = aggregates
            .get(mode.label())
            .context("missing competitive aggregate")?;
        let weekly_n = summary.weekly_samples.max(1) as f64;
        let rooms_n = summary.rooms.max(1) as f64;
        let decided = summary.matchup_wins + summary.matchup_losses + summary.matchup_ties;
        let points = summary.matchup_wins as f64 + 0.5 * summary.matchup_ties as f64;
        let point_rate = if decided == 0 {
            0.0
        } else {
            points / decided as f64
        };

        println!(
            "{:<22} {:>8.1}% {:>10.2} {:>11.1}% {:>10.1} {:>12.1} {:>12} {:>10.0}",
            mode.label(),
            summary.weekly_wins as f64 * 100.0 / weekly_n,
            summary.weekly_rank_sum / weekly_n,
            point_rate * 100.0,
            summary.bb_budget_left_sum / rooms_n,
            summary.field_budget_left_sum / rooms_n,
            summary.max_field_budget_left,
            summary.total_room_spend_sum / rooms_n,
        );
    }

    println!("\nINTERPRETATION");
    println!(
        "PRICE comparison above is pure validation: ESPN values never enter the independent market build."
    );
    println!(
        "TEAM @ ESPN market uses ESPN only as BirdBoard's expected future-price prior; actual bids are H-indifference reservation values and actual sale prices still come from competition."
    );
    println!(
        "TEAM @ BirdBoard market uses the independent synthetic curve as the future-price prior with the same H-indifference bidding rule. This isolates whether BirdBoard's own price expectations improve decisions."
    );
    println!(
        "Weekly scoring uses realized player-week rows; missing rows count as zero production for that calendar matchup."
    );

    Ok(())
}

fn print_equilibrium_vs_espn_comparison(app: &App, espn_prices: &HashMap<PlayerId, f64>) {
    // Compare ESPN to the ACTUAL Preparation prices BirdBoard will show after
    // the saved equilibrium shape has been re-scaled to the full $2,600 room.
    let mut pairs = app
        .market_board
        .values
        .iter()
        .filter_map(|value| {
            let espn = espn_prices.get(&value.player_id)?;
            Some((value.player_id.clone(), value.market_price_exact, *espn))
        })
        .collect::<Vec<_>>();
    pairs.sort_by(|left, right| {
        right
            .2
            .total_cmp(&left.2)
            .then_with(|| left.0.0.cmp(&right.0.0))
    });

    if pairs.is_empty() {
        println!("No independent/ESPN price overlap.");
        return;
    }

    let n = pairs.len() as f64;
    let mae = pairs
        .iter()
        .map(|(_, birdboard, espn)| (birdboard - espn).abs())
        .sum::<f64>()
        / n;
    let bias = pairs
        .iter()
        .map(|(_, birdboard, espn)| birdboard - espn)
        .sum::<f64>()
        / n;
    let correlation = pearson_price_correlation(&pairs);

    println!("\nPRICE CURVE VALIDATION (ESPN was NOT used to fit BirdBoard)");
    println!("overlap: {} players", pairs.len());
    println!("Pearson correlation: {:.3}", correlation);
    println!("mean absolute difference: ${:.2}", mae);
    println!("mean BirdBoard - ESPN bias: {:+.2}", bias);
    println!("\nESPN-RANKED PRICE TIERS");
    println!(
        "{:<12} {:>12} {:>12} {:>10}",
        "tier", "BirdBoard", "ESPN", "BB-ESPN"
    );
    for (label, start, end) in [
        ("1-10", 0usize, 10usize),
        ("11-25", 10, 25),
        ("26-50", 25, 50),
        ("51-100", 50, 100),
        ("101+", 100, pairs.len()),
    ] {
        if start >= pairs.len() {
            continue;
        }
        let end = end.min(pairs.len());
        if start >= end {
            continue;
        }
        let slice = &pairs[start..end];
        let bb = slice.iter().map(|(_, value, _)| *value).sum::<f64>() / slice.len() as f64;
        let espn = slice.iter().map(|(_, _, value)| *value).sum::<f64>() / slice.len() as f64;
        println!(
            "{:<12} ${:>10.2} ${:>10.2} {:+9.2}",
            label,
            bb,
            espn,
            bb - espn
        );
    }

    println!("\nTOP 25 ESPN PLAYERS: independent BirdBoard vs ESPN average");
    println!(
        "{:<3} {:<25} {:>8} {:>8} {:>8}",
        "#", "player", "BB", "ESPN", "diff"
    );
    for (index, (player_id, birdboard, espn)) in pairs.iter().take(25).enumerate() {
        println!(
            "{:>2}. {:<25} ${:>6.1} ${:>6.1} {:+7.1}",
            index + 1,
            player_display_name(app, player_id),
            birdboard,
            espn,
            birdboard - espn,
        );
    }
}

fn pearson_price_correlation(pairs: &[(PlayerId, f64, f64)]) -> f64 {
    if pairs.len() < 2 {
        return 0.0;
    }
    let n = pairs.len() as f64;
    let mean_x = pairs.iter().map(|(_, x, _)| *x).sum::<f64>() / n;
    let mean_y = pairs.iter().map(|(_, _, y)| *y).sum::<f64>() / n;
    let mut covariance = 0.0;
    let mut variance_x = 0.0;
    let mut variance_y = 0.0;
    for (_, x, y) in pairs {
        let dx = *x - mean_x;
        let dy = *y - mean_y;
        covariance += dx * dy;
        variance_x += dx * dx;
        variance_y += dy * dy;
    }
    let denominator = (variance_x * variance_y).sqrt();
    if denominator <= f64::EPSILON {
        0.0
    } else {
        covariance / denominator
    }
}

fn print_competitive_birdboard_roster(
    app: &App,
    state: &SimState,
    mode: CompetitiveBirdBoardBidMode,
) {
    println!("    {} representative BirdBoard roster", mode.label());
    for (index, (player_id, price)) in state.rosters[0]
        .iter()
        .zip(state.prices[0].iter())
        .enumerate()
    {
        println!(
            "      {:>2}. {:<25} ${:>3}",
            index + 1,
            player_display_name(app, player_id),
            price
        );
    }
    println!(
        "      spent ${} | left ${}\n",
        STARTING_BUDGET.saturating_sub(state.budgets[0]),
        state.budgets[0]
    );
}

fn validate_competitive_room(state: &SimState) -> Result<()> {
    for (team, roster) in state.rosters.iter().enumerate() {
        if roster.len() != ROSTER_SIZE {
            bail!(
                "competitive auction ended with team {team} at {}/{} players",
                roster.len(),
                ROSTER_SIZE
            );
        }
    }
    Ok(())
}

fn choose_five_pool_calendar_weeks(
    app: &App,
    candidate_pool: &[PlayerId],
    seed: u64,
) -> Result<Vec<u32>> {
    let candidate_ids = candidate_pool.iter().cloned().collect::<HashSet<_>>();
    let by_week = weekly_rows_by_week_for_sim(&app.stats.weekly);
    let mut weeks = by_week
        .iter()
        .filter_map(|(&week, rows)| {
            let covered = candidate_ids
                .iter()
                .filter(|player_id| rows.contains_key(*player_id))
                .count();
            let ratio = covered as f64 / candidate_ids.len().max(1) as f64;
            (ratio >= 0.55).then_some(week)
        })
        .collect::<Vec<_>>();
    weeks.sort_unstable();

    if weeks.len() < 5 {
        weeks = by_week.keys().copied().collect::<Vec<_>>();
        weeks.sort_unstable();
    }
    if weeks.len() < 5 {
        bail!(
            "source-season cache contains only {} scoreable weeks",
            weeks.len()
        );
    }

    let mut rng = SimRng::new(seed ^ 0x51EE_CAFE);
    for index in (1..weeks.len()).rev() {
        let swap_with = (rng.next_u64() as usize) % (index + 1);
        weeks.swap(index, swap_with);
    }
    weeks.truncate(5);
    weeks.sort_unstable();
    Ok(weeks)
}

fn print_espn_room_calibration(
    app: &App,
    candidate_pool: &[PlayerId],
    espn_prices: &HashMap<PlayerId, f64>,
    global_weights: &[[f64; 9]],
) -> Result<()> {
    let initial_state = SimState::new(candidate_pool);
    let initial_inflation = espn_room_inflation(candidate_pool, &initial_state, espn_prices);
    if (initial_inflation - 1.0).abs() > 1e-9 {
        bail!(
            "ESPN room calibration invariant failed: opening inflation is {:.6}, expected exactly 1.0",
            initial_inflation
        );
    }

    // Verify the BirdBoard ESPN-prior path too.  This catches a subtler version
    // of the same bug: feeding absolute ESPN dollars through the independent
    // equilibrium rescaler would preserve ranking but silently inflate every
    // future-price assumption inside TEAM / H-indifference rollouts.
    let candidate_espn_prices = candidate_pool
        .iter()
        .filter_map(|player_id| {
            espn_prices
                .get(player_id)
                .copied()
                .map(|price| (player_id.clone(), price))
        })
        .collect::<HashMap<_, _>>();
    let mut espn_prior_model = app.durant.clone();
    espn_prior_model
        .set_absolute_market_prices(Some(candidate_espn_prices), AuctionConfig::default());
    let prior_board = market_for_model_team(&espn_prior_model, candidate_pool, &initial_state, 0);

    let mut by_espn = candidate_pool
        .iter()
        .filter_map(|player_id| {
            let espn = espn_prices.get(player_id).copied()?;
            let prior = prior_board.value_for(player_id)?.market_price_exact;
            Some((player_id.clone(), espn, prior))
        })
        .collect::<Vec<_>>();
    by_espn.sort_by(|left, right| right.1.total_cmp(&left.1));

    let top25 = by_espn.iter().take(25).cloned().collect::<Vec<_>>();
    let prior_mae = if top25.is_empty() {
        0.0
    } else {
        top25
            .iter()
            .map(|(_, espn, prior)| (prior - espn).abs())
            .sum::<f64>()
            / top25.len() as f64
    };
    if prior_mae > 0.05 {
        bail!(
            "ESPN absolute-prior calibration failed: top-25 initial TEAM prior MAE is ${:.3}",
            prior_mae
        );
    }

    // Calibrate the field's opening clearing process independently of
    // BirdBoard.  For each of the top 100 ESPN-priced players, sample many
    // deterministic 13-bot opening auctions and inspect second-price clearing.
    // This is deliberately a cold-room diagnostic: it tells us whether a $74
    // ESPN player actually enters the simulated market near $74 before any
    // real room spending can create genuine inflation/deflation.
    let calibration_players = by_espn.iter().take(100).cloned().collect::<Vec<_>>();
    let top25_ids = by_espn
        .iter()
        .take(25)
        .map(|(player_id, _, _)| player_id.clone())
        .collect::<HashSet<_>>();
    let mut sale_abs_errors = Vec::<f64>::new();
    let mut sale_ratios = Vec::<f64>::new();
    let mut top25_ratios = Vec::<f64>::new();
    let samples = 32usize;

    for sample in 0..samples {
        let seed = 0xE5F0_C411u64.wrapping_add((sample as u64).wrapping_mul(65_537));
        let styles = (0..TEAM_COUNT)
            .map(|team| {
                let index =
                    ((seed as usize).wrapping_add(team.wrapping_mul(97))) % global_weights.len();
                global_weights[index]
            })
            .collect::<Vec<_>>();

        for (player_id, espn, _) in &calibration_players {
            let mut bids = (0..TEAM_COUNT)
                .map(|team| {
                    competitive_espn_bot_max_bid(
                        app,
                        &initial_state,
                        team,
                        player_id,
                        espn_prices,
                        initial_inflation,
                        styles[team],
                        seed,
                        1.0,
                    )
                })
                .collect::<Vec<_>>();
            bids.sort_unstable_by(|left, right| right.cmp(left));
            let winner_max = bids.first().copied().unwrap_or(MIN_BID);
            let second_max = bids.get(1).copied().unwrap_or(MIN_BID);
            let sale = winner_max.min(second_max.saturating_add(1).max(MIN_BID)) as f64;
            sale_abs_errors.push((sale - *espn).abs());
            if *espn > f64::EPSILON {
                let ratio = sale / *espn;
                sale_ratios.push(ratio);
                if top25_ids.contains(player_id) {
                    top25_ratios.push(ratio);
                }
            }
        }
    }

    sale_ratios.sort_by(|left, right| left.total_cmp(right));
    top25_ratios.sort_by(|left, right| left.total_cmp(right));
    let median_ratio = median_sorted(&sale_ratios).unwrap_or(1.0);
    let top25_median_ratio = median_sorted(&top25_ratios).unwrap_or(1.0);
    let sale_mae = if sale_abs_errors.is_empty() {
        0.0
    } else {
        sale_abs_errors.iter().sum::<f64>() / sale_abs_errors.len() as f64
    };

    let opening_pressures = (0..TEAM_COUNT)
        .map(|team| {
            let index = team.wrapping_mul(97) % global_weights.len();
            espn_bot_budget_pressure(
                app,
                &initial_state,
                team,
                espn_prices,
                initial_inflation,
                global_weights[index],
                0xE5F0_C411u64,
            )
        })
        .collect::<Vec<_>>();
    let max_opening_pressure = opening_pressures.iter().copied().fold(1.0_f64, f64::max);
    if max_opening_pressure > 1.001 {
        bail!(
            "ESPN budget-pressure calibration failed: opening pressure reached {:.4}; expected 1.0",
            max_opening_pressure
        );
    }

    println!("\nESPN FIELD CALIBRATION (before competitive backtest)");
    println!("opening live-inflation factor: {:.3}", initial_inflation);
    println!(
        "opening finite-roster budget pressure: max {:.3}",
        max_opening_pressure
    );
    println!(
        "BirdBoard TEAM ESPN-prior top-25 MAE at opening state: ${:.3}",
        prior_mae
    );
    println!(
        "all-ESPN cold-room clearing (top 100, {} deterministic samples): MAE ${:.2} | median sale/ESPN {:.3} | top-25 median {:.3}",
        samples, sale_mae, median_ratio, top25_median_ratio,
    );
    let endgame_samples = 8usize;
    let mut field_left_sum = 0.0;
    let mut field_left_max = 0u16;
    let mut endgame_left = Vec::<u16>::new();
    let mut room_spend_sum = 0.0;
    for sample in 0..endgame_samples {
        let seed = 0xE5F0_E0D0u64.wrapping_add((sample as u64).wrapping_mul(1_000_003));
        let state =
            simulate_all_espn_field_room(app, candidate_pool, espn_prices, global_weights, seed)?;
        validate_competitive_room(&state)?;
        let left = state.budgets.iter().copied().sum::<u16>();
        field_left_sum += left as f64 / TEAM_COUNT as f64;
        field_left_max = field_left_max.max(state.budgets.iter().copied().max().unwrap_or(0));
        endgame_left.extend(state.budgets.iter().copied());
        room_spend_sum += (TEAM_COUNT as u16 * STARTING_BUDGET - left) as f64;
    }
    endgame_left.sort_unstable();
    let endgame_median = if endgame_left.is_empty() {
        0.0
    } else if endgame_left.len() % 2 == 0 {
        let middle = endgame_left.len() / 2;
        (endgame_left[middle - 1] as f64 + endgame_left[middle] as f64) / 2.0
    } else {
        endgame_left[endgame_left.len() / 2] as f64
    };
    let p90_index = if endgame_left.is_empty() {
        0
    } else {
        ((endgame_left.len() - 1) as f64 * 0.90).round() as usize
    };
    let endgame_p90 = endgame_left.get(p90_index).copied().unwrap_or(0);

    println!(
        "all-ESPN full-room endgame ({} deterministic rooms): avg $left/team {:.1} | median ${:.1} | p90 ${} | max ${} | avg room spend ${:.0}",
        endgame_samples,
        field_left_sum / endgame_samples as f64,
        endgame_median,
        endgame_p90,
        field_left_max,
        room_spend_sum / endgame_samples as f64,
    );
    println!(
        "Calibration purpose: published ESPN dollars stay on their own dollar scale; only subsequent REAL room over/underspending may move them."
    );

    Ok(())
}

fn median_sorted(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let middle = values.len() / 2;
    if values.len() % 2 == 0 {
        Some((values[middle - 1] + values[middle]) / 2.0)
    } else {
        Some(values[middle])
    }
}

fn simulate_all_espn_field_room(
    app: &App,
    candidate_pool: &[PlayerId],
    espn_prices: &HashMap<PlayerId, f64>,
    global_weights: &[[f64; 9]],
    seed: u64,
) -> Result<SimState> {
    let mut state = SimState::new(candidate_pool);
    let styles = (0..TEAM_COUNT)
        .map(|team| {
            let index =
                ((seed as usize).wrapping_add(team.wrapping_mul(97))) % global_weights.len();
            global_weights[index]
        })
        .collect::<Vec<_>>();

    for sale_index in 0..(TEAM_COUNT * ROSTER_SIZE) {
        if state.available.is_empty() {
            break;
        }
        let inflation = espn_room_inflation(candidate_pool, &state, espn_prices);
        let pressures = (0..TEAM_COUNT)
            .map(|team| {
                espn_bot_budget_pressure(
                    app,
                    &state,
                    team,
                    espn_prices,
                    inflation,
                    styles[team],
                    seed,
                )
            })
            .collect::<Vec<_>>();

        let start_nominator = sale_index % TEAM_COUNT;
        let nominator = (0..TEAM_COUNT)
            .map(|offset| (start_nominator + offset) % TEAM_COUNT)
            .find(|team| state.open_slots(*team) > 0)
            .context("all-ESPN calibration room has no active nominator")?;
        let nominee = choose_competitive_espn_bot_nominee(
            app,
            candidate_pool,
            &state,
            nominator,
            espn_prices,
            inflation,
            styles[nominator],
            seed,
            pressures[nominator],
        )
        .with_context(|| format!("ESPN calibration team {nominator} could not nominate"))?;

        let opening_bid = MIN_BID.min(state.max_legal_bid(nominator));
        let mut bids = Vec::<(usize, u16, u64)>::new();
        for team in 0..TEAM_COUNT {
            if state.open_slots(team) == 0 {
                continue;
            }
            let mut bid = competitive_espn_bot_max_bid(
                app,
                &state,
                team,
                &nominee,
                espn_prices,
                inflation,
                styles[team],
                seed,
                pressures[team],
            );
            if team == nominator {
                bid = bid.max(opening_bid);
            }
            if bid > 0 {
                bids.push((team, bid, stable_player_team_hash(seed, team, &nominee)));
            }
        }
        bids.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.2.cmp(&left.2)));
        let (winner, winner_max, _) = *bids
            .first()
            .context("all-ESPN calibration nomination produced no bids")?;
        let second_max = bids.get(1).map(|entry| entry.1).unwrap_or(0);
        let sale_price = winner_max
            .min(second_max.saturating_add(1).max(MIN_BID))
            .min(state.max_legal_bid(winner));
        state.buy(winner, nominee, sale_price)?;
    }

    Ok(state)
}

fn simulate_competitive_espn_room(
    app: &App,
    candidate_pool: &[PlayerId],
    espn_prices: &HashMap<PlayerId, f64>,
    global_weights: &[[f64; 9]],
    compact_weights: &[[f64; 9]],
    mode: CompetitiveBirdBoardBidMode,
    seed: u64,
) -> Result<SimState> {
    let mut state = SimState::new(candidate_pool);

    // MARKET is a price prior, never a bid ceiling. Give each matched
    // BirdBoard variant its own expected future-price environment, then let
    // exactly the same H-indifference reservation-bid algorithm decide how
    // much the team is actually willing to pay.
    //
    // v80: ESPN is an ABSOLUTE dollar prior.  Do not feed ESPN dollars through
    // BirdBoard's equilibrium-demand rescaler: that mechanism intentionally
    // consumes the full room budget and was turning a ~$74 ESPN Wemby into a
    // ~$110+ future-price assumption simply because the historical validation
    // pool is truncated.
    let mut birdboard_model = app.durant.clone();
    if mode == CompetitiveBirdBoardBidMode::EspnClearingMarket {
        let candidate_espn_prices = candidate_pool
            .iter()
            .filter_map(|player_id| {
                espn_prices
                    .get(player_id)
                    .copied()
                    .map(|price| (player_id.clone(), price))
            })
            .collect::<HashMap<_, _>>();
        birdboard_model
            .set_absolute_market_prices(Some(candidate_espn_prices), AuctionConfig::default());
    }
    let opponent_styles = (0..TEAM_COUNT)
        .map(|team| {
            let index =
                ((seed as usize).wrapping_add(team.wrapping_mul(97))) % global_weights.len();
            global_weights[index]
        })
        .collect::<Vec<_>>();

    let mut bb_targets = Vec::<PlayerId>::new();
    let mut bb_bid_weights = Vec::<[f64; 9]>::new();
    let mut bb_stale = true;
    let mut sales_since_bb_refresh = usize::MAX;

    for sale_index in 0..(TEAM_COUNT * ROSTER_SIZE) {
        if state.available.is_empty() {
            break;
        }

        if state.open_slots(0) > 0
            && (bb_stale
                || sales_since_bb_refresh >= 4
                || !bb_targets
                    .iter()
                    .any(|player_id| state.available.contains(player_id)))
        {
            let (targets, bid_weights) = refresh_birdboard_competitive_plan(
                &birdboard_model,
                candidate_pool,
                &state,
                global_weights,
                compact_weights,
            );
            bb_targets = targets;
            bb_bid_weights = bid_weights;
            bb_stale = false;
            sales_since_bb_refresh = 0;
        }

        let inflation = espn_room_inflation(candidate_pool, &state, espn_prices);
        // ESPN averages are opening-market dollar anchors, but a real finite
        // auction cannot let a manager carry useful discretionary dollars past
        // roster spot 13.  Compute each bot's own shadow-price pressure once
        // per sale from its remaining budget, open slots, and the ESPN-valued
        // alternatives still on the board.  This is 1.0 at the opening state
        // and only rises when that particular manager risks stranding cash.
        let espn_budget_pressures = (0..TEAM_COUNT)
            .map(|team| {
                espn_bot_budget_pressure(
                    app,
                    &state,
                    team,
                    espn_prices,
                    inflation,
                    opponent_styles[team],
                    seed,
                )
            })
            .collect::<Vec<_>>();
        let start_nominator = sale_index % TEAM_COUNT;
        let nominator = (0..TEAM_COUNT)
            .map(|offset| (start_nominator + offset) % TEAM_COUNT)
            .find(|team| state.open_slots(*team) > 0)
            .context("competitive auction has no active nominator")?;

        // A nomination is itself a $1 opening bid by the nominator.  Before
        // v79 the simulator forgot this for BirdBoard: if every opponent was
        // already full and H-indifference preferred PASS on the nominated
        // player, the bid list became empty and the room aborted.  Synthetic
        // market generation already used the correct opening-bid rule.
        //
        // When BirdBoard nominates, first try a few of its ordered TEAM targets
        // and prefer one it would voluntarily bid at least $1 on.  This avoids
        // needlessly forcing an unwanted $1 player late in the auction while
        // keeping nomination choice cheap relative to a full strategy solve.
        let mut precomputed_birdboard_bid = None::<(PlayerId, u16)>;
        let nominee = if nominator == 0 {
            let mut chosen = None::<PlayerId>;
            for target in bb_targets
                .iter()
                .filter(|player_id| state.available.contains(*player_id))
                .take(4)
            {
                let bid = birdboard_competitive_max_bid(
                    &birdboard_model,
                    candidate_pool,
                    &state,
                    target,
                    &bb_bid_weights,
                );
                if bid >= MIN_BID {
                    chosen = Some((*target).clone());
                    precomputed_birdboard_bid = Some(((*target).clone(), bid));
                    break;
                }
            }

            chosen
                .or_else(|| {
                    bb_targets
                        .iter()
                        .find(|player_id| state.available.contains(*player_id))
                        .cloned()
                })
                .or_else(|| {
                    choose_competitive_espn_bot_nominee(
                        app,
                        candidate_pool,
                        &state,
                        nominator,
                        espn_prices,
                        inflation,
                        opponent_styles[nominator],
                        seed,
                        espn_budget_pressures[nominator],
                    )
                })
        } else {
            choose_competitive_espn_bot_nominee(
                app,
                candidate_pool,
                &state,
                nominator,
                espn_prices,
                inflation,
                opponent_styles[nominator],
                seed,
                espn_budget_pressures[nominator],
            )
        }
        .with_context(|| format!("team {nominator} could not nominate a player"))?;

        let opening_bid = MIN_BID.min(state.max_legal_bid(nominator));
        if opening_bid == 0 {
            bail!(
                "auction invariant violated: active nominator {nominator} has no legal opening bid"
            );
        }

        let mut bids = Vec::<(usize, u16, u64)>::new();
        for team in 0..TEAM_COUNT {
            if state.open_slots(team) == 0 {
                continue;
            }
            let mut bid = if team == 0 {
                precomputed_birdboard_bid
                    .as_ref()
                    .filter(|(player_id, _)| player_id == &nominee)
                    .map(|(_, bid)| *bid)
                    .unwrap_or_else(|| {
                        birdboard_competitive_max_bid(
                            &birdboard_model,
                            candidate_pool,
                            &state,
                            &nominee,
                            &bb_bid_weights,
                        )
                    })
            } else {
                competitive_espn_bot_max_bid(
                    app,
                    &state,
                    team,
                    &nominee,
                    espn_prices,
                    inflation,
                    opponent_styles[team],
                    seed,
                    espn_budget_pressures[team],
                )
            };

            // Real salary-cap auctions do not permit a nomination with no
            // opening offer: the nominator owns the $1 bid unless somebody
            // raises.  This also guarantees progress when it is the last team
            // with open roster slots.
            if team == nominator {
                bid = bid.max(opening_bid);
            }

            if bid > 0 {
                bids.push((team, bid, stable_player_team_hash(seed, team, &nominee)));
            }
        }

        if bids.is_empty() {
            bail!(
                "auction invariant violated: nomination {:?} by team {nominator} produced no bids",
                nominee
            );
        }

        bids.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.2.cmp(&left.2)));
        let (winner, winner_max, _) = bids[0];
        let second_max = bids.get(1).map(|entry| entry.1).unwrap_or(0);
        let sale_price = winner_max
            .min(second_max.saturating_add(1).max(MIN_BID))
            .min(state.max_legal_bid(winner));

        let sold_was_top_target = bb_targets
            .iter()
            .take(4)
            .any(|player_id| player_id == &nominee);
        state.buy(winner, nominee.clone(), sale_price)?;
        sales_since_bb_refresh = sales_since_bb_refresh.saturating_add(1);
        if winner == 0 || sold_was_top_target {
            bb_stale = true;
        }
    }

    Ok(state)
}

fn refresh_birdboard_competitive_plan(
    model: &DurantModel,
    candidate_pool: &[PlayerId],
    state: &SimState,
    global_weights: &[[f64; 9]],
    compact_weights: &[[f64; 9]],
) -> (Vec<PlayerId>, Vec<[f64; 9]>) {
    if state.open_slots(0) == 0 {
        return (Vec::new(), Vec::new());
    }

    let available = candidate_pool
        .iter()
        .filter(|player_id| state.available.contains(*player_id))
        .cloned()
        .collect::<Vec<_>>();
    let (opponent_rosters, opponent_budgets) = opponent_views(state, 0);
    let weights = runtime_weights_for_model(
        model,
        &state.rosters[0],
        state.budgets[0],
        &opponent_rosters,
        &opponent_budgets,
        &available,
        global_weights,
        compact_weights,
    );

    let plan = model.roster_plan_with_strategy_weights(
        &state.rosters[0],
        state.budgets[0],
        &opponent_rosters,
        &opponent_budgets,
        &available,
        &weights,
        AuctionConfig::default(),
    );

    let mut targets = plan
        .as_ref()
        .map(|plan| plan.projected_future_players.clone())
        .unwrap_or_default();
    targets.retain(|player_id| state.available.contains(player_id));

    // Max Bid must be allowed to react to an unexpected bargain, even when the
    // nominee is not in the current target list.  Keep a compact set of the
    // TEAM plan's best strategy directions for the per-nomination H-indifference
    // solve instead of using MARKET as an artificial bidding cap.
    let mut bid_weights = Vec::<[f64; 9]>::new();
    if let Some(plan) = plan.as_ref() {
        bid_weights.push(plan.j_weights);
        for alternative in plan.j_alternatives.iter().take(5) {
            if !bid_weights.contains(&alternative.j_weights) {
                bid_weights.push(alternative.j_weights);
            }
        }
    }
    if bid_weights.is_empty() {
        for weights in weights.iter().take(8) {
            if !bid_weights.contains(weights) {
                bid_weights.push(*weights);
            }
        }
    }

    if targets.is_empty() {
        let market = market_for_model_team(model, candidate_pool, state, 0);
        let legal = legal_candidates(candidate_pool, state, 0, &market);
        if !legal.is_empty() {
            let fallback = model
                .immediate_market_advantage_scores(
                    &state.rosters[0],
                    &opponent_rosters,
                    &legal,
                    &market,
                )
                .into_iter()
                .max_by(|left, right| {
                    left.marginal_immediate_matchup_win_probability
                        .total_cmp(&right.marginal_immediate_matchup_win_probability)
                        .then_with(|| {
                            left.immediate_matchup_win_probability
                                .total_cmp(&right.immediate_matchup_win_probability)
                        })
                })
                .map(|score| score.player_id);
            if let Some(fallback) = fallback {
                targets.push(fallback);
            }
        }
    }

    (targets, bid_weights)
}

fn market_for_model_team(
    model: &DurantModel,
    candidate_pool: &[PlayerId],
    state: &SimState,
    team: usize,
) -> MarketBoard {
    let candidates = candidate_pool
        .iter()
        .filter(|player_id| state.available.contains(*player_id))
        .cloned()
        .collect::<Vec<_>>();
    let (opponent_rosters, opponent_budgets) = opponent_views(state, team);
    model.market_board(
        &state.rosters[team],
        state.budgets[team],
        &opponent_rosters,
        &opponent_budgets,
        &candidates,
        AuctionConfig::default(),
    )
}

fn espn_room_inflation(
    candidate_pool: &[PlayerId],
    state: &SimState,
    espn_prices: &HashMap<PlayerId, f64>,
) -> f64 {
    let remaining_slots = (0..TEAM_COUNT)
        .map(|team| state.open_slots(team))
        .sum::<usize>();
    if remaining_slots == 0 {
        return 1.0;
    }

    // ESPN's published averages are already dollar-denominated market prices.
    // The previous simulator normalized the truncated historical player pool
    // to the full $2,600 league bankroll, which made the opening inflation
    // factor ~1.5x and produced absurd sales such as ~$126 Wemby from a ~$74
    // ESPN average.  Instead measure LIVE room pressure relative to this exact
    // candidate pool's STARTING cash/value ratio.  Therefore opening inflation
    // is mathematically 1.0 and only actual over/underspending moves it later.
    let baseline_slots = (TEAM_COUNT * ROSTER_SIZE).min(candidate_pool.len());
    let baseline_discretionary = (TEAM_COUNT as f64 * STARTING_BUDGET as f64
        - baseline_slots as f64 * MIN_BID as f64)
        .max(0.0);
    let baseline_value_mass = espn_excess_value_mass(
        candidate_pool
            .iter()
            .filter_map(|player_id| espn_prices.get(player_id).copied()),
        baseline_slots,
    );

    let remaining_cash = state
        .budgets
        .iter()
        .map(|budget| *budget as f64)
        .sum::<f64>();
    let reserve = remaining_slots as f64 * MIN_BID as f64;
    let discretionary_cash = (remaining_cash - reserve).max(0.0);
    let live_value_mass = espn_excess_value_mass(
        candidate_pool
            .iter()
            .filter(|player_id| state.available.contains(*player_id))
            .filter_map(|player_id| espn_prices.get(player_id).copied()),
        remaining_slots,
    );

    if baseline_value_mass <= f64::EPSILON
        || live_value_mass <= f64::EPSILON
        || baseline_discretionary <= f64::EPSILON
    {
        return 1.0;
    }

    let baseline_ratio = baseline_discretionary / baseline_value_mass;
    let live_ratio = discretionary_cash / live_value_mass;
    (live_ratio / baseline_ratio).clamp(0.65, 1.50)
}

fn espn_excess_value_mass<I>(prices: I, slots: usize) -> f64
where
    I: IntoIterator<Item = f64>,
{
    let mut excess = prices
        .into_iter()
        .map(|price| (price - MIN_BID as f64).max(0.0))
        .collect::<Vec<_>>();
    excess.sort_by(|left, right| right.total_cmp(left));
    excess.into_iter().take(slots).sum::<f64>()
}

fn competitive_espn_expected_price(
    espn_prices: &HashMap<PlayerId, f64>,
    player_id: &PlayerId,
    inflation: f64,
) -> f64 {
    let base = espn_prices
        .get(player_id)
        .copied()
        .unwrap_or(MIN_BID as f64);
    MIN_BID as f64 + (base - MIN_BID as f64).max(0.0) * inflation
}

fn competitive_espn_bot_base_value(
    app: &App,
    _state: &SimState,
    team: usize,
    player_id: &PlayerId,
    espn_prices: &HashMap<PlayerId, f64>,
    inflation: f64,
    strategy: [f64; 9],
    seed: u64,
) -> f64 {
    let expected = competitive_espn_expected_price(espn_prices, player_id, inflation);
    let temperament = 0.96 + 0.08 * stable_player_team_unit(seed ^ 0xB07B_1D5, team, player_id);
    let preference = 0.95 + 0.10 * stable_player_team_unit(seed ^ 0xFACE_1234, team, player_id);
    let fit = app
        .durant
        .x_score_for(player_id)
        .map(|x| {
            let weight_mass = strategy
                .iter()
                .map(|value| value.abs())
                .sum::<f64>()
                .max(1.0);
            let weighted = x
                .iter()
                .zip(strategy.iter())
                .map(|(value, weight)| value * weight)
                .sum::<f64>()
                / weight_mass;
            1.0 + 0.05 * weighted.clamp(-1.5, 1.5)
        })
        .unwrap_or(1.0);

    (expected * temperament * preference * fit).max(MIN_BID as f64)
}

/// Finite-roster shadow price for an ESPN-valued opponent.
///
/// ESPN averages are treated as absolute OPENING prices.  They should not be
/// globally stretched to consume a $2,600 room before the auction starts.  But
/// once a manager has missed targets, its remaining cash can become large
/// relative to the ESPN cost of the best `open_slots` players it can still buy.
/// Those dollars have zero value after slot 13, so willingness to pay must rise.
///
/// The pressure is deliberately endogenous:
///   * exactly 1.0 at the opening state;
///   * rises only when the manager has more cash than its best remaining slots
///     can absorb at ESPN-like prices;
///   * reacts to both team roster progress and the room's shrinking competitive
///     runway, so a lagging rich team cannot wait until every opponent is full.
///
/// This preserves the observed ESPN opening scale while reproducing the basic
/// auction fact that unused endgame money is worthless.
fn espn_bot_budget_pressure(
    app: &App,
    state: &SimState,
    team: usize,
    espn_prices: &HashMap<PlayerId, f64>,
    inflation: f64,
    strategy: [f64; 9],
    seed: u64,
) -> f64 {
    let open = state.open_slots(team);
    if open == 0 || state.available.is_empty() {
        return 1.0;
    }

    let mut alternatives = state
        .available
        .iter()
        .map(|player_id| {
            competitive_espn_bot_base_value(
                app,
                state,
                team,
                player_id,
                espn_prices,
                inflation,
                strategy,
                seed,
            )
        })
        .collect::<Vec<_>>();
    alternatives.sort_by(|left, right| right.total_cmp(left));

    // Approximate what this manager would spend if it filled every remaining
    // slot with its best still-available ESPN-valued alternatives.  If that
    // already exhausts the budget there is no reason to inflate bids.
    let expected_fill_cost = alternatives
        .into_iter()
        .take(open)
        .sum::<f64>()
        .max(open as f64 * MIN_BID as f64);
    let budget = state.budgets[team] as f64;
    if budget <= expected_fill_cost + 1e-9 {
        return 1.0;
    }

    let raw_pressure = (budget / expected_fill_cost).max(1.0);

    // v82: pressure must react to the amount of COMPETITIVE RUNWAY left in
    // the room, not only to this manager's own roster progress.  The v81
    // progress^2 blend was too patient: a manager that had missed several
    // targets could still have many open slots while most opponents were
    // already nearly full.  By the time its own progress finally triggered
    // strong pressure there was nobody left to bid against, so second-price
    // clearing could no longer convert its cash into player quality.
    //
    // Keep the opening state exact: team_progress == room_progress ==
    // competition_closing == 0, so ESPN's published dollars remain untouched.
    // Thereafter combine three independent reasons that unused cash is losing
    // option value:
    //   1. this manager is running out of roster slots;
    //   2. the room as a whole is running out of roster slots;
    //   3. fewer opponents remain able to create future bidding competition.
    //
    // Pressure is still applied ONLY when budget exceeds the ESPN-valued cost
    // of the manager's best remaining `open` alternatives.  Thus ordinary
    // early bidding stays on the ESPN scale; the stronger blend merely acts
    // sooner when cash is genuinely at risk of being stranded.
    let filled = ROSTER_SIZE.saturating_sub(open);
    let team_progress = if ROSTER_SIZE <= 1 {
        1.0
    } else {
        (filled as f64 / (ROSTER_SIZE - 1) as f64).clamp(0.0, 1.0)
    };

    let total_slots = TEAM_COUNT * ROSTER_SIZE;
    let total_open = (0..TEAM_COUNT)
        .map(|other| state.open_slots(other))
        .sum::<usize>();
    let room_progress = if total_slots == 0 {
        1.0
    } else {
        (1.0 - total_open as f64 / total_slots as f64).clamp(0.0, 1.0)
    };

    let active_teams = (0..TEAM_COUNT)
        .filter(|other| state.open_slots(*other) > 0)
        .count();
    let competition_closing = if TEAM_COUNT <= 1 {
        1.0
    } else {
        (1.0 - active_teams.saturating_sub(1) as f64 / (TEAM_COUNT - 1) as f64).clamp(0.0, 1.0)
    };

    // Union-like blend: each source can raise urgency, while 0/0/0 stays
    // exactly zero at the opening state.  Once only four active teams remain,
    // use the full shadow-price correction; waiting longer cannot create more
    // competition.
    let progress_urgency = 1.0 - (1.0 - team_progress) * (1.0 - room_progress);
    let mut urgency = progress_urgency.max(competition_closing.powi(2));
    if active_teams <= 4 {
        urgency = 1.0;
    }

    (1.0 + urgency * (raw_pressure - 1.0)).clamp(1.0, 50.0)
}

fn competitive_espn_bot_max_bid(
    app: &App,
    state: &SimState,
    team: usize,
    player_id: &PlayerId,
    espn_prices: &HashMap<PlayerId, f64>,
    inflation: f64,
    strategy: [f64; 9],
    seed: u64,
    budget_pressure: f64,
) -> u16 {
    let legal = state.max_legal_bid(team);
    if legal == 0 {
        return 0;
    }

    let base_value = competitive_espn_bot_base_value(
        app,
        state,
        team,
        player_id,
        espn_prices,
        inflation,
        strategy,
        seed,
    );

    (base_value * budget_pressure)
        .round()
        .clamp(MIN_BID as f64, legal as f64) as u16
}

fn choose_competitive_espn_bot_nominee(
    app: &App,
    candidate_pool: &[PlayerId],
    state: &SimState,
    team: usize,
    espn_prices: &HashMap<PlayerId, f64>,
    inflation: f64,
    strategy: [f64; 9],
    seed: u64,
    budget_pressure: f64,
) -> Option<PlayerId> {
    candidate_pool
        .iter()
        .filter(|player_id| state.available.contains(*player_id))
        .filter_map(|player_id| {
            let bid = competitive_espn_bot_max_bid(
                app,
                state,
                team,
                player_id,
                espn_prices,
                inflation,
                strategy,
                seed,
                budget_pressure,
            );
            (bid > 0).then_some((player_id.clone(), bid))
        })
        .max_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| left.0.0.as_str().cmp(right.0.0.as_str()))
        })
        .map(|entry| entry.0)
}

fn birdboard_competitive_max_bid(
    model: &DurantModel,
    candidate_pool: &[PlayerId],
    state: &SimState,
    player_id: &PlayerId,
    strategy_weights: &[[f64; 9]],
) -> u16 {
    let legal = state.max_legal_bid(0);
    if legal == 0 || strategy_weights.is_empty() {
        return 0;
    }

    let candidates = candidate_pool
        .iter()
        .filter(|candidate| state.available.contains(*candidate))
        .cloned()
        .collect::<Vec<_>>();
    let (opponent_rosters, opponent_budgets) = opponent_views(state, 0);

    model
        .current_state_reservation_bid_with_strategy_weights(
            &state.rosters[0],
            state.budgets[0],
            &opponent_rosters,
            &opponent_budgets,
            &candidates,
            player_id,
            strategy_weights,
            AuctionConfig::default(),
        )
        .min(legal)
}

fn stable_player_team_hash(seed: u64, team: usize, player_id: &PlayerId) -> u64 {
    let mut hash = seed ^ (team as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xCBF2_9CE4_8422_2325;
    for byte in player_id.0.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100_0000_01B3);
    }
    hash
}

fn stable_player_team_unit(seed: u64, team: usize, player_id: &PlayerId) -> f64 {
    let bits = stable_player_team_hash(seed, team, player_id) >> 11;
    bits as f64 / ((1u64 << 53) as f64)
}
