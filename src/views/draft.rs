use crate::app::App;
use crate::draft::DraftMode;
use ratatui::Frame;
use ratatui::layout::Alignment;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::ListState;
use ratatui::widgets::Paragraph;
use ratatui::widgets::{Block, Borders};

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    let content_areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(75), Constraint::Percentage(25)])
        .split(areas[0]);

    let team_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(app.teams.len() as u16),
            Constraint::Min(0),
        ])
        .split(content_areas[1]);

    let mut player_items = Vec::new();

    for (index, player) in app.players.iter().enumerate() {
        let draft_pick = app.draft_pick_for_player(player.id);
        let drafted = draft_pick.is_some();
        let status = if drafted { " | DRAFTED" } else { "" };
        let text = format!(
            " {}. | {} | {} | ${}{}",
            index + 1,
            player.name,
            player.position,
            player.projected_value,
            status,
        );
        let style = if drafted {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default()
        };

        player_items
            .push(ListItem::new(Line::from(text).alignment(Alignment::Center)).style(style));
    }
    let player_list =
        List::new(player_items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let visible_rows = content_areas[0].height as usize;
    let half_screen_height = visible_rows / 2;
    let offset = match app.selected_player {
        Some(index) => {
            let desired_offset = index.saturating_sub(half_screen_height);
            let max_offset = app.players.len().saturating_sub(visible_rows);

            desired_offset.min(max_offset)
        }
        None => 0,
    };

    let mut list_state = ListState::default()
        .with_selected(app.selected_player)
        .with_offset(offset);

    frame.render_stateful_widget(player_list, content_areas[0], &mut list_state);

    let mut team_items = Vec::new();
    for team in &app.teams {
        let budget = format!("${}", team.budget);

        let row_width = content_areas[1].width.saturating_sub(2) as usize;

        let name_width = row_width.saturating_sub(budget.len());

        let text = format!(" {:<name_width$}{} ", team.name, budget,);

        team_items.push(ListItem::new(text));
    }

    let team_list = List::new(team_items);
    frame.render_widget(team_list, team_areas[0]);

    match (app.draft_mode, app.selected_team) {
        (DraftMode::RecordingDraft, Some(team_index)) => {
            let team = &app.teams[team_index];

            let mut roster_items = Vec::new();
            for pick in app
                .draft_picks
                .iter()
                .filter(|pick| pick.team_id == team.id)
            {
                if let Some(player) = app.player_by_id(pick.player_id) {
                    let price = format!("${}", pick.price);

                    let row_width = team_areas[1].width.saturating_sub(2) as usize;

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
            frame.render_widget(roster_list, team_areas[1]);
        }

        _ => {
            frame.render_widget(Paragraph::new(""), team_areas[1]);
        }
    };

    let footer_text = match app.draft_mode {
        DraftMode::SearchingPlayer => {
            format!("/{}_", app.search_query)
        }
        DraftMode::BrowsingPlayers => String::from("[j/k] | [Enter] Draft player | [q] Quit"),

        DraftMode::RecordingDraft => {
            if let (Some(player_index), Some(team_index)) = (app.selected_player, app.selected_team)
            {
                let player = &app.players[player_index];
                let team = &app.teams[team_index];

                format!(
                    "Draft: {} -> {} |  Price: ${}_",
                    player.name, team.name, app.draft_price_input,
                )
            } else {
                String::from("Unable to record draft")
            }
        }
    };

    let footer = Paragraph::new(footer_text).alignment(Alignment::Center);
    frame.render_widget(footer, areas[1]);
}
