mod app;
mod data;
mod debug;
mod draft;
mod durant;
mod event;
mod h_calibration;
mod h_validation;
mod player;
mod pricing_validation;
mod projection;
mod stats;
mod strategy;
mod strategy_bank;
mod strategy_stage;
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
        // Never block on planner completion. Merge any finished worker result
        // before drawing so Strategy updates on the very next frame.
        app.poll_background_work();

        terminal.draw(|frame| {
            draw(frame, app);
        })?;

        // Some jobs (notably entering Live mode) are intentionally deferred by
        // one frame so the user sees immediate visual feedback before the
        // synchronous calculation begins.
        if app.process_pending_work() {
            continue;
        }

        if let Some(Event::Key(key)) = read_event()? {
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
        Some("--stage-j-info") => {
            strategy_stage::print_stage_relearn_plan(&app);
            return Ok(());
        }
        Some("--stage-j-train") => {
            strategy_stage::train_stage_early_bank(&app)?;
            return Ok(());
        }
        Some("--stage-j-snapshot-info") => {
            strategy_stage::print_stage_snapshot_plan(&app);
            return Ok(());
        }
        Some("--stage-j-snapshot") => {
            strategy_stage::validate_stage_snapshot(&app)?;
            return Ok(());
        }
        Some("--funnel-benchmark-info") => {
            strategy_stage::print_strategy_funnel_benchmark_plan(&app);
            return Ok(());
        }
        Some("--funnel-benchmark") => {
            strategy_stage::benchmark_strategy_funnel(&app)?;
            return Ok(());
        }
        Some("--full-price-curve-info") => {
            pricing_validation::print_plan();
            return Ok(());
        }
        Some("--full-price-curve") => {
            pricing_validation::run(&app)?;
            return Ok(());
        }
        Some("--validate-stage-j-info") => {
            strategy_stage::print_stage_validation_plan(&app);
            return Ok(());
        }
        Some("--validate-stage-j") => {
            strategy_stage::validate_stage_aware_bank(&app)?;
            return Ok(());
        }
        Some("--validate-h-info") => {
            h_validation::print_plan(&app);
            return Ok(());
        }
        Some("--validate-h") => {
            h_validation::validate_and_save(&app)?;
            return Ok(());
        }
        Some("--h-calibration-info") => {
            h_calibration::print_plan(&app);
            return Ok(());
        }
        Some("--fit-h-calibration") => {
            h_calibration::fit_validate_and_save(&app)?;
            return Ok(());
        }
        Some("--validate-deep-bank-info") | Some("--validate-current-j-info") => {
            strategy_validation::print_plan(&app);
            return Ok(());
        }
        Some("--validate-deep-bank") | Some("--validate-current-j") => {
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
            println!("  stage      : {}", app.runtime_strategy_stage_label());
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
