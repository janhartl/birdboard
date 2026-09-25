mod app;
mod data;
mod debug;
mod draft;
mod durant;
mod event;
mod player;
mod projection;
mod stats;
mod strategy;
mod strategy_bank;
mod strategy_validation;
mod team;
mod tui;
mod ui;
mod views;

use anyhow::Result;
use app::App;
use crossterm::event::Event;
use event::read_event;
use tui::{BirdTerminal, init_terminal, restore_terminal};
use ui::draw;

fn run(terminal: &mut BirdTerminal, app: &mut App) -> Result<()> {
    while app.running {
        terminal.draw(|frame| {
            draw(frame, app);
        })?;

        // Some jobs (notably entering Live mode) are intentionally deferred by
        // one frame so the user sees immediate visual feedback before the
        // synchronous calculation begins.
        if app.process_pending_work() {
            continue;
        }

        if let Event::Key(key) = read_event()? {
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

    match std::env::args().nth(1).as_deref() {
        Some("--debug-overrides") => {
            debug::print_manual_override_players(&app);
            return Ok(());
        }
        Some("--debug-auction") => {
            debug::print_blank_auction(&app)?;
            return Ok(());
        }
        Some("--debug-scenarios") => {
            debug::print_roster_auction_scenarios(&app)?;
            return Ok(());
        }
        Some("--debug-dynamic") => {
            debug::print_blank_dynamic(&app);
            debug::print_dynamic_scenarios(&app);
            return Ok(());
        }
        Some("--debug-static") => {
            debug::print_durant_debug(&app);
            return Ok(());
        }
        Some("--debug-j-census") => {
            debug::print_j_census(&app);
            return Ok(());
        }
        Some("--build-strategy-bank") => {
            strategy_bank::build_and_save(&app)?;
            return Ok(());
        }
        Some("--overnight-j-info") => {
            strategy_bank::print_overnight_plan(&app);
            return Ok(());
        }
        Some("--overnight-j-search") => {
            strategy_bank::build_overnight_and_save(&app)?;
            return Ok(());
        }
        Some("--deep-j-info") => {
            strategy_bank::print_deep_plan(&app);
            return Ok(());
        }
        Some("--deep-j-search") => {
            strategy_bank::build_deep_and_save(&app)?;
            return Ok(());
        }
        Some("--auction-j-info") => {
            strategy_bank::print_auction_relearn_plan(&app);
            return Ok(());
        }
        Some("--auction-j-search") => {
            strategy_bank::build_auction_and_save(&app)?;
            return Ok(());
        }
        Some("--auction-j-medium-info") => {
            strategy_bank::print_auction_medium_plan(&app);
            return Ok(());
        }
        Some("--auction-j-medium") => {
            strategy_bank::build_auction_medium_and_save(&app)?;
            return Ok(());
        }
        Some("--auction-j-quick-info") => {
            strategy_bank::print_auction_quick_plan(&app);
            return Ok(());
        }
        Some("--auction-j-quick") => {
            strategy_bank::build_auction_quick_and_save(&app)?;
            return Ok(());
        }
        Some("--validate-deep-bank-info") => {
            strategy_validation::print_plan(&app);
            return Ok(());
        }
        Some("--validate-deep-bank") => {
            strategy_validation::validate_and_save(&app)?;
            return Ok(());
        }
        Some("--live-bank-info") => {
            println!("Runtime strategy bank");
            println!(
                "  source     : {}",
                app.runtime_strategy_source().unwrap_or("not loaded")
            );
            println!(
                "  profile    : {}",
                app.runtime_strategy_profile().unwrap_or("—")
            );
            println!("  strategies : {}", app.runtime_strategy_count());
            if let Some(plan) = &app.live_roster_plan {
                println!(
                    "  projected H: {:.2}%",
                    plan.projected_matchup_win_probability * 100.0
                );
                println!("  build      : {}", plan.build_name);
                println!("  j          : {:?}", plan.j_weights);
            }
            return Ok(());
        }
        Some(arg) => anyhow::bail!("unknown argument: {arg}"),
        None => {}
    }

    let mut terminal = init_terminal()?;
    let result = run(&mut terminal, &mut app);
    restore_terminal(&mut terminal)?;

    result
}
