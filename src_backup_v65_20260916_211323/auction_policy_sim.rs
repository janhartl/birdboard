use crate::app::App;
use crate::durant::{AuctionConfig, MarketBoard};
use crate::player::PlayerId;
use crate::stats;
use crate::strategy_stage::{RuntimeStrategyStage, build_mid_adaptive_weights};
use anyhow::{Context, Result, bail};
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
    budgets: Vec<u16>,
    available: HashSet<PlayerId>,
}

impl SimState {
    fn new(candidate_pool: &[PlayerId]) -> Self {
        Self {
            rosters: vec![Vec::new(); TEAM_COUNT],
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
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
struct LeagueResult {
    user_rank: usize,
    user_avg_h: f64,
    user_avg_categories: f64,
    user_budget_left: u16,
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
            let result = simulate_one_auction(
                &app,
                &candidate_pool,
                global_weights,
                bank.weights.as_slice(),
                policy,
                seed,
            )?;
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
        "{:<18} {:>8} {:>8} {:>9} {:>10} {:>10} {:>9}",
        "policy", "#1 rate", "top-3", "avg rank", "avg H", "avg cats", "$ left"
    );
    for policy in UserPolicy::ALL {
        let summary = aggregates
            .get(policy.label())
            .context("missing policy aggregate")?;
        let n = summary.runs as f64;
        println!(
            "{:<18} {:>7.1}% {:>7.1}% {:>9.2} {:>9.2}% {:>10.3} {:>9.1}",
            policy.label(),
            summary.wins as f64 * 100.0 / n,
            summary.top3 as f64 * 100.0 / n,
            summary.rank_sum / n,
            summary.h_sum * 100.0 / n,
            summary.cats_sum / n,
            summary.budget_sum / n,
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
) -> Result<LeagueResult> {
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

    score_final_league(app, &state)
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

    app.durant.market_board(
        &state.rosters[team],
        state.budgets[team],
        &opponent_rosters,
        &opponent_budgets,
        &candidates,
        AuctionConfig::default(),
    )
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

    Ok(LeagueResult {
        user_rank,
        user_avg_h: user_h,
        user_avg_categories: avg_categories[0],
        user_budget_left: state.budgets[0],
    })
}
