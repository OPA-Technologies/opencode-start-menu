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

    // L'aide occupe tout l'écran : la barre du bas y proposerait des gestes
    // qui ne s'appliquent pas, puisque seul `Échap` et `F1` y répondent.
    if matches!(input_mode, InputMode::Aide { .. })
      {
        render_banner(f, vertical_chunks[0]);
        render_aide(f, vertical_chunks[2], input_mode);
        return;
      }

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
    render_help(f, help_area, input_mode);

    // Creation dialog overlay
    if matches!(input_mode, InputMode::CreatingProject(_)) {
        render_creation_dialog(f, size, creation_state);
    }

    // Popup unifié : cadre, barre d'onglets et contenu de l'onglet actif.
    if let InputMode::Popup(popup) = input_mode {
        render_popup_unifie(f, size, app, popup);
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


/// Barre d'aide contextuelle : une ligne, les gestes du mode courant.
///
/// Elle est rendue depuis `keymap::contextuel`, jamais écrite à la main :
/// c'était la cause de l'aide mensongère (`e Modifier` et `t Valorisation`
/// n'ont jamais existé, les touches réelles n'étaient pas annoncées).
fn render_help(f: &mut Frame, area: Rect, input_mode: &InputMode)
  {
    let raccourcis = crate::keymap::contextuel(input_mode);
    // Si la liste ne contient pas `F1`, le suffixe qui y renvoie doit être
    // réservé d'emblée : sinon il déborde à son tour et disparaît, et la
    // barre finit par ne plus rien dire du tout.
    let suffixe = "F1 aide";
    // La place du suffixe est réservée d'emblée : c'est lui qui indique que
    // la ligne est incomplète. Sans cette marge, il déborde à son tour, et la
    // barre ne dirait plus du tout où trouver le reste.
    let separateur = " · ";
    let largeur_max = usize::from(area.width)
      .saturating_sub(suffixe.chars().count() + separateur.chars().count());
    let mut texte = String::new();
    let mut tronque = false;
    for r in raccourcis
      {
        let morceau = format!("{} {}", r.touche, r.libelle);
        let ajout = if texte.is_empty()
          {
            morceau.chars().count()
          }
          else
          {
            separateur.chars().count() + morceau.chars().count()
          };
        if texte.chars().count() + ajout > largeur_max
          {
            tronque = true;
            break;
          }
        if !texte.is_empty()
          {
            texte.push_str(separateur);
          }
        texte.push_str(&morceau);
      }
    // Ce qui n'a pas tenu sur la ligne n'est pas perdu : `F1` est annoncé pour
    // que l'utilisateur sache où le retrouver. Sans cela, la barre mentait par
    // silence sur ce qu'elle ne montrait pas.
    if tronque && !texte.contains("F1")
      {
        if !texte.is_empty()
          {
            texte.push_str(separateur);
          }
        texte.push_str(suffixe);
      }
    let paragraph = Paragraph::new(texte)
      .style(Style::default().fg(Color::DarkGray))
      .alignment(Alignment::Center);
    f.render_widget(paragraph, area);
  }

/// Écran d'aide `F10` : tous les raccourcis, groupés par écran.
///
/// `F1` n'affiche que `contextuel` ; `F10` affiche la totalité, ce qui évite
/// d'avoir à changer d'écran pour retrouver une touche oubliée. La barre du
/// bas et cet écran sont deux rendus du même tableau.
fn render_aide(f: &mut Frame, size: Rect, input_mode: &InputMode)
  {
    let (detaillee, scroll) = match input_mode
      {
        InputMode::Aide { detaillee, scroll, .. } => (*detaillee, *scroll),
        _ => (false, 0usize),
      };
    // Les gestes affichés sont ceux de l'écran d'origine, pas ceux de l'aide :
    // c'est ce que l'utilisateur est venu chercher.
    let contexte: Vec<crate::keymap::Raccourci> = match input_mode
      {
        InputMode::Aide { origine, .. } => crate::keymap::contextuel(origine),
        _ => Vec::new(),
      };
    let titre = if detaillee { " Aide — tous les raccourcis " } else { " Aide " };
    let pairs: Vec<(String, String)> = if detaillee
      {
        crate::keymap::complet()
          .into_iter()
          .flat_map(|(groupe, raccourcis)| {
            raccourcis.into_iter().map(move |r| {
              (
                groupe.to_string(),
                format!("  {}  {}", r.touche, r.libelle),
              )
            })
          })
          .collect()
      }
    else
      {
        contexte
          .iter()
          .map(|r| (String::new(), format!("  {}  {}", r.touche, r.libelle)))
          .collect()
      };

    let largeur = size.width.min(84).max(24.min(size.width));
    // La fenêtre ne cherche plus à contenir la liste : `F10` en a plus qu'un
    // écran, et une aide tronquée sans défilement est une aide incomplète.
    let hauteur = size.height.saturating_sub(4).max(9.min(size.height)).min(40);
    let x = size.width.saturating_sub(largeur) / 2;
    let y = size.height.saturating_sub(hauteur) / 2;
    let area = Rect { x, y, width: largeur, height: hauteur }.intersection(size);
    if area.width < 8 || area.height < 5
      {
        return;
      }
    f.render_widget(Clear, area);

    let bloc = Block::default()
      .title(titre)
      .borders(Borders::ALL)
      .border_style(Style::default().fg(Color::Cyan))
      .style(Style::default().bg(Color::Black));
    let inner = bloc.inner(area);
    f.render_widget(bloc, area);

    let mut lignes: Vec<Line> = Vec::new();
    let mut dernier_groupe: Option<String> = None;
    for (groupe, texte) in pairs
      {
        if !groupe.is_empty() && dernier_groupe.as_deref() != Some(groupe.as_str())
          {
            lignes.push(Line::from(Span::styled(
              groupe.clone(),
              Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )));
          }
        dernier_groupe = Some(groupe);
        lignes.push(Line::from(Span::raw(texte)));
      }
    let zones = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Min(0), Constraint::Length(1)])
      .split(inner);
    // Borné au derniere ligne affichable : `Fin` pousse le défilement à
    // l'infini, comme pour le tableau des coûts.
    let visibles = usize::from(zones[0].height);
    let total = lignes.len();
    let dernier = total.saturating_sub(visibles);
    let offset = scroll.min(dernier);
    f.render_widget(Paragraph::new(lignes).scroll((offset as u16, 0)), zones[0]);

    let defilement = if dernier > 0
      {
        format!(
          " ↑/↓ défiler ({}/{})  ·  ",
          offset + 1,
          total.saturating_sub(1)
        )
      }
      else
      {
        String::new()
      };
    let pied = Paragraph::new(format!(
      "{defilement}F1 : aide contextuelle  ·  F10 : tous les raccourcis  ·  Échap : fermer "
    ))
    .style(Style::default().fg(Color::DarkGray))
    .alignment(Alignment::Center);
    f.render_widget(pied, zones[1]);
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
    while lines.len() < area.height as usize
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
        let px = (area.x + 3).min(area.x.saturating_add(area.width).saturating_sub(width as u16));
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

/// Contenu de l'onglet Projet : version, statut, description.
///
/// Rendu dans `area` sans cadre ni centrage : le cadre commun et le titre
/// (qui porte le nom du projet) sont dessinés par `render_popup_unifie`.
fn render_contenu_projet(f: &mut Frame, area: Rect, state: &ProjectEditState)
  {
    let opaque = Style::default().bg(Color::Black);

    let block = Block::default()
      .borders(Borders::NONE)
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
    while lines.len() < area.height as usize
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
        // Colonne de début de valeur = libellé réel + « : », puis largeur
        // rendue du préfixe saisi (pas son nombre de caractères).
        let value_x = area.x + labels[active].0.chars().count() as u16 + 3;
        let dans_champ = colonne_curseur(&state.input_buffer, state.input_offset);
        let cursor_x =
          (value_x + dans_champ as u16).min(area.x.saturating_add(area.width).saturating_sub(1));
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
        let px = (area.x + 3).min(area.x.saturating_add(area.width).saturating_sub(width as u16));
        let py = area.y + 4; // sous la ligne du champ Statut (row = area.y+3)
        let parea = Rect {
            x: px,
            y: py,
            width: width as u16,
            height: (visible + 2) as u16,
          };
        let parea = parea.intersection(area);
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

/// Contenu de l'onglet Paramètres : préférences globales, hors projet.
///
/// La logique clavier existait déjà (`handle_parametres_input`) mais aucun
/// rendu n'existait : l'onglet était donc invisible, ce qui rendait `p` et `c`
/// cul-de-sac puisque Tab n'y menait pas.
fn render_contenu_parametres(f: &mut Frame, area: Rect, app: &App, popup: &crate::Popup)
  {
    let etat = &popup.parametres;
    let libelle = Style::default().fg(Color::DarkGray);
    let actif_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);

    let mut lignes: Vec<Line> = vec![Line::from("")];

    // (libellé, valeur, ligne de l'option)
    let tri = match app.prefs.tri
      {
        crate::registry::Tri::Date => "par date",
        crate::registry::Tri::Nom => "alphabétique",
      };
    let options: [(&str, String); 5] = [
      ("Tri de la liste", tri.to_string()),
      (
        "Colonnes visibles",
        format!("{}/{}", app.prefs.colonnes.len(), crate::registry::TOUTES_COLONNES.len()),
      ),
      (
        "Lancement root",
        if app.prefs.launch_root { "activé (sudo -i)" } else { "désactivé" }.to_string(),
      ),
      (
        "Capture du wrapper",
        if app.prefs.capture { "activée" } else { "désactivée" }.to_string(),
      ),
      ("Pays de la source", app.web_cache.country.clone()),
    ];

    for (i, (lbl, valeur)) in options.iter().enumerate()
      {
        let actif = i == etat.actif;
        let style = if actif { actif_style } else { Style::default() };
        // L'option « Colonnes visibles » a une sous-liste : le libellé montre
        // quelle colonne est visée, et les colonnes actives sont listées.
        let mut spans = vec![
          Span::styled(
            if actif { format!(" ▸ {lbl}") } else { format!("   {lbl}") },
            style,
          ),
        ];
        if i == 1
          {
            let nom = crate::registry::TOUTES_COLONNES
              .get(etat.colonne_focus)
              .copied()
              .unwrap_or("—");
            let marque = if app.prefs.colonnes.iter().any(|c| c == nom) { "☑" } else { "☐" };
            spans.push(Span::styled(
              format!("  ←→ {nom} {marque}"),
              if actif { actif_style } else { libelle },
            ));
          }
        else
          {
            spans.push(Span::styled(format!("  : {valeur}"), style));
          }
        // En saisie du code pays, la frappe remplace la valeur.
        if i == 4 && etat.edition
          {
            spans.push(Span::styled(
              etat.tampon.clone(),
              Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
          }
        lignes.push(Line::from(spans));
      }

    // Liste des colonnes activées, pour que l'effet du réglage soit visible
    // sans les ouvrir une par une.
    lignes.push(Line::from(""));
    lignes.push(Line::from(Span::styled(" Colonnes affichées dans le détail", libelle)));
    let colonnes: Vec<String> = crate::registry::TOUTES_COLONNES
      .iter()
      .map(|c| {
        let actif = app.prefs.colonnes.iter().any(|x| x == c);
        if actif { format!("☑ {c}") } else { format!("☐ {c}") }
      })
      .collect();
    for ligne in colonnes.chunks(3)
      {
        let mut spans: Vec<Span> = Vec::new();
        for c in ligne
          {
            spans.push(Span::raw(format!("{c}   ")));
          }
        lignes.push(Line::from(spans));
      }

    lignes.push(Line::from(""));
    lignes.push(Line::from(Span::styled(
      " Entrée basculer  ·  ↑/↓ option  ·  ←→ colonne  ·  Échap fermer ",
      libelle,
    )));

    if let Some(msg) = etat.message.as_deref()
      {
        lignes.push(Line::from(Span::styled(
          msg.to_string(),
          Style::default().fg(Color::Yellow),
        )));
      }

    f.render_widget(Paragraph::new(lignes).wrap(Wrap { trim: false }), area);
  }

/// Point d'entrée unique du popup : cadre, barre d'onglets, puis le contenu
/// de l'onglet actif.
///
/// Une seule implémentation dessert les trois onglets. C'était la cause des
/// trois symptômes « touche pressée, rien ne change » : l'ancien code ne
/// rendait que l'onglet Projet, et la navigation d'onglet n'était câblée que
/// dans le gestionnaire de l'onglet Paramètres — donc `p` et `c` ouvraient
/// un cul-de-sac dont on ne pouvait plus sortir.
pub(crate) fn render_popup_unifie(f: &mut Frame, size: Rect, app: &App, popup: &crate::Popup)
  {
    let largeur = size.width.min(100).max(40.min(size.width));
    let hauteur = size.height.saturating_sub(4).max(9.min(size.height));
    let x = (size.width.saturating_sub(largeur)) / 2;
    let y = (size.height.saturating_sub(hauteur)) / 2;
    let area = Rect { x, y, width: largeur, height: hauteur }.intersection(size);
    if area.width < 8 || area.height < 5
      {
        return;
      }

    // Fond opaque : efface la zone puis repeint, pour ne pas laisser transparaître
    // le menu en dessous.
    f.render_widget(Clear, area);
    f.render_widget(Block::default().style(Style::default().bg(Color::Black)), area);

    let bloc = Block::default()
      .title(format!(" {} ", crate::sanitize(&titre_popup(app, popup))))
      .borders(Borders::ALL)
      .border_style(Style::default().fg(Color::Cyan))
      .style(Style::default().bg(Color::Black));
    let inner = bloc.inner(area);
    f.render_widget(bloc, area);

    // Barre d'onglets : c'est elle qui rend la structure visible. Sans elle,
    // rien n'indiquait que le popup en avait trois, ni lequel était actif.
    let onglets = [crate::Onglet::Projet, crate::Onglet::Couts, crate::Onglet::Parametres];
    let libelles: Vec<String> = onglets
      .iter()
      .map(|o| {
        if *o == popup.onglet {
          format!(" ▸ {} ", o.libelle())
        } else {
          format!("   {} ", o.libelle())
        }
      })
      .collect();
    let tabs = Paragraph::new(Line::from(
        libelles.into_iter().map(Span::raw).collect::<Vec<Span>>(),
    ))
      .style(Style::default().fg(Color::Cyan).bg(Color::Black));
    let zones = Layout::default()
      .direction(Direction::Vertical)
      .constraints([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
      ])
      .split(inner);
    f.render_widget(tabs, zones[0]);

    let contenu = zones[1];
    // Pied commun aux trois onglets : la navigation d'onglets est globale, elle
    // doit donc être annoncée une fois pour tous, pas onglet par onglet.
    let pied = Paragraph::new(" Tab / ⇧Tab : onglets  ·  Échap : fermer ")
      .style(Style::default().fg(Color::DarkGray))
      .alignment(Alignment::Center);
    f.render_widget(pied, zones[2]);
    // Projet et Coûts portent sur un projet précis : sans sélection, on le dit
    // plutôt que d'afficher les valeurs d'un autre projet, ou un vide muet.
    let projet = app.projects.get(popup.projet_idx);
    let besoin_projet = popup.onglet != crate::Onglet::Parametres;
    if besoin_projet && projet.is_none()
      {
        f.render_widget(
          Paragraph::new("Aucun projet sélectionné.")
            .style(Style::default().fg(Color::Yellow).bg(Color::Black))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
          contenu,
        );
        return;
      }

    match popup.onglet
      {
        crate::Onglet::Projet => render_contenu_projet(f, contenu, &popup.projet),
        crate::Onglet::Couts => {
          if let Some(p) = projet
            {
              render_contenu_couts(f, contenu, app, p, &popup.couts);
            }
        }
        crate::Onglet::Parametres => render_contenu_parametres(f, contenu, app, popup),
      }
  }

/// Titre du popup : le projet quand il y en a un.
fn titre_popup(app: &App, popup: &crate::Popup) -> String
  {
    app.projects
      .get(popup.projet_idx)
      .map(|p| p.name.clone())
      .unwrap_or_else(|| "Paramètres".to_string())
  }

/// Contenu de l'onglet Coûts : valorisation + réglage du tarif, puis le
/// détail des coûts par session. Rendu dans `area` sans cadre propre : le
/// cadre et la barre d'onglets sont dessinés une seule fois par
/// `render_popup_unifie`.
///
/// Le défilement porte sur le tableau des sessions, seule partie réellement
/// plus longue que l'espace disponible.
fn render_contenu_couts(
    f: &mut Frame,
    area: Rect,
    app: &App,
    project: &crate::ProjectEntry,
    couts: &crate::EtatCouts,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Percentage(50),
        ])
        .split(area);
    render_bloc_tarif(f, chunks[0], app, project, couts);
    render_tableau_couts(f, chunks[1], project, couts.scroll_offset);
}

/// Tableau des coûts par session. Borné par la hauteur réellement
/// disponible : le défilement est calculé sur ce nombre de lignes, pas sur
/// une estimation. Sans ce calcul, `Fin` (qui pousse l'offset à l'infini)
/// vidait le tableau au lieu d'afficher sa dernière ligne.
fn render_tableau_couts(
    f: &mut Frame,
    area: Rect,
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

    // Défilement borné par la place réelle : une ligne de titre, puis autant
    // de lignes de données que la hauteur permet, moins une ligne de pied.
    let visibles = usize::from(area.height).saturating_sub(2).max(1);
    let max_scroll = rows.len().saturating_sub(1).saturating_sub(visibles);
    let actual_scroll = scroll_offset.min(max_scroll);

    // Take visible rows
    let display_rows: Vec<Row> = if rows.len() > 1 {
        let start = 1 + actual_scroll; // skip header
        let end = (start + visibles).min(rows.len());
        let mut display = vec![rows[0].clone()]; // header
        display.extend(rows[start..end].iter().cloned());
        display
    } else {
        rows.clone()
    };

    let block = Block::default()
      .title(" Détail des coûts ")
      .borders(Borders::TOP)
      .border_style(Style::default().fg(Color::DarkGray))
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
        "▲ {} ligne{}  │  ↑/↓ Ligne  PgUp/PgDn Page  Home/End Extrêmes",
        rows.len().saturating_sub(1),
        if rows.len().saturating_sub(1) > 1 { "s" } else { "" }
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

/// Bloc « valorisation du temps » : temps passé, valorisation en euros, puis
/// le détail du tarif appliqué (origine, année, date d'actualisation).
///
/// Les limites de la méthode sont explicitées : l'utilisateur doit savoir
/// qu'il s'agit d'une estimation fondée sur un salaire horaire moyen
/// publié, et non d'un coût réellement supporté. Rendu dans `area` sans
/// cadre : voir `render_contenu_couts`.
fn render_bloc_tarif(f: &mut Frame, area: Rect, app: &App, project: &crate::ProjectEntry, couts: &crate::EtatCouts) {
    let (active_field, editing, country) =
        (couts.active_field, couts.editing, couts.country.as_str());
    let (input_buffer, message, fetching) =
        (couts.input_buffer.as_str(), couts.message.as_deref(), couts.fetching);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

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

    // Valeur affichée pour le champ actif pendant la saisie : même règle que
    // les autres formulaires, via le helper commun.
    let champ = |i: usize, valeur: &str, actif: usize, buffer: &str, edit: bool| -> String {
        valeur_affiche(valeur, buffer, edit && actif == i, "—")
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
        " ↑/↓ : défiler  ·  ←/→ : champ  ·  Entrée : éditer  ·  r : rafraîchir ",
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
    // --- Popup unifié -------------------------------------------------

    /// Registre minimal avec un projet, chargé sans rescan ni réseau.
    fn app_de_test(nom: &str) -> (crate::App, std::path::PathBuf)
      {
        let dir = std::env::temp_dir().join(format!("op-ui-{}-{nom}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let projet = dir.join(nom);
        std::fs::create_dir_all(&projet).unwrap();
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                nom: {
                    "name": nom,
                    "path": projet.to_str().unwrap(),
                    "version": "1.0.0",
                    "version_debug": "1.0.0",
                    "description": "",
                    "tarif_mode": "manuel"
                }
            }
        });
        std::fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let app = crate::App::new_light(registre.clone(), crate::SortOrder::Modified);
        (app, dir)
      }

    /// Popup du projet 0, sur l'onglet demandé.
    fn popup_de_test(app: &crate::App, onglet: crate::Onglet) -> crate::Popup
      {
        let projet = crate::ProjectEditState::from_project(&app.projects[0], 0);
        crate::Popup::nouveau(0, projet, app.web_cache.country.clone()).avec_onglet(onglet)
      }

    fn rendre_popup(app: &crate::App, popup: &crate::Popup) -> String
      {
        let mut term = Terminal::new(TestBackend::new(110, 34)).unwrap();
        term.draw(|f| render_popup_unifie(f, f.area(), app, popup)).unwrap();
        texte(term.backend().buffer())
      }

    /// Régression B5 : `c` n'affichait rien, faute de rendu pour l'onglet Coûts.
    /// Les trois onglets doivent produire un contenu.
    #[test]
    fn chaque_onglet_du_popup_rend_un_contenu()
      {
        let (app, _dir) = app_de_test("projet-alpha");
        for onglet in
          [
            crate::Onglet::Projet,
            crate::Onglet::Couts,
            crate::Onglet::Parametres,
          ]
        {
            let popup = popup_de_test(&app, onglet);
            let rendu = rendre_popup(&app, &popup);
            for libelle in ["Projet", "Coûts", "Paramètres"]
              {
                assert!(
                    rendu.contains(libelle),
                    "onglet {:?} : la barre d'onglets doit montrer « {libelle} »",
                    onglet
                );
              }
            assert!(rendu.contains("▸ "), "onglet {:?} : aucun onglet actif marqué", onglet);
          }
      }

    /// Régression B5, plan visible : l'onglet Coûts montre le réglage du tarif
    /// *et* le tableau des coûts, dans un seul cadre.
    #[test]
    fn l_onglet_couts_montre_le_tarif_et_le_tableau()
      {
        let (mut app, _dir) = app_de_test("projet-beta");
        app.projects[0].last_duree = "1 h 23 min".to_string();
        app.projects[0].total_duree = "12 h 05 min".to_string();
        app.projects[0].total_cost = "4,20 €".to_string();
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let rendu = rendre_popup(&app, &popup);
        assert!(rendu.contains("Temps passé"), "le tableau de valorisation manque :\n{rendu}");
        assert!(rendu.contains("Tarif manuel"), "le réglage du tarif manque :\n{rendu}");
        assert!(rendu.contains("Pays source"), "le pays source manque :\n{rendu}");
        assert!(rendu.contains("Détail des coûts"), "le tableau des coûts manque :\n{rendu}");
      }

    /// Régression B3 : l'onglet Paramètres n'avait aucun rendu, alors que toute
    /// sa logique clavier existait. `p` et `c` ne pouvaient donc pas y mener.
    #[test]
    fn l_onglet_parametres_montre_les_preferences()
      {
        let (app, _dir) = app_de_test("projet-gamma");
        let popup = popup_de_test(&app, crate::Onglet::Parametres);
        let rendu = rendre_popup(&app, &popup);
        for attendu in
          [
            "Tri de la liste",
            "Colonnes visibles",
            "Lancement root",
            "Capture du wrapper",
            "Pays de la source",
          ]
        {
            assert!(rendu.contains(attendu), "« {attendu} » manque :\n{rendu}");
          }
      }

    /// Régression B9 : les limites de la méthode étaient revenues dans une
    /// fonction morte, jamais affichées. L'utilisateur lisait une valorisation
    /// prise pour un coût réel.
    #[test]
    fn les_avertissements_de_cout_sont_affiches()
      {
        let (mut app, _dir) = app_de_test("projet-delta");
        app.projects[0].tarif_mode = "web".to_string();
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let rendu = rendre_popup(&app, &popup);
        assert!(
            rendu.contains("Estimation, pas un coût"),
            "l'avertissement d'estimation doit être visible :\n{rendu}"
        );
      }

    /// Régression B7 : `Fin` poussait le défilement à l'infini sans borne, et le
    /// rendu ne le bornait pas non plus. Le tableau pouvait donc rendre vide.
    #[test]
    fn un_defilement_extreme_ne_vide_pas_le_tableau()
      {
        let (mut app, _dir) = app_de_test("projet-epsilon");
        app.projects[0].total_cost = "4,20 €".to_string();
        app.projects[0].session_count = "3".to_string();
        let mut popup = popup_de_test(&app, crate::Onglet::Couts);
        popup.couts.scroll_offset = usize::MAX / 2;
        let rendu = rendre_popup(&app, &popup);
        assert!(
            rendu.contains("TOTAL") || rendu.contains("Aucune donnée"),
            "le tableau doit rester lisible malgré un décalage hors bornes :\n{rendu}"
        );
      }

    /// Sans projet sélectionné, les onglets Projet et Coûts n'ont rien à
    /// montrer : ils doivent le dire plutôt qu'afficher le vide.
    #[test]
    fn sans_projet_l_onglet_annonce_l_absence()
      {
        let dir = std::env::temp_dir().join(format!("op-ui-vide-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let registre = dir.join("registre.json");
        std::fs::write(&registre, r#"{"projects":{}}"#).unwrap();
        let app = crate::App::new_light(registre.clone(), crate::SortOrder::Modified);
        let popup = crate::Popup::sans_projet(crate::Onglet::Couts, "FRA".to_string());
        let rendu = rendre_popup(&app, &popup);
        assert!(rendu.contains("Aucun projet sélectionné"), "rendu :\n{rendu}");
      }

    /// Régression B2 : l'aide affichait des touches que le code n'écoutait pas.
    /// Le pied commun annonce la navigation d'onglets, et le pied de l'onglet
    /// Coûts les gestes propres au tableau et aux champs.
    #[test]
    fn le_pied_annonce_la_navigation_d_onglets_dans_les_trois_onglets()
      {
        let (app, _dir) = app_de_test("projet-pied");
        for onglet in
          [
            crate::Onglet::Projet,
            crate::Onglet::Couts,
            crate::Onglet::Parametres,
          ]
        {
          let popup = popup_de_test(&app, onglet);
          let rendu = rendre_popup(&app, &popup);
          assert!(
            rendu.contains("Tab / ⇧Tab : onglets"),
            "onglet {:?} : le pied doit annoncer la navigation d'onglets :\n{rendu}",
            onglet
          );
        }
      }

    /// Le pied de l'onglet Coûts ne doit plus annoncer `Tab : onglets` : Tab
    /// change d'onglet partout, il ne cycle plus les champs tarifaires.
    #[test]
    fn le_pied_des_couts_annonce_le_defilement_et_les_champs()
      {
        let (app, _dir) = app_de_test("projet-couts-pied");
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let rendu = rendre_popup(&app, &popup);
        assert!(rendu.contains("↑/↓ : défiler"), "le défilement doit être annoncé :\n{rendu}");
        assert!(rendu.contains("←/→ : champ"), "les champs doivent être annoncés :\n{rendu}");
        // L'ancien pied annonçait `Tab : onglets` en plus de la bascule de
        // mode : il est remplacé, pas complété.
        assert!(
          !rendu.contains("bascule le mode"),
          "l'ancien pied de l'onglet Coûts doit avoir disparu :\n{rendu}"
        );
      }

    // --- Aide (F1 / F10) ------------------------------------------------

    fn rendre_ecran(mode: &InputMode, largeur: u16, hauteur: u16) -> String
      {
        // Répertoire numéroté : les tests tournent en parallèle et partageraient
        // sinon le même registre, que chacun supprime au début.
        static COMPTEUR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COMPTEUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (mut app, _dir) = app_de_test(&format!("projet-aide-{n}"));
        let mut term = Terminal::new(TestBackend::new(largeur, hauteur)).unwrap();
        term.draw(|f| super::ui(f, &mut app, mode, &ProjectCreationState::default())).unwrap();
        texte(term.backend().buffer())
      }

    /// Écran d'aide ouvert depuis la liste des projets : c'est le cas le plus
    /// courant, et il doit montrer les gestes de cet écran.
    fn aide_contextuelle() -> InputMode
      {
        crate::ouvrir_aide(InputMode::Normal, false)
      }

    fn aide_detaillee() -> InputMode
      {
        crate::ouvrir_aide(InputMode::Normal, true)
      }

    /// Les cinq écrans qui portent une barre d'aide contextuelle, avec un nom
    /// lisible : `InputMode` ne dérive pas de `Debug`.
    ///
    /// L'écran d'aide en est absent : il affiche ses propres gestes dans son
    /// pied, sinon la barre proposerait des raccourcis qui n'y marchent pas.
    fn ecrans_de_la_barre() -> Vec<(&'static str, InputMode)>
      {
        vec![
          ("liste", InputMode::Normal),
          (
            "module",
            InputMode::SelectModule { project_idx: 0, modules: vec![], selected: 0 },
          ),
          (
            "popup projet",
            InputMode::Popup(crate::Popup::sans_projet(crate::Onglet::Projet, "FRA".into())),
          ),
          (
            "popup couts",
            InputMode::Popup(crate::Popup::sans_projet(crate::Onglet::Couts, "FRA".into())),
          ),
          (
            "popup parametres",
            InputMode::Popup(crate::Popup::sans_projet(crate::Onglet::Parametres, "FRA".into())),
          ),
        ]
      }

    /// Régression B2 : l'aide annonçait `e Modifier` et `t Valorisation`, deux
    /// touches qui n'ont jamais été branchées. Un utilisateur qui cherchait
    /// `e` concluait à un programme cassé.
    #[test]
    fn la_barre_d_aide_n_annonce_plus_de_touches_inexistantes()
      {
        let rendu = rendre_ecran(&InputMode::Normal, 120, 40);
        for fantome in ["e Modifier", "t Valorisation"]
          {
            assert!(!rendu.contains(fantome), "« {fantome} » n'existe pas dans le code :\n{rendu}");
          }
        for reel in ["p", "c", "s", "m", "F1"]
          {
            assert!(rendu.contains(reel), "« {reel} » est une touche réelle, elle doit être annoncée");
          }
      }

    /// La barre du bas est rendue depuis le keymap : elle ne peut donc pas
    /// diverger de l'aide contextuelle.
    #[test]
    fn la_barre_d_aide_suit_le_keymap()
      {
        for (nom, mode) in ecrans_de_la_barre()
          {
            let attendus: Vec<String> = crate::keymap::contextuel(&mode)
              .iter()
              .map(|r| r.touche.to_string())
              .collect();
            let rendu = rendre_ecran(&mode, 200, 40);
            for touche in attendus
              {
                assert!(
                  rendu.contains(&touche),
                  "{nom} : la barre n'annonce pas « {touche} » :\n{rendu}"
                );
              }
          }
      }

    /// La barre ne doit jamais annoncer un geste que le keymap ignore : c'est
    /// l'inverse du test précédent, et c'est lui qui avait laissé passer les
    /// touches fantômes.
    #[test]
    fn la_barre_d_aide_est_derivee_du_keymap()
      {
        for (nom, mode) in ecrans_de_la_barre()
          {
            let touches: Vec<&str> =
              crate::keymap::contextuel(&mode).iter().map(|r| r.touche).collect();
            let rendu = rendre_ecran(&mode, 200, 40);
            for faux in ["e Modifier", "t Valorisation", "←/→ Changer onglet"]
              {
                assert!(
                  !rendu.contains(faux),
                  "{nom} : la barre affiche « {faux} », absent du keymap :\n{rendu}"
                );
              }
            assert!(
              touches.iter().any(|t| *t == "Tab/⇧Tab") || !rendu.contains("Onglet suivant"),
              "{nom} : la barre annonce un changement d'onglet sans le gesture correspondant"
            );
          }
      }

    /// F1 : aide contextuelle, titre et gestes du mode courant.
    #[test]
    fn f1_affiche_l_aide_contextuelle()
      {
        let rendu = rendre_ecran(&aide_contextuelle(), 100, 34);
        assert!(rendu.contains("Aide"), "le titre doit apparaître :\n{rendu}");
        assert!(rendu.contains("Ouvrir"), "les gestes du mode courant doivent apparaître :\n{rendu}");
        assert!(rendu.contains("Échap"), "la fermeture doit être annoncée :\n{rendu}");
      }

    /// F10 : tous les raccourcis, groupés par écran, y compris ceux qui ne
    /// s'appliquent pas au mode courant.
    #[test]
    fn f10_affiche_tous_les_raccourcis()
      {
        let rendu = rendre_ecran(&aide_detaillee(), 100, 60);
        for groupe in
          [
            "Liste des projets",
            "Nouveau projet",
            "Popup — onglet Projet",
            "Popup — onglet Coûts",
            "Popup — onglet Paramètres",
          ]
        {
          assert!(rendu.contains(groupe), "« {groupe} » manque dans F10 :\n{rendu}");
          }
        assert!(rendu.contains("Ouvrir un sous-module"), "un geste rare doit figurer dans F10");
      }

    /// Régression : F1 affichait les raccourcis de l'écran d'aide lui-même,
    /// donc un seul geste — « fermer ». L'utilisateur ouvrait l'aide depuis la
    /// liste et n'y trouvait pas `Entrée Ouvrir` : exactement l'inverse de ce
    /// qu'il était venu chercher.
    #[test]
    fn f1_affiche_les_gestes_de_l_ecran_d_origine()
      {
        let rendu = rendre_ecran(&aide_contextuelle(), 100, 34);
        for geste in ["Naviguer", "Ouvrir", "Réglages projet", "Coûts"]
          {
            assert!(rendu.contains(geste), "« {geste} » manque pour l'écran d'origine :\n{rendu}");
          }
        // Et les gestes propres à l'aide restent annoncés, dans son pied.
        assert!(rendu.contains("F10"), "le pied doit proposer la vue complète");
        assert!(rendu.contains("fermer"), "la fermeture doit être annoncée");
      }

    /// La barre d'aide doit tenir dans la largeur du terminal et continuer à
    /// pointer vers `F1` quand elle est tronquée : sinon elle ne dit plus
    /// où trouver ce qu'elle a dû couper.
    #[test]
    fn la_barre_d_aide_tient_dans_la_largeur_et_garde_f1()
      {
        for largeur in [80u16, 100, 120, 200]
          {
            let rendu = rendre_ecran(&InputMode::Normal, largeur, 40);
            let derniere = rendu.lines().last().unwrap_or("").to_string();
            assert!(
              derniere.chars().count() <= usize::from(largeur),
              "largeur {largeur} : la barre déborde ({} caractères)",
              derniere.chars().count()
            );
            assert!(
              derniere.contains("F1"),
              "largeur {largeur} : la barre tronquée doit pointer vers F1 :\n{derniere}"
            );
          }
      }

    /// `F10` liste plus de raccourcis qu'il n'y a de lignes : sans
    /// défilement, la fin de la liste serait inaccessible et l'aide
    /// mentirait par silence.
    #[test]
    fn f10_reste_defilable_jusqu_a_sa_fin()
      {
        let debut = aide_detaillee();
        let rendu_debut = rendre_ecran(&debut, 100, 40);
        // Le haut de la liste est visible, la fin ne l'est pas.
        assert!(rendu_debut.contains("Liste des projets"), "le début doit être visible");
        assert!(
          !rendu_debut.contains("Défiler la liste"),
          "la fin de la liste ne doit pas tenir dans l'écran :\\n{rendu_debut}"
        );

        // `Fin` pousse hors bornes : le rendu doit s'arrêter à la dernière
        // ligne utile, pas vider la liste.
        let fin = match aide_detaillee()
          {
            InputMode::Aide { origine, .. } => {
              InputMode::Aide { detaillee: true, origine, scroll: usize::MAX / 2 }
            }
            _ => unreachable!(),
          };
        let rendu_fin = rendre_ecran(&fin, 100, 40);
        assert!(rendu_fin.contains("défiler"), "la position doit être annoncée :\n{rendu_fin}");
        for dernier in ["Fermer", "Défiler la liste"]
          {
            assert!(rendu_fin.contains(dernier), "« {dernier} » doit rester atteignable :\n{rendu_fin}");
          }
      }

    /// Le défilement doit aussi pouvoir revenir en haut, sinon `Fin` est un
    /// cul-de-sac.
    #[test]
    fn le_defilement_de_l_aide_revient_en_haut()
      {
        let mode = match aide_detaillee()
          {
            InputMode::Aide { origine, .. } => {
              InputMode::Aide { detaillee: true, origine, scroll: 50 }
            }
            _ => unreachable!(),
          };
        let rendu = rendre_ecran(&mode, 100, 40);
        assert!(!rendu.contains("Liste des projets"), "on doit avoir défilé vers le bas");
        assert!(rendre_ecran(&aide_detaillee(), 100, 40).contains("Liste des projets"));
      }

  }
