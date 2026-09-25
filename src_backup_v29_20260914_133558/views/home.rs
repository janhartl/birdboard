use crate::app::{App, SessionPhase};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const LOGO: &str = r#"
          \\
      \\\\ \\
   \\\\\\\\ \\
\\\\\\\\\\ \\

B I R D B O A R D
"#;

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    frame.render_widget(Paragraph::new(LOGO).alignment(Alignment::Center), areas[0]);

    frame.render_widget(
        Paragraph::new(Line::from("Who's playing for 2nd?").italic().dim())
            .alignment(Alignment::Center),
        areas[1],
    );

    let selected = app.home_phase_selection;
    let modes = vec![
        mode_line("Preparation", matches!(selected, SessionPhase::Preparation)),
        mode_line("Live draft", matches!(selected, SessionPhase::LiveDraft)),
    ];

    frame.render_widget(Paragraph::new(modes).alignment(Alignment::Center), areas[3]);

    if app.live_refresh_pending {
        let strategy_count = app.runtime_strategy_count();
        let status = if strategy_count == 0 {
            "Preparing live board...".to_string()
        } else {
            format!("Preparing live board · {strategy_count} DURANT strategies...")
        };

        frame.render_widget(
            Paragraph::new(status)
                .alignment(Alignment::Center)
                .style(Style::default().add_modifier(Modifier::DIM)),
            areas[4],
        );
    }

    let footer = match app.home_phase_selection {
        SessionPhase::Preparation => "[b] Big board  [q] Quit",
        SessionPhase::LiveDraft => "[b] Big board  [r] Rosters  [s] Strategy  [q] Quit",
    };

    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::DIM)),
        areas[5],
    );
}

fn mode_line(label: &'static str, selected: bool) -> Line<'static> {
    if selected {
        Line::from(vec![
            Span::styled("> ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(label, Style::default().add_modifier(Modifier::BOLD)),
        ])
    } else {
        Line::from(Span::styled(
            format!("  {label}"),
            Style::default().add_modifier(Modifier::DIM),
        ))
    }
}
