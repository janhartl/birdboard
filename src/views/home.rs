use crate::app::App;

use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::style::Stylize;
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

const LOGO: &str = r#"
          \
      \\ \
   \\\\ \
\\\\\\ \
      
B I R D B O A R D
"#;

pub fn draw(frame: &mut Frame, _app: &App) {
    let logo = Paragraph::new(LOGO).alignment(Alignment::Center);
    let slogan = Paragraph::new(Line::from("Who's playing for 2nd?").italic().dim())
        .alignment(Alignment::Center);
    let command_line = Paragraph::new("[h] Home | [b] Big board | [r] Rosters | [s] Strategy")
        .alignment(Alignment::Center);
    let quit = Paragraph::new("[q] Quit").alignment(Alignment::Left);

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());
    frame.render_widget(logo, areas[0]);
    frame.render_widget(slogan, areas[1]);
    frame.render_widget(command_line, areas[3]);
    frame.render_widget(quit, areas[5]);
}
