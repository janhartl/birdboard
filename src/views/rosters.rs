use crate::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::{Block, Borders};

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(15),
            Constraint::Length(15),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let teams_per_row = app.teams.len().div_ceil(2).max(1);

    for (row_index, teams) in app.teams.chunks(teams_per_row).enumerate() {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Ratio(1, teams.len() as u32); teams.len()])
            .split(areas[row_index]);

        for (team, area) in teams.iter().zip(columns.iter()) {
            let roster_block = Block::default()
                .borders(Borders::ALL)
                .title(team.name.as_str());

            frame.render_widget(roster_block, *area);
        }
    }
}
