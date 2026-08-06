use crate::app::{
    App, BuildEditTarget, InteractionMode, ReplacementEditTarget, SessionPhase, StrategyPane,
    StrategyTextTarget,
};
use crate::strategy::{Build, ReplacementGroup};

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
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

    let build_focused = matches!(app.strategy_pane, StrategyPane::Build);

    let replacement_focused = matches!(app.strategy_pane, StrategyPane::Replacement);

    render_build_pane(frame, app, strategy_areas[0], build_focused);

    render_replacement_pane(frame, app, strategy_areas[1], replacement_focused);

    render_footer(frame, app, areas[1]);
}

fn render_build_pane(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    let (build, build_title) = selected_build(app);

    let build_content = match build {
        Some(build) => build_text(build, app),

        None if matches!(app.session_phase, SessionPhase::Preparation) => Text::from(vec![
            Line::from(Span::styled(
                "No builds defined",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Add a build to begin planning.",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ]),

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

    let title_selected = matches!(app.interaction_mode, InteractionMode::Edit)
        && focused
        && matches!(
            app.selected_build_edit_target(),
            Some(BuildEditTarget::Title)
        );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(build_border_style(focused))
        .title(build_pane_title(build_title, focused, title_selected));

    render_pane(frame, area, block, build_content);
}

fn render_replacement_pane(frame: &mut Frame, app: &App, area: Rect, focused: bool) {
    let (replacement, replacement_title) = selected_replacement_group(app);

    let replacement_content = match replacement {
        Some(group) => replacement_text(group, app),

        None if matches!(app.session_phase, SessionPhase::Preparation) => Text::from(vec![
            Line::from(Span::styled(
                "No replacement matrices defined",
                Style::default().add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "Add a matrix for a target player.",
                Style::default().add_modifier(Modifier::DIM),
            )),
        ]),

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

    let block = Block::default()
        .borders(Borders::ALL)
        .title(pane_title(replacement_title, focused));

    render_pane(frame, area, block, replacement_content);
}

fn render_pane(frame: &mut Frame, area: Rect, block: Block<'static>, content: Text<'static>) {
    let inner_area = block.inner(area);

    frame.render_widget(block, area);

    let content_panel = Paragraph::new(content).wrap(Wrap { trim: false });

    frame.render_widget(content_panel, inner_area);
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let footer_text = match (&app.session_phase, &app.interaction_mode) {
        (SessionPhase::Preparation, InteractionMode::Browse) => String::from(
            "[Tab] Pane | [j/k] Browse | \
             [E] Edit | [b] Big board | \
             [r] Rosters | [q] Quit",
        ),

        (SessionPhase::Preparation, InteractionMode::Edit) => {
            if app.strategy_text_input.is_some() {
                String::from("[Enter] Confirm | [Esc] Cancel")
            } else {
                let unsaved = if app.strategy_dirty {
                    "UNSAVED · "
                } else {
                    ""
                };

                format!(
                    "{unsaved}[Tab] Pane | \
                     [j/k] Select | [Enter] Edit | \
                     [s] Save | [E/Esc] Leave"
                )
            }
        }

        (SessionPhase::LiveDraft, _) => match app.strategy_pane {
            StrategyPane::Build => String::from(
                "[Tab] Pane | Active build | \
                 [b] Big board | [r] Rosters | \
                 [q] Quit",
            ),

            StrategyPane::Replacement => String::from(
                "[Tab] Pane | [j/k] Matrix | \
                 [b] Big board | [r] Rosters | \
                 [q] Quit",
            ),
        },
    };

    let footer = Paragraph::new(footer_text)
        .alignment(Alignment::Center)
        .style(Style::default().add_modifier(Modifier::DIM));

    frame.render_widget(footer, area);
}

fn selected_build(app: &App) -> (Option<&Build>, String) {
    match app.session_phase {
        SessionPhase::Preparation => {
            let count = app.builds.len();

            if count == 0 {
                return (None, String::from("No builds"));
            }

            let index = app.selected_build % count;

            let build = &app.builds[index];

            let title = preview_text(
                app,
                StrategyTextTarget::Build {
                    build_index: index,
                    target: BuildEditTarget::Title,
                },
                &build.title,
            );

            (Some(build), format!("{} · {}/{}", title, index + 1, count,))
        }

        SessionPhase::LiveDraft => {
            let build = app.current_build();

            let title = match build {
                Some(build) => build.title.clone(),

                None => String::from("No active build"),
            };

            (build, title)
        }
    }
}

fn selected_replacement_group(app: &App) -> (Option<&ReplacementGroup>, String) {
    match app.session_phase {
        SessionPhase::Preparation => {
            let count = app.replacements.len();

            if count == 0 {
                return (None, String::from(" Replacement Matrix "));
            }

            let index = app.selected_replacement_group % count;

            (
                app.replacements.get(index),
                format!(" Replacement Matrix · {}/{} ", index + 1, count,),
            )
        }

        SessionPhase::LiveDraft => {
            let replacements = app.triggered_replacements();

            let count = replacements.len();

            if count == 0 {
                return (None, String::from(" Replacement Matrix "));
            }

            let index = app.selected_replacement % count;

            (
                replacements.get(index).copied(),
                format!(" Replacement Matrix · {}/{} ", index + 1, count,),
            )
        }
    }
}

fn build_pane_title(title: String, focused: bool, selected: bool) -> Line<'static> {
    let style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let title = if selected {
        format!(" > {} ", title.trim())
    } else {
        format!(" {} ", title.trim())
    };

    Line::from(Span::styled(title, style))
}

fn build_border_style(focused: bool) -> Style {
    if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

fn pane_title(title: String, focused: bool) -> Line<'static> {
    let style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    Line::from(Span::styled(title, style))
}

fn build_text(build: &Build, app: &App) -> Text<'static> {
    let heading_style = Style::default().add_modifier(Modifier::BOLD);

    let build_index = app.selected_build;

    let selected_target = if matches!(app.interaction_mode, InteractionMode::Edit)
        && matches!(app.strategy_pane, StrategyPane::Build)
    {
        app.selected_build_edit_target()
    } else {
        None
    };

    let identity_selected = matches!(selected_target, Some(BuildEditTarget::Identity));

    let identity = preview_text(
        app,
        StrategyTextTarget::Build {
            build_index,
            target: BuildEditTarget::Identity,
        },
        &build.identity,
    );

    let mut lines = vec![
        Line::from(Span::styled("Identity", heading_style)),
        editable_line(identity, identity_selected, false, Style::default()),
    ];

    for (section_index, section) in build.sections.iter().enumerate() {
        lines.push(Line::from(""));

        let section_selected = matches!(
            selected_target,
            Some(
                BuildEditTarget::SectionTitle(
                    selected_section,
                )
            ) if selected_section == section_index
        );

        let section_title = preview_text(
            app,
            StrategyTextTarget::Build {
                build_index,
                target: BuildEditTarget::SectionTitle(section_index),
            },
            &section.title,
        );

        lines.push(editable_line(
            section_title,
            section_selected,
            false,
            heading_style,
        ));

        for (item_index, item) in section.items.iter().enumerate() {
            let item_selected = matches!(
                selected_target,
                Some(
                    BuildEditTarget::SectionItem {
                        section_index:
                            selected_section,
                        item_index:
                            selected_item,
                    }
                ) if selected_section == section_index
                    && selected_item == item_index
            );

            let item_text = preview_text(
                app,
                StrategyTextTarget::Build {
                    build_index,
                    target: BuildEditTarget::SectionItem {
                        section_index,
                        item_index,
                    },
                },
                item,
            );

            lines.push(editable_line(
                format!("• {item_text}"),
                item_selected,
                true,
                Style::default(),
            ));
        }
    }

    if !build.target_players.is_empty() {
        lines.push(Line::from(""));

        lines.push(Line::from(Span::styled("Targets", heading_style)));

        for player_id in &build.target_players {
            let Some(player) = app.player_by_id(*player_id) else {
                continue;
            };

            let (status, style) = player_status(app, *player_id);

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

    let group_index = app.selected_replacement_group;

    let selected_target = if matches!(app.interaction_mode, InteractionMode::Edit)
        && matches!(app.strategy_pane, StrategyPane::Replacement)
    {
        app.selected_replacement_edit_target()
    } else {
        None
    };

    let title_selected = matches!(selected_target, Some(ReplacementEditTarget::Title));

    let group_title = preview_text(
        app,
        StrategyTextTarget::Replacement {
            group_index,
            target: ReplacementEditTarget::Title,
        },
        &group.title,
    );

    let mut lines = vec![
        editable_line(group_title, title_selected, false, heading_style),
        Line::from(""),
    ];

    for (option_index, option) in group.alternatives.iter().enumerate() {
        let Some(player) = app.player_by_id(option.player_id) else {
            continue;
        };

        let (status, style) = player_status(app, option.player_id);

        lines.push(Line::from(Span::styled(
            format!(
                "{}  ${}  {}",
                player.display_name(),
                player.projected_value,
                status,
            ),
            style,
        )));

        let left_selected = matches!(
            selected_target,
            Some(
                ReplacementEditTarget::
                    AlternativeLeft(
                        selected_option,
                    )
            ) if selected_option == option_index
        );

        let left_text = preview_text(
            app,
            StrategyTextTarget::Replacement {
                group_index,
                target: ReplacementEditTarget::AlternativeLeft(option_index),
            },
            &option.left,
        );

        lines.push(editable_line(
            format!("{}: {}", group.left_label, left_text,),
            left_selected,
            true,
            Style::default(),
        ));

        let right_selected = matches!(
            selected_target,
            Some(
                ReplacementEditTarget::
                    AlternativeRight(
                        selected_option,
                    )
            ) if selected_option == option_index
        );

        let right_text = preview_text(
            app,
            StrategyTextTarget::Replacement {
                group_index,
                target: ReplacementEditTarget::AlternativeRight(option_index),
            },
            &option.right,
        );

        lines.push(editable_line(
            format!("{}: {}", group.right_label, right_text,),
            right_selected,
            true,
            Style::default(),
        ));

        lines.push(Line::from(""));
    }

    let rule_selected = matches!(selected_target, Some(ReplacementEditTarget::Rule));

    let rule_value = preview_text(
        app,
        StrategyTextTarget::Replacement {
            group_index,
            target: ReplacementEditTarget::Rule,
        },
        group.rule.as_deref().unwrap_or(""),
    );

    let show_rule = !rule_value.is_empty()
        || group.rule.is_some()
        || matches!(app.interaction_mode, InteractionMode::Edit);

    if show_rule {
        lines.push(Line::from(Span::styled("Rule", heading_style)));

        let rule_missing = rule_value.is_empty();

        let displayed_rule = if rule_missing {
            String::from("(none)")
        } else {
            rule_value
        };

        let rule_style = if rule_missing {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default()
        };

        lines.push(editable_line(
            displayed_rule,
            rule_selected,
            false,
            rule_style,
        ));
    }

    Text::from(lines)
}

fn editable_line(text: String, selected: bool, indented: bool, style: Style) -> Line<'static> {
    let prefix = if selected {
        "> "
    } else if indented {
        "  "
    } else {
        ""
    };

    let style = if selected {
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    };

    Line::from(Span::styled(format!("{prefix}{text}"), style))
}

fn player_status(app: &App, player_id: crate::player::PlayerId) -> (String, Style) {
    match app.draft_pick_for_player(player_id) {
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
    }
}

fn preview_text(app: &App, target: StrategyTextTarget, saved_value: &str) -> String {
    app.strategy_text_input
        .as_ref()
        .filter(|input| input.target == target)
        .map(|input| input.value.clone())
        .unwrap_or_else(|| saved_value.to_string())
}
