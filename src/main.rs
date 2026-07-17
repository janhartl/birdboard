mod app;
mod event;
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
    let mut app = App::new();
    let mut terminal = init_terminal()?;

    let result = run(&mut terminal, &mut app);

    restore_terminal(&mut terminal)?;

    result
}
