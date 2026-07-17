//mod app;

use anyhow::Result;
use crossterm::ExecutableCommand;
//use app::App;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::Paragraph;

type BirdTerminal = Terminal<CrosstermBackend<std::io::Stdout>>;

fn init_terminal() -> Result<BirdTerminal> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}
fn restore_terminal(terminal: &mut BirdTerminal) -> Result<()> {
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}

fn run(terminal: &mut BirdTerminal) -> Result<()> {
    let message = Paragraph::new("Welcome to birdboard");
    terminal.draw(|frame| {
        frame.render_widget(message, frame.area());
    })?;
    std::thread::sleep(std::time::Duration::from_secs(4));
    Ok(())
}

fn main() -> Result<()> {
    //    let mut app = App::new();
    let mut terminal = init_terminal()?;

    let result = run(&mut terminal);

    // app.quit();
    restore_terminal(&mut terminal)?;

    Ok(result?)
}
