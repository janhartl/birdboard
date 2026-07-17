//mod app;

use anyhow::Result;
use crossterm::ExecutableCommand;
//use app::App;
use crossterm::execute;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::Paragraph;
use std::io::stdout;

fn main() -> Result<()> {
    //    let mut app = App::new();
    //    setup
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let message = Paragraph::new("Welcome to birdboard");
    terminal.draw(|frame| {
        frame.render_widget(message, frame.area());
    })?;
    std::thread::sleep(std::time::Duration::from_secs(4));
    // println!("Welcome to birdboard!");
    // println!("Running: {}", app.running);

    // app.quit();
    // println!("Running: {}", app.running);
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;

    Ok(())
}
