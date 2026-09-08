mod app;
mod data;
mod draft;
mod durant;
mod event;
mod player;
mod stats;
mod strategy;
mod team;
mod tui;
mod ui;

mod views;

use anyhow::Ok;
use anyhow::Result;
use app::App;
use crossterm::event::Event;
use event::read_event;
use tui::BirdTerminal;
use tui::init_terminal;
use tui::restore_terminal;
use ui::draw;

fn format_j(weights: [f64; 9]) -> String {
    weights
        .iter()
        .map(|weight| {
            if (weight - weight.round()).abs() < 1e-9 {
                format!("{:.0}", weight)
            } else {
                format!("{:.1}", weight)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn print_blank_dynamic(app: &App) {
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

    for (index, score) in scores.iter().take(50).enumerate() {
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

    for score in scores.iter().take(30) {
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

    // Empty opponents = generic average opponent in DurantModel.
    app.durant.dynamic_scores(&own_roster, &[], &candidates)
}

fn print_dynamic_scenarios(app: &App) {
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
fn print_durant_debug(app: &App) {
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

fn run(terminal: &mut BirdTerminal, app: &mut App) -> Result<()> {
    while app.running {
        terminal.draw(|frame| {
            draw(frame, app);
        })?;
        let event = read_event()?;

        if let Event::Key(key) = event {
            app.handle_key(key);
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let season_stats = stats::load_or_fetch()?;

    println!(
        "Loaded {} players for {} using {} statistics",
        season_stats.players.len(),
        season_stats.draft_season,
        season_stats.source_season,
    );

    let mut app = App::new(season_stats)?;

    print_dynamic_scenarios(&app);
    print_blank_dynamic(&app);

    // print_durant_debug(&app);

    return Ok(());

    let mut terminal = init_terminal()?;

    let result = run(&mut terminal, &mut app);

    restore_terminal(&mut terminal)?;

    result
}
