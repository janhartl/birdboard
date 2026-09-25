use crate::app::App;
use crate::durant::AuctionConfig;
use crate::player::PlayerId;
use crate::strategy_stage::{RuntimeStrategyStage, build_mid_adaptive_weights};
use anyhow::{Context, Result, bail};
use std::collections::HashSet;

const DEFAULT_TARGET: &str = "Nikola Jokić";

pub fn print_plan() {
    println!("Full-beam Max Bid sanity test");
    println!("  default target : {DEFAULT_TARGET}");
    println!("  default state  : empty $200 roster, 12 empty $200 opponents");
    println!("  future market  : fully opponent-priced ONCE, then frozen");
    println!("  roster search  : current stage-aware j bank + beam 16x24");
    println!();
    println!("Optional environment variables:");
    println!("  BIRDBOARD_PRICE_CURVE_PLAYER='Nikola Jokić'");
    println!("  BIRDBOARD_PRICE_CURVE_PRICES='20,30,40,48,50,58,60,70'");
    println!("  BIRDBOARD_PRICE_CURVE_OWN='Victor Wembanyama@66'");
    println!();
    println!("OWN accepts comma-separated Name@price entries. If @price is omitted,");
    println!("the Preparation market price is used. This is a controlled synthetic state;");
    println!("opponents remain empty so the first run reproduces the clean early-draft case.");
}

pub fn run(app: &App) -> Result<()> {
    let config = AuctionConfig::default();
    let target_name = std::env::var("BIRDBOARD_PRICE_CURVE_PLAYER")
        .unwrap_or_else(|_| DEFAULT_TARGET.to_string());
    let target_id = resolve_player_id(app, &target_name)?;
    let target_display = app
        .durant
        .score_for(&target_id)
        .map(|score| score.player_name.clone())
        .unwrap_or_else(|| target_name.clone());

    let (own_roster, own_spend) = parse_own_roster(app)?;
    if own_roster.iter().any(|id| id == &target_id) {
        bail!("target {target_display} is already in BIRDBOARD_PRICE_CURVE_OWN");
    }
    if own_spend >= config.starting_budget {
        bail!("synthetic own-roster spend ${own_spend} leaves no auction budget");
    }
    let own_budget = config.starting_budget - own_spend;

    let opponent_count = app.teams.len().saturating_sub(1).max(1);
    let opponent_rosters = vec![Vec::<PlayerId>::new(); opponent_count];
    let opponent_budgets = vec![config.starting_budget; opponent_count];

    let own_set = own_roster.iter().cloned().collect::<HashSet<_>>();
    let candidates = app
        .players
        .iter()
        .filter(|player| !own_set.contains(&player.id))
        .filter(|player| app.durant.score_for(&player.id).is_some())
        .map(|player| player.id.clone())
        .collect::<Vec<_>>();

    let bank = app
        .strategy_bank
        .as_ref()
        .context("runtime strategy bank is not loaded")?;
    if bank.weights.is_empty() {
        bail!("compact pricing bank is empty");
    }

    let pass_weights = stage_weights(
        app,
        &own_roster,
        own_budget,
        &opponent_rosters,
        &opponent_budgets,
        &candidates,
        RuntimeStrategyStage::for_roster_size(own_roster.len()),
    )?;

    let mut buy_roster = own_roster.clone();
    buy_roster.push(target_id.clone());
    let buy_stage = RuntimeStrategyStage::for_roster_size(buy_roster.len());
    let prep_market = app
        .market_value_for(&target_id)
        .map(|value| value.market_price)
        .unwrap_or(config.minimum_bid);
    let buy_seed_budget = own_budget.saturating_sub(prep_market.min(own_budget));
    let buy_candidates = candidates
        .iter()
        .filter(|id| *id != &target_id)
        .cloned()
        .collect::<Vec<_>>();
    let buy_weights = stage_weights(
        app,
        &buy_roster,
        buy_seed_budget,
        &opponent_rosters,
        &opponent_budgets,
        &buy_candidates,
        buy_stage,
    )?;

    let fast = app
        .durant
        .auction_candidate_analysis_with_strategy_weights(
            &own_roster,
            own_budget,
            &opponent_rosters,
            &opponent_budgets,
            &candidates,
            &target_id,
            prep_market,
            &pass_weights,
            config,
        )?;

    let prices = requested_prices(
        prep_market,
        fast.as_ref().map(|score| score.market_price),
        fast.as_ref().map(|score| score.fair_price),
    );

    println!("========================================================================");
    println!("FULL-BEAM FIXED-MARKET PRICE CURVE");
    println!("========================================================================");
    println!("target        : {target_display}");
    println!("own roster    : {} players", own_roster.len());
    if own_roster.is_empty() {
        println!("own players   : EMPTY");
    } else {
        let labels = own_roster
            .iter()
            .map(|id| {
                app.durant
                    .score_for(id)
                    .map(|s| s.player_name.clone())
                    .unwrap_or_else(|| id.0.clone())
            })
            .collect::<Vec<_>>();
        println!("own players   : {}", labels.join(" + "));
    }
    println!("own budget    : ${own_budget}");
    println!(
        "pass j stage  : {} ({} j)",
        RuntimeStrategyStage::for_roster_size(own_roster.len()).label(),
        pass_weights.len()
    );
    println!(
        "buy j stage   : {} ({} j)",
        buy_stage.label(),
        buy_weights.len()
    );
    println!("pricing core  : {} compact j", bank.weights.len());
    println!("prep market   : ${prep_market}");
    if let Some(fast) = &fast {
        println!("FAST comp     : ${}", fast.market_price);
        println!("FAST max bid  : ${}", fast.fair_price);
        println!(
            "FAST H PASS   : {:.2}%",
            fast.pass_projected_matchup_win_probability * 100.0
        );
    }
    println!("curve prices  : {:?}", prices);
    println!();

    let curve = app
        .durant
        .full_beam_price_curve_fixed_market(
            &own_roster,
            own_budget,
            &opponent_rosters,
            &opponent_budgets,
            &candidates,
            &target_id,
            &prices,
            &pass_weights,
            &buy_weights,
            &bank.weights,
            prep_market,
            config,
        )
        .context("full-beam curve found no valid plan")?;

    println!(
        "future market : frozen after {target_display} leaves board at ${}",
        curve.reference_sale_price
    );
    println!("market solve  : {:.2}s", curve.market_seconds);
    println!(
        "FULL H PASS   : {:6.2}% | {:4.2} cats | {} | {} | {:.2}s",
        curve.pass_matchup_win_probability * 100.0,
        curve.pass_expected_categories,
        curve.pass_build_name,
        curve.pass_j_name,
        curve.pass_seconds,
    );
    println!();
    println!(" price |  FULL H |  Δ vs PASS | cats | left | beam time | build");
    println!(
        "-------+---------+------------+------+------+-----------+---------------------------"
    );

    let mut violations = Vec::<(u16, f64, u16, f64)>::new();
    let mut previous: Option<(u16, f64)> = None;
    for point in &curve.points {
        let delta_pp = (point.matchup_win_probability - curve.pass_matchup_win_probability) * 100.0;
        println!(
            " ${:>3}  | {:7.2}% | {:+9.2} pp | {:4.2} | ${:>3} | {:8.2}s | {}",
            point.price,
            point.matchup_win_probability * 100.0,
            delta_pp,
            point.expected_categories,
            point.projected_budget_left,
            point.elapsed_seconds,
            point.build_name,
        );
        if let Some((previous_price, previous_h)) = previous {
            // Prices are sorted ascending. More expensive should not produce a
            // materially BETTER best H under a frozen future market.
            if point.matchup_win_probability > previous_h + 0.0005 {
                violations.push((
                    previous_price,
                    previous_h,
                    point.price,
                    point.matchup_win_probability,
                ));
            }
        }
        previous = Some((point.price, point.matchup_win_probability));
    }

    println!();
    if violations.is_empty() {
        println!("MONOTONICITY: PASS — no >0.05 pp increase when price rises.");
    } else {
        println!(
            "MONOTONICITY: FAIL — {} material violation(s):",
            violations.len()
        );
        for (p0, h0, p1, h1) in violations {
            println!(
                "  ${p0} {:6.2}% -> ${p1} {:6.2}%  ({:+.2} pp)",
                h0 * 100.0,
                h1 * 100.0,
                (h1 - h0) * 100.0
            );
        }
    }

    let affordable = curve
        .points
        .iter()
        .filter(|point| {
            point.matchup_win_probability + 0.0005 >= curve.pass_matchup_win_probability
        })
        .map(|point| point.price)
        .max();
    match affordable {
        Some(price) => println!("Highest TESTED price with H(BUY) >= H(PASS): ${price}"),
        None => println!("No TESTED BUY price reached H(PASS)."),
    }
    println!();
    println!("This is a diagnostic grid, not yet a production Max Bid solver.");

    Ok(())
}

pub fn run_frontier(app: &App) -> Result<()> {
    let config = AuctionConfig::default();
    let target_name = std::env::var("BIRDBOARD_PRICE_CURVE_PLAYER")
        .unwrap_or_else(|_| DEFAULT_TARGET.to_string());
    let target_id = resolve_player_id(app, &target_name)?;
    let target_display = app
        .durant
        .score_for(&target_id)
        .map(|score| score.player_name.clone())
        .unwrap_or_else(|| target_name.clone());

    let (own_roster, own_spend) = parse_own_roster(app)?;
    if own_roster.iter().any(|id| id == &target_id) {
        bail!("target {target_display} is already in BIRDBOARD_PRICE_CURVE_OWN");
    }
    if own_spend >= config.starting_budget {
        bail!("synthetic own-roster spend ${own_spend} leaves no auction budget");
    }
    let own_budget = config.starting_budget - own_spend;
    let opponent_count = app.teams.len().saturating_sub(1).max(1);
    let opponent_rosters = vec![Vec::<PlayerId>::new(); opponent_count];
    let opponent_budgets = vec![config.starting_budget; opponent_count];
    let own_set = own_roster.iter().cloned().collect::<HashSet<_>>();
    let candidates = app
        .players
        .iter()
        .filter(|player| !own_set.contains(&player.id))
        .filter(|player| app.durant.score_for(&player.id).is_some())
        .map(|player| player.id.clone())
        .collect::<Vec<_>>();

    let bank = app
        .strategy_bank
        .as_ref()
        .context("runtime strategy bank is not loaded")?;
    if bank.weights.is_empty() {
        bail!("compact pricing bank is empty");
    }
    let stage = RuntimeStrategyStage::for_roster_size(own_roster.len());
    let search_weights = stage_weights(
        app,
        &own_roster,
        own_budget,
        &opponent_rosters,
        &opponent_budgets,
        &candidates,
        stage,
    )?;
    let prep_market = app
        .market_value_for(&target_id)
        .map(|value| value.market_price)
        .unwrap_or(config.minimum_bid);
    let fast = app
        .durant
        .auction_candidate_analysis_with_strategy_weights(
            &own_roster,
            own_budget,
            &opponent_rosters,
            &opponent_budgets,
            &candidates,
            &target_id,
            prep_market,
            &search_weights,
            config,
        )?;
    let competition = fast
        .as_ref()
        .map(|score| score.market_price)
        .unwrap_or(prep_market);

    println!("========================================================================");
    println!("SHARED PARETO PRICING FRONTIER");
    println!("========================================================================");
    println!("target        : {target_display}");
    println!("own roster    : {} players", own_roster.len());
    println!("own budget    : ${own_budget}");
    println!(
        "j stage       : {} ({} j)",
        stage.label(),
        search_weights.len()
    );
    println!("pricing core  : {} compact j", bank.weights.len());
    println!("prep market   : ${prep_market}");
    println!("FAST comp     : ${competition}");
    if let Some(fast) = &fast {
        println!("FAST max bid  : ${}", fast.fair_price);
    }
    println!();

    let started = std::time::Instant::now();
    let frontier = app
        .durant
        .build_full_pricing_frontier_with_strategy_weights(
            &own_roster,
            own_budget,
            &opponent_rosters,
            &opponent_budgets,
            &candidates,
            &search_weights,
            &bank.weights,
            config,
            None,
        )
        .context("shared Pareto frontier found no valid completion states")?;
    let advice = app
        .durant
        .full_pricing_bid_from_frontier(&frontier, &target_id, prep_market, competition, config)
        .context("frontier could not price target")?;

    let pass = advice.pass_projected_matchup_win_probability;
    println!("frontier total : {:.2}s", started.elapsed().as_secs_f64());
    println!("  market       : {:.2}s", advice.frontier_market_seconds);
    println!("  beam         : {:.2}s", advice.frontier_beam_seconds);
    println!();
    println!("PARETO max bid : ${}", advice.fair_price);
    println!("Edge vs comp   : {:+}", advice.expected_edge);
    println!("H PASS         : {:.2}%", pass * 100.0);
    println!(
        "H @ prep ${:<3}: {:.2}%  {:+.2} pp",
        advice.evaluated_prep_market_price,
        advice.prep_market_projected_matchup_win_probability * 100.0,
        (advice.prep_market_projected_matchup_win_probability - pass) * 100.0,
    );
    println!(
        "H @ comp ${:<3}: {:.2}%  {:+.2} pp",
        advice.evaluated_competition_price,
        advice.competition_projected_matchup_win_probability * 100.0,
        (advice.competition_projected_matchup_win_probability - pass) * 100.0,
    );
    if advice.fair_price > 0 {
        println!(
            "H @ max  ${:<3}: {:.2}%  {:+.2} pp",
            advice.fair_price,
            advice.fair_price_projected_matchup_win_probability * 100.0,
            (advice.fair_price_projected_matchup_win_probability - pass) * 100.0,
        );
    }
    println!("build          : {}", advice.build_name);
    println!("j              : {}", advice.j_name);
    println!();
    println!("Compare PARETO max bid / H values to the v52 full-beam curve for the same state.");
    println!("The live UI uses this shared frontier after each draft-state change.");

    Ok(())
}

fn requested_prices(prep: u16, competition: Option<u16>, fast_max: Option<u16>) -> Vec<u16> {
    if let Ok(raw) = std::env::var("BIRDBOARD_PRICE_CURVE_PRICES") {
        let mut prices = raw
            .split(',')
            .filter_map(|part| part.trim().parse::<u16>().ok())
            .collect::<Vec<_>>();
        prices.sort_unstable();
        prices.dedup();
        if !prices.is_empty() {
            return prices;
        }
    }

    let mut prices = vec![20, 30, 40, 50, 60, prep];
    if let Some(price) = competition {
        prices.push(price);
    }
    if let Some(price) = fast_max {
        prices.push(price);
    }
    prices.sort_unstable();
    prices.dedup();
    prices
}

fn parse_own_roster(app: &App) -> Result<(Vec<PlayerId>, u16)> {
    let raw = std::env::var("BIRDBOARD_PRICE_CURVE_OWN").unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok((Vec::new(), 0));
    }

    let mut roster = Vec::<PlayerId>::new();
    let mut spend = 0u16;
    let mut seen = HashSet::<PlayerId>::new();
    for entry in raw
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (name, explicit_price) = match entry.rsplit_once('@') {
            Some((name, price)) => {
                let price = price
                    .trim()
                    .parse::<u16>()
                    .with_context(|| format!("invalid price in OWN entry '{entry}'"))?;
                (name.trim(), Some(price))
            }
            None => (entry, None),
        };
        let player_id = resolve_player_id(app, name)?;
        if !seen.insert(player_id.clone()) {
            continue;
        }
        let price = explicit_price
            .or_else(|| {
                app.market_value_for(&player_id)
                    .map(|value| value.market_price)
            })
            .unwrap_or(AuctionConfig::default().minimum_bid);
        spend = spend.saturating_add(price);
        roster.push(player_id);
    }
    Ok((roster, spend))
}

fn resolve_player_id(app: &App, query: &str) -> Result<PlayerId> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        bail!("player name is empty");
    }

    let exact = app
        .players
        .iter()
        .filter(|player| player.name.to_lowercase() == needle)
        .collect::<Vec<_>>();
    if exact.len() == 1 {
        return Ok(exact[0].id.clone());
    }

    let partial = app
        .players
        .iter()
        .filter(|player| player.name.to_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    match partial.as_slice() {
        [player] => Ok(player.id.clone()),
        [] => bail!("could not find player matching '{query}'"),
        many => {
            let names = many
                .iter()
                .take(8)
                .map(|player| player.name.as_str())
                .collect::<Vec<_>>();
            bail!("player query '{query}' is ambiguous: {}", names.join(", "))
        }
    }
}

fn stage_weights(
    app: &App,
    roster: &[PlayerId],
    budget: u16,
    opponent_rosters: &[Vec<PlayerId>],
    opponent_budgets: &[u16],
    candidates: &[PlayerId],
    stage: RuntimeStrategyStage,
) -> Result<Vec<[f64; 9]>> {
    let bank = app
        .strategy_bank
        .as_ref()
        .context("runtime strategy bank is not loaded")?;
    let weights = match stage {
        RuntimeStrategyStage::EarlyGlobal => bank.early_weights_or_fallback().to_vec(),
        RuntimeStrategyStage::LateCompact => bank.weights.clone(),
        RuntimeStrategyStage::MidAdaptive => {
            let global = bank.early_weights_or_fallback();
            let seeds = app.durant.coarse_strategy_seed_weights(
                roster,
                budget,
                opponent_rosters,
                opponent_budgets,
                candidates,
                global,
                AuctionConfig::default(),
                24,
            );
            build_mid_adaptive_weights(global, &seeds)
        }
    };
    if weights.is_empty() {
        bail!("{} strategy vocabulary is empty", stage.label());
    }
    Ok(weights)
}

const DEFAULT_REPLAY_ANCHOR: &str = "Shai Gilgeous-Alexander";
const DEFAULT_REPLAY_SALES: &str =
    "Nikola Jokić@77>Luka Legends;Victor Wembanyama@70>FCB;Luka Dončić@51>Maribor";

#[derive(Debug, Clone)]
struct ReplaySale {
    player_id: PlayerId,
    player_name: String,
    price: u16,
    opponent_index: usize,
    team_name: String,
}

pub fn print_replay_plan() {
    println!("Pricing replay validator");
    println!("  anchor        : {DEFAULT_REPLAY_ANCHOR}");
    println!("  own team      : unchanged at $200 / 0 players");
    println!(
        "  default sales : Jokić $77 -> Luka Legends; Wemby $70 -> FCB; Dončić $51 -> Maribor"
    );
    println!("  checkpoints   : before sales, then after each sale");
    println!("  compare       : shared v53 Pareto vs candidate-specific v52 full beam");
    println!("  full window   : shared max bid ±4 dollars (3 expensive reference points)");
    println!();
    println!("Optional environment variables:");
    println!("  BIRDBOARD_REPLAY_ANCHOR='Shai Gilgeous-Alexander'");
    println!("  BIRDBOARD_REPLAY_RADIUS='4'");
    println!(
        "  BIRDBOARD_REPLAY_SALES='Nikola Jokić@77>Luka Legends;Victor Wembanyama@70>FCB;Luka Dončić@51>Maribor'"
    );
    println!();
    println!("The expensive reference only tests three prices around the shared bid.");
    println!("If all three are on the same side of H(PASS), the validator reports that");
    println!("the true full-beam threshold lies outside the tested window.");
}

pub fn run_replay_validation(app: &App) -> Result<()> {
    let config = AuctionConfig::default();
    let anchor_name = std::env::var("BIRDBOARD_REPLAY_ANCHOR")
        .unwrap_or_else(|_| DEFAULT_REPLAY_ANCHOR.to_string());
    let anchor_id = resolve_player_id(app, &anchor_name)?;
    let anchor_display = app
        .durant
        .score_for(&anchor_id)
        .map(|score| score.player_name.clone())
        .unwrap_or(anchor_name);
    let radius = std::env::var("BIRDBOARD_REPLAY_RADIUS")
        .ok()
        .and_then(|raw| raw.parse::<u16>().ok())
        .unwrap_or(4)
        .max(1);
    let sales = parse_replay_sales(app)?;
    if sales.iter().any(|sale| sale.player_id == anchor_id) {
        bail!("replay anchor {anchor_display} is sold in BIRDBOARD_REPLAY_SALES");
    }

    let bank = app
        .strategy_bank
        .as_ref()
        .context("runtime strategy bank is not loaded")?;
    if bank.weights.is_empty() {
        bail!("compact pricing bank is empty");
    }

    let opponent_teams = app
        .teams
        .iter()
        .filter(|team| team.id != app.user_team_id)
        .collect::<Vec<_>>();
    if opponent_teams.is_empty() {
        bail!("replay validator needs at least one opponent team");
    }

    let own_roster = Vec::<PlayerId>::new();
    let own_budget = config.starting_budget;
    let mut opponent_rosters = vec![Vec::<PlayerId>::new(); opponent_teams.len()];
    let mut opponent_budgets = vec![config.starting_budget; opponent_teams.len()];
    let mut drafted = HashSet::<PlayerId>::new();

    println!("========================================================================");
    println!("PRICING REPLAY VALIDATION · SHARED PARETO vs FULL REFERENCE");
    println!("========================================================================");
    println!("anchor       : {anchor_display}");
    println!("our state    : EMPTY roster · ${own_budget} throughout");
    println!("radius       : ±${radius} around shared Pareto bid");
    println!("pricing core : {} compact j", bank.weights.len());
    println!("sales        :");
    for (index, sale) in sales.iter().enumerate() {
        println!(
            "  {}. {} @ ${} -> {}",
            index + 1,
            sale.player_name,
            sale.price,
            sale.team_name
        );
    }
    println!();
    println!("This can take several minutes: each checkpoint runs one shared frontier");
    println!("plus a candidate-specific FULL pass and three FULL buy beam solves.");

    for checkpoint in 0..=sales.len() {
        if checkpoint > 0 {
            let sale = &sales[checkpoint - 1];
            opponent_rosters[sale.opponent_index].push(sale.player_id.clone());
            opponent_budgets[sale.opponent_index] =
                opponent_budgets[sale.opponent_index].saturating_sub(sale.price);
            drafted.insert(sale.player_id.clone());
        }

        let candidates = app
            .players
            .iter()
            .filter(|player| !drafted.contains(&player.id))
            .filter(|player| app.durant.score_for(&player.id).is_some())
            .map(|player| player.id.clone())
            .collect::<Vec<_>>();
        if !candidates.iter().any(|id| id == &anchor_id) {
            bail!("anchor {anchor_display} is unavailable at checkpoint {checkpoint}");
        }

        let stage = RuntimeStrategyStage::for_roster_size(own_roster.len());
        let pass_weights = stage_weights(
            app,
            &own_roster,
            own_budget,
            &opponent_rosters,
            &opponent_budgets,
            &candidates,
            stage,
        )?;
        let prep_market = app
            .market_value_for(&anchor_id)
            .map(|value| value.market_price)
            .unwrap_or(config.minimum_bid);
        let fast = app
            .durant
            .auction_candidate_analysis_with_strategy_weights(
                &own_roster,
                own_budget,
                &opponent_rosters,
                &opponent_budgets,
                &candidates,
                &anchor_id,
                prep_market,
                &pass_weights,
                config,
            )?;
        let competition = fast
            .as_ref()
            .map(|score| score.market_price)
            .unwrap_or(prep_market);

        println!();
        println!("------------------------------------------------------------------------");
        if checkpoint == 0 {
            println!("CHECKPOINT 0 · BEFORE ANY SALES");
        } else {
            let last = &sales[checkpoint - 1];
            println!(
                "CHECKPOINT {checkpoint} · AFTER {} @ ${} -> {}",
                last.player_name, last.price, last.team_name
            );
        }
        println!("------------------------------------------------------------------------");
        println!("available     : {} scored players", candidates.len());
        println!("FAST comp     : ${competition}");
        if let Some(fast) = &fast {
            println!("FAST max bid  : ${}", fast.fair_price);
        }

        let shared_started = std::time::Instant::now();
        let frontier = app
            .durant
            .build_full_pricing_frontier_with_strategy_weights(
                &own_roster,
                own_budget,
                &opponent_rosters,
                &opponent_budgets,
                &candidates,
                &pass_weights,
                &bank.weights,
                config,
                None,
            )
            .context("shared Pareto frontier found no valid completion states")?;
        let shared = app
            .durant
            .full_pricing_bid_from_frontier(&frontier, &anchor_id, prep_market, competition, config)
            .context("shared frontier could not price replay anchor")?;
        let coverage = app
            .durant
            .full_pricing_frontier_coverage(&frontier, &anchor_id);
        let pass_survival = percentage(coverage.pass_without_candidate, coverage.pass_total);
        let buy_survival = percentage(coverage.buy_without_candidate, coverage.buy_total);

        println!("SHARED PARETO");
        println!("  max bid     : ${}", shared.fair_price);
        println!("  edge vs comp: {:+}", shared.expected_edge);
        println!(
            "  H PASS      : {:.2}%",
            shared.pass_projected_matchup_win_probability * 100.0
        );
        println!(
            "  runtime     : {:.2}s  (market {:.2}s + beam {:.2}s)",
            shared_started.elapsed().as_secs_f64(),
            shared.frontier_market_seconds,
            shared.frontier_beam_seconds
        );
        println!("  candidate-exclusion coverage:");
        println!(
            "    PASS {:>6}/{:<6} survive ({:5.1}%) · {:>6} contained {}",
            coverage.pass_without_candidate,
            coverage.pass_total,
            pass_survival,
            coverage.pass_with_candidate,
            anchor_display
        );
        println!(
            "    BUY  {:>6}/{:<6} survive ({:5.1}%) · {:>6} contained {}",
            coverage.buy_without_candidate,
            coverage.buy_total,
            buy_survival,
            coverage.buy_with_candidate,
            anchor_display
        );

        let max_legal =
            replay_max_legal_bid(own_budget, app.durant.team_size(), config.minimum_bid);
        let center = if shared.fair_price >= config.minimum_bid {
            shared.fair_price
        } else {
            competition.max(config.minimum_bid).min(max_legal)
        };
        let low = center.saturating_sub(radius).max(config.minimum_bid);
        let high = center.saturating_add(radius).min(max_legal);
        let mut reference_prices = vec![low, center.min(max_legal), high];
        reference_prices.sort_unstable();
        reference_prices.dedup();

        let mut buy_roster = own_roster.clone();
        buy_roster.push(anchor_id.clone());
        let buy_candidates = candidates
            .iter()
            .filter(|id| *id != &anchor_id)
            .cloned()
            .collect::<Vec<_>>();
        let buy_seed_budget = own_budget.saturating_sub(prep_market.min(own_budget));
        let buy_weights = stage_weights(
            app,
            &buy_roster,
            buy_seed_budget,
            &opponent_rosters,
            &opponent_budgets,
            &buy_candidates,
            RuntimeStrategyStage::for_roster_size(buy_roster.len()),
        )?;

        println!("FULL REFERENCE · candidate-specific market · v52 semantics");
        println!("  tested prices: {:?}", reference_prices);
        let full_started = std::time::Instant::now();
        let full = app
            .durant
            .full_beam_price_curve_fixed_market(
                &own_roster,
                own_budget,
                &opponent_rosters,
                &opponent_budgets,
                &candidates,
                &anchor_id,
                &reference_prices,
                &pass_weights,
                &buy_weights,
                &bank.weights,
                prep_market,
                config,
            )
            .context("FULL replay reference found no valid plan")?;
        println!(
            "  H PASS      : {:.2}%  (shared {:+.2} pp)",
            full.pass_matchup_win_probability * 100.0,
            (shared.pass_projected_matchup_win_probability - full.pass_matchup_win_probability)
                * 100.0
        );
        for point in &full.points {
            println!(
                "  H @ ${:<3}    : {:6.2}%  {:+6.2} pp vs FULL PASS  · {:5.1}s",
                point.price,
                point.matchup_win_probability * 100.0,
                (point.matchup_win_probability - full.pass_matchup_win_probability) * 100.0,
                point.elapsed_seconds
            );
        }
        println!(
            "  FULL runtime : {:.2}s",
            full_started.elapsed().as_secs_f64()
        );
        print_reference_window_result(shared.fair_price, &full);
    }

    println!();
    println!("========================================================================");
    println!("REPLAY COMPLETE");
    println!("========================================================================");
    println!("Interpretation:");
    println!("  • small shared-vs-FULL bid error + healthy coverage => market-state move is real");
    println!(
        "  • large error with low survival => shared beam is losing candidate-exclusion diversity"
    );
    println!("  • large error with high survival => shared-market approximation itself is suspect");

    Ok(())
}

fn print_reference_window_result(shared_max: u16, full: &crate::durant::FullBeamPriceCurve) {
    if full.points.is_empty() {
        println!("  verdict      : no FULL price points");
        return;
    }
    let pass = full.pass_matchup_win_probability;
    let eps = 0.0005;
    let mut last_good = None::<u16>;
    let mut first_bad = None::<u16>;
    for point in &full.points {
        if point.matchup_win_probability + eps >= pass {
            last_good = Some(point.price);
        } else if first_bad.is_none() {
            first_bad = Some(point.price);
        }
    }

    match (last_good, first_bad) {
        (Some(good), Some(bad)) if good < bad => {
            let good_h = full
                .points
                .iter()
                .find(|point| point.price == good)
                .map(|point| point.matchup_win_probability)
                .unwrap_or(pass);
            let bad_h = full
                .points
                .iter()
                .find(|point| point.price == bad)
                .map(|point| point.matchup_win_probability)
                .unwrap_or(pass);
            let estimated = interpolate_threshold(good, good_h, bad, bad_h, pass);
            println!("  FULL max     : bracket ${good}..${bad}, interpolated ≈ ${estimated:.1}");
            println!(
                "  shared error : {:+.1} dollars vs interpolated FULL",
                shared_max as f64 - estimated
            );
        }
        (Some(good), None) => {
            println!("  FULL max     : > ${good} (above tested window)");
            println!("  shared error : shared ${shared_max} is at least too LOW for this window");
        }
        (None, Some(bad)) => {
            println!("  FULL max     : < ${bad} (below tested window)");
            println!("  shared error : shared ${shared_max} is at least too HIGH for this window");
        }
        _ => println!("  FULL max     : threshold could not be bracketed"),
    }
}

fn interpolate_threshold(
    good_price: u16,
    good_h: f64,
    bad_price: u16,
    bad_h: f64,
    pass_h: f64,
) -> f64 {
    if (good_h - bad_h).abs() < 1.0e-12 {
        return good_price as f64;
    }
    let fraction = ((good_h - pass_h) / (good_h - bad_h)).clamp(0.0, 1.0);
    good_price as f64 + fraction * (bad_price.saturating_sub(good_price) as f64)
}

fn percentage(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

fn replay_max_legal_bid(own_budget: u16, team_size: usize, minimum_bid: u16) -> u16 {
    if team_size == 0 {
        return 0;
    }
    let reserve = (team_size.saturating_sub(1) as u32).saturating_mul(minimum_bid as u32);
    (own_budget as u32)
        .saturating_sub(reserve)
        .min(u16::MAX as u32) as u16
}

fn parse_replay_sales(app: &App) -> Result<Vec<ReplaySale>> {
    let raw = std::env::var("BIRDBOARD_REPLAY_SALES")
        .unwrap_or_else(|_| DEFAULT_REPLAY_SALES.to_string());
    let opponent_teams = app
        .teams
        .iter()
        .filter(|team| team.id != app.user_team_id)
        .collect::<Vec<_>>();
    let mut sales = Vec::<ReplaySale>::new();
    let mut seen_players = HashSet::<PlayerId>::new();

    for entry in raw
        .split(';')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        let (player_price, team_query) = entry
            .rsplit_once('>')
            .with_context(|| format!("replay sale '{entry}' must use Player@price>Team"))?;
        let (player_query, price_raw) = player_price
            .rsplit_once('@')
            .with_context(|| format!("replay sale '{entry}' must include @price"))?;
        let price = price_raw
            .trim()
            .parse::<u16>()
            .with_context(|| format!("invalid replay price in '{entry}'"))?;
        let player_id = resolve_player_id(app, player_query.trim())?;
        if !seen_players.insert(player_id.clone()) {
            bail!(
                "player '{}' appears twice in replay sales",
                player_query.trim()
            );
        }
        let player_name = app
            .durant
            .score_for(&player_id)
            .map(|score| score.player_name.clone())
            .unwrap_or_else(|| player_query.trim().to_string());

        let team_needle = team_query.trim().to_lowercase();
        let matching = opponent_teams
            .iter()
            .enumerate()
            .filter(|(_, team)| team.name.to_lowercase() == team_needle)
            .collect::<Vec<_>>();
        let (opponent_index, team) = match matching.as_slice() {
            [(index, team)] => (*index, *team),
            [] => bail!("could not find opponent team '{}'", team_query.trim()),
            _ => bail!("opponent team query '{}' is ambiguous", team_query.trim()),
        };
        sales.push(ReplaySale {
            player_id,
            player_name,
            price,
            opponent_index,
            team_name: team.name.clone(),
        });
    }

    Ok(sales)
}
