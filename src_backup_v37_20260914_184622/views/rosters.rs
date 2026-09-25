use crate::app::{App, InteractionMode, PendingEditCommand, SessionPhase};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

const CATEGORY_NAMES: [&str; 9] = ["FG%", "FT%", "3PM", "PTS", "REB", "AST", "STL", "BLK", "TO"];

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(23),
            Constraint::Percentage(39),
            Constraint::Percentage(38),
        ])
        .split(areas[0]);

    render_team_selector(frame, app, content[0]);
    render_roster(frame, app, content[1]);
    render_evaluation(frame, app, content[2]);
    render_footer(frame, app, areas[1]);
}

fn selected_team<'a>(app: &'a App) -> Option<&'a crate::team::FantasyTeam> {
    app.selected_roster_team
        .and_then(|index| app.teams.get(index))
}

fn render_team_selector(frame: &mut Frame, app: &App, area: Rect) {
    let items = app
        .teams
        .iter()
        .enumerate()
        .map(|(index, team)| {
            let selected = app.selected_roster_team == Some(index);
            let marker = if selected {
                ">"
            } else if team.id == app.user_team_id {
                "*"
            } else {
                " "
            };
            let roster_count = app.roster_ids_for_team(team.id).len();
            let text = format!(
                "{} {:<16} ${:>3}  {:>2}/13",
                marker, team.name, team.budget, roster_count,
            );

            let style = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if team.id == app.user_team_id {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            ListItem::new(text).style(style)
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        List::new(items).block(Block::default().borders(Borders::ALL).title(" Teams ")),
        area,
    );
}

fn render_roster(frame: &mut Frame, app: &App, area: Rect) {
    let Some(team) = selected_team(app) else {
        frame.render_widget(Paragraph::new("No team selected"), area);
        return;
    };

    let roster = app
        .draft_picks
        .iter()
        .filter(|pick| pick.team_id == team.id)
        .collect::<Vec<_>>();
    let roster_count = roster.len();
    let spent = 200u16.saturating_sub(team.budget as u16);
    let open_slots = 13usize.saturating_sub(roster_count);
    let max_bid = if open_slots == 0 {
        0
    } else {
        (team.budget as u16).saturating_sub(open_slots.saturating_sub(1) as u16)
    };

    let mut lines = vec![
        Line::from(vec![
            dim("Budget "),
            Span::raw(format!("${:<3}", team.budget)),
            Span::raw("   "),
            dim("Spent "),
            Span::raw(format!("${spent}")),
        ]),
        Line::from(vec![
            dim("Slots  "),
            Span::raw(format!("{roster_count}/13")),
            Span::raw("   "),
            dim("Max bid "),
            Span::raw(format!("${max_bid}")),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "PLAYER                       $      G",
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ];

    if roster.is_empty() {
        lines.push(Line::from(Span::styled(
            "No players drafted",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        for pick in roster {
            if let Some(player) = app.player_by_id(&pick.player_id) {
                let g = app
                    .durant
                    .score_for(&pick.player_id)
                    .map(|score| format!("{:+.2}", score.total))
                    .unwrap_or_else(|| "—".to_string());
                lines.push(Line::from(format!(
                    "{:<25} ${:>3}  {:>6}",
                    player.display_name(),
                    pick.price,
                    g
                )));
            }
        }
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(format!(" {} ", team.name)),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_evaluation(frame: &mut Frame, app: &App, area: Rect) {
    let Some(team) = selected_team(app) else {
        frame.render_widget(Paragraph::new("No team selected"), area);
        return;
    };

    let roster_ids = app.roster_ids_for_team(team.id);
    let profile = app.team_x_profile(team.id);
    let roster_durant = roster_ids
        .iter()
        .filter_map(|player_id| app.durant.score_for(player_id))
        .map(|score| score.total)
        .sum::<f64>();

    let strongest = profile
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b));
    let weakest = profile
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.total_cmp(b));

    let mut lines = vec![
        Line::from(Span::styled(
            "ROSTER EVALUATION",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("Roster DURANT  {roster_durant:+.2}")),
    ];

    if !roster_ids.is_empty() {
        let spent = 200u16.saturating_sub(team.budget as u16);
        lines.push(Line::from(format!(
            "Avg cost/player ${:.1}",
            spent as f64 / roster_ids.len() as f64
        )));
    }

    if let Some((index, value)) = strongest {
        lines.push(Line::from(format!(
            "Strongest       {} {:+.2}",
            CATEGORY_NAMES[index], value
        )));
    }
    if let Some((index, value)) = weakest {
        lines.push(Line::from(format!(
            "Weakest         {} {:+.2}",
            CATEGORY_NAMES[index], value
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "CATEGORY PROFILE",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    for (name, value) in CATEGORY_NAMES.iter().zip(profile) {
        lines.push(Line::from(format!(
            "{name:<4} {value:+7.2}  {}",
            profile_bar(value)
        )));
    }

    if team.id == app.user_team_id {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "DURANT OUTLOOK",
            Style::default().add_modifier(Modifier::BOLD),
        )));

        if let Some(plan) = &app.live_roster_plan {
            lines.push(Line::from(format!(
                "Current H       {:>5.1}%",
                plan.current_matchup_win_probability * 100.0
            )));
            lines.push(Line::from(format!(
                "Projected H     {:>5.1}%",
                plan.projected_matchup_win_probability * 100.0
            )));
            lines.push(Line::from(format!(
                "Expected cats   {:>5.2}",
                plan.projected_expected_categories
            )));
            lines.push(Line::from(format!("Build           {}", plan.build_name)));
        } else {
            lines.push(Line::from(Span::styled(
                "No runtime DURANT outlook loaded.",
                Style::default().add_modifier(Modifier::DIM),
            )));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Detailed strategy and category probabilities live on [s] Strategy.",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Evaluation "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn profile_bar(value: f64) -> String {
    let magnitude = value.abs().round().clamp(0.0, 8.0) as usize;
    if magnitude == 0 {
        "·".to_string()
    } else if value > 0.0 {
        format!("+{}", "+".repeat(magnitude))
    } else {
        format!("-{}", "-".repeat(magnitude))
    }
}

fn dim(text: &'static str) -> Span<'static> {
    Span::styled(text, Style::default().add_modifier(Modifier::DIM))
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let footer_text = if let Some(input) = &app.team_input {
        match &input.error {
            Some(error) => format!("Enter team name: {}_ · {}", input.value, error),
            None => format!("Enter team name: {}_", input.value),
        }
    } else if matches!(app.interaction_mode, InteractionMode::Edit) {
        let unsaved = if app.teams_dirty { " · UNSAVED" } else { "" };
        let pending = if matches!(app.pending_edit_command, PendingEditCommand::Delete) {
            " · d…"
        } else {
            ""
        };

        format!(
            "EDIT{unsaved}{pending} · [Tab] Select | [Enter] Rename | [a] Add | [dd] Remove | [c] Claim | [s] Save | [E/Esc] Leave"
        )
    } else {
        match app.session_phase {
            SessionPhase::Preparation => {
                "[j/k] Team   [b] Big board   [s] Strategy   [E] Edit teams   [h] Home".to_string()
            }
            SessionPhase::LiveDraft => {
                "[j/k] Team   [b] Big board   [s] Strategy   [h] Home".to_string()
            }
        }
    };

    frame.render_widget(
        Paragraph::new(footer_text)
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::DIM)),
        area,
    );
}
