use crate::app::{App, InteractionMode, PendingEditCommand};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

pub fn draw(frame: &mut Frame, app: &App) {
    const TEAMS_PER_ROW: usize = 5;
    const ROSTER_HEIGHT: u16 = 15;

    let row_count = app.teams.len().div_ceil(TEAMS_PER_ROW).max(1);

    let mut vertical_constraints = vec![Constraint::Length(ROSTER_HEIGHT); row_count];

    vertical_constraints.push(Constraint::Length(1));

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vertical_constraints)
        .split(frame.area());

    for (row_index, teams) in app.teams.chunks(TEAMS_PER_ROW).enumerate() {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![
                Constraint::Ratio(1, TEAMS_PER_ROW as u32,);
                TEAMS_PER_ROW
            ])
            .split(areas[row_index]);

        let first_column = (TEAMS_PER_ROW - teams.len()) / 2;

        for (team_offset, (team, area)) in teams
            .iter()
            .zip(columns.iter().skip(first_column))
            .enumerate()
        {
            let team_index = row_index * TEAMS_PER_ROW + team_offset;

            let selected = matches!(app.interaction_mode, InteractionMode::Edit)
                && app.selected_roster_team == Some(team_index);

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

            let roster_count = roster_items.len();

            if roster_items.is_empty() {
                roster_items.push(
                    ListItem::new("Empty").style(Style::default().add_modifier(Modifier::DIM)),
                );
            }

            let title = if team.id == app.user_team_id {
                format!(" {} · YOU ", team.name)
            } else {
                format!(" {} ", team.name)
            };

            let border_style = if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let roster_list = List::new(roster_items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title_top(Line::from(title).alignment(Alignment::Center))
                    .title_bottom(
                        Line::from(format!(" ${} · {}/13", team.budget, roster_count,))
                            .alignment(Alignment::Center),
                    ),
            );

            frame.render_widget(roster_list, *area);
        }
    }

    let footer_text = if let Some(input) = &app.team_input {
        match &input.error {
            Some(error) => {
                format!("Enter team name: {}_ · {}", input.value, error,)
            }

            None => {
                format!("Enter team name: {}_", input.value,)
            }
        }
    } else if matches!(app.interaction_mode, InteractionMode::Edit) {
        let unsaved = if app.teams_dirty { " · UNSAVED" } else { "" };

        let pending = if matches!(app.pending_edit_command, PendingEditCommand::Delete) {
            " · d…"
        } else {
            ""
        };

        format!(
            "{unsaved}{pending}\
             [Tab] Select | [Enter] Rename | \
             [a] Add | [dd] Remove | [c] Claim | \
             [s] Save | [E/Esc] Leave"
        )
    } else {
        String::from(
            "[b] Big board | [h] Home | \
             [s] Strategy | [E] Edit teams | [q] Quit",
        )
    };

    let footer = Paragraph::new(footer_text)
        .alignment(Alignment::Center)
        .style(Style::default().add_modifier(Modifier::DIM));

    frame.render_widget(footer, areas[row_count]);
}
