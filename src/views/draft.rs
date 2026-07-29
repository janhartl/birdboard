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

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    let mut player_items = Vec::new();

    for (index, player) in app.players.iter().enumerate() {
        let text = format!(
            " {}. | {} | {} | ${}",
            index + 1,
            player.name.as_str(),
            player.position.as_str(),
            player.projected_value,
        );
        player_items.push(ListItem::new(Line::from(text).alignment(Alignment::Center)));
    }
    let player_list =
        List::new(player_items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let visible_rows = areas[0].height as usize;
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

    frame.render_stateful_widget(player_list, areas[0], &mut list_state);

    let footer_text = match app.draft_mode {
        DraftMode::BrowsingPlayers => String::from("[j/k] | [Enter] to draft | [q] Quit"),

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
