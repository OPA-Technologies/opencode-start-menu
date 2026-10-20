use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, List, ListItem, Paragraph, Row, Table, Wrap},
    Frame,
};
use std::{borrow::Cow, path::PathBuf};

use crate::{sanitize, App, InputMode, ProjectCreationState, ProjectEditState};

/// Thème des popups du menu : fond noir, bordure cyan.
///
/// Le centrage, le fond opaque et le cadre viennent d'`op_tui` : ils étaient
/// réécrits dans chaque popup, avec des variantes qui laissaient passer le menu
/// du dessous.
const THEME: op_tui::theme::Popup = op_tui::theme::Popup::SOMBRE;

/// Le formulaire de création et le choix de module sont des dialogues plein
/// cadre posés sur du gris foncé, pas des popups.
const THEME_GRIS: op_tui::theme::Popup = op_tui::theme::Popup::GRIS;

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

    // Choix du type de la nouvelle entrée : Projet ou Bibliothèque.
    if let InputMode::ChoixNouveau { selected } = input_mode {
        render_choix_nouveau_dialog(f, size, *selected);
    }

    // Popup unifié : cadre, barre d'onglets et contenu de l'onglet actif.
    if let InputMode::Popup(popup) = input_mode {
        render_popup_unifie(f, size, app, popup);
    }

    // Modale d'examen des incohérences, et sa confirmation Release éventuelle.
    if let InputMode::Coherence(state) = input_mode {
        render_coherence_dialog(f, size, app, state);
        if let Some(idx) = state.confirmation_release {
            let lignes = crate::lignes_coherence(app);
            if let Some(ligne) = lignes.get(idx) {
                render_confirmation_release(f, size, ligne.action.libelle());
            }
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
            let mut spans = vec![Span::styled(
                format!("  > {}", sanitize(&p.name)),
                style,
            )];
            if p.est_bibliotheque {
                spans.push(Span::styled(
                    "  Bibliothèque",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ));
            }
            ListItem::new(Line::from(spans))
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
                .title(if app.vue_archives { " Archives " } else { " Projets " })
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        // `REVERSED` annule ici la barre blanche voulue : chaque ligne porte
        // déjà `bg(White) + fg(Black) + BOLD` quand elle est sélectionnée, et
        // l'inversion rend la ligne en blanc gras sur noir. La sélection reste
        // nette — le `►` et le gras suffisent — mais l'effet est à l'inverse de
        // l'intention, comme les `REVERSED` retirés des libellés de formulaire.
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED))
        .highlight_symbol("► ");

    f.render_stateful_widget(list, area, &mut app.list_state);
}

fn variant_box(
    title: &str,
    title_color: Color,
    version: &str,
    status: &str,
    date_declaree: &str,
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
            Span::styled("Date maj : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(&dash(date_declaree))),
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
            // « 4,20 € · 4,57 $ » quand la devise est convertible, le seul
            // montant facturé sinon. La conversion ne fait que s'ajouter : elle
            // disparaît si aucun taux n'est connu.
            crate::cout_session_double(
                &or_dash(&p.last_session_cost),
                &p.last_session_cost_local,
            ),
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

    let est_bibliotheque = app
        .selected_project_idx()
        .is_some_and(|idx| app.projects[idx].est_bibliotheque);

    let text: Vec<Line> = vec![
        // Groupe 1
        Line::from(vec![
            Span::styled("Nom          : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(&name)),
            if est_bibliotheque {
                Span::styled(
                    "  ·  Bibliothèque",
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("")
            },
        ]),
        Line::from(vec![
            Span::styled("ID           : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(&id)),
        ]),
        Line::from(vec![
            Span::styled("Chemin       : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(&path)),
        ]),
        sep(),
        // Groupe 2
        Line::from(vec![
            Span::styled("Description  : ", Style::default().fg(Color::DarkGray)),
            Span::raw(sanitize(&desc)),
        ]),
        sep(),
    ];

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
            Constraint::Length(7),
            Constraint::Length(11),
        ])
        .split(inner);
    let top_area = split[0];
    let variant_area = split[1];
    let table_area = split[2];

    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    f.render_widget(paragraph, top_area);

    // Groupe 3 : variantes Debug | Release (Release en lecture seule).
    // Le binaire fait foi (fiche de build alignée sur le dernier build), la
    // base du registre s'ajoute en complément quand les deux divergent.
    // Aucune sonde ni lecture disque ici : le rendu d'une image n'exécute rien.
    let (debug_version, debug_status, debug_date, release_version, release_status, release_date) =
        if let Some(idx) = app.selected_project_idx() {
            let p = &app.projects[idx];
            let variante =
                |profil: &str, base_v: &str, base_s: &str, base_d: &str| -> (String, String, String) {
                    let fiche = crate::fiche_pour_affichage(p, profil);
                    let libelle = crate::libelle_bifilaire(
                        fiche.as_ref().map(|(v, s, d)| (v.as_str(), s.as_str(), d.as_str())),
                        (base_v, base_s),
                    );
                    let statut = fiche
                        .as_ref()
                        .map(|(_, s, _)| s.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| base_s.to_string());
                    let date = fiche
                        .as_ref()
                        .map(|(_, _, d)| d.clone())
                        .filter(|d| !d.is_empty())
                        .unwrap_or_else(|| base_d.to_string());
                    (libelle, statut, date)
                };
            let (dv, ds, dd) = variante("debug", &p.version_debug, &p.status_debug, &p.date_debug);
            let (rv, rs, rd) =
                variante("release", &p.version_release, &p.status_release, &p.date_release);
            (dv, ds, dd, rv, rs, rd)
        } else {
            (
                "—".to_string(),
                "—".to_string(),
                "—".to_string(),
                "—".to_string(),
                "—".to_string(),
                "—".to_string(),
            )
        };
    let var_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(variant_area);
    f.render_widget(
        variant_box(" Debug ", Color::Cyan, &debug_version, &debug_status, &debug_date, &debug_modified),
        var_chunks[0],
    );
    f.render_widget(
        variant_box(" Release ", Color::Green, &release_version, &release_status, &release_date, &release_modified),
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
/// Elle est rendue depuis le keymap via `op_tui::help::barre`, jamais écrite à
/// la main : c'était la cause de l'aide mensongère (`e Modifier` et
/// `t Valorisation` n'ont jamais existé, les touches réelles n'étaient pas
/// annoncées).
fn render_help(f: &mut Frame, area: Rect, input_mode: &InputMode)
  {
    op_tui::help::barre(
      f,
      area,
      &crate::keymap::Aide(Some(input_mode)),
      "F1 aide",
      Style::default().fg(Color::DarkGray),
    );
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
    let cible = crate::keymap::Aide(match input_mode
      {
        InputMode::Aide { origine, .. } => Some(origine.as_ref()),
        _ => None,
      });
    // `overlay::cadre` espace lui-même le titre : le passer déjà encadré
    // produirait deux espaces de chaque côté.
    let titre = if detaillee { "Aide — tous les raccourcis" } else { "Aide" };
    let largeur = size.width.min(84).max(24.min(size.width));
    // La fenêtre ne cherche plus à contenir la liste : `F10` en a plus qu'un
    // écran, et une aide tronquée sans défilement est une aide incomplète.
    let hauteur = size.height.saturating_sub(4).max(9.min(size.height)).min(48);
    let Some(area) = op_tui::layout::popup_rect(size, largeur, hauteur) else {
        return;
    };
    let inner = op_tui::overlay::cadre(f, area, titre, THEME);

    let lignes: Vec<Line> = op_tui::help::lignes(
      &cible,
      detaillee,
      Style::default(),
      Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    );
    let zones = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Min(0), Constraint::Length(1)])
      .split(inner);
    // Borné au derniere ligne affichable : `Fin` pousse le défilement à
    // l'infini, comme pour le tableau des coûts.
    let visibles = usize::from(zones[0].height);
    let total = lignes.len();
    let dernier = total.saturating_sub(visibles);
    let offset = op_tui::viewport::borner(scroll, total, visibles);
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

use op_tui::field::{colonne_curseur, valeur_affiche, VIDE};

fn render_choix_nouveau_dialog(f: &mut Frame, size: Rect, selected: usize)
  {
    let Some(area) = op_tui::layout::popup_rect(size, 60, 8) else {
        return;
    };
    f.render_widget(Clear, area);
    let opaque = Style::default().bg(THEME_GRIS.fond);

    let block = Block::default()
      .title(" Nouveau ")
      .borders(Borders::ALL)
      .border_style(Style::default().fg(Color::Cyan))
      .style(opaque);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let options = ["Projet", "Bibliothèque"];
    let surligne = Style::default()
      .bg(Color::White)
      .fg(Color::Black)
      .add_modifier(Modifier::BOLD);
    let mut lignes: Vec<Line> = options
      .iter()
      .enumerate()
      .map(|(i, opt)| {
        Line::from(Span::styled(format!("  > {opt}"), if i == selected { surligne } else { Style::default() }))
      })
      .collect();
    lignes.push(Line::from(""));
    lignes.push(Line::from(vec![
      Span::styled("Un projet garde son dossier sous la racine des projets ;", Style::default().fg(Color::DarkGray)),
    ]));
    lignes.push(Line::from(vec![
      Span::styled("une bibliothèque de code tombe sous la racine des bibliothèques.", Style::default().fg(Color::DarkGray)),
    ]));

    let zones = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Min(0), Constraint::Length(1)])
      .split(inner);
    f.render_widget(Paragraph::new(lignes), zones[0]);
    f.render_widget(
      Paragraph::new(Line::from(Span::styled(
        "↑/↓ choisir · Entrée valider · Échap annuler",
        Style::default().fg(Color::DarkGray),
      ))).alignment(Alignment::Center),
      zones[1],
    );
  }

fn render_creation_dialog(f: &mut Frame, size: Rect, state: &ProjectCreationState)
  {
    let dialog_width = size.width.min(76);
    let Some(area) = op_tui::layout::popup_rect(size, dialog_width, 15) else {
        return;
    };

    // Fond opaque : efface la zone puis repeint tout en gris foncé
    f.render_widget(Clear, area);

    let opaque = Style::default().bg(THEME_GRIS.fond);

    let block = Block::default()
      .title(if state.bibliotheque { " Nouvelle bibliothèque " } else { " Nouveau projet " })
      .borders(Borders::ALL)
      .border_style(Style::default().fg(Color::Cyan))
      .style(opaque);

// Lignes : libellé aligné + valeur. Le champ actif est surligné.
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

    // Le libellé du champ actif est le repère de navigation : noir sur cyan. Sans
    // `REVERSED`, qui échange les deux plans à l'affichage et annulait donc le
    // surlignage. Ce formulaire étant posé sur un fond gris, le champ actif
    // restait repérable par son fond noir — le défaut y était un style jamais
    // affiché. Dans l'éditeur, sur fond noir, la même inversion rendait le champ
    // actif strictement identique aux inactifs.
    let lbl_style = |i: usize| -> Style
      {
        if i == active
          {
            Style::default().fg(Color::Black).bg(Color::Cyan)
          }
        else
          {
            Style::default().fg(Color::Cyan)
          }
      };

    // Ligne d'un champ simple : libellé + « : » + valeur, une seule rangée.
    let ligne_champ = |i: usize| -> Line<'static>
      {
        Line::from(vec![
            Span::styled(fields[i].0, lbl_style(i)),
            Span::styled(" : ", Style::default().fg(Color::DarkGray)),
            Span::styled(fields[i].1.clone(), Style::default()),
        ])
      };

    // La Description occupe une zone dédiée de hauteur fixe, repliée au mot :
    // lue en entier, elle ne pousse jamais Statut, Version ni « Ouvrir ? », qui
    // gardent leur rangée. `3` = libellé + deux lignes de continuation.
    const DESC_H: usize = 3;
    let largeur = area.width.saturating_sub(2) as usize; // intérieur du cadre
    let prefixe = format!("{} : ", fields[3].0);
    let (repliees, positions) =
      replier_texte(&format!("{prefixe}{}", fields[3].1), largeur);
    let prefixe_chars = prefixe.chars().count();
    let curseur_repli = if active == 3
      {
        let idx = (prefixe_chars + state.input_offset).min(positions.len().saturating_sub(1));
        Some(positions[idx])
      }
      else
        {
          None
        };
    // Défilement automatique : la zone s'ouvre en haut et ne glisse qu'autant
    // qu'il faut pour garder le curseur visible.
    let scroll = match curseur_repli
      {
        Some((cr, _)) if cr >= DESC_H => cr + 1 - DESC_H,
        _ => 0,
      };
    let mut lignes_desc: Vec<Line<'static>> = Vec::with_capacity(DESC_H);
    for li in scroll..scroll + DESC_H
      {
        let ligne = match (li, repliees.get(li))
          {
            (0, Some(txt)) =>
              {
                let coupe = prefixe_chars.min(txt.chars().count());
                let (tete, queue): (String, String) = txt.chars().enumerate().fold(
                  (String::new(), String::new()),
                  |(mut tete, mut queue), (k, c)| {
                    if k < coupe
                      {
                        tete.push(c);
                      }
                    else
                      {
                        queue.push(c);
                      }
                    (tete, queue)
                  },
                );
                Line::from(vec![
                    Span::styled(tete, lbl_style(3)),
                    Span::styled(queue, Style::default()),
                ])
              }
            (_, Some(txt)) => Line::from(Span::styled(txt.clone(), Style::default())),
            (_, None) => Line::from(""),
          };
        lignes_desc.push(ligne);
      }

    // Rangée intérieure d'un champ simple : ID/Nom/Chemin en tête (index 2,
    // 3, 4), puis Statut, Version et « Ouvrir ? » reconduits sous la zone.
    let rangee_champ = |i: usize| -> usize
      {
        if i < 3
          {
            2 + i
          }
        else
          {
            1 + DESC_H + i
          }
      };

    let mut lines: Vec<Line<'static>> = vec![Line::from(""), Line::from("")];
    lines.push(ligne_champ(0));
    lines.push(ligne_champ(1));
    lines.push(ligne_champ(2));
    lines.extend(lignes_desc);
    lines.push(ligne_champ(4));
    lines.push(ligne_champ(5));

    // Ligne de confirmation finale o/N (toujours visible, actif si active_field == 6).
    // L'indication passe en gras quand elle est active : elle cesse d'être un
    // simple rappel et devient l'action que la touche Entrée va déclencher.
    let confirm_lbl = Span::styled("Ouvrir ?     ", lbl_style(6));
    let confirm_style = if active == 6
      {
        Style::default().add_modifier(Modifier::BOLD)
      }
    else
      {
        Style::default()
      };
    lines.push(Line::from(vec![
        confirm_lbl,
        Span::styled(" : ", Style::default().fg(Color::DarkGray)),
        Span::styled("o/N", confirm_style),
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
          "Échap annuler · ↑/↓ champ · ←/→ curseur · Ctrl+←/→ mots"
        }
    )
    .centered());

    // Remplit les lignes restantes pour un fond entièrement opaque
    while lines.len() < area.height as usize
      {
        lines.push(Line::from(""));
      }

    // Rendu sans retour automatique : chaque champ occupe une rangée connue, ce
    // qui est la condition pour que le curseur et le menu Statut se posent au
    // bon endroit. La description est déjà repliée avec le même compte.
    let paragraph = Paragraph::new(lines).block(block).style(opaque);
    f.render_widget(paragraph, area);

    // Curseur réel : dans la zone repliée pour la Description, sur la rangée du
    // champ sinon. La colonne de début de valeur = bordure + libellé + « : »,
    // plus la largeur rendue du préfixe saisi (pas ses caractères : sinon les
    // accents décalent le curseur).
    let (cursor_x, cursor_y) = if active == 3
      {
        let (cr, cc) = curseur_repli.unwrap_or((0, 0));
        (area.x + 1 + cc as u16, area.y + 1 + (2 + 3 + cr - scroll) as u16)
      }
    else
      {
        let dans_champ = if active == 4 && !state.status_free_text
          {
            0
          }
          else
            {
              colonne_curseur(&state.input_buffer, state.input_offset)
            };
        (
          area.x + 1 + fields.get(active).map_or(13, |f| f.0.chars().count()) as u16 + 3
            + dans_champ as u16,
          area.y + 1 + rangee_champ(active) as u16,
        )
      };
    let cursor_x = cursor_x.min(area.x.saturating_add(area.width).saturating_sub(2));
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
        // Sous la ligne du champ Statut (rangée intérieure `5 + DESC_H`, soit
        // `area.y + 1 + 5 + DESC_H` sur l'écran).
        let py = area.y + 2 + (5 + DESC_H) as u16;
        let parea = Rect {
            x: px,
            y: py,
            width: width as u16,
            height: (visible + 2) as u16,
          };
        let parea = parea.intersection(size);
        f.render_widget(Clear, parea);
        let mut popup: Vec<Line> = Vec::new();
        let items_visibles = op_tui::viewport::fenetre(&items, offset, visible);
        for (decalage, item) in items_visibles.iter().enumerate() {
            let idx = decalage + offset;
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
fn render_contenu_projet(
    f: &mut Frame,
    area: Rect,
    state: &ProjectEditState,
    projet: Option<&crate::ProjectEntry>,
  )
  {
    let opaque = Style::default().bg(Color::Black);

    let block = Block::default()
      .borders(Borders::NONE)
      .style(opaque);

    let active = state.active_field.min(8);
    // Même règle que le formulaire de création, via le helper commun : la
    // frappe en cours l'emporte sur la valeur validée.
    let current_value = |state: &ProjectEditState, field: usize| -> String
      {
        let valide = match field
          {
            0 => state.pending_version.as_str(),
            1 => state.pending_status.as_str(),
            2 => state.outils.probe_debug.as_str(),
            3 => state.outils.build_debug.as_str(),
            4 => state.outils.probe_release.as_str(),
            5 => state.outils.build_release.as_str(),
            6 => state.pending_description.as_str(),
            7 => state.pending_typologie.as_str(),
            _ => "o/N",
          };
        let placeholder = match field
          {
            1 => "(aucun)",
            2..=5 => "(auto)",
            _ => VIDE,
          };
        valeur_affiche(
            valide,
            &state.input_buffer,
            state.editing && field == active,
            placeholder,
          )
      };

    // Même repère que le formulaire de création, et pour la même raison : le
    // libellé est surligné sans `REVERSED`, dont la vidéo inversée annulerait
    // les deux couleurs et rendrait le champ actif indiscernable.
    let lbl_style = |i: usize| -> Style
      {
        if i == active
          {
            Style::default().fg(Color::Black).bg(Color::Cyan)
          }
        else
          {
            Style::default().fg(Color::Cyan)
          }
      };

    let labels = [
        ("Version Debug ", "1.2.3 (format x.y.z)"),
        ("Statut Debug  ", "Study · Alpha · Beta · Debug · Beta-Release · Release · Stable · Deprecated"),
        ("Sonde Debug   ", "commande -V (vide : sonde auto)"),
        ("Build Debug   ", "commande de build (vide : Cargo)"),
        ("Sonde Release ", "commande -V (vide : sonde auto)"),
        ("Build Release ", "commande de build (vide : Cargo)"),
        ("Description   ", "texte libre non vide"),
        ("Typologie     ", "Code-App · Code-Lib · Doc-Lib · Doc-office · Other"),
        ("Enregistrer ? ", "Variante Debug seule enregistrée"),
      ];
    let vals = [
        current_value(state, 0),
        current_value(state, 1),
        current_value(state, 2),
        current_value(state, 3),
        current_value(state, 4),
        current_value(state, 5),
        current_value(state, 6),
        current_value(state, 7),
      ];

    // Version et Statut : une rangée chacun, sous une rangée vide d'amorce.
    let ligne_champ = |i: usize| -> Line<'static>
      {
        Line::from(vec![
            Span::styled(labels[i].0, lbl_style(i)),
            Span::styled(" : ", Style::default().fg(Color::DarkGray)),
            Span::styled(vals[i].clone(), Style::default()),
        ])
      };

    // Rappel en lecture seule de la vérité binaire (fiche de build cohérente) :
    // montre à l'utilisateur ce qui est réellement compilé, et l'écart avec la
    // base qu'il édite. Une ligne par variante : Debug (base = valeurs en cours
    // d'édition) et Release (base = valeurs persistées, non éditables ici).
    let ligne_binaire = |profil: &str, base_v: &str, base_s: &str, etiquette: &str| -> Line<'static>
      {
        let libelle = projet
            .map(|p| {
                let fiche = crate::fiche_pour_affichage(p, profil);
                crate::libelle_bifilaire(
                    fiche.as_ref().map(|(v, s, d)| (v.as_str(), s.as_str(), d.as_str())),
                    (base_v, base_s),
                )
            })
            .unwrap_or_else(|| "—".to_string());
        Line::from(vec![
            Span::styled(etiquette.to_string(), Style::default().fg(Color::DarkGray)),
            Span::styled(" : ", Style::default().fg(Color::DarkGray)),
            Span::styled(sanitize(&libelle), Style::default().fg(Color::DarkGray)),
        ])
      };
    let ligne_binaire_debug =
        ligne_binaire("debug", &state.pending_version, &state.pending_status, "Binaire D");
    let ligne_binaire_release = ligne_binaire(
        "release",
        projet.map(|p| p.version_release.as_str()).unwrap_or(""),
        projet.map(|p| p.status_release.as_str()).unwrap_or(""),
        "Binaire R",
    );

    // Bloc bas de page à position fixe : la description défilante, au-dessus,
    // ne peut donc jamais décaler « Enregistrer ? » ni les touches. Neuf
    // rangées en tête (vide + Version + Statut + 4 outils + 2 binaires), quatre
    // en pied.
    let hauteur = area.height as usize;
    let zone_haut = 10usize;
    let zone_bas = 4usize;
    let zone_h = hauteur.saturating_sub(zone_haut + zone_bas).max(1);

    // Description repliée au mot dans sa zone dédiée : lue en entier, elle ne
    // pousse plus les rangées suivantes. Le repli et le curseur partagent le
    // même calcul, donc restent bord à bord.
    let prefixe = format!("{} : ", labels[6].0);
    let (repliees, positions) =
      replier_texte(&format!("{prefixe}{}", vals[6]), area.width as usize);
    let prefixe_chars = prefixe.chars().count();
    let curseur_repli = if state.editing && active == 6
      {
        let idx = (prefixe_chars + state.input_offset).min(positions.len().saturating_sub(1));
        Some(positions[idx])
      }
      else
        {
          None
        };
    // Défilement automatique : la zone s'ouvre en haut et ne glisse qu'autant
    // qu'il faut pour garder le curseur visible pendant l'édition.
    let scroll = match curseur_repli
      {
        Some((cr, _)) if cr >= zone_h => cr + 1 - zone_h,
        _ => 0,
      };

    let mut lignes_zone: Vec<Line<'static>> = Vec::with_capacity(zone_h);
    for li in scroll..scroll + zone_h
      {
        let ligne = match (li, repliees.get(li))
          {
            (0, Some(txt)) =>
              {
                let coupe = prefixe_chars.min(txt.chars().count());
                let (tete, queue): (String, String) = txt.chars().enumerate().fold(
                  (String::new(), String::new()),
                  |(mut tete, mut queue), (k, c)| {
                    if k < coupe
                      {
                        tete.push(c);
                      }
                    else
                      {
                        queue.push(c);
                      }
                    (tete, queue)
                  },
                );
                Line::from(vec![
                    Span::styled(tete, lbl_style(6)),
                    Span::styled(queue, Style::default()),
                ])
              }
            (_, Some(txt)) => Line::from(Span::styled(txt.clone(), Style::default())),
            (_, None) => Line::from(""),
          };
        lignes_zone.push(ligne);
      }

    // « Enregistrer ? » n'est pas un texte : c'est un choix explicite. Le
    // surlignage porte sur la réponse qu'Entrée exécutera, `[Non]` par défaut.
    let ligne_confirm = {
        let choix = |oui: bool| -> Style
          {
            if oui == state.choix_oui
              {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
              }
            else
              {
                Style::default().fg(Color::DarkGray)
              }
          };
        Line::from(vec![
            Span::styled(labels[8].0, lbl_style(8)),
            Span::styled(" : ", Style::default().fg(Color::DarkGray)),
            Span::styled("[Oui]", choix(true)),
            Span::styled(" / ", Style::default().fg(Color::DarkGray)),
            Span::styled("[Non]", choix(false)),
        ])
      };

    let mut lines: Vec<Line<'static>> = vec![
      Line::from(""),
      ligne_champ(0),
      ligne_champ(1),
      ligne_champ(2),
      ligne_champ(3),
      ligne_champ(4),
      ligne_champ(5),
      ligne_champ(7),
      ligne_binaire_debug,
      ligne_binaire_release,
    ];
    lines.extend(lignes_zone);
    lines.push(ligne_confirm);
    lines.push(match &state.error
      {
        Some(err) => Line::from(vec![
            Span::styled("!", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {err}"), Style::default().fg(Color::Yellow)),
        ]),
        None => Line::from(""),
      });
    lines.push(Line::from(
      if state.status_menu_open
        {
          "↑/↓ Naviguer · Entrée valider · Échap annuler · Frappe filtrer"
        }
      else if state.typologie_menu_open
        {
          "↑/↓ Naviguer · Entrée valider · Échap annuler"
        }
      else if state.editing
        {
          "←/→ curseur · Ctrl+←/→ mots · Frappe saisie · Entrée valider · Échap"
        }
      else if active == crate::EDIT_CONFIRM_FIELD
        {
          "←/→ choisir · o Oui · n Non · Entrée valider · Échap annuler"
        }
      else
        {
          "Échap annuler · ↑/↓ changer de champ · Entrée éditer"
        }
    )
    .centered());
    lines.push(Line::from(
      "Version : x.y.z seulement · Statut : valeur connue · Outils : commande shell (vide = auto) · Description : non vide",
    )
    .centered());

    // Rendu sans retour automatique : ligne `i` = rangée `area.y + i`. Le repli
    // de la description est déjà calculé avec le même compte que le curseur.
    let paragraph = Paragraph::new(lines).block(block).style(opaque);
    f.render_widget(paragraph, area);

    // Curseur réel : sur la valeur du champ en cours d'édition.
    if state.editing
      {
        let (cursor_x, cursor_y) = if active == 6
          {
            let (cr, cc) = curseur_repli.unwrap_or((0, 0));
            (area.x + cc as u16, area.y + zone_haut as u16 + (cr - scroll) as u16)
          }
        else
          {
            // Sans bordure : ligne 0 = rangée vide, puis les champs simples
            // (Version, Statut, outils, Typologie) chacun sur une rangée. La
            // Typologie (champ 7) vient après les outils, d'où sa rangée
            // propre ; son menu n'entre jamais en édition texte.
            let value_x = area.x + labels[active].0.chars().count() as u16 + 3;
            let dans_champ = colonne_curseur(&state.input_buffer, state.input_offset);
            let rang = if active == 7 { 7 } else { 1 + active };
            (value_x + dans_champ as u16, area.y + rang as u16)
          };
        let cursor_x = cursor_x.min(area.x.saturating_add(area.width).saturating_sub(1));
        let cursor_y = cursor_y.min(area.y.saturating_add(area.height).saturating_sub(1));
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
        // Superposé à la ligne du champ Statut (rangée `area.y + 2`) : le menu
        // prend la place de la valeur qu'il remplace.
        let py = area.y + 2;
        let parea = Rect {
            x: px,
            y: py,
            width: width as u16,
            height: (visible + 2) as u16,
          };
        let parea = parea.intersection(area);
        f.render_widget(Clear, parea);
        let mut popup: Vec<Line> = Vec::new();
        let items_visibles = op_tui::viewport::fenetre(&items, offset, visible);
        for (decalage, item) in items_visibles.iter().enumerate() {
            let idx = decalage + offset;
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
    // Menu déroulant Typologie (champ 7) : popup opaque sous sa ligne.
    if state.typologie_menu_open
      {
        let items = crate::typologies_liste();
        let visible = items.len().min(5);
        let width = items.iter().map(|s| s.chars().count()).max().unwrap_or(12) + 2;
        let px = (area.x + 3).min(area.x.saturating_add(area.width).saturating_sub(width as u16));
        // Superposé à la rangée de la Typologie (`area.y + 7`).
        let py = area.y + 7;
        let parea = Rect {
            x: px,
            y: py,
            width: width as u16,
            height: (visible + 2) as u16,
          };
        let parea = parea.intersection(area);
        f.render_widget(Clear, parea);
        let mut popup: Vec<Line> = Vec::new();
        for (idx, item) in items.iter().enumerate().take(visible) {
            let style = if idx == state.typologie_cursor
              {
                Style::default().fg(Color::Black).bg(Color::Cyan)
              }
            else
              {
                Style::default().fg(Color::White)
              };
            popup.push(Line::from(Span::styled(*item, style)));
          }
        let pblock = Block::default()
          .title(" Typologie ")
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
    let Some(area) = op_tui::layout::popup_rect(size, dialog_width, dialog_height) else {
        return;
    };
    // Ce dialogue ne faisait aucun `Clear` : il s'appuyait sur le fond du cadre
    // pour masquer le menu. Le fond opaque explicite est équivalent, et correct
    // même si le cadre vient à être redessiné.
    op_tui::overlay::fond_opaque(f, area, THEME_GRIS);
    let block = Block::default()
        .title(" Module ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(THEME_GRIS.bordure))
        .style(Style::default().bg(THEME_GRIS.fond));

    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

/// Modale d'examen des incohérences : une ligne par écart détecté.
///
/// Chaque ligne nomme le projet, décrit l'écart et rappelle l'action proposée.
/// Un écart déjà refusé est marqué `REFUSÉ` avec sa date : il reste visible
/// tant que l'écart n'a pas changé, pour être re-notifié sans insister.
fn render_coherence_dialog(
    f: &mut Frame,
    size: Rect,
    app: &App,
    state: &crate::CoherenceState,
) {
    let lignes = crate::lignes_coherence(app);
    let mut lines: Vec<Line> = Vec::new();

    lines.push(Line::from(Span::styled(
        format!("{} incohérence(s) détectée(s)", lignes.len()),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Hauteur utile : cadre, en-tête, ligne d'aide et une marge.
    let hauteur_utile = (size.height as usize).saturating_sub(6).max(1);
    for (i, ligne) in lignes.iter().enumerate() {
        let selectionnee = i == state.selection;
        let style = if selectionnee {
            Style::default().bg(Color::White).fg(Color::Black).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let marqueur = if selectionnee { "►" } else { " " };
        lines.push(Line::from(Span::styled(
            format!(
                "  {} {} — {}",
                marqueur,
                sanitize(&ligne.projet_nom),
                ligne.ecart.description()
            ),
            style,
        )));
        let mut suffixe = format!("      → {}", ligne.action.libelle());
        if let Some(date) = &ligne.refus_date {
            suffixe.push_str(&format!("  [REFUSÉ le {date}]"));
        }
        lines.push(Line::from(Span::styled(suffixe, style)));
    }

    // N'affiche que la fenêtre visible, centrée sur la sélection.
    let hauteur_contenu = lines.len();
    let debut = state
        .selection
        .saturating_sub(hauteur_utile / 2)
        .min(hauteur_contenu.saturating_sub(hauteur_utile));
    let fin = (debut + hauteur_utile).min(hauteur_contenu);
    let fenetre: Vec<Line> = lines[debut..fin].to_vec();

    let dialog_width = size.width.min(90);
    let dialog_height = (hauteur_utile as u16 + 4).min(size.height.saturating_sub(2));
    let Some(area) = op_tui::layout::popup_rect(size, dialog_width, dialog_height) else {
        return;
    };
    op_tui::overlay::fond_opaque(f, area, THEME_GRIS);
    let block = Block::default()
        .title(" Cohérence ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(THEME_GRIS.bordure))
        .style(Style::default().bg(THEME_GRIS.fond));

    let mut contenu = vec![Line::from("")];
    contenu.extend(fenetre);
    contenu.push(Line::from(""));
    contenu.push(
        Line::from("↑/↓ naviguer  ·  Entrée appliquer  ·  x refuser  ·  Échap fermer")
            .centered(),
    );

    let paragraph = Paragraph::new(contenu).block(block).wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

/// Confirmation dédiée d'une action Release : jamais d'écriture sans ce pas.
fn render_confirmation_release(f: &mut Frame, size: Rect, action: &str)
{
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "Action Release — confirmation requise",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(format!("Vous allez : {action}.")));
    lines.push(Line::from("La Release ne se reconstruit que sur instruction explicite."));
    lines.push(Line::from(""));
    lines.push(Line::from("o/Entrée confirmer  ·  n/Échap annuler").centered());

    let dialog_width = size.width.min(80);
    let dialog_height = (lines.len() as u16 + 2).min(size.height.saturating_sub(2));
    let Some(area) = op_tui::layout::popup_rect(size, dialog_width, dialog_height) else {
        return;
    };
    op_tui::overlay::fond_opaque(f, area, THEME_GRIS);
    let block = Block::default()
        .title(" Confirmation ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .style(Style::default().bg(THEME_GRIS.fond));
    let paragraph = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    f.render_widget(paragraph, area);
}

/// Mode d'emploi de chaque réglage de l'onglet Paramètres, sur une ligne.
///
/// Ligne affichée sous la liste, elle suit la flèche ↑/↓ : l'aide porte
/// toujours sur l'option courante, et fait le pont entre le libellé et ce que
/// ce réglage change réellement — la liste, elle, ne dit que la valeur.
const AIDE_OPTIONS: [&str; 5] = [
  "Tri de la liste : par date de modification ou alphabétique",
  "Colonnes visibles : ←→ choisit la colonne, Entrée l'affiche ou la masque",
  "Lancement root : lance l'éditeur avec sudo -i quand les droits manquent",
  "Capture du wrapper : journalise la sortie du lanceur dans le registre",
  "Pays de la source : code ISO, pour la devise de référence des séries",
];

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

    // L'aide de l'option courante : la liste ne montre que la valeur de
    // chaque réglage, cette ligne dit ce qu'il fait. Elle descend avec ↑/↓,
    // donc aucune des cinq explications n'est reléguée dans la F1.
    lignes.push(Line::from(""));
    lignes.push(Line::from(Span::styled(
      format!(" {}", AIDE_OPTIONS.get(etat.actif).copied().unwrap_or("")),
      libelle,
    )));

    // La liste des colonnes activées se prête mal à une suite de `Span` : les
    // noms n'ont pas la même longueur et les ☑/☐ dansent. Une vraie table,
    // sans cadre — le popup porte déjà la bordure — aligne les cases par
    // colonnes, et déplace le jeu uniquement quand on bascule une colonne.
    let colonnes: Vec<String> = crate::registry::TOUTES_COLONNES
      .iter()
      .map(|c| {
        let actif = app.prefs.colonnes.iter().any(|x| x == c);
        if actif { format!("☑ {c}") } else { format!("☐ {c}") }
      })
      .collect();
    let rangees: Vec<Row> = colonnes
      .chunks(3)
      .map(|groupe| Row::new(groupe.iter().map(|c| Cell::from(c.clone()))))
      .collect();

    // Pied : les gestes, puis l'éventuel message d'erreur. Dans leur propre
    // zone, ils gardent les mêmes rangées quelle que soit la taille du popup
    // et ne cassent plus l'alignement du bloc des colonnes.
    let mut pied: Vec<Line> = vec![
      Line::from(""),
      Line::from(Span::styled(
        " Entrée basculer  ·  ↑/↓ option  ·  ←→ colonne  ·  Échap fermer ",
        libelle,
      )),
    ];
    if let Some(msg) = etat.message.as_deref()
      {
        pied.push(Line::from(Span::styled(msg.to_string(), Style::default().fg(Color::Yellow))));
      }

    let zones = Layout::default()
      .direction(Direction::Vertical)
      .constraints([
        Constraint::Length(lignes.len() as u16),
        Constraint::Length(1 + rangees.len() as u16),
        Constraint::Min(0),
      ])
      .split(area);

    f.render_widget(Paragraph::new(lignes), zones[0]);
    let sous_zones = Layout::default()
      .direction(Direction::Vertical)
      .constraints([Constraint::Length(1), Constraint::Min(0)])
      .split(zones[1]);
    // Le titre du bloc garde toute la largeur : pris comme tête de tableau, il
    // serait découpé à la largeur de la première colonne.
    f.render_widget(
      Paragraph::new(Line::from(Span::styled(" Colonnes affichées dans le détail", libelle))),
      sous_zones[0],
    );
    let table = Table::new(rangees, [Constraint::Percentage(33); 3]).column_spacing(1);
    f.render_widget(table, sous_zones[1]);
    f.render_widget(Paragraph::new(pied), zones[2]);
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
    // Barre d'onglets : c'est elle qui rend la structure visible. Sans elle,
    // rien n'indiquait que le popup en avait trois, ni lequel était actif.
    let onglets = [crate::Onglet::Projet, crate::Onglet::Couts, crate::Onglet::Parametres];
    let libelles: Vec<Cow<'static, str>> =
      onglets.iter().map(|o| Cow::Borrowed(o.libelle())).collect();
    let actif = onglets.iter().position(|o| *o == popup.onglet).unwrap_or(0);
    // Pied commun aux trois onglets : la navigation d'onglets est globale, elle
    // doit donc être annoncée une fois pour tous, pas onglet par onglet.
    let Some(contenu) = op_tui::popup::Fenetre::nouveau(
      crate::sanitize(&titre_popup(app, popup)),
      THEME,
    )
    .avec_onglets(&libelles, actif)
    .avec_pied(" Tab / ⇧Tab : onglets  ·  Échap : fermer ")
    .style_onglets(Style::default().fg(Color::Cyan).bg(Color::Black))
    .style_pied(Style::default().fg(Color::DarkGray))
    .rendu(f, size, largeur, hauteur)
    else
      {
        return;
      };

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
        crate::Onglet::Projet => render_contenu_projet(f, contenu, &popup.projet, projet),
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
    // Note comptable sous la synthèse : pour une bibliothèque, les apports
    // reçus des projets ; pour un projet, les crédits versés aux
    // bibliothèques (affichés à part, jamais additionnés au coût du projet
    // qui les couvre déjà).
    let apports = crate::apport::charger_apports(&crate::apport::chemin_apports(&app.registry_path))
        .unwrap_or_default();
    let note_synthese = if project.est_bibliotheque {
        format!(
            "Apports reçus des projets : {} (non additionnés au coût total)",
            crate::format_currency(
                crate::apport::somme_credits_lib(&apports, &project.id),
                crate::LOCAL_CURRENCY
            )
        )
    } else {
        let verses = crate::apport::somme_credits_emis(&apports, &project.id);
        format!(
            "Dont bibliothèques (non additionnés) : {} portés au crédit des bibliothèques touchées",
            crate::format_currency(verses, crate::LOCAL_CURRENCY)
        )
    };
    let hauteur_note = lignes_repliees(&note_synthese, area.width.saturating_sub(2));

    // Trois blocs : le réglage du tarif, la synthèse du travail accompli et
    // de son coût (plus la note comptable), puis le détail par session. La
    // synthèse tient en trois rangées (bordure haute + libellés + valeurs) ;
    // la note lui emboîte le pas. Le détail garde le reste, avec un plancher
    // pour ne jamais disparaître.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(7),
            Constraint::Length(3 + u16::try_from(hauteur_note).unwrap_or(1)),
            Constraint::Min(6),
        ])
        .split(area);
    render_bloc_tarif(f, chunks[0], app, project, couts);
    render_bloc_synthese(f, chunks[1], project, Some(&note_synthese));
    render_tableau_couts(f, chunks[2], project, couts.scroll_offset);
}

/// Rappel de la signification de « — », affiché seulement quand le coût est
/// inconnu : sans lui, un utilisateur y lit un zéro ou une session non
/// lancée.
const RAPPEL_TIRET: &str = "« — » : le fournisseur ne publie aucun tarif pour ce \
     modèle, opencode enregistre donc un coût nul malgré les tokens consommés.";

/// Nombre de lignes que prend `texte` rendu avec repli automatique sur
/// `largeur` colonnes : mot à mot, comme fait `Wrap`.
///
/// `Paragraph::line_count` appartient à l'API instable `rendered-line-info`
/// de ratatui ; le repli d'un texte sans tabulation se compte aussi à la
/// main. Ce qui compte n'est pas l'exactitude absolue, mais que la place
/// **réservée** et le **rendu** s'appuient sur le même compte : les deux
/// restent alors bord à bord quelle que soit la largeur.
fn lignes_repliees(texte: &str, largeur: u16) -> usize
  {
    if largeur == 0
      {
        return 1;
      }
    let largeur = usize::from(largeur);
    let mut lignes = 1usize;
    let mut colonne = 0usize;
    for mot in texte.split_whitespace()
      {
        let longueur = mot.chars().count();
        if colonne == 0
          {
            colonne = longueur;
          }
        else if colonne + 1 + longueur <= largeur
          {
            colonne += 1 + longueur;
          }
        else
          {
            lignes += 1;
            colonne = longueur;
          }
      }
    lignes
  }

/// Largeur d'affichage d'un caractère, en cellules de terminal.
///
/// Un caractère large (`漢`, certains émojis) en occupe deux : la même mesure
/// que `colonne_curseur`, pour que le repli et le curseur tombent ensemble.
fn largeur_cellule(c: char) -> usize
  {
    Line::from(c.to_string()).width()
  }

/// Replie `texte` en lignes d'au plus `largeur` colonnes d'affichage, au mot
/// quand c'est possible et au caractère pour un mot trop long.
///
/// Retourne les lignes du repli et, pour chaque caractère **et sa fin**, la
/// `(rangée, colonne)` correspondante. Le rendu et le curseur s'appuient ainsi
/// sur le même calcul : ils restent bord à bord à toute largeur, ce qu'un
/// `Paragraph::wrap` ne permettrait pas de retrouver côté curseur.
fn replier_texte(texte: &str, largeur: usize) -> (Vec<String>, Vec<(usize, usize)>)
  {
    let largeur = largeur.max(1);
    let caracteres: Vec<char> = texte.chars().collect();
    let mut lignes: Vec<String> = vec![String::new()];
    let mut positions: Vec<(usize, usize)> = Vec::with_capacity(caracteres.len() + 1);
    let mut rangee = 0usize;
    let mut colonne = 0usize;
    let mut i = 0usize;
    while i < caracteres.len()
      {
        let c = caracteres[i];
        if c == ' '
          {
            // L'espace est ignorée en tête de ligne ; ailleurs elle n'est posée
            // que si le mot suivant tient derrière elle, pour ne pas rester
            // seule en fin de ligne.
            let mut j = i + 1;
            while j < caracteres.len() && caracteres[j] == ' '
              {
                j += 1;
              }
            let largeur_mot: usize = caracteres[j..]
              .iter()
              .take_while(|&&x| x != ' ')
              .map(|&x| largeur_cellule(x))
              .sum();
            if colonne == 0 || j >= caracteres.len() || colonne + 1 + largeur_mot <= largeur
              {
                if colonne > 0
                  {
                    lignes[rangee].push(' ');
                    colonne += 1;
                  }
                positions.push((rangee, colonne.saturating_sub(1)));
              }
            else
              {
                rangee += 1;
                lignes.push(String::new());
                colonne = 0;
                positions.push((rangee, colonne));
              }
            i += 1;
            continue;
          }
        let lc = largeur_cellule(c);
        if colonne > 0 && colonne + lc > largeur
          {
            rangee += 1;
            lignes.push(String::new());
            colonne = 0;
          }
        lignes[rangee].push(c);
        positions.push((rangee, colonne));
        colonne += lc;
        i += 1;
      }
    positions.push((rangee, colonne));
    (lignes, positions)
  }

/// Bloc « Synthèse » : le travail accompli et ce qu'il a coûté, en un seul
/// cadre au-dessus du détail par session.
///
/// Rendu dans `area` sans cadre propre : la bordure haute est dessinée ici,
/// comme pour le tableau des coûts. Une valeur absente retombe sur « — »,
/// jamais sur un zéro qui laisserait croire à un travail non mesuré.
///
/// `note` est une ligne rappelant la comptabilité des bibliothèques (apports
/// reçus ou crédits versés), rendue sous le tableau, dans le même bloc.
fn render_bloc_synthese(
    f: &mut Frame,
    area: Rect,
    project: &crate::ProjectEntry,
    note: Option<&str>,
) {
    let label = Style::default().fg(Color::DarkGray);
    let cout = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);
    let ou_d = |v: &str| -> String
      {
        if v.trim().is_empty() { "—".to_string() } else { sanitize(v) }
      };
    let table = Table::new(
        vec![
            Row::new(vec![
                Cell::from("Temps total").style(label),
                Cell::from("Sessions").style(label),
                Cell::from("Tokens").style(label),
                Cell::from("Coût total").style(label),
            ]),
            Row::new(vec![
                Cell::from(ou_d(&project.total_duree)),
                Cell::from(ou_d(&project.session_count)),
                Cell::from(ou_d(&project.total_tokens)),
                Cell::from(ou_d(&project.total_cost)).style(cout),
            ]),
        ],
        [
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ],
    )
    .column_spacing(2)
    .block(
        Block::default()
            .title(" Synthèse ")
            .borders(Borders::TOP)
            .border_style(Style::default().fg(Color::DarkGray))
            .style(Style::default().bg(Color::Black)),
    );
    f.render_widget(table, area);
    if let Some(note) = note
      {
        // La note descend sans cadre, sous le tableau : la bordure haute du
        // bloc la contient encore visuellement.
        let zone = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(area);
        f.render_widget(
            Paragraph::new(sanitize(note))
                .style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)),
            zone[1],
        );
      }
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

        // Devise de la session, déduite du montant d'origine. Cette lecture ne
        // doit jamais voir un montant converti : « 4,20 € · 4,57 $ » contient
        // les deux symboles, et le premier test rencontré gagnerait.
        let currency = if project.last_session_cost.contains('$') { "$ (USD)" }
        else if project.last_session_cost.contains('€') { "€ (EUR)" }
        else if project.last_session_cost.contains('£') { "£ (GBP)" }
        else if project.last_session_cost.contains('¥') { "¥ (JPY)" }
        else { "—" };

        // Montant équivalent en devise locale. Vide quand la devise est
        // inconnue ou déjà locale : la colonne affiche alors « — », comme
        // avant, plutôt qu'un montant redondant.
        let last_session_cost_local = if project.last_session_cost_local.is_empty() {
            "—".to_string()
        } else {
            project.last_session_cost_local.clone()
        };

        rows.push(Row::new(vec![
            Cell::from(sanitize(last_time)),
            Cell::from(sanitize(last_model)),
            Cell::from(sanitize(&last_session_cost)),
            Cell::from(sanitize(&last_session_cost_local)),
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

    // Défilement borné par la place réelle : la rangée de titre du bloc,
    // l'indicateur de défilement sur la dernière rangée, et — quand il
    // s'affiche — le rappel de « — », juste au-dessus. Ce rappel pèse deux à
    // quatre rangées selon la largeur : le compte se fait en lignes rendues,
    // jamais en constante, sinon la dernière ligne de données passe sous
    // l'un ou l'autre.
    let largeur_replis = area.width.saturating_sub(2);
    // Place laissée au rappel : titre du bloc, ligne d'en-tête, au moins une
    // ligne de données et l'indicateur doivent survivre.
    let place_rappel = usize::from(area.height).saturating_sub(4);
    let lignes_rappel = if cout_inconnu
      {
        lignes_repliees(RAPPEL_TIRET, largeur_replis).min(place_rappel)
      }
    else
      {
        0
      };
    let visibles = usize::from(area.height)
        .saturating_sub(3 + lignes_rappel)
        .max(1);
    let donnees = rows.len().saturating_sub(1);
    let actual_scroll = op_tui::viewport::borner(scroll_offset, donnees, visibles);

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
    .column_spacing(2)
    .block(block);

    f.render_widget(table, area);

    // Rappel de la signification de « — », sur toute la place qu'il lui faut.
    if lignes_rappel > 0
      {
        let hauteur = lignes_rappel as u16;
        let rappel = Paragraph::new(RAPPEL_TIRET)
            .style(Style::default().fg(Color::DarkGray))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        let rappel_area = Rect {
            x: area.x + 1,
            y: area.y + area.height.saturating_sub(1 + hauteur),
            width: largeur_replis,
            height: hauteur,
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
    // ne pas confondre l'estimation avec un coût réellement supporté. Pas de
    // ligne vide devant : chaque rangée gagnée est une de plus affichée, le
    // titre jaune en gras tient déjà le séparateur.
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
    use ratatui::{
        backend::TestBackend,
        buffer::{Buffer, Cell},
        Terminal,
    };

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

    /// Rendu du formulaire dans un buffer. C'est le seul niveau où le style
    /// d'une cellule reste observable : `texte()` ne garde que les symboles et
    /// perdrait la mise en valeur de la ligne o/N.
    fn creation_dans_buffer(state: &ProjectCreationState) -> (Buffer, (u16, u16))
      {
        let mut term = Terminal::new(TestBackend::new(90, 20)).unwrap();
        term.draw(|f| render_creation_dialog(f, f.area(), state)).unwrap();
        let curseur = term.get_cursor_position().unwrap();
        (term.backend().buffer().clone(), (curseur.x, curseur.y))
      }

    /// Rendu du popup unifié dans un buffer, avec la position réelle du
    /// curseur : c'est le couple qui permet de vérifier que le curseur tombe
    /// sur la rangée du champ qu'il prétend éditer.
    fn popup_avec_curseur(
        app: &crate::App,
        popup: &crate::Popup,
        largeur: u16,
        hauteur: u16,
      ) -> (Buffer, (u16, u16))
      {
        let mut term = Terminal::new(TestBackend::new(largeur, hauteur)).unwrap();
        term.draw(|f| render_popup_unifie(f, f.area(), app, popup)).unwrap();
        let curseur = term.get_cursor_position().unwrap();
        (term.backend().buffer().clone(), (curseur.x, curseur.y))
      }

    /// Première rangée portant `ancre`, ou `None` si elle est absente.
    ///
    /// Recherche par colonnes (via `colonne`), pour les mêmes raisons que
    /// `ligne` : `find` sur une `String` donne un indice d'octet.
    fn rang(buf: &Buffer, ancre: &str) -> Option<u16>
      {
        (0..buf.area.height).find(|&y| colonne(&ligne(buf, y), ancre).is_some())
      }

    /// Style appliqué à la première cellule portant `cherche`, ou `None` si le
    /// texte est absent du rendu.
    fn style_de(buf: &Buffer, cherche: &str) -> Option<Modifier>
      {
        let rendu = texte(buf);
        let mut lignes = rendu.lines();
        for y in 0..buf.area.height
          {
            if let Some(x) = lignes.next()?.find(cherche)
              {
                return Some(buf[(x as u16, y)].modifier);
              }
          }
        None
      }

    /// Aspect **tel que le terminal l'affiche** : les deux plans après l'éventuel
    /// échange de `REVERSED`, plus les modificateurs qui survivent.
    ///
    /// `REVERSED` échange `fg` et `bg` au moment du rendu : deux cellules de
    /// styles différents s'affichent alors à l'identique. Un test qui ne compare
    /// que `fg`/`bg` croit distinguer des libellés pareils, et laisse passer
    /// exactement le défaut qu'il cherche. Le gras se compare tel quel : certains
    /// écrans ne jouent leur repère que par la graisse.
    fn aspect_affiche(c: &Cell) -> (Color, Color, Modifier)
      {
        let (fg, bg) = if c.modifier.contains(Modifier::REVERSED)
          {
            (c.bg, c.fg)
          }
        else
          {
            (c.fg, c.bg)
          };
        // `REVERSED` est consommé par l'échange : il ne distingue plus rien.
        (fg, bg, c.modifier - Modifier::REVERSED)
      }

    /// Une ligne du rendu, caractère par caractère, **indexée en colonnes**.
    ///
    /// `texte()` renvoie une `String` où `find()` donne un indice d'octet, alors
    /// qu'un `Buffer` s'indexe en colonnes. La bordure `│` pèse trois octets
    /// pour une seule colonne : confondre les deux revient à viser la mauvaise
    /// cellule — ici, le champ d'à côté.
    fn ligne(buf: &Buffer, y: u16) -> Vec<char>
      {
        (0..buf.area.width)
            .map(|x| buf.cell((x, y)).and_then(|c| c.symbol().chars().next()).unwrap_or(' '))
            .collect()
      }

    /// Colonne du premier caractère de l'ancre, ou `None` si elle est absente.
    fn colonne(ligne: &[char], ancre: &str) -> Option<u16>
      {
        let cible: Vec<char> = ancre.chars().collect();
        ligne
            .windows(cible.len())
            .position(|f| f == cible.as_slice())
            .map(|i| i as u16)
      }

    /// Colonne du premier caractère de la valeur d'un champ : le séparateur
    /// `" : "` et les espaces qui suivent le libellé sont sautés sur place,
    /// jamais comptés en dur — un libellé reformaté ne décale pas le test.
    fn colonne_valeur(ligne: &[char], fin_ancre: u16) -> Option<u16>
      {
        (fin_ancre..ligne.len() as u16).find(|x| !matches!(ligne[*x as usize], ' ' | ':'))
      }

    /// Aspect de la cellule visée par une ancre.
    ///
    /// `saute = Some(0)` observe le libellé lui-même ; `saute = None` observe la
    /// valeur qui le suit, à une colonne déduite du rendu. L'ancre est le
    /// libellé **exactement tel qu'il est écrit**, séparateur compris quand le
    /// formulaire le place avant le contenu : c'est ce qui empêche de confondre
    /// un libellé avec la valeur affichée sur une autre ligne.
    fn aspect_cible(
        buf: &Buffer,
        ancre: &str,
        saute: Option<usize>,
      ) -> Option<(Color, Color, Modifier)>
      {
        for y in 0..buf.area.height
          {
            let l = ligne(buf, y);
            // `continue`, pas `?` : l'ancre peut être ailleurs dans l'écran, et
            // seul le champ qui la contient doit être observé.
            let cible = match colonne(&l, ancre)
              {
                None => continue,
                Some(x) => match saute
                  {
                    Some(d) => Some(x + d as u16),
                    None => colonne_valeur(&l, x + ancre.chars().count() as u16),
                  },
              };
            let Some(cx) = cible else
              {
                continue;
              };
            return Some(aspect_affiche(buf.cell((cx, y))?));
          }
        None
      }

    /// Chaque champ doit se distinguer de tous les autres : c'est la seule
    /// information qui dit où l'on se trouve.
    fn assert_champs_differencies(
        buf: &Buffer,
        ancres: &[&str],
        saute: Option<usize>,
        actif: usize,
        quoi: &str,
      )
      {
        let reels: Vec<(Color, Color, Modifier)> = ancres
            .iter()
            .map(|a| {
                aspect_cible(buf, a, saute)
                    .unwrap_or_else(|| panic!("{quoi} : « {a} » absent du rendu"))
            })
            .collect();
        let attendu = reels[actif];
        for (i, obtenu) in reels.iter().enumerate()
          {
            assert!(
              i == actif || *obtenu != attendu,
              "{quoi} : le champ {i} et le champ actif {actif} s'affichent pareil \
               ({:?}) — la navigation devient invisible",
              attendu
            );
          }
      }

    /// Rend le formulaire de création dans un terminal de test.
    fn rendre_creation(state: &ProjectCreationState) -> (String, (u16, u16))
      {
        let (buf, curseur) = creation_dans_buffer(state);
        (texte(&buf), curseur)
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

    /// Le curseur de l'éditeur doit tomber sur la rangée du champ affiché.
    ///
    /// Le contenu du popup est rendu sans cadre (le cadre est celui de
    /// `Fenetre`) : ligne `i` = rangée `area.y + i`. Le calcul comptait la
    /// bordure en plus et posait le curseur une rangée trop bas — sur le champ
    /// suivant, ou sur le pied quand on éditait le dernier.
    /// « Enregistrer ? » est un choix, pas un champ texte : seuls Version,
    /// Statut et Description portent un curseur.
    #[test]
    fn le_curseur_du_popup_se_pose_sur_la_ligne_du_champ()
      {
        let (app, _dir) = app_de_test("curseur-popup");
        let libelles = [
            "Version Debug",
            "Statut Debug",
            "Sonde Debug",
            "Build Debug",
            "Sonde Release",
            "Build Release",
            "Description",
        ];
        for (champ, libelle) in libelles.iter().enumerate()
          {
            let mut popup = popup_de_test(&app, crate::Onglet::Projet);
            popup.projet.active_field = champ;
            popup.projet.editing = true;
            popup.projet.input_buffer = "abc".to_string();
            popup.projet.input_offset = 3;
            let (buf, (_, curseur_y)) = popup_avec_curseur(&app, &popup, 110, 34);
            let attendu = rang(&buf, libelle)
                .unwrap_or_else(|| panic!("« {libelle} » absent du rendu"));
            assert_eq!(
                curseur_y, attendu,
                "champ {champ} : le curseur doit se poser sur la ligne « {libelle} »"
            );
          }
      }

    /// Une description longue se replie dans sa zone dédiée : « Enregistrer ? »
    /// garde sa rangée fixe, et le texte se lit en entier, ligne après ligne,
    /// au lieu d'être tronqué à droite.
    #[test]
    fn une_description_longue_ne_deplace_pas_les_lignes_suivantes()
      {
        let (mut app, _dir) = app_de_test("desc-longue");
        let rendre = |app: &mut crate::App, desc: &str| -> Buffer
          {
            app.projects[0].description = desc.to_string();
            let popup = popup_de_test(app, crate::Onglet::Projet);
            popup_dans_buffer(app, &popup, 110, 34)
          };
        let courte = rendre(&mut app, "court");
        let longue = rendre(&mut app, &"x".repeat(300));

        let enregistrer = |b: &Buffer| rang(b, "Enregistrer ?")
            .unwrap_or_else(|| panic!("la ligne Enregistrer doit être là :\n{}", texte(b)));
        assert_eq!(
            enregistrer(&courte), enregistrer(&longue),
            "une description longue ne doit pas décaler « Enregistrer ? » :\n{}",
            texte(&longue)
        );

        let description =
          rang(&longue, "Description").expect("la ligne Description doit être là");
        assert!(
            colonne(&ligne(&longue, description + 1), "xxxx").is_some(),
            "la description longue doit se poursuivre sur la ligne suivante :\n{}",
            texte(&longue)
        );
      }

    /// Pendant l'édition d'une description plus longue que sa zone, le curseur
    /// reste visible : la zone défile automatiquement jusqu'au bas, au lieu de
    /// laisser le curseur hors des lignes rendues.
    #[test]
    fn le_curseur_reste_visible_dans_une_description_tres_longue()
      {
        let (mut app, _dir) = app_de_test("desc-defile");
        let texte_long = "x".repeat(5000);
        app.projects[0].description = texte_long.clone();
        let mut popup = popup_de_test(&app, crate::Onglet::Projet);
        popup.projet.editing = true;
        popup.projet.active_field = 6;
        popup.projet.input_buffer = texte_long.clone();
        popup.projet.input_offset = texte_long.chars().count();
        let (buf, (_, curseur_y)) = popup_avec_curseur(&app, &popup, 110, 34);
        let enregistrer =
          rang(&buf, "Enregistrer ?").expect("Enregistrer doit rester visible");
        assert_eq!(
            curseur_y, enregistrer - 1,
            "le curseur doit se poser au bas de la zone, juste au-dessus d'Enregistrer :\n{}",
            texte(&buf)
        );
        assert!(
            ligne(&buf, curseur_y).contains(&'x'),
            "la zone doit avoir défilé jusqu'à la fin de la description :\n{}",
            texte(&buf)
        );
      }

    /// Le curseur du formulaire de création doit viser la rangée du champ
    /// affiché, pour les sept entrées y compris la confirmation finale.
    #[test]
    fn le_curseur_du_formulaire_se_pose_sur_la_ligne_du_champ()
      {
        let libelles = [
            "ID",
            "Nom",
            "Chemin",
            "Description",
            "Statut",
            "Version",
            "Ouvrir ?",
        ];
        for (champ, libelle) in libelles.iter().enumerate()
          {
            let mut state = saisie_en_cours("abc", 3);
            state.active_field = champ;
            let (buf, (_, curseur_y)) = creation_dans_buffer(&state);
            let attendu = rang(&buf, libelle)
                .unwrap_or_else(|| panic!("« {libelle} » absent du rendu"));
            assert_eq!(
                curseur_y, attendu,
                "champ {champ} : le curseur doit se poser sur la ligne « {libelle} »"
            );
          }
      }

    /// Un chemin très long reste tronqué à droite, et la confirmation finale
    /// garde sa rangée. L'écart ID → « Ouvrir ? » inclut la zone fixe de la
    /// Description (libellé + deux lignes) : 8 rangées au total.
    #[test]
    fn une_valeur_longue_ne_deplace_pas_la_confirmation()
      {
        let mut state = saisie_en_cours("", 0);
        state.path = "x".repeat(300);
        let (buf, _) = creation_dans_buffer(&state);
        let id = rang(&buf, "ID").expect("la ligne ID doit être là");
        let ouvrir = rang(&buf, "Ouvrir ?").expect("la ligne Ouvrir doit être là");
        assert_eq!(
            ouvrir, id + 8,
            "le chemin long ne doit pas décaler les lignes suivantes :\n{}",
            texte(&buf)
        );
      }

    /// La Description du formulaire de création se replie dans sa zone fixe :
    /// le début se lit sur la ligne du libellé, la suite en dessous, et les
    /// champs suivants gardent leur rangée.
    #[test]
    fn une_description_longue_se_replie_dans_le_formulaire_de_creation()
      {
        let courte = creation_dans_buffer(&saisie_en_cours("", 0)).0;
        let mut state = saisie_en_cours("", 0);
        state.description = "y".repeat(200);
        let (longue, _) = creation_dans_buffer(&state);
        assert_eq!(
            rang(&courte, "Ouvrir ?"), rang(&longue, "Ouvrir ?"),
            "la description longue ne doit pas décaler « Ouvrir ? » :\n{}",
            texte(&longue)
        );
        let description =
          rang(&longue, "Description").expect("la ligne Description doit être là");
        assert!(
            colonne(&ligne(&longue, description + 1), "yyy").is_some(),
            "la description doit se poursuivre sous sa ligne :\n{}",
            texte(&longue)
        );
      }

    /// Le menu Statut du popup se superpose à la ligne du champ qu'il édite :
    /// il prend la place de la valeur qu'il remplace, pas celle des lignes
    /// d'avant (la Version) ni celles d'après.
    #[test]
    fn le_menu_du_statut_du_popup_couvre_sa_ligne()
      {
        let (app, _dir) = app_de_test("projet-gamma");
        let mut popup = popup_de_test(&app, crate::Onglet::Projet);
        popup.projet.active_field = 1;
        popup.projet.status_menu_open = true;
        let buf = popup_dans_buffer(&app, &popup, 110, 34);
        let version = rang(&buf, "Version Debug").expect("la ligne Version doit rester lisible");
        let menu = rang(&buf, " Statut ")
            .expect("le titre du menu Statut doit être là");
        assert_eq!(
            menu, version + 1,
            "le menu doit couvrir exactement la ligne du champ Statut :\n{}",
            texte(&buf)
        );
      }

    /// Le menu du formulaire de création reste sous la ligne du Statut : la
    /// ligne du champ est à `area.y + 7`, le menu à `area.y + 8`.
    #[test]
    fn le_menu_du_statut_du_formulaire_descend_d_une_rangee()
      {
        let mut state = saisie_en_cours("", 4);
        state.status_menu_open = true;
        let (buf, _) = creation_dans_buffer(&state);
        let statut = rang(&buf, "Statut").expect("la ligne Statut doit être là");
        let menu = rang(&buf, " Statut ")
            .expect("le titre du menu Statut doit être là");
        assert_eq!(
            menu, statut + 1,
            "le menu doit s'ouvrir juste sous la ligne du champ Statut :\n{}",
            texte(&buf)
        );
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

    /// La ligne de confirmation finale doit se distinguer du simple rappel
    /// qu'elle est la plupart du temps : le champ 6 n'est atteignable que par
    /// Entrée sur Version, donc la frappe ne donne aucun autre indice que le
    /// gras.
    #[test]
    fn l_indication_de_confirmation_est_en_gras_quand_elle_est_active()
      {
        let mut state = saisie_en_cours("", 0);
        state.active_field = 6;
        let (buf, _) = creation_dans_buffer(&state);
        let modificateurs = style_de(&buf, "o/N")
          .unwrap_or_else(|| panic!("l'indication o/N doit rester affichée :\n{}", texte(&buf)));
        assert!(
          modificateurs.contains(Modifier::BOLD),
          "o/N doit être en gras quand le champ de confirmation est actif"
        );
      }

    #[test]
    fn l_indication_de_confirmation_est_simple_tant_qu_elle_est_inactive()
      {
        let (buf, _) = creation_dans_buffer(&saisie_en_cours("", 0));
        let modificateurs = style_de(&buf, "o/N")
          .unwrap_or_else(|| panic!("l'indication o/N doit rester affichée :\n{}", texte(&buf)));
        assert!(
          !modificateurs.contains(Modifier::BOLD),
          "o/N ne doit pas être en gras tant que la confirmation n'est pas active"
        );
      }

/// Le formulaire de création : le libellé actif devait ressortir en noir sur
    /// cyan. Il était `fg(Noir) + bg(Cyan) + REVERSED`, or `REVERSED` échange les
    /// deux plans au rendu — le surlignage s'annulait. Ce formulaire est posé sur
    /// un fond gris, si bien que le champ actif restait vaguement repérable par
    /// son fond noir : le défaut y était une promesse de style non tenue, pas la
    /// navigation invisible de l'éditeur. Chaque position est vérifiée.
    #[test]
    fn le_champ_actif_du_formulaire_se_voit()
      {
        let libelles = [
          "ID           ",
          "Nom          ",
          "Chemin       ",
          "Description  ",
          "Statut       ",
          "Version      ",
          "Ouvrir ?     ",
        ];
        for champ in 0..libelles.len()
          {
            let mut state = saisie_en_cours("", 0);
            state.active_field = champ;
            let (buf, _) = creation_dans_buffer(&state);
            assert_champs_differencies(&buf, &libelles, Some(0), champ, "formulaire de création");
          }
      }

/// Le défaut réel, dans l'éditeur atteint par `e` : là le popup est sur fond
    /// noir, et l'inversion rendait le champ actif **exactement** identique aux
    /// inactifs — cyan sur noir des deux côtés. Aucune indication de position,
    /// alors que la frappe part dans le bon champ. Les quatre champs y compris,
    /// « Enregistrer ? ».
    #[test]
    fn le_champ_actif_de_l_editeur_se_voit()
      {
        let (app, _dir) = app_de_test("editeur-actif");
        let libelles = ["Version Debug ", "Statut Debug  ", "Sonde Debug   ", "Build Debug   ", "Sonde Release ", "Build Release ", "Description   ", "Typologie     ", "Enregistrer ? "];
        for champ in 0..libelles.len()
          {
            let mut popup = popup_de_test(&app, crate::Onglet::Projet);
            popup.projet.active_field = champ;
            let buf = popup_dans_buffer(&app, &popup, 110, 34);
            assert_champs_differencies(&buf, &libelles, Some(0), champ, "éditeur");
          }
      }

    /// Le popup Réglages montre la vérité binaire des deux variantes, comme le
    /// popup Détails : Debug et Release ont chacune leur ligne.
    #[test]
    fn le_popup_reglages_montre_les_deux_binaires()
      {
        let (app, _dir) = app_de_test("reglages-bifilaire");
        let popup = popup_de_test(&app, crate::Onglet::Projet);
        let rendu = rendre_popup(&app, &popup);
        assert!(rendu.contains("Binaire D :"), "ligne Debug absente :\n{rendu}");
        assert!(rendu.contains("Binaire R :"), "ligne Release absente :\n{rendu}");
      }

    /// « Enregistrer ? » n'est pas un texte à saisir : c'est un choix
    /// explicite, les deux réponses affichées côte à côte.
    #[test]
    fn la_confirmation_affiche_ses_deux_reponses()
      {
        let (app, _dir) = app_de_test("confirmation-deux");
        let popup = popup_de_test(&app, crate::Onglet::Projet);
        let rendu = rendre_popup(&app, &popup);
        assert!(rendu.contains("[Oui]"), "le choix [Oui] doit être affiché :\n{rendu}");
        assert!(rendu.contains("[Non]"), "le choix [Non] doit être affiché :\n{rendu}");
      }

    /// Le surlignage désigne la réponse qu'Entrée exécutera : il doit basculer
    /// avec le choix, sinon il ne dirait rien de la prochaine action. Le
    /// défaut est `[Non]`.
    #[test]
    fn le_surlignage_de_la_confirmation_suit_le_choix()
      {
        let (app, _dir) = app_de_test("confirmation-surligne");
        let mut popup = popup_de_test(&app, crate::Onglet::Projet);
        let aspect = |buf: &Buffer, ancre: &str| -> (Color, Modifier)
          {
            let (fg, _, modifier) = aspect_cible(buf, ancre, Some(0))
                .unwrap_or_else(|| panic!("« {ancre} » doit être affiché"));
            (fg, modifier)
          };

        let buf = popup_dans_buffer(&app, &popup, 110, 34);
        let (fg_non, mod_non) = aspect(&buf, "[Non]");
        let (fg_oui, mod_oui) = aspect(&buf, "[Oui]");
        assert!(
          mod_non.contains(Modifier::BOLD) && fg_non == Color::Yellow,
          "par défaut, [Non] doit être le choix surligné (gras jaune)"
        );
        assert!(
          !mod_oui.contains(Modifier::BOLD) && fg_oui == Color::DarkGray,
          "[Oui] doit rester en retrait tant qu'il n'est pas choisi"
        );

        popup.projet.choix_oui = true;
        let buf = popup_dans_buffer(&app, &popup, 110, 34);
        let (fg_non, mod_non) = aspect(&buf, "[Non]");
        let (fg_oui, mod_oui) = aspect(&buf, "[Oui]");
        assert!(
          mod_oui.contains(Modifier::BOLD) && fg_oui == Color::Yellow,
          "choisi, [Oui] doit prendre le surlignage"
        );
        assert!(
          !mod_non.contains(Modifier::BOLD) && fg_non == Color::DarkGray,
          "et [Non] doit le rendre"
        );
      }

/// L'onglet Coûts met la valeur en valeur plutôt que le libellé : c'est le
    /// contenu qu'il faut regarder, pas le texte. La mise en valeur porte à la
    /// fois sur la couleur **et** sur le gras, d'où une comparaison d'aspect
    /// complet.
    #[test]
    fn le_champ_actif_de_l_onglet_couts_se_voit()
      {
        let (app, _dir) = app_de_test("couts-actif");
        // Ancres reproduites exactement, séparateur `" : "` compris : sans lui,
        // « Tarif manuel  » désignerait aussi la valeur affichée sur la ligne du
        // champ précédent, et le test observerait la mauvaise cellule.
        let ancres = [" Mode          : ", " Tarif manuel  : ", " Pays source   : "];
        for champ in 0..ancres.len()
          {
            let mut popup = popup_de_test(&app, crate::Onglet::Couts);
            popup.couts.active_field = champ;
            let buf = popup_dans_buffer(&app, &popup, 110, 34);
            assert_champs_differencies(&buf, &ancres, None, champ, "onglet Coûts");
          }
      }

/// L'onglet Paramètres pose un `▸` devant la seule option active. C'est un
    /// repère textuel, donc plus fort qu'une couleur : le test vérifie qu'il
    /// désigne bien la position demandée, et lui seul. Cinq options sont
    /// énumérées, pas quatre.
    #[test]
    fn l_option_active_de_l_onglet_parametres_est_marquee()
      {
        let (app, _dir) = app_de_test("param-actif");
        let libelles = [
          "Tri de la liste",
          "Colonnes visibles",
          "Lancement root",
          "Capture du wrapper",
          "Pays de la source",
        ];
        for (actif, attendu) in libelles.iter().enumerate()
          {
            let mut popup = popup_de_test(&app, crate::Onglet::Parametres);
            popup.parametres.actif = actif;
            let rendu = texte(&popup_dans_buffer(&app, &popup, 110, 34));
            // Seules les lignes d'option comptent : la barre d'onglets porte
            // elle aussi un `▸`, devant l'onglet courant. Compter tous les
            // `▸` de l'écran échouerait pour une raison sans rapport avec les
            // options.
            let marquees: Vec<&str> = rendu
                .lines()
                .filter(|l| libelles.iter().any(|x| l.contains(x)) && l.contains('▸'))
                .collect();
            assert_eq!(
              marquees.len(),
              1,
              "un seul `▸` doit apparaître, position {actif} :\n{rendu}"
            );
            assert!(
              marquees[0].contains(attendu),
              "le `▸` doit désigner « {attendu} », il est sur :\n{rendu}"
            );
            // Les positions rendent des écrans différents : sinon deux options
            // seraient indiscernables.
            let autres: Vec<String> = (0..libelles.len())
              .filter(|i| *i != actif)
              .map(|i| {
                  let mut autre = popup_de_test(&app, crate::Onglet::Parametres);
                  autre.parametres.actif = i;
                  texte(&popup_dans_buffer(&app, &autre, 110, 34))
              })
              .collect();
            for rendu_autre in autres
              {
                assert_ne!(rendu_autre, rendu, "deux positions rendent le même écran");
              }
          }
      }

    // --- Bibliothèques (lot G1) ----------------------------------------

    /// Le choix du type de la nouvelle entrée montre les deux possibilités et
    /// leur geste de validation.
    #[test]
    fn le_choix_du_type_montre_projet_et_bibliotheque()
      {
        let rendu = rendre_ecran(&crate::InputMode::ChoixNouveau { selected: 0 }, 110, 34);
        assert!(rendu.contains("Projet"), "le projet doit être proposé :\n{rendu}");
        assert!(rendu.contains("Bibliothèque"), "la bibliothèque doit être proposée :\n{rendu}");
        assert!(
          rendu.contains("↑/↓ choisir · Entrée valider · Échap annuler"),
          "le pied du choix doit annoncer les gestes :\n{rendu}"
        );
      }

    /// Le formulaire de création change de titre selon le type choisi.
    #[test]
    fn le_titre_du_formulaire_annonce_le_type_choisi()
      {
        let (buf, _) = creation_dans_buffer(&ProjectCreationState {
          bibliotheque: true,
          ..Default::default()
        });
        assert!(
          texte(&buf).contains(" Nouvelle bibliothèque "),
          "le formulaire bibliothèque doit le dire :\n{}",
          texte(&buf)
        );
        let (buf, _) = creation_dans_buffer(&ProjectCreationState::default());
        assert!(
          texte(&buf).contains(" Nouveau projet "),
          "le formulaire projet doit le dire :\n{}",
          texte(&buf)
        );
      }

    /// Une entrée marquée bibliothèque porte un badge dans la liste et dans le
    /// panneau Détails.
    #[test]
    fn la_liste_et_les_details_marquent_une_bibliotheque()
      {
        let dir =
          std::env::temp_dir().join(format!("op-ui-badge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let projet = dir.join("monlib");
        std::fs::create_dir_all(&projet).unwrap();
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "monlib": {
                    "name": "monlib",
                    "path": projet.to_str().unwrap(),
                    "version": "1.0.0",
                    "version_debug": "1.0.0",
                    "description": "",
                    "tarif_mode": "manuel",
                    "est_bibliotheque": true
                }
            }
        });
        std::fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let mut app = crate::App::new_light(registre.clone(), crate::SortOrder::Modified);
        app.filtered = (0..app.projects.len()).collect();
        app.list_state.select(Some(0));
        let mut term = Terminal::new(TestBackend::new(110, 34)).unwrap();
        term.draw(|f| {
          super::ui(f, &mut app, &crate::InputMode::Normal, &ProjectCreationState::default())
        })
        .unwrap();
        let rendu = texte(term.backend().buffer());
        assert!(rendu.contains("Bibliothèque"), "le badge doit apparaître :\n{rendu}");
        assert!(
          rendu.contains(" ·  Bibliothèque"),
          "les Détails doivent porter le badge :\n{rendu}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
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
        rendre_popup_en(app, popup, 110, 34)
      }

    /// Rendu du popup dans une taille imposée : c'est la largeur qui décide
    /// d'une troncature de colonne.
    fn rendre_popup_en(app: &crate::App, popup: &crate::Popup, largeur: u16, hauteur: u16) -> String
      {
        texte(&popup_dans_buffer(app, popup, largeur, hauteur))
      }

    /// Rendu du popup dans un buffer : seul niveau où le style d'une cellule
    /// reste observable, et où l'on peut raisonner sur ce qui s'affiche.
    fn popup_dans_buffer(
        app: &crate::App,
        popup: &crate::Popup,
        largeur: u16,
        hauteur: u16,
      ) -> Buffer
      {
        let mut term = Terminal::new(TestBackend::new(largeur, hauteur)).unwrap();
        term.draw(|f| render_popup_unifie(f, f.area(), app, popup)).unwrap();
        term.backend().buffer().clone()
      }

    /// Un projet dont la dernière session a un coût dans une devise convertible.
    ///
    /// La sélection est forcée sur le projet : le panneau Détails ne lit que le
    /// projet sélectionné, et une application fraîchement chargée n'en a pas.
    fn projet_avec_cout_converti(app: &mut crate::App)
      {
        app.filtered = (0..app.projects.len()).collect();
        app.list_state.select(Some(0));
        let p = &mut app.projects[0];
        p.total_cost = "12,00 €".to_string();
        p.session_count = "3".to_string();
        p.last_session_cost = "4,57 $".to_string();
        p.last_session_cost_local = "4,20 €".to_string();
        p.last_model = "claude".to_string();
        p.last_time_created = "01/10/2026 12:00".to_string();
      }

    /// L'onglet Coûts a une colonne « Converti », vide jusqu'ici. Elle doit
    /// porter l'équivalent en devise locale, à côté du montant facturé.
    #[test]
    fn la_colonne_converti_porte_le_montant_local()
      {
        let (mut app, _dir) = app_de_test("projet-converti");
        projet_avec_cout_converti(&mut app);
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let rendu = rendre_popup(&app, &popup);
        assert!(rendu.contains("Converti"), "la colonne doit rester là :\n{rendu}");
        assert!(
          rendu.contains("4,57 $"),
          "le montant d'origine doit rester affiché :\n{rendu}"
        );
        assert!(
          rendu.contains("4,20 €"),
          "le montant converti doit remplir la colonne « Converti » :\n{rendu}"
        );
      }

    /// Régression : la colonne « Devise » déduit la devise en cherchant un
    /// symbole dans le montant d'origine. Si elle recevait un montant combiné,
    /// « 4,20 € · 4,57 $ » porterait les deux symboles et le premier test
    /// rencontré gagnerait — la ligne serait annoncée en USD alors que son
    /// montant local est en euros.
    #[test]
    fn la_colonne_devise_decrit_la_devise_facturee()
      {
        let (mut app, _dir) = app_de_test("projet-devise");
        projet_avec_cout_converti(&mut app);
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let rendu = rendre_popup(&app, &popup);
        assert!(
          rendu.contains("$ (USD)"),
          "la devise facturée est le dollar, même convertie en euros :\n{rendu}"
        );
        assert!(
          !rendu.contains("€ (EUR)"),
          "la devise locale ne doit pas remplacer la devise facturée :\n{rendu}"
        );
      }

    /// Sans taux connu, la colonne « Converti » reste vide et la ligne porte
    /// « — » comme avant le changement.
    #[test]
    fn sans_taux_la_colonne_converti_reste_vide()
      {
        let (mut app, _dir) = app_de_test("projet-sans-taux");
        projet_avec_cout_converti(&mut app);
        app.projects[0].last_session_cost_local = String::new();
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let rendu = rendre_popup(&app, &popup);
        assert!(rendu.contains("4,57 $"), "le montant d'origine reste :\n{rendu}");
        assert!(
          !rendu.contains("4,20 €"),
          "rien ne doit être inventé sans taux :\n{rendu}"
        );
      }

    /// La vue principale, rendue dans une taille imposée : c'est la largeur
    /// qui décide d'une troncature de colonne.
    fn rendre_vue_en(app: &mut crate::App, largeur: u16, hauteur: u16) -> String
      {
        let mut term = Terminal::new(TestBackend::new(largeur, hauteur)).unwrap();
        let mode = crate::InputMode::Normal;
        term.draw(|f| ui(f, app, &mode, &crate::ProjectCreationState::default())).unwrap();
        texte(term.backend().buffer())
      }

    /// Les deux montants tiennent dans la colonne « Coût » du panneau
    /// Détails, y compris quand la fenêtre rétrécit. C'est une colonne de
    /// 35 % d'un panneau lui-même à 65 % de la largeur : la place manque
    /// vite, et un montant tronqué au milieu d'un séparateur se lit comme une
    /// erreur d'affichage.
    #[test]
    fn les_deux_montants_tiennent_en_page_etroite()
      {
        let (mut app, _dir) = app_de_test("projet-etroit");
        projet_avec_cout_converti(&mut app);
        for largeur in [80u16, 100, 120]
          {
            let rendu = rendre_vue_en(&mut app, largeur, 34);
            assert!(
              rendu.contains("4,20 €") && rendu.contains("4,57 $"),
              "les deux montants doivent rester lisibles en {largeur} colonnes :\n{rendu}"
            );
          }
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

    /// La note comptable d'un projet rappelle que les crédits versés aux
    /// bibliothèques ne s'additionnent pas au coût du projet.
    #[test]
    fn la_synthese_d_un_projet_rappelle_les_bibliotheques_non_additionnees()
      {
        let (app, _dir) = app_de_test("note-projet");
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let rendu = rendre_popup(&app, &popup);
        assert!(
            rendu.contains("Dont bibliothèques (non additionnés)"),
            "la note doit rappeler le non-addition :\n{rendu}"
        );
      }

    /// Pour une bibliothèque, la synthèse affiche les apports reçus des
    /// projets, distincts de son propre coût.
    #[test]
    fn la_synthese_d_une_bibliotheque_affiche_les_apports_recus()
      {
        let (mut app, _dir) = app_de_test("note-lib");
        app.projects[0].est_bibliotheque = true;
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let rendu = rendre_popup(&app, &popup);
        assert!(
            rendu.contains("Apports reçus des projets"),
            "la note des apports reçus doit apparaître :\n{rendu}"
        );
      }

    /// Rendu du seul bloc Synthèse : c'est le seul niveau où l'on peut dire
    /// lesquelles de ses lignes viennent de lui et non de l'en-tête voisin.
    fn synthese_dans_buffer(projet: &crate::ProjectEntry) -> Buffer
      {
        let mut term = Terminal::new(TestBackend::new(80, 3)).unwrap();
        term.draw(|f| render_bloc_synthese(f, f.area(), projet, None)).unwrap();
        term.backend().buffer().clone()
      }

    /// Le travail accompli et ce qu'il a coûté doivent se lire en un coup
    /// d'œil, au-dessus du détail : temps total, sessions, tokens, coût.
    #[test]
    fn la_synthese_monte_le_travail_et_le_cout()
      {
        let (mut app, _dir) = app_de_test("synthese-travail");
        app.projects[0].total_duree = "12 h 05 min".to_string();
        app.projects[0].session_count = "3".to_string();
        app.projects[0].total_tokens = "1 234 567".to_string();
        app.projects[0].total_cost = "4,20 €".to_string();
        let projet = app.projects[0].clone();
        let rendu = texte(&synthese_dans_buffer(&projet));
        for attendu in
          [
            "Synthèse",
            "Temps total",
            "Sessions",
            "Tokens",
            "Coût total",
            "12 h 05 min",
            "1 234 567",
            "4,20 €",
          ]
        {
          assert!(
            rendu.contains(attendu),
            "« {attendu} » manque dans la synthèse :\n{rendu}"
          );
        }

        // Mesures absentes : « — », jamais un zéro qui laisserait croire à
        // une session menée sans temps ni tokens.
        let mut vide = projet;
        vide.total_duree.clear();
        vide.session_count.clear();
        vide.total_tokens.clear();
        vide.total_cost.clear();
        let rendu = texte(&synthese_dans_buffer(&vide));
        assert!(
          rendu.contains("—"),
          "une mesure absente doit retomber sur « — » :\n{rendu}"
        );
        assert!(
          !rendu.contains('0'),
          "aucun zéro ne doit se substituer à une mesure absente :\n{rendu}"
        );
      }

    /// Le rappel de « — » prenait une seule rangée : la fin du texte — celle
    /// qui dit que les tokens ont bien été consommés — disparaissait en
    /// dessous d'une centaine de colonnes. Il doit tenir sur toute sa hauteur.
    #[test]
    fn le_rappel_du_tiret_tient_sur_toutes_ses_lignes()
      {
        let (mut app, _dir) = app_de_test("rappel-complet");
        app.projects[0].total_cost = "—".to_string();
        app.projects[0].last_session_cost = "—".to_string();
        app.projects[0].total_tokens = "1 234 567".to_string();
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let rendu = rendre_popup_en(&app, &popup, 80, 24);
        assert!(
          rendu.contains("malgré les tokens consommés."),
          "le rappel doit être rendu en entier :\n{rendu}"
        );
      }

    /// Le rappel et l'indicateur de défilement occupent des rangées **prévues**
    /// : la ligne de données qui suit les en-têtes ne doit être recouverte ni
    /// par l'un ni par l'autre.
    #[test]
    fn le_rappel_ne_couvre_pas_la_donnee_du_tableau()
      {
        let (mut app, _dir) = app_de_test("rappel-ligne");
        // Coût de la session non communiqué — c'est ce qui déclenche le
        // rappel —, mais total valorisé : le tableau a bien deux lignes de
        // données, la dernière étant la ligne TOTAL.
        app.projects[0].total_cost = "4,20 €".to_string();
        app.projects[0].last_session_cost = "—".to_string();
        app.projects[0].session_count = "3".to_string();
        app.projects[0].total_tokens = "1 234 567".to_string();
        let popup = popup_de_test(&app, crate::Onglet::Couts);
        let buf = popup_dans_buffer(&app, &popup, 80, 24);
        let entetes = rang(&buf, "Date/Heure")
            .expect("les en-têtes du tableau doivent être là");
        let rappel = rang(&buf, "« — »").expect("le rappel doit être affiché");
        let indicateur = rang(&buf, "▲").expect("l'indicateur doit être affiché");
        assert!(
          rappel >= entetes + 2,
          "la ligne de données doit tenir entre l'en-tête et le rappel :\n{}",
          texte(&buf)
        );
        assert!(
          indicateur > rappel,
          "l'indicateur doit rester sous le rappel :\n{}",
          texte(&buf)
        );
        // La ligne TOTAL n'est rendue que si la place la garde au-dessus du
        // rappel ; elle peut être hors champ, scrollable, mais jamais dedans.
        if let Some(total) = rang(&buf, "TOTAL")
          {
            assert!(
              total < rappel,
              "la ligne TOTAL ne doit jamais tomber sous le rappel :\n{}",
              texte(&buf)
            );
          }

        // Défilé jusqu'au bout : la ligne TOTAL, dernière des données, doit
        // s'afficher. Elle n'a le droit ni de passer sous le rappel — où son
        // texte serait effacé au profit de celui-ci — ni sous l'indicateur.
        let mut fin = popup_de_test(&app, crate::Onglet::Couts);
        fin.couts.scroll_offset = usize::MAX;
        let buf = popup_dans_buffer(&app, &fin, 80, 24);
        assert!(
          rang(&buf, "TOTAL").is_some(),
          "défilé en fin, la ligne TOTAL doit rester affichable :\n{}",
          texte(&buf)
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
          ("cohérence", InputMode::Coherence(crate::CoherenceState::default())),
        ]
      }

    /// Régression B2 : l'aide annonçait `t Valorisation` et `←/→ Changer onglet`,
    /// deux touches qui n'ont jamais été branchées. Le fléau contraire existe
    /// aussi : promettre des touches qu'on vient de retirer. `e`, `c` et `s`
    /// ont été libérés au profit de `b`, et la barre ne doit plus annoncer ni
    /// les unes ni les libellés qui leur appartenaient.
    #[test]
    fn la_barre_d_aide_n_annonce_plus_de_touches_inexistantes()
      {
        let rendu = rendre_ecran(&InputMode::Normal, 120, 40);
        let barre = rendu.lines().last().unwrap_or("").to_string();
        assert!(
          !barre.contains("t Valorisation"),
          "« t Valorisation » n'existe pas dans le code :\n{rendu}"
        );
        for libere in ["Modifier", "Coûts", "Préférences"]
          {
            assert!(
              !barre.contains(libere),
              "« {libere} » appartient à une touche retirée : {barre}"
            );
          }
        for reel in ["Réglages", "Bump", "Module", "Nouveau", "F1"]
          {
            assert!(
              barre.contains(reel),
              "« {reel} » est une touche réelle, elle doit être annoncée : {barre}"
            );
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
            let touches: Vec<String> =
              crate::keymap::contextuel(&mode).iter().map(|r| r.touche().to_string()).collect();
            let rendu = rendre_ecran(&mode, 200, 40);
            for faux in ["t Valorisation", "←/→ Changer onglet"]
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
        for geste in ["Naviguer", "Ouvrir", "Réglages", "Bump"]
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

    /// L'aide de l'option courante doit changer avec ↑/↓ : sans elle, deux
    /// options voisines se reconnaîtraient à leur valeur, mais le réglage lui
    /// même resterait muet pour qui ne connaît pas déjà l'écran.
    #[test]
    fn l_aide_des_parametres_suit_l_option_active()
      {
        let (app, _dir) = app_de_test("param-aide");
        let fragments = [
          "par date de modification",
          "choisit la colonne",
          "quand les droits manquent",
          "journalise la sortie",
          "code ISO",
        ];
        for (actif, attendu) in fragments.iter().enumerate()
          {
            let mut popup = popup_de_test(&app, crate::Onglet::Parametres);
            popup.parametres.actif = actif;
            let rendu = rendre_popup_en(&app, &popup, 110, 34);
            assert!(
              rendu.contains(attendu),
              "l'aide doit décrire l'option {actif} :\n{rendu}"
            );
            for (autre_i, absent) in fragments.iter().enumerate()
              {
                if autre_i != actif
                  {
                    assert!(
                      !rendu.contains(absent),
                      "le mode d'emploi d'une autre option ne doit pas s'afficher :\n{rendu}"
                    );
                  }
              }
          }
      }

    /// La grille des colonnes est une vraie table : les ☑/☐ d'une même
    /// colonne partagent la même abscisse, sinon la liste n'est qu'une suite
    /// de cases collées qui dansent à chaque bascule.
    #[test]
    fn les_cases_des_colonnes_sont_aligees_sur_trois_colonnes()
      {
        let (app, _dir) = app_de_test("param-alignement");
        let popup = popup_de_test(&app, crate::Onglet::Parametres);
        let buf = popup_dans_buffer(&app, &popup, 110, 34);
        let rendu = texte(&buf);
        let titre_y = rang(&buf, "Colonnes affichées dans le détail")
          .expect("le titre du bloc des colonnes doit s'afficher");
        let rangees: Vec<Vec<u16>> = (1..=3)
          .map(|d| {
              let y = titre_y + d;
              let ligne = &rendu.lines().collect::<Vec<_>>()[y as usize];
              ligne
                .match_indices('☑')
                .chain(ligne.match_indices('☐'))
                .map(|(i, _)| i as u16)
                .collect::<Vec<_>>()
          })
          .collect();
        assert_eq!(rangees.len(), 3, "9 colonnes sur 3 rangées :\n{rendu}");
        for rangee in &rangees
          {
            assert_eq!(
              rangee.len(),
              3,
              "chaque rangée porte exactement trois cases :\n{rendu}"
            );
          }
        let (c0, c1, c2) = (rangees[0][0], rangees[0][1], rangees[0][2]);
        assert!(
          c0 < c1 && c1 < c2,
          "les trois colonnes doivent se suivre :\n{rendu}"
        );
        for rangee in &rangees[1..]
          {
            assert_eq!(
              *rangee,
              [c0, c1, c2],
              "une case ne doit pas danser hors de sa colonne :\n{rendu}"
            );
          }
      }

    // --- Popup unifié -------------------------------------------------

  }
