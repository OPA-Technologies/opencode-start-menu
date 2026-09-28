use anyhow::Result;
use chrono::TimeZone;
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use num_format::{Locale as NumLocale, ToFormattedString};
use ratatui::{
    backend::CrosstermBackend,
    widgets::ListState,
    Terminal,
};
use std::{
    cmp::Ordering,
    env,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitStatus,
};

mod launch;
mod lock;
mod modules;
mod perms;
mod registry;
mod ui;

use crate::launch::{run_launch_sequence, LaunchKind};
use crate::registry::ProjectEntry;

#[derive(Parser, Debug)]
#[command(
    name = "op",
    version,
    about = "OpenCode Start Menu - Interactive project launcher",
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short = 'r', long = "root", help = "Launch as root via sudo -i")]
    root: bool,

    #[arg(short = 'l', long = "list", help = "List projects without interactive menu")]
    list: bool,

    #[arg(short = 'V', long = "version", help = "Print variant version (self or project passthrough)")]
    version: bool,

    #[arg(short = 's', value_enum, default_value_t = SortOrder::Modified,
          help = "Tri : -sM par date (défaut) · -sN alphabétique")]
    sort: SortOrder,

    #[arg(long = "sort-by-name", conflicts_with = "sort_by_date",
          help = "Trier alphabétiquement (équivalent à -sN)")]
    sort_by_name: bool,

    #[arg(long = "sort-by-date", conflicts_with = "sort_by_name",
          help = "Trier par date de modification, le plus ancien d'abord (défaut, équivalent à -sM)")]
    sort_by_date: bool,

    #[arg(short = 'h', long = "help", help = "Show help")]
    help: bool,

    #[arg(help = "Project keyword (id, name, or path)")]
    keyword: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Create a new project interactively
    New,
}

#[derive(Clone, Copy, PartialEq, Debug, ValueEnum)]
enum SortOrder {
    #[value(name = "M")]
    Modified,
    #[value(name = "N")]
    Name,
}

pub(crate) struct App {
    pub(crate) projects: Vec<ProjectEntry>,
    pub(crate) filtered: Vec<usize>,
    pub(crate) list_state: ListState,
    pub(crate) show_new: bool,
    mode: LaunchMode,
    pub(crate) status_messages: Vec<String>,
    registry_path: PathBuf,
    opencode_db_path: PathBuf,
    sort_order: SortOrder,
    pending_launch: Option<PathBuf>,
    pending_sudo: bool,
    pending_name: Option<String>,
    pending_bare: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum LaunchMode {
    Normal,
    Root,
}

pub(crate) enum InputMode {
    Normal,
    CreatingProject(ProjectCreationState),
    EditingProject(ProjectEditState),
    SelectModule {
        project_idx: usize,
        modules: Vec<PathBuf>,
        selected: usize,
    },
}

#[derive(Clone, Default)]
pub(crate) struct ProjectCreationState {
    pub(crate) active_field: usize,
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) description: String,
    pub(crate) status: Option<String>,
    pub(crate) version: String,
    pub(crate) input_buffer: String,
    pub(crate) input_offset: usize,
    pub(crate) confirm_status: bool,
    pub(crate) rewrite_mode: bool,
    pub(crate) known_statuses: Vec<String>,
    pub(crate) status_menu_open: bool,
    pub(crate) status_filter: String,
    pub(crate) status_cursor: usize,
    // Saisie libre d'un nouveau statut (entrée « + Nouveau statut » du menu).
    pub(crate) status_free_text: bool,
}

#[derive(Clone)]
pub(crate) struct ProjectEditState {
    pub(crate) project_idx: usize,
    pub(crate) active_field: usize,
    pub(crate) editing: bool,
    pub(crate) input_buffer: String,
    pub(crate) input_offset: usize,
    pub(crate) pending_version: String,
    pub(crate) pending_status: String,
    pub(crate) pending_description: String,
    pub(crate) error: Option<String>,
    pub(crate) rewrite_mode: bool,
    pub(crate) status_menu_open: bool,
    pub(crate) status_filter: String,
    pub(crate) status_cursor: usize,
}

impl Default for ProjectEditState
  {
    fn default() -> Self
      {
        Self {
            project_idx: 0,
            active_field: 0,
            editing: false,
            input_buffer: String::new(),
            input_offset: 0,
            pending_version: String::new(),
            pending_status: String::new(),
            pending_description: String::new(),
            error: None,
            rewrite_mode: true,
            status_menu_open: false,
            status_filter: String::new(),
            status_cursor: 0,
          }
      }
  }

impl ProjectEditState
  {
    fn from_project(project: &ProjectEntry, project_idx: usize) -> Self
      {
        Self {
            project_idx,
            active_field: 0,
            editing: false,
            input_buffer: String::new(),
            input_offset: 0,
            pending_version: project.version_debug.clone(),
            pending_status: project.status_debug.clone(),
            pending_description: project.description.clone(),
            error: None,
            rewrite_mode: true,
            status_menu_open: false,
            status_filter: String::new(),
            status_cursor: 0,
          }
      }
  }

// Libellé de variante : version + statut, séparés par « _ ».
// La partie « pré-release » d'une version (ex. 0.9.9-beta) est écartée.
fn variant_label(version: &str, status: &str) -> String {
    let core = version.split('-').next().unwrap_or(version);
    let stat = status.replace(['-', ' '], "_");
    if stat.is_empty() {
        core.to_string()
    } else {
        format!("{core}_{stat}")
    }
}

// --- Édition de texte insensible aux octets UTF-8 : l'offset est un index
// --- de caractères, converti en octets uniquement pour insert/remove.

fn byte_index_for_char(s: &str, char_idx: usize) -> usize
  {
    s.char_indices()
      .nth(char_idx)
      .map(|(i, _)| i)
      .unwrap_or(s.len())
  }

fn insert_char_at(s: &mut String, c: char, char_idx: usize)
  {
    let byte = byte_index_for_char(s, char_idx);
    s.insert(byte, c);
  }

fn delete_char_at(s: &mut String, char_idx: usize)
  {
    let byte = byte_index_for_char(s, char_idx);
    s.remove(byte);
  }

/// Saisie par masque de la version (x.y.z) : chiffres uniquement, point
/// autorisé si le segment courant n'est pas vide, point inséré
/// automatiquement quand un segment atteint 3 chiffres et qu'un chiffre
/// supplémentaire est frappé. Renvoie None si le caractère est rejeté.
fn version_mask_insert(buffer: &str, char_idx: usize, c: char) -> Option<(String, usize)>
  {
    if c.is_ascii_digit()
      {
        let byte = byte_index_for_char(buffer, char_idx);
        let seg_digits = buffer[..byte]
          .rsplit('.')
          .next()
          .unwrap_or("")
          .chars()
          .filter(|ch| ch.is_ascii_digit())
          .count();
        let mut s = buffer.to_string();
        if seg_digits >= 3
          {
            s.insert(byte, '.');
            s.insert(byte + 1, c);
            Some((s, char_idx + 2))
          }
        else
          {
            s.insert(byte, c);
            Some((s, char_idx + 1))
          }
      }
    else if c == '.'
      {
        if char_idx == 0
          {
            return None;
          }
        let byte = byte_index_for_char(buffer, char_idx);
        let prev_byte = byte_index_for_char(buffer, char_idx - 1);
        let prev = buffer[prev_byte..byte].chars().next().unwrap();
        if prev.is_ascii_digit()
          {
            let mut s = buffer.to_string();
            s.insert(byte, '.');
            Some((s, char_idx + 1))
          }
        else
          {
            None
          }
      }
    else
      {
        None
      }
  }

/// Liste éligible du menu déroulant statut : préfixe « deprecated » incluse,
/// filtre insensible à la casse.
pub(crate) fn filtered_statuses(filter: &str) -> Vec<&'static str>
  {
    let f = filter.to_lowercase();
    if f.is_empty()
      {
        registry::STATUSES.to_vec()
      }
    else
      {
        registry::STATUSES
          .iter()
          .filter(|s| s.to_lowercase().starts_with(&f))
          .copied()
          .collect()
      }
  }

/// Incrémente le dernier étage (z) d'une version x.y.z.
fn bump_patch(version: &str) -> Option<String>
  {
    let parts: Vec<&str> = version.trim().split('.').collect();
    if parts.len() != 3
      {
        return None;
      }
    let mut nums = Vec::with_capacity(3);
    for p in &parts
      {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())
          {
            return None;
          }
        nums.push(p.parse::<u64>().ok()?);
      }
    nums[2] += 1;
    Some(format!("{}.{}.{}", nums[0], nums[1], nums[2]))
  }

/// Réécrit la ligne `version = "…"` de la section `[package]` de Cargo.toml,
/// ou l'ajoute juste après l'en-tête si elle est absente. Retourne false si
/// le manifeste est absent ou n'a pas de section `[package]`.
fn bump_manifest_version(dir: &Path, new_version: &str) -> Result<bool>
  {
    let cargo = dir.join("Cargo.toml");
    let Ok(content) = fs::read_to_string(&cargo) else
      {
        return Ok(false);
      };
    let mut out = String::with_capacity(content.len() + 32);
    let mut in_package = false;
    let mut version_seen = false;
    let mut version_injected = false;
    for part in content.split_inclusive('\n')
      {
        let trimmed = part.trim();
        let is_section = trimmed.starts_with('[');
        if is_section
          {
            // Fin d'un [package] sans version : on injecte avant la section suivante.
            if in_package && !version_seen && !version_injected
              {
                out.push_str(&format!("version = \"{new_version}\"\n"));
                version_injected = true;
              }
            in_package = trimmed == "[package]";
            if in_package
              {
                version_seen = false;
              }
          }
        else if in_package && !version_seen
          {
            if let Some(eq) = trimmed.find('=')
              {
                if trimmed[..eq].trim() == "version"
                  {
                    let rest = trimmed[eq + 1..].trim();
                    if rest.starts_with('"') && rest.split('"').count() > 1
                      {
                        let lead = part.len() - part.trim_start().len();
                        out.push_str(&format!("{}version = \"{new_version}\"", &part[..lead]));
                        if part.ends_with('\n')
                          {
                            out.push('\n');
                          }
                        version_seen = true;
                        version_injected = true;
                        continue;
                      }
                  }
              }
          }
        out.push_str(part);
      }
    if in_package && !version_seen && !version_injected
      {
        if !out.is_empty() && !out.ends_with('\n')
          {
            out.push('\n');
          }
        out.push_str(&format!("version = \"{new_version}\"\n"));
        version_injected = true;
      }
    if version_injected
      {
        fs::write(&cargo, out)?;
      }
    Ok(version_injected)
  }

pub(crate) const MAX_MESSAGE_LINES: usize = 6;

impl App {
    fn new(registry_path: PathBuf, sort_order: SortOrder) -> Self {
        let opencode_db_path = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("opencode/opencode.db");
        let mut app = Self {
            projects: Vec::new(),
            filtered: Vec::new(),
            list_state: ListState::default(),
            show_new: true,
            mode: LaunchMode::Normal,
            status_messages: Vec::new(),
            registry_path,
            opencode_db_path,
            sort_order,
            pending_launch: None,
            pending_sudo: false,
            pending_name: None,
            pending_bare: false,
        };
        match registry::load_registry(&app.registry_path) {
            Ok(projects) => app.projects = projects,
            Err(e) => {
                app.push_error(format!("Registre illisible : {e}"));
            }
        }
        // Rescan au démarrage : chaque projet est relu depuis son Cargo.toml disque,
        // pour que la liste reflète l'état réel du répertoire éponyme dès l'affichage.
        if let Err(e) = rescan_repertoire(&mut app) {
            app.push_error(format!("Rescan au démarrage échoué : {e}"));
        }
        finalize_project_list(&mut app);
        app
    }

    fn filter(&mut self, needle: &str, mode: LaunchMode) {
        self.mode = mode;
        let nlow = needle.to_lowercase();
        self.filtered = self
            .projects
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.id.to_lowercase().contains(&nlow)
                    || p.name.to_lowercase().contains(&nlow)
                    || p.path.to_string_lossy().to_lowercase().contains(&nlow)
            })
            .map(|(i, _)| i)
            .collect();

        if self.filtered.len() == 1 {
            self.launch(self.filtered[0]);
        } else {
            self.list_state.select(if self.filtered.is_empty() { None } else { Some(0) });
        }
    }

    fn matching_indexes(&self, needle: &str) -> Vec<usize> {
        let nlow = needle.to_lowercase();
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                p.id.to_lowercase().contains(&nlow)
                    || p.name.to_lowercase().contains(&nlow)
                    || p.path.to_string_lossy().to_lowercase().contains(&nlow)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn launch(&mut self, idx: usize) {
        let project = &self.projects[idx];
        let dir = &project.path;
        if !dir.exists() {
            self.push_error(format!("Dossier introuvable : {}", sanitize(&dir.to_string_lossy())));
            return;
        }
        self.pending_launch = Some(dir.clone());
        self.pending_sudo = matches!(self.mode, LaunchMode::Root);
        self.pending_name = Some(project.name.clone());
    }

    fn next(&mut self) {
        let total = self.entry_count();
        if total == 0 {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i + 1 >= total {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    fn previous(&mut self) {
        let total = self.entry_count();
        if total == 0 {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    total - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Nombre d'entrées affichées : projets filtrés + éventuel « Nouveau
    /// projet » + entrée permanente « Sans projet ».
    fn entry_count(&self) -> usize {
        self.filtered.len() + (self.show_new as usize) + 1
    }

    pub(crate) fn is_bare_selected(&self) -> bool {
        self.list_state.selected() == Some(self.filtered.len() + (self.show_new as usize))
    }

    /// Prépare une session vierge dans `$HOME` : aucun verrou, aucun résumé,
    /// aucune todo, aucune liste de projets, aucun `--continue`, aucun sudo.
    pub(crate) fn set_bare_launch(&mut self) {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        self.pending_launch = Some(home);
        self.pending_bare = true;
        self.pending_sudo = false;
        self.pending_name = Some("Sans projet".to_string());
    }

    /// L'entrée « Nouveau projet » (index `filtered.len()`) est sélectionnée.
    pub(crate) fn is_new_selected(&self) -> bool {
        self.show_new && self.list_state.selected() == Some(self.filtered.len())
    }

    /// Index du projet (`self.projects[idx]`) correspondant à la sélection,
    /// ou `None` si l'entrée « Nouveau projet » ou « Sans projet » est choisie.
    pub(crate) fn selected_project_idx(&self) -> Option<usize> {
        let sel = self.list_state.selected()?;
        if sel < self.filtered.len() {
            Some(self.filtered[sel])
        } else {
            None
        }
    }

    fn confirm_selection(&mut self) -> Option<InputMode> {
        if self.is_bare_selected() {
            self.set_bare_launch();
            None
        } else if self.is_new_selected() {
            Some(InputMode::CreatingProject(ProjectCreationState::default()))
        } else if let Some(idx) = self.selected_project_idx() {
            self.launch(idx);
            None
        } else {
            None
        }
    }

    // Remplace les messages affichés (bilans du rescan, édition, modules…).
    fn push_info(&mut self, msg: String)
      {
        self.status_messages.clear();
        self.status_messages.extend(msg.lines().map(str::to_string));
      }

    // Ajoute un message d'erreur, en bornant la liste aux N plus récents.
    fn push_error(&mut self, msg: String)
      {
        for line in msg.lines()
          {
            self.status_messages.push(line.to_string());
          }
        while self.status_messages.len() > MAX_MESSAGE_LINES
          {
            self.status_messages.remove(0);
          }
      }
}

fn print_help() {
    println!("Usage : op [mot-clé|id|nom]");
    println!();
    println!("  op            menu interactif (↑/↓ · Entrée · q) + création de projet");
    println!("  op <mot>      ouvre le projet unique correspondant, ou menu restreint");
    println!("                si plusieurs projets correspondent au mot-clé");
    println!("  op --root <mot>");
    println!("                ouvre le projet via « sudo -i » (root, environnement");
    println!("                de connexion root chargé)");
    println!("  op -l         liste les projets sans menu (variante Debug)");
    println!("  op -V         version du menu lui-même (variante Release)");
    println!("  op <mot> -V   version de l'exécutable du projet (passthrough)");
    println!("  op -sN        tri alphabétique (--sort-by-name)");
    println!("  op -sM        tri par dernière modification, le plus ancien d'abord");
    println!("                (défaut, --sort-by-date)");
    println!("  op -h         cette aide");
}

pub(crate) fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

#[cfg(unix)]
fn world_writable(parent: Option<&Path>) -> bool {
    parent
        .and_then(|p| fs::metadata(p).ok())
        .is_some_and(|m| {
            use std::os::unix::fs::PermissionsExt;
            m.permissions().mode() & 0o002 != 0
        })
}

#[cfg(not(unix))]
fn world_writable(_parent: Option<&Path>) -> bool {
    false
}

// ---- Formatage localisé (locale fr vs en) ----

fn get_locale() -> String {
    sys_locale::get_locale().unwrap_or_else(|| "en_US".to_string())
}

fn is_french_locale() -> bool {
    get_locale().starts_with("fr")
}

fn format_number(v: u64) -> String {
    let loc = if is_french_locale() { NumLocale::fr } else { NumLocale::en };
    v.to_formatted_string(&loc)
}

fn format_currency(cost: f64) -> String {
    let cents = (cost * 100.0).round() / 100.0;
    if is_french_locale() {
        format!("{:.2}", cents).replace('.', ",")
    } else {
        format!("{:.2}", cents)
    }
}

fn format_datetime(ts: i64) -> String {
    let dt = chrono::Local.timestamp_opt(ts, 0).single().unwrap_or_default();
    if is_french_locale() {
        dt.format("%d/%m/%Y %H:%M").to_string()
    } else {
        dt.format("%m/%d/%Y %H:%M").to_string()
    }
}

// ---- Détection du nom de binaire d'un projet (Cargo.toml) ----

fn binary_name_from_cargo(dir: &Path) -> Option<String> {
    let content = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let mut in_package = false;
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("[package]") {
            in_package = true;
            continue;
        }
        if in_package && t.starts_with('[') {
            break;
        }
        if in_package {
            if let Some((k, v)) = t.split_once('=') {
                if k.trim() == "name" {
                    let name = v.trim().trim_matches('"').to_string();
                    if !name.is_empty() {
                        return Some(name);
                    }
                }
            }
        }
    }
    None
}

fn exit_code_of(status: io::Result<ExitStatus>) -> i32 {
    status
        .ok()
        .and_then(|s| s.code())
        .unwrap_or(1)
}

fn run_project_version_passthrough(project: &ProjectEntry) -> i32 {
    let name = binary_name_from_cargo(&project.path)
        .unwrap_or_else(|| env!("CARGO_PKG_NAME").to_string());
    for profile in ["release", "debug"] {
        let bin = project.path.join("target").join(profile).join(&name);
        if bin.exists() {
            let status = std::process::Command::new(bin).arg("-V").status();
            return exit_code_of(status);
        }
    }
    let status = std::process::Command::new("cargo")
        .args(["run", "-q", "--", "-V"])
        .current_dir(&project.path)
        .status();
    exit_code_of(status)
}

fn self_variant_label(app: &App) -> String {
    for p in &app.projects {
        if p.path == env::current_dir().unwrap_or_default() || p.name == "OpenCode Start Menu" {
            return variant_label(&p.version_debug, &p.status_debug);
        }
    }
    variant_label(env!("CARGO_PKG_VERSION"), "")
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let registry_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode/projects.json");

    // Ensure opencode exists
    match which::which("opencode") {
        Err(_) => {
            eprintln!("Avertissement : la commande « opencode » est introuvable dans le PATH.");
        }
        Ok(path) => {
            let under_home = dirs::home_dir().is_some_and(|h| path.starts_with(h));
            if under_home || world_writable(path.parent()) {
                eprintln!(
                    "Avertissement : « opencode » résolu vers {} (chemin sous le dossier personnel ou modifiable par les autres).",
                    path.display()
                );
            }
        }
    }

    let sort_order = if cli.sort_by_name {
        SortOrder::Name
    } else if cli.sort_by_date {
        SortOrder::Modified
    } else {
        cli.sort
    };

    let mut app = App::new(registry_path.clone(), sort_order);

    if cli.help {
        print_help();
        return Ok(());
    }

    if cli.list {
        if app.projects.is_empty() {
            println!("Aucun projet enregistré. Utilisez : op");
        } else {
            for p in &app.projects {
                println!("  \x1b[1;34m* {}\x1b[0m", sanitize(&p.name));
                println!("    ID       : \x1b[36m{}\x1b[0m", sanitize(&p.id));
                println!("    Chemin   : {}", sanitize(&p.path.to_string_lossy()));
                println!("    Descript.: {}", sanitize(&p.description));
                println!("    Modifié  : {}", sanitize(&p.modified));
                println!("    Variante : {}", sanitize(&variant_label(&p.version_debug, &p.status_debug)));
                println!();
            }
        }
        return Ok(());
    }

    if cli.version {
        if let Some(keyword) = cli.keyword.as_deref() {
            let idx = app.matching_indexes(keyword);
            match idx.as_slice() {
                [] => {
                    eprintln!("Aucun projet ne correspond à : {keyword}");
                }
                [i] => {
                    let code = run_project_version_passthrough(&app.projects[*i]);
                    std::process::exit(code);
                }
                _ => {
                    eprintln!("Plusieurs projets correspondent à : {keyword} (mot-clé ambigu)");
                }
            }
        } else {
            println!("{}", self_variant_label(&app));
        }
        return Ok(());
    }

    match cli.command {
        Some(Commands::New) => {
            // Will be handled in TUI
        }
        None => {}
    }

    if let Some(keyword) = cli.keyword {
        app.filter(&keyword, if cli.root { LaunchMode::Root } else { LaunchMode::Normal });
        if let Some(dir) = app.pending_launch.take() {
            let name = app.pending_name.take()
                .unwrap_or_else(|| dir.display().to_string());
            run_launch_sequence(LaunchKind::Resume, &name, &dir, app.pending_sudo)?;
        }
        return Ok(());
    }

    if cli.root {
        eprintln!("Erreur : l'option --root requiert un mot-clé de projet.");
        return Ok(());
    }

    // Interactive TUI mode
    run_tui(app)
}

fn run_tui(mut app: App) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut input_mode = InputMode::Normal;

    loop {
        // Extract creation/edit states from input_mode for rendering
        let creation_state = match &input_mode {
            InputMode::CreatingProject(state) => state.clone(),
            _ => ProjectCreationState::default(),
        };
        let edit_state = match &input_mode {
            InputMode::EditingProject(state) => state.clone(),
            _ => ProjectEditState::default(),
        };

        terminal.draw(|f| ui::ui(f, &mut app, &input_mode, &creation_state, &edit_state))?;

        if let Event::Key(key) = event::read()? {
            if key.kind != KeyEventKind::Press {
                continue;
            }

            input_mode = match input_mode {
                InputMode::Normal => match key.code {
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        rescan_repertoire(&mut app)?;
                        InputMode::Normal
                    }
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.next();
                        InputMode::Normal
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.previous();
                        InputMode::Normal
                    }
                    KeyCode::Enter => {
                        if let Some(mode) = app.confirm_selection() {
                            mode
                        } else {
                            InputMode::Normal
                        }
                    }
                    KeyCode::Char('n') => {
                        InputMode::CreatingProject(ProjectCreationState::default())
                    }
                    KeyCode::Char('e') | KeyCode::Char('E') => {
                        if let Some(idx) = app.selected_project_idx() {
                            let state = ProjectEditState::from_project(&app.projects[idx], idx);
                            InputMode::EditingProject(state)
                        } else {
                            InputMode::Normal
                        }
                    }
                    KeyCode::Char('m') => {
                        if let Some(idx) = app.selected_project_idx() {
                            let modules = modules::list_modules(&app.projects[idx].path);
                            if modules.is_empty() {
                                app.push_info("Aucun module détecté dans ce projet".to_string());
                                InputMode::Normal
                            } else {
                                InputMode::SelectModule {
                                    project_idx: idx,
                                    modules,
                                    selected: 0,
                                }
                            }
                        } else {
                            InputMode::Normal
                        }
                    }
                    KeyCode::Char('v') => {
                        app.set_bare_launch();
                        InputMode::Normal
                    }
                    _ => InputMode::Normal,
                },
                InputMode::SelectModule {
                    project_idx,
                    modules,
                    selected,
                } => match key.code {
                    KeyCode::Esc => InputMode::Normal,
                    KeyCode::Down | KeyCode::Char('j') => {
                        let sel = if selected >= modules.len() { 0 } else { selected + 1 };
                        InputMode::SelectModule {
                            project_idx,
                            modules,
                            selected: sel,
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        let sel = if selected == 0 { modules.len() } else { selected - 1 };
                        InputMode::SelectModule {
                            project_idx,
                            modules,
                            selected: sel,
                        }
                    }
                    KeyCode::Enter => {
                        let project = &app.projects[project_idx];
                        let (dir, label) = if selected == 0 {
                            (project.path.clone(), "racine".to_string())
                        } else {
                            let m = &modules[selected - 1];
                            let label = m
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| m.display().to_string());
                            (m.clone(), label)
                        };
                        app.pending_launch = Some(dir);
                        app.pending_sudo = matches!(app.mode, LaunchMode::Root);
                        app.pending_name = Some(format!("{} / {}", project.name, label));
                        InputMode::Normal
                    }
                    _ => InputMode::SelectModule {
                        project_idx,
                        modules,
                        selected,
                    },
                },
                InputMode::CreatingProject(mut state) => {
                    handle_creation_input(key, &mut state, &mut app)?
                }
                InputMode::EditingProject(mut state) => handle_edit_input(key, &mut state, &mut app)?,
            };
        }

        // Handle pending launch: suspend TUI, run child, resume TUI
        if let Some(dir) = app.pending_launch.take() {
            let sudo = app.pending_sudo;
            let name = app.pending_name.take()
                .unwrap_or_else(|| dir.display().to_string());

            // Suspend TUI menu
            disable_raw_mode()?;
            execute!(
                terminal.backend_mut(),
                LeaveAlternateScreen,
                DisableMouseCapture
            )?;
            terminal.show_cursor()?;
            terminal.backend_mut().flush()?;

            // Résumé + lancement d'opencode
            let kind = if app.pending_bare {
                LaunchKind::Bare
            } else {
                LaunchKind::Resume
            };
            app.pending_bare = false;
            let launch_ok = if let Err(e) = run_launch_sequence(kind, &name, &dir, sudo) {
                app.push_error(format!("Échec du lancement : {e}"));
                false
            } else {
                true
            };

            // Reprendre le TUI menu
            enable_raw_mode()?;
            execute!(
                terminal.backend_mut(),
                EnterAlternateScreen,
                EnableMouseCapture
            )?;
            terminal.clear()?;

            // AutoPatch : rescan immédiat après chaque session réussie pour
            // détecter une éventuelle nouvelle compilation et incrémenter z ,
            // avec suivi visuel dès le retour dans le menu.
            if launch_ok
              {
                if let Err(e) = rescan_repertoire(&mut app) {
                    app.push_error(format!("Rescan post-session échoué : {e}"));
                }
              }
        }
    }

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

// Remonte le buffer du champ actif depuis sa valeur propre.
fn load_creation_field(state: &mut ProjectCreationState)
  {
    state.input_buffer = match state.active_field
      {
        0 => state.id.clone(),
        1 => state.name.clone(),
        2 => state.path.clone(),
        3 => state.description.clone(),
        4 => state.status.clone().unwrap_or_default(),
        5 => state.version.clone(),
        _ => String::new(),
      };
    state.input_offset = state.input_buffer.chars().count();
    state.rewrite_mode = true;
  }

fn rescan_repertoire(app: &mut App) -> Result<()>
  {
    let mut changed = 0usize;
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut written = false;
    app.projects.retain(|p|
      {
        if p.path.exists() && !registry::is_excluded(&p.path)
          {
            true
          }
        else
          {
            removed += 1;
            false
          }
      });
    if removed > 0
      {
        written = true;
      }
    if let Some(root) = projects_root(&app.projects)
      {
        added = registry::reindex_from_disk(&root, &mut app.projects)?;
      }
    let usage = registry::load_usage(&app.opencode_db_path);
    let mut bumps: Vec<String> = Vec::new();
    for entry in &mut app.projects
      {
        entry.debug_modified =
          registry::variant_modified(&entry.path, "debug").unwrap_or_else(|| "jamais compilée".to_string());
        entry.release_modified =
          registry::variant_modified(&entry.path, "release").unwrap_or_else(|| "jamais compilée".to_string());
        let cargo = entry.path.join("Cargo.toml");
        if let Ok(content) = fs::read_to_string(&cargo)
          {
            let mut new_version = None;
            for line in content.lines()
              {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("version")
                  {
                    if let Some(r2) = rest.trim_start().strip_prefix('=')
                      {
                        let v = r2.trim().trim_matches('"');
                        if !v.is_empty()
                          {
                            new_version = Some(v.to_string());
                          }
                      }
                    break;
                  }
              }
            if let Some(v) = new_version
              {
                entry.version = v.clone();
                // La variante Debug n'est resynchronisée que si elle est vide,
                // pour ne pas écraser une version éditée manuellement.
                if entry.version_debug.is_empty()
                  {
                    entry.version_debug = v;
                  }
                changed += 1;
              }
          }
        if let Some(u) = usage.get(&entry.path)
          {
            let cum = &u.cumulative;
            entry.token = format_number(cum.total_tokens);
            if cum.total_tokens >= 1_000_000
              {
                entry.used_pct = "99+".to_string();
              }
            else
              {
                entry.used_pct = format!("{}", (cum.total_tokens * 100) / 1_000_000);
              }
            entry.spent = if cum.total_cost > 0.0
              {
                format_currency(cum.total_cost)
              }
            else
              {
                "—".to_string()
              };
            entry.total_tokens = format_number(cum.total_tokens);
            entry.total_cost = if cum.total_cost > 0.0
              {
                format_currency(cum.total_cost)
              }
            else
              {
                "—".to_string()
              };
            entry.session_count = format!("{}", cum.session_count);
            if let Some(ls) = &u.last_session
              {
                entry.last_session_tokens = format_number(ls.tokens);
                entry.last_session_cost = if ls.cost > 0.0
                  {
                    format_currency(ls.cost)
                  }
                else
                  {
                    "—".to_string()
                  };
                entry.last_model = ls.model.clone();
                entry.last_agent = ls.agent.clone();
                entry.last_time_created = format_datetime(ls.time_created);
              }
            else
              {
                entry.last_session_tokens = String::new();
                entry.last_session_cost = String::new();
                entry.last_model = String::new();
                entry.last_agent = String::new();
                entry.last_time_created = String::new();
              }
            changed += 1;
          }
        else
          {
            entry.token = String::new();
            entry.used_pct = String::new();
            entry.spent = String::new();
            entry.total_tokens = String::new();
            entry.total_cost = String::new();
            entry.session_count = String::new();
            entry.last_session_tokens = String::new();
            entry.last_session_cost = String::new();
            entry.last_model = String::new();
            entry.last_agent = String::new();
            entry.last_time_created = String::new();
          }
        // AutoPatch : seule la variante dont la compilation (target/debug ou
        // /release) est plus récente que sa propre référence est traitée.
        // Debug (variante élue) incrémente z de x.y.z ; Release rattrape la
        // version/statut du code courant dès que son binaire est reconstruit.
        // Aucune compilation → aucune mise à jour.
        let t_debug = registry::variant_modified_time(&entry.path, "debug")
          .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()));
        let t_release = registry::variant_modified_time(&entry.path, "release")
          .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()));
        if let Some(t) = t_debug
          {
            if t > entry.last_compile_debug_secs
              {
                let valid = registry::validate_version(&entry.version_debug);
                let init = !valid;
                let baseline = entry.last_compile_debug_secs == 0;
                entry.last_compile_debug_secs = t;
                written = true;
                if init || !baseline
                  {
                    let new_version = if valid
                      {
                        bump_patch(&entry.version_debug)
                      }
                    else
                      {
                        Some("0.0.1".to_string())
                      };
                    if let Some(v) = new_version
                      {
                        let old = if entry.version_debug.is_empty()
                          {
                            "—".to_string()
                          }
                        else
                          {
                            entry.version_debug.clone()
                          };
                        entry.version_debug = v.clone();
                        // La Release n'est PAS bumpée ici : son binaire n'a pas
                        // été reconstruit, elle reste à sa version antérieure.
                        entry.version = v.clone();
                        let _ = bump_manifest_version(&entry.path, &v)?;
                        bumps.push(format!("{old} → {v}"));
                        changed += 1;
                      }
                  }
              }
          }
        if let Some(t) = t_release
          {
            if t > entry.last_compile_release_secs
              {
                entry.last_compile_release_secs = t;
                // Rattrapage : la Release reconstruite reprend la version et le
                // statut du code courant (variante Debug élue / manifeste).
                let v = if entry.version_debug.is_empty()
                  {
                    entry.version.clone()
                  }
                else
                  {
                    entry.version_debug.clone()
                  };
                if !v.is_empty()
                  {
                    entry.version_release = v;
                    entry.status_release = entry.status_debug.clone();
                    changed += 1;
                  }
                written = true;
              }
          }
      }
    if added > 0 || changed > 0 || written
      {
        registry::save_registry(&app.registry_path, &app.projects)?;
        let mut parts: Vec<String> = Vec::new();
        if !bumps.is_empty()
          {
            parts.push(format!("AutoPatch : {} (nouvelle compilation)", bumps.join(" · ")));
          }
        if removed > 0
          {
            parts.push(format!("{removed} retiré(s)"));
          }
        if added > 0
          {
            parts.push(format!("{added} ajouté(s)"));
          }
        if changed > 0
          {
            parts.push(format!("{changed} resynchronisée(s)"));
          }
        if parts.is_empty()
          {
            app.push_info("Rescan : références de compilation actualisées".to_string());
          }
        else
          {
            app.push_info(parts.join("  ·  "));
          }
      }
    else
      {
        app.push_info("Rescan : base déjà alignée sur le disque".to_string());
      }
    finalize_project_list(app);
    Ok(())
  }

fn initial_selection_index(order: SortOrder, len: usize) -> usize {
    if order == SortOrder::Modified {
        len.saturating_sub(1)
    } else {
        0
    }
}

fn sort_projects(projects: &mut [ProjectEntry], order: SortOrder) {
    match order {
        SortOrder::Name => {
            projects.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        }
        SortOrder::Modified => {
            projects.sort_by(|a, b| {
                let ta = registry::last_modified_time(&a.path);
                let tb = registry::last_modified_time(&b.path);
                match (ta, tb) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (None, Some(_)) => Ordering::Greater,
                    (Some(_), None) => Ordering::Less,
                    (None, None) => Ordering::Equal,
                }
            });
        }
    }
}

fn finalize_project_list(app: &mut App) {
    sort_projects(&mut app.projects, app.sort_order);
    app.filtered = (0..app.projects.len()).collect();
    if !app.projects.is_empty() {
        app.list_state.select(Some(initial_selection_index(app.sort_order, app.projects.len())));
    }
}

fn projects_root(projects: &[ProjectEntry]) -> Option<PathBuf> {
    if projects.is_empty() {
        return dirs::home_dir().map(|h| h.join("projets"));
    }
    let mut counts = std::collections::HashMap::<PathBuf, usize>::new();
    for p in projects {
        if let Some(parent) = p.path.parent() {
            *counts.entry(parent.to_path_buf()).or_insert(0) += 1;
        }
    }
    counts
        .into_iter()
        .max_by_key(|&(_, c)| c)
        .map(|(dir, _)| dir)
        .or_else(|| dirs::home_dir().map(|h| h.join("projets")))
}

// Liste du menu déroulant statut en création : « aucun » + éligibles + « nouveau ».
pub(crate) fn status_menu_creation_items(filter: &str) -> Vec<&'static str>
  {
    let mut items = vec!["<Aucun>"];
    items.extend(filtered_statuses(filter));
    items.push("+ Nouveau statut");
    items
  }

fn handle_creation_input(
  key: event::KeyEvent,
  state: &mut ProjectCreationState,
  app: &mut App,
) -> Result<InputMode>
  {
    const FIELD_COUNT: usize = 6;
    match key.code
      {
        KeyCode::Esc =>
          {
            if state.status_menu_open
              {
                state.status_menu_open = false;
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            if state.confirm_status
              {
                state.status_free_text = true;
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            return Ok(InputMode::Normal);
          }
        KeyCode::Up | KeyCode::Down =>
          {
            if state.status_menu_open
              {
                let items = status_menu_creation_items(&state.status_filter);
                let delta = if key.code == KeyCode::Up { usize::MAX } else { 1 };
                if delta == 1 && state.status_cursor + 1 < items.len()
                  {
                    state.status_cursor += 1;
                  }
                else if delta == usize::MAX && state.status_cursor > 0
                  {
                    state.status_cursor -= 1;
                  }
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            if state.confirm_status
              {
                state.confirm_status = false;
                state.status_free_text = true;
              }
            let delta = if key.code == KeyCode::Up
              {
                usize::MAX
              }
            else
              {
                1
              };
            if delta == 1 && state.active_field < FIELD_COUNT
              {
                state.active_field += 1;
              }
            else if delta == usize::MAX && state.active_field > 0
              {
                state.active_field -= 1;
              }
            state.status_free_text = false;
            load_creation_field(state);
            return Ok(InputMode::CreatingProject(state.clone()));
          }
        KeyCode::Left | KeyCode::Right =>
          {
            if state.status_menu_open
              {
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            let count = state.input_buffer.chars().count();
            if key.code == KeyCode::Left
              {
                if state.input_offset > 0
                  {
                    state.input_offset -= 1;
                  }
              }
            else if state.input_offset < count
              {
                state.input_offset += 1;
              }
            state.rewrite_mode = false;
            return Ok(InputMode::CreatingProject(state.clone()));
          }
        KeyCode::Backspace =>
          {
            if state.status_menu_open
              {
                state.status_filter.pop();
                let items = status_menu_creation_items(&state.status_filter);
                if state.status_cursor >= items.len()
                  {
                    state.status_cursor = items.len().saturating_sub(1);
                  }
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            if state.input_offset > 0
              {
                let idx = state.input_offset - 1;
                delete_char_at(&mut state.input_buffer, idx);
                state.input_offset = idx;
                state.rewrite_mode = false;
              }
            return Ok(InputMode::CreatingProject(state.clone()));
          }
        KeyCode::Enter =>
          {
            // Menu déroulant ouvert : sélectionner l'entrée courante.
            if state.status_menu_open
              {
                let items = status_menu_creation_items(&state.status_filter);
                let sel = items.get(state.status_cursor).copied();
                state.status_menu_open = false;
                if let Some(item) = sel
                  {
                    if item == "<Aucun>"
                      {
                        state.status = None;
                        state.status_free_text = false;
                        state.active_field = 5;
                        load_creation_field(state);
                      }
                    else if item == "+ Nouveau statut"
                      {
                        state.status_free_text = true;
                        load_creation_field(state);
                      }
                    else
                      {
                        state.status = Some(item.to_string());
                        if !state.known_statuses.contains(&item.to_string())
                          {
                            state.known_statuses.push(item.to_string());
                          }
                        state.status_free_text = false;
                        state.active_field = 5;
                        load_creation_field(state);
                      }
                  }
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            // Confirmation d'un nouveau statut hors liste : o/N décidé lors de la frappe.
            if state.confirm_status
              {
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            match state.active_field
              {
                0 =>
                  {
                    let id = state.input_buffer.trim().replace(' ', "-");
                    if id.is_empty()
                      {
                        return Ok(InputMode::Normal);
                      }
                    state.id = id;
                    state.active_field = 1;
                    load_creation_field(state);
                  }
                1 =>
                  {
                    state.name = if state.input_buffer.trim().is_empty()
                      {
                        state.id.clone()
                      }
                    else
                      {
                        state.input_buffer.trim().to_string()
                      };
                    state.active_field = 2;
                    load_creation_field(state);
                  }
                2 =>
                  {
                    state.path = state.input_buffer.trim().to_string();
                    state.active_field = 3;
                    load_creation_field(state);
                  }
                3 =>
                  {
                    state.description = state.input_buffer.trim().to_string();
                    state.active_field = 4;
                    load_creation_field(state);
                  }
                4 =>
                  {
                    if state.status_free_text
                      {
                        let value = state.input_buffer.trim().to_string();
                        if value.is_empty()
                          {
                            state.status = None;
                            state.status_free_text = false;
                            state.active_field = 5;
                            load_creation_field(state);
                          }
                        else if state.known_statuses.iter().any(|s| s == &value)
                          {
                            state.status = Some(value);
                            state.status_free_text = false;
                            state.active_field = 5;
                            load_creation_field(state);
                          }
                        else
                          {
                            state.confirm_status = true;
                          }
                      }
                    else
                      {
                        // Ouverture du menu déroulant des statuts connus.
                        state.status_menu_open = true;
                        state.status_filter.clear();
                        state.status_cursor = 0;
                        state.status_free_text = false;
                      }
                  }
                5 =>
                  {
                    state.version = state.input_buffer.trim().to_string();
                    state.active_field = 6;
                    load_creation_field(state);
                  }
                // Champ de confirmation final o/N : créer le projet.
                _ =>
                  {
                    let dir = if state.path.is_empty()
                      {
                        dirs::home_dir()
                          .unwrap_or_else(|| PathBuf::from("."))
                          .join("projets")
                          .join(&state.id)
                      }
                    else
                      {
                        PathBuf::from(shellexpand::tilde(&state.path).to_string())
                      };

                    fs::create_dir_all(&dir)?;
                    if !dir.join("opencode.json").exists()
                      {
                        fs::write(
                          dir.join("opencode.json"),
                          r#"{
  "$schema": "https://opencode.ai/config.json",
  "instructions": ["~/.config/opencode/instructions.md"]
}"#,
                        )?;
                      }

                    let status = state
                      .status
                      .clone()
                      .or_else(|| {
                        let v = state.input_buffer.trim();
                        if v.is_empty()
                          {
                            None
                          }
                        else
                          {
                            Some(v.to_string())
                          }
                      });

                    let status = status.unwrap_or_default();
                    let version = state.version.clone();

                    app.projects.push(ProjectEntry {
                        id: state.id.clone(),
                        name: state.name.clone(),
                        path: dir.clone(),
                        description: state.description.clone(),
                        status: status.clone(),
                        version: version.clone(),
                        token: String::new(),
                        used_pct: String::new(),
                        spent: String::new(),
                        last_model: String::new(),
                        modified: "—".to_string(),
                        total_tokens: String::new(),
                        total_cost: String::new(),
                        session_count: String::new(),
                        last_session_tokens: String::new(),
                        last_session_cost: String::new(),
                        last_agent: String::new(),
                        last_time_created: String::new(),
                        status_debug: status.clone(),
                        version_debug: version.clone(),
                        status_release: status.clone(),
                        version_release: version.clone(),
                        debug_modified: "jamais compilée".to_string(),
                        release_modified: "jamais compilée".to_string(),
                        last_compile_debug_secs: 0,
                        last_compile_release_secs: 0,
                      });
                    registry::save_registry(&app.registry_path, &app.projects)?;

                    if matches!(
                      state.input_buffer.to_lowercase().as_str(),
                      "o" | "oui" | "y" | "yes"
                    ) && !status.is_empty()
                      {
                        app.pending_launch = Some(dir);
                        app.pending_sudo = false;
                      }
                    return Ok(InputMode::Normal);
                  }
              }
            return Ok(InputMode::CreatingProject(state.clone()));
          }
        KeyCode::Char(c) =>
          {
            // Décision o/N sur la confirmation d'un statut hors liste.
            if state.confirm_status
              {
                match c
                  {
                    'o' | 'O' =>
                      {
                        let value = state.input_buffer.trim().to_string();
                        let value = if value.is_empty()
                          {
                            state.status.clone().unwrap_or_default()
                          }
                        else
                          {
                            if !state.known_statuses.contains(&value)
                              {
                                state.known_statuses.push(value.clone());
                              }
                            value
                          };
                        state.status = Some(value.clone());
                        state.confirm_status = false;
                        state.status_free_text = false;
                        state.active_field = 5;
                        load_creation_field(state);
                      }
                    'n' | 'N' =>
                      {
                        state.status = None;
                        state.input_buffer.clear();
                        state.confirm_status = false;
                        state.status_free_text = true;
                        state.active_field = 4;
                        state.rewrite_mode = true;
                      }
                    _ =>
                      {}
                  }
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            // Menu déroulant ouvert : la frappe réduit la liste par préfixe.
            if state.status_menu_open
              {
                if c.is_alphanumeric() || c == '-'
                  {
                    state.status_filter.push(c);
                    state.status_cursor = 0;
                  }
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            // Champ Statut : la frappe ouvre le menu et commence à filtrer.
            if state.active_field == 4 && !state.status_free_text
              {
                if c.is_alphanumeric() || c == '-'
                  {
                    state.status_menu_open = true;
                    state.status_filter.clear();
                    state.status_filter.push(c);
                    state.status_cursor = 0;
                    state.rewrite_mode = false;
                  }
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            // Champ Version : saisie guidée par le masque x.y.z.
            if state.active_field == 5
              {
                if state.rewrite_mode && state.input_offset >= state.input_buffer.chars().count()
                  {
                    let old = std::mem::take(&mut state.input_buffer);
                    state.input_offset = 0;
                    if let Some((s, off)) = version_mask_insert("", 0, c)
                      {
                        state.input_buffer = s;
                        state.input_offset = off;
                        state.rewrite_mode = false;
                      }
                    else
                      {
                        state.input_buffer = old;
                      }
                  }
                else
                  {
                    state.rewrite_mode = false;
                    if let Some((s, off)) =
                      version_mask_insert(&state.input_buffer, state.input_offset, c)
                      {
                        state.input_buffer = s;
                        state.input_offset = off;
                      }
                  }
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            if state.rewrite_mode && state.input_offset >= state.input_buffer.chars().count()
              {
                state.input_buffer = c.to_string();
                state.input_offset = 1;
                state.rewrite_mode = false;
              }
            else
              {
                insert_char_at(&mut state.input_buffer, c, state.input_offset);
                state.input_offset += 1;
              }
            return Ok(InputMode::CreatingProject(state.clone()));
          }
        _ =>
          {}
      }
    Ok(InputMode::CreatingProject(state.clone()))
  }

// Remonte le buffer du champ actif depuis sa valeur propre (édition).
fn load_edit_field(state: &mut ProjectEditState)
  {
    state.input_buffer = match state.active_field
      {
        0 => state.pending_version.clone(),
        1 => state.pending_status.clone(),
        2 => state.pending_description.clone(),
        _ => String::new(),
      };
    state.input_offset = state.input_buffer.chars().count();
    state.rewrite_mode = true;
  }

/// Applique aux champs du projet la validation d'un champ en cours d'édition.
/// Met à jour `pending_*` si valide et prépare le champ suivant ; sinon remplit `error`.
fn commit_edit_field(state: &mut ProjectEditState) {
    let value = state.input_buffer.trim().to_string();
    let ok = match state.active_field {
        0 => {
            if registry::validate_version(&value) {
                state.pending_version = value;
                true
            } else {
                false
            }
        }
        1 => {
            if registry::validate_status(&value) {
                state.pending_status = value;
                true
            } else {
                false
            }
        }
        2 => {
            if !value.is_empty() {
                state.pending_description = value;
                true
            } else {
                false
            }
        }
        _ => false,
    };
    if ok {
        state.error = None;
        state.editing = false;
        if state.active_field < 3 {
            state.active_field += 1;
        }
    } else {
        state.error = Some(match state.active_field {
            0 => "Version invalide : format attendu x.y.z (ex. 1.2.3)".to_string(),
            1 => format!("Statut invalide : attendu l'un de {}.", registry::STATUSES.join(" · ")),
            2 => "Description invalide : texte non vide requis".to_string(),
            _ => String::new(),
        });
    }
}

/// Valide l'ensemble des champs et applique les modifications au projet,
/// en diffusant la variante Debug sur la variante Release (miroir).
fn apply_edit(app: &mut App, state: &ProjectEditState) -> bool {
    if !registry::validate_version(&state.pending_version) {
        return false;
    }
    if !registry::validate_status(&state.pending_status) {
        return false;
    }
    if state.pending_description.trim().is_empty() {
        return false;
    }
    if let Some(p) = app.projects.get_mut(state.project_idx) {
        p.version_debug = state.pending_version.clone();
        p.status_debug = state.pending_status.clone();
        p.description = state.pending_description.clone();
        // Sans miroir : la Release garde sa version/statut tant que son
        // binaire n'est pas reconstruit (rattrapage au prochain rescan).
        // Champs historiques alignés sur la variante élue (Debug).
        p.version = p.version_debug.clone();
        p.status = p.status_debug.clone();
    }
    true
}

const EDIT_CONFIRM_FIELD: usize = 3;

fn handle_edit_input(
  key: event::KeyEvent,
  state: &mut ProjectEditState,
  app: &mut App,
) -> Result<InputMode>
  {
    if state.editing
      {
        match key.code
          {
            KeyCode::Esc =>
              {
                state.editing = false;
                return Ok(InputMode::EditingProject(state.clone()));
              }
            KeyCode::Left =>
              {
                if state.input_offset > 0
                  {
                    state.input_offset -= 1;
                  }
                state.rewrite_mode = false;
                return Ok(InputMode::EditingProject(state.clone()));
              }
            KeyCode::Right =>
              {
                if state.input_offset < state.input_buffer.chars().count()
                  {
                    state.input_offset += 1;
                  }
                state.rewrite_mode = false;
                return Ok(InputMode::EditingProject(state.clone()));
              }
            KeyCode::Backspace =>
              {
                if state.input_offset > 0
                  {
                    let idx = state.input_offset - 1;
                    delete_char_at(&mut state.input_buffer, idx);
                    state.input_offset = idx;
                    state.rewrite_mode = false;
                  }
                return Ok(InputMode::EditingProject(state.clone()));
              }
            KeyCode::Enter =>
              {
                commit_edit_field(state);
                return Ok(InputMode::EditingProject(state.clone()));
              }
            KeyCode::Char(c) =>
              {
                // Champ Version : saisie guidée par le masque x.y.z.
                if state.active_field == 0
                  {
                    if state.rewrite_mode
                      && state.input_offset >= state.input_buffer.chars().count()
                      {
                        let old = std::mem::take(&mut state.input_buffer);
                        state.input_offset = 0;
                        if let Some((s, off)) = version_mask_insert("", 0, c)
                          {
                            state.input_buffer = s;
                            state.input_offset = off;
                            state.rewrite_mode = false;
                          }
                        else
                          {
                            state.input_buffer = old;
                          }
                      }
                    else
                      {
                        state.rewrite_mode = false;
                        if let Some((s, off)) =
                          version_mask_insert(&state.input_buffer, state.input_offset, c)
                          {
                            state.input_buffer = s;
                            state.input_offset = off;
                          }
                      }
                  }
                else if state.rewrite_mode
                  && state.input_offset >= state.input_buffer.chars().count()
                  {
                    state.input_buffer = c.to_string();
                    state.input_offset = 1.min(state.input_buffer.chars().count());
                    state.rewrite_mode = false;
                  }
                else
                  {
                    let pos = state.input_offset.min(state.input_buffer.chars().count());
                    insert_char_at(&mut state.input_buffer, c, pos);
                    state.input_offset = (pos + 1).min(state.input_buffer.chars().count());
                  }
                state.error = None;
                return Ok(InputMode::EditingProject(state.clone()));
              }
            _ =>
              {}
          }
      }
    else
      {
        match key.code
          {
            KeyCode::Esc =>
              {
                if state.status_menu_open
                  {
                    state.status_menu_open = false;
                    return Ok(InputMode::EditingProject(state.clone()));
                  }
                return Ok(InputMode::Normal);
              }
            KeyCode::Up | KeyCode::Down =>
              {
                if state.status_menu_open
                  {
                    let items = filtered_statuses(&state.status_filter);
                    let delta = if key.code == KeyCode::Up { usize::MAX } else { 1 };
                    if delta == 1 && state.status_cursor + 1 < items.len()
                      {
                        state.status_cursor += 1;
                      }
                    else if delta == usize::MAX && state.status_cursor > 0
                      {
                        state.status_cursor -= 1;
                      }
                    return Ok(InputMode::EditingProject(state.clone()));
                  }
                let delta = if key.code == KeyCode::Up
                  {
                    usize::MAX
                  }
                else
                  {
                    1
                  };
                if delta == 1 && state.active_field < EDIT_CONFIRM_FIELD
                  {
                    state.active_field += 1;
                  }
                else if delta == usize::MAX && state.active_field > 0
                  {
                    state.active_field -= 1;
                  }
                return Ok(InputMode::EditingProject(state.clone()));
              }
            KeyCode::Enter =>
              {
                if state.status_menu_open
                  {
                    let items = filtered_statuses(&state.status_filter);
                    if let Some(sel) = items.get(state.status_cursor).copied()
                      {
                        state.pending_status = sel.to_string();
                        state.status_menu_open = false;
                        state.error = None;
                        if state.active_field < 3
                          {
                            state.active_field += 1;
                          }
                      }
                    return Ok(InputMode::EditingProject(state.clone()));
                  }
                if state.active_field == 1
                  {
                    // Champ Statut : ouverture du menu déroulant.
                    state.status_menu_open = true;
                    state.status_filter.clear();
                    state.status_cursor = 0;
                    return Ok(InputMode::EditingProject(state.clone()));
                  }
                if state.active_field < EDIT_CONFIRM_FIELD
                  {
                    state.editing = true;
                    load_edit_field(state);
                  }
                else if apply_edit(app, state)
                  {
                    registry::save_registry(&app.registry_path, &app.projects)?;
                    app.push_info(
                      "Projet mis à jour : variante Debug enregistrée".to_string(),
                    );
                    return Ok(InputMode::Normal);
                  }
                else
                  {
                    state.error = Some(
                      "Corrigez les champs avant d'enregistrer (version, statut, description)".to_string(),
                    );
                  }
                return Ok(InputMode::EditingProject(state.clone()));
              }
            KeyCode::Char(c) if state.status_menu_open =>
              {
                if c.is_alphanumeric() || c == '-'
                  {
                    state.status_filter.push(c);
                    state.status_cursor = 0;
                  }
                return Ok(InputMode::EditingProject(state.clone()));
              }
            KeyCode::Char('o') | KeyCode::Char('O') =>
              {
                if state.active_field == EDIT_CONFIRM_FIELD && apply_edit(app, state)
                  {
                    registry::save_registry(&app.registry_path, &app.projects)?;
                    app.push_info(
                      "Projet mis à jour : variante Debug enregistrée".to_string(),
                    );
                    return Ok(InputMode::Normal);
                  }
                return Ok(InputMode::EditingProject(state.clone()));
              }
            KeyCode::Char('n') | KeyCode::Char('N') =>
              {
                if state.active_field == EDIT_CONFIRM_FIELD
                  {
                    return Ok(InputMode::Normal);
                  }
                return Ok(InputMode::EditingProject(state.clone()));
              }
            _ =>
              {}
          }
      }
    Ok(InputMode::EditingProject(state.clone()))
  }

#[cfg(test)]
mod tests
  {
    use crate::{sanitize, variant_label};
    use std::{fs, path::PathBuf};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("op-start-menu-main-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_strips_ctrl_chars() {
        assert_eq!(sanitize("a\x1b[31mb"), "a[31mb");
        assert_eq!(sanitize("ligne\nsuivante"), "lignesuivante");
    }

    #[test]
    fn sanitize_keeps_plain_text() {
        assert_eq!(sanitize("Bonjour le monde 2026 ! — éà"), "Bonjour le monde 2026 ! — éà");
    }

    #[test]
    fn sanitize_empty() {
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn variant_label_appends_status() {
        assert_eq!(variant_label("1.2.3", "Beta"), "1.2.3_Beta");
    }

    #[test]
    fn variant_label_strips_prerelease() {
        assert_eq!(variant_label("0.9.9-beta", "Pre-Release"), "0.9.9_Pre_Release");
    }

    #[test]
    fn variant_label_without_status() {
        assert_eq!(variant_label("3.4.5", ""), "3.4.5");
    }

    #[test]
    fn binary_name_reads_cargo_package() {
        let dir = temp_dir("binname");
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"dummyproj\"\nversion = \"1.2.3\"\n",
        )
        .unwrap();
        assert_eq!(super::binary_name_from_cargo(&dir).as_deref(), Some("dummyproj"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn binary_name_missing_returns_none() {
        let dir = temp_dir("nobin");
        assert_eq!(super::binary_name_from_cargo(&dir), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn initial_selection_modified_targets_latest() {
        assert_eq!(super::initial_selection_index(super::SortOrder::Modified, 1), 0);
        assert_eq!(super::initial_selection_index(super::SortOrder::Modified, 5), 4);
    }

    #[test]
    fn initial_selection_name_targets_first() {
        assert_eq!(super::initial_selection_index(super::SortOrder::Name, 0), 0);
        assert_eq!(super::initial_selection_index(super::SortOrder::Name, 5), 0);
    }

    fn entry(id: &str, name: &str, path: PathBuf) -> super::ProjectEntry {
        use crate::registry::ProjectEntry;
        ProjectEntry {
            id: id.to_string(),
            name: name.to_string(),
            path,
            description: String::new(),
            status: String::new(),
            version: String::new(),
            token: String::new(),
            used_pct: String::new(),
            spent: String::new(),
            last_model: String::new(),
            modified: String::new(),
            total_tokens: String::new(),
            total_cost: String::new(),
            session_count: String::new(),
            last_session_tokens: String::new(),
            last_session_cost: String::new(),
            last_agent: String::new(),
            last_time_created: String::new(),
            status_debug: String::new(),
            version_debug: String::new(),
            status_release: String::new(),
            version_release: String::new(),
            debug_modified: String::new(),
            release_modified: String::new(),
            last_compile_debug_secs: 0,
            last_compile_release_secs: 0,
        }
    }

    #[test]
    fn sort_by_name_is_case_insensitive() {
        let mut projects = vec![
            entry("c", "Beta", PathBuf::from("/tmp/x")),
            entry("b", "alpha", PathBuf::from("/tmp/y")),
            entry("a", "Gamma", PathBuf::from("/tmp/z")),
        ];
        super::sort_projects(&mut projects, super::SortOrder::Name);
        assert_eq!(
            projects.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "Beta", "Gamma"]
        );
    }

    #[test]
    fn sort_by_name_is_stable() {
        let mut projects = vec![
            entry("c", "Delta", PathBuf::from("/tmp/1")),
            entry("b", "delta", PathBuf::from("/tmp/2")),
            entry("a", "alpha", PathBuf::from("/tmp/3")),
        ];
        super::sort_projects(&mut projects, super::SortOrder::Name);
        assert_eq!(projects[0].id, "a");
        assert_eq!(projects[1].id, "c");
        assert_eq!(projects[2].id, "b");
    }

    #[test]
    fn sort_by_modified_is_oldest_first_and_none_last() {
        let dir = temp_dir("sortmod");
        let old = dir.join("old");
        let recent = dir.join("recent");
        let missing = dir.join("missing");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&recent).unwrap();
        let set_mtime = |dir: &PathBuf, secs: u64| {
            let f = dir.join("fichier.txt");
            fs::write(&f, b"x").unwrap();
            let t = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            fs::File::options()
                .write(true)
                .open(&f)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(t))
                .unwrap();
        };
        set_mtime(&old, 1_600_000_000);
        set_mtime(&recent, 1_700_000_000);
        let mut projects = vec![
            entry("r", "recent", recent),
            entry("m", "missing", missing),
            entry("o", "old", old),
        ];
        super::sort_projects(&mut projects, super::SortOrder::Modified);
        assert_eq!(
            projects.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["o", "r", "m"]
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn byte_index_for_char_maps_utf8()
      {
        let s = "café";
        assert_eq!(super::byte_index_for_char(s, 0), 0);
        assert_eq!(super::byte_index_for_char(s, 2), 2);
        assert_eq!(super::byte_index_for_char(s, 3), 3);
        assert_eq!(super::byte_index_for_char(s, 4), 5); // fin (é = 2 octets)
        assert_eq!(super::byte_index_for_char(s, 99), 5);
      }

    #[test]
    fn insert_char_at_respects_utf8_boundaries()
      {
        let mut s = "café".to_string();
        super::insert_char_at(&mut s, 'x', 3);
        assert_eq!(s, "cafxé");
        super::insert_char_at(&mut s, '!', 0);
        assert_eq!(s, "!cafxé");
      }

    #[test]
    fn delete_char_at_respects_utf8_boundaries()
      {
        let mut s = "café".to_string();
        super::delete_char_at(&mut s, 3);
        assert_eq!(s, "caf");
        super::delete_char_at(&mut s, 0);
        assert_eq!(s, "af");
        let mut s2 = "En-cours Testé".to_string();
        for _ in 0..5
          {
            super::delete_char_at(&mut s2, 0);
          }
        assert_eq!(s2, "urs Testé");
      }

    #[test]
    fn version_mask_auto_dot_after_three_digits()
      {
        assert_eq!(
          super::version_mask_insert("123", 3, '4'),
          Some(("123.4".to_string(), 5))
        );
        assert_eq!(
          super::version_mask_insert("1.2.3", 5, '4'),
          Some(("1.2.34".to_string(), 6))
        );
      }

    #[test]
    fn version_mask_manual_dot_and_rejects()
      {
        assert_eq!(
          super::version_mask_insert("1", 1, '.'),
          Some(("1.".to_string(), 2))
        );
        assert_eq!(super::version_mask_insert("", 0, '.'), None);
        assert_eq!(super::version_mask_insert("1.2.3", 5, 'a'), None);
        assert_eq!(super::version_mask_insert("1.", 2, '.'), None);
        assert_eq!(super::version_mask_insert("1.2.3", 2, 'z'), None);
      }

    #[test]
    fn filtered_statuses_matches_prefix_ci()
      {
        assert_eq!(super::filtered_statuses("b"), vec!["Beta", "Beta-Release"]);
        assert_eq!(super::filtered_statuses("B"), vec!["Beta", "Beta-Release"]);
        assert_eq!(super::filtered_statuses("rele"), vec!["Release"]);
        assert_eq!(super::filtered_statuses("zzz"), Vec::<&str>::new());
        assert_eq!(super::filtered_statuses("").len(), super::registry::STATUSES.len());
        let items = super::status_menu_creation_items("be");
        assert_eq!(items, vec!["<Aucun>", "Beta", "Beta-Release", "+ Nouveau statut"]);
      }

    #[test]
    fn bump_patch_increments_patch()
      {
        assert_eq!(super::bump_patch("0.9.9").as_deref(), Some("0.9.10"));
        assert_eq!(super::bump_patch("999.0.9").as_deref(), Some("999.0.10"));
        assert_eq!(super::bump_patch("0.0.0").as_deref(), Some("0.0.1"));
        assert_eq!(super::bump_patch("1.2"), None);
        assert_eq!(super::bump_patch(""), None);
        assert_eq!(super::bump_patch("1.x.3"), None);
        assert_eq!(super::bump_patch("1..3"), None);
      }

    #[test]
    fn bump_manifest_version_updates_package_only()
      {
        let dir = temp_dir("manifest");
        fs::write(
          dir.join("Cargo.toml"),
          "[package]\nname = \"demo\"\nversion = \"1.2.3\"\n\n[dependencies]\nfoo = { version = \"7.8.9\" }\n",
        )
        .unwrap();
        assert_eq!(super::bump_manifest_version(&dir, "1.2.4").unwrap(), true);
        let content = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(content.contains("version = \"1.2.4\""));
        assert!(content.contains("foo = { version = \"7.8.9\" }"));
        assert!(content.contains("name = \"demo\""));
        fs::remove_dir_all(&dir).unwrap();
      }

    #[test]
    fn bump_manifest_version_absent_returns_false()
      {
        let dir = temp_dir("nomanifest");
        fs::write(dir.join("Cargo.toml"), "[workspace]\nmembers = [\"a\"]\n").unwrap();
        assert_eq!(super::bump_manifest_version(&dir, "1.2.4").unwrap(), false);
        let content = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(!content.contains("1.2.4"));
        fs::remove_dir_all(&dir).unwrap();
      }

    #[test]
    fn bump_manifest_version_inserts_absent_version()
      {
        let dir = temp_dir("nover");
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo\"\n\n[dependencies]\nfoo = \"1\"\n").unwrap();
        assert_eq!(super::bump_manifest_version(&dir, "0.0.1").unwrap(), true);
        let content = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(content.contains("version = \"0.0.1\""));
        assert!(content.contains("name = \"demo\""));
        assert!(content.contains("foo = \"1\""));
        assert!(content.contains("[dependencies]"));
        fs::remove_dir_all(&dir).unwrap();
      }
  }