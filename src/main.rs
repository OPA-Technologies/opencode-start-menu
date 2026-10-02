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
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

mod launch;
mod lock;
mod modules;
mod perms;
mod registry;
mod startup;
mod ui;

use crate::launch::{run_launch_sequence, LaunchKind};
use crate::registry::ProjectEntry;
use crate::startup::{Progress, Step};

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

    #[arg(short = 'v', long = "verbose", help = "Trace textuelle des étapes de lancement sur stderr")]
    verbose: bool,

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
    /// Cache des séries de tarifs récupérées de la source web, partagé par
    /// tous les projets (évite un téléchargement par projet).
    pub(crate) web_cache: registry::WebCache,
    /// Préférences globales (onglet Paramètres), en mémoire pour que chaque
    /// bascule s'applique sans recharger le registre.
    pub(crate) prefs: registry::Preferences,
    /// Registre présent mais illisible : aucune réindexation ni réécriture
    /// automatique tant qu'il n'est pas réparé.
    registry_broken: bool,
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
    CostDetail {
        project_idx: usize,
        scroll_offset: usize,
    },
    /// Popup de valorisation du temps passé (touche `t`) : consultation et
    /// réglage du tarif (source web ou saisie manuelle).
    Tariff {
        project_idx: usize,
        /// Champ actif : 0 = mode, 1 = tarif manuel (€/h), 2 = pays source.
        active_field: usize,
        /// Saisie en cours (édition du champ actif).
        editing: bool,
        input_buffer: String,
        input_offset: usize,
        /// Pays source en cours d'édition.
        country: String,
        /// Message d'erreur ou d'information de la dernière action.
        message: Option<String>,
        /// Publication d'un nouveau tarif depuis la source web.
        fetching: bool,
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

/// Écriture atomique : fichier temporaire voisin puis renommage, pour qu'une
/// interruption (Échap dans la jauge, Ctrl+C, plantage) ne laisse jamais un
/// fichier tronqué.
fn write_atomic(path: &Path, contenu: &str) -> Result<()>
  {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, contenu)?;
    if let Ok(meta) = fs::metadata(path)
      {
        let _ = fs::set_permissions(&tmp, meta.permissions());
      }
    if let Err(e) = fs::rename(&tmp, path)
      {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
      }
    Ok(())
  }

/// Réécrit la clé `version` de la section `[package]` de Cargo.toml (guillemets
/// doubles ou simples, commentaire de fin de ligne conservé), ou l'ajoute en
/// fin de section si elle est absente. Une version héritée
/// (`version.workspace = true`, `version = { workspace = true }`) n'est pas
/// touchée. Retourne `true` si le fichier a été modifié.
fn rewrite_manifest_version(dir: &Path, new_version: &str) -> Result<bool>
  {
    let cargo = dir.join("Cargo.toml");
    let Ok(content) = fs::read_to_string(&cargo) else
      {
        return Ok(false);
      };
    let mut out = String::with_capacity(content.len() + 32);
    let mut in_package = false;
    let mut version_seen = false;
    let mut modifie = false;
    for part in content.split_inclusive('\n')
      {
        let trimmed = part.trim();
        let eol = &part[part.trim_end().len()..];
        if trimmed.starts_with('[')
          {
            if in_package && !version_seen
              {
                out.push_str(&format!("version = \"{new_version}\"\n"));
                version_seen = true;
                modifie = true;
              }
            in_package = trimmed.split('#').next().unwrap_or("").trim() == "[package]";
            if in_package
              {
                version_seen = false;
              }
          }
        else if in_package
          && !version_seen
          && let Some(eq) = trimmed.find('=')
          {
            let cle = trimmed[..eq].trim();
            if cle == "version"
              {
                let rest = trimmed[eq + 1..].trim_start();
                match rest.chars().next().filter(|c| *c == '"' || *c == '\'')
                  {
                    Some(q) =>
                      {
                        let after = &rest[1..];
                        if let Some(end) = after.find(q)
                          {
                            let lead = &part[..part.len() - part.trim_start().len()];
                            let tail = &after[end + 1..];
                            out.push_str(lead);
                            out.push_str(&format!("version = \"{new_version}\"{tail}{eol}"));
                            version_seen = true;
                            modifie = true;
                            continue;
                          }
                      }
                    None =>
                      {
                        // Version héritée : on n'y touche pas.
                        version_seen = true;
                        out.push_str(part);
                        continue;
                      }
                  }
              }
            else if cle == "version.workspace"
              {
                version_seen = true;
                out.push_str(part);
                continue;
              }
          }
        out.push_str(part);
      }
    if in_package && !version_seen
      {
        if !out.is_empty() && !out.ends_with('\n')
          {
            out.push('\n');
          }
        out.push_str(&format!("version = \"{new_version}\"\n"));
        modifie = true;
      }
    if modifie
      {
        write_atomic(&cargo, &out)?;
      }
    Ok(modifie)
  }

fn bump_manifest_version(dir: &Path, new_version: &str) -> Result<bool>
  {
    rewrite_manifest_version(dir, new_version)
  }

/// Définit la version exacte dans Cargo.toml (sans incrément).
/// Utilisé lors de l'édition manuelle pour synchroniser le manifeste.
fn set_manifest_version(dir: &Path, version: &str) -> Result<bool>
  {
    rewrite_manifest_version(dir, version)
  }

/// Synchronise la version du projet menu (opencode-start-menu) avec
/// la version du binaire (CARGO_PKG_VERSION). Appelé au démarrage.
/// Ne synchronise QUE si la version binaire est PLUS RÉCENTE que le registre
/// (semver), ou si le registre est vide. Ne JAMAIS écraser un bump AutoPatch.
fn sync_menu_project_version(app: &mut App) -> Result<()>
  {
    let expected_version = env!("CARGO_PKG_VERSION");
    let expected_status = "Beta-Release";
    if let Some(p) = app.projects.iter_mut().find(|p| p.name == "OpenCode Start Menu")
      {
        let registry_version = &p.version_debug;
        let should_sync = registry_version.is_empty()
            || version_greater(expected_version, registry_version);
        if should_sync {
            p.version_debug = expected_version.to_string();
            p.status_debug = expected_status.to_string();
            p.version = p.version_debug.clone();
            p.status = p.status_debug.clone();
            // Sync auto vers CARGO_PKG_VERSION ≠ édition manuelle utilisateur
            // Ne PAS toucher à version_manually_edited ici.
            let _ = set_manifest_version(&p.path, &p.version_debug);
            // Persister la mise à jour dans le registre.
            registry::save_registry_full(&app.registry_path, &app.projects, &app.web_cache, &app.prefs)?;
        }
      }
    Ok(())
  }

/// Compare deux versions semver (x.y.z). Retourne true si a > b.
fn version_greater(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() != 3 { return None; }
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    };
    match (parse(a), parse(b)) {
        (Some((ma, mi, pa)), Some((mb, mib, pb))) => {
            if ma != mb { return ma > mb; }
            if mi != mib { return mi > mib; }
            pa > pb
        }
        _ => false,
    }
}

pub(crate) const MAX_MESSAGE_LINES: usize = 6;

impl App {
    fn new(registry_path: PathBuf, sort_order: SortOrder, progress: &mut Progress) -> Self {
        progress.begin(Step::Paths);
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
            web_cache: registry::WebCache::default(),
            prefs: registry::Preferences::default(),
            registry_broken: false,
        };
        progress.begin(Step::WebCache);
        app.web_cache = registry::load_web_cache(&app.registry_path);
        app.prefs = registry::load_preferences(&app.registry_path);
        progress.begin(Step::Registry);
        match registry::load_registry(&app.registry_path) {
            Ok(projects) => app.projects = projects,
            Err(e) => {
                app.push_error(format!("Registre illisible : {e}"));
                app.registry_broken = true;
            }
        }
        // Rescan au démarrage : chaque projet est relu depuis son Cargo.toml disque,
        // pour que la liste reflète l'état réel du répertoire éponyme dès l'affichage.
        if let Err(e) = rescan_repertoire(&mut app, progress) {
            app.push_error(format!("Rescan au démarrage échoué : {e}"));
        }
        // Auto-sync projet menu : aligner version_debug sur CARGO_PKG_VERSION.
        progress.begin(Step::SyncVersion);
        let _ = sync_menu_project_version(&mut app);
        progress.begin(Step::Finalize);
        refresh_project_selection(&mut app);
        progress.done();
        app
    }

    /// Chargement minimal pour `-l` et `-V` : ni rescan, ni réseau, ni écriture
    /// (aucun bump de Cargo.toml pour une simple consultation).
    fn new_light(registry_path: PathBuf, sort_order: SortOrder) -> Self {
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
            web_cache: registry::WebCache::default(),
            prefs: registry::Preferences::default(),
            registry_broken: false,
        };
        match registry::load_registry(&app.registry_path) {
            Ok(projects) => app.projects = projects,
            Err(e) => eprintln!("Registre illisible : {e}"),
        }
        sort_projects(&mut app.projects, sort_order);
        app
    }

    fn filter(&mut self, needle: &str, mode: LaunchMode) {
        self.mode = mode;
        // Même recherche que le filtre interactif, déléguée à
        // `matching_indexes` : les deux chemins ne peuvent plus diverger, et la
        // normalisation Unicode s'applique donc aussi à la ligne de commande.
        self.filtered = self.matching_indexes(needle);

        if self.filtered.len() == 1 {
            self.launch(self.filtered[0]);
        } else {
            self.list_state.select(if self.filtered.is_empty() { None } else { Some(0) });
        }
    }

    fn matching_indexes(&self, needle: &str) -> Vec<usize> {
        // Les chemins et noms saisis par l'utilisateur ne sont pas forcément
        // dans la même forme Unicode que ceux du registre : « Procédure-BA »
        // peut arriver précomposé (NFC) alors qu'il est stocké décomposé
        // (NFD), ou l'inverse. `to_lowercase` ne réconcilie pas les deux
        // formes, donc la recherche échouait silencieusement. On compare en
        // NFC des deux côtés.
        let nlow = normaliser_recherche(needle);
        self.projects
            .iter()
            .enumerate()
            .filter(|(_, p)| {
                normaliser_recherche(&p.id).contains(&nlow)
                    || normaliser_recherche(&p.name).contains(&nlow)
                    || normaliser_recherche(&p.path.to_string_lossy()).contains(&nlow)
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

/// Signale un binaire plus ancien que les sources qu'il devrait exécuter.
///
/// `op -V` lit la version du registre, mais cette version ne change qu'au
/// moment de la compilation : sans cet avertissement, un `op` qui n'a pas
/// été reconstruit affiche une version figée sans le moindre avertissement.
fn binary_freshness_note(projet: &Path) -> Option<String>
  {
    let exe = std::env::current_exe().ok()?;
    let binaire = exe.metadata().ok()?.modified().ok()?;
    let sources = registry::source_modified_time(projet)?;
    if binaire >= sources
      {
        return None;
      }
    let heure = |t: std::time::SystemTime| -> Option<String> {
      let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
      Some(
        chrono::DateTime::from_timestamp(d.as_secs() as i64, 0)?
          .with_timezone(&chrono::Local)
          .format("%d/%m %H:%M")
          .to_string(),
      )
    };
    Some(format!(
      "⚠ binaire du {} — sources modifiées le {} : exécuter « cargo build »",
      heure(binaire)?,
      heure(sources)?
    ))
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
    println!("  op -v         trace textuelle des étapes de lancement sur stderr,");
    println!("                puis ouverture du menu (défaut : jauge de démarrage)");
    println!("  op -sN        tri alphabétique (--sort-by-name)");
    println!("  op -sM        tri par dernière modification, le plus ancien d'abord");
    println!("                (défaut, --sort-by-date)");
    println!("  op -h         cette aide");
}

pub(crate) fn sanitize(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Forme de comparaison pour la recherche de projets.
///
/// Normalise en NFC puis en minuscules, dans cet ordre : la NFC réconcilie
/// les formes décomposées, la minuscule rend la recherche insensible à la
/// casse. L'ordre importe : minoriser avant de recomposer peut laisser
/// certaines combinaisons décomposées.
fn normaliser_recherche(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect::<String>().to_lowercase()
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

const LOCAL_CURRENCY: &str = "EUR";

fn format_number(v: u64) -> String {
    let loc = if is_french_locale() { NumLocale::fr } else { NumLocale::en };
    v.to_formatted_string(&loc)
}

fn format_currency(cost: f64, currency: &str) -> String {
    let cents = (cost * 100.0).round() / 100.0;
    let symbol = match currency {
        "USD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "JPY" => "¥",
        _ => currency,
    };
    let formatted = if is_french_locale() {
        format!("{:.2}", cents).replace('.', ",")
    } else {
        format!("{:.2}", cents)
    };
    if is_french_locale() {
        format!("{} {}", formatted, symbol)
    } else {
        format!("{}{}", symbol, formatted)
    }
}

/// Coût d'une session ou d'un cumul, ou « — » quand le fournisseur n'en
/// communique aucun.
///
/// Un coût nul accompagné de tokens n'est pas une session gratuite : c'est une
/// donnée absente. opencode écrit `cost = 0` quand le fournisseur ne fournit
/// aucune grille tarifaire (OpenCode Zen, modèles « free »), et le menu
/// affichait « 0,00 € », ce qui laissait croire à une gratuité. Seule une
/// session réellement sans tokens garde un montant à zéro.
fn cout_affiche(cout: f64, tokens: u64, devise: &str) -> String {
  if cout <= 0.0 && tokens > 0 {
    "—".to_string()
  } else {
    format_currency(cout, devise)
  }
}

fn format_datetime(ts: i64) -> String {
    // opencode stocke time_created en millisecondes ; un timestamp en secondes
    // reste en dessous de 10^10 jusqu'en 2286, un ms est > 10^12.
    let secs = if ts > 10_000_000_000 { ts / 1000 } else { ts };
    let dt = chrono::Local.timestamp_opt(secs, 0).single().unwrap_or_default();
    if is_french_locale() {
        dt.format("%d/%m/%Y %H:%M").to_string()
    } else {
        dt.format("%m/%d/%Y %H:%M").to_string()
    }
}

/// Millions avec une décimale, ex. 874_009 → "0.9M" / "0,9M" (locale fr),
/// 41_703_416 → "41.7M" / "41,7M". Zéro final retiré (40_000_000 → "40M").
pub(crate) fn format_millions(v: u64) -> String {
    let m = v as f64 / 1_000_000.0;
    let mut s = format!("{:.1}", m);
    if s.ends_with(".0")
      {
        s.truncate(s.len() - 2);
      }
    if is_french_locale() { s.replace('.', ",") } else { s }
}

// self_variant_label removed — op -V now prints CARGO_PKG_VERSION directly.

/// Répertoire du crate `wrapper`, déduit de l'emplacement du binaire courant
/// (target/debug ou target/release), avec repli sur la constante de compilation.
fn wrapper_source_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().and_then(|p| p.parent()).map(|p| p.join("../wrapper")))
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../wrapper"))
}

/// Empreinte SHA-256 d'un fichier. Sert à comparer le wrapper déployé au
/// binaire release local, sans dépendre d'une version codée en dur qui
/// dériverait à chaque release.
/// Compare deux fichiers octet par octet, par blocs de 64 KiB, pour dire si le
/// wrapper déployé est bien le binaire release local.
///
/// Seule l'égalité importe ici : les deux binaires font une dizaine de Mo et
/// leurs empreintes SHA-256 coûtent ~700 ms en profil Debug, contre ~20 ms
/// pour une comparaison directe bornée par la taille.
/// Remplit `buf` autant que possible : `read` peut rendre moins d'octets que
/// demandé sans que le fichier soit fini.
fn lire_plein(f: &mut fs::File, buf: &mut [u8]) -> io::Result<usize> {
    use std::io::Read;
    let mut total = 0;
    while total < buf.len() {
        let n = f.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

fn fichiers_identiques(a: &Path, b: &Path) -> bool {
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else {
        return false;
    };
    let (Ok(ma), Ok(mb)) = (fa.metadata(), fb.metadata()) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    let mut ba = [0u8; 64 * 1024];
    let mut bb = [0u8; 64 * 1024];
    loop {
        let (na, nb) = match (lire_plein(&mut fa, &mut ba), lire_plein(&mut fb, &mut bb)) {
            (Ok(x), Ok(y)) => (x, y),
            _ => return false,
        };
        if na != nb || ba[..na] != bb[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

/// État du wrapper déployé au regard du binaire release local.
enum WrapperEtat {
    /// Absent de /usr/local/bin.
    Absent,
    /// Présent et identique au binaire local, ou aucun binaire local de
    /// référence pour conclure.
    AJour,
    /// Présent mais différent du binaire local : le deployed est obsolète.
    Perime { local: PathBuf },
}

fn wrapper_etat(installe: &Path) -> WrapperEtat {
    if !installe.exists() {
        return WrapperEtat::Absent;
    }
    let local = wrapper_source_dir().join("target/release/opencode-wrap");
    if !local.exists() {
        // Sans binaire release local de référence, impossible de conclure à une
        // péremption. Déclarer « absent » serait faux — le binaire déployé
        // existe — et proposer une compilation échouerait, la source n'étant pas
        // là. C'est le cas de toute installation packagée, où le wrapper est
        // installé par le canal de distribution.
        return WrapperEtat::AJour;
    }
    if fichiers_identiques(installe, &local) {
        WrapperEtat::AJour
    } else {
        WrapperEtat::Perime { local }
    }
}

/// Vérifie que le wrapper opencode-wrap est installé dans /usr/local/bin et à
/// jour. Si absent ou plus récent que le binaire release local, propose
/// l'installation ou la mise à jour (sudo requis).
fn ensure_wrapper_installed() -> Result<()> {
    let wrapper_path = PathBuf::from("/usr/local/bin/opencode-wrap");
    let (binary_path, mise_a_jour) = match wrapper_etat(&wrapper_path) {
        WrapperEtat::Absent => (wrapper_source_dir().join("target/release/opencode-wrap"), false),
        WrapperEtat::AJour => return Ok(()),
        WrapperEtat::Perime { local } => (local, true),
    };

    // Installation ou mise à jour → n'interrompre que sur un TTY.
    use std::io::{self, Write, IsTerminal};
    if !io::stderr().is_terminal() {
        // Pas en mode interactif (ex: -V, -l, etc.) → ne pas bloquer
        return Ok(());
    }

    eprintln!();
    eprintln!("═══════════════════════════════════════════════════════════");
    if mise_a_jour {
        eprintln!("Wrapper 'opencode-wrap' obsolète dans /usr/local/bin");
        eprintln!();
        eprintln!("Le binaire déployé diffère de wrapper/target/release.");
        eprintln!("Sans mise à jour, le suivi du % de contexte, des coûts");
        eprintln!("et de la devise peut rester sans effet.");
        eprint!("Mettre à jour maintenant ? [O/n] ");
    } else {
        eprintln!("Wrapper 'opencode-wrap' non trouvé dans /usr/local/bin");
        eprintln!();
        eprintln!("Ce wrapper lance opencode et capture le % de contexte");
        eprintln!("à la sortie pour l'afficher dans opencode-start-menu.");
        eprint!("Installer maintenant ? [O/n] ");
    }
    eprintln!();
    eprintln!("L'opération nécessite sudo pour écrire dans /usr/local/bin.");
    io::stderr().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let answer = input.trim().to_lowercase();

    if answer.is_empty() || answer == "o" || answer == "oui" || answer == "y" || answer == "yes" {
        eprintln!(
            "Compilation {} du wrapper...",
            if mise_a_jour { "et mise à jour" } else { "et installation" }
        );

        let wrapper_dir = wrapper_source_dir();

        let status = std::process::Command::new("cargo")
            .args(["build", "--release"])
            .current_dir(&wrapper_dir)
            .status()?;

        if !status.success() {
            eprintln!("Échec de la compilation du wrapper.");
            return Ok(());
        }

        // Copier avec sudo par renommage atomique. Un « cp » direct échoue
        // avec ETXTBSY (« Fichier texte occupé ») dès qu'un wrapper tourne,
        // ce qui est systématiquement le cas : c'est la session lancée par ce
        // menu qui exécute /usr/local/bin/opencode-wrap. rename(2) ne modifie
        // que l'entrée de répertoire, donc le processus en cours garde son
        // ancienne image et n'est pas interrompu.
        let temp_path = "/usr/local/bin/.opencode-wrap.new";
        let copied = std::process::Command::new("sudo")
            .args(["cp", binary_path.to_str().unwrap(), temp_path])
            .status()?
            .success();

        let installed = if copied {
            let moved = std::process::Command::new("sudo")
                .args(["mv", "-f", temp_path, "/usr/local/bin/opencode-wrap"])
                .status()?
                .success();

            if !moved {
                let _ = std::process::Command::new("sudo").args(["rm", "-f", temp_path]).status();
            }

            moved
        } else {
            false
        };

        if installed {
            let verbe = if mise_a_jour { "mis à jour" } else { "installé" };
            eprintln!("✓ Wrapper {verbe} dans /usr/local/bin/opencode-wrap");
            eprintln!("  Le nouveau binaire s'appliquera à la prochaine session.");
        } else {
            eprintln!("Échec de l'installation (sudo requis).");
        }
    } else {
        eprintln!("Opération annulée. Le % contexte ne sera pas à jour.");
    }

    eprintln!("═══════════════════════════════════════════════════════════");
    eprintln!();
    Ok(())
}

/// Avertissements levés par la vérification de `opencode` dans le PATH.
/// En mode jauge ils ne peuvent pas être écrits sur stderr (cela casserait le
/// dessin) : ils sont reportés dans la zone de messages du menu.
fn check_opencode() -> Vec<String>
  {
    match which::which("opencode")
      {
        Err(_) => vec![
          "Avertissement : la commande « opencode » est introuvable dans le PATH.".to_string(),
        ],
        Ok(path) =>
          {
            let under_home = dirs::home_dir().is_some_and(|h| path.starts_with(h));
            if under_home || world_writable(path.parent())
              {
                vec![
                  format!(
                    "Avertissement : « opencode » résolu vers {} (chemin sous le dossier personnel ou modifiable par les autres).",
                    path.display()
                  ),
                ]
              }
            else
              {
                Vec::new()
              }
          }
      }
  }

/// Démarrage instrumenté : la jauge anime l'avancement des étapes pendant que
/// le thread de travail construit l'`App`.
///
/// Renvoie `Ok(None)` si l'utilisateur interrompt (Ctrl+C ou Échap).
fn run_startup_gauge(
    registry_path: PathBuf,
    sort_order: SortOrder,
    avertissements: Vec<String>,
) -> Result<Option<App>>
  {
    use std::{sync::mpsc, thread};

    let mut avertissements = avertissements;
    let (tx, rx) = mpsc::channel::<startup::Signal>();
    // Étapes antérieures au travail : elles ne durent rien, mais elles doivent
    // apparaître dans la jauge pour que la numérotation soit continue.
    {
      let emetteur = startup::Channel(tx.clone());
      let mut progress = Progress::new(&emetteur);
      progress.begin(Step::Args);
      progress.begin(Step::WhichOpencode);
      if !avertissements.is_empty()
        {
          progress.detail(Step::WhichOpencode, avertissements.join(" · "));
        }
      progress.done();
    }

    let worker = thread::spawn(move ||
      {
        let emetteur = startup::Channel(tx.clone());
        let mut progress = Progress::new(&emetteur);
        let app = App::new(registry_path, sort_order, &mut progress);
        let _ = tx.send(startup::Signal::Ready(Box::new(app)));
      });

    let mut terminal = open_terminal()?;
    let mut state = startup::GaugeState::new();
    // La boucle anime la jauge. Elle est isolée dans une fonction pour que
    // `close_terminal` soit atteint même quand une erreur I/O survient : avec
    // les `?` en place ici, une erreur de `draw`, de `poll` ou de `read`
    // remontait directement vers `main`, laissant le shell en mode brut dans
    // l'écran alternatif, où plus rien ne s'affiche. C'est le pire mode de
    // défaillance possible pour un TUI, et il faut un `reset` pour s'en sortir.
    let resultat = animer_jauge(&mut terminal, &mut state, &rx, &mut avertissements);

    close_terminal(&mut terminal);
    drop(worker);
    resultat.unwrap_or(Ok(None))
  }

/// Anime la jauge jusqu'à ce que le démarrage soit prêt, qu'une touche
/// d'annulation arrive, ou qu'une erreur I/O se produise.
///
/// Ne touche pas au terminal : l'appelant reste seul responsable de le
/// refermer, quelle que soit la sortie.
/// Borne `B` explicite : la jauge est ainsi testable avec un `TestBackend`,
/// sans dépendre d'un vrai terminal.
fn animer_jauge<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    state: &mut startup::GaugeState,
    rx: &std::sync::mpsc::Receiver<startup::Signal>,
    avertissements: &mut Vec<String>,
) -> Option<Result<Option<App>>>
  {
    use std::{sync::mpsc::TryRecvError, time::Duration};

    const RAFRAICHISSEMENT: Duration = Duration::from_millis(40);

    let mut issue: Option<Result<Option<App>>> = None;
    'attente: while issue.is_none()
      {
        state.tick();
        if let Err(e) = terminal.draw(|f| startup::render(f, &state))
          {
            issue = Some(Err(e.into()));
            break;
          }
        match event::poll(RAFRAICHISSEMENT)
          {
            Err(e) =>
              {
                issue = Some(Err(e.into()));
                break;
              }
            Ok(false) =>
              {
                // délai expiré : on draine le canal ci-dessous
              }
            Ok(true) =>
              {
                match event::read()
                  {
                    Err(e) =>
                      {
                        issue = Some(Err(e.into()));
                        break;
                      }
                    Ok(Event::Key(key)) =>
                      {
                        if key.kind == KeyEventKind::Press
                            && (ctrl_quit(key) || key.code == KeyCode::Esc)
                          {
                            issue = Some(Ok(None));
                            break 'attente;
                          }
                      }
                    Ok(_) =>
                      {
                        // souris ou redimensionnement : sans effet sur la jauge
                      }
                  }
              }
          }
        loop
          {
            match rx.try_recv()
              {
                Ok(startup::Signal::Step(e)) => state.apply(e),
                Ok(startup::Signal::Ready(app)) =>
                  {
                    let mut app = *app;
                    app.status_messages.extend(std::mem::take(avertissements));
                    issue = Some(Ok(Some(app)));
                    break;
                  }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) =>
                  {
                    issue = Some(Err(anyhow::anyhow!("le thread de démarrage s'est arrêté")));
                    break;
                  }
              }
          }
      }
    issue
  }

fn main() -> Result<()> {
    let cli = Cli::parse();

    if cli.help {
        print_help();
        return Ok(());
    }

    let registry_path = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("opencode/projects.json");

    let sort_order = if cli.sort_by_name {
        SortOrder::Name
    } else if cli.sort_by_date {
        SortOrder::Modified
    } else {
        cli.sort
    };

    // La jauge ne concerne que le chemin interactif : -l, -V, -h et le
    // lancement direct par mot-clé restent silencieux et rapides.
    let interactif = !cli.help && !cli.list && !cli.version && cli.keyword.is_none();

    let verbose = if cli.verbose
      {
        Some(startup::Verbose::new())
      }
    else
      {
        None
      };

    if interactif
      {
        // Restaure le terminal si un panic survient en mode brut / écran alterné.
        let defaut = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info|
          {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
            defaut(info);
          }));
      }

    // -l et -V ne font que lire le registre : ni rescan, ni réseau, ni bump.
    let leger = cli.list || cli.version;
    let avertissements = if leger { Vec::new() } else { check_opencode() };

    let mut app = if leger
      {
        App::new_light(registry_path.clone(), sort_order)
      }
    else
      {
        match verbose.as_ref()
      {
        Some(journal) =>
          {
            let mut progress = Progress::new(journal);
            progress.begin(Step::Args);
            progress.begin(Step::WhichOpencode);
            if !avertissements.is_empty()
              {
                progress.detail(Step::WhichOpencode, avertissements.join(" · "));
              }
            progress.done();
            App::new(registry_path.clone(), sort_order, &mut progress)
          }
        None if interactif && io::stdout().is_terminal() =>
          {
            match run_startup_gauge(registry_path.clone(), sort_order, avertissements)?
              {
                Some(app) => app,
                None =>
                  {
                    eprintln!("Démarrage annulé.");
                    return Ok(());
                  }
              }
          }
        None =>
          {
            for a in &avertissements
              {
                eprintln!("{a}");
              }
            App::new(registry_path.clone(), sort_order, &mut Progress::noop())
          }
      }
      };

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
                    let p = &app.projects[*i];
                    println!(
                        "{} : Debug={} ({}) Release={} ({})",
                        sanitize(&p.name),
                        sanitize(&p.version_debug),
                        sanitize(&p.status_debug),
                        sanitize(&p.version_release),
                        sanitize(&p.status_release)
                    );
                }
                _ => {
                    for i in &idx {
                        let p = &app.projects[*i];
                        println!(
                            "{} : Debug={} ({}) Release={} ({})",
                            sanitize(&p.name),
                            sanitize(&p.version_debug),
                            sanitize(&p.status_debug),
                            sanitize(&p.version_release),
                            sanitize(&p.status_release)
                        );
                    }
                }
            }
        } else {
            // Version du menu lui-même (depuis le registre si possible).
            if let Some(p) = app.projects.iter().find(|p| p.name == "OpenCode Start Menu") {
                println!("{}_{}", sanitize(&p.version_debug), sanitize(&p.status_debug).replace(['-', ' '], "_"));
                // La version affichée ne change qu'à la compilation : sans cette
                // ligne, un binaire vieux de plusieurs commits affiche encore
                // l'ancienne version, sans aucun signe visible du décalage.
                if let Some(note) = binary_freshness_note(&p.path) {
                    println!("{note}");
                }
            } else {
                println!("{}", env!("CARGO_PKG_VERSION"));
            }
        }
        return Ok(());
    }

    match cli.command {
        Some(Commands::New) => {
            // Will be handled in TUI
        }
        None => {}
    }

    let a_mot_cle = cli.keyword.is_some();
    if let Some(keyword) = cli.keyword.clone() {
        app.filter(&keyword, if cli.root { LaunchMode::Root } else { LaunchMode::Normal });
        if let Some(dir) = app.pending_launch.take() {
            let name = app.pending_name.take()
                .unwrap_or_else(|| dir.display().to_string());
            run_launch_sequence(LaunchKind::Resume, &name, &dir, app.pending_sudo)?;
            return Ok(());
        }
        if app.filtered.len() <= 1 {
            // Aucun projet, ou un seul dont le lancement a échoué : dire pourquoi.
            if app.status_messages.is_empty() {
                eprintln!("Aucun projet ne correspond à : {keyword}");
            } else {
                for m in &app.status_messages {
                    eprintln!("{m}");
                }
            }
            return Ok(());
        }
        // Plusieurs correspondances : menu restreint à ces projets.
    }

    if cli.root && !a_mot_cle {
        eprintln!("Erreur : l'option --root requiert un mot-clé de projet.");
        return Ok(());
    }

    // Vérifier et installer le wrapper si nécessaire (une seule fois, mode interactif seulement).
    // Cette étape lit une réponse sur stdin : elle a donc lieu en mode cooked,
    // jamais à l'intérieur de la jauge ni du menu.
    let debut_wrapper = std::time::Instant::now();
    let mut wrapper_ok = true;
    if let Err(e) = ensure_wrapper_installed() {
        eprintln!("Erreur wrapper: {e}");
        wrapper_ok = false;
    }
    if let Some(journal) = verbose.as_ref()
      {
        startup::trace_tail(journal, debut_wrapper.elapsed(), wrapper_ok);
      }

    // Session terminal unique : le menu réutilise le même terminal, et la
    // restauration est garantie même si le menu remonte une erreur.
    let debut_terminal = std::time::Instant::now();
    let mut terminal = open_terminal()?;
    let resultat = run_tui(app, &mut terminal);
    close_terminal(&mut terminal);
    if let Some(journal) = verbose.as_ref()
      {
        startup::trace_terminal(journal, debut_terminal.elapsed());
      }
    resultat
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// Ctrl+C ou Ctrl+D : quitter. Sans modificateur, la touche « c » garde son
/// sens (popup Coût) et « q » la sortie normale.
fn ctrl_quit(key: crossterm::event::KeyEvent) -> bool
  {
    use crossterm::event::KeyModifiers;
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d') | KeyCode::Char('\u{3}'))
  }

/// Établit la session terminal (mode brut + écran alternatif + souris).
///
/// Chaque étape qui peut échouer après `enable_raw_mode` restaure le mode
/// précédent avant de propager l'erreur : sans cela, un échec de `execute!`
/// ou de `clear` laisserait le shell suivant en mode brut dans l'écran
/// alternatif, où plus rien ne s'affiche.
fn open_terminal() -> Result<Term>
  {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(e) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
      {
        let _ = disable_raw_mode();
        return Err(e.into());
      }
    let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
    if let Err(e) = terminal.clear()
      {
        let _ = close_terminal(&mut terminal);
        return Err(e.into());
      }
    Ok(terminal)
  }

/// Rend le terminal à l'interpréteur. Appelé sur TOUS les chemins de sortie,
/// y compris les erreurs : sans cela, le shell suivant démarre en mode brut
/// dans l'écran alternatif et plus rien ne s'affiche.
fn close_terminal(terminal: &mut Term)
  {
    let _ = disable_raw_mode();
    let _ = execute!(
      terminal.backend_mut(),
      LeaveAlternateScreen,
      DisableMouseCapture
    );
    let _ = terminal.show_cursor();
    let _ = terminal.backend_mut().flush();
  }

fn run_tui(app: App, terminal: &mut Term) -> Result<()> {
    tui_loop(terminal, app)
}

fn tui_loop(terminal: &mut Term, mut app: App) -> Result<()> {
    let mut input_mode = InputMode::Normal;

    loop {
        // États de création et d'édition, extraits de `input_mode` pour le rendu.
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

            // Ctrl+C / Ctrl+D quittent depuis n'importe quel mode : en mode
            // brut, ISIG est coupé et l'octet 0x03 arrivait ici comme une
            // simple touche « c », qui ouvrait le popup Coût au lieu de sortir.
            if ctrl_quit(key)
              {
                break;
              }

            input_mode = match input_mode {
                InputMode::Normal => match key.code {
                    KeyCode::Char('r') | KeyCode::Char('R') => {
                        if let Err(e) = rescan_repertoire(&mut app, &mut Progress::noop()) {
                            app.push_error(format!("Rescan échoué : {e}"));
                        }
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
                    KeyCode::Char('c') => {
                        if let Some(idx) = app.selected_project_idx() {
                            InputMode::CostDetail {
                                project_idx: idx,
                                scroll_offset: 0,
                            }
                        } else {
                            InputMode::Normal
                        }
                    }
                    KeyCode::Char('t') | KeyCode::Char('T') => {
                        if let Some(idx) = app.selected_project_idx() {
                            InputMode::Tariff {
                                project_idx: idx,
                                active_field: 0,
                                editing: false,
                                input_buffer: String::new(),
                                input_offset: 0,
                                country: app.web_cache.country.clone(),
                                message: None,
                                fetching: false,
                            }
                        } else {
                            InputMode::Normal
                        }
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
                    match handle_creation_input(key, &mut state, &mut app) {
                        Ok(mode) => mode,
                        Err(e) => {
                            app.push_error(format!("Création impossible : {e}"));
                            InputMode::Normal
                        }
                    }
                }
                InputMode::EditingProject(mut state) => {
                    match handle_edit_input(key, &mut state, &mut app) {
                        Ok(mode) => mode,
                        Err(e) => {
                            app.push_error(format!("Édition impossible : {e}"));
                            InputMode::Normal
                        }
                    }
                }
                InputMode::CostDetail {
                    project_idx,
                    mut scroll_offset,
                } => match key.code {
                    KeyCode::Esc => InputMode::Normal,
                    KeyCode::Up | KeyCode::Char('k') => {
                        if scroll_offset > 0 {
                            scroll_offset -= 1;
                        }
                        InputMode::CostDetail {
                            project_idx,
                            scroll_offset,
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        scroll_offset = scroll_offset.saturating_add(1);
                        InputMode::CostDetail {
                            project_idx,
                            scroll_offset,
                        }
                    }
                    KeyCode::PageUp => {
                        if scroll_offset >= 10 {
                            scroll_offset -= 10;
                        } else {
                            scroll_offset = 0;
                        }
                        InputMode::CostDetail {
                            project_idx,
                            scroll_offset,
                        }
                    }
                    KeyCode::PageDown => {
                        scroll_offset = scroll_offset.saturating_add(10);
                        InputMode::CostDetail {
                            project_idx,
                            scroll_offset,
                        }
                    }
                    KeyCode::Home => {
                        scroll_offset = 0;
                        InputMode::CostDetail {
                            project_idx,
                            scroll_offset,
                        }
                    }
                    KeyCode::End => {
                        scroll_offset = usize::MAX;
                        InputMode::CostDetail {
                            project_idx,
                            scroll_offset,
                        }
                    }
                    _ => InputMode::CostDetail {
                        project_idx,
                        scroll_offset,
                    },
                },
                InputMode::Tariff {
                    project_idx,
                    active_field,
                    editing,
                    input_buffer,
                    input_offset,
                    country,
                    message,
                    fetching,
                } => match handle_tariff_input(
                    key,
                    &mut app,
                    project_idx,
                    active_field,
                    editing,
                    input_buffer,
                    input_offset,
                    country,
                    message,
                    fetching,
                ) {
                    Ok(mode) => mode,
                    Err(e) => {
                        app.push_error(format!("Valorisation : {e}"));
                        InputMode::Normal
                    }
                },
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
            let launch_err = run_launch_sequence(kind, &name, &dir, sudo).err();

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
            // Rescan après toute session, y compris terminée par Ctrl+C (code de
            // sortie non nul) : une compilation a pu avoir lieu. Le message
            // d'échec est posté après, le rescan remplaçant les messages.
            if let Err(e) = rescan_repertoire(&mut app, &mut Progress::noop()) {
                app.push_error(format!("Rescan post-session échoué : {e}"));
            }
            if let Some(e) = launch_err {
                app.push_error(format!("Échec du lancement : {e}"));
            }
        }
    }

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

fn rescan_repertoire(app: &mut App, progress: &mut Progress) -> Result<()>
  {
    if app.registry_broken
      {
        // Registre illisible : ne rien réindexer ni réécrire, sinon une
        // sauvegarde le remplacerait par un registre quasi vide.
        finalize_project_list(app);
        return Ok(());
      }
    let selection = app
      .selected_project_idx()
      .map(|i| app.projects[i].id.clone());
    let mut alertes: Vec<String> = Vec::new();
    let mut changed = 0usize;
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut written = false;
    progress.begin(Step::RescanPurge);
    app.projects.retain(|p|
      {
        // Un projet n'est retiré que si son dossier a disparu alors que le
        // dossier parent existe : un disque démonté ou un partage réseau
        // absent ne doit pas effacer le registre.
        let disparu = !p.path.exists() && p.path.parent().is_some_and(|d| d.is_dir());
        if !disparu && !registry::is_excluded(&p.path)
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
    progress.begin(Step::RescanIndex);
    if let Some(root) = projects_root(&app.projects)
      {
        match registry::reindex_from_disk(&root, &mut app.projects)
          {
            Ok(n) => added = n,
            Err(e) => alertes.push(format!("Indexation impossible : {e}")),
          }
      }
    progress.begin(Step::RescanUsage);
    let usage = registry::load_usage(&app.opencode_db_path);
    // Rafraîchissement de la source de tarifs : au plus une fois par période
    // de fraîcheur, et seulement si un projet utilise effectivement le mode
    // web (pas de requête inutile pour l'utilisateur en mode manuel).
    let maintenant = registry::maintenant_ms();
    let mode_web_utilise = app
        .projects
        .iter()
        .any(|p| p.tarif_mode != "manuel");
    progress.begin(Step::RescanTarifs);
    if mode_web_utilise && app.web_cache.a_fetcher(maintenant) {
        let pays = app.web_cache.country.clone();
        let rafraichit = app.web_cache.rafraichir(&pays, maintenant, false);
        if rafraichit || !app.web_cache.last_error.is_empty() {
            written = true;
        }
        if !app.web_cache.last_error.is_empty() {
            alertes.push(app.web_cache.last_error.clone());
        }
    } else {
        progress.skip(Step::RescanTarifs);
    }
    // La boucle par projet couvre deux étapes distinctes : on chronomètre
    // chaque moitié séparément pour les restituer indépendamment.
    progress.begin(Step::RescanManifests);
    let total_projets = app.projects.len();
    let mut t_manifestes = std::time::Duration::ZERO;
    let mut t_autopatch = std::time::Duration::ZERO;
    let mut bumps: Vec<String> = Vec::new();
    for (rang, entry) in app.projects.iter_mut().enumerate()
      {
        progress.detail(Step::RescanManifests, format!("projet {}/{}", rang + 1, total_projets));
        let avant = signature_persistee(entry);
        let debut = std::time::Instant::now();
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
              }
          }
        let agrege = registry::usage_du_projet(&usage, &entry.path);
        if let Some(u) = agrege.as_ref()
          {
            let cum = &u.cumulative;
            entry.token = format_number(cum.total_tokens);
            // Budget 100M tokens pour le calcul du pourcentage (affiché dans Totaux).
            const BUDGET_TOKENS: u64 = 100_000_000;
            let pct = (cum.total_tokens * 100) / BUDGET_TOKENS;
            entry.used_pct = format!("{}", pct);
            entry.spent = cout_affiche(cum.total_cost_local, cum.total_tokens, LOCAL_CURRENCY);
            entry.total_tokens = format_number(cum.total_tokens);
            entry.total_cost = cout_affiche(cum.total_cost_local, cum.total_tokens, LOCAL_CURRENCY);
            entry.session_count = format!("{}", cum.session_count);
            // --- Temps passé et valorisation du temps -------------------
            // La durée d'une session est l'écart entre son premier et son
            // dernier message ; le cumul additionne toutes les sessions.
            entry.total_duree = registry::format_duree(cum.total_duration_ms);
            let (serie, secteur, fetched_ms) = registry::serie_du_projet(entry, &app.web_cache);
            // En mode web, la série du cache (pays courant) fait référence :
            // si elle est plus récente que la copie du projet, on la adopte.
            // Cela prend en compte un changement de pays ou une actualisation.
            if entry.tarif_mode != "manuel"
              && !serie.is_empty()
              && (entry.tarif_web_series.is_empty()
                || entry.tarif_web_fetched_ms < fetched_ms
                || (fetched_ms > 0 && entry.tarif_web_fetched_ms == 0)
                // Série rapportée à un autre pays (changement de pays source)
                || !entry
                  .tarif_web_pays
                  .eq_ignore_ascii_case(&app.web_cache.country))
              {
                entry.tarif_web_series = serie.clone();
                entry.tarif_web_secteur = secteur;
                entry.tarif_web_pays = app.web_cache.country.clone();
                entry.tarif_web_fetched_ms = fetched_ms;
                written = true;
              }
            if let Some(ls) = &u.last_session
              {
                entry.last_session_tokens = format_number(ls.tokens);
                entry.last_session_cost = cout_affiche(ls.cost, ls.tokens, &ls.currency);
                entry.last_model = ls.model.clone();
                entry.last_agent = ls.agent.clone();
                entry.last_time_created = format_datetime(ls.time_created);
                entry.last_session_context_pct = format!("{}%", ls.context_pct);
                entry.last_duree = registry::format_duree(ls.duration_ms);
                // La session est valorisée au tarif en vigueur à sa date.
                entry.last_valorisation =
                  match registry::valoriser_session(
                    ls.duration_ms,
                    ls.time_created,
                    &serie,
                    secteur,
                    fetched_ms,
                    &entry.tarif_mode,
                    entry.tarif_manual,
                  ) {
                    Some(v) => format!(
                      "{} ({})",
                      format_currency(v.euros, LOCAL_CURRENCY),
                      v.etiquette()
                    ),
                    None => "—".to_string(),
                  };
              }
            else
              {
                entry.last_session_tokens = String::new();
                // Aucune session rattachée au projet : rien à afficher, pas un
                // montant à zéro.
                entry.last_session_cost = "—".to_string();
                entry.last_model = String::new();
                entry.last_agent = String::new();
                entry.last_time_created = String::new();
                entry.last_session_context_pct = "0%".to_string();
                entry.last_duree = "—".to_string();
                entry.last_valorisation = "—".to_string();
              }
            // Le cumul applique le tarif de chaque session à sa propre date
            // (carry-forward) puis somme les montants.
            entry.total_valorisation =
              match registry::valoriser_cumul(
                &u.sessions,
                &serie,
                secteur,
                fetched_ms,
                &entry.tarif_mode,
                entry.tarif_manual,
              ) {
                Some(v) => format!(
                  "{} ({})",
                  format_currency(v.euros, LOCAL_CURRENCY),
                  v.etiquette()
                ),
                None => "—".to_string(),
              };
            entry.tarif_resume = resume_tarif(entry, &serie, secteur, fetched_ms);
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
            entry.total_duree = "—".to_string();
            entry.last_duree = "—".to_string();
            entry.last_valorisation = "—".to_string();
            entry.total_valorisation = "—".to_string();
            entry.tarif_resume = resume_tarif(entry, &[], false, 0);
          }
        t_manifestes += debut.elapsed();
        // AutoPatch : détection des modifications de code via hash SHA256
        // (Cargo.toml + src/**/*.rs) — plus fiable que mtime target/debug.
        let debut = std::time::Instant::now();
        let current_hash = registry::compute_source_hash(&entry.path);
        let hash_changed = current_hash != entry.source_hash;

        if hash_changed {
            // Modification de code détectée
            let baseline = entry.source_hash.is_empty();
            entry.source_hash = current_hash.clone();
            // Mettre à jour la référence mtime pour cohérence
            let t_debug = registry::variant_modified_time(&entry.path, "debug")
              .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()));
            if let Some(t) = t_debug {
                entry.last_compile_debug_secs = t;
            }
            written = true;

            if entry.version_manually_edited {
                // Édition manuelle → PAS de bump, reset flag seulement
                entry.version_manually_edited = false;
            } else {
                // Modif code → bump z normal
                let valid = registry::validate_version(&entry.version_debug);
                let init = !valid;
                if init || !baseline {
                    let new_version = if valid {
                        bump_patch(&entry.version_debug)
                    } else {
                        Some("0.0.1".to_string())
                    };
                    if let Some(v) = new_version {
                        let old = if entry.version_debug.is_empty() {
                            "—".to_string()
                        } else {
                            entry.version_debug.clone()
                        };
                        entry.version_debug = v.clone();
                        // La Release n'est PAS bumpée ici : son binaire n'a pas
                        // été reconstruit, elle reste à sa version antérieure.
                        entry.version = v.clone();
                        if let Err(e) = bump_manifest_version(&entry.path, &v)
                          {
                            alertes.push(format!("{} : Cargo.toml non mis à jour ({e})", entry.name));
                          }
                        // Resynchroniser le hash sur le manifeste APRÈS écriture.
                        // Sans cela le hash stocké décrit l'ancienne version, alors
                        // que le manifeste porte la nouvelle : le rescan suivant y
                        // voit une fausse modification et rebump à chaque
                        // lancement.
                        entry.source_hash = registry::compute_source_hash(&entry.path);
                        bumps.push(format!("{} : {old} → {v}", entry.name));
                    }
                }
            }
        }

        // Release rattrapage : si target/release reconstruit
        let t_release = registry::variant_modified_time(&entry.path, "release")
          .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()));
        if let Some(t) = t_release {
            if t > entry.last_compile_release_secs {
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
                  }
                written = true;
              }
          }
        t_autopatch += debut.elapsed();
        if signature_persistee(entry) != avant
          {
            written = true;
            changed += 1;
          }
      }
    progress.close(
      Step::RescanManifests,
      t_manifestes,
      format!("{total_projets} projet(s)"),
    );
    progress.close(
      Step::AutoPatch,
      t_autopatch,
      format!(
        "{total_projets} projet(s), {} bump(s)",
        bumps.len()
      ),
    );
    if added > 0 || changed > 0 || written
      {
        progress.begin(Step::RescanSave);
        registry::save_registry_full(&app.registry_path, &app.projects, &app.web_cache, &app.prefs)?;
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
        progress.done();
      }
    else
      {
        app.push_info("Rescan : base déjà alignée sur le disque".to_string());
        progress.skip(Step::RescanSave);
      }
    // NOTE: sync_menu_project_version appelé UNIQUEMENT au démarrage (App::new).
    // Le rescan lit déjà Cargo.toml pour tous les projets (lignes 1308-1338),
    // ce qui synchronise aussi le projet menu. Ne pas écraser un AutoPatch bump ici.
    finalize_project_list(app);
    // Conserver la sélection courante plutôt que la ramener au dernier projet.
    if let Some(id) = selection
      && let Some(pos) = app.projects.iter().position(|p| p.id == id)
      {
        app.list_state.select(Some(pos));
      }
    for a in alertes
      {
        app.push_error(a);
      }
    Ok(())
  }

/// Signature des champs persistés qu'un rescan peut modifier : sert à ne
/// réécrire le registre que si quelque chose a réellement changé.
fn signature_persistee(e: &ProjectEntry) -> String
  {
    [
      e.version.clone(),
      e.version_debug.clone(),
      e.status_debug.clone(),
      e.status_release.clone(),
      e.version_release.clone(),
      e.token.clone(),
      e.used_pct.clone(),
      e.spent.clone(),
      e.total_tokens.clone(),
      e.total_cost.clone(),
      e.session_count.clone(),
      e.last_session_tokens.clone(),
      e.last_session_cost.clone(),
      e.last_model.clone(),
      e.last_agent.clone(),
      e.last_time_created.clone(),
      e.last_session_context_pct.clone(),
      e.last_duree.clone(),
      e.total_duree.clone(),
      e.last_valorisation.clone(),
      e.total_valorisation.clone(),
      e.tarif_resume.clone(),
      e.source_hash.clone(),
      e.tarif_web_pays.clone(),
      e.tarif_web_fetched_ms.to_string(),
      e.tarif_web_series.len().to_string(),
      e.tarif_web_secteur.to_string(),
      e.last_compile_debug_secs.to_string(),
      e.last_compile_release_secs.to_string(),
      e.version_manually_edited.to_string(),
    ]
    .join("|")
  }

/// Résumé lisible du tarif appliqué, affiché dans le popup (touche `t`).
/// Exemple : « web · 21,58 €/h · OIT/ILOSTAT (secteur J) · 2024 ·
/// récupéré le sept. 2026 ».
fn resume_tarif(
    p: &ProjectEntry,
    serie: &[registry::TarifPoint],
    secteur: bool,
    _fetched_ms: i64,
) -> String {
    if p.tarif_mode == "manuel" {
        return if p.tarif_manual > 0.0 {
            format!(
                "manuel · {} /h saisi",
                format_currency(p.tarif_manual, LOCAL_CURRENCY)
            )
        } else {
            "manuel · aucun tarif saisi".to_string()
        };
    }
    if serie.is_empty() {
        return "web · aucune donnée source".to_string();
    }
    // Dernier point connu de la série (le plus récent).
    let Some(dernier) = serie.last() else {
        return "web · aucune donnée source".to_string();
    };
    let annee = dernier.annee;
    let granularite = if secteur { "secteur J" } else { "tous secteurs" };
    format!(
        "web · {} /h · {granularite} · {annee}",
        format_currency(dernier.valeur_eur_h, LOCAL_CURRENCY)
    )
}

fn initial_selection_index(order: SortOrder, len: usize) -> usize {
    if order == SortOrder::Modified {
        len.saturating_sub(1)
    } else {
        0
    }
}

/// Clé de tri projet, calculée UNE fois par projet.
///
/// `last_modified_time` parcourt récursivement tout l'arborescence du projet,
/// `target/` compris : l'appeler dans le comparateur de `sort_by` le rendait
/// O(n log n) fois, soit plus d'une seconde de démarrage sur huit projets.
/// On décore donc chaque entrée avant de trier (decorate-sort-undecorate).
enum CleTri
  {
    /// Tri alphabétique insensible à la casse.
    Nom(String),
    /// Tri par date de modification, du plus ancien au plus récent.
    Modifie(Option<std::time::SystemTime>),
  }

fn sort_projects(projects: &mut Vec<ProjectEntry>, order: SortOrder) {
    let cles: Vec<CleTri> = projects
        .iter()
        .map(|p| match order
          {
            SortOrder::Name => CleTri::Nom(p.name.to_lowercase()),
            SortOrder::Modified => CleTri::Modifie(registry::last_modified_time(&p.path)),
          })
        .collect();

    let mut entrees: Vec<(CleTri, ProjectEntry)> = projects
        .drain(..)
        .zip(cles)
        .map(|(p, cle)| (cle, p))
        .collect();
    entrees.sort_by(|(a, _), (b, _)| match (a, b)
      {
        (CleTri::Nom(x), CleTri::Nom(y)) => x.cmp(y),
        (CleTri::Modifie(Some(x)), CleTri::Modifie(Some(y))) => x.cmp(y),
        (CleTri::Modifie(None), CleTri::Modifie(Some(_))) => Ordering::Greater,
        (CleTri::Modifie(Some(_)), CleTri::Modifie(None)) => Ordering::Less,
        (CleTri::Modifie(None), CleTri::Modifie(None)) => Ordering::Equal,
        // Tri inhomogène : impossible, `order` est fixé pour tout l'appel.
        (CleTri::Nom(_), _) | (_, CleTri::Nom(_)) => Ordering::Equal,
      });
    projects.extend(entrees.into_iter().map(|(_, p)| p));
}

fn finalize_project_list(app: &mut App) {
    sort_projects(&mut app.projects, app.sort_order);
    refresh_project_selection(app);
}

/// Recalcule le filtrage et la sélection sans retrier.
///
/// `rescan_repertoire` trie déjà la liste ; le startup n'a plus qu'à
/// réinitialiser la sélection. Retrier ici reparcourrait `target/` de tous les
/// projets une seconde fois pour un résultat identique, puisque rien n'a
/// modifié les dates de modification.
fn refresh_project_selection(app: &mut App) {
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

                    if state.id.trim().is_empty()
                      {
                        app.push_error("Identifiant obligatoire".to_string());
                        return Ok(InputMode::CreatingProject(state.clone()));
                      }
                    if app.projects.iter().any(|p| p.id == state.id)
                      {
                        app.push_error(format!("Un projet d'identifiant « {} » existe déjà", state.id));
                        return Ok(InputMode::CreatingProject(state.clone()));
                      }

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

                    let status = state.status.clone().unwrap_or_default();
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
                        last_session_context_pct: String::new(),
                        status_debug: status.clone(),
                        version_debug: version.clone(),
                        status_release: status.clone(),
                        version_release: version.clone(),
debug_modified: "jamais compilée".to_string(),
                        release_modified: "jamais compilée".to_string(),
                        last_compile_debug_secs: 0,
                        last_compile_release_secs: 0,
                        version_manually_edited: false,
                        source_hash: String::new(),
                        tarif_mode: "web".to_string(),
                        tarif_manual: 0.0,
                        tarif_web_series: Vec::new(),
                        tarif_web_fetched_ms: 0,
                        tarif_web_secteur: false,
                        tarif_web_pays: String::new(),
                        last_duree: String::new(),
                        total_duree: String::new(),
                        last_valorisation: String::new(),
                        total_valorisation: String::new(),
                        tarif_resume: String::new(),
                    });
                    registry::save_registry_full(&app.registry_path, &app.projects, &app.web_cache, &app.prefs)?;
                    finalize_project_list(app);

                    if matches!(
                      state.input_buffer.to_lowercase().as_str(),
                      "o" | "oui" | "y" | "yes"
                    )
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
    let Some(p) = app.projects.get_mut(state.project_idx) else {
        return false;
    };
    // Une valeur inchangée reste acceptée même héritée d'un ancien registre
    // (statut libre, version vide) : seules les valeurs modifiées sont validées.
    let version_ok = state.pending_version == p.version_debug
        || registry::validate_version(&state.pending_version);
    let statut_ok = state.pending_status == p.status_debug
        || registry::validate_status(&state.pending_status);
    let desc_ok = !state.pending_description.trim().is_empty()
        || state.pending_description == p.description;
    if !(version_ok && statut_ok && desc_ok) {
        return false;
    }
    p.version_debug = state.pending_version.clone();
    p.status_debug = state.pending_status.clone();
    p.description = state.pending_description.clone();
    // Sans miroir : la Release garde sa version/statut tant que son
    // binaire n'est pas reconstruit (rattrapage au prochain rescan).
    // Champs historiques alignés sur la variante élue (Debug).
    p.version = p.version_debug.clone();
    p.status = p.status_debug.clone();
    if registry::validate_version(&p.version_debug) {
        // Le drapeau « édité manuellement » n'a de sens que si l'édition a
        // réellement modifié le manifeste : sinon il resterait levé et
        // avalerait le prochain vrai bump.
        let avant = registry::compute_source_hash(&p.path);
        let _ = set_manifest_version(&p.path, &p.version_debug);
        if registry::compute_source_hash(&p.path) != avant {
            p.version_manually_edited = true;
        }
    }
    true
}

const EDIT_CONFIRM_FIELD: usize = 3;

/// Nombre de champs éditables du popup de valorisation :
/// 0 = mode, 1 = tarif manuel, 2 = pays source.
const TARIF_CHAMPS: usize = 3;

/// Gestionnaire clavier du popup « Valorisation du temps » (touche `t`).
#[allow(clippy::too_many_arguments)]
fn handle_tariff_input(
  key: event::KeyEvent,
  app: &mut App,
  mut project_idx: usize,
  active_field: usize,
  editing: bool,
  mut input_buffer: String,
  mut input_offset: usize,
  country: String,
  mut message: Option<String>,
  _fetching: bool,
) -> Result<InputMode>
   {
    macro_rules! popup
      {
        ($editing:expr, $buffer:expr, $offset:expr, $msg:expr) =>
          {
            InputMode::Tariff {
              project_idx,
              active_field,
              editing: $editing,
              input_buffer: $buffer,
              input_offset: $offset,
              country,
              message: $msg,
              fetching: false,
            }
          };
      }

    // Some project might have been removed while the popup was open.
    if project_idx >= app.projects.len()
      {
        return Ok(InputMode::Normal);
      }

    // Saisie en cours sur le champ actif.
    if editing
      {
        match key.code
          {
            KeyCode::Esc => return Ok(popup!(false, String::new(), 0, message)),
            KeyCode::Left =>
              {
                if input_offset > 0
                  {
                    input_offset -= 1;
                  }
                return Ok(popup!(true, input_buffer, input_offset, message));
              }
            KeyCode::Right =>
              {
                if input_offset < input_buffer.chars().count()
                  {
                    input_offset += 1;
                  }
                return Ok(popup!(true, input_buffer, input_offset, message));
              }
            KeyCode::Backspace =>
              {
                if input_offset > 0
                  {
                    let idx = input_offset - 1;
                    delete_char_at(&mut input_buffer, idx);
                    input_offset = idx;
                  }
                return Ok(popup!(true, input_buffer, input_offset, message));
              }
            KeyCode::Char(c) =>
              {
                // Le tarif manuel n'accepte que les chiffres, le point et la
                // virgule décimale ; le pays est un code ISO3 en majuscules.
                let accepte = match active_field
                  {
                    1 => c.is_ascii_digit() || c == '.' || c == ',',
                    2 => c.is_ascii_alphabetic(),
                    _ => false,
                  };
                if accepte
                  {
                    let pos = input_offset.min(input_buffer.chars().count());
                    let c2 = if active_field == 2
                      {
                        c.to_ascii_uppercase()
                      }
                    else
                      {
                        c
                      };
                    insert_char_at(&mut input_buffer, c2, pos);
                    input_offset = (pos + 1).min(input_buffer.chars().count());
                    message = None;
                  }
                return Ok(popup!(true, input_buffer, input_offset, message));
              }
            KeyCode::Enter => {}
            _ => return Ok(popup!(true, input_buffer, input_offset, message)),
          }
        // Validation (Entrée)
        match active_field
          {
            1 =>
              {
                // Le tarif saisi remplace la virgule décimale par un point.
                let brut = input_buffer.trim().replace(',', ".");
                match brut.parse::<f64>()
                  {
                    Ok(v) if registry::tarif_valide(v) =>
                      {
                        app.projects[project_idx].tarif_manual = v;
                        app.projects[project_idx].tarif_mode = "manuel".to_string();
                        // Le changement de tarif doit être persisté et la
                        // valorisation recalculée immédiatement. Le rescan
                        // peut re-trier la liste : on retrouve l'index du
                        // projet par son identifiant, qui est stable.
                        let id = app.projects[project_idx].id.clone();
                        let _ = rescan_repertoire(app, &mut Progress::noop());
                        if let Some(nouveau) = app.projects.iter().position(|p| p.id == id)
                          {
                            project_idx = nouveau;
                          }
                        message = Some(format!(
                          "Tarif manuel enregistré : {} /h",
                          format_currency(v, LOCAL_CURRENCY)
                        ));
                        return Ok(popup!(false, String::new(), 0, message));
                      }
                    // « 1e400 » donne `inf` : refusé, car il rendrait le
                    // registre illisible. Le message distingue le cas.
                    Ok(_) =>
                      {
                        message = Some(format!(
                          "« {input_buffer} » n'est pas un tarif utilisable : il faut un nombre fini et positif"
                        ));
                        return Ok(popup!(true, input_buffer, input_offset, message));
                      }
                    Err(_) =>
                      {
                        message =
                          Some(format!("Saisie invalide : « {input_buffer} »"));
                        return Ok(popup!(true, input_buffer, input_offset, message));
                      }
                  }
              }
            2 =>
              {
                let code = input_buffer.trim().to_uppercase();
                if code.len() == 3 && code.chars().all(|c| c.is_ascii_alphabetic())
                  {
                    app.web_cache.country = code.clone();
                    app.web_cache.last_failure_ms = 0;
                    // Le pays est stocké globalement : il est persisté avec
                    // le registre. La valorisation n'est recalculée qu'au
                    // rafraîchissement explicite (r), pour ne pas écraser un
                    // tarif manuel au simple changement de pays.
                    if let Err(e) = registry::save_registry_full(
                      &app.registry_path,
                      &app.projects,
                      &app.web_cache,
                      &app.prefs,
                    ) {
                      message = Some(format!("Registre non écrit : {e}"));
                    } else {
                      message = Some(format!("Pays source : {code} — r pour actualiser"));
                    }
                    return Ok(popup!(false, String::new(), 0, message));
                  }
                message = Some(format!(
                  "Code pays invalide : « {code} » (3 lettres, ex. FRA)"
                ));
                return Ok(popup!(true, input_buffer, input_offset, message));
              }
            _ => {}
          }
        return Ok(popup!(false, String::new(), 0, message));
      }

    // Navigation entre champs et actions.
    match key.code
      {
        KeyCode::Esc | KeyCode::Char('q') => return Ok(InputMode::Normal),
        KeyCode::Up | KeyCode::Char('k') =>
          {
            let next = if active_field == 0 { TARIF_CHAMPS - 1 } else { active_field - 1 };
            return Ok(InputMode::Tariff {
              project_idx,
              active_field: next,
              editing: false,
              input_buffer: String::new(),
              input_offset: 0,
              country,
              message,
              fetching: false,
            });
          }
        KeyCode::Down | KeyCode::Char('j') =>
          {
            let next = (active_field + 1) % TARIF_CHAMPS;
            return Ok(InputMode::Tariff {
              project_idx,
              active_field: next,
              editing: false,
              input_buffer: String::new(),
              input_offset: 0,
              country,
              message,
              fetching: false,
            });
          }
        KeyCode::Tab =>
          {
            let next = (active_field + 1) % TARIF_CHAMPS;
            return Ok(InputMode::Tariff {
              project_idx,
              active_field: next,
              editing: false,
              input_buffer: String::new(),
              input_offset: 0,
              country,
              message,
              fetching: false,
            });
          }
        KeyCode::Enter =>
          {
            // Champ 0 : bascule web / manuel. Champ 1 et 2 : ouverture de
            // l'éditeur, pré-rempli avec la valeur courante.
            if active_field == 0
              {
                let projet = &mut app.projects[project_idx];
                if projet.tarif_mode == "manuel"
                  {
                    projet.tarif_mode = "web".to_string();
                    message =
                      Some("Mode source web activé — r pour actualiser".to_string());
                  }
                else
                  {
                    if projet.tarif_manual <= 0.0
                      {
                        message =
                          Some("Saisissez d'abord un tarif manuel (champ 2)".to_string());
                        return Ok(popup!(false, String::new(), 0, message));
                      }
                    projet.tarif_mode = "manuel".to_string();
                    message = Some("Mode manuel activé".to_string());
                  }
                let id = app.projects[project_idx].id.clone();
                let _ = rescan_repertoire(app, &mut Progress::noop());
                if let Some(nouveau) = app.projects.iter().position(|p| p.id == id)
                  {
                    project_idx = nouveau;
                  }
                return Ok(popup!(false, String::new(), 0, message));
              }
            let buffer = match active_field
              {
                1 => app.projects[project_idx]
                  .tarif_manual
                  .to_string()
                  .replace('.', ","),
                2 => app.web_cache.country.clone(),
                _ => String::new(),
              };
            let offset = buffer.chars().count();
            return Ok(popup!(true, buffer, offset, message));
          }
        KeyCode::Char('r') | KeyCode::Char('R') =>
          {
            // Actualisation forcée de la source, pour le pays courant.
            let pays = app.web_cache.country.clone();
            let maintenant = registry::maintenant_ms();
            let change = app.web_cache.rafraichir(&pays, maintenant, true);
            if !app.web_cache.last_error.is_empty()
              {
                let err = app.web_cache.last_error.clone();
                message = Some(err);
              }
            else if change
              {
                let annee = app
                  .web_cache
                  .rates
                  .get(&pays)
                  .and_then(|s| s.last())
                  .map(|p| p.annee)
                  .unwrap_or(0);
                let maj = registry::format_mois_annee(maintenant);
                message = Some(format!(
                  "Source actualisée ({pays}, {annee}) le {maj}"
                ));
              }
            else
              {
                message = Some(format!("Source déjà à jour ({pays})"));
              }
            // Recalcul immédiat des durées valorisées.
            let _ = rescan_repertoire(app, &mut Progress::noop());
            return Ok(popup!(false, String::new(), 0, message));
          }
        _ => return Ok(popup!(false, String::new(), 0, message)),
      }
   }

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
                    registry::save_registry_full(&app.registry_path, &app.projects, &app.web_cache, &app.prefs)?;
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
                    registry::save_registry_full(&app.registry_path, &app.projects, &app.web_cache, &app.prefs)?;
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
    use crate::{cout_affiche, normaliser_recherche, sanitize, startup, variant_label, Progress};
    use std::{fs, path::PathBuf};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("op-start-menu-main-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cout_nul_avec_des_tokens_vaut_pas_zero() {
        // opencode écrit cost = 0 quand le fournisseur ne publie pas de tarif.
        // Afficher « 0,00 € » ferait croire à une session gratuite.
        assert_eq!(cout_affiche(0.0, 467_383, "EUR"), "—");
        // Session réellement sans tokens : le montant reste affichable.
        assert_ne!(cout_affiche(0.0, 0, "EUR"), "—");
        // Un coût connu reste prioritaire sur le cas « tokens > 0 ».
        assert_ne!(cout_affiche(12.5, 1000, "EUR"), "—");
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

    /// « é » précomposé (NFC, U+00E9) contre « e » + accent combinant (NFD,
    /// U+0065 U+0301) : deux chaînes visuellement identiques, différentes pour
    /// `contains`. Les chemins du registre et la saisie clavier ne partagent
    /// pas forcément la même forme, d'où ce test.
    #[test]
    fn la_normalisation_reconcilie_les_deux_formes_unicode() {
        let compose: String = "\u{e9}".to_string();
        let decompose: String = "e\u{301}".to_string();
        assert_ne!(compose, decompose, "le test suppose deux formes distinctes");
        assert_eq!(normaliser_recherche(&compose), normaliser_recherche(&decompose));
    }

    #[test]
    fn la_normalisation_ne_depend_pas_de_la_casse() {
        assert_eq!(normaliser_recherche("Proc\u{e9}dure"), normaliser_recherche("proc\u{e9}dure"));
        assert_eq!(normaliser_recherche("PROC\u{c9}DURE"), "proc\u{e9}dure");
    }

    #[test]
    fn la_normalisation_laisse_intact_un_texte_simple() {
        assert_eq!(normaliser_recherche("opencode-start-menu"), "opencode-start-menu");
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
            last_session_context_pct: String::new(),
            status_debug: String::new(),
            version_debug: String::new(),
            status_release: String::new(),
            version_release: String::new(),
            debug_modified: String::new(),
            release_modified: String::new(),
            last_compile_debug_secs: 0,
            last_compile_release_secs: 0,
            version_manually_edited: false,
            source_hash: String::new(),
            tarif_mode: "web".to_string(),
            tarif_manual: 0.0,
            tarif_web_series: Vec::new(),
            tarif_web_fetched_ms: 0,
            tarif_web_secteur: false,
            tarif_web_pays: String::new(),
            last_duree: String::new(),
            total_duree: String::new(),
            last_valorisation: String::new(),
            total_valorisation: String::new(),
            tarif_resume: String::new(),
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
    fn autopatch_ne_rebump_pas_a_l_chaque_lancement()
      {
        let dir = temp_dir("autopatch");
        let projet = dir.join("projet-factice");
        fs::create_dir_all(projet.join("src")).unwrap();
        let manifeste = |v: &str| {
            format!("[package]\nname = \"projet-factice\"\nversion = \"{v}\"\n\n[dependencies]\n")
          };
        fs::write(projet.join("Cargo.toml"), manifeste("0.1.0")).unwrap();
        fs::write(projet.join("src/main.rs"), "fn main() {}\n").unwrap();

        // Registre décrivant l'état AVANT la modification de code : source_hash
        // correspond au code d'origine, version 0.1.0.
        let hash_initial = super::registry::compute_source_hash(&projet);
        let registre = dir.join("projects.json");
        let ecrire_registre = |source_hash: &str| {
            let json = serde_json::json!({
                "projects": {
                    "projet-factice": {
                        "name": "projet-factice",
                        "path": projet.to_str().unwrap(),
                        "version": "0.1.0",
                        "version_debug": "0.1.0",
                        "source_hash": source_hash,
                        // Mode manuel : aucun appel réseau pendant le rescan.
                        "tarif_mode": "manuel",
                        "description": ""
                    }
                }
            });
            fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
          };
        ecrire_registre(&hash_initial);

        // Modification de code : c'est elle qui doit déclencher l'unique bump.
        fs::write(projet.join("src/main.rs"), "fn main() { println!(\"v2\"); }\n").unwrap();

        // Lancement n°1 : bump attendu, puis hash resynchronisé sur le
        // manifeste réellement écrit.
        let app = super::App::new(registre.clone(), super::SortOrder::Modified, &mut Progress::noop());
        let apres_run1 = app
          .projects
          .iter()
          .find(|p| p.path == projet)
          .expect("projet présent après le premier rescan")
          .clone();
        assert_eq!(apres_run1.version_debug, "0.1.1");
        let manifest = fs::read_to_string(projet.join("Cargo.toml")).unwrap();
        assert!(manifest.contains("version = \"0.1.1\""), "manifeste : {manifest}");
        // L'invariant central : le hash stocké décrit le manifeste ÉCRIT.
        assert_eq!(
          apres_run1.source_hash,
          super::registry::compute_source_hash(&projet),
          "le hash stocké doit décrire le manifeste post-bump, sinon le rescan \
           suivant détecte une fausse modification"
        );

        // Lancement n°2 et n°3 : plus aucun bump, plus aucune écriture.
        for n in 2..=3 {
            let app = super::App::new(registre.clone(), super::SortOrder::Modified, &mut Progress::noop());
            let e = app
              .projects
              .iter()
              .find(|p| p.path == projet)
              .expect("projet présent au rescan suivant");
            assert_eq!(e.version_debug, "0.1.1", "bump intempestif au lancement n°{n}");
            let manifest = fs::read_to_string(projet.join("Cargo.toml")).unwrap();
            assert!(manifest.contains("version = \"0.1.1\""), "lancement n°{n} : {manifest}");
          }
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

    #[test]
    fn bump_manifest_version_respecte_workspace_et_apostrophes()
      {
        let dir = temp_dir("ws");
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"a\"\nversion.workspace = true\n").unwrap();
        assert_eq!(super::bump_manifest_version(&dir, "1.0.1").unwrap(), false);
        let c = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert_eq!(c.matches("version").count(), 1);
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"a\"\nversion = '1.0.0' # note\n").unwrap();
        assert_eq!(super::bump_manifest_version(&dir, "1.0.1").unwrap(), true);
        let c = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(c.contains("version = \"1.0.1\" # note"), "{c}");
        fs::remove_dir_all(&dir).unwrap();
      }

    #[test]
    fn format_millions_une_decimale_et_zero_final()
      {
        // La locale machine (fr_FR) produit une virgule décimale.
        let sep = if super::is_french_locale() { "," } else { "." };
        assert_eq!(super::format_millions(874_009), format!("0{sep}9"));
        assert_eq!(super::format_millions(41_703_416), format!("41{sep}7"));
        assert_eq!(super::format_millions(40_000_000), "40");
        assert_eq!(super::format_millions(0), "0");
      }

    /// Backend qui délègue tout à un `TestBackend` sauf `draw`, toujours en
    /// erreur. Sert à vérifier que la jauge rend la main quand l'affichage
    /// échoue, au lieu de propager l'erreur hors de la boucle.
    struct BackendEnPanne(ratatui::backend::TestBackend);

    impl ratatui::backend::Backend for BackendEnPanne
      {
        fn draw<'a, I>(&mut self, _content: I) -> std::io::Result<()>
          where
            I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>
          {
            Err(std::io::Error::other("panne du terminal simulee"))
          }
        fn hide_cursor(&mut self) -> std::io::Result<()> { self.0.hide_cursor() }
        fn show_cursor(&mut self) -> std::io::Result<()> { self.0.show_cursor() }
        fn get_cursor_position(&mut self) -> std::io::Result<ratatui::prelude::Position>
          { self.0.get_cursor_position() }
        fn set_cursor_position<P: Into<ratatui::prelude::Position>>(&mut self, p: P) -> std::io::Result<()>
          { self.0.set_cursor_position(p) }
        fn clear(&mut self) -> std::io::Result<()> { self.0.clear() }
        fn size(&self) -> std::io::Result<ratatui::prelude::Size> { self.0.size() }
        fn window_size(&mut self) -> std::io::Result<ratatui::backend::WindowSize>
          { self.0.window_size() }
        fn flush(&mut self) -> std::io::Result<()> { self.0.flush() }
      }

    /// Régression P1-1 : une erreur d'affichage pendant l'initialisation
    /// remontait par `?` hors de `run_startup_gauge`, avant `close_terminal`.
    /// Le shell se retrouvait en mode brut dans l'écran alternatif, sans rien
    /// afficher, jusqu'à un `reset` manuel.
    ///
    /// La fonction rend désormais la main avec l'erreur stockée, ce qui permet
    /// à l'appelant de toujours restaurer le terminal.
    #[test]
    fn animer_jauge_rend_la_main_meme_si_l_affichage_echoue()
      {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<startup::Signal>();
        // Le sender reste vivant : l'annulation ne peut pas venir du canal,
        // seule l'erreur d'affichage peut terminer la boucle.
        let backend = BackendEnPanne(ratatui::backend::TestBackend::new(20, 5));
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = startup::GaugeState::new();
        let mut avertissements: Vec<String> = Vec::new();

        let issue = super::animer_jauge(&mut terminal, &mut state, &rx, &mut avertissements);

        let issue = issue.expect("la boucle doit se terminer et rendre une issue");
        // `App` n'implémente pas `Debug`, donc pas `expect_err` : on déroule
        // le Result à la main.
        let Err(err) = issue else {
            panic!("l'erreur du terminal doit etre propagee, pas absorbee");
        };
        assert!(
            err.to_string().contains("panne du terminal simulee"),
            "erreur inattendue : {err}"
        );
        drop(tx);
      }

    /// Sans terminal réel, `event::poll` échoue (« Failed to initialize input
    /// reader »). Ce test vérifie deux choses qui comptent pour le correctif :
    /// la boucle rend toujours la main au lieu de boucler ou de propager, et
    /// l'échec est signalé plutôt que silencieusement absorbé.
    #[test]
    fn animer_jauge_rend_la_main_meme_sans_terminal_reel()
      {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel::<startup::Signal>();
        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let mut state = startup::GaugeState::new();
        let mut avertissements: Vec<String> = Vec::new();

        let issue = super::animer_jauge(&mut terminal, &mut state, &rx, &mut avertissements)
          .expect("la boucle doit rendre la main, jamais boucler indefiniment");
        // Ici aucune touche n'a pu être lue : l'issue est forcément une erreur,
        // ce qui prouve que le `break` sur échec de `poll` est atteint.
        let Err(err) = issue else {
            panic!("sans terminal, la lecture d'evenement doit echouer");
        };
        assert!(!err.to_string().is_empty(), "l'erreur doit etre decrite");
        drop(tx);
      }
  }
