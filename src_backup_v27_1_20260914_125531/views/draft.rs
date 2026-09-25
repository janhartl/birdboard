use crate::app::{
    App, InteractionMode, PendingEditCommand, PlayerForm, PlayerFormField, PlayerFormMode,
    ProjectionField, ProjectionForm, SessionPhase,
};
use crate::draft::DraftMode;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

pub fn draw(frame: &mut Frame, app: &App) {
    match app.session_phase {
        SessionPhase::Preparation => {
            // Preparation is intentionally just the DURANT board + one large
            // player/projection workspace.  The right panel spans the full
            // terminal height; the board footer lives only below the left side.
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(73), Constraint::Percentage(27)])
                .split(frame.area());

            let left_areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(columns[0]);

            render_player_board(frame, app, left_areas[0]);
            render_right_detail_panel(frame, app, columns[1]);
            render_footer(frame, app, left_areas[1]);
        }
        SessionPhase::LiveDraft => {
            let areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(frame.area());

            let content_areas = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(73), Constraint::Percentage(27)])
                .split(areas[0]);

            let team_height = (app.teams.len() as u16 + 1).min(content_areas[1].height / 2);
            let right_areas = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(team_height), Constraint::Min(0)])
                .split(content_areas[1]);

            render_player_board(frame, app, content_areas[0]);
            render_team_list(frame, app, right_areas[0]);
            render_right_detail_panel(frame, app, right_areas[1]);
            render_footer(frame, app, areas[1]);
        }
    }
}

fn render_player_board(frame: &mut Frame, app: &App, area: Rect) {
    let board_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let header = match app.session_phase {
        SessionPhase::Preparation => format!(
            " {:>3}  {:<31} {:<7} {:>8} {:>8}",
            "#", "PLAYER", "POS", "DURANT", "MARKET"
        ),
        SessionPhase::LiveDraft => format!(
            " {:>3}  {:<23} {:<7} {:>8} {:>9} {:>9} {:>6} {:>6}",
            "#", "PLAYER", "POS", "MARKET", "NOW ΔH", "FINAL ΔH", "BUILD", "MOVE"
        ),
    };

    frame.render_widget(
        Paragraph::new(header).style(Style::default().add_modifier(Modifier::DIM)),
        board_areas[0],
    );

    let player_items = app
        .players
        .iter()
        .enumerate()
        .map(|(index, player)| {
            let drafted = app.draft_pick_for_player(&player.id).is_some();
            let marker = if drafted { "D" } else { " " };

            let text = match app.session_phase {
                SessionPhase::Preparation => {
                    let market = app
                        .market_value_for(&player.id)
                        .map(|value| format!("${}", value.market_price))
                        .unwrap_or_else(|| "—".to_string());
                    let durant = app
                        .durant
                        .score_for(&player.id)
                        .map(|score| format!("{:.2}", score.total))
                        .unwrap_or_else(|| "—".to_string());

                    format!(
                        "{}{:>3}.  {:<30} {:<7} {:>8} {:>8}",
                        marker,
                        index + 1,
                        player.display_name(),
                        player.position,
                        durant,
                        market,
                    )
                }
                SessionPhase::LiveDraft => {
                    let market = app
                        .live_advantage_for(&player.id)
                        .map(|score| format!("${}", score.market_price))
                        .or_else(|| {
                            app.market_value_for(&player.id)
                                .map(|value| format!("${}", value.market_price))
                        })
                        .unwrap_or_else(|| "—".to_string());
                    let live_rank = app
                        .live_board_rank_for(&player.id)
                        .map(|rank| rank.to_string())
                        .unwrap_or_else(|| "—".to_string());
                    let now_delta = app
                        .live_advantage_for(&player.id)
                        .map(|score| {
                            format!(
                                "{:+.2}pp",
                                score.marginal_immediate_matchup_win_probability * 100.0
                            )
                        })
                        .unwrap_or_else(|| "—".to_string());
                    let final_delta = app
                        .live_team_building_delta_for(&player.id)
                        .map(|delta| format!("{:+.2}pp", delta * 100.0))
                        .unwrap_or_else(|| "—".to_string());
                    let buildability =
                        movement_label(app.live_buildability_movement_for(&player.id));
                    let movement = movement_label(app.live_rank_movement_for(&player.id));

                    format!(
                        "{}{:>3}.  {:<22} {:<7} {:>8} {:>9} {:>9} {:>6} {:>6}",
                        marker,
                        live_rank,
                        player.display_name(),
                        player.position,
                        market,
                        now_delta,
                        final_delta,
                        buildability,
                        movement,
                    )
                }
            };

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

    let visible_rows = board_areas[1].height as usize;
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

    frame.render_stateful_widget(player_list, board_areas[1], &mut list_state);
}

fn movement_label(movement: Option<i16>) -> String {
    match movement {
        Some(value) if value > 0 => format!("↑{value}"),
        Some(value) if value < 0 => format!("↓{}", value.unsigned_abs()),
        Some(_) => "·".to_string(),
        None => "—".to_string(),
    }
}

fn render_team_list(frame: &mut Frame, app: &App, area: Rect) {
    let team_items = app
        .teams
        .iter()
        .enumerate()
        .map(|(index, team)| {
            let selected = matches!(&app.draft_mode, DraftMode::RecordingDraft)
                && app.selected_team == Some(index);
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
                Style::default().add_modifier(Modifier::BOLD)
            } else if team.id == app.user_team_id {
                Style::default()
            } else {
                Style::default().add_modifier(Modifier::DIM)
            };

            ListItem::new(text).style(style)
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        List::new(team_items).block(Block::default().borders(Borders::LEFT).title(" Teams ")),
        area,
    );
}

fn render_right_detail_panel(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(form) = &app.projection_form {
        render_projection_form(frame, form, area);
        return;
    }

    if matches!(&app.interaction_mode, InteractionMode::Edit) {
        match &app.player_form {
            Some(form) => render_player_form(frame, form, area),
            None => render_editor_panel(frame, app, area),
        }
        return;
    }

    if matches!(&app.draft_mode, DraftMode::RecordingDraft) {
        render_nomination_panel(frame, app, area);
        return;
    }

    render_player_detail(frame, app, area);
}

fn render_player_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(player) = app.selected_player_ref() else {
        frame.render_widget(Paragraph::new("No player selected"), area);
        return;
    };

    let block = Block::default().borders(Borders::ALL).title(format!(
        " {} · {} ",
        player.display_name(),
        player.position
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if matches!(app.session_phase, SessionPhase::LiveDraft) {
        let static_rank = app
            .durant_rank_for(&player.id)
            .map(|rank| format!("#{rank}"))
            .unwrap_or_else(|| "—".to_string());
        let market = app
            .live_advantage_for(&player.id)
            .map(|score| format!("${}", score.market_price))
            .or_else(|| {
                app.market_value_for(&player.id)
                    .map(|value| format!("${}", value.market_price))
            })
            .unwrap_or_else(|| "—".to_string());

        let (current_h, immediate_h, immediate_gain, final_h, final_edge) = app
            .live_advantage_for(&player.id)
            .map(|score| {
                (
                    format!("{:.2}%", score.current_matchup_win_probability * 100.0),
                    format!("{:.2}%", score.immediate_matchup_win_probability * 100.0),
                    format!(
                        "{:+.2} pp",
                        score.marginal_immediate_matchup_win_probability * 100.0
                    ),
                    format!(
                        "{:.2}%",
                        score.buy_projected_matchup_win_probability * 100.0
                    ),
                    format!(
                        "{:+.2} pp",
                        score.marginal_projected_matchup_win_probability * 100.0
                    ),
                )
            })
            .unwrap_or_else(|| {
                (
                    "—".to_string(),
                    "—".to_string(),
                    "—".to_string(),
                    "—".to_string(),
                    "—".to_string(),
                )
            });

        let immediate_rank = app
            .live_immediate_rank_for(&player.id)
            .map(|rank| format!("#{rank}"))
            .unwrap_or_else(|| "—".to_string());
        let projected_rank = app
            .live_projected_rank_for(&player.id)
            .map(|rank| format!("#{rank}"))
            .unwrap_or_else(|| "—".to_string());
        let buildability = movement_label(app.live_buildability_movement_for(&player.id));

        let mut lines = vec![
            heading("TEAM BUILDING"),
            Line::from(""),
            kv("Current H", &current_h),
            kv(
                "Immediate",
                &format!("{immediate_h}   {immediate_gain}   rank {immediate_rank}"),
            ),
            kv(
                "Best finish",
                &format!("{final_h}   {final_edge}   rank {projected_rank}"),
            ),
            kv("Buildability", &buildability),
            kv(
                "After last pick",
                &movement_label(app.live_rank_movement_for(&player.id)),
            ),
            kv("Market", &market),
            Line::from(""),
            heading("BASELINE"),
            kv("DURANT rank", &static_rank),
            kv(
                "DURANT",
                &app.durant
                    .score_for(&player.id)
                    .map(|score| format!("{:.3}", score.total))
                    .unwrap_or_else(|| "—".to_string()),
            ),
        ];

        if let Some(score) = app.live_advantage_for(&player.id) {
            lines.push(Line::from(""));
            lines.push(kv("Build", &score.build_name));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "[Enter] Exact max bid",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            "[s] Strategy / why this player",
            Style::default().add_modifier(Modifier::DIM),
        )));

        if let Some(ms) = app.live_board_ms {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!(
                    "{} players · {} strategies · {} ms",
                    app.live_board_evaluated_count(),
                    app.runtime_strategy_count(),
                    ms
                ),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
        if let Some(error) = &app.live_board_error {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
        return;
    }

    let mut lines = Vec::new();
    let rank = app
        .durant_rank_for(&player.id)
        .map(|rank| format!("#{rank}"))
        .unwrap_or_else(|| "—".to_string());
    let score = app.durant.score_for(&player.id);
    let market = app.market_value_for(&player.id);

    lines.push(kv("DURANT rank", &rank));
    lines.push(kv(
        "DURANT",
        &score
            .map(|score| format!("{:.3}", score.total))
            .unwrap_or_else(|| "—".to_string()),
    ));
    lines.push(kv(
        "Market",
        &market
            .map(|value| format!("${}", value.market_price))
            .unwrap_or_else(|| "—".to_string()),
    ));

    if let Some(projection) = app.projection_for(&player.id) {
        lines.push(Line::from(""));
        lines.push(heading("PROJECTION"));
        lines.push(kv(
            "Source",
            if projection.is_manual_override {
                "manual"
            } else {
                "historical seed"
            },
        ));
        lines.push(kv("FG%", &format!("{:.1}%", projection.fg_pct() * 100.0)));
        lines.push(kv("FT%", &format!("{:.1}%", projection.ft_pct() * 100.0)));
        lines.push(kv("3PM", &format!("{:.1}", projection.threes_pg)));
        lines.push(Line::from(""));
        lines.push(kv("PTS", &format!("{:.1}", projection.points_pg)));
        lines.push(kv("REB", &format!("{:.1}", projection.rebounds_pg)));
        lines.push(kv("AST", &format!("{:.1}", projection.assists_pg)));
        lines.push(Line::from(""));
        lines.push(kv("STL", &format!("{:.1}", projection.steals_pg)));
        lines.push(kv("BLK", &format!("{:.1}", projection.blocks_pg)));
        lines.push(kv("TO", &format!("{:.1}", projection.turnovers_pg)));
    }

    if let Some(score) = score {
        lines.push(Line::from(""));
        lines.push(heading("CATEGORY G"));
        lines.push(kv("FG%", &format!("{:+.2}", score.field_goal)));
        lines.push(kv("FT%", &format!("{:+.2}", score.free_throw)));
        lines.push(kv("3PM", &format!("{:+.2}", score.threes)));
        lines.push(Line::from(""));
        lines.push(kv("PTS", &format!("{:+.2}", score.points)));
        lines.push(kv("REB", &format!("{:+.2}", score.rebounds)));
        lines.push(kv("AST", &format!("{:+.2}", score.assists)));
        lines.push(Line::from(""));
        lines.push(kv("STL", &format!("{:+.2}", score.steals)));
        lines.push(kv("BLK", &format!("{:+.2}", score.blocks)));
        lines.push(kv("TO", &format!("{:+.2}", score.turnovers)));
    }

    let detail_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        detail_areas[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "[e] Edit projection",
            Style::default().add_modifier(Modifier::DIM),
        ))),
        detail_areas[1],
    );
}

fn render_nomination_panel(frame: &mut Frame, app: &App, area: Rect) {
    let (Some(player_index), Some(team_index)) = (app.selected_player, app.selected_team) else {
        frame.render_widget(Paragraph::new("Unable to record nomination"), area);
        return;
    };
    let Some(player) = app.players.get(player_index) else {
        return;
    };
    let Some(team) = app.teams.get(team_index) else {
        return;
    };

    let market = app
        .live_advantage_for(&player.id)
        .map(|score| format!("${}", score.market_price))
        .or_else(|| {
            app.market_value_for(&player.id)
                .map(|value| format!("${}", value.market_price))
        })
        .unwrap_or_else(|| "—".to_string());
    let delta_h = app
        .live_team_building_delta_for(&player.id)
        .map(|delta| format!("{:+.2} pp", delta * 100.0))
        .unwrap_or_else(|| "—".to_string());
    let price = if app.draft_price_input.is_empty() {
        "$—".to_string()
    } else {
        format!("${}", app.draft_price_input)
    };

    let mut lines = vec![
        heading("NOMINATION"),
        Line::from(""),
        Line::from(Span::styled(
            player.display_name().to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        kv("Market", &market),
        kv("Team-build ΔH", &delta_h),
        kv("Current bid", &price),
    ];

    if let Some(advice) = &app.nomination_advice {
        let bid = app.draft_price_input.parse::<u16>().ok();
        let decision = match bid {
            Some(current) if current <= advice.fair_price => "BUY",
            Some(_) => "PASS",
            None => "—",
        };
        let exact_delta_h = (advice.market_price_projected_matchup_win_probability
            - advice.pass_projected_matchup_win_probability)
            * 100.0;

        lines.push(Line::from(""));
        lines.push(heading("DURANT AUCTION"));
        lines.push(kv("Max bid", &format!("${}", advice.fair_price)));
        lines.push(kv("Edge", &format_money_edge(advice.expected_edge)));
        let delta_label = if advice.evaluated_market_price == advice.market_price {
            "Exact ΔH"
        } else {
            "ΔH @ legal max"
        };
        lines.push(kv(delta_label, &format!("{:+.2} pp", exact_delta_h)));
        lines.push(kv("Decision", decision));
        lines.push(Line::from(""));
        lines.push(kv("Build", &advice.build_name));

        if let Some(ms) = app.nomination_advice_ms {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("{} strategies · {} ms", app.runtime_strategy_count(), ms),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
    } else if let Some(error) = &app.nomination_advice_error {
        lines.push(Line::from(""));
        lines.push(heading("DURANT AUCTION"));
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }

    lines.push(Line::from(""));
    lines.push(heading("WINNING TEAM"));
    lines.push(kv("Team", &team.name));
    lines.push(kv("Budget", &format!("${}", team.budget)));

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Draft Decision "),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn format_money_edge(edge: i16) -> String {
    if edge > 0 {
        format!("+${edge}")
    } else if edge < 0 {
        format!("-${}", edge.unsigned_abs())
    } else {
        "$0".to_string()
    }
}

fn render_projection_form(frame: &mut Frame, form: &ProjectionForm, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Edit Projection ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(inner);

    let mut lines = vec![
        Line::from(Span::styled(
            form.player_name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    for field in ProjectionField::ALL {
        let active = form.active_field == field;
        let marker = if active { ">" } else { " " };
        let style = if active {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };

        lines.push(Line::from(Span::styled(
            format!(
                "{marker} {:<4} {}{}",
                field.label(),
                form.value(field),
                if active { "_" } else { "" }
            ),
            style,
        )));

        if matches!(
            field,
            ProjectionField::Fga | ProjectionField::Fta | ProjectionField::Points
        ) {
            lines.push(Line::from(""));
        }
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), areas[0]);

    let instruction_lines = if let Some(error) = &form.error {
        vec![
            Line::from(Span::styled(
                error.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "[Esc] Cancel",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ]
    } else {
        vec![
            Line::from(Span::styled(
                "[j/k] Field   [c] Clear field",
                Style::default().add_modifier(Modifier::DIM),
            )),
            Line::from(Span::styled(
                "[e] Save + rebuild DURANT   [Esc] Cancel",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ]
    };

    frame.render_widget(Paragraph::new(instruction_lines), areas[1]);
}

fn kv(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<14}"),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(value.to_string()),
    ])
}

fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ))
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
    let Some(player_index) = app.selected_player else {
        frame.render_widget(
            Paragraph::new("No player selected")
                .block(Block::default().borders(Borders::ALL).title(" Editor ")),
            area,
        );
        return;
    };
    let Some(player) = app.players.get(player_index) else {
        return;
    };

    let block = Block::default().borders(Borders::ALL).title(format!(
        " {} · {} ",
        player.name,
        player_index + 1
    ));
    let inner_area = block.inner(area);
    frame.render_widget(block, area);

    let panel_areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(inner_area);

    let mut player_lines = vec![
        editor_field("Position", &player.position),
        editor_field("Legacy value", &format!("${}", player.projected_value)),
    ];

    if let Some(register) = &app.player_register {
        player_lines.push(Line::from(""));
        player_lines.push(Line::from(Span::styled(
            format!("Cut: {}", register.player.display_name()),
            dim_style,
        )));
    }

    frame.render_widget(
        Paragraph::new(player_lines).wrap(Wrap { trim: false }),
        panel_areas[0],
    );

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
            format!("{label:<12}"),
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
            "Legacy value",
            &form.projected_value,
            form.active_field == PlayerFormField::ProjectedValue,
        ),
        Line::from(""),
    ];

    if let Some(error) = &form.error {
        lines.push(Line::from(Span::styled(
            error.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "[Tab] Next field  [Enter] Confirm  [Esc] Cancel",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn form_field_line(label: &str, value: &str, active: bool) -> Line<'static> {
    let marker = if active { ">" } else { " " };
    let cursor = if active { "_" } else { "" };
    let style = if active {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };

    Line::from(Span::styled(
        format!("{marker} {label:<12} {value}{cursor}"),
        style,
    ))
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let dim_style = Style::default().add_modifier(Modifier::DIM);

    let footer_line = match &app.draft_mode {
        DraftMode::SearchingPlayer => Line::from(format!("/{}_", app.search_query)),
        DraftMode::BrowsingPlayers => browsing_footer(app, dim_style),
        DraftMode::RecordingDraft => recording_footer(app, dim_style),
    };

    frame.render_widget(
        Paragraph::new(footer_line).alignment(Alignment::Center),
        area,
    );
}

fn browsing_footer(app: &App, dim_style: Style) -> Line<'static> {
    match (&app.session_phase, &app.interaction_mode) {
        (SessionPhase::Preparation, InteractionMode::Browse) => Line::from(Span::styled(
            "[j/k] Move   [/] Search   [e] Edit   [h] Home",
            dim_style,
        )),
        (SessionPhase::Preparation, InteractionMode::Edit) => {
            let unsaved = if app.data_dirty { " · UNSAVED" } else { "" };
            let pending = if matches!(app.pending_edit_command, PendingEditCommand::Delete) {
                " · d…"
            } else {
                ""
            };
            Line::from(Span::styled(
                format!(
                    "EDIT{unsaved}{pending}   [j/k] Move   [/] Search   [s] Save   [E/Esc] Leave"
                ),
                dim_style,
            ))
        }
        (SessionPhase::LiveDraft, _) => Line::from(Span::styled(
            "[j/k] Move   [/] Search   [Enter] Max bid   [s] Why   [u] Undo   [h] Home",
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
            "[j/k] Team   [digits] Price   [Enter] Confirm   [Esc] Cancel   ",
            dim_style,
        ),
        Span::raw(format!(
            "{} -> {} | Price: ${}_",
            player.display_name(),
            team.name,
            app.draft_price_input,
        )),
    ])
}
