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

    print_durant_debug(&app);
    return Ok(());

    let mut terminal = init_terminal()?;

    let result = run(&mut terminal, &mut app);

    restore_terminal(&mut terminal)?;

    result
}
