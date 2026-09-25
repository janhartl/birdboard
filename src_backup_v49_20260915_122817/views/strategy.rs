use crate::app::{App, SessionPhase};

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

    // Strategy is a draft-thinking workstation. The left rail is compact
    // context. The right side gives the plan a small headline area and spends
    // the large horizontal canvas on category profiles that are useful even
    // while the expensive opponent-priced beam search is still running.
    let content = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(23), Constraint::Percentage(77)])
        .split(areas[0]);

    let strategy = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(content[1]);

    let strategy_top = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(strategy[0]);

    render_candidate_snapshot(frame, app, content[0]);
    render_plan_summary(frame, app, strategy_top[0]);
    render_projected_roster(frame, app, strategy_top[1]);
    render_category_workbench(frame, app, strategy[1]);

    let footer = match app.session_phase {
        SessionPhase::Preparation => "[b] Big board   [r] Rosters   [h] Home",
        SessionPhase::LiveDraft => "[b] Back to board   [r] Rosters   [h] Home",
    };

    frame.render_widget(
        Paragraph::new(footer)
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::DIM)),
        areas[1],
    );
}

fn render_candidate_snapshot(frame: &mut Frame, app: &App, area: Rect) {
    let Some(player) = app.selected_player_ref() else {
        frame.render_widget(
            Paragraph::new("No player selected")
                .block(Block::default().borders(Borders::ALL).title(" Candidate ")),
            area,
        );
        return;
    };

    let market = selected_price(app, &player.id)
        .map(|price| format!("${price}"))
        .unwrap_or_else(|| "—".to_string());
    let durant = app
        .durant
        .score_for(&player.id)
        .map(|score| format!("{:.3}", score.total))
        .unwrap_or_else(|| "—".to_string());
    let prep_market = app
        .market_value_for(&player.id)
        .map(|value| value.market_price)
        .map(|price| format!("${price}"))
        .unwrap_or_else(|| "—".to_string());

    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("{} · {}", player.display_name(), player.position),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(format!("Market       {market}")),
        Line::from(format!(
            "DURANT       {}  {}",
            durant,
            app.durant_rank_for(&player.id)
                .map(|rank| format!("#{rank}"))
                .unwrap_or_else(|| "—".to_string()),
        )),
        Line::from(format!(
            "NOW rank     {}  {}",
            app.live_board_rank_for(&player.id)
                .map(|rank| format!("#{rank}"))
                .unwrap_or_else(|| "—".to_string()),
            movement_label(app.live_rank_movement_for(&player.id)),
        )),
        Line::from(format!("Prep price   {prep_market}")),
        Line::from(""),
        Line::from(Span::styled(
            "PLAYER PROFILE · DURANT G",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "CAT        G     PROFILE",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(Span::styled(
            "-------------------------",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ];

    if let Some(score) = app.durant.score_for(&player.id) {
        let values = [
            score.field_goal,
            score.free_throw,
            score.threes,
            score.points,
            score.rebounds,
            score.assists,
            score.steals,
            score.blocks,
            score.turnovers,
        ];
        for (category, value) in CATEGORY_NAMES.iter().zip(values) {
            lines.push(Line::from(format!(
                "{category:<5} {value:+7.2}   {:<8}",
                category_g_marker(value)
            )));
            lines.push(Line::from(""));
        }
    } else {
        lines.push(Line::from("No DURANT category profile available."));
    }

    lines.push(Line::from(Span::styled(
        "Each +/- is ~0.4 DURANT G; stronger contributions read more dramatically.",
        Style::default().add_modifier(Modifier::DIM),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" Candidate "))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_plan_summary(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = vec![Line::from("")];

    if let Some(player) = app.selected_player_ref() {
        if let Some(price) = selected_price(app, &player.id) {
            lines.push(Line::from(Span::styled(
                format!("IF BUY {} @ ${price}", player.display_name()),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        }
    }

    if app.strategy_plan_loading() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("{} FINDING BEST STRATEGY", app.strategy_plan_spinner()),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Your player + team profiles below are already live.",
            Style::default().add_modifier(Modifier::DIM),
        )));
        lines.push(Line::from("Rationally pricing the remaining market..."));
        lines.push(Line::from(format!(
            "{} {} strategies · beam 16x24",
            app.strategy_plan_strategy_count(),
            app.strategy_plan_stage_label(),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("elapsed {:.1}s", app.strategy_plan_elapsed_seconds()),
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else if let Some(plan) = &app.live_roster_plan {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "DURANT STRATEGY",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            plan.build_name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(plan.j_name.clone()));
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            "FINAL H   {:>5.1}%",
            plan.projected_matchup_win_probability * 100.0,
        )));
        lines.push(Line::from(format!(
            "EXP. CATS {:>5.2}",
            plan.projected_expected_categories,
        )));

        if !plan.j_alternatives.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "ALTERNATIVES",
                Style::default().add_modifier(Modifier::DIM),
            )));
            for alternative in plan.j_alternatives.iter().take(2) {
                lines.push(Line::from(format!(
                    "{:>5.1}%  {}",
                    alternative.projected_matchup_win_probability * 100.0,
                    alternative.build_name,
                )));
            }
        }

        lines.push(Line::from(""));
        let timing = app
            .live_roster_plan_ms
            .map(|ms| format!(" · {ms} ms"))
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!(
                "{} · {} j · beam 16x24 · margin {:.2} pp{}",
                app.strategy_plan_stage_label(),
                app.strategy_plan_strategy_count(),
                plan.j_margin * 100.0,
                timing,
            ),
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else if let Some(error) = app.strategy_plan_error() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            error.to_string(),
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "No projected plan loaded yet.",
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

fn render_projected_roster(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = vec![Line::from("")];

    if let Some(plan) = &app.live_roster_plan {
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
            let suffix = selected_price(app, &player.id)
                .map(|price| format!(" (${price})"))
                .unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("{:>2}. + {}{}", slot, player.display_name(), suffix),
                Style::default().add_modifier(Modifier::BOLD),
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
        lines.push(Line::from(format!(
            "Future spend ${}   Projected ${} left",
            plan.projected_future_spend, plan.projected_budget_left,
        )));
    } else if app.strategy_plan_loading() {
        for player_id in app.own_roster_ids() {
            let name = app
                .durant
                .score_for(&player_id)
                .map(|score| score.player_name.as_str())
                .unwrap_or("Owned player");
            lines.push(Line::from(format!("✓ {name}")));
        }
        if let Some(player) = app.selected_player_ref() {
            let suffix = selected_price(app, &player.id)
                .map(|price| format!(" (${price})"))
                .unwrap_or_default();
            lines.push(Line::from(Span::styled(
                format!("+ {}{}", player.display_name(), suffix),
                Style::default().add_modifier(Modifier::BOLD),
            )));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("{} completing the roster...", app.strategy_plan_spinner()),
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "Projected roster will appear here.",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Projected 13-Man Roster "),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_category_workbench(frame: &mut Frame, app: &App, area: Rect) {
    let selected = app.selected_player_ref();
    let live_score = selected.and_then(|player| app.live_advantage_for(&player.id));
    let plan = app.live_roster_plan.as_ref();

    let best_header = if app.strategy_plan_loading() {
        format!("BEST CASE {}", app.strategy_plan_spinner())
    } else {
        "BEST CASE".to_string()
    };

    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "{:^8} {:^30} {:^30} {:^30}",
        "CAT", "CURRENT TEAM", "AFTER BUY", best_header,
    )));
    lines.push(Line::from(Span::styled(
        "         ------------------------------ ------------------------------ ------------------------------",
        Style::default().add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(""));

    for index in 0..9 {
        let current = live_score.map(|score| score.current_category_win_probabilities[index]);
        let after_buy = live_score.map(|score| score.immediate_category_win_probabilities[index]);
        let best_case = plan.map(|plan| plan.projected_category_win_probabilities[index]);

        lines.push(Line::from(format!(
            "{:^8} {:^30} {:^30} {:^30}",
            CATEGORY_NAMES[index],
            matchup_profile_cell(current),
            matchup_profile_cell(after_buy),
            if best_case.is_none() && app.strategy_plan_loading() {
                format!("{} computing", app.strategy_plan_spinner())
            } else {
                matchup_profile_cell(best_case)
            },
        )));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "         ------------------------------ ------------------------------ ------------------------------",
        Style::default().add_modifier(Modifier::DIM),
    )));
    lines.push(Line::from(""));

    if let Some(score) = live_score {
        let current_h = score.current_matchup_win_probability * 100.0;
        let buy_h = score.immediate_matchup_win_probability * 100.0;
        let final_h = plan.map(|plan| plan.projected_matchup_win_probability * 100.0);
        let current_cats: f64 = score.current_category_win_probabilities.iter().sum();
        let buy_cats: f64 = score.immediate_category_win_probabilities.iter().sum();
        let final_cats = plan.map(|plan| plan.projected_expected_categories);

        lines.push(Line::from(format!(
            "{:^8} {:^30} {:^30} {:^30}",
            "H",
            format!("{current_h:.1}%"),
            format!("{buy_h:.1}%"),
            final_h
                .map(|value| format!("{value:.1}%"))
                .unwrap_or_else(|| if app.strategy_plan_loading() {
                    app.strategy_plan_spinner().to_string()
                } else {
                    "—".to_string()
                }),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            "{:^8} {:^30} {:^30} {:^30}",
            "CATS",
            format!("{current_cats:.2}"),
            format!("{buy_cats:.2}"),
            final_cats
                .map(|value| format!("{value:.2}"))
                .unwrap_or_else(|| if app.strategy_plan_loading() {
                    app.strategy_plan_spinner().to_string()
                } else {
                    "—".to_string()
                }),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Team columns are category matchup win probabilities. +/- shows strength relative to a 50% category matchup.",
        Style::default().add_modifier(Modifier::DIM),
    )));

    frame.render_widget(
        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Category Workbench "),
            )
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn matchup_profile_cell(probability: Option<f64>) -> String {
    match probability {
        Some(probability) => format!(
            "{:>5.1}%  {:<6}",
            probability * 100.0,
            probability_strength_marker(probability),
        ),
        None => "—".to_string(),
    }
}

fn category_g_marker(value: f64) -> String {
    let magnitude = ((value.abs() / 0.4).ceil()).clamp(0.0, 7.0) as usize;
    if magnitude == 0 {
        "·".to_string()
    } else if value > 0.0 {
        "+".repeat(magnitude)
    } else {
        "-".repeat(magnitude)
    }
}

fn selected_price(app: &App, player_id: &crate::player::PlayerId) -> Option<u16> {
    app.live_advantage_for(player_id)
        .map(|score| score.market_price)
        .or_else(|| {
            app.market_value_for(player_id)
                .map(|value| value.market_price)
        })
}

fn movement_label(movement: Option<i16>) -> String {
    match movement {
        Some(value) if value > 0 => format!("↑{value}"),
        Some(value) if value < 0 => format!("↓{}", value.unsigned_abs()),
        Some(_) => "·".to_string(),
        None => "—".to_string(),
    }
}

fn probability_strength_marker(probability: f64) -> String {
    let delta_pp = (probability - 0.5) * 100.0;
    let magnitude = ((delta_pp.abs() + 2.5) / 5.0).floor().clamp(0.0, 6.0) as usize;
    if magnitude == 0 {
        "·".to_string()
    } else if delta_pp > 0.0 {
        "+".repeat(magnitude)
    } else {
        "-".repeat(magnitude)
    }
}
