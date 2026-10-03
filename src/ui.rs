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

    // Edition dialog overlay (onglet Projet du popup unifié)
    if let InputMode::Popup(popup) = input_mode {
        if popup.onglet == crate::Onglet::Projet {
            render_edit_dialog(f, size, app, &popup.projet);
        }
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
        _total_tokens,
        used_pct,
        total_cost,
        session_count,
        last_session_tokens,
        last_session_cost,
        last_model,
        last_agent,
        last_time_created,
        last_session_context_pct,
        total_tokens_m,
        last_duree,
        total_duree,
        last_valorisation,
        total_valorisation,
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
            "—".to_string(),
            "0".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
        )
    } else if let Some(idx) = app.selected_project_idx() {
        let p = &app.projects[idx];
        let or_dash =
            |s: &str| if s.is_empty() { "—".to_string() } else { s.to_string() };
        // Tokens en millions pour l'affichage "XM / 100M (Y%)" — on retire
        // tous les séparateurs de milliers (espace ASCII, U+202F, virgules
        // selon locale) puis on formate avec une décimale (874_009 → "0,9M").
        let total_tokens_m = if p.total_tokens.is_empty() {
            "0".to_string()
        } else {
            let digits: String = p
                .total_tokens
                .chars()
                .filter(|c| c.is_ascii_digit())
                .collect();
            let tokens = digits.parse::<u64>().unwrap_or(0);
            crate::format_millions(tokens)
        };
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
            or_dash(&p.last_session_context_pct),
            total_tokens_m,
            if p.last_duree.is_empty() { "—".to_string() } else { p.last_duree.clone() },
            if p.total_duree.is_empty() { "—".to_string() } else { p.total_duree.clone() },
            if p.last_valorisation.is_empty() {
                "—".to_string()
            } else {
                p.last_valorisation.clone()
            },
            if p.total_valorisation.is_empty() {
                "—".to_string()
            } else {
                p.total_valorisation.clone()
            },
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
            Constraint::Length(11),
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
    // Construire la ligne Tokens Totaux : "XM / 100M (Y%)"
    let tokens_total_display = if used_pct == "—"
      {
        "—".to_string()
      }
    else
      {
        format!("{}M / 100M ({}%)", total_tokens_m, used_pct)
      };
    let rows = vec![
        Row::new(vec![
            Cell::from("Tokens").style(label_style),
            Cell::from(sanitize(&last_session_tokens)),
            Cell::from(sanitize(&tokens_total_display)),
        ]),
        Row::new(vec![
            Cell::from("Contexte").style(label_style),
            Cell::from(sanitize(&last_session_context_pct)),
            Cell::from("—"),
        ]),
        Row::new(vec![
            Cell::from("Coût").style(label_style),
            Cell::from(sanitize(&last_session_cost)),
            Cell::from(sanitize(&total_cost)),
        ]),
        Row::new(vec![
            Cell::from("Temps").style(label_style),
            Cell::from(sanitize(&last_duree)),
            Cell::from(sanitize(&total_duree)),
        ]),
        Row::new(vec![
            Cell::from("Valorisation").style(label_style),
            Cell::from(sanitize(&last_valorisation)),
            Cell::from(sanitize(&total_valorisation)),
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
            "↑/↓ Naviguer  ·  Entrée Ouvrir  ·  m Module  ·  e Modifier  ·  n Nouveau projet  ·  c Coûts détaillés  ·  t Valorisation  ·  q Quitter"
          }
        InputMode::SelectModule { .. } =>
          {
            "↑/↓ Choisir module  ·  Entrée Ouvrir  ·  Échap Annuler"
          }
        InputMode::Popup(_) => {
            "↑/↓ Naviguer · ←/→ Changer onglet · Entrée Éditer · Échap Fermer"
        }
        InputMode::Aide { detaillee: _ } => {
            "Échap/F1 Fermer l'aide"
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
                    "Saisir o puis Entrée = créer et ouvrir  ·  Entrée seule = créer sans ouvrir"
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
/// Placeholder affiché quand un champ n'a rien à montrer.
const VIDE: &str = "(vide)";

/// Valeur réellement affichée pour un champ de formulaire.
///
/// La frappe en cours (`buffer`) prime sur la valeur validée (`valide`) : c'est
/// la règle qui manquait au formulaire de création, où seule la valeur
/// validée à Entrée était peinte. Le texte tapé atterrissait dans `buffer`,
/// donc rien ne bougeait à l'écran avant validation — la saisie était
/// aveugle, alors que le curseur, lui, suivait bien `input_offset`.
///
/// L'onglet Projet appliquait déjà cette règle à la main ; elle est factorisée
/// ici pour que les deux dialogues ne puissent plus diverger.
pub(crate) fn valeur_affiche(valide: &str, buffer: &str, en_saisie: bool, placeholder: &str) -> String
  {
    let source = if en_saisie { buffer } else { valide };
    if source.trim().is_empty() {
        placeholder.to_string()
    } else {
        source.to_string()
    }
}

/// Colonne d'affichage du curseur dans un champ : largeur *rendue* du préfixe
/// saisi, pas son nombre de caractères. Un offset de caractères utilisé tel
/// quel comme colonne décale le curseur de droite sur tout caractère large
/// (`é`, `→`, caractères CJK…).
pub(crate) fn colonne_curseur(buffer: &str, offset_chars: usize) -> usize
  {
    let octets = crate::byte_index_for_char(buffer, offset_chars);
    let prefixe = buffer.get(..octets).unwrap_or(buffer);
    // `Line::width` mesure la largeur d'affichage réelle (une colonne par
    // cellule), contrairement à `chars().count()`.
    Line::from(prefixe).width()
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
    let area = area.intersection(size);

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
    // Un champ est « en saisie » quand la frappe courante lui appartient. Pour
    // le Statut, ce n'est le cas qu'en texte libre : sinon la frappe alimente
    // le filtre du menu déroulant, pas `input_buffer`.
    let affiche = |valide: &str, en_saisie: bool, placeholder: &str| -> String
      {
        valeur_affiche(valide, &state.input_buffer, en_saisie, placeholder)
      };
    let saisie = |i: usize| i == active;
    let fields: [(&str, String); 6] = [
        ("ID           ", affiche(&state.id, saisie(0), VIDE)),
        ("Nom          ", affiche(&state.name, saisie(1), VIDE)),
        ("Chemin       ", affiche(&state.path, saisie(2), VIDE)),
        ("Description  ", affiche(&state.description, saisie(3), VIDE)),
        (
            "Statut       ",
            affiche(
                state.status.as_deref().unwrap_or(""),
                saisie(4) && state.status_free_text,
                "(aucun)",
            ),
        ),
        ("Version      ", affiche(&state.version, saisie(5), VIDE)),
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
    // Décalage dans le champ : largeur rendue du préfixe saisi, pas son
    // nombre de caractères (sinon les accents décalent le curseur).
    let dans_champ = if active == 4 && !state.status_free_text
      {
        0
      }
    else
      {
        colonne_curseur(&state.input_buffer, state.input_offset)
      };
    let cursor_x = (area.x + 1 + fields.get(active).map_or(13, |f| f.0.chars().count()) as u16 + 3
      + dans_champ as u16)
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
        let parea = parea.intersection(size);
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
    let area = area.intersection(size);

    // Fond opaque : efface la zone puis repeint tout en gris foncé
    f.render_widget(Clear, area);

    let opaque = Style::default().bg(Color::DarkGray);

    let block = Block::default()
      .title(format!(" Édition — {} ", sanitize(name)))
      .borders(Borders::ALL)
      .border_style(Style::default().fg(Color::Cyan))
      .style(opaque);

    let active = state.active_field.min(3);
    // Même règle que le formulaire de création, via le helper commun : la
    // frappe en cours l'emporte sur la valeur validée.
    let current_value = |state: &ProjectEditState, field: usize| -> String
      {
        let valide = match field
          {
            0 => state.pending_version.as_str(),
            1 => state.pending_status.as_str(),
            2 => state.pending_description.as_str(),
            _ => "o/N",
          };
        let placeholder = if field == 1 { "(aucun)" } else { VIDE };
        valeur_affiche(
            valide,
            &state.input_buffer,
            state.editing && field == active,
            placeholder,
          )
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
        let parea = parea.intersection(size);
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
    let area = area.intersection(size);

    let block = Block::default()
        .title(" Module ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::DarkGray));

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

fn render_cost_detail_popup(
    f: &mut Frame,
    size: Rect,
    project: &crate::ProjectEntry,
    scroll_offset: usize,
) {
    let mut rows: Vec<Row> = Vec::new();

    // Header
    rows.push(Row::new(vec![
        Cell::from("Date/Heure").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Cell::from("Modèle").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Cell::from("Coût orig.").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Cell::from("Converti").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Cell::from("Devise").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
    ]));

    // For now, we need to get session data from the DB
    // Since we don't have direct access to all sessions here, we'll show the last session info
    // and a note about aggregated data
    // Un « — » en colonne Coût ne signifie pas « gratuit » : opencode
    // enregistre cost = 0 quand le fournisseur ne publie aucune grille
    // tarifaire pour le modèle employé, alors que les tokens ont bien été
    // consommés. Les deux cas sont donc traités séparément.
    let cout_inconnu = project.last_session_cost.trim() == "—"
        || project.total_cost.trim() == "—";

    let has_usage = project.total_cost.chars().any(|c| c.is_ascii_digit() && c != '0');

    if has_usage {
        // Show last session
        let last_session_cost = if project.last_session_cost.is_empty() {
            "—".to_string()
        } else {
            project.last_session_cost.clone()
        };
        let last_model = if project.last_model.is_empty() { "—" } else { &project.last_model };
        let last_time = if project.last_time_created.is_empty() { "—" } else { &project.last_time_created };

        // Extract currency from last_session_cost
        let currency = if project.last_session_cost.contains('$') { "$ (USD)" }
        else if project.last_session_cost.contains('€') { "€ (EUR)" }
        else if project.last_session_cost.contains('£') { "£ (GBP)" }
        else if project.last_session_cost.contains('¥') { "¥ (JPY)" }
        else { "—" };

        rows.push(Row::new(vec![
            Cell::from(sanitize(last_time)),
            Cell::from(sanitize(last_model)),
            Cell::from(sanitize(&last_session_cost)),
            Cell::from("—"),
            Cell::from(sanitize(currency)),
        ]));

        // Total row
        rows.push(Row::new(vec![
            Cell::from("TOTAL").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from(format!("{} sessions", project.session_count)).style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from("").style(Style::default().add_modifier(Modifier::BOLD)),
            Cell::from(sanitize(&project.total_cost)).style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Cell::from("").style(Style::default().add_modifier(Modifier::BOLD)),
        ]));
    } else {
        // Message d'absence : distinguer « le fournisseur ne publie pas de
        // tarif » de « aucune session ».
        let explication = if cout_inconnu && !project.total_tokens.trim().is_empty() {
            Cell::from(format!(
                "Coût non communiqué par le fournisseur ({} tokens consommés)",
                project.total_tokens
            ))
        } else {
            Cell::from("Aucune donnée de coût disponible")
        };
        rows.push(Row::new(vec![
            explication,
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
            Cell::from(""),
        ]));
    }

    // Scroll handling
    let visible_rows = rows.len().saturating_sub(1); // -1 for header
    let max_scroll = visible_rows.saturating_sub(10); // ~10 visible rows
    let actual_scroll = scroll_offset.min(max_scroll);

    // Take visible rows
    let display_rows: Vec<Row> = if rows.len() > 1 {
        let start = 1 + actual_scroll; // skip header
        let end = (start + 10).min(rows.len());
        let mut display = vec![rows[0].clone()]; // header
        display.extend(rows[start..end].iter().cloned());
        display
    } else {
        rows
    };

    // Calculate popup size (80% of screen)
    let popup_width = (size.width as f32 * 0.8) as u16;
    let popup_height = (size.height as f32 * 0.7) as u16;
    let x = (size.width.saturating_sub(popup_width)) / 2;
    let y = (size.height.saturating_sub(popup_height)) / 2;

    let area = Rect {
        x,
        y,
        width: popup_width,
        height: popup_height,
    };
    let area = area.intersection(size);

    // Clear background
    f.render_widget(Clear, area);

    let block = Block::default()
        .title(" DÉTAIL COÛTS PAR SESSION ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let table = Table::new(
        display_rows,
        [
            Constraint::Percentage(20),
            Constraint::Percentage(30),
            Constraint::Percentage(20),
            Constraint::Percentage(15),
            Constraint::Percentage(15),
        ],
    )
    .header(Row::new(vec![
            Cell::from("").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Cell::from("").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Cell::from("").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Cell::from("").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
            Cell::from("").style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]))
    .column_spacing(2)
    .block(block);

    f.render_widget(table, area);

    // Rappel de la signification de « — », affiché seulement dans ce cas :
    // sans lui, un utilisateur y lit un zéro ou une session non lancée.
    if cout_inconnu {
        let rappel = Paragraph::new(
            "« — » : le fournisseur ne publie aucun tarif pour ce modèle, \
             opencode enregistre donc un coût nul malgré les tokens consommés.",
        )
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        let rappel_area = Rect {
            x: area.x + 1,
            y: area.y + area.height.saturating_sub(2),
            width: area.width.saturating_sub(2),
            height: 1,
        };
        if rappel_area.height > 0 {
            f.render_widget(rappel, rappel_area);
        }
    }

    // Scroll indicator
    let scroll_info = format!(
        "▲ {} lignes  │  ↑/↓ Ligne  PgUp/PgDn Page  Home/End Extrêmes  │  Échap Fermer",
        visible_rows
    );
    let scroll_area = Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(1),
        width: area.width.saturating_sub(2),
        height: 1,
    };
    let scroll_para = Paragraph::new(scroll_info)
        .style(Style::default().fg(Color::DarkGray))
        .alignment(Alignment::Center);
    f.render_widget(scroll_para, scroll_area);
}

/// Popup « Valorisation du temps » (touche `t`).
///
/// Affiche le temps passé de la dernière session et du cumul, sa valorisation
/// en euros, puis le détail du tarif appliqué (origine, année, date
/// d'actualisation). Les limites de la méthode sont explicitées : l'utilisateur
/// doit savoir qu'il s'agit d'une estimation fondée sur un salaire horaire
/// moyen statistic published, et non d'un coût réellement supporté.
#[allow(clippy::too_many_arguments)]
fn render_tariff_popup(
    f: &mut Frame,
    size: Rect,
    app: &App,
    project: &crate::ProjectEntry,
    active_field: usize,
    editing: bool,
    input_buffer: &str,
    country: &str,
    message: Option<&str>,
    fetching: bool,
) {
    let popup_width = size.width.min(88).saturating_sub(4);
    let popup_height = size.height.min(30).saturating_sub(4);
    let x = (size.width.saturating_sub(popup_width)) / 2;
    let y = (size.height.saturating_sub(popup_height)) / 2;
    let area = Rect {
        x,
        y,
        width: popup_width,
        height: popup_height,
    };
    let area = area.intersection(size);

    f.render_widget(Clear, area);

    let block = Block::default()
        .title(format!(
            " VALORISATION DU TEMPS — {} ",
            sanitize(&project.name)
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));
    f.render_widget(block.clone(), area);
    let inner = block.inner(area);

    // Décalage de 2 lignes pour l'en-tête de colonnes : le popup partage sa
    // surface avec le tableau des durées, pas avec le tableau de la liste.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(inner);

    // --- En-tête : temps passé et valorisation -------------------------
    let valeur_style = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
    let label = Style::default().fg(Color::DarkGray);
    let en_tete = Table::new(
        vec![
            Row::new(vec![
                Cell::from("Mesure").style(label),
                Cell::from("Session").style(label),
                Cell::from("Cumul").style(label),
            ]),
            Row::new(vec![
                Cell::from("Temps passé"),
                Cell::from(sanitize(
                    &if project.last_duree.is_empty() {
                        "—".to_string()
                    } else {
                        project.last_duree.clone()
                    },
                )),
                Cell::from(sanitize(
                    &if project.total_duree.is_empty() {
                        "—".to_string()
                    } else {
                        project.total_duree.clone()
                    },
                )),
            ]),
            Row::new(vec![
                Cell::from("Valorisation"),
                Cell::from(sanitize(
                    &if project.last_valorisation.is_empty() {
                        "—".to_string()
                    } else {
                        project.last_valorisation.clone()
                    },
                ))
                .style(valeur_style),
                Cell::from(sanitize(
                    &if project.total_valorisation.is_empty() {
                        "—".to_string()
                    } else {
                        project.total_valorisation.clone()
                    },
                ))
                .style(valeur_style),
            ]),
        ],
        [
            Constraint::Percentage(30),
            Constraint::Percentage(35),
            Constraint::Percentage(35),
        ],
    )
    .column_spacing(2);
    f.render_widget(en_tete, chunks[0]);

    // --- Corps : réglage du tarif + limites de la méthode ---------------
    let (serie, secteur, fetched_ms) =
        crate::registry::serie_du_projet(project, &app.web_cache);
    let web = project.tarif_mode != "manuel";

    let champ_style = |actif: bool, edite: bool| {
        if edite {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else if actif {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };

    // Valeur affichée pour le champ actif pendant la saisie.
    let champ = |i: usize, valeur: &str, actif: usize, buffer: &str, edit: bool| -> String {
        if edit && actif == i {
            if buffer.is_empty() {
                "(vide)".to_string()
            } else {
                buffer.to_string()
            }
        } else if valeur.is_empty() {
            "—".to_string()
        } else {
            valeur.to_string()
        }
    };

    let mode_libelle = if web {
        "Source web (OIT/ILOSTAT)"
    } else {
        "Tarif manuel"
    };
    let tarif_libelle = if project.tarif_manual > 0.0 {
        format!(
            "{} /h",
            crate::format_currency(project.tarif_manual, crate::LOCAL_CURRENCY)
        )
    } else {
        "—".to_string()
    };

    let mut lignes: Vec<Line> = Vec::new();
    lignes.push(Line::from(vec![
        Span::styled(" Mode          : ", label),
        Span::styled(
            champ(0, mode_libelle, active_field, input_buffer, editing),
            champ_style(active_field == 0, editing),
        ),
    ]));
    lignes.push(Line::from(vec![
        Span::styled(" Tarif manuel  : ", label),
        Span::styled(
            champ(1, &tarif_libelle, active_field, input_buffer, editing),
            champ_style(active_field == 1, editing),
        ),
    ]));
    lignes.push(Line::from(vec![
        Span::styled(" Pays source   : ", label),
        Span::styled(
            champ(2, country, active_field, input_buffer, editing),
            champ_style(active_field == 2, editing),
        ),
    ]));

    // Détail du tarif appliqué et date d'actualisation de la source.
    if web {
        if serie.is_empty() {
            lignes.push(Line::from(vec![
                Span::styled(" Source        : ", label),
                Span::styled(
                    "aucune donnée — appuyer sur r pour interroger la source",
                    Style::default().fg(Color::Yellow),
                ),
            ]));
        } else {
            let dernier = serie.last();
            let annees: Vec<String> =
                serie.iter().map(|p| p.annee.to_string()).collect();
            let granularite = if secteur {
                "secteur J — information et communication"
            } else {
                "tous secteurs (repli national)"
            };
            let annee_ref = dernier.map(|p| p.annee).unwrap_or(0);
            lignes.push(Line::from(vec![
                Span::styled(" Source        : ", label),
                Span::raw(format!("OIT/ILOSTAT · {granularite}")),
            ]));
            lignes.push(Line::from(vec![
                Span::styled(" Années        : ", label),
                Span::raw(annees.join(", ")),
            ]));
            if let Some(p) = dernier {
                lignes.push(Line::from(vec![
                    Span::styled(" Tarif retenu  : ", label),
                    Span::styled(
                        format!(
                            "{} /h ({annee_ref}, source {})",
                            crate::format_currency(p.valeur_eur_h, crate::LOCAL_CURRENCY),
                            if p.devise_src.is_empty() {
                                crate::LOCAL_CURRENCY
                            } else {
                                p.devise_src.as_str()
                            }
                        ),
                        valeur_style,
                    ),
                ]));
            }
            // Date d'actualisation de la source, en « mois année » localisé.
            let maj = if fetched_ms > 0 { fetched_ms } else {
                dernier.map(|p| p.fetched_ms).unwrap_or(0)
            };
            let date_maj = if maj > 0 {
                crate::registry::format_mois_annee(maj)
            } else {
                "—".to_string()
            };
            lignes.push(Line::from(vec![
                Span::styled(" Actualisé le  : ", label),
                Span::raw(date_maj),
            ]));
        }
    } else {
        lignes.push(Line::from(vec![
            Span::styled(" Source        : ", label),
            Span::raw("valeur saisie manuellement, aucune source externe"),
        ]));
    }

    if fetching {
        lignes.push(Line::from(Span::styled(
            " Interrogation de la source en cours…",
            Style::default().fg(Color::Yellow),
        )));
    }
    if let Some(msg) = message {
        lignes.push(Line::from(Span::styled(
            msg.to_string(),
            Style::default().fg(Color::Yellow),
        )));
    }

    // Limites de la méthode : information indispensable à l'utilisateur pour
    // ne pas confondre l'estimation avec un coût réellement supporté.
    lignes.push(Line::from(""));
    lignes.push(Line::from(Span::styled(
        " Avertissement",
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    )));
    for avert in avertissements_tarif(web) {
        lignes.push(Line::from(Span::styled(
            format!(" • {avert}"),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraphe = Paragraph::new(lignes).wrap(Wrap { trim: false });
    f.render_widget(paragraphe, chunks[1]);

    let pied = Paragraph::new(
        " Entrée : basculer le mode / éditer  ·  r : rafraîchir la source  ·  Échap : fermer ",
    )
    .style(Style::default().fg(Color::DarkGray))
    .alignment(Alignment::Center);
    f.render_widget(pied, chunks[2]);
}

/// Limites de la méthode, à rappeler explicitement à l'utilisateur.
///
/// En mode web, le tarif provient d'un salaire horaire **moyen** publié par
/// l'OIT : c'est un ordre de grandeur, pas une facturation. De plus ce salaire
/// est un salaire brut, hors charges patronales, alors que le coût réel
/// d'un salarié pour un employeur inclut ces charges. La valorisation est
/// donc une estimation du temps passé, et non un coût subi.
fn avertissements_tarif(web: bool) -> Vec<String> {
    if web {
        vec![
            "Estimation, pas un coût réellement supporté.".to_string(),
            "Source : salaire horaire brut moyen OIT/ILOSTAT (charges patronales \
             exclues), et non le coût employeur complet."
                .to_string(),
            "La durée est mesurée entre le premier et le dernier message de la \
             session : le temps hors opencode n'est pas compté."
                .to_string(),
            "Chaque session est valorisée au tarif de son année (repli sur la \
             première valeur connue si la série commence plus tard)."
                .to_string(),
            "Les montants hors euro sont convertis au taux du jour de la mise à \
             jour de la source lorsqu'il est disponible, sinon à un taux de repli \
             fixe et approximatif."
                .to_string(),
        ]
    } else {
        vec![
            "Estimation, pas un coût réellement supporté.".to_string(),
            "Le tarif est une valeur saisie manuellement : l'utilisateur en reste \
             pleinement responsable."
                .to_string(),
            "La durée est mesurée entre le premier et le dernier message de la \
             session : le temps hors opencode n'est pas compté."
                .to_string(),
        ]
    }
}

#[cfg(test)]
mod tests
  {
    use ratatui::{backend::TestBackend, buffer::Buffer, Terminal};

    use super::*;

    /// Contenu texte d'un buffer, ligne par ligne.
    fn texte(buf: &Buffer) -> String
      {
        (0..buf.area.height)
          .map(|y| {
            (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>()
          })
          .collect::<Vec<_>>()
          .join("\n")
      }

    /// Rend le formulaire de création dans un terminal de test.
    fn rendre_creation(state: &ProjectCreationState) -> (String, (u16, u16))
      {
        let mut term = Terminal::new(TestBackend::new(90, 20)).unwrap();
        term.draw(|f| render_creation_dialog(f, f.area(), state)).unwrap();
        let curseur = term.get_cursor_position().unwrap();
        (texte(term.backend().buffer()), (curseur.x, curseur.y))
      }

    /// Formulaire prêt à saisir : champ Nom actif, rien de validé encore.
    fn saisie_en_cours(buffer: &str, offset: usize) -> ProjectCreationState
      {
        ProjectCreationState {
            active_field: 1,
            input_buffer: buffer.to_string(),
            input_offset: offset,
            ..ProjectCreationState::default()
          }
      }

    /// Régression B1 : la frappe doit être visible *avant* validation.
    ///
    /// Le formulaire peignait `state.name`, alimenté uniquement par Entrée, et
    /// non `state.input_buffer` où la frappe arrive : taper ne produisait rien
    /// à l'écran. Le curseur, lui, suivait l'offset — d'un texte figé.
    #[test]
    fn le_formulaire_affiche_la_frappe_avant_entree()
      {
        let (rendu, _) = rendre_creation(&saisie_en_cours("hello", 5));
        assert!(
            rendu.contains("hello"),
            "la frappe doit apparaître sans attendre Entrée. Rendu :\n{rendu}"
        );
      }

    /// Le champ actif ne doit pas masquer les valeurs déjà validées des
    /// autres champs : sinon le formulaire vidait son contenu à la frappe.
    #[test]
    fn les_champs_non_actifs_conservent_leur_valeur_validee()
      {
        let state = ProjectCreationState {
            id: "mon-projet".to_string(),
            name: "Mon Projet".to_string(),
            active_field: 2,
            input_buffer: "/tmp/x".to_string(),
            input_offset: 6,
            ..ProjectCreationState::default()
          };
        let (rendu, _) = rendre_creation(&state);
        assert!(rendu.contains("mon-projet"), "l'ID validé a disparu :\n{rendu}");
        assert!(rendu.contains("Mon Projet"), "le nom validé a disparu :\n{rendu}");
      }

    /// Régression B6 : le curseur doit avancer en colonnes affichées, pas en
    /// caractères. Un caractère large occupe deux colonnes ; le compter pour
    /// un laissait une colonne plus tôt que le texte réellement peint.
    #[test]
    fn le_curseur_suit_la_largeur_affichee()
      {
        assert_eq!(colonne_curseur("", 0), 0);
        assert_eq!(colonne_curseur("abc", 3), 3);
        // Un accent tient sur une colonne : comme le caractère, aucune
        // différence — c'est le cas de référence.
        assert_eq!(colonne_curseur("é", 1), 1);
        // Caractère large : deux colonnes pour un seul caractère.
        assert_eq!(colonne_curseur("あ", 1), 2);
        assert_eq!(colonne_curseur("aあ", 2), 3);
      }

    /// Le curseur du formulaire suit la même règle que le helper.
    ///
    /// « あ » fait un seul caractère pour deux colonnes : il doit donc placer
    /// le curseur exactement où le place « ab », deux caractères mais deux
    /// colonnes. Compter les caractères le mettait une colonne trop tôt.
    #[test]
    fn le_curseur_du_formulaire_tient_compte_des_caracteres_larges()
      {
        let (_, (x_deux_caracteres, _)) = rendre_creation(&saisie_en_cours("ab", 2));
        let (_, (x_un_caractere_large, _)) = rendre_creation(&saisie_en_cours("あ", 1));
        assert_eq!(
            x_un_caractere_large, x_deux_caracteres,
            "le curseur doit suivre les colonnes affichées, pas les caractères"
        );
      }

/// Règle centrale, testée isolément : la frappe prime sur la valeur
    /// validée, et le placeholder ne sort que si les deux sont vides.
    #[test]
    fn la_frappe_prime_sur_la_valeur_validee()
      {
        // En saisie : le buffer gagne.
        assert_eq!(valeur_affiche("ancien", "nouveau", true, VIDE), "nouveau");
        // Hors saisie : la valeur validée reste.
        assert_eq!(valeur_affiche("ancien", "nouveau", false, VIDE), "ancien");
        // En saisie, buffer vidé : le champ est en cours d'effacement, on
        // montre le placeholder — pas l'ancienne valeur, qui ferait croire
        // que le texte est encore là alors qu'Entrée l'écraserait par vide.
        assert_eq!(valeur_affiche("ancien", "", true, VIDE), VIDE);
        // Rien du tout : placeholder.
        assert_eq!(valeur_affiche("", "", true, VIDE), VIDE);
        assert_eq!(valeur_affiche("", "", true, "(aucun)"), "(aucun)");
        // Un contenu fait d'espaces reste considéré vide.
        assert_eq!(valeur_affiche("   ", "", false, VIDE), VIDE);
      }

    /// Le champ Statut n'affiche `input_buffer` qu'en texte libre : sinon la
    /// frappe alimente le filtre du menu déroulant, et l'afficher ici
    /// remplacerait le statut par son propre filtre.
    #[test]
    fn le_statut_n_affiche_le_buffer_quen_mode_texte_libre()
      {
        let mut state = saisie_en_cours("Beta", 4);
        state.active_field = 4;
        state.status = Some("Study".to_string());
        state.status_filter = "filtre".to_string();
        state.status_free_text = false;
        let (rendu, _) = rendre_creation(&state);
        assert!(rendu.contains("Study"), "le statut choisi doit rester visible :\n{rendu}");
        assert!(!rendu.contains("filtre"), "le filtre ne doit pas voler la ligne du statut :\n{rendu}");
      }
  }
