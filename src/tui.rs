use anyhow::Result;
use crossterm::ExecutableCommand;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

pub type BirdTerminal = Terminal<CrosstermBackend<std::io::Stdout>>;

pub fn init_terminal() -> Result<BirdTerminal> {
    let mut stdout = std::io::stdout();
    enable_raw_mode()?;
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}
pub fn restore_terminal(terminal: &mut BirdTerminal) -> Result<()> {
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}
