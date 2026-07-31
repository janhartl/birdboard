use crate::app::App;
use crate::player::PlayerId;

use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::widgets::Paragraph;

pub fn draw(frame: &mut Frame, app: &App) {
    let strategy = Paragraph::new("Strategy").alignment(Alignment::Center);
    let no_tatum = Paragraph::new("NO TATUM").alignment(Alignment::Center);
    let footer = Paragraph::new("[b] Big board | [h] Home | [r] Roster | [q] Quit")
        .alignment(Alignment::Center);
    const TATUM_ID: PlayerId = PlayerId(6);

    let has_tatum = app.team_has_player(app.user_team_id, TATUM_ID);

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    if has_tatum {
        frame.render_widget(strategy, areas[0]);
        frame.render_widget(footer, areas[1]);
    } else {
        frame.render_widget(no_tatum, areas[0]);
        frame.render_widget(footer, areas[1]);
    }
}
