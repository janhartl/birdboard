use crate::app::App;
use crate::player::PlayerId;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph};

const TATUM_ID: PlayerId = PlayerId(6);

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    let has_tatum = app.team_has_player(app.user_team_id, TATUM_ID);

    let strategy_text = if has_tatum {
        "\
TATUM BALANCED BUILD

Strengths
  Points
  Three-pointers
  Free-throw percentage
  Stable all-around production

Priorities
  Primary assists
  Blocks
  Field-goal percentage

Avoid
  Overpaying for another scoring wing"
    } else {
        "\
No anchor strategy active

Draft a cornerstone player to activate a build plan."
    };

    let strategy = Paragraph::new(strategy_text)
        .block(Block::default().borders(Borders::ALL).title(" Strategy "));

    frame.render_widget(strategy, areas[0]);

    let footer = Paragraph::new("[b] Big board | [h] Home | [r] Rosters | [q] Quit")
        .alignment(Alignment::Center)
        .style(Style::default().add_modifier(Modifier::DIM));

    frame.render_widget(footer, areas[1]);
}
