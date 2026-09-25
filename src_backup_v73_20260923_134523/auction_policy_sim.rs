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

/// Build BirdBoard's independent preseason auction market.
///
/// v72 used a fast "budget-share" approximation: each manager split all of
/// its discretionary money across its current 13 favorite players.  That was
/// useful as a smoke test but it is NOT a max-bid model and it compressed the
/// elite tier badly (Jokic/Wemby could barely clear the mid-$30s).
///
/// v73 instead reuses BirdBoard's H-indifference Max Bid logic.  For every
/// candidate and every representative synthetic strategy profile we solve:
///
///     max p such that H(BUY candidate @ p) >= H(PASS candidate)
///
/// A synthetic 13-team room is then just a Vickrey clearing event for that
/// player: second-highest rational max bid + $1.  We cheaply resample hundreds
/// of heterogeneous 13-manager rooms from the expensive bid matrix.
///
/// To avoid baking the legacy G/$ curve into the answer, the calculation is
/// iterated. Epoch 0 uses the legacy market ONLY as a neutral completion-price
/// initialization.  Each later epoch uses the previous rational-bid curve as
/// its future-price prior.  ESPN is never read by this builder.
#[test]
#[ignore = "expensive one-time rational max-bid equilibrium market census"]
fn build_independent_equilibrium_market() -> Result<()> {
    let rooms = std::env::var("BIRDBOARD_MARKET_ROOMS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(600)
        .max(50);
    let requested_profiles = std::env::var("BIRDBOARD_MARKET_PROFILES")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(16)
        .max(TEAM_COUNT);
    let epochs = std::env::var("BIRDBOARD_MARKET_EPOCHS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(3)
        .clamp(1, 8);

    let stats = stats::load_or_fetch()?;
    let app = App::new(stats)?;
    let bank = app
        .strategy_bank
        .as_ref()
        .context("equilibrium market build requires a runtime strategy bank")?;
    let strategy_weights = bank.early_weights_or_fallback();
    if strategy_weights.is_empty() {
        bail!("runtime strategy bank contains no strategy weights");
    }

    // v72 defaulted to the top 200. That caused the ESPN weekly validation to
    // have only 161 triple-overlap players. The one-time market should cover
    // every player BirdBoard can score unless the user explicitly asks for a
    // smaller debugging pool.
    let pool_limit = std::env::var("BIRDBOARD_MARKET_PLAYERS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(app.durant.scores.len())
        .max(TEAM_COUNT * ROSTER_SIZE)
        .min(app.durant.scores.len());
    let candidate_pool = app
        .durant
        .scores
        .iter()
        .take(pool_limit)
        .map(|score| score.player_id.clone())
        .collect::<Vec<_>>();
    if candidate_pool.len() < TEAM_COUNT * ROSTER_SIZE {
        bail!(
            "equilibrium market pool has only {} players; need at least {}",
            candidate_pool.len(),
            TEAM_COUNT * ROSTER_SIZE
        );
    }

    let profiles = representative_strategy_profiles(strategy_weights, requested_profiles);
    if profiles.len() < TEAM_COUNT {
        bail!(
            "equilibrium market has only {} representative strategy profiles; need at least {}",
            profiles.len(),
            TEAM_COUNT
        );
    }
    let room_profiles = sampled_room_profile_sets(profiles.len(), rooms);
    let config = AuctionConfig::default();

    println!("\nBirdBoard independent rational-bid equilibrium market v73");
    println!(
        "{} players | {} representative manager profiles | {} synthetic 13-manager rooms | {} fixed-point epochs",
        candidate_pool.len(),
        profiles.len(),
        rooms,
        epochs
    );
    println!(
        "pricing primitive: exact H-indifference reservation bid; each room clears at second-highest max bid + $1"
    );
    println!(
        "epoch 0 prior: legacy statistical market initialization only; epochs 1+ use the previous rational-bid curve"
    );
    println!("ESPN inputs: NONE\n");

    let mut prior_prices: Option<HashMap<PlayerId, f64>> = None;
    let mut final_rows = Vec::<EquilibriumMarketRow>::new();

    for epoch in 0..epochs {
        let epoch_started = std::time::Instant::now();
        println!(
            "EPOCH {}/{}: solving {} x {} rational max bids...",
            epoch + 1,
            epochs,
            candidate_pool.len(),
            profiles.len()
        );

        let bid_matrix = candidate_pool
            .par_iter()
            .map(|player_id| {
                let bids = profiles
                    .iter()
                    .map(|weights| {
                        app.durant.preseason_reservation_bid_with_strategy_weights(
                            player_id,
                            &candidate_pool,
                            std::slice::from_ref(weights),
                            prior_prices.as_ref(),
                            config,
                        )
                    })
                    .collect::<Vec<_>>();
                (player_id.clone(), bids)
            })
            .collect::<Vec<_>>();

        let mut rows = Vec::<EquilibriumMarketRow>::with_capacity(candidate_pool.len());
        for (player_id, bids) in &bid_matrix {
            let mut clearings = room_profiles
                .iter()
                .map(|profile_set| second_price_from_profile_bids(bids, profile_set))
                .collect::<Vec<_>>();
            clearings.sort_unstable();

            let mean_price = clearings.iter().map(|price| *price as f64).sum::<f64>()
                / clearings.len().max(1) as f64;
            let median_price = quantile_sorted_u16(&clearings, 0.50);
            let p25_price = quantile_sorted_u16(&clearings, 0.25);
            let p75_price = quantile_sorted_u16(&clearings, 0.75);
            // Every player gets a hypothetical opening-room clearing quote.
            // The saved value is a RELATIVE demand weight; live BirdBoard
            // rescales the draftable slice to the actual remaining room money.
            let equilibrium_price = mean_price.max(MIN_BID as f64);

            rows.push(EquilibriumMarketRow {
                player_id: player_id.0.clone(),
                player_name: player_display_name(&app, player_id),
                equilibrium_price,
                mean_price,
                median_price,
                p25_price,
                p75_price,
                sale_rate: 1.0,
                observations: clearings.len(),
                rooms,
            });
        }
        rows.sort_by(|left, right| {
            right
                .equilibrium_price
                .total_cmp(&left.equilibrium_price)
                .then_with(|| right.median_price.total_cmp(&left.median_price))
                .then_with(|| left.player_name.cmp(&right.player_name))
        });

        let new_prior = rows
            .iter()
            .map(|row| (PlayerId(row.player_id.clone()), row.equilibrium_price))
            .collect::<HashMap<_, _>>();

        let change = prior_prices.as_ref().map(|old| {
            let mut absolute = 0.0;
            let mut count = 0usize;
            for (player_id, new_price) in &new_prior {
                if let Some(old_price) = old.get(player_id) {
                    absolute += (new_price - old_price).abs();
                    count += 1;
                }
            }
            if count == 0 {
                0.0
            } else {
                absolute / count as f64
            }
        });

        println!(
            "epoch {} finished in {:.1}s{}",
            epoch + 1,
            epoch_started.elapsed().as_secs_f64(),
            change
                .map(|value| format!(" | mean absolute prior change ${value:.2}"))
                .unwrap_or_default()
        );
        println!("  top rational clearing weights:");
        for (index, row) in rows.iter().take(10).enumerate() {
            println!(
                "  {:>2}. {:<24} mean ${:>5.1}  median ${:>5.1}  IQR ${:>3.0}-${:<3.0}",
                index + 1,
                row.player_name,
                row.mean_price,
                row.median_price,
                row.p25_price,
                row.p75_price,
            );
        }
        println!();

        prior_prices = Some(new_prior);
        final_rows = rows;
    }

    let path = equilibrium_market::save_for_season(&app.stats.draft_season, &final_rows)?;
    println!("Saved independent market to {}", path.display());
    println!("\nTOP 30 FINAL RATIONAL CLEARING WEIGHTS");
    println!(
        "{:<3} {:<25} {:>7} {:>7} {:>11}",
        "#", "player", "mean", "median", "IQR"
    );
    for (index, row) in final_rows.iter().take(30).enumerate() {
        println!(
            "{:>2}. {:<25} ${:>5.1} ${:>5.1}  ${:>3.0}-${:<3.0}",
            index + 1,
            row.player_name,
            row.mean_price,
            row.median_price,
            row.p25_price,
            row.p75_price,
        );
    }
    println!(
        "\nThese are demand weights from rational reservation bids, not ESPN-fitted prices. Normal BirdBoard rescales them to the real room's remaining discretionary dollars."
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

    for sale_index in 0..(TEAM_COUNT * ROSTER_SIZE) {
        let start = sale_index % TEAM_COUNT;
        let nominator = (0..TEAM_COUNT)
            .map(|offset| (start + offset) % TEAM_COUNT)
            .find(|team| state.open_slots(*team) > 0)
            .context("equilibrium auction has no active nominator")?;

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

        let mut bids = Vec::<(usize, u16, u64)>::new();
        for team in 0..TEAM_COUNT {
            if state.open_slots(team) == 0 {
                continue;
            }
            let schedule = equilibrium_bid_schedule(
                utility_cache[team]
                    .as_ref()
                    .context("missing equilibrium utility cache")?,
                &state.available,
                state.budgets[team],
                state.open_slots(team),
            );
            let bid = schedule
                .iter()
                .find(|(player_id, _)| player_id == &nominee)
                .map(|(_, bid)| *bid)
                .unwrap_or(MIN_BID)
                .min(state.max_legal_bid(team));
            bids.push((
                team,
                bid.max(MIN_BID),
                stable_player_team_hash(seed, team, &nominee),
            ));
        }
        bids.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.2.cmp(&left.2)));

        let (winner, winner_max, _) = bids[0];
        let second_max = bids.get(1).map(|entry| entry.1).unwrap_or(0);
        let sale_price = winner_max
            .min(second_max.saturating_add(1).max(MIN_BID))
            .min(state.max_legal_bid(winner));
        state.buy(winner, nominee.clone(), sale_price)?;
        sales.push((nominee, sale_price));
        utility_cache[winner] = None;
    }

    for team in 0..TEAM_COUNT {
        if state.rosters[team].len() != ROSTER_SIZE {
            bail!(
                "equilibrium room ended with team {team} at {}/{} players",
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
    match RuntimeStrategyStage::for_roster_size(own_roster.len()) {
        RuntimeStrategyStage::EarlyGlobal => global_weights.to_vec(),
        RuntimeStrategyStage::LateCompact => compact_weights.to_vec(),
        RuntimeStrategyStage::MidAdaptive => {
            let seeds = app.durant.coarse_strategy_seed_weights(
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
    /// Isolate roster-selection quality: BirdBoard follows TEAM strategy but
    /// is willing to pay the same room-clearing ESPN market that opponents use.
    EspnClearingMarket,
    /// End-to-end stress test: BirdBoard still follows TEAM strategy, but its
    /// ceiling comes from BirdBoard's independently simulated equilibrium market.
    IndependentEquilibriumMarket,
}

impl CompetitiveBirdBoardBidMode {
    const ALL: [Self; 2] = [Self::EspnClearingMarket, Self::IndependentEquilibriumMarket];

    fn label(self) -> &'static str {
        match self {
            Self::EspnClearingMarket => "TEAM @ ESPN market",
            Self::IndependentEquilibriumMarket => "TEAM @ equilibrium",
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
            "no independent equilibrium market found; first run `BIRDBOARD_MARKET_ROOMS=600 cargo test --release build_independent_equilibrium_market -- --ignored --nocapture` (expected {})",
            equilibrium_market::path_for_season(&app.stats.draft_season).display()
        )
    })?;
    let bank = app
        .strategy_bank
        .as_ref()
        .context("competitive ESPN simulation requires a runtime strategy bank")?;

    println!("\nIndependent BirdBoard equilibrium market vs ESPN");
    println!(
        "Loaded {} independent prices from {}.",
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

    let rows_by_week = weekly_rows_by_week_for_sim(&app.stats.weekly);
    println!(
        "\nUsing {} players with independent prices + ESPN prices + weekly history.",
        candidate_pool.len()
    );
    println!(
        "Auction model: rotating nominations, all teams submit legal max bids, winner pays second-highest max + $1."
    );
    println!(
        "ESPN opponents use current ESPN averages + live room inflation + mild stable roster preference."
    );
    println!(
        "BirdBoard TEAM selection uses the independently generated equilibrium market. Two matched variants differ ONLY in BirdBoard's bid ceiling: ESPN clearing estimate vs independent equilibrium estimate."
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
        "TEAM @ ESPN market asks whether BirdBoard's PLAYER/ROSTER selection is useful when it must pay the ESPN-derived room price."
    );
    println!(
        "TEAM @ equilibrium is the end-to-end test of BirdBoard's independent market + TEAM strategy against the ESPN-valued field."
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
    let opponent_styles = (0..TEAM_COUNT)
        .map(|team| {
            let index =
                ((seed as usize).wrapping_add(team.wrapping_mul(97))) % global_weights.len();
            global_weights[index]
        })
        .collect::<Vec<_>>();

    let mut bb_targets = Vec::<PlayerId>::new();
    let mut bb_native_prices = HashMap::<PlayerId, u16>::new();
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
            let (targets, native_prices) = refresh_birdboard_competitive_plan(
                app,
                candidate_pool,
                &state,
                global_weights,
                compact_weights,
            );
            bb_targets = targets;
            bb_native_prices = native_prices;
            bb_stale = false;
            sales_since_bb_refresh = 0;
        }

        let inflation = espn_room_inflation(candidate_pool, &state, espn_prices);
        let start_nominator = sale_index % TEAM_COUNT;
        let nominator = (0..TEAM_COUNT)
            .map(|offset| (start_nominator + offset) % TEAM_COUNT)
            .find(|team| state.open_slots(*team) > 0)
            .context("competitive auction has no active nominator")?;

        let nominee = if nominator == 0 {
            bb_targets
                .iter()
                .find(|player_id| state.available.contains(*player_id))
                .cloned()
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
            )
        }
        .with_context(|| format!("team {nominator} could not nominate a player"))?;

        let mut bids = Vec::<(usize, u16, u64)>::new();
        for team in 0..TEAM_COUNT {
            if state.open_slots(team) == 0 {
                continue;
            }
            let bid = if team == 0 {
                birdboard_competitive_max_bid(
                    app,
                    candidate_pool,
                    &state,
                    &nominee,
                    espn_prices,
                    inflation,
                    &bb_targets,
                    &bb_native_prices,
                    mode,
                )
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
                )
            };
            if bid > 0 {
                bids.push((team, bid, stable_player_team_hash(seed, team, &nominee)));
            }
        }

        if bids.is_empty() {
            bail!("no team produced a legal bid for {:?}", nominee);
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
    app: &App,
    candidate_pool: &[PlayerId],
    state: &SimState,
    global_weights: &[[f64; 9]],
    compact_weights: &[[f64; 9]],
) -> (Vec<PlayerId>, HashMap<PlayerId, u16>) {
    if state.open_slots(0) == 0 {
        return (Vec::new(), HashMap::new());
    }

    let available = candidate_pool
        .iter()
        .filter(|player_id| state.available.contains(*player_id))
        .cloned()
        .collect::<Vec<_>>();
    let (opponent_rosters, opponent_budgets) = opponent_views(state, 0);
    let weights = runtime_weights_for_state(
        app,
        &state.rosters[0],
        state.budgets[0],
        &opponent_rosters,
        &opponent_budgets,
        &available,
        global_weights,
        compact_weights,
    );

    let mut targets = app
        .durant
        .roster_plan_with_strategy_weights(
            &state.rosters[0],
            state.budgets[0],
            &opponent_rosters,
            &opponent_budgets,
            &available,
            &weights,
            AuctionConfig::default(),
        )
        .map(|plan| plan.projected_future_players)
        .unwrap_or_default();

    targets.retain(|player_id| state.available.contains(player_id));
    if targets.is_empty() {
        let market = market_for_team(app, candidate_pool, state, 0);
        if let Some(fallback) = choose_user_player(
            app,
            candidate_pool,
            state,
            &market,
            UserPolicy::NowDeltaH,
            global_weights,
            compact_weights,
        ) {
            targets.push(fallback);
        }
    }

    let native_market = market_for_team(app, candidate_pool, state, 0);
    let native_prices = targets
        .iter()
        .filter_map(|player_id| {
            market_price_for(&native_market, player_id).map(|price| (player_id.clone(), price))
        })
        .collect::<HashMap<_, _>>();

    (targets, native_prices)
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

    let remaining_cash = state
        .budgets
        .iter()
        .map(|budget| *budget as f64)
        .sum::<f64>();
    let reserve = remaining_slots as f64 * MIN_BID as f64;
    let discretionary_cash = (remaining_cash - reserve).max(0.0);

    let mut excess_values = candidate_pool
        .iter()
        .filter(|player_id| state.available.contains(*player_id))
        .filter_map(|player_id| espn_prices.get(player_id).copied())
        .map(|price| (price - MIN_BID as f64).max(0.0))
        .collect::<Vec<_>>();
    excess_values.sort_by(|left, right| right.total_cmp(left));
    let value_mass = excess_values.into_iter().take(remaining_slots).sum::<f64>();

    if value_mass <= f64::EPSILON {
        1.0
    } else {
        (discretionary_cash / value_mass).clamp(0.65, 2.50)
    }
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

fn competitive_espn_bot_max_bid(
    app: &App,
    state: &SimState,
    team: usize,
    player_id: &PlayerId,
    espn_prices: &HashMap<PlayerId, f64>,
    inflation: f64,
    strategy: [f64; 9],
    seed: u64,
) -> u16 {
    let legal = state.max_legal_bid(team);
    if legal == 0 {
        return 0;
    }

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

    (expected * temperament * preference * fit)
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
    _app: &App,
    _candidate_pool: &[PlayerId],
    state: &SimState,
    player_id: &PlayerId,
    espn_prices: &HashMap<PlayerId, f64>,
    inflation: f64,
    targets: &[PlayerId],
    native_prices: &HashMap<PlayerId, u16>,
    mode: CompetitiveBirdBoardBidMode,
) -> u16 {
    let legal = state.max_legal_bid(0);
    if legal == 0 {
        return 0;
    }
    let Some(target_rank) = targets.iter().position(|target| target == player_id) else {
        return 0;
    };

    let priority_multiplier = match target_rank {
        0 => 1.08,
        1 => 1.05,
        2 | 3 => 1.03,
        _ => 1.00,
    };

    let ceiling = match mode {
        CompetitiveBirdBoardBidMode::EspnClearingMarket => {
            competitive_espn_expected_price(espn_prices, player_id, inflation) * priority_multiplier
        }
        CompetitiveBirdBoardBidMode::IndependentEquilibriumMarket => {
            let equilibrium = native_prices.get(player_id).copied().unwrap_or(MIN_BID) as f64;
            equilibrium * priority_multiplier
        }
    };

    ceiling.round().clamp(MIN_BID as f64, legal as f64) as u16
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
