use crate::app::App;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem};

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
            let mut roster_items = Vec::new();

            for pick in app
                .draft_picks
                .iter()
                .filter(|pick| pick.team_id == team.id)
            {
                if let Some(player) = app.player_by_id(pick.player_id) {
                    let price = format!("${}", pick.price);

                    let row_width = area.width.saturating_sub(2) as usize;

                    let name_width = row_width.saturating_sub(price.len());

                    let text = format!("{:<name_width$}{}", player.display_name(), price,);

                    roster_items.push(ListItem::new(text));
                }
            }

            if roster_items.is_empty() {
                roster_items.push(
                    ListItem::new("No players drafted")
                        .style(Style::default().add_modifier(Modifier::DIM)),
                );
            }

            let roster_list = List::new(roster_items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(team.name.as_str()),
            );

            frame.render_widget(roster_list, *area);
        }
    }
}
