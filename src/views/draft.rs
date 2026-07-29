use crate::app::App;
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

    let quitting = Paragraph::new("Press 'q' to quit").alignment(Alignment::Left);

    frame.render_stateful_widget(player_list, areas[0], &mut list_state);
    frame.render_widget(quitting, areas[1]);
}
