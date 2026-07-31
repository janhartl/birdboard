use crate::app::App;
use crate::strategy::{Build, active_build};

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

    let active_build = active_build(&app.builds, |player_id| {
        app.team_has_player(app.user_team_id, player_id)
    });

    let content = match active_build {
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

    let strategy_panel = Paragraph::new(content)
        .block(Block::default().borders(Borders::ALL).title(" Strategy "))
        .wrap(Wrap { trim: false });

    frame.render_widget(strategy_panel, areas[0]);

    let footer = Paragraph::new("[b] Big board | [h] Home | [r] Rosters | [q] Quit")
        .alignment(Alignment::Center)
        .style(Style::default().add_modifier(Modifier::DIM));

    frame.render_widget(footer, areas[1]);
}

fn build_text(build: &Build, app: &App) -> Text<'static> {
    let heading_style = Style::default().add_modifier(Modifier::BOLD);

    let mut lines = vec![
        Line::from(Span::styled(build.title.clone(), heading_style)),
        Line::from(""),
        Line::from(Span::styled("Identity", heading_style)),
        Line::from(build.identity.clone()),
    ];

    for section in &build.sections {
        lines.push(Line::from(""));

        lines.push(Line::from(Span::styled(
            section.title.clone(),
            heading_style,
        )));

        for item in &section.items {
            lines.push(Line::from(format!("  • {item}")));
        }
    }

    if !build.target_players.is_empty() {
        lines.push(Line::from(""));

        lines.push(Line::from(Span::styled("Targets", heading_style)));

        for player_id in &build.target_players {
            let Some(player) = app.player_by_id(*player_id) else {
                continue;
            };

            let (status, style) = match app.draft_pick_for_player(*player_id) {
                None => (String::from("AVAILABLE"), Style::default()),

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
