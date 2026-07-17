mod app;

use anyhow::Result;
use app::App;
use crossterm::ExecutableCommand;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::read;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::disable_raw_mode;
use crossterm::terminal::enable_raw_mode;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Alignment;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::widgets::Paragraph;

type BirdTerminal = Terminal<CrosstermBackend<std::io::Stdout>>;

const LOGO: &str = r#"
          \
      \\ \
   \\\\ \
\\\\\\ \
      
B I R D B O A R D
3 3
      "#;

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

fn run(terminal: &mut BirdTerminal, app: &mut App) -> Result<()> {
    while app.running {
        let logo = Paragraph::new(LOGO).alignment(Alignment::Center);
        let placeholder = Paragraph::new("Big board incomming...").alignment(Alignment::Center);
        let quiting = Paragraph::new("Press 'q' to quit").alignment(Alignment::Left);

        terminal.draw(|frame| {
            let areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(10),
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .split(frame.area());
            frame.render_widget(logo, areas[0]);
            frame.render_widget(placeholder, areas[1]);
            frame.render_widget(quiting, areas[2]);
        })?;
        let event = read()?;

        if let Event::Key(key) = event
            && key.code == KeyCode::Char('q')
        {
            app.quit();
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
