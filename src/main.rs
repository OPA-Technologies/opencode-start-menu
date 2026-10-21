// La propriété « zéro warning » est vérifiée par les builds eux-mêmes : deux
// affectations mortes et un popup mal réaligné ont dormi ici parce que rien ne
// les signalait. `clippy::all` reste hors du déni, son contenu évolue avec les
// versions de clippy : ce sont les diagnostics de rustc qui bloquent, pas ceux
// d'un outil annexe.
#![deny(warnings)]

use anyhow::Result;
use chrono::TimeZone;
use clap::{Parser, ValueEnum};
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

mod apport;
mod coherence;
mod compta;
mod conformite;
mod constat;
mod keymap;
mod launch;
mod lock;
mod modules;
mod perms;
mod registry;
mod startup;
mod ui;
mod variante;

use crate::launch::{run_launch_sequence, session_existante, LaunchKind};
use crate::registry::ProjectEntry;
use crate::startup::{Progress, Step};

/// Contrat `-V` du menu lui-même. Op est un projet comme un autre : son binaire
/// publie sa propre vérité, figée à la compilation (`<version>_<statut>
/// (<jj-mm-yyyy>)`), lue depuis ces trois constantes — jamais déduite du
/// registre. `RELEASE` suit `CARGO_PKG_VERSION` ; `STATUT` et `DATE_LAST_MAJ`
/// se tiennent comme ceux de tout projet.
const RELEASE: &str = env!("CARGO_PKG_VERSION");
const STATUT: &str = "Release";
const DATE_LAST_MAJ: &str = "20-10-2026";

#[derive(Parser, Debug)]
#[command(
    name = "op",
    version,
    about = "OpenCode Start Menu - Interactive project launcher",
    disable_help_flag = true,
    disable_version_flag = true
)]
struct Cli {
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

    #[arg(long = "new", value_name = "NOM", num_args = 0..=1, default_missing_value = "",
          help = "Ouvrir le formulaire de création (équivalent de « n » dans le menu)")]
    new: Option<String>,

    #[arg(short = 'h', long = "help", help = "Show help")]
    help: bool,

    #[arg(help = "Project keyword (id, name, or path)")]
    keyword: Option<String>,
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
    /// Vue courante : la liste normale ou celle des projets archivés.
    ///
    /// L'archivage déplace le dossier dans [`App::archive_root`] ; c'est ce
    /// chemin, et non un champ du registre, qui dit qu'un projet y est.
    pub(crate) vue_archives: bool,
    /// Répertoire des archives, hors de `~/projets` : les projets déplacés
    /// dedans ne doivent plus être réindexés à chaque rescan.
    pub(crate) archive_root: PathBuf,
    /// Répertoire des projets actifs, mémorisé pour savoir où ramener un
    /// projet archivé quand plus aucun actif ne reste pour le dire.
    pub(crate) projets_root: PathBuf,
    mode: LaunchMode,
    pub(crate) status_messages: Vec<String>,
    registry_path: PathBuf,
    opencode_db_path: PathBuf,
    sort_order: SortOrder,
    pending_launch: Option<PathBuf>,
    pending_sudo: bool,
    pending_name: Option<String>,
    pending_bare: bool,
    /// Index du projet dont un bump de version manuel attend la `o` de
    /// confirmation. Mémorisé entre deux frappes seulement : toute autre
    /// touche l'annule, ce qui interdit tout décalage d'index.
    bump_en_attente: Option<usize>,
    /// Cache des séries de tarifs récupérées de la source web, partagé par
    /// tous les projets (évite un téléchargement par projet).
    pub(crate) web_cache: registry::WebCache,
    /// Préférences globales (onglet Paramètres), en mémoire pour que chaque
    /// bascule s'applique sans recharger le registre.
    pub(crate) prefs: registry::Preferences,
    /// Registre présent mais illisible : aucune réindexation ni réécriture
    /// automatique tant qu'il n'est pas réparé.
    registry_broken: bool,
    /// Refus de cohérence par projet (id → refus), pour re-notification.
    pub(crate) refus: std::collections::HashMap<String, Vec<registry::RefusCoherence>>,
    /// Diagnostics de cohérence courants, recalculés à chaque contrôle
    /// (lancement et retour au menu).
    pub(crate) diagnostics: Vec<DiagnosticProjet>,
}

/// Diagnostic de cohérence d'un projet, accompagné de son identité.
pub(crate) struct DiagnosticProjet {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) diagnostic: coherence::Diagnostic,
}

#[derive(PartialEq, Clone, Copy)]
enum LaunchMode {
    Normal,
    Root,
}

/// Onglet du popup unifié.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum Onglet {
    /// Réglages du projet sélectionné (version, statut, description).
    #[default]
    Projet,
    /// Valorisation et coûts, avec la synthèse des deux.
    Couts,
    /// Préférences générales, hors projet.
    Parametres,
}

impl Onglet {
    /// Onglet suivant, cyclique.
    pub(crate) fn suivant(self) -> Self {
        match self {
            Onglet::Projet => Onglet::Couts,
            Onglet::Couts => Onglet::Parametres,
            Onglet::Parametres => Onglet::Projet,
        }
    }

    /// Onglet précédent, cyclique.
    pub(crate) fn precedent(self) -> Self {
        match self {
            Onglet::Projet => Onglet::Parametres,
            Onglet::Couts => Onglet::Projet,
            Onglet::Parametres => Onglet::Couts,
        }
    }

    pub(crate) fn libelle(self) -> &'static str {
        match self {
            Onglet::Projet => "Projet",
            Onglet::Couts => "Coûts",
            Onglet::Parametres => "Paramètres",
        }
    }
}

/// État de l'onglet Coûts : réglage du tarif et défilement des tableaux.
///
/// Les deux cohabitent pour qu'un aller-retour entre la saisie d'un tarif et
/// la lecture du tableau ne perde ni l'un ni l'autre.
#[derive(Clone, Default)]
pub(crate) struct EtatCouts {
    /// Champ tarifaire actif : 0 = mode, 1 = tarif manuel (€/h), 2 = pays.
    pub(crate) active_field: usize,
    pub(crate) editing: bool,
    pub(crate) input_buffer: String,
    pub(crate) input_offset: usize,
    pub(crate) country: String,
    pub(crate) message: Option<String>,
    pub(crate) fetching: bool,
    /// Défilement du tableau des coûts par session.
    pub(crate) scroll_offset: usize,
}

/// État de l'onglet Paramètres : une option active, éventuellement en saisie.
#[derive(Clone, Default)]
pub(crate) struct EtatParametres {
    pub(crate) actif: usize,
    /// Saisie en cours, réservée au code pays.
    pub(crate) edition: bool,
    pub(crate) tampon: String,
    pub(crate) curseur: usize,
    /// Colonne visée dans l'option « Colonnes visibles ».
    pub(crate) colonne_focus: usize,
    pub(crate) message: Option<String>,
}

/// Le popup unifié. Chaque onglet garde son état : saisir un tarif, aller sur
/// Projet, puis revenir restitue la saisie intacte.
#[derive(Clone)]
pub(crate) struct Popup {
    pub(crate) onglet: Onglet,
    pub(crate) projet_idx: usize,
    pub(crate) projet: ProjectEditState,
    pub(crate) couts: EtatCouts,
    pub(crate) parametres: EtatParametres,
}

impl Popup {
    /// Popup sans projet sélectionné.
    ///
    /// Les préférences sont globales : `s` doit rester accessible même quand la
    /// liste est vide. Les onglets Projet et Coûts indicate alors qu'il n'y a
    /// rien à afficher plutôt que de bloquer l'accès aux réglages.
    pub(crate) fn sans_projet(onglet: Onglet, country: String) -> Self {
        Self {
            onglet,
            projet_idx: 0,
            projet: ProjectEditState::default(),
            couts: EtatCouts { country, ..EtatCouts::default() },
            parametres: EtatParametres::default(),
        }
    }

    pub(crate) fn nouveau(projet_idx: usize, projet: ProjectEditState, country: String) -> Self {
        Self {
            onglet: Onglet::Projet,
            projet_idx,
            projet,
            couts: EtatCouts { country, ..EtatCouts::default() },
            parametres: EtatParametres::default(),
        }
    }

    /// Même popup, onglet Projet remplacé.
    ///
    /// Les handlers d'édition ne connaissent que leur propre état ; ils
    /// rendent donc le popup qu'on leur a fourni, avec leur état dedans. Sans
    /// cela, quitter l'onglet Projet pour aller voir les coûts puis revenir
    /// réinitialiserait la saisie en cours.
    pub(crate) fn avec_projet(&self, projet: ProjectEditState) -> Popup {
        Popup { projet, ..self.clone() }
    }

    /// Même popup, onglet Coûts remplacé.
    pub(crate) fn avec_couts(&self, couts: EtatCouts) -> Popup {
        Popup { couts, ..self.clone() }
    }

    /// Même popup, onglet affiché remplacé.
    pub(crate) fn avec_onglet(&self, onglet: Onglet) -> Popup {
        Popup { onglet, ..self.clone() }
    }

    /// Même popup, projet affiché remplacé.
    ///
    /// `projet_idx` est un index, pas une référence : tout re-tri ou rescan de
    /// `app.projects` le rend caduc. Les handlers qui déclenchent un rescan
    /// doivent donc réaligner le popup via ce builder — sinon le titre et
    /// l'onglet Coûts décrivent un autre projet que l'onglet Projet.
    pub(crate) fn avec_projet_idx(&self, projet_idx: usize) -> Popup {
        Popup { projet_idx, ..self.clone() }
    }
}

/// Réaligne `popup.projet_idx` sur le projet d'identifiant `id`.
///
/// `projet_idx` est un index, pas une référence. L'appelant lit l'identifiant
/// correspondant **avant** de re-trier `app.projects`, puis cette fonction rend
/// au popup l'index qu'occupe le projet identifié après le re-tri. L'ordre des
/// deux opérations est essentiel : lire l'identifiant après le tri relirait un
/// index déjà caduc, et la correction serait sans effet.
///
/// Un identifiant absent — projet supprimé, ou liste vidée — laisse l'index
/// inchangé : l'affichage rend alors le projet absent plutôt qu'un autre projet
/// au hasard.
fn popup_aligne(app: &App, popup: &Popup, id: Option<&str>) -> Popup {
  match id.and_then(|id| app.projects.iter().position(|p| p.id == id)) {
    Some(nouveau) => popup.avec_projet_idx(nouveau),
    None => popup.clone(),
  }
}

/// Identifiant du projet que désigne `projet_idx`.
///
/// À appeler avant tout re-tri ou rescan : c'est la seule valeur stable d'un
/// index.
fn id_a_l_index(app: &App, projet_idx: usize) -> Option<String> {
  app.projects.get(projet_idx).map(|p| p.id.clone())
}

#[derive(Clone)]
pub(crate) enum InputMode {
    Normal,
    /// Choix du type de la nouvelle entrée : « Projet » ou « Bibliothèque ».
    ChoixNouveau {
        selected: usize,
    },
    CreatingProject(ProjectCreationState),
    SelectModule {
        project_idx: usize,
        modules: Vec<PathBuf>,
        selected: usize,
    },
    /// Popup unique regroupant le projet, les coûts et les préférences.
    /// Une seule touche y mène depuis la liste : `p`, sur Paramètres, et
    /// `Tab`/`⇧Tab` desservent les trois onglets.
    Popup(Popup),
    /// Examen des incohérences détectées : proposer une action par écart,
    /// laisser l'utilisateur l'appliquer ou la refuser.
    Coherence(CoherenceState),
    /// Aide contextuelle (F1) ou liste complète (F10).
    ///
    Aide {
        detaillee: bool,
        /// Mode d'où l'aide a été ouverte, conservé pour y revenir.
        ///
        /// Sans cette copie, `F1` en plein milieu d'une saisie perduait le
        /// formulaire : l'aide serait devenue destructive.
        origine: Box<InputMode>,
        /// Ligne du haut de la liste affichée. `F10` tient rarement en un
        /// écran : sans défilement, la fin de la liste serait inaccessible.
        scroll: usize,
    },
}

/// État de la modale d'examen des incohérences.
///
/// La liste affichée est plate : chaque ligne est un écart d'un projet. Les
/// refus déjà enregistrés restent visibles, marqués, pour être re-notifiés.
#[derive(Clone, Default)]
pub(crate) struct CoherenceState {
    /// Index de la ligne (écart) sélectionnée dans la liste plate.
    pub(crate) selection: usize,
    /// Une action Release attend sa confirmation dédiée : index de la ligne.
    pub(crate) confirmation_release: Option<usize>,
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
    /// Le champ vient d'être ouvert : `Suppr` le vide d'un coup, et la
    /// Version repart de zéro à la première frappe.
    pub(crate) premiere_touche: bool,
    pub(crate) known_statuses: Vec<String>,
    pub(crate) status_menu_open: bool,
    pub(crate) status_filter: String,
    pub(crate) status_cursor: usize,
    // Saisie libre d'un nouveau statut (entrée « + Nouveau statut » du menu).
    pub(crate) status_free_text: bool,
    /// La création porte une bibliothèque (« Nouvelle bibliothèque ») : le
    /// dossier par défaut tombe sous la racine des bibliothèques.
    pub(crate) bibliotheque: bool,
}

/// Commandes d'outils éditables dans l'onglet Projet : sonde `-V` et commande
/// de build, par variante. Vide = auto (sonde `target/<profil>`, build Cargo).
///
/// Regroupées et boxées pour ne pas alourdir `InputMode` (variante `Popup`) :
/// quatre `String` en clair feraient franchir le seuil de `large_enum_variant`.
#[derive(Clone, Default)]
pub(crate) struct OutilsEdit {
    pub(crate) probe_debug: String,
    pub(crate) build_debug: String,
    pub(crate) probe_release: String,
    pub(crate) build_release: String,
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
    /// Commandes de sonde (`-V`) et de build par variante, éditées dans
    /// l'onglet Projet. Vide = auto (sonde `target/<profil>`, build Cargo).
    pub(crate) outils: Box<OutilsEdit>,
    pub(crate) error: Option<String>,
    /// Le champ vient d'être ouvert : `Suppr` le vide d'un coup, et la
    /// Version repart de zéro à la première frappe.
    pub(crate) premiere_touche: bool,
    /// Choix surligné sur « Enregistrer ? » : `true` = `[Oui]`, `false` =
    /// `[Non]`. Le défaut est `[Non]` : la frappe accidentelle d'Entrée ne
    /// doit pas enregistrer.
    pub(crate) choix_oui: bool,
    pub(crate) status_menu_open: bool,
    pub(crate) status_filter: String,
    pub(crate) status_cursor: usize,
    /// Typologie du projet, choisie dans un popup dédié (onglet Projet).
    pub(crate) pending_typologie: String,
    pub(crate) typologie_menu_open: bool,
    pub(crate) typologie_cursor: usize,
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
            outils: Box::new(OutilsEdit::default()),
            error: None,
            premiere_touche: true,
            choix_oui: false,
            status_menu_open: false,
            status_filter: String::new(),
            status_cursor: 0,
            pending_typologie: registry::TYPOLOGIE_DEFAUT.to_string(),
            typologie_menu_open: false,
            typologie_cursor: 0,
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
            outils: Box::new(OutilsEdit {
                probe_debug: project.probe_debug.clone(),
                build_debug: project.build_debug.clone(),
                probe_release: project.probe_release.clone(),
                build_release: project.build_release.clone(),
            }),
            error: None,
            premiere_touche: true,
            choix_oui: false,
            status_menu_open: false,
            status_filter: String::new(),
            status_cursor: 0,
            pending_typologie: if registry::validate_typologie(&project.typologie) {
                project.typologie.clone()
            } else {
                registry::TYPOLOGIE_DEFAUT.to_string()
            },
            typologie_menu_open: false,
            typologie_cursor: 0,
          }
      }
  }

// Libellé de variante : version + statut, séparés par « _ ».
// La partie « pré-release » d'une version (ex. 0.9.9-beta) est écartée.
#[cfg(test)]
fn variant_label(version: &str, status: &str) -> String {
    let core = version.split('-').next().unwrap_or(version);
    let stat = status.replace(['-', ' '], "_");
    if stat.is_empty() {
        core.to_string()
    } else {
        format!("{core}_{stat}")
    }
}

/// Affichage bifilaire d'une variante : la vérité binaire (sonde `-V` ou
/// fiche de build) d'abord, la base du registre en complément quand elles
/// diffèrent.
///
/// Format : `0.50.4_Stable (15-10-2026)` pour le binaire ; si la base
/// porte une autre version ou un autre statut, ` · base 0.50.5_Terminée`
/// est ajouté. Sans binaire lisible, la base seule (« — » si elle est
/// vide) : le vide muet n'existe pas dans cet affichage.
pub(crate) fn libelle_bifilaire(
  binaire: Option<(&str, &str, &str)>,
  base: (&str, &str),
  ) -> String
  {
    let base_libelle = |(v, s): (&str, &str)| -> String
      {
        if v.is_empty() && s.is_empty()
          {
            "—".to_string()
          }
        else if s.is_empty()
          {
            v.to_string()
          }
        else
          {
            format!("{v}_{s}")
          }
      };
    let Some((bv, bs, bd)) = binaire.filter(|(v, _, _)| !v.is_empty()) else
      {
        return base_libelle(base);
      };
    let mut out = if bs.is_empty() { bv.to_string() } else { format!("{bv}_{bs}") };
    if !bd.is_empty()
      {
        out.push_str(&format!(" ({bd})"));
      }
    if base.0 != bv || (!base.1.is_empty() && base.1 != bs)
      {
        out.push_str(&format!(" · base {}", base_libelle(base)));
      }
    out
  }

/// Fiche de build à afficher (lecture seule), sans sonde ni lecture disque.
///
/// La fiche ne fait foi que si elle colle à la référence du dernier build
/// détecté (`last_compile_*_secs`, rafraîchie au chargement et par le
/// rattrapage) : une fiche périmée ne doit pas faire passer un vieux binaire
/// pour neuf. Aucun accès disque ici, car ce chemin est appelé à chaque image
/// par le rendu TUI.
pub(crate) fn fiche_pour_affichage(
  p: &registry::ProjectEntry,
  profil: &str,
  ) -> Option<(String, String, String)>
  {
    let (fiche, reference) = if profil == "release"
      {
        (p.release_snapshot.as_ref(), p.last_compile_release_secs)
      }
    else
      {
        (p.debug_snapshot.as_ref(), p.last_compile_debug_secs)
      };
    let fiche = fiche?;
    if reference == 0 || reference != fiche.compile_secs
      {
        return None;
      }
    Some((fiche.version.clone(), fiche.statut.clone(), fiche.date.clone()))
  }

/// Vérité binaire à afficher.
///
/// En ligne de commande (`sonder == true`) : lecture directe du binaire
/// (`-V`) à chaque appel. Le registre ne sert qu'en complément d'affichage
/// si les deux divergent.
///
/// En TUI : on n'affiche le snapshot que s'il décrit encore le binaire
/// présent ; sinon « — » plutôt qu'une fiche périmée.
pub(crate) fn affichage_variante_binaire(
  p: &registry::ProjectEntry,
  profil: &str,
  sonder: bool,
) -> Option<(String, String, String)> {
    if sonder {
        let sonde_cmd = p.sonde(profil);
        if let Some(lv) = variante::lire_variante(&p.path, profil, sonde_cmd) && !lv.etat.version.is_empty() {
            return Some((lv.etat.version, lv.etat.statut, lv.etat.date));
        }
        // Sonde demandée mais pas de lecture contractuelle : rien d'affiché
        // plutôt que de deviner depuis un cache potentiellement faux.
        return None;
    }
    fiche_pour_affichage(p, profil)
}

// --- Édition de texte : les gestes standards viennent de `op-tui::edit`,
// --- l'application ne fait que traduire les touches du terminal.

/// Traduit une frappe en geste d'édition standard, ou `None` quand la touche
/// n'appartient pas à un champ (Entrée, Échap, `↑`/`↓`, raccourcis) : le
/// gestionnaire garde alors sa sémantique propre.
fn touche_edition(key: event::KeyEvent) -> Option<op_tui::edit::Touche>
  {
    use op_tui::edit::Touche;
    let controle = key.modifiers.contains(event::KeyModifiers::CONTROL);
    let alternate = key.modifiers.contains(event::KeyModifiers::ALT);
    match key.code
      {
        KeyCode::Left if controle => Some(Touche::MotGauche),
        KeyCode::Right if controle => Some(Touche::MotDroite),
        KeyCode::Left => Some(Touche::Gauche),
        KeyCode::Right => Some(Touche::Droite),
        KeyCode::Home => Some(Touche::Debut),
        KeyCode::End => Some(Touche::Fin),
        KeyCode::PageUp => Some(Touche::PageHaut),
        KeyCode::PageDown => Some(Touche::PageBas),
        KeyCode::Backspace if !controle => Some(Touche::Retour),
        KeyCode::Delete if !controle => Some(Touche::Supprimer),
        KeyCode::Char(c) if !controle && !alternate => Some(Touche::Caractere(c)),
        _ => None,
      }
  }

/// Applique un geste d'édition au couple `(tampon, curseur)` du champ courant.
///
/// Deux particularités tiennent à la façon dont le champ a été ouvert :
/// - `Suppr`, en toute première touche, vide le champ d'un coup : c'est le
///   remplacement de la valeur affichée, à la place d'une sélection ;
/// - sur un champ à masque — la Version, `x.y.z` — la première frappe part
///   de zéro, pour que la saisie guidée reste lisible.
///
/// Les saisies déjà ouvertes sur une valeur (tarif, code pays) passent
/// `premiere_touche = false` : elles n'ont rien à remplacer.
fn appliquer_edition(
  tampon: &mut String,
  curseur: &mut usize,
  premiere_touche: bool,
  touche: op_tui::edit::Touche,
  masque: bool,
  )
  {
    use op_tui::edit::Touche;
    if premiere_touche
      {
        if touche == Touche::Supprimer
          {
            tampon.clear();
            *curseur = 0;
            return;
          }
        if masque && matches!(touche, Touche::Caractere(_))
          {
            tampon.clear();
            *curseur = 0;
          }
      }
    match touche
      {
        Touche::Caractere(c) if masque =>
          {
            let resultat = version_mask_insert(tampon.as_str(), *curseur, c);
            if let Some((s, off)) = resultat
              {
                *tampon = s;
                *curseur = off;
              }
          }
        autre =>
          {
            op_tui::edit::gerer(tampon, curseur, autre);
          }
      }
  }

/// Saisie par masque de la version (x.y.z) : chiffres uniquement, point
/// autorisé si le segment courant n'est pas vide, point inséré
/// automatiquement quand un segment atteint 3 chiffres et qu'un chiffre
/// supplémentaire est frappé. Renvoie None si le caractère est rejeté.
fn version_mask_insert(buffer: &str, char_idx: usize, c: char) -> Option<(String, usize)>
  {
    if c.is_ascii_digit()
      {
        let byte = op_tui::field::octet_pour_caractere(buffer, char_idx);
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
        let byte = op_tui::field::octet_pour_caractere(buffer, char_idx);
        let prev_byte = op_tui::field::octet_pour_caractere(buffer, char_idx - 1);
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

/// Typologies proposées au choix dans l'onglet Projet (liste fermée).
pub(crate) fn typologies_liste() -> Vec<&'static str>
  {
    registry::TYPOLOGIES.to_vec()
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

/// Synchronise la base de l'entrée menu (opencode-start-menu) sur la version
/// du binaire en cours (`CARGO_PKG_VERSION`), si elle est plus récente
/// (semver), ou si le registre est vide. Ne JAMAIS écraser un bump AutoPatch.
///
/// Rattrapage de base en lecture seule : le manifeste n'est PAS réécrit ici —
/// la seule écriture de source est l'édition manuelle du popup. Le statut
/// n'est pas écrit non plus : c'était un artefact (« Beta-Release » codé en
/// dur) qui falsifiait la variante Release.
fn sync_menu_project_version(app: &mut App) -> Result<()>
  {
    let expected_version = env!("CARGO_PKG_VERSION");
    if let Some(p) = app.projects.iter_mut().find(|p| p.name == "OpenCode Start Menu")
      {
        let registry_version = &p.version_debug;
        let should_sync = registry_version.is_empty()
            || version_greater(expected_version, registry_version);
        if should_sync {
            registry::accepter_modification(format!("{}:version_debug", p.id));
            registry::accepter_modification(format!("{}:version", p.id));
            p.version_debug = expected_version.to_string();
            p.version = p.version_debug.clone();
            // Référence de compilation du menu : le mtime de target/debug, et
            // le snapshot correspondant — le binaire courant fait foi.
            if let Some(t) = registry::variant_modified_time(&p.path, "debug")
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()))
            {
                registry::accepter_modification(format!("{}:last_compile_debug_secs", p.id));
                registry::accepter_modification(format!("{}:debug_snapshot", p.id));
                p.last_compile_debug_secs = t;
                // La date et l'empreinte viennent du binaire lui-même ; à
                // défaut de sonde, les constantes compilées du menu font foi.
                let (date, empreinte_v) =
                    match variante::lire_variante_avec_sonde(&p.path, "debug", p.sonde("debug"))
                  {
                    Some(l) => (l.etat.date, l.empreinte),
                    None => (
                        DATE_LAST_MAJ.to_string(),
                        variante::empreinte_v(&format!("{RELEASE}_{STATUT} ({DATE_LAST_MAJ})")),
                    ),
                  };
                p.debug_snapshot = Some(registry::SnapshotCompile {
                    version: expected_version.to_string(),
                    statut: p.status_debug.clone(),
                    date,
                    compile_secs: t,
                    empreinte_v,
                });
            }
            // Sync auto vers CARGO_PKG_VERSION ≠ édition manuelle utilisateur :
            // seule la base suit, le manifeste reste tel quel (doctrine —
            // seule l'édition popup écrit la source).
            // Persister la mise à jour dans le registre.
            let alertes = registry::save_registry_full(&app.registry_path, &app.projects, &app.web_cache, &app.prefs)?;
            if !alertes.is_empty() {
                app.push_info(alertes.join("  ·  "));
            }
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

/// Garde « vérité du disque » au chargement.
///
/// Détecte les métadonnées qui se sont écartées d'une vérité connue pendant
/// que le menu ne tournait pas — typiquement une édition manuelle du registre
/// (la cause du faux « Release 0.50.1 »). Les valeurs sont restaurées et
/// déclarées à la garde, qui les re-persistera sans les neutraliser.
///
/// Variante Release : aucun chemin légitime ne l'écrit sans reconstruction.
/// Dès qu'une vérité est connue — snapshot cohérent avec le binaire, ou, à
/// défaut de fiche, la version réellement compilée (sonde `-V`) —
/// une valeur divergente est rendue.
///
/// Variante Debug : la seule incohérence sûre est une version relancée PLUS
/// ANCIENNE que le dernier build constaté. Une version plus récente est un
/// bump légitime non encore reconstruit, jamais restaurée ici.
///
/// Migration : une fiche antérieure à l'introduction de `date`/`empreinte_v`
/// est complétée depuis la sonde du binaire témoin (mtime cohérent), sans
/// jamais toucher version ni statut.
///
/// Renvoie (nombre de restaurations, alertes à afficher).
fn valider_metadonnees_chargement(app: &mut App) -> (usize, Vec<String>) {
    let mut restaurees = 0usize;
    let mut alertes: Vec<String> = Vec::new();
    for p in app.projects.iter_mut() {
        let id = p.id.clone();
        let chemin = p.path.clone();
        let sonde_release = p.probe_release.clone();
        let sonde_debug = p.probe_debug.clone();
        let t_release = registry::variant_modified_time(&chemin, "release")
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()));
        let t_debug = registry::variant_modified_time(&chemin, "debug")
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()));
        // Clone : éviter d'emprunter `p` (via le snapshot) en réécrivant `p`.
        let snap_release = p.release_snapshot.clone();
        if let Some(snap) = snap_release {
            // Le binaire présent EST celui du snapshot : la fiche fait foi.
            if t_release == Some(snap.compile_secs) {
                if p.version_release != snap.version {
                    registry::accepter_modification(format!("{id}:version_release"));
                    p.version_release = snap.version.clone();
                    restaurees += 1;
                    alertes.push(format!(
                      "{id} : version Release restaurée (vérité du dernier build)."
                    ));
                }
                if p.status_release != snap.statut {
                    registry::accepter_modification(format!("{id}:status_release"));
                    p.status_release = snap.statut.clone();
                    restaurees += 1;
                    alertes.push(format!(
                      "{id} : statut Release restauré (vérité du dernier build)."
                    ));
                }
                // Migration : fiche antérieure à l'introduction de `date` et
                // `empreinte_v`. Le binaire témoin est présent (mtime cohérent) :
                // sa sonde `-V` complète la fiche, sans toucher version ni
                // statut — la Release reste immuable hors reconstruction.
                if snap.empreinte_v.is_empty()
                    && let Some(l) = variante::lire_variante_avec_sonde(
                        &chemin,
                        "release",
                        (!sonde_release.is_empty()).then_some(sonde_release.as_str()),
                    )
                    && let Some(fiche) = p.release_snapshot.as_mut()
                {
                    registry::accepter_modification(format!("{id}:release_snapshot"));
                    fiche.date = l.etat.date;
                    fiche.empreinte_v = l.empreinte;
                    restaurees += 1;
                    alertes.push(format!(
                      "{id} : fiche Release complétée (date et empreinte)."
                    ));
                }
            }
            // binaire plus récent que la fiche : le rattrapage du rescan fera
            // foi, on ne force rien ici.
        } else if let Some(v) = variante::lire_variante_avec_sonde(
            &chemin,
            "release",
            (!sonde_release.is_empty()).then_some(sonde_release.as_str()),
        )
        .map(|l| l.etat.version)
        {
            // Pas de fiche : le binaire est le seul témoin fiable (sonde).
            if p.version_release != v {
                registry::accepter_modification(format!("{id}:version_release"));
                p.version_release = v;
                restaurees += 1;
                alertes.push(format!(
                  "{id} : version Release restaurée (valeur compilée du binaire)."
                ));
            }
        }
        // Variante Debug : ne restaurer jamais une version bumpée en avant.
        let snap_debug = p.debug_snapshot.clone();
        if let Some(snap) = snap_debug {
            if t_debug == Some(snap.compile_secs)
                && registry::validate_version(&p.version_debug)
                && registry::validate_version(&snap.version)
                && version_greater(&snap.version, &p.version_debug)
            {
                registry::accepter_modification(format!("{id}:version_debug"));
                registry::accepter_modification(format!("{id}:version"));
                p.version_debug = snap.version.clone();
                p.version = snap.version.clone();
                restaurees += 1;
                alertes.push(format!(
                  "{id} : version Debug restaurée (plus ancienne que le dernier build)."
                ));
            }
            // Migration de la fiche Debug (date/empreinte manquantes).
            if snap.empreinte_v.is_empty()
                && t_debug == Some(snap.compile_secs)
                && let Some(l) = variante::lire_variante_avec_sonde(
                    &chemin,
                    "debug",
                    (!sonde_debug.is_empty()).then_some(sonde_debug.as_str()),
                )
                && let Some(fiche) = p.debug_snapshot.as_mut()
            {
                registry::accepter_modification(format!("{id}:debug_snapshot"));
                fiche.date = l.etat.date;
                fiche.empreinte_v = l.empreinte;
                restaurees += 1;
                alertes.push(format!(
                  "{id} : fiche Debug complétée (date et empreinte)."
                ));
            }
        }
    }
    (restaurees, alertes)
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
            vue_archives: false,
            archive_root: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("archives-projets"),
            projets_root: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("projets"),
            mode: LaunchMode::Normal,
            status_messages: Vec::new(),
            registry_path,
            opencode_db_path,
            sort_order,
            pending_launch: None,
            pending_sudo: false,
            pending_name: None,
            pending_bare: false,
            bump_en_attente: None,
            web_cache: registry::WebCache::default(),
            prefs: registry::Preferences::default(),
            registry_broken: false,
            refus: std::collections::HashMap::new(),
            diagnostics: Vec::new(),
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
        // Les entrées vivant sous la racine des bibliothèques sont des
        // bibliothèques de droit, même si la mention n'a jamais été écrite.
        registry::deriver_bibliotheques(&mut app.projects, &app.prefs.lib_root_resolu());
        // Garde « vérité du disque » au chargement : des métadonnées écartées
        // d'une vérité connue (édition manuelle) sont restaurées et persistées.
        let (restaurees, alertes_meta) = valider_metadonnees_chargement(&mut app);
        if !alertes_meta.is_empty() {
            app.push_error(alertes_meta.join("  ·  "));
        }
        if restaurees > 0 {
            match registry::save_registry_full(
              &app.registry_path,
              &app.projects,
              &app.web_cache,
              &app.prefs,
            ) {
                Ok(_) => {}
                Err(e) => app.push_error(format!("Métadonnées restaurées mais registre non écrit : {e}")),
            }
        }
        // Auto-sync projet menu : aligner version_debug sur CARGO_PKG_VERSION.
        progress.begin(Step::SyncVersion);
        let _ = sync_menu_project_version(&mut app);
        progress.begin(Step::Finalize);
        refresh_project_selection(&mut app);
        progress.done();
        // Complément « git » : reconstruit la trace comptable des sessions
        // antérieures à la comptabilisation par constat. Une session déjà
        // tracée n'est jamais réécrite, ce qui rend l'opération idempotente.
        if let Ok(n) = apport::reconstruire_historique_git(
          &app.opencode_db_path,
          &app.projects,
          &apport::chemin_apports(&app.registry_path),
        ) && n > 0
        {
            app.push_info(format!("Historique : {n} sessions reconstruites par git"));
        }
        // Bandeau de démarrage : rappel de l'état de la comptabilité.
        if let Ok(apports) = apport::charger_apports(&apport::chemin_apports(&app.registry_path))
            && let Some(bandeau) = apport::bandeau_debut(&apports)
        {
            app.push_info(bandeau);
        }
        // Contrôle de cohérence au lancement : on expose les écarts, on ne
        // tranche pas. Les refus déjà enregistrés sont rechargés et re-notifiés.
        app.refus = registry::load_refus(&app.registry_path);
        signaler_coherence(&mut app);
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
            vue_archives: false,
            archive_root: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("archives-projets"),
            projets_root: dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("projets"),
            mode: LaunchMode::Normal,
            status_messages: Vec::new(),
            registry_path,
            opencode_db_path,
            sort_order,
            pending_launch: None,
            pending_sudo: false,
            pending_name: None,
            pending_bare: false,
            bump_en_attente: None,
            web_cache: registry::WebCache::default(),
            prefs: registry::Preferences::default(),
            registry_broken: false,
            refus: std::collections::HashMap::new(),
            diagnostics: Vec::new(),
        };
        match registry::load_registry(&app.registry_path) {
            Ok(projects) => app.projects = projects,
            Err(e) => eprintln!("Registre illisible : {e}"),
        }
        registry::deriver_bibliotheques(&mut app.projects, &app.prefs.lib_root_resolu());
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

    /// Le projet à cet index a-t-il été déplacé dans les archives ?
    ///
    /// C'est son chemin qui le dit : l'archivage déplace le dossier, et
    /// aucun champ du registre ne double cette vérité.
    pub(crate) fn est_archive(&self, idx: usize) -> bool {
        self.projects[idx].path.starts_with(&self.archive_root)
    }

    /// Destination libre dans `racine`.
    ///
    /// Deux projets homonymes peuvent coexister (l'un actif, l'autre déjà
    /// archivé) : le second atterrit à côté du premier plutôt que de faire
    /// échouer tout le geste.
    fn destination_unique(racine: &Path, nom: &str) -> PathBuf {
        let base = racine.join(nom);
        if !base.exists() {
            return base;
        }
        let mut n = 1u32;
        loop {
            let candidate = racine.join(format!("{nom}-{n}"));
            if !candidate.exists() {
                return candidate;
            }
            n += 1;
        }
    }

    /// Déplace le projet sélectionné dans `archive_root` : récupérable, et
    /// sorti de `~/projets` donc invisible pour le rescan.
    pub(crate) fn archiver_selection(&mut self) -> Result<String> {
        let idx = self
            .selected_project_idx()
            .ok_or_else(|| anyhow::anyhow!("Aucun projet sélectionné"))?;
        let nom = nom_de_dossier(&self.projects[idx]);
        let source = self.projects[idx].path.clone();
        let cible = Self::destination_unique(&self.archive_root, &nom);
        fs::create_dir_all(&self.archive_root)?;
        fs::rename(&source, &cible)?;
        self.projects[idx].path = cible.clone();
        self.sauver_et_refiltrer()?;
        Ok(format!("« {nom} » archivé → {}", cible.display()))
    }

    /// Ramène un projet archivé dans la racine des projets actifs.
    pub(crate) fn restaurer_selection(&mut self) -> Result<String> {
        let idx = self
            .selected_project_idx()
            .ok_or_else(|| anyhow::anyhow!("Aucun projet archivé sélectionné"))?;
        if !self.est_archive(idx) {
            return Err(anyhow::anyhow!("Ce projet n'est pas archivé"));
        }
        let nom = nom_de_dossier(&self.projects[idx]);
        // La racine se déduit des projets encore actifs ; si tous dorment
        // aux archives, c'est celle retenue au dernier rescan qui tranche —
        // jamais le répertoire personnel, qui n'a rien à y faire ici.
        let racine = projects_root_actifs(&self.projects, &self.archive_root)
            .unwrap_or_else(|| self.projets_root.clone());
        let source = self.projects[idx].path.clone();
        let cible = Self::destination_unique(&racine, &nom);
        fs::create_dir_all(&racine)?;
        fs::rename(&source, &cible)?;
        self.projects[idx].path = cible.clone();
        self.sauver_et_refiltrer()?;
        Ok(format!("« {nom} » restauré → {}", cible.display()))
    }

    /// Bascule entre la liste des projets et celle des archives.
    pub(crate) fn basculer_vue_archives(&mut self) {
        self.vue_archives = !self.vue_archives;
        // Créer un projet depuis les archives n'a aucun sens : la ligne
        // « Nouveau projet » n'y est pas offerte.
        self.show_new = !self.vue_archives;
        refresh_project_selection(self);
    }

    /// Persiste le registre puis recalcule la liste à jour.
    ///
    /// Un déplacement de dossier non persisté réapparaîtrait à l'ancien emplacement
    /// au prochain démarrage, le disque disant « plus rien ici » : le rescan
    /// effacerait alors l'entrée.
    fn sauver_et_refiltrer(&mut self) -> Result<()> {
        let alertes = registry::save_registry_full(
            &self.registry_path,
            &self.projects,
            &self.web_cache,
            &self.prefs,
        )?;
        if !alertes.is_empty() {
            self.push_info(alertes.join("  ·  "));
        }
        refresh_project_selection(self);
        Ok(())
    }

    fn confirm_selection(&mut self) -> Option<InputMode> {
        if self.is_bare_selected() {
            self.set_bare_launch();
            None
        } else if self.is_new_selected() {
            Some(InputMode::ChoixNouveau { selected: 0 })
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
    println!("       op --new [nom]");
    println!();
    println!("  op            menu interactif (↑/↓ · Entrée · q) + création de projet");
    println!("  op <mot>      ouvre le projet unique correspondant, ou menu restreint");
    println!("                si plusieurs projets correspondent au mot-clé");
    println!("  op --new      ouvre directement le formulaire de création");
    println!("                (équivalent de la touche « n » dans le menu)");
    println!("  op --new <nom>  idem, avec le nom pré-rempli");
    println!("  op -r, --root <mot>");
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

/// La devise de la session est-elle déjà la devise locale ?
///
/// Une session payée en euros n'a pas d'équivalent à afficher : la colonne
/// « Converti » resterait vide et la cellule combinée se répéterait deux fois.
fn devise_est_locale(devise: &str) -> bool {
  devise.eq_ignore_ascii_case(LOCAL_CURRENCY)
}

/// Montant d'une session « 4,20 € · 4,57 $ » : l'équivalent local d'abord,
/// pour qu'il soit en tête à l'œil, puis le montant réellement facturé.
///
/// `local` vide (aucun taux connu, ou coût non communiqué) retombe sur le seul
/// montant d'origine : mieux vaut une valeur qui existe qu'une conversion qui
/// n'existe pas. Un espace fine sépare les deux montants.
fn cout_session_double(orig: &str, local: &str) -> String {
  if local.is_empty() || local == "—" {
    orig.to_string()
  } else {
    format!("{local}\u{2009}·\u{2009}{orig}")
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
/// sans dépendre d'un vrai terminal. Depuis ratatui 0.30 le trait `Backend`
/// laisse le type d'erreur libre (`io::Error` en production, `Infallible` pour
/// `TestBackend`) : on le contraint seulement à ce qu'`anyhow::Error` sait
/// absorber, pour ne pas perdre la cause d'une erreur au passage.
fn animer_jauge<B>(terminal: &mut Terminal<B>, state: &mut startup::GaugeState,
    rx: &std::sync::mpsc::Receiver<startup::Signal>, avertissements: &mut Vec<String>) -> Option<Result<Option<App>>>
where
    B: ratatui::backend::Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
  {
    use std::{sync::mpsc::TryRecvError, time::Duration};

    const RAFRAICHISSEMENT: Duration = Duration::from_millis(40);

    let mut issue: Option<Result<Option<App>>> = None;
    'attente: while issue.is_none()
      {
        state.tick();
        if let Err(e) = terminal.draw(|f| startup::render(f, state))
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
    // lancement direct par mot-clé restent silencieux et rapides. `--new` est
    // interactif par construction : il n'a de sens qu'avec le formulaire ouvert.
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
                let bin_d = affichage_variante_binaire(p, "debug", false);
                let bin_r = affichage_variante_binaire(p, "release", false);
                let d_base = (p.version_debug.as_str(), p.status_debug.as_str());
                let r_base = (p.version_release.as_str(), p.status_release.as_str());
                let d_bin = bin_d.as_ref().map(|(v,s,d)| (v.as_str(), s.as_str(), d.as_str()));
                let r_bin = bin_r.as_ref().map(|(v,s,d)| (v.as_str(), s.as_str(), d.as_str()));
                println!("    Variante : Debug {} · Release {}", libelle_bifilaire(d_bin, d_base), libelle_bifilaire(r_bin, r_base));
                println!();
            }
        }
        return Ok(());
    }

    if cli.version {
        // Vérité binaire d'abord (sonde ponctuelle des deux variantes), base
        // du registre en complément quand elle diverge : le vide muet
        // n'existe pas, une variante sans build s'affiche « — ».
        let affiche = |p: &ProjectEntry| {
            let bin_d = affichage_variante_binaire(p, "debug", true);
            let bin_r = affichage_variante_binaire(p, "release", true);
            let d = libelle_bifilaire(
                bin_d.as_ref().map(|(v, s, d)| (v.as_str(), s.as_str(), d.as_str())),
                (p.version_debug.as_str(), p.status_debug.as_str()),
            );
            let r = libelle_bifilaire(
                bin_r.as_ref().map(|(v, s, d)| (v.as_str(), s.as_str(), d.as_str())),
                (p.version_release.as_str(), p.status_release.as_str()),
            );
            println!("{} : Debug={} Release={}", sanitize(&p.name), sanitize(&d), sanitize(&r));
        };
        if let Some(keyword) = cli.keyword.as_deref() {
            let idx = app.matching_indexes(keyword);
            match idx.as_slice() {
                [] => {
                    eprintln!("Aucun projet ne correspond à : {keyword}");
                }
                [i] => {
                    affiche(&app.projects[*i]);
                }
                _ => {
                    for i in &idx {
                        affiche(&app.projects[*i]);
                    }
                }
            }
        } else {
            // Version du menu lui-même : la vérité compilée du binaire en
            // cours, pas une valeur du registre (qui ne change qu'à la
            // compilation — d'où la note de fraîcheur qui l'accompagne).
            println!("{RELEASE}_{STATUT} ({DATE_LAST_MAJ})");
            if let Some(p) = app.projects.iter().find(|p| p.name == "OpenCode Start Menu")
                && let Some(note) = binary_freshness_note(&p.path)
            {
                println!("{note}");
            }
        }
        return Ok(());
    }
    let a_mot_cle = cli.keyword.is_some();
    if let Some(keyword) = cli.keyword.clone() {
        app.filter(&keyword, if cli.root { LaunchMode::Root } else { LaunchMode::Normal });
        if let Some(dir) = app.pending_launch.take() {
            let name = app.pending_name.take()
                .unwrap_or_else(|| dir.display().to_string());
            // Comptabilité : la classe de la session est figée au lancement,
            // le projet est déjà dans `app.projects` (filtre par mot-clé).
            compta::enregistrer_lancement(
                &compta::chemin_table(&app.registry_path),
                &compta::classer_repertoire(&app.projects, &dir, app.pending_bare),
            );
            // Constat de départ de la comptabilisation (best-effort).
            apport::demarrer(&app.registry_path, &app.projects, &dir);
            let kind = kind_pour_repertoire(&dir, &app.opencode_db_path, app.pending_bare);
            app.pending_bare = false;
            run_launch_sequence(kind, &name, &dir, app.pending_sudo)?;
            // Constat de fin : la session passée est auditée (best-effort).
            apport::constater_retour(
                &app.registry_path,
                &app.opencode_db_path,
                &app.projects,
                &dir,
                false,
            )
            .ok();
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

    // `op --new [nom]` ouvre le formulaire de création, comme la touche `n`
    // dans le menu. Le nom est pré-rempli quand il est fourni ; `default_missing_value`
    // fait que `--new` seul arrive ici avec une chaîne vide.
    let mode_initial = match cli.new.as_deref() {
      Some(nom) => {
        let mut state = ProjectCreationState {
          name: nom.to_string(),
          // Le nom est le champ 1 : c'est lui que le curseur doit viser.
          active_field: if nom.is_empty() { 0 } else { 1 },
          premiere_touche: true,
          ..ProjectCreationState::default()
        };
        load_creation_field(&mut state);
        InputMode::CreatingProject(state)
      }
      None => InputMode::Normal,
    };

    // Session terminal unique : le menu réutilise le même terminal, et la
    // restauration est garantie même quand le menu remonte une erreur.
    let debut_terminal = std::time::Instant::now();
    let mut terminal = open_terminal()?;
    let resultat = run_tui(app, &mut terminal, mode_initial);
    close_terminal(&mut terminal);
    if let Some(journal) = verbose.as_ref()
      {
        startup::trace_terminal(journal, debut_terminal.elapsed());
      }
    resultat
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

/// Ctrl+C ou Ctrl+D : quitter. Sans modificateur, la touche « c » garde son
/// sens (contrôle de cohérence) et « q » la sortie normale.
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
        close_terminal(&mut terminal);
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

fn run_tui(app: App, terminal: &mut Term, mode_initial: InputMode) -> Result<()> {
    tui_loop(terminal, app, mode_initial)
}

/// Ouvre le popup unifié sur l'onglet demandé, ou reste sur la liste si aucun
/// projet n'est sélectionné.
///
/// Une seule touche y mène depuis la liste : `p`, qui se pose sur
/// Paramètres. `Tab` et `⇧Tab` desservent ensuite les trois onglets, ce qui
/// évite de tenir trois lettres (`e`, `c`, `s`) pour trois entrées du même
/// écran.
///
/// Les réglages du projet et les coûts portent sur un projet précis : sans
/// sélection, il n'y a rien à montrer. Les préférences sont globales et
/// restent donc accessibles même quand la liste est vide.
fn ouvrir_popup(app: &mut App, onglet: Onglet) -> InputMode {
    let Some(idx) = app.selected_project_idx() else {
        return InputMode::Popup(Popup::sans_projet(onglet, app.web_cache.country.clone()));
    };
    let Some(project) = app.projects.get(idx) else {
        return InputMode::Popup(Popup::sans_projet(onglet, app.web_cache.country.clone()));
    };
    let projet = ProjectEditState::from_project(project, idx);
    let country = app.web_cache.country.clone();
    InputMode::Popup(Popup::nouveau(idx, projet, country).avec_onglet(onglet))
}

/// Demande de bump manuel : annonce rien de plus, n'écrit rien encore.
///
/// Le projet est mémorisé par son index : le confirmer immédiatement après
/// (`o`) garantit qu'aucun rescan n'a pu décaler la sélection entre les
/// deux frappes, puisqu'une autre touche annule la demande.
fn demander_bump(app: &mut App)
  {
    let Some(idx) = app.selected_project_idx()
      else
        {
          app.push_info("Aucun projet sélectionné".to_string());
          return;
        };
    let nom = app.projects[idx].name.clone();
    app.bump_en_attente = Some(idx);
    app.push_info(format!("Bumper « {nom} » ? (o/N)"));
  }

/// Annulation : plus rien à confirmer, la zone de message redevient vide.
fn annuler_bump(app: &mut App)
  {
    app.bump_en_attente = None;
    app.status_messages.clear();
  }

/// Confirmation : incrément du patch, manifeste réécrit, registre persisté.
fn confirmer_bump(app: &mut App)
  {
    let Some(idx) = app.bump_en_attente.take()
      else
        {
          return;
        };
    if idx >= app.projects.len()
      {
        return;
      }
    let resultat = bump_projet(&mut app.projects[idx]);
    match resultat
      {
        Ok((avant, apres)) =>
          {
            let nom = app.projects[idx].name.clone();
            let sauve = registry::save_registry_full(
              &app.registry_path,
              &app.projects,
              &app.web_cache,
              &app.prefs,
            );
            match sauve
              {
                Ok(alertes) =>
                  {
                    let mut msg = format!("{nom} : {avant} → {apres}");
                    if !alertes.is_empty()
                      {
                        msg.push_str("  ·  ");
                        msg.push_str(&alertes.join("  ·  "));
                      }
                    app.push_info(msg);
                  }
                Err(e) => app.push_error(format!(
                  "{nom} : {avant} → {apres}, mais registre non écrit ({e})"
                )),
              }
          }
        Err(e) => app.push_error(format!("Bump impossible : {e}")),
      }
  }

// --- Contrôle de cohérence ------------------------------------------------

/// Une ligne affichée par la modale de cohérence : un écart d'un projet,
/// l'action proposée, et la date d'un refus éventuel.
#[derive(Clone)]
pub(crate) struct LigneCoherence {
    pub(crate) projet_id: String,
    pub(crate) projet_nom: String,
    pub(crate) ecart: coherence::Ecart,
    pub(crate) action: coherence::Action,
    pub(crate) refus_date: Option<String>,
}

/// Date du jour au format `jj-mm-yyyy` (convention du contrat `-V`).
fn aujourd_hui() -> String
  {
    chrono::Local::now().format("%d-%m-%Y").to_string()
  }

/// Recalcule les diagnostics de tous les projets, les mémorise dans `app`, et
/// renvoie `(total, non refusés)`.
///
/// Le total compte tous les écarts, y compris refusés : un refus est conservé
/// et re-notifié. Les « non refusés » sont ceux qui n'ont pas encore été
/// tranchés par l'utilisateur.
fn controler_coherence(app: &mut App) -> (usize, usize)
  {
    let mut diagnostics = Vec::new();
    for p in &app.projects
      {
        let obs = coherence::observer(p);
        let diagnostic = coherence::diagnostiquer(&obs);
        if !diagnostic.est_vide()
          {
            diagnostics.push(DiagnosticProjet {
                id: p.id.clone(),
                name: p.name.clone(),
                diagnostic,
            });
          }
      }
    app.diagnostics = diagnostics;
    let mut total = 0usize;
    let mut non_refuses = 0usize;
    for d in &app.diagnostics
      {
        for e in &d.diagnostic.ecarts
          {
            total += 1;
            if !refus_existe(app, &d.id, &e.cle(), e.action().code())
              {
                non_refuses += 1;
              }
          }
      }
    (total, non_refuses)
  }

/// Contrôle la cohérence et signale à l'utilisateur les écarts non tranchés.
fn signaler_coherence(app: &mut App)
  {
    let (_, non_refuses) = controler_coherence(app);
    if non_refuses > 0
      {
        app.push_info(format!(
          "{non_refuses} incohérence(s) détectée(s) — touche 'c' pour examiner"
        ));
      }
  }

/// Un refus est-il déjà enregistré pour cet écart et cette action ?
fn refus_existe(app: &App, id: &str, ecart: &str, action: &str) -> bool
  {
    app.refus
        .get(id)
        .is_some_and(|liste| liste.iter().any(|r| r.ecart == ecart && r.action == action))
  }

/// Construit la liste plate des écarts affichés par la modale.
pub(crate) fn lignes_coherence(app: &App) -> Vec<LigneCoherence>
  {
    let mut lignes = Vec::new();
    for d in &app.diagnostics
      {
        for e in &d.diagnostic.ecarts
          {
            let action = e.action();
            let refus_date = app.refus.get(&d.id).and_then(|liste| {
                liste
                    .iter()
                    .find(|r| r.ecart == e.cle() && r.action == action.code())
                    .map(|r| r.quand.clone())
            });
            lignes.push(LigneCoherence {
                projet_id: d.id.clone(),
                projet_nom: d.name.clone(),
                ecart: e.clone(),
                action,
                refus_date,
            });
          }
      }
    lignes
  }

/// Ouvre la modale de cohérence, ou annonce qu'il n'y a rien à voir.
fn ouvrir_coherence(app: &mut App) -> InputMode
  {
    let (total, _) = controler_coherence(app);
    if total == 0
      {
        app.push_info("Aucune incohérence détectée".to_string());
        return InputMode::Normal;
      }
    // Résume les actions proposées, sans doublon, dans l'ordre des projets.
    let mut libelles: Vec<&'static str> = Vec::new();
    for d in &app.diagnostics
      {
        for a in d.diagnostic.actions()
          {
            let l = a.libelle();
            if !libelles.contains(&l)
              {
                libelles.push(l);
              }
          }
      }
    app.push_info(format!(
      "{total} incohérence(s) — actions proposées : {}",
      libelles.join(", ")
    ));
    InputMode::Coherence(CoherenceState::default())
  }

/// Aligne la base (registre) sur la vérité du disque pour un projet.
///
/// Rattrapage lecture seule : les deux variantes sont interrogées (fiche si
/// elle décrit le binaire présent, sinon sonde `-V`), version, statut et
/// date inclus. Un statut obsolète est effacé quand le binaire n'en déclare
/// plus ; les champs historiques suivent la variante élue (Debug). Aucune
/// écriture de source.
fn executer_maj_base(app: &mut App, id: &str) -> Result<()>
  {
    let Some(idx) = app.projects.iter().position(|p| p.id == id)
      else
        {
          return Ok(());
        };
    let obs = coherence::observer(&app.projects[idx]);
    // Variante Debug : binaire s'il répond au contrat, sinon manifeste. La date
    // vient du binaire ; à défaut d'un `-V`, la cible `target/debug` (mtime) est
    // le dernier recours — c'est la seule vérité d'un projet non compilé.
    let (version_d, statut_d, date_d) = match &obs.debug
      {
        Some(e) if !e.version.is_empty() =>
          (e.version.clone(), e.statut.clone(), e.date.clone()),
        _ =>
          (
            obs.sources_version.clone(),
            String::new(),
            obs.cible_date_debug.clone().unwrap_or_default(),
          ),
      };
    if version_d.is_empty() && date_d.is_empty()
      {
        anyhow::bail!("aucune version à aligner");
      }
    // Variante Release : sonde ou fiche, jamais empruntée au Debug. Sans `-V`,
    // la cible `target/release` (mtime) prend le relais.
    let (version_r, statut_r, date_r) = match &obs.release
      {
        Some(e) if !e.version.is_empty() =>
          (e.version.clone(), e.statut.clone(), e.date.clone()),
        _ => (
            String::new(),
            String::new(),
            obs.cible_date_release.clone().unwrap_or_default(),
        ),
      };
    registry::accepter_modification(format!("{id}:version_debug"));
    registry::accepter_modification(format!("{id}:status_debug"));
    registry::accepter_modification(format!("{id}:version"));
    registry::accepter_modification(format!("{id}:status"));
    registry::accepter_modification(format!("{id}:version_release"));
    registry::accepter_modification(format!("{id}:status_release"));
    let p = &mut app.projects[idx];
    // Version/statut ne sont écrits que si une vérité les porte : un binaire
    // `-V`, ou le manifeste. Sans rien de tel (projet sans code), la base
    // garde ses valeurs — seule sa date suit la cible.
    let debug_declare = obs.debug.is_some() || !obs.sources_version.is_empty();
    if debug_declare
      {
        p.version_debug = version_d.clone();
        // Le statut suit toujours la vérité sondée, y compris vers le vide :
        // un statut obsolète ne doit pas survivre à un binaire qui ne le
        // déclare plus.
        p.status_debug = statut_d.clone();
        p.version = version_d;
        p.status = statut_d;
      }
    if !version_r.is_empty()
      {
        p.version_release = version_r;
        p.status_release = statut_r;
      }
    // Dates déclarées : la fiche du binaire est la vérité, à défaut la date
    // est laissée telle quelle (elle sera complétée par le rattrapage).
    if !date_d.is_empty()
      {
        registry::accepter_modification(format!("{id}:date_debug"));
        p.date_debug = date_d;
      }
    if !date_r.is_empty()
      {
        registry::accepter_modification(format!("{id}:date_release"));
        p.date_release = date_r;
      }
    let alertes = registry::save_registry_full(
      &app.registry_path,
      &app.projects,
      &app.web_cache,
      &app.prefs,
    )?;
    if !alertes.is_empty()
      {
        app.push_info(alertes.join("  ·  "));
      }
    Ok(())
  }

/// Extrait des dernières lignes de `stderr` pour un message d'erreur lisible.
///
/// Renvoie `""` quand il n'y a rien à montrer, ou `" : <extrait>"` sinon —
/// forme directement interpolable à la suite d'un message « … a échoué ».
fn extrait_erreur(stderr: &[u8]) -> String
  {
    let texte = String::from_utf8_lossy(stderr);
    let lignes: Vec<&str> = texte.lines().filter(|l| !l.trim().is_empty()).collect();
    let debut = lignes.len().saturating_sub(5);
    let extrait = lignes[debut..].join(" | ");
    if extrait.is_empty()
      {
        String::new()
      }
    else
      {
        format!(" : {extrait}")
      }
  }

/// Lance `cargo` dans un projet en capturant sa sortie.
///
/// La sortie est capturée, jamais héritée : en mode brut, sur l'écran alternatif,
/// les lignes de cargo écriraient par-dessus l'interface. En cas d'échec, un
/// extrait de stderr remonte dans le message d'erreur, sinon l'utilisateur ne
/// verrait qu'un code de retour muet.
fn lancer_cargo(dir: &Path, args: &[&str]) -> Result<()>
  {
    let sortie = std::process::Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .output()?;
    if !sortie.status.success()
      {
        let extrait = extrait_erreur(&sortie.stderr);
        anyhow::bail!("cargo {} a échoué{extrait}", args.join(" "));
      }
    Ok(())
  }

/// Lance la commande de build d'une variante, selon son langage.
///
/// La commande personnalisée du projet (`build_debug`/`build_release`) prime ;
/// à défaut, Cargo est utilisé si le dossier porte un `Cargo.toml` ; sinon la
/// reconstruction s'arrête sur une erreur explicite — un projet d'un autre
/// langage doit déclarer sa commande dans Réglages.
fn lancer_build(dir: &Path, profil: &str, commande: &str) -> Result<()>
  {
    let commande = commande.trim();
    if !commande.is_empty()
      {
        let sortie = std::process::Command::new("sh")
            .arg("-c")
            .arg(commande)
            .current_dir(dir)
            .output()?;
        if !sortie.status.success()
          {
            let extrait = extrait_erreur(&sortie.stderr);
            anyhow::bail!("`{commande}` a échoué{extrait}");
          }
        return Ok(());
      }
    if dir.join("Cargo.toml").is_file()
      {
        let args: &[&str] =
            if profil == "release" { &["build", "--release"] } else { &["build"] };
        return lancer_cargo(dir, args);
      }
    anyhow::bail!(
      "aucune commande de build {profil} : renseignez-la dans Réglages (p)"
    )
  }

/// Reconstruit la variante Debug d'un projet, met à jour le snapshot Debug
/// et aligne la base Debug sur le binaire fraîchement construit.
fn executer_rebuild_debug(app: &mut App, id: &str) -> Result<()>
  {
    let Some(idx) = app.projects.iter().position(|p| p.id == id)
      else
        {
          return Ok(());
        };
    let path = app.projects[idx].path.clone();
    let sonde = app.projects[idx].probe_debug.clone();
    let build = app.projects[idx].build_debug.clone();
    lancer_build(&path, "debug", &build)?;
    let Some(lv) = crate::variante::lire_variante_avec_sonde(
        &path,
        "debug",
        (!sonde.is_empty()).then_some(sonde.as_str()),
    )
    else
      {
          anyhow::bail!("binaire debug hors contrat après build");
      };
    let mtime = registry::variant_modified_time(&path, "debug")
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()))
        .unwrap_or_else(|| {
          std::time::SystemTime::now()
              .duration_since(std::time::UNIX_EPOCH)
              .unwrap_or_default()
              .as_secs()
        });
    registry::accepter_modification(format!("{id}:version_debug"));
    registry::accepter_modification(format!("{id}:status_debug"));
    registry::accepter_modification(format!("{id}:date_debug"));
    registry::accepter_modification(format!("{id}:last_compile_debug_secs"));
    registry::accepter_modification(format!("{id}:debug_snapshot"));
    let p = &mut app.projects[idx];
    p.version_debug = lv.etat.version.clone();
    p.status_debug = lv.etat.statut.clone();
    if !lv.etat.date.is_empty()
      {
        p.date_debug = lv.etat.date.clone();
      }
    p.last_compile_debug_secs = mtime;
    p.debug_snapshot = Some(registry::SnapshotCompile {
        version: lv.etat.version.clone(),
        statut: lv.etat.statut.clone(),
        date: lv.etat.date.clone(),
        empreinte_v: lv.empreinte.clone(),
        compile_secs: mtime,
    });
    let alertes = registry::save_registry_full(
      &app.registry_path,
      &app.projects,
      &app.web_cache,
      &app.prefs,
    )?;
    if !alertes.is_empty()
      {
        app.push_info(alertes.join("  ·  "));
      }
    Ok(())
  }

/// Reconstruit la variante Release d'un projet, met à jour le snapshot Release
/// et aligne la base Release sur le binaire fraîchement construit.
fn executer_rebuild_release(app: &mut App, id: &str) -> Result<()>
  {
    let Some(idx) = app.projects.iter().position(|p| p.id == id)
      else
        {
          return Ok(());
        };
    let path = app.projects[idx].path.clone();
    let sonde = app.projects[idx].probe_release.clone();
    let build = app.projects[idx].build_release.clone();
    lancer_build(&path, "release", &build)?;
    let Some(lv) = crate::variante::lire_variante_avec_sonde(
        &path,
        "release",
        (!sonde.is_empty()).then_some(sonde.as_str()),
    )
    else
      {
          anyhow::bail!("binaire release hors contrat après build");
      };
    let mtime = registry::variant_modified_time(&path, "release")
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()))
        .unwrap_or_else(|| {
          std::time::SystemTime::now()
              .duration_since(std::time::UNIX_EPOCH)
              .unwrap_or_default()
              .as_secs()
        });
    registry::accepter_modification(format!("{id}:version_release"));
    registry::accepter_modification(format!("{id}:status_release"));
    registry::accepter_modification(format!("{id}:date_release"));
    registry::accepter_modification(format!("{id}:last_compile_release_secs"));
    registry::accepter_modification(format!("{id}:release_snapshot"));
    let p = &mut app.projects[idx];
    p.version_release = lv.etat.version.clone();
    p.status_release = lv.etat.statut.clone();
    if !lv.etat.date.is_empty()
      {
        p.date_release = lv.etat.date.clone();
      }
    p.last_compile_release_secs = mtime;
    p.release_snapshot = Some(registry::SnapshotCompile {
        version: lv.etat.version.clone(),
        statut: lv.etat.statut.clone(),
        date: lv.etat.date.clone(),
        empreinte_v: lv.empreinte.clone(),
        compile_secs: mtime,
    });
    let alertes = registry::save_registry_full(
      &app.registry_path,
      &app.projects,
      &app.web_cache,
      &app.prefs,
    )?;
    if !alertes.is_empty()
      {
        app.push_info(alertes.join("  ·  "));
      }
    Ok(())
  }

/// Statut valide à retenir pour un projet : le statut enregistré Debug s'il
/// appartient à la liste connue, sinon celui de Release, sinon « Release ».
///
/// Sert à la fois à conformer un binaire hors contrat et à corriger un statut
/// hors liste : on ne retient jamais une valeur inventée, seulement une valeur
/// déjà connue d'op.
fn statut_cible(entry: &registry::ProjectEntry) -> String
  {
    for s in [&entry.status_debug, &entry.status_release]
      {
        if registry::validate_status(s)
          {
            return s.clone();
          }
      }
    "Release".to_string()
  }

/// Version des sources à figer dans `RELEASE` : la version du manifeste, sinon
/// celle enregistrée pour Debug.
fn version_cible(entry: &registry::ProjectEntry) -> String
  {
    if entry.version.is_empty()
      {
        entry.version_debug.clone()
      }
    else
      {
        entry.version.clone()
      }
  }

/// Date à figer dans `DATE_LAST_MAJ` lors d'une conformisation.
///
/// Si `RELEASE` et `STATUT` du source portent déjà la cible, rien ne change :
/// la date déjà déclarée est préservée (une conformisation ne périme pas un
/// code qui n'a pas bougé). Sinon le source est réellement réécrit : la date
/// prend celle du jour.
fn date_conformisation(contenu: &str, version: &str, statut: &str) -> String
  {
    let deja_aligne = conformite::lire_constante(contenu, "RELEASE").as_deref() == Some(version)
        && conformite::lire_constante(contenu, "STATUT").as_deref() == Some(statut);
    if deja_aligne
      {
        conformite::lire_constante(contenu, "DATE_LAST_MAJ").unwrap_or_default()
      }
    else
      {
        aujourd_hui()
      }
  }

/// Conforme le module principal d'un projet au contrat `-V`, puis reconstruit
/// Debug pour que le binaire publié reflète le code corrigé.
///
/// L'écriture est confinée au module principal : aucune autre source n'est
/// touchée, et une bibliothèque (sans binaire) n'est jamais conforme.
///
/// `DATE_LAST_MAJ` n'est réécrite que si le source change réellement : une
/// conformisation qui ne corrige rien (RELEASE et STATUT déjà alignés)
/// préserve la date déjà déclarée. Une conformisation ne doit pas périmer la
/// date d'un code qui n'a pas bougé.
fn conformer_et_rebuild(app: &mut App, id: &str, statut: &str) -> Result<()>
  {
    let Some(idx) = app.projects.iter().position(|p| p.id == id)
      else
        {
          return Ok(());
        };
    let path = app.projects[idx].path.clone();
    let version = version_cible(&app.projects[idx]);
    if version.is_empty()
      {
        anyhow::bail!("version inconnue : manifeste muet et registre vide");
      }
    let Some(module) = conformite::module_principal(&path)
      else
        {
          anyhow::bail!("module principal introuvable (bibliothèque sans binaire ?)");
        };
    let contenu = fs::read_to_string(&module)?;
    // Décidée par le source lui-même : si `RELEASE` porte déjà la version cible
    // et `STATUT` le statut cible, corriger la date serait la seule écriture —
    // or rien n'a changé, la date doit rester celle de la dernière vraie
    // modification. On la réinjecte donc telle quelle.
    let date = date_conformisation(&contenu, &version, statut);
    let (nouveau, modifie) =
      conformite::conformer_constantes(&contenu, &version, statut, &date);
    if modifie
      {
        write_atomic(&module, &nouveau)?;
      }
    // Le binaire Debug doit republier la vérité corrigée : sans reconstruction,
    // il resterait hors contrat.
    executer_rebuild_debug(app, id)?;
    Ok(())
  }

/// Conforme le code au contrat `-V` (binaire qui ne publie pas sa version).
fn executer_conformer_code(app: &mut App, id: &str) -> Result<()>
  {
    let Some(idx) = app.projects.iter().position(|p| p.id == id)
      else
        {
          return Ok(());
        };
    let statut = statut_cible(&app.projects[idx]);
    conformer_et_rebuild(app, id, &statut)
  }

/// Corrige un statut hors liste en le ramenant à une valeur connue.
///
/// Le statut est une constante du code : le corriger dans la base seule serait
/// un mensonge contredit par le `-V` du binaire. On révise donc le code, puis on
/// reconstruit Debug.
fn executer_corriger_statut(app: &mut App, id: &str) -> Result<()>
  {
    let Some(idx) = app.projects.iter().position(|p| p.id == id)
      else
        {
          return Ok(());
        };
    let statut = statut_cible(&app.projects[idx]);
    conformer_et_rebuild(app, id, &statut)
  }

/// Libellé d'une action « base mise à jour » : « Release rattrapée » quand la
/// base rattrape une variante Release, message générique sinon.
fn libelle_maj_base(ecart: &coherence::Ecart) -> &'static str
  {
    match ecart.variante()
      {
        Some(coherence::Variante::Release) => "Release rattrapée",
        _ => "base mise à jour",
      }
  }

/// Applique l'action proposée par une ligne, si elle est exécutable.
fn appliquer_action(app: &mut App, ligne: &LigneCoherence) -> Result<()>
  {
    use coherence::Action::*;
    match ligne.action
      {
        MajBase =>
          {
            executer_maj_base(app, &ligne.projet_id)?;
            retirer_refus_ecart(app, &ligne.projet_id, &ligne.ecart.cle())?;
            controler_coherence(app);
            app.push_info(format!(
                "{} : {}",
                ligne.projet_nom,
                libelle_maj_base(&ligne.ecart)
            ));
            Ok(())
          }
        RebuildDebug =>
          {
            executer_rebuild_debug(app, &ligne.projet_id)?;
            retirer_refus_ecart(app, &ligne.projet_id, &ligne.ecart.cle())?;
            controler_coherence(app);
            app.push_info(format!("{} : Debug reconstruit", ligne.projet_nom));
            Ok(())
          }
        RebuildRelease =>
          {
            executer_rebuild_release(app, &ligne.projet_id)?;
            retirer_refus_ecart(app, &ligne.projet_id, &ligne.ecart.cle())?;
            controler_coherence(app);
            app.push_info(format!("{} : Release reconstruite", ligne.projet_nom));
            Ok(())
          }
        ConformerCode =>
          {
            executer_conformer_code(app, &ligne.projet_id)?;
            retirer_refus_ecart(app, &ligne.projet_id, &ligne.ecart.cle())?;
            controler_coherence(app);
            app.push_info(format!("{} : code conformé au contrat -V", ligne.projet_nom));
            Ok(())
          }
        CorrigerStatut =>
          {
            executer_corriger_statut(app, &ligne.projet_id)?;
            retirer_refus_ecart(app, &ligne.projet_id, &ligne.ecart.cle())?;
            controler_coherence(app);
            app.push_info(format!("{} : statut corrigé", ligne.projet_nom));
            Ok(())
          }
      }
  }

/// Enregistre un refus, persisté et re-notifié tant que l'écart est identique.
fn enregistrer_refus(app: &mut App, ligne: &LigneCoherence) -> Result<()>
  {
    let ecart = ligne.ecart.cle();
    let action = ligne.action.code().to_string();
    let liste = app.refus.entry(ligne.projet_id.clone()).or_default();
    if !liste.iter().any(|r| r.ecart == ecart && r.action == action)
      {
        liste.push(registry::RefusCoherence {
            ecart,
            action,
            quand: aujourd_hui(),
        });
      }
    registry::save_refus(&app.registry_path, &app.refus)?;
    controler_coherence(app);
    app.push_info(format!(
      "{} : {} refusée (touche c pour revoir)",
      ligne.projet_nom,
      ligne.action.libelle()
    ));
    Ok(())
  }

/// Retire tout refus portant sur un écart donné (l'écart est résolu).
fn retirer_refus_ecart(app: &mut App, id: &str, ecart: &str) -> Result<()>
  {
    if let Some(liste) = app.refus.get_mut(id)
      {
        liste.retain(|r| r.ecart != ecart);
        if liste.is_empty()
          {
            app.refus.remove(id);
          }
      }
    registry::save_refus(&app.registry_path, &app.refus)
  }

/// Gère les touches de la modale de cohérence.
fn handle_coherence_input(
    key: event::KeyEvent,
    state: &mut CoherenceState,
    app: &mut App,
) -> Result<InputMode>
  {
    let lignes = lignes_coherence(app);
    if lignes.is_empty()
      {
        return Ok(InputMode::Normal);
      }
    let n = lignes.len();
    state.selection = state.selection.min(n - 1);

    // Confirmation dédiée d'une action Release en attente.
    if let Some(conf) = state.confirmation_release
      {
        match key.code
          {
            KeyCode::Char('o') | KeyCode::Char('O') | KeyCode::Enter =>
              {
                state.confirmation_release = None;
                if let Some(ligne) = lignes.get(conf).cloned()
                  {
                    appliquer_action(app, &ligne)?;
                  }
              }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') =>
              {
                state.confirmation_release = None;
              }
            _ => {}
          }
        if lignes_coherence(app).is_empty()
          {
            return Ok(InputMode::Normal);
          }
        return Ok(InputMode::Coherence(state.clone()));
      }

    match key.code
      {
        KeyCode::Esc | KeyCode::Char('q') => return Ok(InputMode::Normal),
        KeyCode::Down | KeyCode::Char('j') =>
          {
            state.selection = (state.selection + 1) % n;
          }
        KeyCode::Up | KeyCode::Char('k') =>
          {
            state.selection = (state.selection + n - 1) % n;
          }
        KeyCode::Enter =>
          {
            let ligne = lignes[state.selection].clone();
            // Toute action touchant la Release exige une confirmation dédiée.
            if ligne.action.touche_release()
              {
                state.confirmation_release = Some(state.selection);
              }
            else
              {
                appliquer_action(app, &ligne)?;
              }
          }
        KeyCode::Char('x') | KeyCode::Char('X') | KeyCode::Char('r') =>
          {
            let ligne = lignes[state.selection].clone();
            enregistrer_refus(app, &ligne)?;
          }
        _ => {}
      }
    // Si une action a tout résolu, la modale n'a plus lieu d'être.
    if lignes_coherence(app).is_empty()
      {
        return Ok(InputMode::Normal);
      }
    Ok(InputMode::Coherence(state.clone()))
  }

/// Incrémente le patch d'un projet et resynchronise le manifeste et le hash.
///
/// Même déroulé qu'AutoPatch (`rescan_repertoire`), en un seul geste :
/// version du registre, `version` élue, manifeste, puis recalcul du hash —
/// le manifeste étant lui-même hashé, l'ordre décide de la suite : sans ce
/// recalcul, le rescan suivant prendrait la nouvelle version pour une
/// modification et rebumperait à son tour.
fn bump_projet(p: &mut ProjectEntry) -> Result<(String, String)>
  {
    let avant = if p.version_debug.is_empty()
      {
        "—".to_string()
      }
    else
      {
        p.version_debug.clone()
      };
    let nouvelle = bump_patch(&p.version_debug).unwrap_or_else(|| "0.0.1".to_string());
    // Geste utilisateur explicite (touche `b`) : mutation déclarée à la garde.
    registry::accepter_modification(format!("{}:version_debug", p.id));
    registry::accepter_modification(format!("{}:version", p.id));
    p.version_debug = nouvelle.clone();
    p.version = nouvelle.clone();
    // Un édition manuelle suspendait les bumps automatiques : le geste
    // explicite remet la détection en marche.
    p.version_manually_edited = false;
    bump_manifest_version(&p.path, &nouvelle)?;
    p.source_hash = registry::compute_source_hash(&p.path);
    Ok((avant, nouvelle))
  }

/// Index des options de l'onglet Paramètres, dans l'ordre d'affichage.
const OPTION_TRI: usize = 0;
const OPTION_COLONNES: usize = 1;
const OPTION_LANCEMENT: usize = 2;
const OPTION_CAPTURE: usize = 3;
const OPTION_PAYS: usize = 4;

/// Nombre d'options de l'onglet Paramètres : tri, colonnes, lancement,
/// capture, pays source.
pub(crate) const PARAMETRES_CHAMPS: usize = 5;

/// Longueur du code pays, comme l'attend l'API de conversion.
const LONGUEUR_PAYS: usize = 3;

/// Ouvre l'aide en gardant le mode courant pour y revenir.
///
/// Le mode est mis dans une `Box` : l'aide y range l'écran d'origine, et
/// l'enum se référerait donc à lui-même.
fn ouvrir_aide(origine: InputMode, detaillee: bool) -> InputMode
  {
    InputMode::Aide { detaillee, origine: Box::new(origine), scroll: 0 }
  }

/// Une saisie de texte est-elle en cours dans le mode courant ?
///
/// `F10` est refusé dans ce cas : l'utilisateur est en train de taper une
/// version, une description ou un tarif, et la touche lui appartient.
fn saisie_en_cours_dans(mode: &InputMode) -> bool
  {
    match mode
      {
        InputMode::CreatingProject(state) => !state.status_menu_open,
        InputMode::Popup(popup) => saisie_en_cours(popup),
        _ => false,
      }
  }

/// Applique Tab et Shift+Tab : passage à l'onglet suivant ou précédent.
///
/// Renvoie `true` si la touche a été consommée, pour que le popup n'envoie
/// pas la même touche au handler de l'onglet d'arrivée.
fn naviguer_onglets(popup: &mut Popup, code: KeyCode) -> bool
  {
    if saisie_en_cours(popup)
      {
        return false;
      }
    match code
      {
        KeyCode::Tab =>
          {
            popup.onglet = popup.onglet.suivant();
            true
          }
        KeyCode::BackTab =>
          {
            popup.onglet = popup.onglet.precedent();
            true
          }
        _ => false,
      }
  }

/// Une saisie de texte est-elle en cours dans l'un des trois onglets ?
///
/// Tab et Shift+Tab doivent rester dans le champ : changer d'onglet en
/// plein milieu d'une frappe viderait le tampon.
fn saisie_en_cours(popup: &Popup) -> bool
  {
    popup.projet.editing || popup.couts.editing || popup.parametres.edition
  }

/// Clavier de l'onglet Paramètres.
///
/// En saisie, le champ capte tout : sinon taper `f` dans le code pays
/// changerait d'onglet et `j`/`k` passeraient pour de la navigation.
fn handle_parametres_input(key: event::KeyEvent, app: &mut App, popup: &mut Popup) -> InputMode {
  if popup.parametres.edition
    {
      edition_pays(key, app, popup);
      return InputMode::Popup(popup.clone());
    }
  match key.code
    {
      KeyCode::Esc => return InputMode::Normal,
      // Tab et Shift+Tab changent d'onglet : c'est le dispatch central qui
      // s'en charge, avant d'arriver ici. `←`/`→` ne servent plus qu'au
      // curseur de « Colonnes visibles », seul écran horizontal du popup.
      KeyCode::Left => deplacer_colonne(popup, false),
      KeyCode::Right => deplacer_colonne(popup, true),
      KeyCode::Up | KeyCode::Char('k') => deplacer_option(popup, -1),
      KeyCode::Down | KeyCode::Char('j') => deplacer_option(popup, 1),
      KeyCode::Enter | KeyCode::Char(' ') => basculer_parametre(app, popup),
      _ => {}
    }
  InputMode::Popup(popup.clone())
}

/// Déplace la ligne active, cycliquement.
fn deplacer_option(popup: &mut Popup, pas: isize)
  {
    let n = PARAMETRES_CHAMPS as isize;
    popup.parametres.actif = (popup.parametres.actif as isize + pas).rem_euclid(n) as usize;
    popup.parametres.message = None;
  }

/// Déplace le curseur dans la liste des colonnes visibles, cycliquement.
fn deplacer_colonne(popup: &mut Popup, avant: bool)
  {
    let n = registry::TOUTES_COLONNES.len();
    let f = popup.parametres.colonne_focus;
    popup.parametres.colonne_focus =
      if avant { (f + n - 1) % n } else { (f + 1) % n };
    popup.parametres.message = None;
  }

/// Saisie du code pays : trois lettres, chiffres et symboles refusés à la
/// frappe pour que la valeur reste valide à tout instant.
fn edition_pays(key: event::KeyEvent, app: &mut App, popup: &mut Popup)
  {
    let longueur = popup.parametres.tampon.chars().count();
    match key.code
      {
        KeyCode::Esc =>
          {
            popup.parametres.edition = false;
            popup.parametres.tampon.clear();
            popup.parametres.curseur = 0;
            popup.parametres.message = None;
          }
        KeyCode::Enter =>
          {
            let code = popup.parametres.tampon.trim().to_string();
            popup.parametres.edition = false;
            popup.parametres.tampon.clear();
            popup.parametres.curseur = 0;
            if code.is_empty()
              {
                popup.parametres.message = Some("Code pays inchangé".to_string());
                return;
              }
            if code.len() != LONGUEUR_PAYS || !code.chars().all(|c| c.is_ascii_alphabetic())
              {
                popup.parametres.message =
                  Some(format!("Code pays invalide : {LONGUEUR_PAYS} lettres (ex. FRA)"));
                return;
              }
            // Le pays conditionne la valorisation : les deux emplacements
            // doivent bouger ensemble, sinon le tarif serait recalculé dans la
            // devise de l'ancien pays.
            app.web_cache.country = code.clone();
            popup.couts.country = code.clone();
            enregistrer_parametres(app, popup, &format!("Pays source : {code}"));
          }
        KeyCode::Backspace | KeyCode::Delete | KeyCode::Left | KeyCode::Right
        | KeyCode::Home | KeyCode::End | KeyCode::PageUp | KeyCode::PageDown =>
          {
            // Curseur et suppressions : les mêmes gestes que partout ailleurs.
            if let Some(touche) = touche_edition(key)
              {
                appliquer_edition(
                  &mut popup.parametres.tampon,
                  &mut popup.parametres.curseur,
                  false,
                  touche,
                  false,
                );
              }
          }
        KeyCode::Char(c) =>
          {
            if longueur >= LONGUEUR_PAYS
              {
                popup.parametres.message =
                  Some(format!("Code pays : {LONGUEUR_PAYS} lettres maximum (ex. FRA)"));
                return;
              }
            if !c.is_ascii_alphabetic()
              {
                popup.parametres.message = Some("Lettres uniquement (ex. FRA)".to_string());
                return;
              }
            if let Some(op_tui::edit::Touche::Caractere(c2)) = touche_edition(key)
              {
                appliquer_edition(
                  &mut popup.parametres.tampon,
                  &mut popup.parametres.curseur,
                  false,
                  op_tui::edit::Touche::Caractere(c2.to_ascii_uppercase()),
                  false,
                );
                popup.parametres.message = None;
              }
          }
        _ => {}
      }
  }

/// Applique la valeur saisie puis l'enregistre. Toute modification part dans le
/// registre : une préférence non écrite serait perdue au prochain lancement.
fn enregistrer_parametres(app: &mut App, popup: &mut Popup, message: &str)
  {
    match registry::save_preferences(&app.registry_path, &app.prefs)
      {
        Ok(()) => popup.parametres.message = Some(message.to_string()),
        Err(e) =>
          {
            popup.parametres.message = Some(format!("Préférence non enregistrée : {e}"));
            app.push_error(format!("Registre non écrit : {e}"));
          }
      }
  }

/// Bascule l'option sous le curseur, ou ouvre sa saisie.
fn basculer_parametre(app: &mut App, popup: &mut Popup)
  {
    match popup.parametres.actif
      {
        OPTION_TRI =>
          {
            app.prefs.tri = match app.prefs.tri
              {
                registry::Tri::Date => registry::Tri::Nom,
                registry::Tri::Nom => registry::Tri::Date,
              };
            appliquer_tri(app, popup);
            let libelle = match app.prefs.tri
              {
                registry::Tri::Date => "Tri : par date",
                registry::Tri::Nom => "Tri : alphabétique",
              };
            enregistrer_parametres(app, popup, libelle);
          }
        OPTION_COLONNES => basculer_colonne(app, popup),
        OPTION_LANCEMENT =>
          {
            app.prefs.launch_root = !app.prefs.launch_root;
            let libelle = if app.prefs.launch_root
              { "Lancement root activé" } else { "Lancement root désactivé" };
            enregistrer_parametres(app, popup, libelle);
          }
        OPTION_CAPTURE =>
          {
            app.prefs.capture = !app.prefs.capture;
            let libelle = if app.prefs.capture
              {
                "Capture du wrapper activée"
              }
            else
              {
                "Capture désactivée : contexte et coûts ne seront pas enregistrés"
              };
            enregistrer_parametres(app, popup, libelle);
          }
        OPTION_PAYS =>
          {
            popup.parametres.edition = true;
            popup.parametres.tampon = app.web_cache.country.clone();
            popup.parametres.curseur = popup.parametres.tampon.chars().count();
            popup.parametres.message = None;
          }
        _ => {}
      }
  }

/// Affiche ou masque la colonne visée, puis la range dans l'ordre canonique.
///
/// L'ordre du registre suit celui de `TOUTES_COLONNES` : sans ce tri, des
/// bascules successives produiraient une liste illisible et, surtout, un
/// registre différent selon l'ordre de manipulation.
fn basculer_colonne(app: &mut App, popup: &mut Popup)
  {
    let libelle = registry::TOUTES_COLONNES[popup.parametres.colonne_focus].to_string();
    let affichee = app.prefs.colonne_visible(&libelle);
    if affichee && app.prefs.colonnes.len() == 1
      {
        popup.parametres.message = Some("Au moins une colonne doit rester visible".to_string());
        return;
      }
    let mut colonnes = app.prefs.colonnes.clone();
    if affichee
      {
        colonnes.retain(|c| c != &libelle);
      }
    else
      {
        colonnes.push(libelle.clone());
        colonnes.sort_by_key(|c| {
          registry::TOUTES_COLONNES
            .iter()
            .position(|t| t == c)
            .unwrap_or(usize::MAX)
        });
      }
    app.prefs.colonnes = colonnes;
    let etat = if affichee { "masquée" } else { "affichée" };
    enregistrer_parametres(app, popup, &format!("Colonne « {libelle} » {etat}"));
  }

/// Réapplique le tri mémorisé à la liste courante.
fn appliquer_tri(app: &mut App, popup: &mut Popup)
  {
    let ordre = match app.prefs.tri
      {
        registry::Tri::Date => SortOrder::Modified,
        registry::Tri::Nom => SortOrder::Name,
      };
    // `projet_idx` est un index : le re-tri le rend caduc, et les onglets
    // Projet puis Coûts ouvriraient un autre projet que celui demandé.
    // L'identifiant est capturé **avant** le tri, c'est lui qui est stable.
    let id = id_a_l_index(app, popup.projet_idx);
    app.sort_order = ordre;
    sort_projects(&mut app.projects, ordre);
    *popup = popup_aligne(app, popup, id.as_deref());
    refresh_project_selection(app);
  }


/// Mode de lancement à utiliser pour `dir`.
///
/// `--continue` n'est proposé que si une session existe déjà : opencode
/// refuse de reprendre une session absente et le lancement échoue. Un
/// premier passage bascule donc sur `LaunchKind::Module` (verrou posé,
/// aucun résumé), qui démarre proprement dans un répertoire vierge.
fn kind_pour_repertoire(dir: &Path, db_path: &Path, bare: bool) -> LaunchKind
  {
    if bare
      {
        LaunchKind::Bare
      }
    else if session_existante(db_path, dir)
      {
        LaunchKind::Resume
      }
    else
      {
        LaunchKind::Module
      }
  }

/// Mode courant, sous forme de libellé court pour les journaux.
fn libelle_mode(mode: &InputMode) -> &'static str
  {
    match mode
      {
        InputMode::Normal => "normal",
        InputMode::ChoixNouveau { .. } => "choix-nouveau",
        InputMode::CreatingProject(_) => "creation",
        InputMode::SelectModule { .. } => "module",
        InputMode::Popup(_) => "popup",
        InputMode::Coherence(_) => "coherence",
        InputMode::Aide { detaillee: true, .. } => "aide-complete",
        InputMode::Aide { detaillee: false, .. } => "aide",
      }
  }

/// Trace des événements, sous `OSC_TRACE_KEYS=1`.
///
/// La valeur, si elle n'est pas vide, désigne le fichier ; sinon c'est
/// `$TMPDIR/opencode-start-menu-touches.log`. Tout ce que crossterm livre au
/// dispatch y est consigné, événements compris : F10 n'arrivait jamais au
/// bras `KeyCode::F(10)` alors que l'octet `^[[21~` part bien du terminal, et
/// un journal vide dira que rien n'est jamais parvenu ici.
fn tracer_evenement(evenement: &Event, mode: &InputMode)
  {
    if env::var_os("OSC_TRACE_KEYS").is_none()
      {
        return;
      }
    let valeur = env::var("OSC_TRACE_KEYS").unwrap_or_default();
    let chemin = if valeur.is_empty()
      {
        env::temp_dir().join("opencode-start-menu-touches.log")
      }
    else
      {
        PathBuf::from(valeur)
      };
    let horodatage = chrono::Local::now().format("%H:%M:%S%.3f");
    let ligne = format!("{horodatage} mode={} {evenement:?}\n", libelle_mode(mode));
    if let Ok(mut fichier) = fs::OpenOptions::new().create(true).append(true).open(chemin)
      {
        let _ = fichier.write_all(ligne.as_bytes());
      }
  }

fn tui_loop(terminal: &mut Term, mut app: App, mode_initial: InputMode) -> Result<()> {
    let mut input_mode = mode_initial;

    loop {
        // États de création et d'édition, extraits de `input_mode` pour le rendu.
        let creation_state = match &input_mode {
            InputMode::CreatingProject(state) => state.clone(),
            _ => ProjectCreationState::default(),
        };
        terminal.draw(|f| ui::ui(f, &mut app, &input_mode, &creation_state))?;

        let evenement = event::read()?;
        tracer_evenement(&evenement, &input_mode);
        if let Event::Key(key) = evenement {
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

            // Bump manuel en attente : la touche décide seule, à la manière
            // du `o/N` du formulaire de création. Une autre touche annule
            // d'abord la demande, puis reste libre d'agir ailleurs — c'est
            // ce qui rend l'index mémorisé inattaquable, aucun rescan ne
            // peut s'intercaler entre les deux frappes.
            if matches!(input_mode, InputMode::Normal) && app.bump_en_attente.is_some()
              {
                match key.code
                  {
                    KeyCode::Char('o') | KeyCode::Char('O') =>
                      {
                        confirmer_bump(&mut app);
                        continue;
                      }
                    KeyCode::Char('n')
                    | KeyCode::Char('N')
                    | KeyCode::Char('q')
                    | KeyCode::Char('Q')
                    | KeyCode::Esc =>
                      {
                        annuler_bump(&mut app);
                        continue;
                      }
                    _ => annuler_bump(&mut app),
                  }
              }

            // F1 ouvre l'aide contextuelle, F10 la liste complète. Les deux
            // sont globaux : l'aide doit être atteignable depuis n'importe
            // quel écran. F10 est écarté pendant une saisie, sinon il
            // volerait un caractère au champ actif.
            // Déjà dans l'aide : ces touches appartiennent à son propre
            // dispatch (fermer, basculer la vue), pas à celui-ci.
            let dans_aide = matches!(input_mode, InputMode::Aide { .. });
            if key.code == KeyCode::F(1) && !dans_aide
              {
                input_mode = ouvrir_aide(input_mode, false);
                continue;
              }
            if key.code == KeyCode::F(10) && !dans_aide && !saisie_en_cours_dans(&input_mode)
              {
                input_mode = ouvrir_aide(input_mode, true);
                continue;
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
                    KeyCode::Char('n') | KeyCode::Char('N') => {
                        InputMode::ChoixNouveau { selected: 0 }
                    }
                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        ouvrir_popup(&mut app, Onglet::Parametres)
                    }
                    KeyCode::Char('b') | KeyCode::Char('B') => {
                        demander_bump(&mut app);
                        InputMode::Normal
                    }
                    KeyCode::Char('c') | KeyCode::Char('C') => ouvrir_coherence(&mut app),
                    // `a` archive dans la liste, restaure dans les archives :
                    // un seul geste, sa direction donnée par la vue.
                    KeyCode::Char('a') => {
                        let resultat = if app.vue_archives {
                            app.restaurer_selection()
                        } else {
                            app.archiver_selection()
                        };
                        match resultat {
                            Ok(msg) => app.push_info(msg),
                            Err(e) => app.push_error(format!("{e}")),
                        }
                        InputMode::Normal
                    }
                    KeyCode::Char('A') => {
                        app.basculer_vue_archives();
                        InputMode::Normal
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
                InputMode::ChoixNouveau { selected } => gerer_choix_nouveau(key.code, selected),
                InputMode::CreatingProject(mut state) => {
                    match handle_creation_input(key, &mut state, &mut app) {
                        Ok(mode) => mode,
                        Err(e) => {
                            app.push_error(format!("Création impossible : {e}"));
                            InputMode::Normal
                        }
                    }
                }
                InputMode::Popup(mut popup) => 'onglet: {
                    // Tab et Shift+Tab changent d'onglet partout, quelle que
                    // soit la position : c'est le seul geste qui doit marcher
                    // dans les trois onglets. La touche est consommée, sans
                    // reacher le handler de l'onglet vers lequel elle vient
                    // de basculer : une touche, un effet.
                    if naviguer_onglets(&mut popup, key.code)
                      {
                        break 'onglet InputMode::Popup(popup);
                      }
                    // Onglet Coûts : réglage du tarif tant qu'aucune saisie
                    // n'est en cours, sinon défilement du tableau des coûts.
                    if popup.onglet == Onglet::Couts && !popup.couts.editing {
                        let scroll = &mut popup.couts.scroll_offset;
                        match key.code {
                            KeyCode::Up | KeyCode::Char('k') =>
                                *scroll = op_tui::viewport::decaler(*scroll, -1),
                            KeyCode::Down | KeyCode::Char('j') =>
                                *scroll = op_tui::viewport::decaler(*scroll, 1),
                            KeyCode::PageUp =>
                                *scroll = op_tui::viewport::decaler(*scroll, -10),
                            KeyCode::PageDown =>
                                *scroll = op_tui::viewport::decaler(*scroll, 10),
                            KeyCode::Home => *scroll = 0,
                            // `Fin` va au bout : le rendu bornera l'affichage.
                            KeyCode::End => *scroll = op_tui::viewport::FIN,
                            _ => {}
                        }
                    }
                    match popup.onglet {
                        Onglet::Projet => {
                            let mut state = popup.projet.clone();
                            let resultat = handle_edit_input(key, &mut state, &mut app, &popup);
                            match resultat {
                                Ok(mode) => mode,
                                Err(e) => {
                                    app.push_error(format!("Édition impossible : {e}"));
                                    InputMode::Normal
                                }
                            }
                        }
                        Onglet::Couts => match handle_tariff_input(
                            key,
                            &mut app,
                            &popup,
                            popup.projet_idx,
                            popup.couts.active_field,
                            popup.couts.editing,
                            popup.couts.input_buffer.clone(),
                            popup.couts.input_offset,
                            popup.couts.country.clone(),
                            popup.couts.message.clone(),
                            popup.couts.fetching,
                        ) {
                            Ok(mode) => mode,
                            Err(e) => {
                                app.push_error(format!("Valorisation : {e}"));
                                InputMode::Normal
                            }
                        },
                        Onglet::Parametres => handle_parametres_input(key, &mut app, &mut popup),
                    }
                }
                // Modale de cohérence : appliquer, refuser ou fermer.
                InputMode::Coherence(mut state) => {
                    match handle_coherence_input(key, &mut state, &mut app) {
                        Ok(mode) => mode,
                        Err(e) => {
                            app.push_error(format!("Cohérence : {e}"));
                            InputMode::Normal
                        }
                    }
                }
                // Échap et F1 referment l'aide. F10 y bascule vers la vue
                // complète, ce qui évite d'avoir à refermer pour y aller.
                InputMode::Aide { detaillee, origine, scroll } => {
                    let suivant = |detaillee: bool, scroll: usize| InputMode::Aide {
                        detaillee,
                        origine: origine.clone(),
                        scroll,
                    };
                    match key.code {
                        // Revenir à l'écran d'origine, formulaire intact.
                        KeyCode::Esc | KeyCode::F(1) => *origine,
                        // Changer de vue repart du haut : l'autre liste n'a pas
                        // les mêmes lignes.
                        KeyCode::F(10) => suivant(!detaillee, 0),
                        KeyCode::Up | KeyCode::Char('k') => {
                            suivant(detaillee, op_tui::viewport::decaler(scroll, -1))
                        }
                        KeyCode::Down | KeyCode::Char('j') =>
                            suivant(detaillee, op_tui::viewport::decaler(scroll, 1)),
                        KeyCode::PageUp =>
                            suivant(detaillee, op_tui::viewport::decaler(scroll, -10)),
                        KeyCode::PageDown =>
                            suivant(detaillee, op_tui::viewport::decaler(scroll, 10)),
                        KeyCode::Home => suivant(detaillee, 0),
                        // Le rendu borne le défilement à la dernière ligne.
                        KeyCode::End => suivant(detaillee, op_tui::viewport::FIN),
                        _ => suivant(detaillee, scroll),
                    }
                }
            };
        }

        // Handle pending launch: suspend TUI, run child, resume TUI
        if let Some(dir) = app.pending_launch.take() {
            let sudo = app.pending_sudo;
            let name = app.pending_name.take()
                .unwrap_or_else(|| dir.display().to_string());

            // Comptabilité : figer la classe de la session dès maintenant.
            // `app.projects` est à jour : la création recente sa propre
            // entrée avant de poser `pending_launch`.
            let bare = app.pending_bare;
            compta::enregistrer_lancement(
                &compta::chemin_table(&app.registry_path),
                &compta::classer_repertoire(&app.projects, &dir, bare),
            );
            app.pending_bare = false;
            // Constat de départ de la comptabilisation (best-effort).
            apport::demarrer(&app.registry_path, &app.projects, &dir);

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
            let kind = kind_pour_repertoire(&dir, &app.opencode_db_path, bare);
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
            let rescan_err = if let Err(e) = rescan_repertoire(&mut app, &mut Progress::noop()) {
                Some(format!("Rescan post-session échoué : {e}"))
            } else {
                None
            };
            // Retour au menu : contrôle de cohérence systématique (C5).
            signaler_coherence(&mut app);
            // Constat de fin : la session passée est auditée (best-effort).
            if let Some(resume) = apport::constater_retour(
                &app.registry_path,
                &app.opencode_db_path,
                &app.projects,
                &dir,
                bare,
            )
            .ok()
            .flatten()
              {
                app.push_info(resume);
              }
            if let Some(e) = rescan_err {
                app.push_error(e);
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
    state.premiere_touche = true;
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
    if let Some(root) = projects_root(&app.projects, &app.archive_root)
      {
        // Mémorisé pour la restauration : si un jour tous les projets sont
        // archivés, plus rien ne dira plus où les ramener.
        app.projets_root = root.clone();
        match registry::reindex_from_disk(&root, &app.prefs.lib_root_resolu(), &mut app.projects)
          {
            Ok(n) => added = n,
            Err(e) => alertes.push(format!("Indexation impossible : {e}")),
          }
        registry::deriver_bibliotheques(&mut app.projects, &app.prefs.lib_root_resolu());
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
                // Le manifeste fait foi : la version relue du disque est une
                // mutation « vérité du disque », déclarée à la garde.
                registry::accepter_modification(format!("{}:version", entry.id));
                entry.version = v.clone();
                // La variante Debug n'est resynchronisée que si elle est vide,
                // pour ne pas écraser une version éditée manuellement.
                if entry.version_debug.is_empty()
                  {
                    registry::accepter_modification(format!("{}:version_debug", entry.id));
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
                // Montant équivalent en devise locale, pour la colonne
                // « Converti » et les totaux. Il manque tant qu'aucun taux n'est
                // connu pour cette devise, et il ne vaut pas « 0,00 € » quand le
                // fournisseur ne publie aucun tarif — les deux cas sont traités
                // par `montant_local`, qui renvoie `None` dans les deux.
                entry.last_session_cost_local =
                  match registry::montant_local(
                    ls.cost,
                    &ls.currency,
                    ls.exchange_rate,
                    ls.cost_local,
                  ) {
                    Some(local) if !devise_est_locale(&ls.currency) =>
                      format_currency(local, LOCAL_CURRENCY),
                    _ => String::new(),
                  };
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
                entry.last_session_cost_local = String::new();
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
            entry.last_session_cost_local = String::new();
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
            // La modification du source vaut mise à jour des constantes : quand
            // le module principal en porte, `DATE_LAST_MAJ` prend la date de la
            // dernière modification du source (le fichier, pas le jour de
            // détection — un rescan tardif ne doit pas antidater une modif
            // ancienne). Les langages sans module Rust relèvent d'opencode.
            let date_maj = registry::source_modified_time(&entry.path)
                .map(registry::date_jj_mm_yyyy)
                .unwrap_or_else(registry::date_du_jour);
            let version_source = if entry.version_debug.is_empty()
              {
                entry.version.clone()
              }
            else
              {
                entry.version_debug.clone()
              };
            if !version_source.is_empty()
                && conformite::ecrire_constantes(
                    &entry.path,
                    &version_source,
                    &entry.status_debug,
                    &date_maj,
                )
                .unwrap_or(false)
              {
                registry::accepter_modification(format!("{}:date_debug", entry.id));
                entry.date_debug = date_maj.clone();
                // Le hash suit la source réécrite : sans ce recalcul, le rescan
                // suivant y verrait une modification de plus et rebumperait.
                entry.source_hash = registry::compute_source_hash(&entry.path);
            }
            // Constat documenté : une nouvelle compilation est détectée sur le
            // disque, la référence de compilation et le snapshot Debug suivent
            // le binaire (sa vérité). Le snapshot fige la version DEJA en
            // vigueur à cet instant — si le code redemande un bump ci-dessous,
            // elle reflète le dernier état réellement construit.
            let t_debug = registry::variant_modified_time(&entry.path, "debug")
              .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()));
            if let Some(t) = t_debug {
                registry::accepter_modification(format!("{}:last_compile_debug_secs", entry.id));
                registry::accepter_modification(format!("{}:debug_snapshot", entry.id));
                entry.last_compile_debug_secs = t;
                let version_snapshot =
                    if entry.version_debug.is_empty() { entry.version.clone() } else { entry.version_debug.clone() };
                // Date et empreinte : complément issu du binaire (sonde `-V`).
                // La version/statut conservent leur sémantique d'avant-bump.
                let (date, empreinte_v) = match variante::lire_variante_avec_sonde(
                    &entry.path,
                    "debug",
                    entry.sonde("debug"),
                )
                  {
                    Some(l) => (l.etat.date, l.empreinte),
                    None => (String::new(), String::new()),
                  };
                // La date déclarée de la variante suit le binaire sondé : le
                // rattrapage est la seule écriture silencieuse autorisée.
                if !date.is_empty() {
                    registry::accepter_modification(format!("{}:date_debug", entry.id));
                    entry.date_debug = date.clone();
                }
                entry.debug_snapshot = Some(registry::SnapshotCompile {
                    version: version_snapshot,
                    statut: entry.status_debug.clone(),
                    date,
                    compile_secs: t,
                    empreinte_v,
                });
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
                        // Bump AutoPatch : mutation déclarée à la garde.
                        registry::accepter_modification(format!("{}:version_debug", entry.id));
                        registry::accepter_modification(format!("{}:version", entry.id));
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
        let t_release_syst = registry::variant_modified_time(&entry.path, "release");
        let t_release = t_release_syst.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs()));
        if let Some(t) = t_release
          && t > entry.last_compile_release_secs
        {
            // Exiger un binaire au moins aussi récent que les sources : un
            // binaire Release retombé sur un code plus vieux (checkout,
            // récupération) ne doit pas être pris pour une reconstruction.
            // Sinon : alerte, aucune écriture — la fiche reste l'ancienne.
            let sources = registry::source_modified_time(&entry.path);
            let binaire_apres_sources = sources.is_none_or(|s| t_release_syst.is_some_and(|b| b >= s));
            if binaire_apres_sources {
                // Constat documenté d'un build release validé : la référence,
                // les métadonnées de la variante et son snapshot suivent.
                registry::accepter_modification(format!("{}:last_compile_release_secs", entry.id));
                registry::accepter_modification(format!("{}:version_release", entry.id));
                registry::accepter_modification(format!("{}:status_release", entry.id));
                registry::accepter_modification(format!("{}:release_snapshot", entry.id));
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
                    entry.version_release = v.clone();
                    entry.status_release = entry.status_debug.clone();
                    // Date et empreinte de la Release reconstruite : le binaire
                    // release vient d'être sonde, il déclare sa propre vérité.
                    let (date, empreinte_v) = match variante::lire_variante_avec_sonde(
                        &entry.path,
                        "release",
                        entry.sonde("release"),
                    )
                      {
                        Some(l) => (l.etat.date, l.empreinte),
                        None => (String::new(), String::new()),
                      };
                    if !date.is_empty() {
                        registry::accepter_modification(format!("{}:date_release", entry.id));
                        entry.date_release = date.clone();
                    }
                    entry.release_snapshot = Some(registry::SnapshotCompile {
                        version: v,
                        statut: entry.status_debug.clone(),
                        date,
                        compile_secs: t,
                        empreinte_v,
                    });
                  }
                written = true;
            } else {
                alertes.push(format!(
                  "{} : binaire Release plus ancien que les sources, rattrapage ignoré",
                  entry.name
                ));
            }
          }
        // Cible sans code : quand aucun binaire `-V` ne répond (projet sans
        // source compilée, ou langage non couvert par la sonde), la date de
        // base suit le mtime de la cible `target/<profil>`. Le binaire fait
        // toujours foi quand il existe : on ne retombe sur le mtime qu'à
        // défaut. Les deux champs epoch bornent la réécriture (anti-boucle).
        maj_date_cible(entry, "debug");
        maj_date_cible(entry, "release");
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
        let alertes_garde = registry::save_registry_full(&app.registry_path, &app.projects, &app.web_cache, &app.prefs)?;
        alertes.extend(alertes_garde);
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

/// La date de base d'une variante suit la cible `target/<profil>` quand aucun
/// binaire `-V` n'y répond : dernier recours. Le binaire contractuel fait
/// toujours foi.
fn maj_date_cible(entry: &mut ProjectEntry, profil: &str)
  {
    // Un binaire sondable répond : vérité contractuelle, on ne touche pas à la date.
    if variante::lire_variante(&entry.path, profil, entry.sonde(profil)).is_some() {
        return;
    }
    let Some(mtime) = registry::variant_modified_time(&entry.path, profil) else {
        return;
    };
    let Ok(duree) = mtime.duration_since(std::time::UNIX_EPOCH) else {
        return;
    };
    let secs = duree.as_secs();
    let date = registry::date_jj_mm_yyyy(mtime);
    if profil == "release" {
        if secs <= entry.last_target_release_secs && date == entry.date_release {
            return;
        }
        registry::accepter_modification(format!("{}:date_release", entry.id));
        registry::accepter_modification(format!("{}:last_target_release_secs", entry.id));
        entry.date_release = date;
        entry.last_target_release_secs = secs;
    } else {
        if secs <= entry.last_target_debug_secs && date == entry.date_debug {
            return;
        }
        registry::accepter_modification(format!("{}:date_debug", entry.id));
        registry::accepter_modification(format!("{}:last_target_debug_secs", entry.id));
        entry.date_debug = date;
        entry.last_target_debug_secs = secs;
    }
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
      e.last_target_debug_secs.to_string(),
      e.last_target_release_secs.to_string(),
      e.version_manually_edited.to_string(),
      e.typologie.clone(),
      e.date_debug.clone(),
      e.date_release.clone(),
      e.probe_debug.clone(),
      e.probe_release.clone(),
      e.build_debug.clone(),
      e.build_release.clone(),
      signature_fiche(e.debug_snapshot.as_ref()),
      signature_fiche(e.release_snapshot.as_ref()),
    ]
    .join("|")
  }

/// Signature courte d'une fiche d'état (snapshot), pour `signature_persistee`.
/// Renvoie un vide si la fiche est absente.
fn signature_fiche(fiche: Option<&registry::SnapshotCompile>) -> String
  {
    match fiche
      {
        Some(f) => format!(
          "{};{};{};{};{}",
          f.version, f.statut, f.date, f.compile_secs, f.empreinte_v
        ),
        None => String::new(),
      }
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
            SortOrder::Modified => CleTri::Modifie(registry::cible_modified_time(&p.path)),
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

/// Nom de dossier d'un projet, pour construire la destination d'un
/// déplacement. Le nom affiché peut avoir été renommé dans le menu sans que
/// le dossier le soit : c'est le dossier qui doit partir, pas l'affichage.
fn nom_de_dossier(p: &ProjectEntry) -> String {
    p.path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| p.name.clone())
}

/// Recalcule le filtrage et la sélection sans retrier.
///
/// `rescan_repertoire` trie déjà la liste ; le startup n'a plus qu'à
/// réinitialiser la sélection. Retrier ici reparcourrait `target/` de tous les
/// projets une seconde fois pour un résultat identique, puisque rien n'a
/// modifié les dates de modification.
///
/// Le filtrage distingue aussi les deux vues : un projet archivé vit dans
/// `archive_root` et n'apparaît plus qu'entre les murs de `A`.
fn refresh_project_selection(app: &mut App) {
    app.filtered = (0..app.projects.len())
        .filter(|&i| app.est_archive(i) == app.vue_archives)
        .collect();
    if app.filtered.is_empty() {
        app.list_state.select(None);
        return;
    }
    // La sélection initiale vise un index de `projects` : il faut la
    // reporter sur la position occupée dans la liste réellement affichée.
    let voulu = initial_selection_index(app.sort_order, app.projects.len());
    let position = app
        .filtered
        .iter()
        .position(|&i| i == voulu)
        .unwrap_or(0);
    app.list_state.select(Some(position));
}

fn projects_root(projects: &[ProjectEntry], archive_root: &Path) -> Option<PathBuf> {
    projects_root_actifs(projects, archive_root)
        .or_else(|| dirs::home_dir().map(|h| h.join("projets")))
}

/// Racine retenue à partir des projets encore actifs, sans repli de
/// confort : `None` quand il n'en reste plus qu'aux archives. C'est ce
/// `None` qui, à la restauration, rend la parole au dernier rescan mémorisé
/// plutôt qu'au répertoire personnel.
fn projects_root_actifs(projects: &[ProjectEntry], archive_root: &Path) -> Option<PathBuf> {
    // Les archives ne doivent pas entrer dans le décompte : leur nombre
    // ferait basculer la racine vers `archive_root`, et le rescan y
    // réindexerait alors de « nouveaux » projets ceux qui y dorment.
    let actifs = projects.iter().filter(|p| !p.path.starts_with(archive_root));
    let mut counts = std::collections::HashMap::<PathBuf, usize>::new();
    for p in actifs {
        if let Some(parent) = p.path.parent() {
            *counts.entry(parent.to_path_buf()).or_insert(0) += 1;
        }
    }
    counts.into_iter().max_by_key(|&(_, c)| c).map(|(dir, _)| dir)
}

// Liste du menu déroulant statut en création : « aucun » + éligibles + « nouveau ».
pub(crate) fn status_menu_creation_items(filter: &str) -> Vec<&'static str>
  {
    let mut items = vec!["<Aucun>"];
    items.extend(filtered_statuses(filter));
    items.push("+ Nouveau statut");
    items
  }

fn gerer_choix_nouveau(code: KeyCode, selected: usize) -> InputMode {
    match code {
        // ↑/↓ parcourent Projet / Bibliothèque ; pas de boucle : les bornes
        // sont nettes et suffisent pour deux entrées.
        KeyCode::Up | KeyCode::Char('k') if selected > 0 => {
            InputMode::ChoixNouveau { selected: selected - 1 }
        }
        KeyCode::Down | KeyCode::Char('j') if selected < 1 => {
            InputMode::ChoixNouveau { selected: selected + 1 }
        }
        KeyCode::Enter => InputMode::CreatingProject(ProjectCreationState {
            bibliotheque: selected == 1,
            ..Default::default()
        }),
        KeyCode::Esc => InputMode::Normal,
        // Désignation directe, sans navigation.
        KeyCode::Char('p') | KeyCode::Char('P') => {
            InputMode::CreatingProject(ProjectCreationState::default())
        }
        KeyCode::Char('b') | KeyCode::Char('B') => {
            InputMode::CreatingProject(ProjectCreationState {
                bibliotheque: true,
                ..Default::default()
            })
        }
        _ => InputMode::ChoixNouveau { selected },
    }
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
        KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
        | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Backspace
        | KeyCode::Delete =>
          {
            // Le menu déroulant garde son propre jeu de touches : seul le
            // filtre y change, la navigation se fait aux flèches.
            if state.status_menu_open
              {
                if key.code == KeyCode::Backspace
                  {
                    state.status_filter.pop();
                    let items = status_menu_creation_items(&state.status_filter);
                    if state.status_cursor >= items.len()
                      {
                        state.status_cursor = items.len().saturating_sub(1);
                      }
                  }
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            if let Some(touche) = touche_edition(key)
              {
                let masque = state.active_field == 5;
                appliquer_edition(
                  &mut state.input_buffer,
                  &mut state.input_offset,
                  state.premiere_touche,
                  touche,
                  masque,
                );
                state.premiere_touche = false;
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
                    state.id = id.clone();
                    // Une bibliothèque tombe sous la racine dédiée : le champ
                    // Chemin s'en prédéclare, éditable comme un projet.
                    if state.bibliotheque && state.path.is_empty()
                      {
                        state.path =
                          format!("{}/{}", app.prefs.lib_root.trim_end_matches('/'), state.id);
                      }
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
                        if state.bibliotheque
                          {
                            app.prefs.lib_root_resolu().join(&state.id)
                          }
                        else
                          {
                            dirs::home_dir()
                              .unwrap_or_else(|| PathBuf::from("."))
                              .join("projets")
                              .join(&state.id)
                          }
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
                        last_session_cost_local: String::new(),
                        last_agent: String::new(),
                        last_time_created: String::new(),
                        last_session_context_pct: String::new(),
                        status_debug: status.clone(),
                        version_debug: version.clone(),
                        // Un projet neuf n'a encore buildé aucune Release : les
                        // champs restent vides, l'écran affiche « — ». La date
                        // Release hérite de la date Debug (« jamais buildée »).
                        status_release: String::new(),
                        version_release: String::new(),
                        date_debug: registry::date_du_jour(),
                        date_release: registry::date_du_jour(),
                        probe_debug: String::new(),
                        probe_release: String::new(),
                        build_debug: String::new(),
                        build_release: String::new(),
                        debug_modified: "jamais compilée".to_string(),
                        release_modified: "jamais compilée".to_string(),
                        last_compile_debug_secs: 0,
                        last_compile_release_secs: 0,
                        last_target_debug_secs: 0,
                        last_target_release_secs: 0,
                        debug_snapshot: None,
                        release_snapshot: None,
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
                        est_bibliotheque: state.bibliotheque,
                        typologie: registry::typologie_auto(&dir).to_string(),
                    });
                    // Création de projet : la fiche naît du menu, toutes ses
                    // métadonnées sont déclarées d'emblée à la garde.
                    for champ in [
                      "status",
                      "version",
                      "status_debug",
                      "version_debug",
                      "status_release",
                      "version_release",
                      "date_debug",
                      "date_release",
                      "last_compile_debug_secs",
                      "last_compile_release_secs",
                      "debug_snapshot",
                      "release_snapshot",
                    ] {
                      registry::accepter_modification(format!("{}:{champ}", state.id));
                    }
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
                        state.input_offset = 0;
                        state.confirm_status = false;
                        state.status_free_text = true;
                        state.active_field = 4;
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
                  }
                return Ok(InputMode::CreatingProject(state.clone()));
              }
            // Édition du champ courant : le masque x.y.z de la Version est
            // appliqué par `appliquer_edition`, qui le signale par `masque`.
            if let Some(touche) = touche_edition(key)
              {
                let masque = state.active_field == 5;
                appliquer_edition(
                  &mut state.input_buffer,
                  &mut state.input_offset,
                  state.premiere_touche,
                  touche,
                  masque,
                );
                state.premiere_touche = false;
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
        2 => state.outils.probe_debug.clone(),
        3 => state.outils.build_debug.clone(),
        4 => state.outils.probe_release.clone(),
        5 => state.outils.build_release.clone(),
        6 => state.pending_description.clone(),
        _ => String::new(),
      };
    state.input_offset = state.input_buffer.chars().count();
    state.premiere_touche = true;
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
            state.outils.probe_debug = value;
            true
        }
        3 => {
            state.outils.build_debug = value;
            true
        }
        4 => {
            state.outils.probe_release = value;
            true
        }
        5 => {
            state.outils.build_release = value;
            true
        }
        6 if !value.is_empty() => {
            state.pending_description = value;
            true
        }
        6 => false,
        _ => false,
    };
    if ok {
        state.error = None;
        state.editing = false;
        if state.active_field < EDIT_CONFIRM_FIELD {
            state.active_field += 1;
        }
    } else {
        state.error = Some(match state.active_field {
            0 => "Version invalide : format attendu x.y.z (ex. 1.2.3)".to_string(),
            1 => format!("Statut invalide : attendu l'un de {}.", registry::STATUSES.join(" · ")),
            6 => "Description invalide : texte non vide requis".to_string(),
            _ => String::new(),
        });
    }
}

/// Valide l'ensemble des champs et applique les modifications au projet.
///
/// Seule la variante Debug reçoit version et statut : la Release garde les
/// siens de dernière compilation tant que son binaire n'est pas reconstruit
/// (rattrapage au prochain rescan). Les commandes d'outils (sonde, build) sont
/// en revanche enregistrées pour les deux variantes.
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
    // Édition utilisateur explicite (popup nouveau) : mutation déclarée.
    let ancienne_version = p.version_debug.clone();
    registry::accepter_modification(format!("{}:version_debug", p.id));
    registry::accepter_modification(format!("{}:status_debug", p.id));
    registry::accepter_modification(format!("{}:version", p.id));
    registry::accepter_modification(format!("{}:status", p.id));
    registry::accepter_modification(format!("{}:date_debug", p.id));
    registry::accepter_modification(format!("{}:typologie", p.id));
    p.version_debug = state.pending_version.clone();
    p.status_debug = state.pending_status.clone();
    p.description = state.pending_description.clone();
    if registry::validate_typologie(&state.pending_typologie)
      {
        p.typologie = state.pending_typologie.clone();
      }
    // Commandes d'outils par variante : simples réglages, non protégés par la
    // garde des métadonnées de vérité. Vide = auto (sonde `target/<profil>`,
    // build Cargo).
    p.probe_debug = state.outils.probe_debug.trim().to_string();
    p.build_debug = state.outils.build_debug.trim().to_string();
    p.probe_release = state.outils.probe_release.trim().to_string();
    p.build_release = state.outils.build_release.trim().to_string();
    // Sans miroir : la Release garde sa version/statut tant que son
    // binaire n'est pas reconstruit (rattrapage au prochain rescan).
    // Champs historiques alignés sur la variante élue (Debug).
    p.version = p.version_debug.clone();
    p.status = p.status_debug.clone();
    // L'édition écrit la SOURCE et rien d'autre : constantes du contrat dans
    // le module principal, manifeste Cargo, date du jour. Aucun rebuild — le
    // binaire suivra à la compilation suivante, la source déclarée fait foi.
    if registry::validate_version(&p.version_debug) {
        let date = registry::date_du_jour();
        let _ = set_manifest_version(&p.path, &p.version_debug);
        // Un projet sans module Rust (ou non inscriptible) conserve son
        // édition en base : l'écart sera exposé par la conformité. La date de
        // base ne suit que si le module a réellement été réécrit.
        if conformite::ecrire_constantes(
            &p.path,
            &p.version_debug,
            &p.status_debug,
            &date,
        )
        .unwrap_or(false)
        {
            p.date_debug = date;
        }
        // Le hash suit la source réécrite : sans cela, le rescan y verrait
        // une modification légitime et rebumperait à la session suivante.
        p.source_hash = registry::compute_source_hash(&p.path);
        // Le drapeau « édité manuellement » supprime le prochain bump
        // AutoPatch : il ne se lève que si la version a réellement changé,
        // sinon il avalerait le prochain vrai bump.
        if p.version_debug != ancienne_version {
            p.version_manually_edited = true;
        }
    }
    true
}

const EDIT_CONFIRM_FIELD: usize = 8;

/// Nombre de champs éditables du popup de valorisation :
/// 0 = mode, 1 = tarif manuel, 2 = pays source.
const TARIF_CHAMPS: usize = 3;

/// Gestionnaire clavier du popup « Valorisation du temps » (touche `t`).
#[allow(clippy::too_many_arguments)]
fn handle_tariff_input(
  key: event::KeyEvent,
  app: &mut App,
  base: &Popup,
  project_idx: usize,
  active_field: usize,
  editing: bool,
  mut input_buffer: String,
  mut input_offset: usize,
  country: String,
  mut message: Option<String>,
  _fetching: bool,
) -> Result<InputMode>
   {
    // Copie modifiable du popup reçu. Les deux sites qui déclenchent un rescan
    // doivent réaligner son `projet_idx`, sans muter l'état du dispatcheur.
    //
    // Elle est déclarée *avant* la macro et utilisée sous le nom `popup_base` :
    // `macro_rules!` résout ses identifiants libres sur le site de définition,
    // donc un simple masquage de `base` resterait invisible depuis `popup!`.
    let mut popup_base = base.clone();

    // Reconstruit l'onglet Coûts à partir des variables locales du corps, puis
    // le rend dans le popup reçu : les deux autres onglets gardent leur état.
    macro_rules! popup
      {
        ($editing:expr, $buffer:expr, $offset:expr, $msg:expr) =>
          {
            InputMode::Popup(popup_base.avec_couts(EtatCouts {
              active_field,
              editing: $editing,
              input_buffer: $buffer,
              input_offset: $offset,
              country,
              message: $msg,
              fetching: popup_base.couts.fetching,
              scroll_offset: popup_base.couts.scroll_offset,
            }))
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
                  && let Some(op_tui::edit::Touche::Caractere(c2)) = touche_edition(key)
                  {
                    let c2 = if active_field == 2
                      {
                        c2.to_ascii_uppercase()
                      }
                    else
                      {
                        c2
                      };
                    appliquer_edition(
                      &mut input_buffer,
                      &mut input_offset,
                      false,
                      op_tui::edit::Touche::Caractere(c2),
                      false,
                    );
                    message = None;
                  }
                return Ok(popup!(true, input_buffer, input_offset, message));
              }
            KeyCode::Enter => {}
            // Curseur et suppressions : les mêmes gestes que partout ailleurs.
            // Ces saisies s'ouvrent sur une valeur déjà en place, mais rien à
            // remplacer d'un seul coup : la première touche est normale.
            _ =>
              {
                if let Some(touche) = touche_edition(key)
                  {
                    appliquer_edition(&mut input_buffer, &mut input_offset, false, touche, false);
                  }
                return Ok(popup!(true, input_buffer, input_offset, message));
              }
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
                        // peut re-trier la liste : l'identifiant est capturé
                        // avant, le popup est réaligné après.
                        let id = id_a_l_index(app, project_idx);
                        let _ = rescan_repertoire(app, &mut Progress::noop());
                        popup_base = popup_aligne(app, &popup_base, id.as_deref());
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
        KeyCode::Esc | KeyCode::Char('q') => Ok(InputMode::Normal),
        // `←`/`→` passent d'un champ tarifaire à l'autre. `↑`/`↓` ne le font
        // pas : le dispatch central s'en sert pour faire défiler le tableau,
        // et une touche ne doit pas avoir deux effets. Tab et Shift+Tab
        // changent d'onglet, comme dans les deux autres onglets.
        KeyCode::Left =>
          {
            let next = if active_field == 0 { TARIF_CHAMPS - 1 } else { active_field - 1 };
            // `popup_base` et non `base` : la copie alignée après rescan. Ces
            // deux bras n'emploient pas `popup!` parce qu'ils déplacent le champ
            // actif, que la macro prend tel quel dans ses variables locales.
            Ok(InputMode::Popup(popup_base.avec_couts(EtatCouts {
              active_field: next,
              editing: false,
              input_buffer: String::new(),
              input_offset: 0,
              country,
              message,
              fetching: popup_base.couts.fetching,
              scroll_offset: popup_base.couts.scroll_offset,
            })))
          }
        KeyCode::Right =>
          {
            let next = (active_field + 1) % TARIF_CHAMPS;
            Ok(InputMode::Popup(popup_base.avec_couts(EtatCouts {
              active_field: next,
              editing: false,
              input_buffer: String::new(),
              input_offset: 0,
              country,
              message,
              fetching: popup_base.couts.fetching,
              scroll_offset: popup_base.couts.scroll_offset,
            })))
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
                let id = id_a_l_index(app, project_idx);
                let _ = rescan_repertoire(app, &mut Progress::noop());
                popup_base = popup_aligne(app, &popup_base, id.as_deref());
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
            Ok(popup!(true, buffer, offset, message))
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
            Ok(popup!(false, String::new(), 0, message))
          }
        _ => Ok(popup!(false, String::new(), 0, message)),
      }
   }

fn handle_edit_input(
  key: event::KeyEvent,
  state: &mut ProjectEditState,
  app: &mut App,
  base: &Popup,
) -> Result<InputMode>
   {
    if state.editing
      {
        match key.code
          {
            KeyCode::Esc =>
              {
                state.editing = false;
                return Ok(InputMode::Popup(base.avec_projet(state.clone())));
              }
            KeyCode::Enter =>
              {
                commit_edit_field(state);
                return Ok(InputMode::Popup(base.avec_projet(state.clone())));
              }
            // Tout le reste relève du champ en cours : le composant d'édition
            // porte les gestes, le masque x.y.z protège la Version.
            _ =>
              {
                if let Some(touche) = touche_edition(key)
                  {
                    let masque = state.active_field == 0;
                    appliquer_edition(
                      &mut state.input_buffer,
                      &mut state.input_offset,
                      state.premiere_touche,
                      touche,
                      masque,
                    );
                    state.premiere_touche = false;
                    if matches!(touche, op_tui::edit::Touche::Caractere(_))
                      {
                        state.error = None;
                      }
                    return Ok(InputMode::Popup(base.avec_projet(state.clone())));
                  }
              }
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
                    return Ok(InputMode::Popup(base.avec_projet(state.clone())));
                  }
                if state.typologie_menu_open
                  {
                    state.typologie_menu_open = false;
                    return Ok(InputMode::Popup(base.avec_projet(state.clone())));
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
                    return Ok(InputMode::Popup(base.avec_projet(state.clone())));
                  }
                if state.typologie_menu_open
                  {
                    let items = typologies_liste();
                    if key.code == KeyCode::Down && state.typologie_cursor + 1 < items.len()
                      {
                        state.typologie_cursor += 1;
                      }
                    else if key.code == KeyCode::Up && state.typologie_cursor > 0
                      {
                        state.typologie_cursor -= 1;
                      }
                    return Ok(InputMode::Popup(base.avec_projet(state.clone())));
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
                return Ok(InputMode::Popup(base.avec_projet(state.clone())));
              }
            // Sur « Enregistrer ? », les flèches horizontales choisissent la
            // réponse : ← vise `[Oui]`, → vise `[Non]`. C'est le choix
            // surligné qu'Entrée exécutera.
            KeyCode::Left | KeyCode::Right if state.active_field == EDIT_CONFIRM_FIELD =>
              {
                state.choix_oui = key.code == KeyCode::Left;
                return Ok(InputMode::Popup(base.avec_projet(state.clone())));
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
                        if state.active_field < EDIT_CONFIRM_FIELD
                          {
                            state.active_field += 1;
                          }
                      }
                    return Ok(InputMode::Popup(base.avec_projet(state.clone())));
                  }
                if state.typologie_menu_open
                  {
                    let items = typologies_liste();
                    if let Some(sel) = items.get(state.typologie_cursor).copied()
                      {
                        state.pending_typologie = sel.to_string();
                        state.typologie_menu_open = false;
                        state.error = None;
                        if state.active_field < EDIT_CONFIRM_FIELD
                          {
                            state.active_field += 1;
                          }
                      }
                    return Ok(InputMode::Popup(base.avec_projet(state.clone())));
                  }
                if state.active_field == 1
                  {
                    // Champ Statut : ouverture du menu déroulant.
                    state.status_menu_open = true;
                    state.status_filter.clear();
                    state.status_cursor = 0;
                    return Ok(InputMode::Popup(base.avec_projet(state.clone())));
                  }
                if state.active_field == 7
                  {
                    // Champ Typologie : ouverture du menu de choix.
                    state.typologie_menu_open = true;
                    state.typologie_cursor = typologies_liste()
                      .iter()
                      .position(|t| *t == state.pending_typologie)
                      .unwrap_or(0);
                    return Ok(InputMode::Popup(base.avec_projet(state.clone())));
                  }
                if state.active_field < EDIT_CONFIRM_FIELD
                  {
                    state.editing = true;
                    load_edit_field(state);
                  }
                else if !state.choix_oui
                  {
                    // Choix surligné `[Non]` : fermer sans rien écrire, à
                    // l'instar d'Échap. C'est le défaut, pour qu'une Entrée
                    // posée au hasard ne dépose jamais de variante.
                    return Ok(InputMode::Normal);
                  }
                else if apply_edit(app, state)
                  {
                    let alertes = registry::save_registry_full(&app.registry_path, &app.projects, &app.web_cache, &app.prefs)?;
                    let mut msg = "Projet mis à jour : variante Debug enregistrée".to_string();
                    if !alertes.is_empty()
                      {
                        msg.push_str("  ·  ");
                        msg.push_str(&alertes.join("  ·  "));
                      }
                    app.push_info(msg);
                    return Ok(InputMode::Normal);
                  }
                else
                  {
                    state.error = Some(
                      "Corrigez les champs avant d'enregistrer (version, statut, description)".to_string(),
                    );
                  }
                return Ok(InputMode::Popup(base.avec_projet(state.clone())));
              }
            KeyCode::Char(c) if state.status_menu_open =>
              {
                if c.is_alphanumeric() || c == '-'
                  {
                    state.status_filter.push(c);
                    state.status_cursor = 0;
                  }
                return Ok(InputMode::Popup(base.avec_projet(state.clone())));
              }
            // `o` et `n` désignent la réponse à valider, sans l'exécuter :
            // le surlignage montre ce qu'Entrée fera.
            KeyCode::Char('o') | KeyCode::Char('O') =>
              {
                if state.active_field == EDIT_CONFIRM_FIELD
                  {
                    state.choix_oui = true;
                  }
                return Ok(InputMode::Popup(base.avec_projet(state.clone())));
              }
            KeyCode::Char('n') | KeyCode::Char('N') =>
              {
                if state.active_field == EDIT_CONFIRM_FIELD
                  {
                    state.choix_oui = false;
                  }
                return Ok(InputMode::Popup(base.avec_projet(state.clone())));
              }
            _ =>
              {}
          }
      }
    Ok(InputMode::Popup(base.avec_projet(state.clone())))
  }

#[cfg(test)]
mod tests
  {
    use crate::{
        cout_affiche, kind_pour_repertoire, normaliser_recherche, sanitize, startup,
        variant_label, App, Cli, LaunchKind, Progress, SortOrder,
    };
    use clap::Parser;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("op-start-menu-main-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn le_premier_lancement_evite_la_reprise_de_session() {
        let dir = temp_dir("sans-session");
        let db = dir.join("opencode.db");
        // Base absente : aucun --continue ne doit être tenté, sinon
        // opencode s'arrête sur « Expected a string starting with ses ».
        assert_eq!(
            kind_pour_repertoire(&dir, &db, false),
            LaunchKind::Module,
            "sans session, le lancement se fait à blanc"
        );
        // L'entrée « Sans projet » garde sa voie, quelle que soit la base.
        assert_eq!(kind_pour_repertoire(&dir, &db, true), LaunchKind::Bare);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn un_repertoire_deja_ouvert_reprend_son_session() {
        let dir = temp_dir("avec-session");
        let db = dir.join("opencode.db");
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute(
                "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session (id, directory) VALUES ('ses_1', ?1)",
                [dir.to_string_lossy().as_ref()],
            )
            .unwrap();
        }
        assert_eq!(
            kind_pour_repertoire(&dir, &db, false),
            LaunchKind::Resume,
            "session existante : le résumé et --continue restent de mise"
        );
        let _ = fs::remove_dir_all(&dir);
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

    // --- Surface de la ligne de commande ---------------------------------
    // `op new` et `op --new new` se ressemblent mais n'ont rien à voir :
    // le premier est un mot-clé de projet, le second une demande de
    // création. C'est le genre de distinction que seul un test protège.

    /// Le `-V` du menu respecte son propre contrat : les trois constantes
    /// s'assemblent en une ligne que la sonde sait relire.
    #[test]
    fn le_contrat_version_du_menu_se_relit() {
        let ligne = format!("{}_{} ({})", crate::RELEASE, crate::STATUT, crate::DATE_LAST_MAJ);
        let etat = crate::variante::parser_etat_variante(&ligne).unwrap();
        assert_eq!(etat.version, crate::RELEASE);
        assert_eq!(etat.statut, crate::STATUT);
        assert_eq!(etat.date, crate::DATE_LAST_MAJ);
    }

    #[test]
    fn op_new_est_un_mot_cle_et_non_une_creation() {
        let cli = Cli::try_parse_from(["op", "new"]).unwrap();
        assert_eq!(cli.keyword.as_deref(), Some("new"), "« new » est un mot-clé");
        assert_eq!(cli.new, None, "ce n'est pas une demande de création");
    }

    #[test]
    fn op_new_demande_un_formulaire_vide() {
        let cli = Cli::try_parse_from(["op", "--new"]).unwrap();
        assert_eq!(cli.new.as_deref(), Some(""), "valeur absente → chaîne vide");
        assert_eq!(cli.keyword, None);
    }

    #[test]
    fn op_new_vide_presuppose_le_nom_du_projet() {
        let cli = Cli::try_parse_from(["op", "--new", "demo"]).unwrap();
        assert_eq!(cli.new.as_deref(), Some("demo"));
        assert_eq!(cli.keyword, None, "le nom est à la création, pas un mot-clé");
    }

    #[test]
    fn op_new_accepte_egalement_la_forme_avec_egale() {
        let cli = Cli::try_parse_from(["op", "--new=demo"]).unwrap();
        assert_eq!(cli.new.as_deref(), Some("demo"));
    }

    #[test]
    fn le_lancement_par_defaut_ne_demande_rien() {
        let cli = Cli::try_parse_from(["op"]).unwrap();
        assert_eq!(cli.new, None);
        assert_eq!(cli.keyword, None);
    }

    /// `--new` n'emporte que ce qui suit sur la même ligne : les options
    /// placées avant restent des options, pas un nom de projet.
    #[test]
    fn les_options_precedent_le_nom_du_projet() {
        let cli = Cli::try_parse_from(["op", "--root", "--new", "demo"]).unwrap();
        assert!(cli.root);
        assert_eq!(cli.new.as_deref(), Some("demo"));
    }

    #[test]
    fn le_sous_commande_new_a_disparu() {
        // `op new` reste valide : c'est un mot-clé. Il ne doit plus exister de
        // sous-commande, donc aucune forme ne doit se plaindre d'un sous-commande
        // inconnu.
        assert!(Cli::try_parse_from(["op", "new"]).is_ok());
    }

    /// Le tri retenu sur la ligne de commande prime sur la préférence, pour la
    /// seule exécution en cours.
    #[test]
    fn le_tri_de_la_ligne_de_commande_est_prioritaire() {
        let nom = Cli::try_parse_from(["op", "-sN"]).unwrap();
        assert_eq!(nom.sort, SortOrder::Name);
        let date = Cli::try_parse_from(["op", "-sM"]).unwrap();
        assert_eq!(date.sort, SortOrder::Modified);
        let defaut = Cli::try_parse_from(["op"]).unwrap();
        assert_eq!(defaut.sort, SortOrder::Modified);
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
    fn libelle_bifilaire_identique_omet_la_base()
      {
        let l = super::libelle_bifilaire(
            Some(("0.50.4", "Stable", "15-10-2026")),
            ("0.50.4", "Stable"),
        );
        assert_eq!(l, "0.50.4_Stable (15-10-2026)");
      }

    #[test]
    fn libelle_bifilaire_different_ajoute_la_base()
      {
        let l = super::libelle_bifilaire(
            Some(("0.50.4", "Stable", "15-10-2026")),
            ("0.50.5", "Terminée"),
        );
        assert_eq!(l, "0.50.4_Stable (15-10-2026) · base 0.50.5_Terminée");
      }

    #[test]
    fn libelle_bifilaire_sans_binaire_montre_la_base()
      {
        assert_eq!(super::libelle_bifilaire(None, ("0.50.4", "Stable")), "0.50.4_Stable");
        assert_eq!(super::libelle_bifilaire(None, ("", "")), "—");
      }

    #[test]
    fn libelle_bifilaire_ignore_un_statut_de_base_vide()
      {
        // Base sans statut connue : pas de complément parasite.
        assert_eq!(
            super::libelle_bifilaire(Some(("0.50.4", "Stable", "")), ("0.50.4", "")),
            "0.50.4_Stable",
        );
      }

    /// Le message d'action distingue le rattrapage d'une Release du simple
    /// alignement de la base Debug.
    #[test]
    fn libelle_maj_base_nomme_la_release_rattrapee()
      {
        use crate::coherence::{Ecart, Variante};
        assert_eq!(
            super::libelle_maj_base(&Ecart::ReleaseDivergeDeBase {
                base: "1.0.0".to_string(),
                binaire: "0.9.0".to_string(),
            }),
            "Release rattrapée",
        );
        assert_eq!(
            super::libelle_maj_base(&Ecart::FicheBuildPerimee { variante: Variante::Release }),
            "Release rattrapée",
        );
        assert_eq!(
            super::libelle_maj_base(&Ecart::DebugDivergeDeBase {
                base: "1.0.0".to_string(),
                binaire: "1.0.1".to_string(),
            }),
            "base mise à jour",
        );
        assert_eq!(
            super::libelle_maj_base(&Ecart::DebugDivergeDeRelease {
                debug: "1.0.0".to_string(),
                release: "0.9.0".to_string(),
            }),
            "base mise à jour",
        );
      }

    /// La fiche de build ne s'affiche que si elle colle à la référence du
    /// dernier build : une fiche périmée ne doit pas faire passer un vieux
    /// binaire pour neuf.
    #[test]
    fn fiche_pour_affichage_exige_la_reference_du_build()
      {
        let dir = temp_dir("fiche-affichage");
        let mut p = entry("projet", "Projet", dir.join("projet"));
        p.debug_snapshot = Some(crate::registry::SnapshotCompile {
            version: "1.2.3".to_string(),
            statut: "Stable".to_string(),
            date: "15-10-2026".to_string(),
            compile_secs: 1_700_000_000,
            empreinte_v: String::new(),
        });
        // Référence alignée sur la fiche : la fiche fait foi.
        p.last_compile_debug_secs = 1_700_000_000;
        assert_eq!(
            super::fiche_pour_affichage(&p, "debug"),
            Some(("1.2.3".to_string(), "Stable".to_string(), "15-10-2026".to_string())),
            "fiche alignée sur le dernier build : affichée",
        );
        // La référence a bougé : la fiche a été prise en défaut.
        p.last_compile_debug_secs = 1_700_000_100;
        assert_eq!(
            super::fiche_pour_affichage(&p, "debug"),
            None,
            "référence divergente : aucune fiche affichée",
        );
        // Jamais compilée : pas de fiche non plus.
        p.last_compile_debug_secs = 0;
        assert_eq!(super::fiche_pour_affichage(&p, "debug"), None);
        // Sans fiche du tout, rien.
        p.debug_snapshot = None;
        p.last_compile_debug_secs = 1_700_000_000;
        assert_eq!(super::fiche_pour_affichage(&p, "debug"), None);
        let _ = fs::remove_dir_all(&dir);
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
            last_session_cost_local: String::new(),
            last_agent: String::new(),
            last_time_created: String::new(),
            last_session_context_pct: String::new(),
            status_debug: String::new(),
            version_debug: String::new(),
            status_release: String::new(),
            version_release: String::new(),
            date_debug: String::new(),
            date_release: String::new(),
            probe_debug: String::new(),
            probe_release: String::new(),
            build_debug: String::new(),
            build_release: String::new(),
            debug_modified: String::new(),
            release_modified: String::new(),
            last_compile_debug_secs: 0,
            last_compile_release_secs: 0,
            last_target_debug_secs: 0,
            last_target_release_secs: 0,
            debug_snapshot: None,
            release_snapshot: None,
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
            est_bibliotheque: false,
            typologie: "Other".to_string(),
        }
    }

    #[test]
    fn le_statut_cible_retient_une_valeur_connue() {
        // Aucun statut enregistré : repli sur « Release », jamais une invention.
        let mut p = entry("a", "A", PathBuf::from("/tmp/a"));
        assert_eq!(super::statut_cible(&p), "Release");
        // Un statut valide enregistré est retenu tel quel.
        p.status_debug = "Beta".to_string();
        assert_eq!(super::statut_cible(&p), "Beta");
        // Un statut hors liste est ignoré au profit d'un autre, valide.
        p.status_debug = "Zorglub".to_string();
        p.status_release = "Beta-Release".to_string();
        assert_eq!(super::statut_cible(&p), "Beta-Release");
        // Tous hors liste : repli sur « Release ».
        p.status_debug = "Zorglub".to_string();
        p.status_release = "Gnarf".to_string();
        assert_eq!(super::statut_cible(&p), "Release");
    }

    #[test]
    fn la_version_cible_prefere_le_manifeste() {
        let mut p = entry("a", "A", PathBuf::from("/tmp/a"));
        p.version = "2.0.0".to_string();
        p.version_debug = "1.9.0".to_string();
        assert_eq!(super::version_cible(&p), "2.0.0");
        // Manifeste muet : la version enregistrée pour Debug fait foi.
        p.version = String::new();
        assert_eq!(super::version_cible(&p), "1.9.0");
    }

    /// Bout en bout : un projet dont le statut est hors liste est corrigé dans
    /// son module principal, reconstruit, et son `-V` publie alors le contrat.
    ///
    /// Le projet d'essai n'a aucune dépendance : il se compile hors ligne. C'est
    /// le seul test qui lance réellement `cargo build`.
    #[test]
    fn corriger_statut_rend_le_binaire_conforme() {
        let dir = temp_dir("corriger-statut");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"opessai\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            dir.join("src/main.rs"),
            "const RELEASE: &str = \"1.0.0\";\n\
             const STATUT: &str = \"Zorglub\";\n\
             const DATE_LAST_MAJ: &str = \"01-01-2020\";\n\
             fn main() {\n\
             \x20   if std::env::args().any(|a| a == \"-V\") {\n\
             \x20       println!(\"{RELEASE}_{STATUT} ({DATE_LAST_MAJ})\");\n\
             \x20       return;\n\
             \x20   }\n\
             }\n",
        )
        .unwrap();

        let mut app = App::new_light(dir.join("projects.json"), SortOrder::Modified);
        let mut p = entry("opessai", "opessai", dir.clone());
        p.version = "1.0.0".to_string();
        p.status_debug = "Release".to_string();
        app.projects = vec![p];

        super::executer_corriger_statut(&mut app, "opessai").unwrap();

        let contenu = fs::read_to_string(dir.join("src/main.rs")).unwrap();
        assert!(
            contenu.contains("const STATUT: &str = \"Release\";"),
            "le statut doit être ramené à une valeur connue :\n{contenu}"
        );
        let etat = crate::variante::lire_variante_avec_sonde(&dir, "debug", None)
            .expect("le binaire Debug doit publier le contrat")
            .etat;
        assert_eq!(etat.version, "1.0.0");
        assert_eq!(etat.statut, "Release");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Écrit un exécutable shell jouant le rôle d'un binaire `-V`.
    fn ecrire_outil(chemin: &Path, ligne: &str)
      {
        fs::create_dir_all(chemin.parent().unwrap()).unwrap();
        fs::write(chemin, format!("#!/bin/sh\necho \"{ligne}\"\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(chemin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
      }

    /// Rattrapage de base : les deux variantes sont alignées sur ce que
    /// déclarent leurs binaires (ici des sondes personnalisées), dates
    /// comprises, et un statut obsolète est effacé vers la vérité sondée.
    #[test]
    fn maj_base_aligne_les_deux_variantes_et_efface_le_statut_obsolete()
      {
        let dir = temp_dir("maj-base");
        let projet = dir.join("projet");
        fs::create_dir_all(&projet).unwrap();
        fs::write(projet.join("Cargo.toml"), "[package]\nname = \"projet\"\n").unwrap();
        ecrire_outil(&projet.join("tools/d"), "2.0.0_Stable (01-01-2026)");
        ecrire_outil(&projet.join("tools/r"), "1.5.0_Release (02-02-2026)");
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "projet": {
                    "name": "projet",
                    "path": projet.to_str().unwrap(),
                    "description": "",
                    "version": "0.1.0",
                    "version_debug": "0.1.0",
                    "status_debug": "Terminée",
                    "probe_debug": "./tools/d",
                    "probe_release": "./tools/r"
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let mut app = App::new_light(registre.clone(), SortOrder::Modified);

        super::executer_maj_base(&mut app, "projet").unwrap();

        let p = &app.projects[0];
        assert_eq!(p.version_debug, "2.0.0", "vérité du binaire Debug");
        assert_eq!(p.status_debug, "Stable", "le statut obsolète est effacé");
        assert_eq!(p.date_debug, "01-01-2026");
        assert_eq!(p.version_release, "1.5.0", "la Release suit sa propre sonde");
        assert_eq!(p.status_release, "Release");
        assert_eq!(p.date_release, "02-02-2026");
        // Champs historiques alignés sur la variante élue (Debug).
        assert_eq!(p.version, "2.0.0");
        assert_eq!(p.status, "Stable");
        // La base écrite survit à la relecture (garde neutralisée par les
        // déclarations).
        let relu = super::registry::load_registry(&registre).unwrap();
        assert_eq!(relu[0].version_debug, "2.0.0");
        assert_eq!(relu[0].status_debug, "Stable");
        assert_eq!(relu[0].version_release, "1.5.0");
        assert_eq!(relu[0].date_release, "02-02-2026");
        let _ = fs::remove_dir_all(&dir);
      }

    /// Sans binaire Release, le rattrapage ne prélève rien sur le Debug :
    /// la Release reste vide, jamais empruntée.
    #[test]
    fn maj_base_ne_prete_rien_au_release_sans_binaire()
      {
        let dir = temp_dir("maj-base-release-vide");
        let projet = dir.join("projet");
        fs::create_dir_all(&projet).unwrap();
        fs::write(projet.join("Cargo.toml"), "[package]\nname = \"projet\"\n").unwrap();
        ecrire_outil(&projet.join("tools/d"), "3.0.0_Beta (03-03-2026)");
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "projet": {
                    "name": "projet",
                    "path": projet.to_str().unwrap(),
                    "description": "",
                    "version_debug": "0.1.0",
                    "probe_debug": "./tools/d"
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let mut app = App::new_light(registre, SortOrder::Modified);

        super::executer_maj_base(&mut app, "projet").unwrap();

        let p = &app.projects[0];
        assert_eq!(p.version_debug, "3.0.0");
        assert_eq!(p.status_debug, "Beta");
        assert!(p.version_release.is_empty(), "jamais d'emprunt au Debug");
        assert!(p.status_release.is_empty());
        let _ = fs::remove_dir_all(&dir);
      }

    /// Le rebuild écrit toutes ses métadonnées (base, date, fiche) et les
    /// déclare à la garde : une relecture du registre ne doit rien
    /// restaurer — avant correctif, la fiche était rétablie à son ancienne
    /// valeur en silence.
    #[test]
    fn rebuild_debug_persiste_sans_restauration_de_la_garde()
      {
        let dir = temp_dir("rebuild-persiste");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
          dir.join("Cargo.toml"),
          "[package]\nname = \"opreb\"\nversion = \"0.9.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
          dir.join("src/main.rs"),
          "const RELEASE: &str = \"1.2.3\";\n\
           const STATUT: &str = \"Debug\";\n\
           const DATE_LAST_MAJ: &str = \"05-06-2026\";\n\
           fn main() {\n\
           \x20   if std::env::args().any(|a| a == \"-V\") {\n\
           \x20       println!(\"{RELEASE}_{STATUT} ({DATE_LAST_MAJ})\");\n\
           \x20       return;\n\
           \x20   }\n\
           }\n",
        )
        .unwrap();
        let registre = dir.join("projects.json");
        let mut app = App::new_light(registre.clone(), SortOrder::Modified);
        let mut p = entry("opreb", "opreb", dir.clone());
        p.version_debug = "0.9.0".to_string();
        p.status_debug = "Alpha".to_string();
        app.projects = vec![p];
        crate::registry::save_registry_full(&registre, &app.projects, &app.web_cache, &app.prefs)
          .unwrap();

        super::executer_rebuild_debug(&mut app, "opreb").unwrap();

        // Relecture depuis le disque : la garde ne doit rien restaurer.
        let relu = super::registry::load_registry(&registre).unwrap();
        assert_eq!(relu[0].version_debug, "1.2.3", "base persistée");
        assert_eq!(relu[0].status_debug, "Debug");
        assert_eq!(relu[0].date_debug, "05-06-2026", "date persistée");
        assert!(relu[0].last_compile_debug_secs > 0, "référence de build");
        let fiche = relu[0].debug_snapshot.as_ref().expect("fiche persistée");
        assert_eq!(fiche.version, "1.2.3");
        assert_eq!(fiche.date, "05-06-2026");
        assert!(!fiche.empreinte_v.is_empty());
        let _ = fs::remove_dir_all(&dir);
      }

    /// La commande personnalisée d'une variante prime sur Cargo, et s'exécute
    /// dans le dossier du projet.
    #[test]
    fn lancer_build_execute_la_commande_personnalisee()
      {
        let dir = temp_dir("build-commande");
        // Un `Cargo.toml` sans source ferait échouer Cargo : la réussite prouve
        // que la commande déclarée a été utilisée, pas le repli Cargo.
        fs::write(
          dir.join("Cargo.toml"),
          "[package]\nname = \"oprebu\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        super::lancer_build(&dir, "debug", "printf fait > marqueur").unwrap();
        assert_eq!(
          fs::read_to_string(dir.join("marqueur")).unwrap(),
          "fait",
          "la commande doit tourner dans le dossier du projet"
        );
        let _ = fs::remove_dir_all(&dir);
      }

    /// Sans commande déclarée ni `Cargo.toml`, la reconstruction s'arrête sur
    /// un message qui dit où régler la commande.
    #[test]
    fn lancer_build_sans_cargo_ni_commande_echoue()
      {
        let dir = temp_dir("build-sans-commande");
        let err = super::lancer_build(&dir, "release", "").unwrap_err().to_string();
        assert!(
          err.contains("commande de build release"),
          "le message doit nommer la variante et le réglage : {err}"
        );
        let _ = fs::remove_dir_all(&dir);
      }

    /// Une commande personnalisée qui échoue remonte un extrait de son stderr,
    /// plutôt qu'un code de retour muet.
    #[test]
    fn lancer_build_remonte_le_stderr_de_la_commande()
      {
        let dir = temp_dir("build-stderr");
        let err = super::lancer_build(&dir, "debug", "printf boom >&2; exit 3")
          .unwrap_err()
          .to_string();
        assert!(
          err.contains("boom"),
          "un extrait de stderr doit être remonté : {err}"
        );
        let _ = fs::remove_dir_all(&dir);
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


    // --- Édition de champ : le composant `op-tui::edit` porte les gestes ---

    /// Les touches du terminal se traduisent en gestes de champ, et rien
    /// d'autre : une touche d'application (Entrée, `↑`, `Ctrl`+lettre) ne doit
    /// jamais devenir une frappe.
    #[test]
    fn les_touches_du_champ_se_traduisent_en_gestes()
      {
        use op_tui::edit::Touche;
        let frappe = |code, mods| super::touche_edition(KeyEvent::new(code, mods));
        assert_eq!(frappe(KeyCode::Left, KeyModifiers::NONE), Some(Touche::Gauche));
        assert_eq!(frappe(KeyCode::Right, KeyModifiers::NONE), Some(Touche::Droite));
        assert_eq!(frappe(KeyCode::Left, KeyModifiers::CONTROL), Some(Touche::MotGauche));
        assert_eq!(frappe(KeyCode::Right, KeyModifiers::CONTROL), Some(Touche::MotDroite));
        assert_eq!(frappe(KeyCode::Home, KeyModifiers::NONE), Some(Touche::Debut));
        assert_eq!(frappe(KeyCode::End, KeyModifiers::NONE), Some(Touche::Fin));
        assert_eq!(frappe(KeyCode::PageDown, KeyModifiers::NONE), Some(Touche::PageBas));
        assert_eq!(frappe(KeyCode::Backspace, KeyModifiers::NONE), Some(Touche::Retour));
        assert_eq!(frappe(KeyCode::Delete, KeyModifiers::NONE), Some(Touche::Supprimer));
        assert_eq!(
          frappe(KeyCode::Char('é'), KeyModifiers::NONE),
          Some(Touche::Caractere('é'))
        );
        assert_eq!(frappe(KeyCode::Enter, KeyModifiers::NONE), None, "Entrée valide");
        assert_eq!(frappe(KeyCode::Up, KeyModifiers::NONE), None, "la navigation reste au gestionnaire");
        assert_eq!(frappe(KeyCode::Char('s'), KeyModifiers::CONTROL), None, "le raccourci ne frappe pas");
      }

    /// La première frappe continue le texte ouvert : l'ancien écrasement du
    /// champ entier a disparu, et les suppressions marchent tout de suite.
    #[test]
    fn la_frappe_continue_le_champ_ouvert()
      {
        use op_tui::edit::Touche;
        let mut tampon = "description existante".to_string();
        let mut curseur = tampon.chars().count();
        super::appliquer_edition(&mut tampon, &mut curseur, true, Touche::Caractere('!'), false);
        assert_eq!(tampon, "description existante!");
        assert_eq!(curseur, 22, "à la fin du texte inséré");
        super::appliquer_edition(&mut tampon, &mut curseur, false, Touche::Retour, false);
        assert_eq!(tampon, "description existante", "le retour arrière agit immédiatement");
        assert_eq!(curseur, 21);
      }

    /// `Suppr`, en toute première touche, vide le champ d'un coup : c'est le
    /// remplacement de la valeur affichée, à défaut de sélection. Ensuite il
    /// redevient une simple suppression.
    #[test]
    fn suppr_en_premiere_touche_vide_le_champ()
      {
        use op_tui::edit::Touche;
        let mut tampon = "ancienne description".to_string();
        let mut curseur = tampon.chars().count();
        super::appliquer_edition(&mut tampon, &mut curseur, true, Touche::Supprimer, false);
        assert_eq!(tampon, "", "le champ est vidé d'un seul geste");
        assert_eq!(curseur, 0);
        tampon = "abc".to_string();
        curseur = 3;
        super::appliquer_edition(&mut tampon, &mut curseur, false, Touche::Supprimer, false);
        assert_eq!(tampon, "abc", "rien sous le curseur : rien à supprimer");
      }

    /// La Version se saisit par masque : la première frappe repart de zéro,
    /// le point s'insère seul, et les lettres restent dehors.
    #[test]
    fn la_version_se_re_saisit_par_masque()
      {
        use op_tui::edit::Touche;
        let mut tampon = "0.20.21".to_string();
        let mut curseur = tampon.chars().count();
        let mut premiere = true;
        for c in "0.21.0".chars()
          {
            super::appliquer_edition(&mut tampon, &mut curseur, premiere, Touche::Caractere(c), true);
            premiere = false;
          }
        assert_eq!(tampon, "0.21.0");
        assert_eq!(curseur, 6);
        super::appliquer_edition(&mut tampon, &mut curseur, false, Touche::Caractere('x'), true);
        assert_eq!(tampon, "0.21.0", "le masque refuse les lettres");
      }

    /// Ouvrir le champ Version d'un projet et frapper doit conduire à la
    /// nouvelle version, pas à un mélange des deux.
    #[test]
    fn le_champ_version_du_popup_s_edite_au_clavier()
      {
        let mut app = app_avec_projet();
        let popup = crate::Popup::nouveau(
          0,
          super::ProjectEditState::from_project(&app.projects[0], 0),
          "FRA".to_string(),
        );
        // Entrée ouvre le champ : la valeur courante devient le tampon.
        let ouvert = frappe_dans_le_champ(&mut app, &popup, &[KeyCode::Enter]);
        assert!(ouvert.editing, "le champ est en saisie");
        assert_eq!(ouvert.input_buffer, "1.0.0", "le tampon est pré-rempli");
        assert_eq!(ouvert.input_offset, 5, "curseur en fin");
        // Même ouverture, puis `Suppr` qui vide et la frappe de la suite.
        let etat = frappe_dans_le_champ(
          &mut app,
          &popup,
          &[
            KeyCode::Enter,
            KeyCode::Delete,
            KeyCode::Char('1'),
            KeyCode::Char('.'),
            KeyCode::Char('0'),
            KeyCode::Char('.'),
            KeyCode::Char('1'),
          ],
        );
        assert_eq!(etat.input_buffer, "1.0.1", "la nouvelle version tient seule");
        assert_eq!(etat.input_offset, 5);
      }

    /// La Description est du texte libre : ouvrir le champ ne prépare pas un
    /// écrasement, la frappe s'ajoute au texte déjà présent.
    #[test]
    fn la_description_du_popup_s_edite_sans_ecrasement()
      {
        let mut app = app_avec_projet();
        let popup = crate::Popup::nouveau(
          0,
          super::ProjectEditState::from_project(&app.projects[0], 0),
          "FRA".to_string(),
        );
        let mut ouvert = popup.projet.clone();
        ouvert.active_field = 6;
        ouvert.pending_description = "texte existant".to_string();
        let base = popup.avec_projet(ouvert);
        let etat = frappe_dans_le_champ(&mut app, &base, &[KeyCode::Enter, KeyCode::Char('!')]);
        assert!(etat.editing, "Entrée ouvre le champ");
        assert_eq!(
          etat.input_buffer, "texte existant!",
          "la frappe continue le texte ouvert"
        );
        assert_eq!(etat.input_offset, 15, "curseur derrière la frappe");
        // Un champ vide se remplit pareil, sans écrasement particulier.
        let mut vide = popup.projet.clone();
        vide.active_field = 6;
        let base = popup.avec_projet(vide);
        let etat = frappe_dans_le_champ(
          &mut app,
          &base,
          &[KeyCode::Enter, KeyCode::Char('!'), KeyCode::Char('!')],
        );
        assert_eq!(etat.input_buffer, "!!");
      }

    /// Le raccourci des flèches latérales : `→` au milieu du texte écrit à
    /// gauche du curseur, comme dans tout éditeur.
    #[test]
    fn le_curseur_du_popup_sert_a_inserer()
      {
        let mut app = app_avec_projet();
        let popup = crate::Popup::nouveau(
          0,
          super::ProjectEditState::from_project(&app.projects[0], 0),
          "FRA".to_string(),
        );
        let mut ouvert = popup.projet.clone();
        ouvert.active_field = 6;
        ouvert.pending_description = "abcd".to_string();
        let base = popup.avec_projet(ouvert);
        let etat = frappe_dans_le_champ(
          &mut app,
          &base,
          &[KeyCode::Enter, KeyCode::Left, KeyCode::Left, KeyCode::Char('X')],
        );
        assert_eq!(etat.input_buffer, "abXcd", "insertion à la position du curseur");
        assert_eq!(etat.input_offset, 3);
      }

    /// Appelé par les tests d'édition : frappe chaque touche dans l'état du
    /// popup, et rend l'état qui en ressort.
    fn frappe_dans_le_champ(
      app: &mut crate::App,
      popup: &crate::Popup,
      touches: &[KeyCode],
    ) -> super::ProjectEditState
      {
        let mut courant = popup.clone();
        for code in touches
          {
            let mut etat = courant.projet.clone();
            let touche = KeyEvent::new(*code, KeyModifiers::NONE);
            match super::handle_edit_input(touche, &mut etat, app, &courant)
              {
                Ok(crate::InputMode::Popup(p)) => courant = p,
                _ => panic!("{code:?} doit rester dans le popup"),
              }
          }
        courant.projet
      }

    /// Appelé par les tests de confirmation : frappe chaque touche et rend le
    /// mode final. La confirmation a le droit de fermer le popup — c'est
    /// justement ce que ces tests vérifient, d'où ce rendu différent de
    /// `frappe_dans_le_champ`.
    fn frappe_de_confirmation(
      app: &mut crate::App,
      popup: &crate::Popup,
      touches: &[KeyCode],
    ) -> crate::InputMode
      {
        let mut courant = popup.clone();
        for code in touches
          {
            let mut etat = courant.projet.clone();
            let touche = KeyEvent::new(*code, KeyModifiers::NONE);
            match super::handle_edit_input(touche, &mut etat, app, &courant).unwrap()
              {
                crate::InputMode::Popup(p) => courant = p,
                autre => return autre,
              }
          }
        crate::InputMode::Popup(courant)
      }

    /// Un popup posé sur le champ de confirmation, avec un projet valide.
    fn popup_confirmation(app: &crate::App) -> crate::Popup
      {
        let mut projet = super::ProjectEditState::from_project(&app.projects[0], 0);
        projet.active_field = super::EDIT_CONFIRM_FIELD;
        crate::Popup::nouveau(0, projet, "FRA".to_string())
      }

    /// Le défaut `[Non]` protège : une Entrée posée sur la confirmation sans
    /// avoir choisi doit fermer sans rien déposer.
    #[test]
    fn la_confirmation_par_defaut_ne_enregistre_pas()
      {
        let mut app = app_avec_projet();
        let mut popup = popup_confirmation(&app);
        popup.projet.pending_description = "modifie".to_string();
        let mode = frappe_de_confirmation(&mut app, &popup, &[KeyCode::Enter]);
        assert!(
          matches!(mode, crate::InputMode::Normal),
          "[Non] + Entrée doit fermer le popup"
        );
        assert_eq!(
          app.projects[0].description, "",
          "aucune variante ne doit être déposée sans choix [Oui]"
        );
      }

    /// Le choix `[Oui]` est le seul qui dépose la variante.
    #[test]
    fn la_confirmation_par_oui_enregistre()
      {
        let mut app = app_avec_projet();
        let mut popup = popup_confirmation(&app);
        popup.projet.pending_description = "modifie".to_string();
        popup.projet.choix_oui = true;
        let mode = frappe_de_confirmation(&mut app, &popup, &[KeyCode::Enter]);
        assert!(
          matches!(mode, crate::InputMode::Normal),
          "[Oui] + Entrée enregistre puis ferme"
        );
        assert_eq!(app.projects[0].description, "modifie");
      }

    /// `o` et `n` désignent la réponse : ils ne l'exécutent pas, sinon le
    /// surlignage serait un décor et Entrée n'aurait plus rien à valider.
    #[test]
    fn o_et_n_designent_la_reponse_de_la_confirmation()
      {
        let mut app = app_avec_projet();
        let popup = popup_confirmation(&app);
        let etat = frappe_dans_le_champ(&mut app, &popup, &[KeyCode::Char('o')]);
        assert!(etat.choix_oui, "o doit designer [Oui]");
        let etat = frappe_dans_le_champ(
          &mut app,
          &popup,
          &[KeyCode::Char('o'), KeyCode::Char('n')],
        );
        assert!(!etat.choix_oui, "n doit designer [Non]");
        assert_eq!(
          app.projects[0].description, "",
          "désigner une réponse ne doit rien enregistrer"
        );
      }

    /// Sur la confirmation, `←` vise `[Oui]` et `→` vise `[Non]` : les mêmes
    /// flèches que partout ailleurs, appliquées au choix affiché.
    #[test]
    fn les_fleches_horizontales_choisissent_la_reponse()
      {
        let mut app = app_avec_projet();
        let popup = popup_confirmation(&app);
        let etat = frappe_dans_le_champ(&mut app, &popup, &[KeyCode::Left]);
        assert!(etat.choix_oui, "← doit designer [Oui]");
        let etat = frappe_dans_le_champ(&mut app, &popup, &[KeyCode::Right]);
        assert!(!etat.choix_oui, "→ doit designer [Non]");
      }

    /// La saisie du code pays reprend les mêmes gestes : suppression au
    /// curseur, puis frappe à la position courante.
    #[test]
    fn la_saisie_du_pays_edite_au_curseur()
      {
        let mut app = app_avec_projet();
        let mut popup = popup_navigation().avec_onglet(crate::Onglet::Parametres);
        popup.parametres.edition = true;
        popup.parametres.tampon = "FRA".to_string();
        popup.parametres.curseur = 3;
        super::edition_pays(
          KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
          &mut app,
          &mut popup,
        );
        assert_eq!(popup.parametres.tampon, "FR");
        assert_eq!(popup.parametres.curseur, 2);
        super::edition_pays(
          KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
          &mut app,
          &mut popup,
        );
        assert_eq!(popup.parametres.curseur, 1);
        super::edition_pays(
          KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
          &mut app,
          &mut popup,
        );
        assert_eq!(popup.parametres.tampon, "FAR", "la frappe s'insère au curseur, en majuscule");
        assert_eq!(popup.parametres.curseur, 2);
      }

    /// La saisie tarifaire garde ses règles (chiffres, virgule) tout en
    /// prenant le curseur standard.
    #[test]
    fn la_saisie_tarifaire_s_edite_au_curseur()
      {
        let mut app = app_avec_projet();
        let popup = popup_navigation().avec_onglet(crate::Onglet::Couts);
        let ouvrir = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        let mode = super::handle_tariff_input(
          ouvrir,
          &mut app,
          &popup,
          0,
          1,
          false,
          String::new(),
          0,
          "FRA".to_string(),
          None,
          false,
        )
        .unwrap();
        let mut courant = match mode
          {
            crate::InputMode::Popup(p) => p,
            _ => panic!("l'entrée doit ouvrir la saisie"),
          };
        assert!(courant.couts.editing, "champ tarifaire en saisie");
        assert_eq!(courant.couts.input_buffer, "0", "tarif actuel pré-rempli");
        assert_eq!(courant.couts.input_offset, 1, "curseur en fin");
        assert_eq!(courant.couts.active_field, 1, "champ tarifaire");

        // Effacement puis frappe de la nouvelle valeur.
        for code in [KeyCode::Backspace, KeyCode::Char('4'), KeyCode::Char('2')]
          {
            let touche = KeyEvent::new(code, KeyModifiers::NONE);
            let resultat = super::handle_tariff_input(
              touche,
              &mut app,
              &courant,
              0,
              courant.couts.active_field,
              courant.couts.editing,
              courant.couts.input_buffer.clone(),
              courant.couts.input_offset,
              courant.couts.country.clone(),
              courant.couts.message.clone(),
              false,
            )
            .unwrap();
            match resultat
              {
                crate::InputMode::Popup(p) => courant = p,
                _ => panic!("{code:?} doit rester dans l'onglet Coûts"),
              }
          }
        assert_eq!(courant.couts.input_buffer, "42", "chiffres saisis au curseur");
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
        assert!(super::bump_manifest_version(&dir, "1.2.4").unwrap());
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
        assert!(!super::bump_manifest_version(&dir, "1.2.4").unwrap());
        let content = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(!content.contains("1.2.4"));
        fs::remove_dir_all(&dir).unwrap();
      }

    #[test]
    fn bump_manifest_version_inserts_absent_version()
      {
        let dir = temp_dir("nover");
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"demo\"\n\n[dependencies]\nfoo = \"1\"\n").unwrap();
        assert!(super::bump_manifest_version(&dir, "0.0.1").unwrap());
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
        assert!(!super::bump_manifest_version(&dir, "1.0.1").unwrap());
        let c = fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert_eq!(c.matches("version").count(), 1);
        fs::write(dir.join("Cargo.toml"), "[package]\nname = \"a\"\nversion = '1.0.0' # note\n").unwrap();
        assert!(super::bump_manifest_version(&dir, "1.0.1").unwrap());
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

    /// `TestBackend` ne peut pas échouer (`Error = Infallible`) : convertir son
    /// résultat en `io::Error` est donc total, via l'absence de cas.
    fn jamais(e: std::convert::Infallible) -> std::io::Error
      {
        match e {}
      }

    impl ratatui::backend::Backend for BackendEnPanne
      {
        // ratatui 0.30 a introduit un type d'erreur associé au trait
        // `Backend` ; `std::io::Error` est celui des backends déjà testés.
        type Error = std::io::Error;

        fn draw<'a, I>(&mut self, _content: I) -> std::io::Result<()>
          where
            I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>
          {
            Err(std::io::Error::other("panne du terminal simulee"))
          }
fn hide_cursor(&mut self) -> std::io::Result<()> { self.0.hide_cursor().map_err(jamais) }
        fn show_cursor(&mut self) -> std::io::Result<()> { self.0.show_cursor().map_err(jamais) }
        fn get_cursor_position(&mut self) -> std::io::Result<ratatui::prelude::Position>
          { self.0.get_cursor_position().map_err(jamais) }
        fn set_cursor_position<P: Into<ratatui::prelude::Position>>(&mut self, p: P) -> std::io::Result<()>
          { self.0.set_cursor_position(p).map_err(jamais) }
        fn clear(&mut self) -> std::io::Result<()> { self.0.clear().map_err(jamais) }
        fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> std::io::Result<()>
          { self.0.clear_region(clear_type).map_err(jamais) }
        fn size(&self) -> std::io::Result<ratatui::prelude::Size> { self.0.size().map_err(jamais) }
        fn window_size(&mut self) -> std::io::Result<ratatui::backend::WindowSize>
          { self.0.window_size().map_err(jamais) }
        fn flush(&mut self) -> std::io::Result<()> { self.0.flush().map_err(jamais) }
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
    // --- Navigation entre onglets (Tab / Shift+Tab) ---------------------

    /// `App` avec un seul projet : le handler de l'onglet Coûts refuse un
    /// index de projet absent, il faut donc un vrai registre sous la main.
    ///
    /// Le répertoire est numéroté : les tests tournent en parallèle et
    /// partageraient sinon le même registre, que chacun supprime au début.
    fn app_avec_projet() -> crate::App
      {
        static COMPTEUR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COMPTEUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = temp_dir(&format!("onglets-{n}"));
        let projet = dir.join("projet");
        fs::create_dir_all(&projet).unwrap();
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "projet": {
                    "name": "projet",
                    "path": projet.to_str().unwrap(),
                    "version": "1.0.0",
                    "version_debug": "1.0.0",
                    "description": "",
                    "tarif_mode": "manuel"
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        crate::App::new_light(registre, SortOrder::Modified)
      }

    /// L'édition du popup écrit la SOURCE : les trois constantes du contrat
    /// dans le module principal, la date du jour, aucun rebuild. La base
    /// suit, la Release reste intacte.
    #[test]
    fn l_edition_ecrit_les_constantes_de_la_source()
      {
        let mut app = app_avec_projet();
        let projet = app.projects[0].path.clone();
        fs::create_dir_all(projet.join("src")).unwrap();
        fs::write(
          projet.join("src/main.rs"),
          "const RELEASE: &str = \"1.0.0\";\nfn main() {}\n",
        )
        .unwrap();
        let mut state = super::ProjectEditState::from_project(&app.projects[0], 0);
        state.pending_version = "2.0.0".to_string();
        state.pending_status = "Stable".to_string();

        assert!(super::apply_edit(&mut app, &state), "l'édition est acceptée");

        let source = fs::read_to_string(projet.join("src/main.rs")).unwrap();
        assert!(source.contains("const RELEASE: &str = \"2.0.0\";"), "{source}");
        assert!(source.contains("const STATUT: &str = \"Stable\";"), "{source}");
        assert!(
          source.contains(&format!(
            "const DATE_LAST_MAJ: &str = \"{}\";",
            super::registry::date_du_jour()
          )),
          "la date du jour est déclarée : {source}"
        );
        let p = &app.projects[0];
        assert_eq!(p.version_debug, "2.0.0");
        assert_eq!(p.status_debug, "Stable");
        assert_eq!(p.date_debug, super::registry::date_du_jour());
        assert!(p.version_manually_edited, "version changée : drapeau levé");
        // La Release n'est jamais miroir du Debug.
        assert!(p.version_release.is_empty());
        // Le hash décrit la source réécrite : pas de faux bump au rescan.
        assert_eq!(
          p.source_hash,
          super::registry::compute_source_hash(&projet)
        );
      }

    /// Un projet sans module Rust conserve son édition en base : rien à
    /// écrire dans une source que op ne connaît pas, sans erreur.
    #[test]
    fn l_edition_d_un_projet_sans_module_reste_en_base()
      {
        let mut app = app_avec_projet();
        let date_avant = app.projects[0].date_debug.clone();
        let mut state = super::ProjectEditState::from_project(&app.projects[0], 0);
        state.pending_version = "3.1.4".to_string();
        state.pending_status = "Deprecated".to_string();

        assert!(super::apply_edit(&mut app, &state));
        assert_eq!(app.projects[0].version_debug, "3.1.4");
        assert_eq!(app.projects[0].status_debug, "Deprecated");
        // Aucune source réécrite : la date de base ne bouge pas (elle ne prend
        // pas la date du jour comme si le module avait été retouché).
        assert_eq!(
          app.projects[0].date_debug, date_avant,
          "sans module écrit, la date de base ne bouge pas"
        );
      }

    /// La date de base d'une variante suit le mtime de `target/<profil>` quand
    /// aucun binaire `-V` ne répond : dernier recours pour un projet dont la
    /// sonde ne dit rien.
    #[test]
    fn maj_date_cible_suit_le_mtime_sans_binaire()
      {
        let dir = temp_dir("maj-date-cible");
        let projet = dir.join("projet");
        fs::create_dir_all(projet.join("target/debug")).unwrap();
        let mut p = entry("projet", "Projet", projet.clone());
        super::maj_date_cible(&mut p, "debug");
        let mtime = super::registry::variant_modified_time(&projet, "debug").unwrap();
        assert_eq!(p.date_debug, super::registry::date_jj_mm_yyyy(mtime));
        assert!(p.last_target_debug_secs > 0, "epoch du mtime mémorisé");
        // Idempotent : un second passage ne réécrit rien.
        let secs = p.last_target_debug_secs;
        super::maj_date_cible(&mut p, "debug");
        assert_eq!(p.last_target_debug_secs, secs);
      }

    /// Un binaire `-V` répondant au contrat couvre la cible : sa date fait foi,
    /// le mtime est ignoré et la date de base ne suit pas le dossier.
    #[test]
    fn maj_date_cible_ignore_le_mtime_si_un_binaire_repond()
      {
        let dir = temp_dir("maj-date-cible-binaire");
        let projet = dir.join("projet");
        let cible = projet.join("target/debug/projet");
        fs::create_dir_all(cible.parent().unwrap()).unwrap();
        fs::write(&cible, "#!/bin/sh\necho \"2.0.0_Stable (05-05-2026)\"\n").unwrap();
        let mut droits = fs::metadata(&cible).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            droits.set_mode(0o755);
        }
        fs::set_permissions(&cible, droits).unwrap();
        let mut p = entry("projet", "Projet", projet.clone());
        super::maj_date_cible(&mut p, "debug");
        assert_eq!(p.date_debug, "", "le binaire fait foi, le mtime est ignoré");
        assert_eq!(p.last_target_debug_secs, 0);
      }

    /// Une conformisation qui ne change rien au source (RELEASE et STATUT déjà
    /// alignés) PRÉSERVE la `DATE_LAST_MAJ` du module : elle ne périme pas un
    /// code qui n'a pas bougé.
    #[test]
    fn conformer_preserve_la_date_si_rien_ne_change()
      {
        let contenu = "const RELEASE: &str = \"1.0.0\";\n\
            const STATUT: &str = \"Stable\";\n\
            const DATE_LAST_MAJ: &str = \"01-01-2020\";\n\
            fn main() {}\n";
        assert_eq!(
            super::date_conformisation(contenu, "1.0.0", "Stable"),
            "01-01-2020",
            "déjà aligné : la date du module est préservée",
        );
        assert_eq!(
            super::date_conformisation(contenu, "2.0.0", "Stable"),
            super::aujourd_hui(),
            "version à corriger : le source est réécrit, date du jour",
        );
        assert_eq!(
            super::date_conformisation(contenu, "1.0.0", "Release"),
            super::aujourd_hui(),
            "statut à corriger : date du jour",
        );
      }

    /// Le tri « Modifié » s'aligne sur `target/debug` : un projet dont la cible
    /// est ancienne passe derrière un projet à la cible récente, même si ses
    /// sources hors cible sont récentes.
    #[test]
    fn le_tri_modifie_suit_la_cible_debug()
      {
        let dir = temp_dir("tri-modifie-cible");
        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir_all(a.join("target/debug")).unwrap();
        fs::create_dir_all(b.join("target/debug")).unwrap();
        // `a` : cible ancienne, source hors cible récente. `b` : cible récente.
        reculer_mtime(&a.join("target/debug"), 3_600);
        reculer_mtime(&b.join("target/debug"), 0);
        fs::write(a.join("recent.txt"), "x").unwrap();
        let ta = super::registry::cible_modified_time(&a).unwrap();
        let tb = super::registry::cible_modified_time(&b).unwrap();
        assert!(ta < tb, "la cible prime sur une source hors cible plus récente");
      }

    /// Recule (best effort) le mtime d'un chemin de `secs` secondes.
    fn reculer_mtime(chemin: &Path, secs: u64) {
        let Ok(f) = fs::File::open(chemin) else { return };
        let instant = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
        let _ = f.set_modified(instant);
    }

    /// Les commandes d'outils éditables (sonde et build, par variante) sont
    /// reportées sur le projet, rognées. Ce sont des réglages, pas des
    /// métadonnées de vérité : la garde ne les protège pas.
    #[test]
    fn l_edition_enregistre_les_commandes_d_outils()
      {
        let mut app = app_avec_projet();
        let mut state = super::ProjectEditState::from_project(&app.projects[0], 0);
        state.outils.probe_debug = "  ./bin/mreport -V  ".to_string();
        state.outils.build_debug = "make debug".to_string();
        state.outils.probe_release = "./bin/mreport -V".to_string();
        state.outils.build_release = "make release".to_string();

        assert!(super::apply_edit(&mut app, &state));
        let p = &app.projects[0];
        assert_eq!(p.probe_debug, "./bin/mreport -V", "espaces rognés");
        assert_eq!(p.build_debug, "make debug");
        assert_eq!(p.probe_release, "./bin/mreport -V");
        assert_eq!(p.build_release, "make release");
        assert_eq!(p.sonde("debug"), Some("./bin/mreport -V"));
        assert_eq!(p.sonde("release"), Some("./bin/mreport -V"));
      }

    /// Entrée sur le champ Typologie ouvre le menu de choix, ↓ déplace le
    /// curseur, Entrée retient la valeur ; le choix se reporte en base.
    #[test]
    fn le_menu_typologie_choisit_et_reporte_en_base()
      {
        let mut app = app_avec_projet();
        let mut popup = crate::Popup::nouveau(
          0,
          super::ProjectEditState::from_project(&app.projects[0], 0),
          "FRA".to_string(),
        );
        let mut ouvert = popup.projet.clone();
        ouvert.active_field = 7;
        popup = popup.avec_projet(ouvert);
        // Entrée ouvre le menu sur la valeur courante, ↓ puis Entrée choisit.
        let etat = frappe_dans_le_champ(
          &mut app,
          &popup,
          &[KeyCode::Enter, KeyCode::Down, KeyCode::Enter],
        );
        assert!(
          super::registry::validate_typologie(&etat.pending_typologie),
          "une typologie connue est retenue : {}",
          etat.pending_typologie
        );
        assert!(!etat.typologie_menu_open, "le menu se referme après validation");

        let mut a_retenir = super::ProjectEditState::from_project(&app.projects[0], 0);
        a_retenir.pending_typologie = "Code-App".to_string();
        assert!(super::apply_edit(&mut app, &a_retenir));
        assert_eq!(app.projects[0].typologie, "Code-App");
      }

    /// Un simple changement de statut (version inchangée) écrit la source
    /// sans lever le drapeau anti-bump : le prochain bump reste légitime.
    #[test]
    fn un_changement_de_statut_seul_ne_leve_pas_le_drapeau()
      {
        let mut app = app_avec_projet();
        let projet = app.projects[0].path.clone();
        fs::create_dir_all(projet.join("src")).unwrap();
        fs::write(
          projet.join("src/main.rs"),
          "const RELEASE: &str = \"1.0.0\";\nfn main() {}\n",
        )
        .unwrap();
        let mut state = super::ProjectEditState::from_project(&app.projects[0], 0);
        state.pending_version = "1.0.0".to_string();
        state.pending_status = "Deprecated".to_string();

        assert!(super::apply_edit(&mut app, &state));
        let source = fs::read_to_string(projet.join("src/main.rs")).unwrap();
        assert!(source.contains("const STATUT: &str = \"Deprecated\";"), "{source}");
        assert!(
          !app.projects[0].version_manually_edited,
          "version inchangée : pas de drapeau"
        );
      }

    /// Un projet neuf n'a aucune Release : sans binaire release, version et
    /// statut restent vides, affichés « — ». Le formulaire ne doit pas
    /// recopier la variante Debug sur la Release au moment de créer.
    #[test]
    fn la_creation_ne_recopie_pas_la_variante_release()
      {
        let mut app = app_avec_projet();
        let mut state = crate::ProjectCreationState::default();
        let entre = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
        let remplir = |state: &mut crate::ProjectCreationState,
                       app: &mut crate::App,
                       valeur: &str| {
            state.input_buffer = valeur.to_string();
            state.input_offset = valeur.chars().count();
            state.premiere_touche = false;
            let avant = state.active_field;
            assert!(
              matches!(
                crate::handle_creation_input(entre(KeyCode::Enter), state, app),
                Ok(crate::InputMode::CreatingProject(_))
              ),
              "le champ {avant} doit continuer la création"
            );
          };
        let dir = temp_dir("creation-f");
        for champ in ["projet-f", "Projet F", dir.to_str().unwrap(), "test lot F"]
          {
            remplir(&mut state, &mut app, champ);
          }
        // Champ statut : Entrée ouvre le menu déroulant, une seconde Entrée
        // choisit « <Aucun> » (première entrée, curseur 0).
        remplir(&mut state, &mut app, "");
        remplir(&mut state, &mut app, "");
        assert_eq!(state.active_field, 5, "le champ statut doit passer à la version");
        remplir(&mut state, &mut app, "0.9.0");
        assert_eq!(state.active_field, 6, "la version validée ouvre la confirmation");
        let entree = entre(KeyCode::Enter);
        let mode = crate::handle_creation_input(entree, &mut state, &mut app).unwrap();
        assert!(
          matches!(mode, crate::InputMode::Normal),
          "« o » sur la confirmation clôt le formulaire"
        );

        let p = app
          .projects
          .iter()
          .find(|p| p.id == "projet-f")
          .expect("le projet doit être créé");
        assert_eq!(p.version_debug, "0.9.0");
        assert_eq!(p.version_release, "", "pas encore de build release : « — » à l'écran");
        assert_eq!(p.status_release, "");
        let _ = fs::remove_dir_all(&dir);
      }

    /// Le mode « Nouveau » ouvre d'abord le choix Projet / Bibliothèque :
    /// mouvement borné, désignation directe p/b, Entrée valide le type.
    #[test]
    fn le_choix_du_type_parcourt_projet_et_bibliotheque()
      {
        assert!(
          matches!(
            crate::gerer_choix_nouveau(KeyCode::Down, 0),
            crate::InputMode::ChoixNouveau { selected: 1 }
          ),
          "la flèche bas désigne la bibliothèque"
        );
        assert!(
          matches!(
            crate::gerer_choix_nouveau(KeyCode::Up, 0),
            crate::InputMode::ChoixNouveau { selected: 0 }
          ),
          "pas de boucle sur la première entrée"
        );
        assert!(
          matches!(
            crate::gerer_choix_nouveau(KeyCode::Down, 1),
            crate::InputMode::ChoixNouveau { selected: 1 }
          ),
          "pas de boucle sur la dernière entrée"
        );
        match crate::gerer_choix_nouveau(KeyCode::Enter, 1)
          {
            crate::InputMode::CreatingProject(s) =>
              assert!(s.bibliotheque, "la bibliothèque cochée est validée"),
            _ => panic!("Entrée doit valider le choix"),
          }
        match crate::gerer_choix_nouveau(KeyCode::Enter, 0)
          {
            crate::InputMode::CreatingProject(s) =>
              assert!(!s.bibliotheque, "le projet reste le défaut"),
            _ => panic!("Entrée doit valider le choix"),
          }
        assert!(
          matches!(crate::gerer_choix_nouveau(KeyCode::Esc, 0), crate::InputMode::Normal),
          "Échap referme le choix sans créer"
        );
        match crate::gerer_choix_nouveau(KeyCode::Char('b'), 0)
          {
            crate::InputMode::CreatingProject(s) =>
              assert!(s.bibliotheque, "p désigne directement la bibliothèque"),
            _ => panic!("la touche b doit ouvrir la création bibliothèque"),
          }
        match crate::gerer_choix_nouveau(KeyCode::Char('p'), 1)
          {
            crate::InputMode::CreatingProject(s) =>
              assert!(!s.bibliotheque, "n désigne directement le projet"),
            _ => panic!("la touche p doit ouvrir la création projet"),
          }
      }

    /// La création « bibliothèque » prédéclare le Chemin sous la racine des
    /// bibliothèques dès la saisie de l'identifiant.
    #[test]
    fn une_bibliotheque_prefixe_le_chemin_par_defaut()
      {
        let mut app = app_avec_projet();
        let libs = temp_dir("prefixe-chemin-lib").join("Libs");
        app.prefs.lib_root = libs.to_string_lossy().to_string();
        let mut state =
          crate::ProjectCreationState { bibliotheque: true, ..Default::default() };
        state.input_buffer = "ma-lib".to_string();
        let mode = crate::handle_creation_input(
          KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
          &mut state,
          &mut app,
        )
        .unwrap();
        assert!(matches!(mode, crate::InputMode::CreatingProject(_)));
        assert_eq!(state.id, "ma-lib");
        assert_eq!(state.path, libs.join("ma-lib").to_string_lossy());
        let _ = fs::remove_dir_all(libs.parent().unwrap());
      }

    /// Une bibliothèque créée porte son drapeau et atterrit sous la racine des
    /// bibliothèques, pas sous celle des projets.
    #[test]
    fn une_bibliotheque_creee_est_marquee_et_rangee_sous_la_racine()
      {
        let mut app = app_avec_projet();
        let libs = temp_dir("creation-racine-lib").join("Libs");
        app.prefs.lib_root = libs.to_string_lossy().to_string();
        let mut state =
          crate::ProjectCreationState { bibliotheque: true, ..Default::default() };
        let entre = |code: KeyCode| KeyEvent::new(code, KeyModifiers::NONE);
        let remplir = |state: &mut crate::ProjectCreationState,
                       app: &mut crate::App,
                       valeur: &str| {
          state.input_buffer = valeur.to_string();
          state.input_offset = valeur.chars().count();
          state.premiere_touche = false;
          let avant = state.active_field;
          assert!(
            matches!(
              crate::handle_creation_input(entre(KeyCode::Enter), state, app),
              Ok(crate::InputMode::CreatingProject(_))
            ),
            "le champ {avant} doit continuer la création"
          );
        };
        remplir(&mut state, &mut app, "ma-lib");
        remplir(&mut state, &mut app, "Ma Lib");
        assert_eq!(
          state.path, libs.join("ma-lib").to_string_lossy(),
          "le Chemin est prédéclaré sous la racine des bibliothèques"
        );
        remplir(&mut state, &mut app, &libs.join("ma-lib").to_string_lossy());
        remplir(&mut state, &mut app, "ma description");
        remplir(&mut state, &mut app, "");
        remplir(&mut state, &mut app, "");
        remplir(&mut state, &mut app, "0.1.0");
        let mode =
          crate::handle_creation_input(entre(KeyCode::Enter), &mut state, &mut app).unwrap();
        assert!(matches!(mode, crate::InputMode::Normal), "la confirmation clôt le formulaire");
        let p = app
          .projects
          .iter()
          .find(|p| p.id == "ma-lib")
          .expect("la bibliothèque doit être créée");
        assert!(p.est_bibliotheque, "le drapeau est enregistré");
        assert_eq!(p.path, libs.join("ma-lib"));
        assert!(p.path.exists(), "le dossier existe sous la racine des bibliothèques");
        let _ = fs::remove_dir_all(libs.parent().unwrap());
      }

    /// Un popup sans projet suffit : la navigation d'onglets ne touche qu'à
    /// l'onglet actif et à l'état de saisie.
    fn popup_navigation() -> crate::Popup
      {
        crate::Popup::sans_projet(crate::Onglet::Projet, "FRA".to_string())
      }

    /// Champ tarifaire atteint après `code`, en partant de `depart`.
    fn champ_apres(popup: &crate::Popup, code: KeyCode, depart: usize) -> usize
      {
        let touche = KeyEvent::new(code, KeyModifiers::NONE);
        match crate::handle_tariff_input(
          touche,
          &mut app_avec_projet(),
          popup,
          0,
          depart,
          false,
          String::new(),
          0,
          "FRA".to_string(),
          None,
          false,
        ) {
          Ok(crate::InputMode::Popup(p)) => p.couts.active_field,
          _ => panic!("{code:?} doit rester dans l'onglet Coûts"),
        }
      }

    /// Régression : Tab n'était traité que dans l'onglet Paramètres, et
    /// Shift+Tab nulle part. Le projet ne pouvait donc pas être quitté au
    /// clavier, alors que la barre d'onglets l'annonçait.
    #[test]
    fn tab_parcourt_les_trois_onglets_et_shift_tab_revient()
      {
        let mut popup = popup_navigation();
        for attendu in [crate::Onglet::Couts, crate::Onglet::Parametres, crate::Onglet::Projet]
          {
            assert!(crate::naviguer_onglets(&mut popup, KeyCode::Tab), "Tab doit être consommé");
            assert_eq!(popup.onglet, attendu);
          }
        for attendu in [crate::Onglet::Parametres, crate::Onglet::Couts, crate::Onglet::Projet]
          {
            assert!(
              crate::naviguer_onglets(&mut popup, KeyCode::BackTab),
              "Shift+Tab doit être consommé"
            );
            assert_eq!(popup.onglet, attendu);
          }
      }

    /// Tab est le geste de navigation global : il doit mener au même onglet
    /// depuis les trois onglets, y compris Paramètres où il était le seul
    /// implémenté.
    #[test]
    fn tab_mene_partout_au_meme_onglet_suivant()
      {
        for depart in [crate::Onglet::Projet, crate::Onglet::Couts, crate::Onglet::Parametres]
          {
            let mut popup = popup_navigation().avec_onglet(depart);
            assert!(crate::naviguer_onglets(&mut popup, KeyCode::Tab));
            assert_eq!(popup.onglet, depart.suivant(), "depuis {depart:?}");
          }
      }

    /// Changer d'onglet pendant une frappe jetterait le tampon : le champ doit
    /// garder la main.
    #[test]
    fn tab_ne_change_pas_d_onglet_pendant_une_saisie()
      {
        let mut popup = popup_navigation();
        popup.projet.editing = true;
        assert!(!crate::naviguer_onglets(&mut popup, KeyCode::Tab), "saisie projet");
        assert_eq!(popup.onglet, crate::Onglet::Projet);

        let mut popup = popup_navigation().avec_onglet(crate::Onglet::Couts);
        popup.couts.editing = true;
        assert!(!crate::naviguer_onglets(&mut popup, KeyCode::Tab), "saisie tarif");
        assert_eq!(popup.onglet, crate::Onglet::Couts);

        let mut popup = popup_navigation().avec_onglet(crate::Onglet::Parametres);
        popup.parametres.edition = true;
        assert!(!crate::naviguer_onglets(&mut popup, KeyCode::Tab), "saisie pays");
        assert_eq!(popup.onglet, crate::Onglet::Parametres);
      }

    /// Seules Tab et Shift+Tab sont consommées : les autres touches doivent
    /// atteindre le handler de l'onglet.
    #[test]
    fn les_autres_touches_ne_sont_pas_consommees()
      {
        let mut popup = popup_navigation();
        for code in
          [
            KeyCode::Char('x'),
            KeyCode::Enter,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Esc,
          ]
        {
            assert!(!crate::naviguer_onglets(&mut popup, code), "{code:?} ne doit rien faire");
          }
        assert_eq!(popup.onglet, crate::Onglet::Projet);
      }

    /// Passer d'onglet ne doit rien perdre de l'onglet quitté : ni la saisie
    /// d'un tarif, ni la position du tableau.
    #[test]
    fn le_changement_d_onglet_preserve_la_saisie_tarifaire()
      {
        let mut popup = popup_navigation().avec_onglet(crate::Onglet::Couts);
        popup.couts.input_buffer = "42".to_string();
        popup.couts.scroll_offset = 7;
        assert!(crate::naviguer_onglets(&mut popup, KeyCode::Tab));
        assert_eq!(popup.onglet, crate::Onglet::Parametres);
        assert_eq!(popup.couts.input_buffer, "42", "la saisie ne doit pas être jetée");
        assert_eq!(popup.couts.scroll_offset, 7, "le défilement ne doit pas être perdu");
      }

    /// Régression : `↑`/`↓` changeaient de champ tarifaire *et* faisaient
    /// défiler le tableau, dans le même traitement de touche. Les flèches
    /// verticales appartiennent désormais au tableau seul ; le passage de champ
    /// se fait avec `←`/`→`.
    #[test]
    fn les_fleches_verticales_ne_changent_pas_de_champ_tarifaire()
      {
        let popup = popup_navigation().avec_onglet(crate::Onglet::Couts);
        for code in [KeyCode::Up, KeyCode::Down, KeyCode::Char('j'), KeyCode::Char('k')]
          {
            assert_eq!(
              champ_apres(&popup, code, popup.couts.active_field),
              popup.couts.active_field,
              "{code:?} ne doit pas changer de champ tarifaire"
            );
          }
      }

    /// `←`/`→` cyclent les trois champs tarifaires (mode, €/h, pays).
    #[test]
    fn les_fleches_horizontales_cyclent_les_champs_tarifaires()
      {
        let popup = popup_navigation().avec_onglet(crate::Onglet::Couts);
        let mut champ = popup.couts.active_field;
        for attendu in [1, 2, 0]
          {
            champ = champ_apres(&popup, KeyCode::Right, champ);
            assert_eq!(champ, attendu, "→ vers le champ {attendu}");
          }
        champ = champ_apres(&popup, KeyCode::Left, champ);
        assert_eq!(champ, 2, "cyclique inverse");
        assert_eq!(champ_apres(&popup, KeyCode::Left, popup.couts.active_field), 2, "depuis 0");
      }

    /// Un projet enregistré (« zeta ») et un dossier frère présent sur le
    /// disque mais absent du registre (« alpha »). Le rescan va découvrir
    /// « alpha », et le tri par nom le place devant « zeta ».
    ///
    /// C'est le mécanisme exact du défaut corrigé : le rescan réindexe, la
    /// liste change de longueur, et l'index que porte le popup devient caduc.
    fn app_avec_projet_decouvert() -> crate::App
      {
        static COMPTEUR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COMPTEUR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = temp_dir(&format!("rescan-{n}"));
        let zeta = dir.join("zeta");
        fs::create_dir_all(&zeta).unwrap();
        fs::create_dir_all(dir.join("alpha")).unwrap();
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "zeta": {
                    "name": "zeta",
                    "path": zeta.to_str().unwrap(),
                    "version": "1.0.0",
                    "description": "",
                    "tarif_mode": "manuel"
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let mut app = crate::App::new_light(registre, SortOrder::Name);
        // Aucun appel réseau pendant un test : le cache se déclare en échec
        // récent, ce qui interdit à `rescan_repertoire` de rafraîchir les
        // tarifs.
        app.web_cache.last_failure_ms = crate::registry::maintenant_ms();
        app
      }

    /// Le rescan doit avoir bien changé la liste, sinon le test qui suit ne
    /// prouverait rien.
    fn assert_rescan_a_deplace(app: &crate::App)
      {
        assert_eq!(app.projects.len(), 2, "le rescan doit avoir découvert alpha");
        assert_eq!(app.projects[0].name, "alpha", "tri par nom : alpha avant zeta");
        assert_eq!(app.projects[1].name, "zeta");
      }

    /// Un index seul ne survit pas à un re-tri : seul l'identifiant du projet
    /// est stable. `popup_aligne` s'en sert pour réaligner le popup.
    #[test]
    fn popup_aligne_suit_le_projet_reordonne()
      {
        let mut app = app_avec_projet_decouvert();
        assert_eq!(app.projects.len(), 1, "un seul projet au départ");
        // Le popup désigne le projet « zeta », seul, donc l'index 0.
        let popup = popup_navigation().avec_projet_idx(0);
        crate::rescan_repertoire(&mut app, &mut crate::Progress::noop()).unwrap();
        assert_rescan_a_deplace(&app);

        let aligne = crate::popup_aligne(&app, &popup, Some("zeta"));
        assert_eq!(aligne.projet_idx, 1, "zeta a changé de place, le popup doit le suivre");
        assert_eq!(app.projects[aligne.projet_idx].name, "zeta");
      }

    /// Un projet disparu laisse l'index inchangé : l'affichage le rend alors
    /// absent plutôt que valide, et surtout il ne pointe pas sur un autre
    /// projet par hasard.
    #[test]
    fn popup_aligne_laisse_un_projet_absent()
      {
        let app = app_avec_projet_decouvert();
        let popup = popup_navigation().avec_projet_idx(7);
        assert_eq!(crate::popup_aligne(&app, &popup, None).projet_idx, 7);
        assert_eq!(
          crate::popup_aligne(&app, &popup, Some("disparu")).projet_idx,
          7,
          "un identifiant inconnu ne doit pas faire pointer sur un autre projet"
        );
      }

    /// Régression : enregistrer un tarif manuel déclenche un rescan, qui peut
    /// découvrir un projet et re-trier la liste. Le popup rendu doit continuer
    /// de désigner le projet dont le tarif vient d'être enregistré.
    ///
    /// Le défaut était une affectation de l'index dans une variable locale
    /// par valeur, jamais relue : le popup repartait avec son index d'origine,
    /// et son titre et son onglet Coûts décrivaient un autre projet que
    /// l'onglet Projet.
    #[test]
    fn le_tarif_manuel_ne_decale_pas_le_projet_du_popup()
      {
        let mut app = app_avec_projet_decouvert();
        let popup = popup_navigation().avec_projet_idx(0);
        let touche = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        // Saisie du tarif manuel (champ 2), puis validation : c'est ce chemin
        // qui déclenche le rescan.
        let mode = crate::handle_tariff_input(
          touche,
          &mut app,
          &popup,
          0,
          1,
          true,
          "42,50".to_string(),
          5,
          "FRA".to_string(),
          None,
          false,
        )
        .unwrap();

        assert_rescan_a_deplace(&app);
        let crate::InputMode::Popup(p) = mode else {
          panic!("l'onglet Coûts doit rester ouvert");
        };
        assert_eq!(
          app.projects[p.projet_idx].name, "zeta",
          "le popup doit désigner le projet dont le tarif vient d'être enregistré"
        );
        assert_eq!(p.projet_idx, 1);
      }

    /// Le même chemin, par la bascule de mode : le rescan y est déclenché par
    /// la touche Entrée sur le champ 0.
    #[test]
    fn la_bascule_de_mode_ne_decale_pas_le_projet_du_popup()
      {
        let mut app = app_avec_projet_decouvert();
        let popup = popup_navigation().avec_projet_idx(0);
        let touche = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        let mode = crate::handle_tariff_input(
          touche,
          &mut app,
          &popup,
          0,
          0,
          false,
          String::new(),
          0,
          "FRA".to_string(),
          None,
          false,
        )
        .unwrap();

        assert_rescan_a_deplace(&app);
        let crate::InputMode::Popup(p) = mode else {
          panic!("l'onglet Coûts doit rester ouvert");
        };
        assert_eq!(app.projects[p.projet_idx].name, "zeta");
      }

    /// Une session en devise locale n'a pas d'équivalent à afficher : la
    /// colonne « Converti » resterait vide et la cellule combinée se
    /// répéterait deux fois.
    #[test]
    fn devise_locale_ne_redouble_pas_le_montant()
      {
        assert!(crate::devise_est_locale("EUR"));
        assert!(crate::devise_est_locale("eur"), "la casse ne tranche rien");
        assert!(!crate::devise_est_locale("USD"));
        assert!(!crate::devise_est_locale(""));
      }

    /// La cellule « Coût » affiche l'équivalent local puis le montant facturé,
    /// séparés par une espace fine — la place est comptée en colonnes, et une
    /// espace ordinaire ferait basculer la mise en page d'une colonne.
    #[test]
    fn cout_session_double_ajoute_la_conversion()
      {
        assert_eq!(
          crate::cout_session_double("4,57 $", "4,20 €"),
          "4,20 €\u{2009}·\u{2009}4,57 $"
        );
        // Pas de taux, ou coût non communiqué : le seul montant d'origine.
        assert_eq!(crate::cout_session_double("4,57 $", ""), "4,57 $");
        assert_eq!(crate::cout_session_double("—", ""), "—");
        // Un « — » en local ne double pas un montant réel.
        assert_eq!(crate::cout_session_double("4,57 $", "—"), "4,57 $");
      }

    /// Le coût nul d'une session sans grille tarifaire ne doit produire aucun
    /// montant converti : « 0,00 € · 4,57 $ » afficherait une gratuité qui
    /// n'existe pas. Les deux cellules retombent sur « — ».
    #[test]
    fn un_cout_non_communique_ne_produit_aucun_montant_local()
      {
        // `montant_local` refuse un coût nul…
        assert_eq!(crate::registry::montant_local(0.0, "USD", Some(0.92), Some(0.0)), None);
        // …donc le champ converti reste vide…
        let local = String::new();
        // …et la cellule n'affiche que le montant d'origine, déjà « — ».
        assert_eq!(crate::cout_session_double("—", &local), "—");
      }

    /// Manifeste et sources : le décor minimal qu'un bump a le droit d'écrire.
    fn projet_a_bumper(dir: &Path) -> PathBuf {
        let projet = dir.join("projet");
        fs::create_dir_all(projet.join("src")).unwrap();
        fs::write(
            projet.join("Cargo.toml"),
            "[package]\nname = \"projet\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(projet.join("src/main.rs"), "fn main() {}\n").unwrap();
        projet
    }

    fn app_sur(dir: &Path, projet: &Path) -> crate::App {
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "projet": {
                    "name": "projet",
                    "path": projet.to_str().unwrap(),
                    "version": "1.0.0",
                    "version_debug": "1.0.0",
                    "description": "",
                    "tarif_mode": "manuel"
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let mut app = crate::App::new_light(registre, SortOrder::Modified);
        crate::refresh_project_selection(&mut app);
        app.list_state.select(Some(0));
        app
    }

    #[test]
    fn le_bump_manuel_passe_de_1_0_0_a_1_0_1() {
        let dir = temp_dir("bump");
        let projet = projet_a_bumper(&dir);
        let mut app = app_sur(&dir, &projet);

        crate::demander_bump(&mut app);
        assert_eq!(app.bump_en_attente, Some(0), "`b` mémorise la sélection");
        assert!(
            app.status_messages.join(" ").contains('o'),
            "la demande annonce la `o` de confirmation"
        );

        crate::confirmer_bump(&mut app);
        assert_eq!(app.bump_en_attente, None, "plus rien en attente");
        assert_eq!(app.projects[0].version_debug, "1.0.1");
        assert_eq!(app.projects[0].version, "1.0.1");
        let manifeste = fs::read_to_string(projet.join("Cargo.toml")).unwrap();
        assert!(
            manifeste.contains("version = \"1.0.1\""),
            "Cargo.toml doit être réécrit :\n{manifeste}"
        );
        assert!(
            app.status_messages.join(" ").contains("1.0.1"),
            "le bilan annonce la version atteinte"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn le_bump_resynchronise_le_hash_pour_ne_pas_rebumper() {
        let dir = temp_dir("bump-hash");
        let projet = projet_a_bumper(&dir);
        let mut app = app_sur(&dir, &projet);

        crate::demander_bump(&mut app);
        crate::confirmer_bump(&mut app);
        assert_eq!(
            app.projects[0].source_hash,
            crate::registry::compute_source_hash(&projet),
            "hash non resynchronisé : le rescan suivant rebumperait"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn le_bump_annule_ne_rien_ecrit() {
        let dir = temp_dir("bump-annule");
        let projet = projet_a_bumper(&dir);
        let mut app = app_sur(&dir, &projet);

        crate::demander_bump(&mut app);
        crate::annuler_bump(&mut app);
        assert_eq!(app.bump_en_attente, None);
        assert_eq!(app.projects[0].version_debug, "1.0.0");
        assert!(app.status_messages.is_empty(), "l'annulation vide la zone");
        let manifeste = fs::read_to_string(projet.join("Cargo.toml")).unwrap();
        assert!(
            manifeste.contains("version = \"1.0.0\""),
            "une annulation ne touche pas au manifeste"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn le_bump_sans_selection_ne_demande_rien() {
        let dir = temp_dir("bump-vide");
        let projet = projet_a_bumper(&dir);
        let mut app = app_sur(&dir, &projet);
        app.list_state.select(None);

        crate::demander_bump(&mut app);
        assert_eq!(
            app.bump_en_attente, None,
            "sans projet, aucune confirmation ne doit être armée"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    /// Un projet unique dans `dir/projets`, avec les deux racines dirigées
    /// vers le temporaire : aucun test ne doit toucher au vrai `~/projets`.
    fn decor_archive(dir: &Path) -> (crate::App, PathBuf) {
        let racine = dir.join("projets");
        let projet = racine.join("foo");
        fs::create_dir_all(projet.join("src")).unwrap();
        fs::write(
            projet.join("Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        fs::write(projet.join("src/main.rs"), "fn main() {}\n").unwrap();
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "foo": {
                    "name": "foo",
                    "path": projet.to_str().unwrap(),
                    "version": "1.0.0",
                    "description": "",
                    "tarif_mode": "manuel"
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let mut app = crate::App::new_light(registre, SortOrder::Name);
        app.archive_root = dir.join("archives-projets");
        app.projets_root = racine.clone();
        crate::refresh_project_selection(&mut app);
        app.list_state.select(Some(0));
        (app, racine)
    }

    #[test]
    fn l_archivage_sort_le_dossier_de_la_racine_des_projets() {
        let dir = temp_dir("archiver");
        let (mut app, racine) = decor_archive(&dir);

        let message = app.archiver_selection().expect("archivage possible");
        assert!(message.contains("archivé"), "bilan explicite : {message}");
        assert!(
            !racine.join("foo").exists(),
            "le dossier doit quitter la racine des projets"
        );
        assert!(
            dir.join("archives-projets/foo").exists(),
            "le dossier doit atterrir dans l'archive"
        );
        assert!(app.est_archive(0), "le chemin dit désormais l'archive");
        assert!(
            app.filtered.is_empty(),
            "un archivé disparaît de la liste normale"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn la_vue_archives_ne_montre_que_les_archives() {
        let dir = temp_dir("vue-archives");
        let (mut app, _racine) = decor_archive(&dir);
        app.archiver_selection().expect("archivage possible");

        app.basculer_vue_archives();
        assert_eq!(app.filtered, vec![0], "l'archivé réapparaît dedans");
        assert!(!app.show_new, "pas de création depuis les archives");
        assert_eq!(
            app.list_state.selected(),
            Some(0),
            "la vue d'archives se sélectionne toute seule"
        );

        app.basculer_vue_archives();
        assert!(
            app.filtered.is_empty(),
            "la liste normale ne revoit pas l'archivé"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn la_restoration_ramene_le_dossier_dans_la_racine() {
        let dir = temp_dir("restaurer");
        let (mut app, racine) = decor_archive(&dir);
        app.archiver_selection().expect("archivage possible");
        app.basculer_vue_archives();

        let message = app.restaurer_selection().expect("restauration possible");
        assert!(message.contains("restauré"), "bilan explicite : {message}");
        assert!(
            racine.join("foo").exists(),
            "le dossier revient là où il vivait"
        );
        assert!(!app.est_archive(0));
        assert!(
            !dir.join("archives-projets/foo").exists(),
            "l'archive se vide bien"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    /// Les archives ne doivent jamais emporter la racine : leur nombre ferait
    /// basculer le rescan vers `archive_root`, qui réindexerait alors comme
    /// « trouvés » les projets qui y dorment.
    #[test]
    fn la_racine_des_projets_ignore_les_archives() {
        let dir = temp_dir("racine-archives");
        let (mut app, racine) = decor_archive(&dir);
        // Deux projets actifs en plus, pour que l'archive soit en minorité
        // même si le décompte ne l'excluait pas.
        for nom in ["beta", "gamma"] {
            let p = racine.join(nom);
            fs::create_dir_all(&p).unwrap();
            app.projects.push(crate::registry::ProjectEntry {
                id: nom.to_string(),
                name: nom.to_string(),
                path: p,
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
                last_session_cost_local: String::new(),
                last_agent: String::new(),
                last_time_created: String::new(),
                last_session_context_pct: String::new(),
                last_duree: String::new(),
                total_duree: String::new(),
                last_valorisation: String::new(),
                total_valorisation: String::new(),
                tarif_resume: String::new(),
                status_debug: String::new(),
                version_debug: String::new(),
                status_release: String::new(),
                version_release: String::new(),
                date_debug: String::new(),
                date_release: String::new(),
                probe_debug: String::new(),
                probe_release: String::new(),
                build_debug: String::new(),
                build_release: String::new(),
                debug_modified: String::new(),
                release_modified: String::new(),
                last_compile_debug_secs: 0,
                last_compile_release_secs: 0,
                last_target_debug_secs: 0,
                last_target_release_secs: 0,
                debug_snapshot: None,
                release_snapshot: None,
                version_manually_edited: false,
                source_hash: String::new(),
                tarif_mode: "manuel".to_string(),
                tarif_manual: 0.0,
                tarif_web_series: Vec::new(),
                tarif_web_fetched_ms: 0,
                tarif_web_secteur: false,
                tarif_web_pays: String::new(),
            est_bibliotheque: false,
            typologie: "Other".to_string(),
            });
        }
        app.archiver_selection().expect("archivage possible");

        let trouve = crate::projects_root(&app.projects, &app.archive_root)
            .expect("une racine doit rester");
        assert_eq!(
            trouve, racine,
            "les archives ne doivent pas peser dans le décompte"
        );

        fs::remove_dir_all(&dir).unwrap();
    }

    /// Un « binaire » simulé sous `target/<profil>`, dont le mtime fait foi.
    fn binaire_simule(profil: &str, projet: &Path) -> PathBuf {
        let bin = projet
            .join("target")
            .join(profil)
            .join("opencode-start-menu");
        fs::create_dir_all(bin.parent().unwrap()).unwrap();
        fs::write(&bin, "binaire simulé").unwrap();
        bin
    }

    /// Fixe l'horodatage de modification d'un fichier au temps voulu.
    fn horodater(p: &Path, t: std::time::SystemTime) {
        use std::fs::FileTimes;
        fs::File::options()
            .write(true)
            .open(p)
            .unwrap()
            .set_times(FileTimes::new().set_modified(t))
            .unwrap();
    }

    /// Registre d'un projet dont la fiche Release est volontairement décalée
    /// d'une snapshot de build validé : la garde de chargement doit la rétablir.
    fn registre_release_derivee(dir: &Path, projet: &Path) -> PathBuf {
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "projet": {
                    "name": "projet",
                    "path": projet.to_str().unwrap(),
                    "version": "1.0.0",
                    "version_debug": "1.0.0",
                    "version_release": "9.9.9",
                    "status_release": "Release",
                    "description": "",
                    "tarif_mode": "manuel",
                    "release_snapshot": {
                        "version": "0.20.24",
                        "statut": "Beta-Release",
                        "compile_secs": 1700000000
                    }
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        binaire_simule("release", projet);
        horodater(
            &projet.join("target/release/opencode-start-menu"),
            t,
        );
        registre
    }

    /// La fiche Release dérive de sa propre snapshot validée mais le binaire
    /// est intact : au chargement, la vérité du dernier build reprend le dessus.
    #[test]
    fn le_chargement_restaure_la_release_sur_la_verite_du_binaire() {
        let dir = temp_dir("verite-release");
        let projet = projet_a_bumper(&dir);
        let registre = registre_release_derivee(&dir, &projet);
        let mut app = crate::App::new_light(registre, SortOrder::Modified);

        let (restaurees, alertes) = crate::valider_metadonnees_chargement(&mut app);

        assert_eq!(app.projects[0].version_release, "0.20.24");
        assert_eq!(app.projects[0].status_release, "Beta-Release");
        assert_eq!(restaurees, 2, "{alertes:?}");
        assert!(alertes.iter().any(|a| a.contains("Release")), "{alertes:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Un binaire plus récent que la fiche laisse la Release au rattrapage du
    /// rescan : la garde ne force rien à son insu.
    #[test]
    fn le_chargement_ne_restaure_pas_une_release_au_binaire_present() {
        let dir = temp_dir("verite-release-nouveau");
        let projet = projet_a_bumper(&dir);
        let registre = registre_release_derivee(&dir, &projet);
        // Un second fichier, plus récent que la snapshot : le binaire simulé a
        // changé sur le disque, la fiche n'a plus autorité.
        let autre = projet.join("target/release/contenu-nouveau");
        fs::write(&autre, "fraîchement écrit").unwrap();
        let mut app = crate::App::new_light(registre, SortOrder::Modified);

        let (restaurees, _) = crate::valider_metadonnees_chargement(&mut app);

        assert_eq!(restaurees, 0, "aucune restauration malgré la fiche divergente");
        assert_eq!(app.projects[0].version_release, "9.9.9", "le rattrapage tranchera");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// La variante Debug ne suit jamais un bump posé en avant de la dernière
    /// vérité : une version plus jeune que le dernier build n'est pas rétablie.
    #[test]
    fn le_chargement_ne_rebute_pas_un_bump_debug_en_avant() {
        let dir = temp_dir("verite-debug-avant");
        let projet = projet_a_bumper(&dir);
        let bin = binaire_simule("debug", &projet);
        horodater(&bin, std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000));
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "projet": {
                    "name": "projet",
                    "path": projet.to_str().unwrap(),
                    "version": "2.0.0",
                    "version_debug": "2.0.0",
                    "description": "",
                    "tarif_mode": "manuel",
                    "debug_snapshot": {
                        "version": "1.0.0",
                        "statut": "Debug",
                        "compile_secs": 1700000000
                    }
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let mut app = crate::App::new_light(registre, SortOrder::Modified);

        let (restaurees, _) = crate::valider_metadonnees_chargement(&mut app);

        assert_eq!(restaurees, 0, "la 2.0.0 (bump en avant) a raison sur la fiche");
        assert_eq!(app.projects[0].version_debug, "2.0.0");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// L'inverse : une version Debug retombée derrière le dernier build validé
    /// (fiche éditable à la main) est relevée à la vérité du disque.
    #[test]
    fn le_chargement_releve_une_version_debug_retombee_derriere() {
        let dir = temp_dir("verite-debug-arriere");
        let projet = projet_a_bumper(&dir);
        let bin = binaire_simule("debug", &projet);
        horodater(&bin, std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000));
        let registre = dir.join("registre.json");
        let json = serde_json::json!({
            "projects": {
                "projet": {
                    "name": "projet",
                    "path": projet.to_str().unwrap(),
                    "version": "0.9.0",
                    "version_debug": "0.9.0",
                    "description": "",
                    "tarif_mode": "manuel",
                    "debug_snapshot": {
                        "version": "1.0.0",
                        "statut": "Debug",
                        "compile_secs": 1700000000
                    }
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let mut app = crate::App::new_light(registre, SortOrder::Modified);

        let (restaurees, alertes) = crate::valider_metadonnees_chargement(&mut app);

        assert_eq!(app.projects[0].version_debug, "1.0.0");
        assert_eq!(app.projects[0].version, "1.0.0");
        assert_eq!(restaurees, 1, "{alertes:?}");
        assert!(alertes.iter().any(|a| a.contains("Debug")), "{alertes:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

  }

