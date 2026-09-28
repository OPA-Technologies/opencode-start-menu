use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap},
    Frame,
};
use std::path::PathBuf;

use crate::{sanitize, App, InputMode, ProjectCreationState, ProjectEditState};

pub(crate) fn ui(
    f: &mut Frame,
    app: &mut App,
    input_mode: &InputMode,
    creation_state: &ProjectCreationState,
    edit_state: &ProjectEditState,
) {
    let size = f.area();

    let msg_h = if app.status_messages.is_empty() {
        0
    } else {
        (2 + app.status_messages.len())
            .min(crate::MAX_MESSAGE_LINES)
            .min(size.height.saturating_sub(9) as usize) // 8 ban. + 1 help
    };
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),     // banner: 8 lignes (max de 8 OPA et 6 Code côte à côte)
            Constraint::Length(msg_h as u16), // messages (0 ou taille adaptative)
            Constraint::Min(0),        // zone contenu
        ])
        .split(size);

    // Render banner
    render_banner(f, vertical_chunks[0]);

    // Status messages (between banner and content)
    if !app.status_messages.is_empty() {
        let area = vertical_chunks[1];
        let block = Block::default()
            .title(" Message ")
            .borders(Borders::ALL)
            .style(Style::default().bg(Color::Black).fg(Color::Yellow));
        let text = app.status_messages.join("\n");
        let paragraph = Paragraph::new(text).block(block).wrap(Wrap { trim: true });
        f.render_widget(paragraph, area);
    }

    // Main content area
    let content_area = vertical_chunks[2];
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
    render_help(f, help_area, input_mode, creation_state, edit_state);

    // Creation dialog overlay
    if matches!(input_mode, InputMode::CreatingProject(_)) {
        render_creation_dialog(f, size, creation_state);
    }

    // Edition dialog overlay
    if matches!(input_mode, InputMode::EditingProject(_)) {
        render_edit_dialog(f, size, app, edit_state);
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

fn render_banner(f: &mut Frame, area: Rect) {
    let banner_opa = [
        "▓▓▓▓▓▓▓▓  ▓▓▓▓▓▓▓▓  ▓▓▓▓▓▓▓▓        ▓▓▓▓▓▓▓▓▓▓             ▓   ",
        "▓░░░░░░▓  ▓░░░░░░▓░ ▓░░░░░░▓░      ░░░░▓▓░░░░             ▓░   ",
        "       ▓░        ▓░        ▓░             ▓▓▓▓▓▓░ ▓▓▓▓▓▓░ ▓▓▓▓▓░",
        "▓░     ▓░ ▓░     ▓░ ▓░     ▓░         ▓░ ▓░   ▓░ ▓░░░░░░ ▓░░░░▓░",
        "▓░     ▓░ ▓▓▓▓▓▓▓▓░ ▓▓▓▓▓▓▓▓░        ▓░   ▓▓▓▓░               ▓░",
        "▓░     ▓░ ▓░░░░░░░░ ▓░░░░░░▓░        ▓░ ▓░      ▓░      ▓░   ▓░",
        "▓▓▓▓▓▓▓▓░ ▓░        ▓░     ▓░       ▓░ ▓▓▓▓▓▓▓ ▓▓▓▓▓▓▓░ ▓░   ▓░",
        " ░░░░░░░░  ░         ░      ░        ░  ░░░░░░  ░░░░░░░  ░    ░ ",
    ];
    let banner_code = [
	"",
        "                                         █",
        "▒▒▒▒▒ ▒▒▒▒▒ ▒▒▒▒▒ ▒▒▒▒  █████ █████ █████ █████",
        "▒   ▒ ▒   ▒ ▒   ▒ ▒   ▒ █     █   █ █   █ █   █",
        "▒░░░▒ ▒░░░▒ ▒▒▒▒▒ ▒░░░▒ █░░░░ █░░░█ █░░░█ █████",
        " ▒░░░▒ ▒░░░▒ ▒░░░░ ▒░░░▒ █░░░░ █░░░█ █░░░█ █░░░░",
        " ▒▒▒▒▒ ▒▒▒▒▒ ▒▒▒▒▒ ▒░░░▒ █████ █████ █████ █████",
        "      ▒                                        ",
    ];

    let opa_h = banner_opa.len();
    let code_h = banner_code.len();
    let height = opa_h.max(code_h);
    let code_top_pad = (opa_h.saturating_sub(banner_code.len())) / 2;

    // Calculate max rendered line width (including ALL spaces)
    let mut max_line_width = 0usize;
    for i in 0..height {
        let opa_part = if i < banner_opa.len() { banner_opa[i] } else { "" };
        let code_part = if i >= code_top_pad && i < code_top_pad + banner_code.len() {
            banner_code[i - code_top_pad]
        } else {
            ""
        };
        let line_width = opa_part.chars().count() + 4 + code_part.chars().count();
        if line_width > max_line_width {
            max_line_width = line_width;
        }
    }

    // Centrage horizontal basé sur la largeur RÉELLE rendue
    let left_pad = if area.width as usize > max_line_width {
        (area.width as usize - max_line_width) / 2
    } else {
        0
    };
    let left_pad_str = " ".repeat(left_pad);

    let mut all_lines: Vec<Line> = Vec::with_capacity(height);

    for i in 0..height {
        let opa_part = if i < banner_opa.len() { banner_opa[i] } else { "" };
        
        let code_part = if i >= code_top_pad && i < code_top_pad + banner_code.len() {
            banner_code[i - code_top_pad]
        } else {
            ""
        };
        
        let combined = format!("{}{}{}{}", left_pad_str, opa_part, "    ", code_part);
        all_lines.push(Line::from(vec![
            Span::styled(combined, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        ]));
    }

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

fn variant_box(
    title: &str,
    title_color: Color,
    version: &str,
    status: &str,
    modified: &str,
) -> Paragraph<'static> {
    let dash = |s: &str| -> String { if s.is_empty() { "—".to_string() } else { s.to_string() } };
    let lines = vec![
        Line::from(vec![
            Span::styled("Version  : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(&dash(version))),
        ]),
        Line::from(vec![
            Span::styled("Statut   : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(&dash(status))),
        ]),
        Line::from(vec![
            Span::styled("Modifié le : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(modified)),
        ]),
    ];
    let block = Block::default()
        .title(Span::styled(
            title.to_string(),
            Style::default().fg(title_color).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(title_color));
    Paragraph::new(lines).block(block).wrap(Wrap { trim: false })
}

fn render_details(f: &mut Frame, app: &App, area: Rect) {
    let (
        name,
        id,
        path,
        desc,
        debug_modified,
        release_modified,
        total_tokens,
        used_pct,
        total_cost,
        session_count,
        last_session_tokens,
        last_session_cost,
        last_model,
        last_agent,
        last_time_created,
    ) = if app.is_new_selected() {
        (
            "Nouveau projet".to_string(),
            "à définir".to_string(),
            "à définir".to_string(),
            "Créer un nouveau projet".to_string(),
            "jamais compilée".to_string(),
            "jamais compilée".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
        )
    } else if let Some(idx) = app.selected_project_idx() {
        let p = &app.projects[idx];
        let or_dash =
            |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
        (
            p.name.clone(),
            if p.id.is_empty() { "—".to_string() } else { p.id.clone() },
            p.path.display().to_string(),
            p.description.clone(),
            p.debug_modified.clone(),
            p.release_modified.clone(),
            or_dash(&p.total_tokens),
            or_dash(&p.used_pct),
            or_dash(&p.total_cost),
            or_dash(&p.session_count),
            or_dash(&p.last_session_tokens),
            or_dash(&p.last_session_cost),
            or_dash(&p.last_model),
            or_dash(&p.last_agent),
            or_dash(&p.last_time_created),
        )
    } else {
        (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        )
    };

    let sep_pad = (area.width as usize * 25) / 100;
    let sep = || {
        Line::from(vec![
            Span::raw(" ".repeat(sep_pad)),
            Span::styled("═".repeat(25), Style::default().fg(Color::DarkGray)),
        ])
    };

    let mut text: Vec<Line> = Vec::new();
    // Groupe 1
    text.push(Line::from(vec![
        Span::styled("Nom          : ", Style::default().fg(Color::DarkGray)),
        Span::raw(sanitize(&name)),
    ]));
    text.push(Line::from(vec![
        Span::styled("ID           : ", Style::default().fg(Color::DarkGray)),
        Span::raw(sanitize(&id)),
    ]));
    text.push(Line::from(vec![
        Span::styled("Chemin       : ", Style::default().fg(Color::DarkGray)),
        Span::raw(sanitize(&path)),
    ]));
    text.push(sep());
    // Groupe 2
    text.push(Line::from(vec![
        Span::styled("Description  : ", Style::default().fg(Color::DarkGray)),
        Span::raw(sanitize(&desc)),
    ]));
    text.push(sep());

    let block = Block::default()
        .title(format!(" Détails — {} ", sanitize(&name)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));
    f.render_widget(block.clone(), area);
    let inner = block.inner(area);

    // Zone supérieure : groupes 1 et 2, puis variantes, puis tableau
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(6),
            Constraint::Length(9),
        ])
        .split(inner);
    let top_area = split[0];
    let variant_area = split[1];
    let table_area = split[2];

    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    f.render_widget(paragraph, top_area);

    // Groupe 3 : variantes Debug | Release (Release en lecture seule)
    let (debug_version, debug_status, release_version, release_status) =
        if let Some(idx) = app.selected_project_idx() {
            let p = &app.projects[idx];
            (
                p.version_debug.as_str(),
                p.status_debug.as_str(),
                p.version_release.as_str(),
                p.status_release.as_str(),
            )
        } else {
            ("", "", "", "")
        };
    let var_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(variant_area);
    f.render_widget(
        variant_box(" Debug ", Color::Cyan, debug_version, debug_status, &debug_modified),
        var_chunks[0],
    );
    f.render_widget(
        variant_box(" Release ", Color::Green, release_version, release_status, &release_modified),
        var_chunks[1],
    );

    // Groupe 4 : tableau 3 colonnes Libellé | Session | Totaux
    let header_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(Color::DarkGray);
    let header = Row::new(vec![
        Cell::from("Libellé"),
        Cell::from("Session"),
        Cell::from("Totaux"),
    ])
    .style(header_style);
    let rows = vec![
        Row::new(vec![
            Cell::from("Tokens").style(label_style),
            Cell::from(sanitize(&last_session_tokens)),
            Cell::from(sanitize(&total_tokens)),
        ]),
        Row::new(vec![
            Cell::from("% Utilisé").style(label_style),
            Cell::from(sanitize(&used_pct)),
            Cell::from("—"),
        ]),
        Row::new(vec![
            Cell::from("Coût").style(label_style),
            Cell::from(sanitize(&last_session_cost)),
            Cell::from(sanitize(&total_cost)),
        ]),
        Row::new(vec![
            Cell::from("Modèle").style(label_style),
            Cell::from(sanitize(&last_model)),
            Cell::from("—"),
        ]),
        Row::new(vec![
            Cell::from("Agent").style(label_style),
            Cell::from(sanitize(&last_agent)),
            Cell::from("—"),
        ]),
        Row::new(vec![
            Cell::from("Date / Heure").style(label_style),
            Cell::from(sanitize(&last_time_created)),
            Cell::from("—"),
        ]),
        Row::new(vec![
            Cell::from("Sessions").style(label_style),
            Cell::from("—"),
            Cell::from(sanitize(&session_count)),
        ]),
    ];
    let table = Table::new(
        rows,
        [
            Constraint::Percentage(30),
            Constraint::Percentage(35),
            Constraint::Percentage(35),
        ],
    )
    .header(header)
    .column_spacing(2);

    f.render_widget(table, table_area);
}


fn render_help(
  f: &mut Frame,
  area: Rect,
  input_mode: &InputMode,
  _creation_state: &ProjectCreationState,
  _edit_state: &ProjectEditState,
)
  {
    let help_text = match input_mode
      {
        InputMode::Normal =>
          {
            "↑/↓ Naviguer  ·  Entrée Ouvrir  ·  m Module  ·  e Modifier  ·  n Nouveau projet  ·  q Quitter"
          }
        InputMode::SelectModule { .. } =>
          {
            "↑/↓ Choisir module  ·  Entrée Ouvrir  ·  Échap Annuler"
          }
        InputMode::EditingProject(state) =>
          {
            if state.status_menu_open
              {
                "↑/↓ Choisir un statut · Entrée valider · Échap annuler · Frappe filtrer"
              }
            else if state.editing
              {
                "←/→ déplacer le curseur  ·  Frappe saisie  ·  Entrée valider le champ  ·  Échap annuler"
              }
            else if state.active_field == 3
              {
                "o = Enregistrer  ·  N = Annuler  ·  Échap annuler"
              }
            else
              {
                "↑/↓ changer de champ  ·  Entrée éditer  ·  Échap annuler"
              }
          }
        InputMode::CreatingProject(state) =>
          {
            match state.active_field
              {
                0 =>
                  {
                    "↑/↓ changer de champ  ·  ←/→ déplacer le curseur  ·  Frappe saisie  ·  Entrée suivant"
                  }
                1 =>
                  {
                    "↑/↓ changer de champ  ·  ←/→ déplacer le curseur  ·  Frappe saisie  ·  Entrée suivant"
                  }
                2 =>
                  {
                    "↑/↓ changer de champ  ·  ←/→ déplacer le curseur  ·  Frappe saisie  ·  Entrée suivant"
                  }
                3 =>
                  {
                    "↑/↓ changer de champ  ·  ←/→ déplacer le curseur  ·  Frappe saisie  ·  Entrée suivant"
                  }
                4 => if state.status_menu_open
                  {
                    "↑/↓ Choisir un statut · Entrée valider · Échap annuler · Frappe filtrer"
                  }
                else
                  {
                    "↑/↓ changer de champ  ·  Entrée : liste des statuts · Frappe filtrer"
                  },
                5 =>
                  {
                    "↑/↓ changer de champ  ·  ←/→ déplacer le curseur  ·  Frappe saisie  ·  Entrée suivant"
                  }
                _ =>
                  {
                    "o = créer et ouvrir  ·  N = ouvrir sans créer  ·  Entrée = o"
                  }
              }
          }
      };

    let paragraph = Paragraph::new(help_text)
      .style(Style::default().fg(Color::DarkGray))
      .alignment(Alignment::Center);
    f.render_widget(paragraph, area);
  }

/// Champ de formulaire : texte affiché (ou « (vide) » si aucun contenu).
fn field_text(v: &str) -> String {
    let t = v.trim();
    if t.is_empty() {
        "(vide)".to_string()
    } else {
        t.to_string()
    }
}

fn render_creation_dialog(f: &mut Frame, size: Rect, state: &ProjectCreationState)
  {
    let dialog_width = size.width.min(76);
    let dialog_height = 15;
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

    // Lignes : libellé aligné + valeur. Le champ actif est surligné (REVERSED).
    let active = state.active_field.min(6);
    let fields: [(&str, String); 6] = [
        ("ID           ", field_text(&state.id)),
        ("Nom          ", field_text(&state.name)),
        ("Chemin       ", field_text(&state.path)),
        ("Description  ", field_text(&state.description)),
        ("Statut       ", state.status.clone().unwrap_or_else(|| "(aucun)".to_string())),
        ("Version      ", field_text(&state.version)),
      ];

    let lbl_style = |i: usize| -> Style
      {
        if i == active
          {
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::REVERSED)
          }
        else
          {
            Style::default().fg(Color::Cyan)
          }
      };

    let mut lines = vec![
        Line::from(""),
        Line::from(""),
      ];
    for (i, (lbl, val)) in fields.iter().enumerate()
      {
        // Le champ actif prêt à être écrasé (rewrite) est surligné en jaune,
        // sauf les placeholders (« (vide) », « (aucun) »).
        let value_style = if i == active
          && state.rewrite_mode
          && !val.starts_with('(')
          {
            Style::default().fg(Color::Black).bg(Color::Yellow)
          }
        else
          {
            Style::default()
          };
        lines.push(Line::from(vec![
            Span::styled(*lbl, lbl_style(i)),
            Span::styled(" : ", Style::default().fg(Color::DarkGray)),
            Span::styled(val.clone(), value_style),
          ]));
      }

    // Ligne de confirmation finale o/N (toujours visible, actif si active_field == 6)
    let confirm_lbl = Span::styled("Ouvrir ?     ", lbl_style(6));
    let confirm_hint: &str = if active == 6 { "o/N" } else { "o/N" };
    lines.push(Line::from(vec![
        confirm_lbl,
        Span::styled(" : ", Style::default().fg(Color::DarkGray)),
        Span::raw(confirm_hint),
      ]));

    // Confirmation d'un nouveau statut (hors liste du registre)
    if state.confirm_status
      {
        lines.push(Line::from(vec![
            Span::styled(
              format!("Créer un nouveau statut « {} » ? (o/N)", state.input_buffer.trim()),
              Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
          ]));
      }
    else
      {
        lines.push(Line::from(""));
      }
    lines.push(Line::from(
      if state.status_menu_open
        {
          "↑/↓ Naviguer · Entrée valider · Échap annuler · Frappe filtrer"
        }
      else
        {
          "Échap pour annuler  ·  ↑/↓ pour changer de champ"
        }
    )
    .centered());

    // Remplit les lignes restantes pour un fond entièrement opaque
    while lines.len() < dialog_height.saturating_sub(2) as usize
      {
        lines.push(Line::from(""));
      }

    let paragraph = Paragraph::new(lines)
      .block(block)
      .style(opaque)
      .wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);

    // Curseur réel : sur la valeur du champ actif.
    // Colonne de début de valeur = bordure + libellé réel + « : ».
    let cursor_y = area.y + 3 + active as u16; // bordure(1) + 2 lignes vides + index champ
    let cursor_x = (area.x + 1 + fields[active].0.chars().count() as u16 + 3
      + state.input_offset as u16)
      .min(x.saturating_add(dialog_width).saturating_sub(2));
    f.set_cursor_position(Position::new(cursor_x, cursor_y));

    // Menu déroulant Statut (champ 4) : popup opaque sous la ligne du champ.
    if state.status_menu_open
      {
        let items = crate::status_menu_creation_items(&state.status_filter);
        let visible = items.len().min(4);
        let mut offset = 0usize;
        if state.status_cursor >= visible
          {
            offset = state.status_cursor - visible + 1;
          }
        let width = items.iter().map(|s| s.chars().count()).max().unwrap_or(8) + 2;
        let px = (area.x + 3).min(x.saturating_add(dialog_width).saturating_sub(width as u16));
        let py = area.y + 8; // sous la ligne du champ Statut (row = area.y+7)
        let parea = Rect {
            x: px,
            y: py,
            width: width as u16,
            height: (visible + 2) as u16,
          };
        f.render_widget(Clear, parea);
        let mut popup: Vec<Line> = Vec::new();
        for (idx, item) in items.iter().enumerate().skip(offset).take(visible)
          {
            let style = if idx == state.status_cursor
              {
                Style::default().fg(Color::Black).bg(Color::Cyan)
              }
            else
              {
                Style::default().fg(Color::White)
              };
            popup.push(Line::from(Span::styled(*item, style)));
          }
        let title = if state.status_filter.is_empty()
          {
            " Statut ".to_string()
          }
        else
          {
            format!(" Statut · filtre:{} ", state.status_filter)
          };
        let pblock = Block::default()
          .title(title)
          .borders(Borders::ALL)
          .border_style(Style::default().fg(Color::Cyan))
          .style(Style::default().bg(Color::Black));
        let ppar = Paragraph::new(popup).block(pblock);
        f.render_widget(ppar, parea);
      }
  }

fn render_edit_dialog(f: &mut Frame, size: Rect, app: &App, state: &ProjectEditState)
  {
    let name = app
      .projects
      .get(state.project_idx)
      .map(|p| p.name.as_str())
      .unwrap_or("Projet");

    let dialog_width = size.width.min(82);
    let dialog_height = 12;
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
      .title(format!(" Édition — {} ", sanitize(name)))
      .borders(Borders::ALL)
      .border_style(Style::default().fg(Color::Cyan))
      .style(opaque);

    let active = state.active_field.min(3);
    let current_value = |state: &ProjectEditState, field: usize| -> String
      {
        if state.editing && field == active
          {
            if state.input_buffer.is_empty()
              {
                "(vide)".to_string()
              }
            else
              {
                state.input_buffer.clone()
              }
          }
        else
          {
            match field
              {
                0 => if state.pending_version.is_empty()
                  {
                    "(vide)".to_string()
                  }
                else
                  {
                    state.pending_version.clone()
                  },
                1 => if state.pending_status.is_empty()
                  {
                    "(aucun)".to_string()
                  }
                else
                  {
                    state.pending_status.clone()
                  },
                2 => if state.pending_description.is_empty()
                  {
                    "(vide)".to_string()
                  }
                else
                  {
                    state.pending_description.clone()
                  },
                _ =>
                  {
                    "o/N".to_string()
                  }
              }
          }
      };

    let lbl_style = |i: usize| -> Style
      {
        if i == active
          {
            Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::REVERSED)
          }
        else
          {
            Style::default().fg(Color::Cyan)
          }
      };

    let labels = [
        ("Version Debug ", "1.2.3 (format x.y.z)"),
        ("Statut Debug  ", "Study · Alpha · Beta · Debug · Beta-Release · Release · Stable · Deprecated"),
        ("Description   ", "texte libre non vide"),
        ("Enregistrer ? ", "Variante Debug seule enregistrée"),
      ];
    let vals = [
        current_value(state, 0),
        current_value(state, 1),
        current_value(state, 2),
        current_value(state, 3),
      ];

    let mut lines = vec![Line::from("")];
    for i in 0..4
      {
        // Le champ actif prêt à être écrasé (rewrite) est surligné en jaune.
        let value_style = if state.editing
          && i == active
          && state.rewrite_mode
          && !state.input_buffer.is_empty()
          {
            Style::default().fg(Color::Black).bg(Color::Yellow)
          }
        else
          {
            Style::default()
          };
        lines.push(Line::from(vec![
            Span::styled(labels[i].0, lbl_style(i)),
            Span::styled(" : ", Style::default().fg(Color::DarkGray)),
            Span::styled(&vals[i], value_style),
          ]));
      }
    if let Some(err) = &state.error
      {
        lines.push(Line::from(vec![
            Span::styled("!", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {err}"), Style::default().fg(Color::Yellow)),
          ]));
      }
    else
      {
        lines.push(Line::from(""));
      }
    lines.push(Line::from(
      if state.status_menu_open
        {
          "↑/↓ Naviguer · Entrée valider · Échap annuler · Frappe filtrer"
        }
      else if state.editing
        {
          "←/→ curseur · Frappe saisie · Entrée valider le champ · Échap annuler"
        }
      else if active == 3
        {
          "o = Enregistrer · N = Annuler · Échap annuler"
        }
      else
        {
          "Échap annuler · ↑/↓ changer de champ · Entrée éditer"
        }
    )
    .centered());
    // indices ^ [line1] - hint. Aide champ (sur 2 lignes) + espace réservé
    lines.push(Line::from(
      "Version : x.y.z seulement · Statut : valeur de la liste connue · Description : non vide",
    )
    .centered());

    // Remplit les lignes restantes pour un fond entièrement opaque
    while lines.len() < dialog_height.saturating_sub(2) as usize
      {
        lines.push(Line::from(""));
      }

    let paragraph = Paragraph::new(lines)
      .block(block)
      .style(opaque)
      .wrap(Wrap { trim: false });
    f.render_widget(paragraph, area);

    // Curseur réel : sur la valeur du champ en cours d'édition.
    if state.editing
      {
        let cursor_y = area.y + 2 + active as u16; // bordure(1) + ligne vide(1) + index champ
        // Colonne de début de valeur = bordure + libellé réel + « : ».
        let value_x = area.x + 1 + labels[active].0.chars().count() as u16 + 3;
        let cursor_x = (value_x + state.input_offset as u16).min(
          x.saturating_add(dialog_width).saturating_sub(2),
        );
        f.set_cursor_position(Position::new(cursor_x, cursor_y));
      }

    // Menu déroulant Statut (champ 1) : popup opaque sous la ligne du champ.
    if state.status_menu_open
      {
        let items = crate::filtered_statuses(&state.status_filter);
        let visible = items.len().min(5);
        let mut offset = 0usize;
        if !items.is_empty() && state.status_cursor >= visible
          {
            offset = state.status_cursor - visible + 1;
          }
        let width = items.iter().map(|s| s.chars().count()).max().unwrap_or(8) + 2;
        let px = (area.x + 3).min(x.saturating_add(dialog_width).saturating_sub(width as u16));
        let py = area.y + 4; // sous la ligne du champ Statut (row = area.y+3)
        let parea = Rect {
            x: px,
            y: py,
            width: width as u16,
            height: (visible + 2) as u16,
          };
        f.render_widget(Clear, parea);
        let mut popup: Vec<Line> = Vec::new();
        for (idx, item) in items.iter().enumerate().skip(offset).take(visible)
          {
            let style = if idx == state.status_cursor
              {
                Style::default().fg(Color::Black).bg(Color::Cyan)
              }
            else
              {
                Style::default().fg(Color::White)
              };
            popup.push(Line::from(Span::styled(*item, style)));
          }
        let title = if state.status_filter.is_empty()
          {
            " Statut ".to_string()
          }
        else
          {
            format!(" Statut · filtre:{} ", state.status_filter)
          };
        let pblock = Block::default()
          .title(title)
          .borders(Borders::ALL)
          .border_style(Style::default().fg(Color::Cyan))
          .style(Style::default().bg(Color::Black));
        let ppar = Paragraph::new(popup).block(pblock);
        f.render_widget(ppar, parea);
      }
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
