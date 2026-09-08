use crate::app::App;
use crate::strategy::{Build, ReplacementGroup};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    let strategy_areas = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(areas[0]);

    let active_build = app.current_build();

    let replacements = app.triggered_replacements();
    let replacement_count = replacements.len();

    let selected_replacement_index = if replacement_count == 0 {
        0
    } else {
        app.selected_replacement % replacement_count
    };

    let replacement = replacements.get(selected_replacement_index).copied();

    let build_content = match active_build {
        Some(build) => build_text(build, app),

        None => Text::from(vec![
            Line::from(Span::styled(
                "No anchor build active",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Draft a cornerstone player to activate a build plan.",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ]),
    };

    let build_panel = Paragraph::new(build_content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Active Build "),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(build_panel, strategy_areas[0]);

    let replacement_content = match replacement {
        Some(group) => replacement_text(group, app),

        None => Text::from(vec![
            Line::from(Span::styled(
                "No replacement needed",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "No active build target with a replacement matrix has been drafted by another team.",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ]),
    };
    let replacement_title = if replacement_count == 0 {
        String::from(" Replacement Matrix ")
    } else {
        format!(
            " Replacement Matrix · {}/{} ",
            selected_replacement_index + 1,
            replacement_count,
        )
    };

    let replacement_panel = Paragraph::new(replacement_content)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(replacement_title),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(replacement_panel, strategy_areas[1]);

    let footer =
        Paragraph::new("[j/k] Replacement | [b] Big board | [h] Home | [r] Rosters | [q] Quit")
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::DIM));

    frame.render_widget(footer, areas[1]);
}

fn build_text(build: &Build, app: &App) -> Text<'static> {
    let heading_style = Style::default().add_modifier(Modifier::BOLD);

    let mut lines = vec![
        Line::from(Span::styled(build.title.clone(), heading_style)),
        Line::from(""),
        Line::from(build.text.clone()),
    ];

    if !build.target_players.is_empty() {
        lines.push(Line::from(""));

        lines.push(Line::from(Span::styled("Targets", heading_style)));

        for player_id in &build.target_players {
            let Some(player) = app.player_by_id(player_id) else {
                continue;
            };

            let (status, style) = match app.draft_pick_for_player(player_id) {
                None => (String::from("AVAILABLE"), Style::default()),

                Some(pick) if pick.team_id == app.user_team_id => (
                    String::from("OWNED"),
                    Style::default().add_modifier(Modifier::BOLD),
                ),

                Some(pick) => {
                    let team_name = app
                        .team_by_id(pick.team_id)
                        .map(|team| team.name.as_str())
                        .unwrap_or("Unknown team");

                    (
                        format!("DRAFTED → {team_name}"),
                        Style::default().add_modifier(Modifier::DIM),
                    )
                }
            };

            let row = format!(
                "  {:<20}  ${:<3}  {}",
                player.display_name(),
                player.projected_value,
                status,
            );

            lines.push(Line::from(Span::styled(row, style)));
        }
    }

    Text::from(lines)
}

fn replacement_text(group: &ReplacementGroup, app: &App) -> Text<'static> {
    let heading_style = Style::default().add_modifier(Modifier::BOLD);

    let mut lines = vec![
        Line::from(Span::styled(group.title.clone(), heading_style)),
        Line::from(""),
    ];

    for option in &group.alternatives {
        let Some(player) = app.player_by_id(&option.player_id) else {
            continue;
        };

        let (status, style) = match app.draft_pick_for_player(&option.player_id) {
            None => (String::from("AVAILABLE"), Style::default()),

            Some(pick) if pick.team_id == app.user_team_id => (
                String::from("OWNED"),
                Style::default().add_modifier(Modifier::BOLD),
            ),

            Some(pick) => {
                let team_name = app
                    .team_by_id(pick.team_id)
                    .map(|team| team.name.as_str())
                    .unwrap_or("Unknown team");

                (
                    format!("DRAFTED → {team_name}"),
                    Style::default().add_modifier(Modifier::DIM),
                )
            }
        };

        lines.push(Line::from(Span::styled(
            format!(
                "{}  ${}  {}",
                player.display_name(),
                player.projected_value,
                status,
            ),
            style,
        )));

        lines.push(Line::from(format!(
            "  {}: {}",
            group.left_label, option.left,
        )));

        lines.push(Line::from(format!(
            "  {}: {}",
            group.right_label, option.right,
        )));

        lines.push(Line::from(""));
    }

    if let Some(rule) = &group.rule {
        lines.push(Line::from(Span::styled("Rule", heading_style)));

        lines.push(Line::from(rule.clone()));
    }

    Text::from(lines)
}
