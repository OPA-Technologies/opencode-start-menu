use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};
use std::path::PathBuf;

use crate::{sanitize, App, InputMode, ProjectCreationState};

pub(crate) fn ui(f: &mut Frame, app: &mut App, input_mode: &InputMode, creation_state: &ProjectCreationState) {
    let size = f.area();
    app.terminal_size = (size.width, size.height);

    // Calculate layout
    let banner_height = 8;
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(banner_height),
            Constraint::Min(0),
        ])
        .split(size);

    // Render banner
    render_banner(f, vertical_chunks[0], app.banner_width);

    // Main content area
    let content_area = vertical_chunks[1];
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(35),
            Constraint::Percentage(65),
        ])
        .split(content_area);

    // Left panel - project list
    render_project_list(f, app, chunks[0]);

    // Right panel - details
    render_details(f, app, chunks[1]);

    // Help line at bottom
    let help_area = Rect {
        x: size.x,
        y: size.y + size.height.saturating_sub(1),
        width: size.width,
        height: 1,
    };
    render_help(f, help_area, input_mode, creation_state);

    // Status message overlay
    if let Some(msg) = &app.status_message {
        let area = Rect {
            x: 2,
            y: 2,
            width: size.width.saturating_sub(4),
            height: 3,
        };
        let block = Block::default()
            .title(" Message ")
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Black).fg(Color::Yellow));
        let paragraph = Paragraph::new(msg.as_str()).block(block).wrap(Wrap { trim: true });
        f.render_widget(paragraph, area);
    }

    // Creation dialog overlay
    if matches!(input_mode, InputMode::CreatingProject(_)) {
        render_creation_dialog(f, size, creation_state);
    }

    // Module selector overlay
    if let InputMode::SelectModule {
        project_idx,
        modules,
        selected,
    } = input_mode {
        let name = app
            .projects
            .get(*project_idx)
            .map(|p| p.name.as_str())
            .unwrap_or("Projet");
        render_module_dialog(f, size, name, modules, *selected);
    }
}

fn render_banner(f: &mut Frame, area: Rect, banner_width: u16) {
    let banner_opa = [
        "█▀▀█ █▀▀█ █▀▀▀ █▀▀█ █▀▀▀ █▀▀█ █▀▀▀ █▀▀█ █▀▀▀ █▀▀█ █▀▀▀ █▀▀█ █▀▀█ █▀▀▀",
        "█  █ █  █ █    █  █ █    █  █ █    █  █ █    █  █ █    █  █ █  █ █",
        "█  █ █  █ █    █  █ █    █  █ █    █  █ █    █  █ █    █  █ █  █ █",
        "█▀▀█ █▀▀█ █▀▀▀ █▀▀█ █▀▀▀ █▀▀▀ █▀▀▀ █▀▀█ █▀▀▀ █▀▀█ █▀▀▀ █▀▀█ █▀▀█ █▀▀▀",
        "█  █ █ █  █    █  █ █    █ █   █    █  █ █    █ █  █    █  █ █  █ █",
        "█  █ █  █ █    █  █ █    █  █ █    █  █ █    █  █ █    █  █ █  █ █",
        "█▀▀█ █   █ █▄▄▄ █   █ █▄▄▄ █   █ █▄▄▄ █   █ █▄▄▄ █   █ █   █ █   █ █▄▄▄",
        "                               █                                ",
    ];
    let banner_code = [
        "█▀▀█ █▀▀▀ █▀▀█ █▀▀█ █▀▀█ █▀▀▀ █▀▀█ █▀▀▀",
        "█  █ █    █  █ █  █ █  █ █    █  █ █",
        "█  █ █▀▀▀ █▀▀█ █▀▀█ █▀▀█ █▀▀▀ █  █ █▀▀▀",
        "█  █ █    █  █ █  █ █  █ █    █  █ █",
        "█▀▀█ █▄▄▄ █  █ █  █ █▀▀█ █▄▄▄ █▀▀█ █▄▄▄",
    ];

    let lines: Vec<Line> = (0..8)
        .map(|i| {
            Line::from(vec![
                Span::styled(
                    format!("{:width$}", "", width = (area.width.saturating_sub(banner_width) / 2) as usize),
                    Style::default(),
                ),
                Span::styled(banner_opa[i], Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ])
        })
        .collect();

    let code_lines: Vec<Line> = (0..5)
        .map(|i| {
            Line::from(vec![
                Span::styled(
                    format!("{:width$}", "", width = (area.width.saturating_sub(banner_width) / 2) as usize),
                    Style::default(),
                ),
                Span::styled("    ", Style::default()),
                Span::styled(banner_code[i], Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
            ])
        })
        .collect();

    let mut all_lines = lines;
    all_lines.push(Line::from(""));
    all_lines.extend(code_lines);

    let paragraph = Paragraph::new(all_lines).alignment(Alignment::Left);
    f.render_widget(paragraph, area);
}

fn render_project_list(f: &mut Frame, app: &mut App, area: Rect) {
    let mut items: Vec<ListItem> = app
        .filtered
        .iter()
        .enumerate()
        .map(|(i, &idx)| {
            let p = &app.projects[idx];
            let selected = app.list_state.selected() == Some(i);
            let style = if selected {
                Style::default().bg(Color::White).fg(Color::Black).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(
                format!("  > {}", sanitize(&p.name)),
                style,
            )))
        })
        .collect();

    if app.show_new {
        let i = app.filtered.len();
        let selected = app.list_state.selected() == Some(i);
        let style = if selected {
            Style::default().bg(Color::White).fg(Color::Black).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Green)
        };
        items.push(ListItem::new(Line::from(Span::styled(
            "  [+] Nouveau projet",
            style,
        ))));
    }

    // Entrée permanente « Sans projet » (index filtered.len()+show_new)
    let bare_sel = app.is_bare_selected();
    let bare_style = if bare_sel {
        Style::default().bg(Color::White).fg(Color::Black).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    items.push(ListItem::new(Line::from(Span::styled(
        "  Sans projet  (v)",
        bare_style,
    ))));

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Projets ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("► ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn render_details(f: &mut Frame, app: &App, area: Rect) {
    let (name, desc, modified, status, version) = if app.is_new_selected() {
        (
            "Nouveau projet",
            "Créer un nouveau projet",
            "—",
            "à définir",
            "à définir",
        )
    } else if let Some(idx) = app.selected_project_idx() {
        let p = &app.projects[idx];
        (
            p.name.as_str(),
            p.description.as_str(),
            p.modified.as_str(),
            if p.status.is_empty() { "—" } else { &p.status },
            if p.version.is_empty() { "—" } else { &p.version },
        )
    } else {
        ("", "", "", "", "")
    };

    let text = vec![
        Line::from(vec![
            Span::styled("Description  : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(desc)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Modifié le   : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(modified)),
        ]),
        Line::from(vec![
            Span::styled("Statut       : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(status)),
        ]),
        Line::from(vec![
            Span::styled("Version      : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(version)),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(
            Block::default()
                .title(format!(" Détails — {} ", sanitize(name)))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .wrap(Wrap { trim: true });

    f.render_widget(paragraph, area);
}

fn render_help(f: &mut Frame, area: Rect, input_mode: &InputMode, _creation_state: &ProjectCreationState) {
    let help_text = match input_mode {
        InputMode::Normal => "↑/↓ Naviguer  ·  Entrée Ouvrir  ·  m Module  ·  n Nouveau projet  ·  q Quitter",
        InputMode::SelectModule { .. } => {
            "↑/↓ Choisir module  ·  Entrée Ouvrir  ·  Échap Annuler"
        }
        InputMode::CreatingProject(state) => {
            const PROMPTS: &[&str] = &[
                "ID (lettres/chiffres/tirets) : ",
                "Nom (Entrée = ID) : ",
                "Chemin (Entrée = ~/projets/<id>) : ",
                "Description : ",
                "Statut (1=Alpha, 2=Beta, 3=Validation, 4=Clôturé, Entrée=aucun) : ",
                "Version (ex: 0.1) : ",
                "Ouvrir maintenant ? (o/N) : ",
            ];
            if state.step < PROMPTS.len() {
                PROMPTS[state.step]
            } else {
                "Terminé"
            }
        }
    };

    let paragraph = Paragraph::new(help_text)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(paragraph, area);
}

fn render_creation_dialog(f: &mut Frame, size: Rect, state: &ProjectCreationState) {
    let dialog_width = size.width.min(72);
    let dialog_height = 16;
    let x = (size.width.saturating_sub(dialog_width)) / 2;
    let y = (size.height.saturating_sub(dialog_height)) / 2;

    let area = Rect {
        x,
        y,
        width: dialog_width,
        height: dialog_height,
    };

    // Fond opaque : efface la zone puis repeint tout en gris foncé
    f.render_widget(Clear, area);

    let opaque = Style::default().bg(Color::DarkGray);

    let block = Block::default()
        .title(" Nouveau projet ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(opaque);

    let prompt_desc = match state.step {
        0 => "Attendu : identifiant court et unique — lettres/chiffres/tirets (ex : mon-projet)",
        1 => "Attendu : nom d'affichage du projet (Entrée = reprend l'ID)",
        2 => "Attendu : chemin du dossier (Entrée = ~/projets/<id>)",
        3 => "Attendu : description courte du projet (facultatif)",
        4 => "Attendu : 1=Alpha, 2=Beta, 3=Validation, 4=Clôturé (Entrée = aucun)",
        5 => "Attendu : version (ex : 0.1), facultatif",
        _ => "Attendu : o/N pour ouvrir le projet dès maintenant",
    };

    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("ID            : ", Style::default().fg(Color::Cyan)),
            Span::raw(&state.id),
        ]),
        Line::from(vec![
            Span::styled("Nom           : ", Style::default().fg(Color::Cyan)),
            Span::raw(&state.name),
        ]),
        Line::from(vec![
            Span::styled("Chemin        : ", Style::default().fg(Color::Cyan)),
            Span::raw(&state.path),
        ]),
        Line::from(vec![
            Span::styled("Description   : ", Style::default().fg(Color::Cyan)),
            Span::raw(&state.description),
        ]),
        Line::from(vec![
            Span::styled("Statut        : ", Style::default().fg(Color::Cyan)),
            Span::raw(match &state.status {
                Some(s) => s.clone(),
                None => "(aucun)".to_string(),
            }),
        ]),
        Line::from(vec![
            Span::styled("Version       : ", Style::default().fg(Color::Cyan)),
            Span::raw(&state.version),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Saisie : ", Style::default().fg(Color::Cyan)),
            Span::raw(&state.input_buffer),
            Span::styled("█", Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(Span::styled(prompt_desc, Style::default().fg(Color::Yellow))).centered(),
        Line::from("Échap pour annuler  ·  Entrée pour valider").centered(),
    ];

    // Remplit les lignes restantes pour un fond entièrement opaque
    while lines.len() < dialog_height.saturating_sub(2) as usize {
        lines.push(Line::from(""));
    }

    let paragraph = Paragraph::new(lines)
        .block(block)
        .style(opaque)
        .wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

fn render_module_dialog(
    f: &mut Frame,
    size: Rect,
    project_name: &str,
    modules: &[PathBuf],
    selected: usize,
) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("Projet : ", Style::default().fg(Color::Cyan)),
        Span::styled(
            sanitize(project_name),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));

    for i in 0..=modules.len() {
        let (label, is_selected) = if i == 0 {
            ("（racine）".to_string(), selected == 0)
        } else {
            let m = &modules[i - 1];
            let label = m
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| m.display().to_string());
            (label, selected == i)
        };
        let style = if is_selected {
            Style::default()
                .bg(Color::White)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(Span::styled(
            format!("  {} {}", if is_selected { "►" } else { " " }, sanitize(&label)),
            style,
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from("Échap annuler  ·  ↑/↓ naviguer  ·  Entrée ouvrir").centered());

    let dialog_width = size.width.min(70);
    let dialog_height = (lines.len() as u16 + 2).min(size.height.saturating_sub(2));
    let x = (size.width.saturating_sub(dialog_width)) / 2;
    let y = (size.height.saturating_sub(dialog_height)) / 2;

    let area = Rect {
        x,
        y,
        width: dialog_width,
        height: dialog_height,
    };

    let block = Block::default()
        .title(" Module ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::DarkGray));

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}