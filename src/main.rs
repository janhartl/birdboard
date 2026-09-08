mod app;
mod data;
mod draft;
mod event;
mod player;
mod stats;
mod strategy;
mod team;
mod tui;
mod ui;

mod views;

use anyhow::Result;
use app::App;
use crossterm::event::Event;
use event::read_event;
use tui::BirdTerminal;
use tui::init_terminal;
use tui::restore_terminal;
use ui::draw;

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
    let mut terminal = init_terminal()?;

    let result = run(&mut terminal, &mut app);

    restore_terminal(&mut terminal)?;

    result
}
