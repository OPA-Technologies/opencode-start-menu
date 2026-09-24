use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    widgets::ListState,
    Terminal,
};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
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
#[command(name = "op", version, about = "OpenCode Start Menu - Interactive project launcher", disable_help_flag = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    #[arg(short = 'r', long = "root", help = "Launch as root via sudo -i")]
    root: bool,

    #[arg(short = 'l', long = "list", help = "List projects without interactive menu")]
    list: bool,

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

pub(crate) struct App {
    pub(crate) projects: Vec<ProjectEntry>,
    pub(crate) filtered: Vec<usize>,
    pub(crate) list_state: ListState,
    pub(crate) show_new: bool,
    mode: LaunchMode,
    pub(crate) status_message: Option<String>,
    registry_path: PathBuf,
    pub(crate) terminal_size: (u16, u16),
    pub(crate) banner_width: u16,
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
}

impl App {
    fn new(registry_path: PathBuf) -> Self {
        let mut app = Self {
            projects: Vec::new(),
            filtered: Vec::new(),
            list_state: ListState::default(),
            show_new: true,
            mode: LaunchMode::Normal,
            status_message: None,
            registry_path,
            terminal_size: (80, 24),
            banner_width: 77,
            pending_launch: None,
            pending_sudo: false,
            pending_name: None,
            pending_bare: false,
        };
        match registry::load_registry(&app.registry_path) {
            Ok(projects) => app.projects = projects,
            Err(e) => {
                app.status_message = Some(format!("Registre illisible : {e}"));
            }
        }
        // Rescan au démarrage : chaque projet est relu depuis son Cargo.toml disque,
        // pour que la liste reflète l'état réel du répertoire éponyme dès l'affichage.
        if let Err(e) = rescan_repertoire(&mut app) {
            app.status_message = Some(format!("Rescan au démarrage échoué : {e}"));
        }
        app.filtered = (0..app.projects.len()).collect();
        if !app.projects.is_empty() {
            app.list_state.select(Some(0));
        }
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

    fn launch(&mut self, idx: usize) {
        let project = &self.projects[idx];
        let dir = &project.path;
        if !dir.exists() {
            self.status_message = Some(format!("Dossier introuvable : {}", sanitize(&dir.to_string_lossy())));
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
    println!("  op -l         liste les projets sans menu");
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

    let mut app = App::new(registry_path.clone());

    if cli.help {
        print_help();
        return Ok(());
    }

    if cli.list {
        app.projects = registry::load_registry(&registry_path)?;
        if app.projects.is_empty() {
            println!("Aucun projet enregistré. Utilisez : op");
        } else {
            for p in &app.projects {
                println!("  \x1b[1;34m* {}\x1b[0m", sanitize(&p.name));
                println!("    ID       : \x1b[36m{}\x1b[0m", sanitize(&p.id));
                println!("    Chemin   : {}", sanitize(&p.path.to_string_lossy()));
                println!("    Descript.: {}", sanitize(&p.description));
                println!("    Modifié  : {}", sanitize(&p.modified));
                println!("    Statut   : {}", sanitize(if p.status.is_empty() { "(aucun)" } else { &p.status }));
                println!("    Version  : {}", sanitize(if p.version.is_empty() { "(aucune)" } else { &p.version }));
                println!();
            }
        }
        return Ok(());
    }

    match cli.command {
        Some(Commands::New) => {
            app.projects = registry::load_registry(&registry_path)?;
            // Will be handled in TUI
        }
        None => {}
    }

    if let Some(keyword) = cli.keyword {
        app.projects = registry::load_registry(&registry_path)?;
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
        // Extract creation_state from input_mode for rendering
        let creation_state = match &input_mode {
            InputMode::CreatingProject(state) => state.clone(),
            _ => ProjectCreationState::default(),
        };

        terminal.draw(|f| ui::ui(f, &mut app, &input_mode, &creation_state))?;

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
                    KeyCode::Char('m') => {
                        if let Some(idx) = app.selected_project_idx() {
                            let modules = modules::list_modules(&app.projects[idx].path);
                            if modules.is_empty() {
                                app.status_message =
                                    Some("Aucun module détecté dans ce projet".to_string());
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
            if let Err(e) = run_launch_sequence(kind, &name, &dir, sudo) {
                app.status_message = Some(format!("Échec du lancement : {e}"));
            }

            // Reprendre le TUI menu
            enable_raw_mode()?;
            execute!(
                terminal.backend_mut(),
                EnterAlternateScreen,
                EnableMouseCapture
            )?;
            terminal.clear()?;
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
fn load_creation_field(state: &mut ProjectCreationState) {
    state.input_buffer = match state.active_field {
        0 => state.id.clone(),
        1 => state.name.clone(),
        2 => state.path.clone(),
        3 => state.description.clone(),
        4 => state.status.clone().unwrap_or_default(),
        5 => state.version.clone(),
        _ => String::new(),
    };
    state.input_offset = state.input_buffer.len();
    state.rewrite_mode = true;
}

fn rescan_repertoire(app: &mut App) -> Result<()> {
    let mut changed = 0usize;
    for entry in &mut app.projects {
        let cargo = entry.path.join("Cargo.toml");
        if let Ok(content) = fs::read_to_string(&cargo) {
            let mut new_version = None;
            for line in content.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("version") {
                    if let Some(r2) = rest.trim_start().strip_prefix('=') {
                        let v = r2.trim().trim_matches('"');
                        if !v.is_empty() {
                            new_version = Some(v.to_string());
                        }
                    }
                    break;
                }
            }
            if let Some(v) = new_version {
                entry.version = v;
                changed += 1;
            }
        }
    }
    if changed > 0 {
        registry::save_registry(&app.registry_path, &app.projects)?;
        app.status_message =
            Some(format!("Rescan : {changed} version(s) resynchronisée(s) depuis le disque"));
    } else {
        app.status_message = Some("Rescan : base déjà alignée sur le disque".to_string());
    }
    app.filtered = (0..app.projects.len()).collect();
    Ok(())
}

fn handle_creation_input(
    key: event::KeyEvent,
    state: &mut ProjectCreationState,
    app: &mut App,
) -> Result<InputMode> {
    const FIELD_COUNT: usize = 6;
    match key.code {
        KeyCode::Esc => {
            return Ok(InputMode::Normal);
        }
        KeyCode::Up | KeyCode::Down => {
            if state.confirm_status {
                state.confirm_status = false;
            }
            let delta = if key.code == KeyCode::Up {
                usize::MAX
            } else {
                1
            };
            if delta == 1 && state.active_field < FIELD_COUNT {
                state.active_field += 1;
            } else if delta == usize::MAX && state.active_field > 0 {
                state.active_field -= 1;
            }
            load_creation_field(state);
            return Ok(InputMode::CreatingProject(state.clone()));
        }
        KeyCode::Left => {
            if state.input_offset > 0 {
                state.input_offset -= 1;
            }
            state.rewrite_mode = false;
            return Ok(InputMode::CreatingProject(state.clone()));
        }
        KeyCode::Right => {
            if state.input_offset < state.input_buffer.len() {
                state.input_offset += 1;
            }
            state.rewrite_mode = false;
            return Ok(InputMode::CreatingProject(state.clone()));
        }
        KeyCode::Backspace => {
            if state.input_offset > 0 {
                let idx = state.input_offset - 1;
                state.input_buffer.remove(idx);
                state.input_offset = idx;
                state.rewrite_mode = false;
            }
            return Ok(InputMode::CreatingProject(state.clone()));
        }
        KeyCode::Enter => {
            // Confirmation d'un nouveau statut hors liste : o/N décidé lors de la frappe.
            if state.confirm_status {
                return Ok(InputMode::CreatingProject(state.clone()));
            }
            match state.active_field {
                0 => {
                    let id = state.input_buffer.trim().replace(' ', "-");
                    if id.is_empty() {
                        return Ok(InputMode::Normal);
                    }
                    state.id = id;
                    state.active_field = 1;
                    load_creation_field(state);
                }
                1 => {
                    state.name = if state.input_buffer.trim().is_empty() {
                        state.id.clone()
                    } else {
                        state.input_buffer.trim().to_string()
                    };
                    state.active_field = 2;
                    load_creation_field(state);
                }
                2 => {
                    state.path = state.input_buffer.trim().to_string();
                    state.active_field = 3;
                    load_creation_field(state);
                }
                3 => {
                    state.description = state.input_buffer.trim().to_string();
                    state.active_field = 4;
                    load_creation_field(state);
                }
                4 => {
                    let value = state.input_buffer.trim().to_string();
                    if value.is_empty() {
                        state.status = None;
                        state.active_field = 5;
                        load_creation_field(state);
                    } else if state.known_statuses.iter().any(|s| s == &value) {
                        state.status = Some(value);
                        state.active_field = 5;
                        load_creation_field(state);
                    } else {
                        state.confirm_status = true;
                            }
                }
                5 => {
                    state.version = state.input_buffer.trim().to_string();
                    state.active_field = 6;
                    load_creation_field(state);
                }
                // Champ de confirmation final o/N : créer le projet (bloc conservé).
                _ => {                let dir = if state.path.is_empty() {
                    dirs::home_dir()
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join("projets")
                        .join(&state.id)
                } else {
                    PathBuf::from(shellexpand::tilde(&state.path).to_string())
                };

                fs::create_dir_all(&dir)?;
                if !dir.join("opencode.json").exists() {
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
                        if v.is_empty() {
                            None
                        } else {
                            Some(v.to_string())
                        }
                    });

                app.projects.push(ProjectEntry {
                    id: state.id.clone(),
                    name: state.name.clone(),
                    path: dir.clone(),
                    description: state.description.clone(),
                    status: status.clone().unwrap_or_default(),
                    version: state.version.clone(),
                    modified: "—".to_string(),
                });
                registry::save_registry(&app.registry_path, &app.projects)?;

                if matches!(
                    state.input_buffer.to_lowercase().as_str(),
                    "o" | "oui" | "y" | "yes"
                ) && status.is_some()
                {
                    app.pending_launch = Some(dir);
                    app.pending_sudo = false;
                }
                return Ok(InputMode::Normal);
                }
            }
            return Ok(InputMode::CreatingProject(state.clone()));
        }
        KeyCode::Char(c) => {
            // Décision o/N sur la confirmation d'un statut hors liste.
            if state.confirm_status {
                match c {
                    'o' | 'O' => {
                        let value = state.input_buffer.trim().to_string();
                        let value = if value.is_empty() {
                            state.status.clone().unwrap_or_default()
                        } else {
                            if !state.known_statuses.contains(&value) {
                                state.known_statuses.push(value.clone());
                            }
                            value
                        };
                        state.status = Some(value.clone());
                        state.confirm_status = false;
                        state.active_field = 5;
                        load_creation_field(state);
                    }
                    'n' | 'N' => {
                        state.status = None;
                        state.input_buffer.clear();
                        state.confirm_status = false;
                        state.active_field = 4;
                        state.rewrite_mode = true;
                    }
                    _ => {}
                }
                return Ok(InputMode::CreatingProject(state.clone()));
            }
            if state.rewrite_mode && state.input_offset >= state.input_buffer.len() {
                state.input_buffer = c.to_string();
                state.input_offset = 1;
                state.rewrite_mode = false;
            } else {
                state.input_buffer.insert(state.input_offset, c);
                state.input_offset += 1;
            }
            return Ok(InputMode::CreatingProject(state.clone()));
        }
        _ => {}
    }
    Ok(InputMode::CreatingProject(state.clone()))
}

#[cfg(test)]
mod tests
  {
    use crate::sanitize;

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
  }