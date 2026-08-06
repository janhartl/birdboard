use crate::app::{
    App, BoardMode, PendingEditCommand, PlayerForm, PlayerFormField, PlayerFormMode, SessionPhase,
};
use crate::draft::DraftMode;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

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

    render_player_board(frame, app, content_areas[0]);

    render_team_list(frame, app, team_areas[0]);

    render_right_detail_panel(frame, app, team_areas[1]);

    render_footer(frame, app, areas[1]);
}

fn render_player_board(frame: &mut Frame, app: &App, area: Rect) {
    let rank_width = app.players.len().to_string().len();

    let name_width = app
        .players
        .iter()
        .map(|player| player.name.chars().count())
        .max()
        .unwrap_or(0)
        .saturating_add(10);

    let position_width = app
        .players
        .iter()
        .map(|player| player.position.chars().count())
        .max()
        .unwrap_or(0);

    let value_width = app
        .players
        .iter()
        .map(|player| format!("${}", player.projected_value).chars().count())
        .max()
        .unwrap_or(0);

    let player_items = app
        .players
        .iter()
        .enumerate()
        .map(|(index, player)| {
            let drafted = app.draft_pick_for_player(player.id).is_some();

            let status = if drafted { " DRAFTED" } else { "" };

            let text = format!(
                " {:>rank_width$}.  \
                 {:<name_width$}  \
                 {:<position_width$}  \
                 {:>value_width$} {}",
                index + 1,
                player.name,
                player.position,
                format!("${}", player.projected_value),
                status,
            );

            let style = if drafted {
                Style::default().add_modifier(Modifier::DIM)
            } else {
                Style::default()
            };

            ListItem::new(text).style(style)
        })
        .collect::<Vec<_>>();

    let player_list =
        List::new(player_items).highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let visible_rows = area.height as usize;
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

    frame.render_stateful_widget(player_list, area, &mut list_state);
}

fn render_team_list(frame: &mut Frame, app: &App, area: Rect) {
    let team_items = app
        .teams
        .iter()
        .enumerate()
        .map(|(index, team)| {
            let selected = matches!(&app.draft_mode, DraftMode::RecordingDraft)
                && app.selected_team == Some(index);

            let marker = if selected { ">" } else { " " };

            let budget = format!("${}", team.budget);

            let row_width = area.width.saturating_sub(1) as usize;

            let name_width = row_width.saturating_sub(2).saturating_sub(budget.len());

            let text = format!("{} {:<name_width$}{} ", marker, team.name, budget,);

            let style = if selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::DIM)
            };

            ListItem::new(text).style(style)
        })
        .collect::<Vec<_>>();

    let team_list = List::new(team_items).block(Block::default().borders(Borders::LEFT));

    frame.render_widget(team_list, area);
}

fn render_right_detail_panel(frame: &mut Frame, app: &App, area: Rect) {
    if matches!(&app.draft_mode, DraftMode::RecordingDraft) {
        render_roster_panel(frame, app, area);
        return;
    }

    if matches!(&app.board_mode, BoardMode::Edit) {
        match &app.player_form {
            Some(form) => {
                render_player_form(frame, form, area);
            }

            None => {
                render_editor_panel(frame, app, area);
            }
        }

        return;
    }

    frame.render_widget(Paragraph::new(""), area);
}

fn render_roster_panel(frame: &mut Frame, app: &App, area: Rect) {
    let Some(team_index) = app.selected_team else {
        frame.render_widget(Paragraph::new(""), area);

        return;
    };

    let Some(team) = app.teams.get(team_index) else {
        frame.render_widget(Paragraph::new(""), area);

        return;
    };

    let mut roster_items = app
        .draft_picks
        .iter()
        .filter(|pick| pick.team_id == team.id)
        .filter_map(|pick| {
            let player = app.player_by_id(pick.player_id)?;

            let price = format!("${}", pick.price);

            let row_width = area.width.saturating_sub(2) as usize;

            let name_width = row_width.saturating_sub(price.len());

            let text = format!("{:<name_width$}{}", player.display_name(), price,);

            Some(ListItem::new(text))
        })
        .collect::<Vec<_>>();

    let roster_count = roster_items.len();

    if roster_items.is_empty() {
        roster_items.push(
            ListItem::new("No players drafted").style(Style::default().add_modifier(Modifier::DIM)),
        );
    }

    let roster_list = List::new(roster_items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Roster · {roster_count}/13")),
    );

    frame.render_widget(roster_list, area);
}

fn action_row(left: &str, right: &str, width: usize, style: Style) -> Line<'static> {
    let gap = width.saturating_sub(left.len() + right.len());

    Line::from(vec![
        Span::styled(left.to_string(), style),
        Span::raw(" ".repeat(gap)),
        Span::styled(right.to_string(), style),
    ])
}

fn render_editor_panel(frame: &mut Frame, app: &App, area: Rect) {
    let dim_style = Style::default().add_modifier(Modifier::DIM);
    let heading_style = Style::default().add_modifier(Modifier::BOLD);

    let Some(player_index) = app.selected_player else {
        let panel = Paragraph::new(Span::styled("No player selected", dim_style))
            .block(Block::default().borders(Borders::ALL).title(" Editor "));

        frame.render_widget(panel, area);
        return;
    };

    let Some(player) = app.players.get(player_index) else {
        return;
    };

    let block = Block::default().borders(Borders::ALL).title(format!(
        " {} · {} ",
        player.name,
        player_index + 1,
    ));

    let inner_area = block.inner(area);

    frame.render_widget(block, area);

    let panel_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(inner_area);

    let mut player_lines = vec![
        editor_field("Position", &player.position),
        editor_field("Value", &format!("${}", player.projected_value)),
    ];

    if let Some(register) = &app.player_register {
        player_lines.push(Line::from(""));
        player_lines.push(Line::from(Span::styled(
            format!("Cut: {}", register.player.display_name(),),
            dim_style,
        )));
    }

    let player_details = Paragraph::new(player_lines).wrap(Wrap { trim: false });

    frame.render_widget(player_details, panel_areas[0]);

    let action_width = panel_areas[1].width as usize;

    let action_lines = vec![
        Line::from(""),
        action_row("[Enter] Edit", "[a] Add", action_width, dim_style),
        action_row("[dd] Cut", "[p/P] Paste", action_width, dim_style),
    ];

    frame.render_widget(Paragraph::new(action_lines), panel_areas[1]);
}

fn editor_field(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<10}"),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(value.to_string()),
    ])
}

fn render_player_form(frame: &mut Frame, form: &PlayerForm, area: Rect) {
    let title = match &form.mode {
        PlayerFormMode::Add => " Add Player ",

        PlayerFormMode::Edit(_) => " Edit Player ",
    };

    let mut lines = vec![
        form_field_line(
            "Name",
            &form.name,
            form.active_field == PlayerFormField::Name,
        ),
        Line::from(""),
        form_field_line(
            "Short name",
            &form.short_name,
            form.active_field == PlayerFormField::ShortName,
        ),
        Line::from(""),
        form_field_line(
            "Position",
            &form.position,
            form.active_field == PlayerFormField::Position,
        ),
        Line::from(""),
        form_field_line(
            "Value",
            &form.projected_value,
            form.active_field == PlayerFormField::ProjectedValue,
        ),
        Line::from(""),
    ];

    match &form.error {
        Some(error) => {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        }

        None => {
            lines.push(Line::from(Span::styled(
                "[Tab] Next field",
                Style::default().add_modifier(Modifier::DIM),
            )));

            lines.push(Line::from(Span::styled(
                "[Enter] Confirm",
                Style::default().add_modifier(Modifier::DIM),
            )));

            lines.push(Line::from(Span::styled(
                "[Esc] Cancel",
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
    }

    let form_panel = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .wrap(Wrap { trim: false });

    frame.render_widget(form_panel, area);
}

fn form_field_line(label: &str, value: &str, active: bool) -> Line<'static> {
    let marker = if active { ">" } else { " " };

    let cursor = if active { "_" } else { "" };

    let text = format!("{marker} {label:<11} {value}{cursor}");

    let style = if active {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };

    Line::from(Span::styled(text, style))
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let dim_style = Style::default().add_modifier(Modifier::DIM);

    let footer_line = match &app.draft_mode {
        DraftMode::SearchingPlayer => Line::from(format!("/{}_", app.search_query,)),

        DraftMode::BrowsingPlayers => browsing_footer(app, dim_style),

        DraftMode::RecordingDraft => recording_footer(app, dim_style),
    };

    let footer = Paragraph::new(footer_line).alignment(Alignment::Center);

    frame.render_widget(footer, area);
}

fn browsing_footer(app: &App, dim_style: Style) -> Line<'static> {
    match (&app.session_phase, &app.board_mode) {
        (SessionPhase::Preparation, BoardMode::Browse) => Line::from(Span::styled(
            "PREPARATION · [j/k] Move | \
             [/] Search | [Enter] Draft | \
             [E] Edit | [q] Quit",
            dim_style,
        )),

        (SessionPhase::Preparation, BoardMode::Edit) => {
            let unsaved = if app.data_dirty { " · UNSAVED" } else { "" };

            let pending = if matches!(app.pending_edit_command, PendingEditCommand::Delete) {
                " · d…"
            } else {
                ""
            };

            let register = app
                .player_register
                .as_ref()
                .map(|register| format!(" · CUT: {}", register.player.display_name(),))
                .unwrap_or_default();

            Line::from(Span::styled(
                format!(
                    "EDIT{unsaved}{pending}{register} · \
                     [j/k] Move | [/] Search | \
                     [s] Save | [E/Esc] Leave"
                ),
                dim_style,
            ))
        }

        (SessionPhase::LiveDraft, _) => Line::from(Span::styled(
            "LIVE DRAFT · [j/k] Move | \
             [/] Search | [Enter] Draft | \
             [u] Undo | [q] Quit",
            dim_style,
        )),
    }
}

fn recording_footer(app: &App, dim_style: Style) -> Line<'static> {
    let (Some(player_index), Some(team_index)) = (app.selected_player, app.selected_team) else {
        return Line::from("Unable to record draft");
    };

    let Some(player) = app.players.get(player_index) else {
        return Line::from("Unable to record draft");
    };

    let Some(team) = app.teams.get(team_index) else {
        return Line::from("Unable to record draft");
    };

    Line::from(vec![
        Span::styled(
            "[j/k] Team | [Enter] Confirm | \
             [Esc] Cancel  ||  ",
            dim_style,
        ),
        Span::raw(format!(
            "Draft: {} -> {} | Price: ${}_",
            player.name, team.name, app.draft_price_input,
        )),
    ])
}
