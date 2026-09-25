use crate::{app::App, durant};

pub fn print_manual_override_players(app: &App) {
    let names = [
        "Jayson Tatum",
        "Tyrese Haliburton",
        "Kyrie Irving",
        "Damian Lillard",
        "Fred VanVleet",
        "Jimmy Butler",
        "Ja Morant",
        "Trae Young",
    ];

    println!();
    println!("==========================================================================");
    println!("DURANT — MANUAL OVERRIDE CHECK");
    println!("==========================================================================");
    println!(
        "{:>4} {:<24} {:>8} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "RANK", "PLAYER", "TOTAL", "FG%", "FT%", "3PM", "PTS", "REB", "AST", "STL", "BLK", "TO",
    );

    for name in names {
        match app
            .durant
            .scores
            .iter()
            .enumerate()
            .find(|(_, score)| score.player_name == name)
        {
            Some((index, score)) => {
                println!(
                    "{:>4} {:<24} {:>8.3} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2} {:>7.2}",
                    index + 1,
                    score.player_name,
                    score.total,
                    score.field_goal,
                    score.free_throw,
                    score.threes,
                    score.points,
                    score.rebounds,
                    score.assists,
                    score.steals,
                    score.blocks,
                    score.turnovers,
                );
            }
            None => {
                println!("{:>4} {:<24} {}", "-", name, "NOT FOUND");
            }
        }
    }
}

pub fn print_roster_auction_scenarios(app: &App) -> anyhow::Result<()> {
    use std::collections::HashSet;

    const OWN_BUDGET_REMAINING: u16 = 120;

    let scenarios: &[(&str, &[&str])] = &[
        (
            "GIANNIS + GOBERT",
            &["Giannis Antetokounmpo", "Rudy Gobert"],
        ),
        ("LUKA + CLINGAN", &["Luka Dončić", "Donocan Clingan"]),
        ("JOKIĆ + MATAS", &["Nikola Jokić", "Matas Buzelis"]),
    ];

    let mut analyses = Vec::new();

    for (title, roster_names) in scenarios {
        let own_roster = roster_names
            .iter()
            .map(|name| {
                app.players
                    .iter()
                    .find(|player| player.name == *name)
                    .map(|player| player.id.clone())
                    .ok_or_else(|| anyhow::anyhow!("could not find player: {name}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        let owned = own_roster.iter().cloned().collect::<HashSet<_>>();

        let candidates = app
            .players
            .iter()
            .filter(|player| !owned.contains(&player.id))
            .map(|player| player.id.clone())
            .collect::<Vec<_>>();

        let analysis = app.durant.auction_analysis(
            &own_roster,
            OWN_BUDGET_REMAINING,
            &[],
            &[],
            &candidates,
            durant::AuctionConfig::default(),
        )?;

        println!();
        println!("========================================================================");
        println!("AUCTION DURANT — {title}");
        println!("Roster: {}", roster_names.join(", "));
        println!("Budget remaining: ${OWN_BUDGET_REMAINING}");
        println!("========================================================================");
        println!(
            "slots={} | league $={} | reserved $={} | discretionary $={} | repl G={:.3} | G/$={:.5}",
            analysis.economy.remaining_roster_slots,
            analysis.economy.remaining_league_dollars,
            analysis.economy.reserved_minimum_dollars,
            analysis.economy.discretionary_dollars,
            analysis.economy.replacement_g_score,
            analysis.economy.g_above_replacement_per_dollar,
        );

        println!();
        println!("TOP 30 VALUES");
        println!(
            "{:>3} {:<24} {:>7} {:>7} {:>7} {:>8} {:>8}",
            "#", "Player", "MARKET", "FAIR", "EDGE", "PASS", "BUY@MKT"
        );

        for (index, score) in analysis.scores.iter().take(30).enumerate() {
            println!(
                "{:>3} {:<24} ${:>6} ${:>6} {:+7} {:>7.2}% {:>7.2}%",
                index + 1,
                score.player_name,
                score.market_price,
                score.fair_price,
                score.expected_edge,
                score.pass_projected_matchup_win_probability * 100.0,
                score.market_price_projected_matchup_win_probability * 100.0,
            );
        }

        let mut bottom = analysis.scores.clone();
        bottom.sort_by_key(|score| score.expected_edge);

        println!();
        println!("BOTTOM 10 VALUES");
        println!(
            "{:>3} {:<24} {:>7} {:>7} {:>7}",
            "#", "Player", "MARKET", "FAIR", "EDGE"
        );

        for (index, score) in bottom.iter().take(10).enumerate() {
            println!(
                "{:>3} {:<24} ${:>6} ${:>6} {:+7}",
                index + 1,
                score.player_name,
                score.market_price,
                score.fair_price,
                score.expected_edge,
            );
        }

        println!();
        println!("TOP 3 VALUE PATHS");

        for score in analysis.scores.iter().take(3) {
            println!();
            println!(
                "{} | market ${} | fair ${} | edge {:+}",
                score.player_name, score.market_price, score.fair_price, score.expected_edge,
            );

            println!(
                "  H: PASS {:.2}% | BUY@MKT {:.2}% | BUY@FAIR {:.2}%",
                score.pass_projected_matchup_win_probability * 100.0,
                score.market_price_projected_matchup_win_probability * 100.0,
                score.fair_price_projected_matchup_win_probability * 100.0,
            );

            println!("  BUILD : {}", score.build_name);
            println!("  j-plan: {}", score.j_name);

            println!(
                "  future spend ${} | cash left ${}",
                score.projected_future_spend, score.projected_budget_left,
            );

            println!(
                "  future: {}",
                score.projected_future_player_names.join(", ")
            );
        }

        analyses.push((title.to_string(), analysis));
    }

    let compare_players = [
        "Tyrese Maxey",
        "Shai Gilgeous-Alexander",
        "Victor Wembanyama",
        "Stephen Curry",
        "Donovan Mitchell",
        "Jalen Johnson",
        "Trey Murphy III",
        "Derrick White",
        "Chet Holmgren",
        "Evan Mobley",
        "Donovan Clingan",
        "Jalen Duren",
    ];

    println!();
    println!("========================================================================");
    println!("CROSS-SCENARIO FAIR VALUE COMPARISON");
    println!("All three rosters have ${OWN_BUDGET_REMAINING} remaining.");
    println!("========================================================================");

    for player_name in compare_players {
        println!();
        println!("{player_name}");
        println!(
            "  {:<20} {:>7} {:>7} {:>7} {:>8} {:>8}",
            "SCENARIO", "MARKET", "FAIR", "EDGE", "PASS", "BUY@MKT"
        );

        for (title, analysis) in &analyses {
            if let Some(score) = analysis
                .scores
                .iter()
                .find(|score| score.player_name == player_name)
            {
                println!(
                    "  {:<20} ${:>6} ${:>6} {:+7} {:>7.2}% {:>7.2}%",
                    title,
                    score.market_price,
                    score.fair_price,
                    score.expected_edge,
                    score.pass_projected_matchup_win_probability * 100.0,
                    score.market_price_projected_matchup_win_probability * 100.0,
                );
            }
        }
    }

    Ok(())
}

pub fn print_blank_auction(app: &App) -> anyhow::Result<()> {
    let candidates = app
        .players
        .iter()
        .map(|player| player.id.clone())
        .collect::<Vec<_>>();

    let analysis = app.durant.auction_analysis(
        &[],
        200,
        &[],
        &[],
        &candidates,
        durant::AuctionConfig::default(),
    )?;

    println!();
    println!("========================================================================");
    println!("AUCTION DURANT — EMPTY ROSTER");
    println!("========================================================================");
    println!(
        "slots={} | league $={} | reserved $={} | discretionary $={} | repl G={:.3} | G/$={:.5}",
        analysis.economy.remaining_roster_slots,
        analysis.economy.remaining_league_dollars,
        analysis.economy.reserved_minimum_dollars,
        analysis.economy.discretionary_dollars,
        analysis.economy.replacement_g_score,
        analysis.economy.g_above_replacement_per_dollar,
    );

    let mut by_market = analysis.scores.clone();
    by_market.sort_by(|a, b| b.market_price_exact.total_cmp(&a.market_price_exact));

    println!();
    println!("MARKET PRICE SANITY CHECK");
    println!(
        "{:>3} {:<24} {:>9} {:>7}",
        "#", "Player", "MARKET*", "MARKET"
    );

    for (index, score) in by_market.iter().take(40).enumerate() {
        println!(
            "{:>3} {:<24} ${:>8.2} ${:>6}",
            index + 1,
            score.player_name,
            score.market_price_exact,
            score.market_price,
        );
    }

    println!();
    println!("DURANT VALUE — EMPTY ROSTER");
    println!(
        "{:>3} {:<24} {:>7} {:>7} {:>7} {:>8} {:>8} {:>9}",
        "#", "Player", "MARKET", "FAIR", "EDGE", "PASS", "BUY@MKT", "BUY@FAIR"
    );

    for (index, score) in analysis.scores.iter().take(40).enumerate() {
        println!(
            "{:>3} {:<24} ${:>6} ${:>6} {:+7} {:>7.2}% {:>7.2}% {:>8.2}%",
            index + 1,
            score.player_name,
            score.market_price,
            score.fair_price,
            score.expected_edge,
            score.pass_projected_matchup_win_probability * 100.0,
            score.market_price_projected_matchup_win_probability * 100.0,
            score.fair_price_projected_matchup_win_probability * 100.0,
        );
    }

    let positive_edges = analysis
        .scores
        .iter()
        .filter(|score| score.expected_edge > 0)
        .count();
    let zero_edges = analysis
        .scores
        .iter()
        .filter(|score| score.expected_edge == 0)
        .count();
    let negative_edges = analysis
        .scores
        .iter()
        .filter(|score| score.expected_edge < 0)
        .count();

    println!();
    println!("EDGE DISTRIBUTION");
    println!("positive: {positive_edges}");
    println!("zero    : {zero_edges}");
    println!("negative: {negative_edges}");

    let mut by_edge_ascending = analysis.scores.clone();
    by_edge_ascending.sort_by_key(|score| score.expected_edge);

    println!();
    println!("BOTTOM 20 VALUES");
    println!(
        "{:>3} {:<24} {:>7} {:>7} {:>7} {:>8} {:>8}",
        "#", "Player", "MARKET", "FAIR", "EDGE", "PASS", "BUY@MKT"
    );

    for (index, score) in by_edge_ascending.iter().take(20).enumerate() {
        println!(
            "{:>3} {:<24} ${:>6} ${:>6} {:+7} {:>7.2}% {:>7.2}%",
            index + 1,
            score.player_name,
            score.market_price,
            score.fair_price,
            score.expected_edge,
            score.pass_projected_matchup_win_probability * 100.0,
            score.market_price_projected_matchup_win_probability * 100.0,
        );
    }

    println!();
    println!("FAIR-PRICE CURVES");

    for player_name in [
        "Tyrese Maxey",
        "Jalen Brunson",
        "De'Aaron Fox",
        "Nikola Jokić",
    ] {
        let Some(score) = analysis
            .scores
            .iter()
            .find(|score| score.player_name == player_name)
        else {
            continue;
        };

        let start = score.fair_price.saturating_sub(5).max(1);
        let end = (score.fair_price + 5).min(188);
        let prices = (start..=end).collect::<Vec<_>>();

        let curve = app.durant.auction_price_curve(
            &[],
            200,
            &[],
            &[],
            &candidates,
            &score.player_id,
            &prices,
            durant::AuctionConfig::default(),
        )?;

        println!();
        println!(
            "{} | market ${} | reported fair ${} | PASS {:.2}%",
            score.player_name,
            score.market_price,
            score.fair_price,
            score.pass_projected_matchup_win_probability * 100.0,
        );
        println!("{:>7} {:>10} {:>10}", "PRICE", "H_BUY", "VS PASS");

        for (price, h_buy) in curve {
            let delta = h_buy - score.pass_projected_matchup_win_probability;
            println!(
                "${:>6} {:>9.2}% {:+9.2}%",
                price,
                h_buy * 100.0,
                delta * 100.0,
            );
        }
    }

    Ok(())
}

fn format_j(weights: [f64; 9]) -> String {
    weights
        .iter()
        .map(|weight| {
            if (weight - weight.round()).abs() < 1e-9 {
                format!("{weight:.0}")
            } else {
                format!("{weight:.1}")
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn dynamic_scenario(app: &App, roster_names: &[&str]) -> Vec<durant::DynamicDurantScore> {
    use std::collections::HashSet;

    let own_roster = roster_names
        .iter()
        .map(|name| {
            app.players
                .iter()
                .find(|player| player.name == *name)
                .unwrap_or_else(|| panic!("could not find player: {name}"))
                .id
                .clone()
        })
        .collect::<Vec<_>>();

    let owned = own_roster.iter().cloned().collect::<HashSet<_>>();

    let candidates = app
        .players
        .iter()
        .filter(|player| !owned.contains(&player.id))
        .map(|player| player.id.clone())
        .collect::<Vec<_>>();

    app.durant.dynamic_scores(&own_roster, &[], &candidates)
}

pub fn print_blank_dynamic(app: &App) {
    let scores = dynamic_scenario(app, &[]);

    println!();
    println!("==========================================================================");
    println!("DYNAMIC DURANT — EMPTY ROSTER");
    println!("Roster: —");
    println!("==========================================================================");

    println!(
        "{:>3} {:<23} {:>7} {:>7}  {:<18} {:<25} {}",
        "#", "Player", "NOW", "FINAL", "BUILD", "j-PLAN", "j",
    );

    for (index, score) in scores.iter().take(10).enumerate() {
        println!(
            "{:>3} {:<23} {:>6.2}% {:>6.2}%  {:<18} {:<25} [{}]",
            index + 1,
            score.player_name,
            score.matchup_win_probability * 100.0,
            score.projected_matchup_win_probability * 100.0,
            score.build_name,
            score.j_name,
            format_j(score.j_weights),
        );
    }

    println!();
    println!("TOP 10 STARTING PATHS");

    for score in scores.iter().take(10) {
        println!();
        println!("{}", score.player_name);
        println!("  BUILD : {}", score.build_name);
        println!("  j-plan: {}", score.j_name);
        println!("  j     : [{}]", format_j(score.j_weights));
        println!("          FG FT 3 PTS REB AST STL BLK TO");
        println!(
            "  j margin: {:.3} percentage points",
            score.j_margin * 100.0
        );

        println!("  TOP j:");
        println!(
            "    1. {:<45} {:>6.2}%  [{}]",
            score.j_name,
            score.projected_matchup_win_probability * 100.0,
            format_j(score.j_weights),
        );

        for (index, alternative) in score.j_alternatives.iter().enumerate() {
            println!(
                "    {}. {:<45} {:>6.2}%  [{}]",
                index + 2,
                alternative.j_name,
                alternative.projected_matchup_win_probability * 100.0,
                format_j(alternative.j_weights),
            );
        }

        println!(
            "  future: {}",
            score
                .projected_future_player_names
                .iter()
                .take(6)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );

        let p = score.projected_category_win_probabilities;

        println!(
            "  FG {:>3.0}% | FT {:>3.0}% | 3PM {:>3.0}% | PTS {:>3.0}% | \
             REB {:>3.0}% | AST {:>3.0}% | STL {:>3.0}% | BLK {:>3.0}% | TO {:>3.0}%",
            p[0] * 100.0,
            p[1] * 100.0,
            p[2] * 100.0,
            p[3] * 100.0,
            p[4] * 100.0,
            p[5] * 100.0,
            p[6] * 100.0,
            p[7] * 100.0,
            p[8] * 100.0,
        );
    }
}

pub fn print_dynamic_scenarios(app: &App) {
    use std::collections::HashMap;

    let neutral = dynamic_scenario(app, &[]);

    let neutral_ranks = neutral
        .iter()
        .enumerate()
        .map(|(index, score)| (score.player_id.clone(), index + 1))
        .collect::<HashMap<_, _>>();

    let scenarios: &[(&str, &[&str])] = &[
        ("GIANNIS", &["Giannis Antetokounmpo"]),
        (
            "GIANNIS + GOBERT",
            &["Giannis Antetokounmpo", "Rudy Gobert"],
        ),
        ("SGA + MAXEY", &["Shai Gilgeous-Alexander", "Tyrese Maxey"]),
    ];

    for (title, roster) in scenarios {
        let scores = dynamic_scenario(app, roster);

        println!();
        println!("==========================================================================");
        println!("DYNAMIC DURANT — {title}");
        println!("Roster: {}", roster.join(", "));
        println!("==========================================================================");

        println!(
            "{:>3} {:<23} {:>6} {:>7} {:>7}  {:<24}",
            "#", "Player", "MOVE", "NOW", "FINAL", "BUILD",
        );

        for (index, score) in scores.iter().take(50).enumerate() {
            let rank = index + 1;
            let old_rank = neutral_ranks.get(&score.player_id).copied().unwrap_or(rank);
            let movement = old_rank as isize - rank as isize;

            println!(
                "{:>3} {:<23} {:+6} {:>6.2}% {:>6.2}%  {:<24}",
                rank,
                score.player_name,
                movement,
                score.matchup_win_probability * 100.0,
                score.projected_matchup_win_probability * 100.0,
                score.build_name,
            );
        }

        println!();
        println!("TOP 5 PROJECTED CONTINUATIONS");

        for score in scores.iter().take(5) {
            println!();
            println!("{} -> {}", score.player_name, score.build_name);

            println!(
                "  future: {}",
                score
                    .projected_future_player_names
                    .iter()
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            );

            let p = score.projected_category_win_probabilities;

            println!(
                "  FG {:>3.0}% | FT {:>3.0}% | 3PM {:>3.0}% | PTS {:>3.0}% | \
                 REB {:>3.0}% | AST {:>3.0}% | STL {:>3.0}% | BLK {:>3.0}% | TO {:>3.0}%",
                p[0] * 100.0,
                p[1] * 100.0,
                p[2] * 100.0,
                p[3] * 100.0,
                p[4] * 100.0,
                p[5] * 100.0,
                p[6] * 100.0,
                p[7] * 100.0,
                p[8] * 100.0,
            );
        }
    }
}

pub fn print_durant_debug(app: &App) {
    use std::collections::HashMap;

    let model = &app.durant;
    let p = &model.parameters;

    let games_by_player = app
        .stats
        .players
        .iter()
        .map(|player| (player.player_id.clone(), player.games))
        .collect::<HashMap<_, _>>();

    let mut weeks_by_player = HashMap::new();

    for week in &app.stats.weekly {
        *weeks_by_player
            .entry(week.player_id.clone())
            .or_insert(0usize) += 1;
    }

    println!();
    println!("STATIC DURANT / G-SCORE");
    println!(
        "Q = {} players | team size = {} ",
        model.reference_size, model.team_size,
    );

    println!();
    println!("COUNTING CATEGORY PARAMETERS");
    println!(
        "{:<8} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "Cat", "mean", "sigma", "tau", "tau/sig", "shrink"
    );

    let counting = [
        ("PTS", p.points),
        ("3PM", p.threes),
        ("REB", p.rebounds),
        ("AST", p.assists),
        ("STL", p.steals),
        ("BLK", p.blocks),
        ("TO", p.turnovers),
    ];

    for (name, params) in counting {
        let noise_ratio = params.tau / params.sigma;
        let shrink = params.sigma / (params.sigma.powi(2) + params.tau.powi(2)).sqrt();

        println!(
            "{:<8} {:>10.3} {:>10.3} {:>10.3} {:>10.3} {:>10.3}",
            name, params.mean, params.sigma, params.tau, noise_ratio, shrink,
        );
    }

    println!();
    println!("PERCENTAGE CATEGORY PARAMETERS");
    println!(
        "{:<8} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
        "Cat", "att", "rate", "sigma", "tau", "tau/sig", "shrink"
    );

    for (name, params) in [("FG%", p.field_goal), ("FT%", p.free_throw)] {
        let noise_ratio = params.tau_rate / params.sigma_rate;
        let shrink =
            params.sigma_rate / (params.sigma_rate.powi(2) + params.tau_rate.powi(2)).sqrt();

        println!(
            "{:<8} {:>10.3} {:>10.4} {:>10.4} {:>10.4} {:>10.3} {:>10.3}",
            name,
            params.mean_attempts,
            params.mean_rate,
            params.sigma_rate,
            params.tau_rate,
            noise_ratio,
            shrink,
        );
    }

    let print_slice = |title: &str, start: usize, end: usize| {
        println!();
        println!("{title}");
        println!(
            "{:>3} {:<24} {:>7} {:>3} {:>5} {:>3} \
             {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6} {:>6}",
            "#",
            "Player",
            "TOTAL",
            "GP",
            "WKS",
            "Q?",
            "PTS",
            "3PM",
            "REB",
            "AST",
            "STL",
            "BLK",
            "TO",
            "FG%",
            "FT%",
        );

        for (index, score) in model
            .scores
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
        {
            let games = games_by_player.get(&score.player_id).copied().unwrap_or(0);
            let weeks = weeks_by_player.get(&score.player_id).copied().unwrap_or(0);
            let in_reference = if model.reference_players.contains(&score.player_id) {
                "Y"
            } else {
                "-"
            };

            println!(
                "{:>3} {:<24} {:>7.3} {:>3} {:>5} {:>3} \
                 {:>6.2} {:>6.2} {:>6.2} {:>6.2} {:>6.2} \
                 {:>6.2} {:>6.2} {:>6.2} {:>6.2}",
                index + 1,
                score.player_name,
                score.total,
                games,
                weeks,
                in_reference,
                score.points,
                score.threes,
                score.rebounds,
                score.assists,
                score.steals,
                score.blocks,
                score.turnovers,
                score.field_goal,
                score.free_throw,
            );
        }
    };

    let n = model.scores.len();

    print_slice("TOP 210", 0, 210);
    print_slice("MIDDLE SAMPLE — RANKS 70-80", 69.min(n), 80.min(n));
    print_slice("AROUND Q CUTOFF — RANKS 160-180", 159.min(n), 180.min(n));
    print_slice("BOTTOM 15", n.saturating_sub(15), n);
}

#[derive(Debug, Default)]
struct StrategyCensusEntry {
    best_wins: usize,
    top_three_appearances: usize,
    best_margin_sum: f64,
    best_h_sum: f64,
    scenario_hits: std::collections::HashSet<String>,
    example_names: Vec<String>,
}

pub fn print_j_census(app: &App) {
    use std::collections::HashMap;

    // Deliberately varied roster states. This is a pilot census for the
    // current 2,620-vector library, not the overnight giant search.
    let scenarios: &[(&str, &[&str])] = &[
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

    let all_candidates = app
        .durant
        .scores
        .iter()
        .map(|score| score.player_id.clone())
        .collect::<Vec<_>>();

    let mut census = HashMap::<String, StrategyCensusEntry>::new();
    let mut total_best_observations = 0usize;
    let mut total_top_three_observations = 0usize;

    println!();
    println!("========================================================================");
    println!("DURANT j-STRATEGY CENSUS");
    println!("========================================================================");
    println!(
        "{} representative states | current strategy library",
        scenarios.len()
    );

    for (scenario_name, roster_names) in scenarios {
        let own_roster = roster_names
            .iter()
            .map(|name| {
                app.durant
                    .scores
                    .iter()
                    .find(|score| score.player_name == *name)
                    .unwrap_or_else(|| panic!("could not find DURANT player: {name}"))
                    .player_id
                    .clone()
            })
            .collect::<Vec<_>>();

        let owned = own_roster
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();

        let candidates = all_candidates
            .iter()
            .filter(|player_id| !owned.contains(*player_id))
            .cloned()
            .collect::<Vec<_>>();

        let scores = app.durant.dynamic_scores(&own_roster, &[], &candidates);

        println!(
            "  {:<20} {:>4} candidate decisions",
            scenario_name,
            scores.len()
        );

        for score in scores {
            total_best_observations += 1;

            let key = format_j(score.j_weights);
            let entry = census.entry(key).or_default();
            entry.best_wins += 1;
            entry.top_three_appearances += 1;
            entry.best_margin_sum += score.j_margin;
            entry.best_h_sum += score.projected_matchup_win_probability;
            entry.scenario_hits.insert((*scenario_name).to_string());

            if entry.example_names.len() < 3 {
                entry
                    .example_names
                    .push(format!("{} / {}", scenario_name, score.player_name));
            }

            total_top_three_observations += 1;

            for alternative in &score.j_alternatives {
                let alt_key = format_j(alternative.j_weights);
                let alt_entry = census.entry(alt_key).or_default();
                alt_entry.top_three_appearances += 1;
                alt_entry.scenario_hits.insert((*scenario_name).to_string());
                total_top_three_observations += 1;
            }
        }
    }

    let mut rows = census.into_iter().collect::<Vec<_>>();

    let distinct_best = rows.iter().filter(|(_, entry)| entry.best_wins > 0).count();

    let distinct_top_three = rows
        .iter()
        .filter(|(_, entry)| entry.top_three_appearances > 0)
        .count();

    rows.sort_by(|a, b| {
        b.1.best_wins
            .cmp(&a.1.best_wins)
            .then_with(|| b.1.top_three_appearances.cmp(&a.1.top_three_appearances))
    });

    println!();
    println!("SUMMARY");
    println!("best-j observations : {total_best_observations}");
    println!("top-3 observations  : {total_top_three_observations}");
    println!("distinct best j     : {distinct_best}");
    println!("distinct top-3 j    : {distinct_top_three}");

    println!();
    println!("TOP STRATEGIES BY #1 WINS");
    println!(
        "{:>3} {:<27} {:>7} {:>7} {:>8} {:>9} {:>10}",
        "#", "j", "WINS", "TOP-3", "STATES", "AVG ΔH", "AVG H"
    );

    for (index, (key, entry)) in rows.iter().take(40).enumerate() {
        let avg_margin = if entry.best_wins == 0 {
            0.0
        } else {
            entry.best_margin_sum / entry.best_wins as f64
        };

        let avg_h = if entry.best_wins == 0 {
            0.0
        } else {
            entry.best_h_sum / entry.best_wins as f64
        };

        println!(
            "{:>3} [{:<25}] {:>7} {:>7} {:>8} {:>8.3}pp {:>9.2}%",
            index + 1,
            key,
            entry.best_wins,
            entry.top_three_appearances,
            entry.scenario_hits.len(),
            avg_margin * 100.0,
            avg_h * 100.0,
        );
    }

    println!();
    println!("CUMULATIVE COVERAGE OF BEST-j DECISIONS");

    for cutoff in [5usize, 10, 25, 50, 75, 100, 150, 200] {
        let covered = rows
            .iter()
            .take(cutoff)
            .map(|(_, entry)| entry.best_wins)
            .sum::<usize>();

        let share = if total_best_observations == 0 {
            0.0
        } else {
            covered as f64 / total_best_observations as f64
        };

        println!(
            "top {:>3}: {:>5}/{:<5} = {:>6.2}%",
            cutoff.min(rows.len()),
            covered,
            total_best_observations,
            share * 100.0,
        );
    }

    let mut niche = rows
        .iter()
        .filter(|(_, entry)| entry.best_wins >= 2)
        .collect::<Vec<_>>();

    niche.sort_by(|a, b| {
        let a_margin = a.1.best_margin_sum / a.1.best_wins as f64;
        let b_margin = b.1.best_margin_sum / b.1.best_wins as f64;
        b_margin.total_cmp(&a_margin)
    });

    println!();
    println!("HIGH-CONVICTION / NICHE STRATEGIES");
    println!(
        "{:>3} {:<27} {:>7} {:>8} {:>9}  {}",
        "#", "j", "WINS", "STATES", "AVG ΔH", "EXAMPLES"
    );

    for (index, (key, entry)) in niche.iter().take(20).enumerate() {
        let avg_margin = entry.best_margin_sum / entry.best_wins as f64;

        println!(
            "{:>3} [{:<25}] {:>7} {:>8} {:>8.3}pp  {}",
            index + 1,
            key,
            entry.best_wins,
            entry.scenario_hits.len(),
            avg_margin * 100.0,
            entry.example_names.join(" | "),
        );
    }

    println!();
    println!("Interpretation:");
    println!("  WINS   = times this j was the best continuation");
    println!("  TOP-3  = times this j appeared among the stored best three");
    println!("  STATES = number of different roster states where it appeared");
    println!("  AVG ΔH = average gap over the runner-up when this j won");
    println!();
    println!(
        "The overnight search should reuse this census logic and save only the \
         strategies needed for high coverage plus genuinely high-margin niche plans."
    );
}
