use crate::app::{App, SessionPhase};
use crate::strategy::{Build, ReplacementGroup};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

const CATEGORY_NAMES: [&str; 9] = ["FG%", "FT%", "3PM", "PTS", "REB", "AST", "STL", "BLK", "TO"];

pub fn draw(frame: &mut Frame, app: &App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(frame.area());

    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(31),
            Constraint::Percentage(39),
            Constraint::Percentage(30),
        ])
        .split(areas[0]);

    render_roster_outlook(frame, app, content[0]);
    render_selected_player_case(frame, app, content[1]);
    render_plan_and_notes(frame, app, content[2]);

    let footer = match app.session_phase {
        SessionPhase::Preparation => "[j/k] Replacement   [b] Big board   [r] Rosters   [h] Home",
        SessionPhase::LiveDraft => "[j/k] Replacement   [b] Back to board   [r] Rosters   [h] Home",
    };

    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::DIM)),
        areas[1],
    );
}

fn render_roster_outlook(frame: &mut Frame, app: &App, area: Rect) {
    let roster = app.own_roster_ids();
    let team = app.team_by_id(app.user_team_id);
    let budget = team.map(|team| team.budget as u16).unwrap_or(0);
    let roster_count = roster.len();
    let open_slots = 13usize.saturating_sub(roster_count);
    let max_bid = if open_slots == 0 {
        0
    } else {
        budget.saturating_sub(open_slots.saturating_sub(1) as u16)
    };

    let selected = app.selected_player_ref();
    let assumed_price = selected.and_then(|player| {
        app.live_advantage_for(&player.id)
            .map(|score| score.market_price)
            .or_else(|| {
                app.market_value_for(&player.id)
                    .map(|value| value.market_price)
            })
    });
    let mut buy_roster = roster.clone();
    if let Some(player) = selected {
        if !buy_roster.iter().any(|player_id| player_id == &player.id) {
            buy_roster.push(player.id.clone());
        }
    }
    let profile = app.durant.roster_x_profile(&buy_roster);

    let mut lines = vec![
        heading("ROSTER OUTLOOK"),
        Line::from(""),
        Line::from(format!(
            "Current {roster_count}/13   Budget ${budget}   Max ${max_bid}"
        )),
    ];

    if let (Some(player), Some(price)) = (selected, assumed_price) {
        lines.push(Line::from(format!(
            "If buy {} @ ${}: {}/13   ${} left",
            player.display_name(),
            price,
            (roster_count + 1).min(13),
            budget.saturating_sub(price),
        )));
    }

    if app.strategy_plan_loading() {
        lines.push(Line::from(Span::styled(
            format!(
                "{} full-market plan loading · {:.1}s",
                app.strategy_plan_spinner(),
                app.strategy_plan_elapsed_seconds(),
            ),
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else if let Some(error) = app.strategy_plan_error() {
        lines.push(Line::from(Span::styled(
            format!("Plan error: {error}"),
            Style::default().add_modifier(Modifier::DIM),
        )));
    }

    if let Some(plan) = &app.live_roster_plan {
        lines.push(Line::from(format!(
            "After buy H {:>5.1}%   Projected H {:>5.1}%",
            plan.current_matchup_win_probability * 100.0,
            plan.projected_matchup_win_probability * 100.0,
        )));
        lines.push(Line::from(format!(
            "Expected cats {:>4.2}   {}",
            plan.projected_expected_categories, plan.build_name
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "CAT   X(BUY)    BUY    FINAL",
        Style::default().add_modifier(Modifier::BOLD),
    )));

    for (index, name) in CATEGORY_NAMES.iter().enumerate() {
        let now = app
            .live_roster_plan
            .as_ref()
            .map(|plan| {
                format!(
                    "{:>5.1}%",
                    plan.current_category_win_probabilities[index] * 100.0
                )
            })
            .unwrap_or_else(|| "    —".to_string());
        let final_probability = app
            .live_roster_plan
            .as_ref()
            .map(|plan| {
                format!(
                    "{:>5.1}%",
                    plan.projected_category_win_probabilities[index] * 100.0
                )
            })
            .unwrap_or_else(|| "    —".to_string());

        lines.push(Line::from(format!(
            "{name:<4} {:+7.2}  {now}  {final_probability}",
            profile[index]
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "Replacement G {:+.3} · League $ left ${}",
            app.market_board.economy.replacement_g_score,
            app.market_board.economy.remaining_league_dollars
        ),
        Style::default().add_modifier(Modifier::DIM),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Team State "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_selected_player_case(frame: &mut Frame, app: &App, area: Rect) {
    let Some(player) = app.selected_player_ref() else {
        frame.render_widget(
            Paragraph::new("No player selected").block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Why this player? "),
            ),
            area,
        );
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
    let durant = app
        .durant
        .score_for(&player.id)
        .map(|score| format!("{:.3}", score.total))
        .unwrap_or_else(|| "—".to_string());

    let mut lines = vec![
        heading(&format!("{} · {}", player.display_name(), player.position)),
        Line::from(""),
        Line::from(format!(
            "NOW rank {}   Move {}   Market {}",
            app.live_board_rank_for(&player.id)
                .map(|rank| format!("#{rank}"))
                .unwrap_or_else(|| "—".to_string()),
            movement_label(app.live_rank_movement_for(&player.id)),
            market,
        )),
        Line::from(format!(
            "Static DURANT {}   Rank {}",
            durant,
            app.durant_rank_for(&player.id)
                .map(|rank| format!("#{rank}"))
                .unwrap_or_else(|| "—".to_string()),
        )),
    ];

    let Some(score) = app.live_advantage_for(&player.id) else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "No budget-aware DURANT evaluation for this player.",
            Style::default().add_modifier(Modifier::DIM),
        )));

        frame.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Why this player? "),
                )
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    };

    let immediate_rank = app
        .live_immediate_rank_for(&player.id)
        .map(|rank| format!("#{rank}"))
        .unwrap_or_else(|| "—".to_string());
    let projected_rank = app
        .live_projected_rank_for(&player.id)
        .map(|rank| format!("#{rank}"))
        .unwrap_or_else(|| "—".to_string());
    let buildability = movement_label(app.live_buildability_movement_for(&player.id));

    lines.push(Line::from(format!(
        "CURRENT H {:>5.2}%",
        score.current_matchup_win_probability * 100.0,
    )));
    lines.push(Line::from(format!(
        "IMMEDIATE {:>5.2}%  {:+.2} pp  rank {}",
        score.immediate_matchup_win_probability * 100.0,
        score.marginal_immediate_matchup_win_probability * 100.0,
        immediate_rank,
    )));

    if !score.has_projected_finish {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Future-roster projection is intentionally limited to the top 25 NOW options.",
            Style::default().add_modifier(Modifier::DIM),
        )));
        lines.push(Line::from(Span::styled(
            "Return to Draft and press Enter on this player for the exact rational-market Max Bid model.",
            Style::default().add_modifier(Modifier::DIM),
        )));

        frame.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Why this player? "),
                )
                .wrap(Wrap { trim: false }),
            area,
        );
        return;
    }
    lines.push(Line::from(format!(
        "BEST FINISH {:>5.2}%  edge {:+.2} pp  rank {}",
        score.buy_projected_matchup_win_probability * 100.0,
        score.marginal_projected_matchup_win_probability * 100.0,
        projected_rank,
    )));
    lines.push(Line::from(format!(
        "Buildability {}   PASS finish {:>5.2}%",
        buildability,
        score.pass_projected_matchup_win_probability * 100.0,
    )));
    let prep_market = app
        .market_value_for(&player.id)
        .map(|value| value.market_price);
    lines.push(Line::from(format!(
        "Competition ${}{}   resulting build  {}",
        score.market_price,
        prep_market
            .map(|price| format!(" · prep static ${price}"))
            .unwrap_or_default(),
        score.build_name
    )));
    lines.push(Line::from(Span::styled(
        score.j_name.clone(),
        Style::default().add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "CATEGORY PATH",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "CAT   CURRENT    NOW    FINAL     j",
        Style::default().add_modifier(Modifier::DIM),
    )));

    for index in 0..9 {
        let current = score.current_category_win_probabilities[index] * 100.0;
        let immediate = score.immediate_category_win_probabilities[index] * 100.0;
        let final_probability = score.buy_projected_category_win_probabilities[index] * 100.0;
        lines.push(Line::from(format!(
            "{:<4} {:>6.1}% {:>6.1}% {:>7.1}%  {:>4.2}",
            CATEGORY_NAMES[index], current, immediate, final_probability, score.j_weights[index],
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "FAST BOARD ESTIMATE",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    if score.projected_future_player_names.is_empty() {
        lines.push(Line::from("Roster complete"));
    } else {
        for name in &score.projected_future_player_names {
            lines.push(Line::from(format!("  {name}")));
        }
    }
    lines.push(Line::from(Span::styled(
        format!(
            "Future spend ${} · projected ${} left",
            score.projected_future_spend, score.projected_budget_left
        ),
        Style::default().add_modifier(Modifier::DIM),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Why this player? "),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_plan_and_notes(frame: &mut Frame, app: &App, area: Rect) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(78), Constraint::Percentage(22)])
        .split(area);

    render_computer_plan(frame, app, vertical[0]);
    render_human_notes(frame, app, vertical[1]);
}

fn render_computer_plan(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = vec![heading("COMPUTER PLAN"), Line::from("")];

    if app.strategy_plan_loading() {
        lines.push(Line::from(Span::styled(
            format!("{} FULL MARKET LOADING", app.strategy_plan_spinner(),),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        if let Some(player) = app.selected_player_ref() {
            let price = app
                .live_advantage_for(&player.id)
                .map(|score| score.market_price)
                .or_else(|| {
                    app.market_value_for(&player.id)
                        .map(|value| value.market_price)
                });
            if let Some(price) = price {
                lines.push(Line::from(format!(
                    "IF BUY {} @ ${price}",
                    player.display_name()
                )));
            }
        }
        lines.push(Line::from("Rationally repricing every remaining player..."));
        lines.push(Line::from(format!(
            "Searching {} {} strategies",
            app.strategy_plan_strategy_count(),
            app.strategy_plan_stage_label(),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("elapsed {:.1}s", app.strategy_plan_elapsed_seconds()),
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else if let Some(plan) = &app.live_roster_plan {
        if let Some(player) = app.selected_player_ref() {
            let price = app
                .live_advantage_for(&player.id)
                .map(|score| score.market_price)
                .or_else(|| {
                    app.market_value_for(&player.id)
                        .map(|value| value.market_price)
                });
            if let Some(price) = price {
                lines.push(Line::from(Span::styled(
                    format!("IF BUY {} @ ${price}", player.display_name()),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
        }
        lines.push(Line::from(format!(
            "H {:>5.1}% · {:>4.2} cats · {}",
            plan.projected_matchup_win_probability * 100.0,
            plan.projected_expected_categories,
            plan.build_name
        )));
        lines.push(Line::from(format!(
            "j margin {:.2} pp",
            plan.j_margin * 100.0
        )));
        lines.push(Line::from(Span::styled(
            plan.j_name.clone(),
            Style::default().add_modifier(Modifier::DIM),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "CAT      j    FINAL",
            Style::default().add_modifier(Modifier::BOLD),
        )));

        for index in 0..9 {
            lines.push(Line::from(format!(
                "{:<4} {:>5.2}  {:>5.1}%",
                CATEGORY_NAMES[index],
                plan.j_weights[index],
                plan.projected_category_win_probabilities[index] * 100.0
            )));
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "PROJECTED 13-MAN ROSTER",
            Style::default().add_modifier(Modifier::BOLD),
        )));

        let mut slot = 1usize;
        for player_id in app.own_roster_ids() {
            let name = app
                .durant
                .score_for(&player_id)
                .map(|score| score.player_name.as_str())
                .unwrap_or("Owned player");
            lines.push(Line::from(format!("{:>2}. ✓ {name}", slot)));
            slot += 1;
        }

        if let Some(player) = app.selected_player_ref() {
            let price = app
                .live_advantage_for(&player.id)
                .map(|score| score.market_price)
                .or_else(|| {
                    app.market_value_for(&player.id)
                        .map(|value| value.market_price)
                });
            let suffix = price
                .map(|price| format!(" (${price})"))
                .unwrap_or_default();
            lines.push(Line::from(format!(
                "{:>2}. + {}{}",
                slot,
                player.display_name(),
                suffix
            )));
            slot += 1;
        }

        for name in &plan.projected_future_player_names {
            if slot > 13 {
                break;
            }
            lines.push(Line::from(format!("{:>2}.   {name}", slot)));
            slot += 1;
        }

        lines.push(Line::from(""));
        let timing = app
            .live_roster_plan_ms
            .map(|ms| format!(" · plan {ms} ms"))
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!(
                "{} {} strategies · future ${} · left ${}{}",
                app.strategy_plan_stage_label(),
                app.strategy_plan_strategy_count(),
                plan.projected_future_spend,
                plan.projected_budget_left,
                timing
            ),
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else if let Some(error) = app.strategy_plan_error() {
        lines.push(Line::from(Span::styled(
            error.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "No full-market plan loaded yet.",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" DURANT Plan "),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_human_notes(frame: &mut Frame, app: &App, area: Rect) {
    let active_build = app.current_build();
    let replacements = app.triggered_replacements();
    let replacement_count = replacements.len();
    let selected_index = if replacement_count == 0 {
        0
    } else {
        app.selected_replacement % replacement_count
    };
    let selected_replacement = replacements.get(selected_index).copied();

    let mut lines = vec![heading("HUMAN NOTES"), Line::from("")];

    match active_build {
        Some(build) => append_build_summary(&mut lines, build, app),
        None => lines.push(Line::from(Span::styled(
            "No manual anchor build active.",
            Style::default().add_modifier(Modifier::DIM),
        ))),
    }

    lines.push(Line::from(""));

    match selected_replacement {
        Some(group) => append_replacement_summary(&mut lines, group, app),
        None => lines.push(Line::from(Span::styled(
            "No replacement matrix triggered.",
            Style::default().add_modifier(Modifier::DIM),
        ))),
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(if replacement_count > 0 {
                        format!(
                            " Human · replacement {}/{} ",
                            selected_index + 1,
                            replacement_count
                        )
                    } else {
                        " Human ".to_string()
                    }),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn append_build_summary(lines: &mut Vec<Line<'static>>, build: &Build, app: &App) {
    lines.push(Line::from(Span::styled(
        build.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));

    let available_targets = build
        .target_players
        .iter()
        .filter(|player_id| app.draft_pick_for_player(player_id).is_none())
        .filter_map(|player_id| app.player_by_id(player_id))
        .take(5)
        .collect::<Vec<_>>();

    if !available_targets.is_empty() {
        lines.push(Line::from("Targets"));
        for player in available_targets {
            let market = app
                .live_advantage_for(&player.id)
                .map(|score| format!("${}", score.market_price))
                .or_else(|| {
                    app.market_value_for(&player.id)
                        .map(|value| format!("${}", value.market_price))
                })
                .unwrap_or_else(|| "—".to_string());
            lines.push(Line::from(format!(
                "  {:<18} {:>4}",
                player.display_name(),
                market
            )));
        }
    }
}

fn append_replacement_summary(lines: &mut Vec<Line<'static>>, group: &ReplacementGroup, app: &App) {
    lines.push(Line::from(Span::styled(
        group.title.clone(),
        Style::default().add_modifier(Modifier::BOLD),
    )));

    for option in group.alternatives.iter().take(4) {
        let Some(player) = app.player_by_id(&option.player_id) else {
            continue;
        };
        let status = match app.draft_pick_for_player(&option.player_id) {
            None => "available",
            Some(pick) if pick.team_id == app.user_team_id => "owned",
            Some(_) => "gone",
        };
        lines.push(Line::from(format!(
            "  {:<18} {}",
            player.display_name(),
            status
        )));
    }
}

fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().add_modifier(Modifier::BOLD),
    ))
}

fn movement_label(movement: Option<i16>) -> String {
    match movement {
        Some(value) if value > 0 => format!("↑{value}"),
        Some(value) if value < 0 => format!("↓{}", value.unsigned_abs()),
        Some(_) => "·".to_string(),
        None => "—".to_string(),
    }
}
