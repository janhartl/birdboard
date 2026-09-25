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
