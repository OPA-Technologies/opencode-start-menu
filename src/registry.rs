use anyhow::{anyhow, Result};
use chrono::Datelike;
use reqwest::Client;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use crate::perms;

static HTTP_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .pool_max_idle_per_host(0)
        .build()
        .unwrap_or_else(|_| Client::new())
});

const LOCAL_CURRENCY: &str = "EUR";

/// Cache des taux de change, verrouillé.
///
/// Un mutex empoisonné ne rend pas ce cache inutilisable : les blocs
/// verrouillés se contentent de lire ou d'insérer une entrée, aucun ne peut
/// paniquer. Le verrou est donc récupéré par `into_inner()` plutôt que
/// laissé en `unwrap()` : sinon un simple prix manquant ferait pancreatic le
/// menu entier, y compris pour un chemin qui n'a rien à voir avec la devise.
static EXCHANGE_RATE_CACHE: LazyLock<std::sync::Mutex<HashMap<String, f64>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

async fn fetch_exchange_rate(from: &str, to: &str) -> Option<f64> {
    if from == to {
        return Some(1.0);
    }
    let cache_key = format!("{from}->{to}");
    {
        let cache = EXCHANGE_RATE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(&rate) = cache.get(&cache_key) {
            return Some(rate);
        }
    }

    let url = format!("https://api.frankfurter.app/latest?from={from}&to={to}");
    let response = HTTP_CLIENT
        .get(&url)
        .timeout(std::time::Duration::from_millis(HTTP_TIMEOUT_MS))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let json: serde_json::Value = response.json().await.ok()?;
    let rate = json.get("rates")?.get(to)?.as_f64()?;

    if rate > 0.0 {
        let mut cache = EXCHANGE_RATE_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.insert(cache_key, rate);
        Some(rate)
    } else {
        None
    }
}

fn fallback_exchange_rate(from: &str, to: &str) -> Option<f64> {
    match (from, to) {
        ("USD", "EUR") => Some(0.92),
        ("EUR", "USD") => Some(1.09),
        ("GBP", "EUR") => Some(1.18),
        ("EUR", "GBP") => Some(0.85),
        ("JPY", "EUR") => Some(0.0062),
        ("EUR", "JPY") => Some(161.0),
        _ if from == to => Some(1.0),
        _ => None,
    }
}

/// Montant d'une session exprimé en `LOCAL_CURRENCY`.
///
/// Trois sources sont essayées dans l'ordre, du plus fiable au moins fiable :
/// le montant déjà converti écrit par le registre, le taux stocké avec la
/// session, puis le taux de repli embarqué. Les deux premières viennent des
/// colonnes de `session` ; la troisième permet d'afficher un total quand
/// opencode n'a encore rien écrit pour cette devise.
///
/// Le cumul des totaux et l'affichage de la dernière session passent tous deux
/// par cette fonction : c'est la seule façon d'éviter que la ligne « Converti »
/// et la ligne TOTAL ne se contredisent quand un taux change entre deux scans.
///
/// Renvoie `None` si la devise n'a aucun taux connu, ou si le coût est nul :
/// opencode écrit `cost = 0` quand le fournisseur ne publie aucune grille, et
/// convertir ce zéro en « 0,00 € » afficherait un montant qui n'existe pas.
///
/// Un taux stocké mais aberrant (infini, négatif) est écarté au profit du taux
/// de repli : il rendrait le total affiché illisible.
pub(crate) fn montant_local(
  cost: f64,
  devise: &str,
  exchange_rate: Option<f64>,
  cost_local: Option<f64>,
) -> Option<f64>
  {
    if cost <= 0.0 {
      return None;
    }
    if let Some(local) = cost_local.filter(|v| v.is_finite() && *v > 0.0) {
      return Some(local);
    }
    let taux = exchange_rate
      .filter(|t| t.is_finite() && *t > 0.0)
      .or_else(|| fallback_exchange_rate(devise, LOCAL_CURRENCY))
      .filter(|t| t.is_finite() && *t > 0.0)?;
    Some(cost * taux)
  }

// Nouvelles structures pour les données d'usage
#[derive(Debug, Clone, Default)]
pub(crate) struct LastSession {
    pub tokens: u64,
    pub cost: f64,
    pub currency: String,
    pub exchange_rate: Option<f64>,
    pub cost_local: Option<f64>,
    pub model: String,
    pub agent: String,
    pub time_created: i64,
    pub context_pct: u8,
    /// Durée de la session (ms), dérivée des messages
    /// (MAX(time_updated) − MIN(time_created) par session).
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CumulativeUsage {
    pub total_tokens: u64,
    pub cost_by_currency: HashMap<String, f64>,
    pub total_cost_local: f64,
    pub session_count: usize,
    /// Temps cumulé (ms) sur l'ensemble des sessions du projet.
    pub total_duration_ms: u64,
}

/// Une session horodatée + sa durée (pour la valorisation carry-forward).
#[derive(Debug, Clone, Default)]
pub(crate) struct SessionDuree {
    pub time_created_ms: i64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ProjectUsage {
    pub cumulative: CumulativeUsage,
    pub last_session: Option<LastSession>,
    /// Détail par session (année + durée) pour le carry-forward du tarif.
    pub sessions: Vec<SessionDuree>,
}

// --- Tarif de valorisation du temps passé -----------------------------

/// Point d'une série datée de tarif horaire (en EUR/heure).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TarifPoint {
    pub annee: i32,
    pub valeur_eur_h: f64,
    /// Devise d'origine côté source (ex. "EUR", "USD") et taux appliqué.
    pub devise_src: String,
    pub tx_eur: f64,
    /// Horodatage de récupération de ce point (ms).
    pub fetched_ms: i64,
}

impl Default for TarifPoint
  {
    fn default() -> Self
      {
        Self {
            annee: 0,
            valeur_eur_h: 0.0,
            devise_src: String::new(),
            tx_eur: 1.0,
            fetched_ms: 0,
        }
      }
  }

/// Sélection carry-forward : le point de la série dont l'année est la plus
/// récente ≤ à la date donnée (ms). S'il n'existe aucun point antérieur,
/// on prend le premier point connu de la série.
pub(crate) fn tarif_point_for_ms(serie: &[TarifPoint], ts_ms: i64) -> Option<TarifPoint>
  {
    if serie.is_empty()
      {
        return None;
      }
    let dt = chrono::DateTime::from_timestamp(ts_ms / 1000, 0)?;
    let annee = dt.year();
    serie
      .iter()
      .filter(|p| p.annee <= annee)
      .max_by_key(|p| p.annee)
      .or_else(|| serie.iter().min_by_key(|p| p.annee))
      .cloned()
  }

fn default_tarif_mode() -> String {
    "web".to_string()
  }

/// Cliché des métadonnées d'une variante, posé uniquement par un build validé
/// (popup de détection, rattrapage, reconstruction documentée).
///
/// C'est la preuve « vérité du disque » : tant que le binaire n'a pas été
/// recompilé (mtime égal à `compile_secs`, version compilée égale à `version`),
/// les métadonnées de la variante ne doivent pas bouger. Une fiche présente et
/// cohérente immunise le registre contre toute écriture manuelle non marquée ;
/// une fiche absente laisse la sonde `-V` du binaire trancher.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub(crate) struct SnapshotCompile {
    /// Version compilée du binaire au moment du build validé.
    pub(crate) version: String,
    /// Statut affiché par le binaire (`env!("CARGO_PKG_VERSION")`-age du menu).
    pub(crate) statut: String,
    /// Date de dernière mise à jour déclarée (`jj-mm-yyyy`), vide hors contrat.
    /// Champ ajouté après coup : les anciennes fiches le migrent vide.
    #[serde(default)]
    pub(crate) date: String,
    /// Vérité du disque : horodatage (epoch secondes) du binaire mis en build.
    pub(crate) compile_secs: u64,
    /// Empreinte SHA-256 de la ligne `-V`, signal fin de départage (§B7). Vide
    /// hors contrat et sur les fiches antérieures à son introduction.
    #[serde(default)]
    pub(crate) empreinte_v: String,
}

/// Refus utilisateur d'une action de cohérence, conservé pour re-notification.
///
/// Le refus se rattache à la **clé de l'écart** (voir `coherence::Ecart::cle`) :
/// tant que l'écart reste identique, la proposition est re-présentée marquée
/// « refusée » ; si l'écart change, la clé change et le refus ne s'applique
/// plus.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub(crate) struct RefusCoherence {
    /// Clé de l'écart refusé.
    #[serde(default)]
    pub(crate) ecart: String,
    /// Code stable de l'action refusée (voir `coherence::Action::code`).
    #[serde(default)]
    pub(crate) action: String,
    /// Date du refus (`jj-mm-yyyy`).
    #[serde(default)]
    pub(crate) quand: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub(crate) struct Project {
    name: String,
    path: String,
    description: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    token: String,
    #[serde(default)]
    used_pct: String,
    #[serde(default)]
    spent: String,
    #[serde(default)]
    last_model: String,
    #[serde(default)]
    total_tokens: String,
    #[serde(default)]
    total_cost: String,
    #[serde(default)]
    session_count: String,
    #[serde(default)]
    last_session_tokens: String,
    #[serde(default)]
    last_session_cost: String,
    /// Montant de la dernière session converti en `LOCAL_CURRENCY`, pour
    /// l'onglet Coûts. Vide quand aucun taux n'est connu.
    #[serde(default)]
    last_session_cost_local: String,
    #[serde(default)]
    last_agent: String,
    #[serde(default)]
    last_time_created: String,
    #[serde(default)]
    status_debug: String,
    #[serde(default)]
    version_debug: String,
    #[serde(default)]
    status_release: String,
    #[serde(default)]
    version_release: String,
    /// Date de dernière mise à jour déclarée de la variante Debug
    /// (`jj-mm-yyyy`). Complétée au chargement : fiche, puis mtime de build,
    /// puis date du dossier, puis date du jour.
    #[serde(default)]
    date_debug: String,
    /// Date de dernière mise à jour déclarée de la variante Release
    /// (`jj-mm-yyyy`). Hérite de Debug quand la Release n'a jamais été
    /// construite (`last_compile_release_secs` vaut 0).
    #[serde(default)]
    date_release: String,
    /// Commande de sonde `-V` pour la variante Debug (vide = auto). Sert aux
    /// projets dont l'artefact n'est pas construit par Cargo.
    #[serde(default)]
    probe_debug: String,
    /// Commande de sonde `-V` pour la variante Release (vide = auto).
    #[serde(default)]
    probe_release: String,
    /// Commande de build de la variante Debug (vide = auto : Cargo).
    #[serde(default)]
    build_debug: String,
    /// Commande de build de la variante Release (vide = auto : Cargo).
    #[serde(default)]
    build_release: String,
    #[serde(default)]
    last_compile_debug_secs: u64,
    #[serde(default)]
    last_compile_release_secs: u64,
    /// Epoch secs du dernier mtime de cible `target/<profil>` pris en compte
    /// pour un projet **sans sonde `-V` exploitable**. Sert de référence
    /// anti-boucle : tant que le mtime de la cible n'a pas dépassé cette
    /// valeur, la date de base n'est pas réécrite.
    #[serde(default)]
    last_target_debug_secs: u64,
    #[serde(default)]
    last_target_release_secs: u64,
    // Champ historique (uniquement pour migrer les anciens registres).
    #[serde(default)]
    last_compile_secs: u64,
    // Marque si la version a été éditée manuellement (pour éviter AutoPatch bump).
    #[serde(default)]
    version_manually_edited: bool,
    // Hash SHA256 des sources (Cargo.toml + src/**/*.rs) pour détecter les
    // modifications de code sans dépendre du mtime de target/debug.
    #[serde(default)]
    source_hash: String,
    // Snapshots de build validé par variante (voir SnapshotCompile). Absents
    // dans les anciens registres : le champ est alors migré par défaut.
    #[serde(default)]
    debug_snapshot: Option<SnapshotCompile>,
    #[serde(default)]
    release_snapshot: Option<SnapshotCompile>,
    #[serde(default = "default_true")]
    active: bool,
    // --- Valorisation du temps passé ------------------------------------
    // Mode : "web" (source OIT/ILOSTAT) ou "manuel" (tarif saisi €/h).
    #[serde(default = "default_tarif_mode")]
    tarif_mode: String,
    #[serde(default)]
    tarif_manual: f64,
    // Série datée récupérée de la source web (année → €/h).
    #[serde(default)]
    tarif_web_series: Vec<TarifPoint>,
    // Horodatage (ms) de la dernière récupération web pour ce projet.
    #[serde(default)]
    tarif_web_fetched_ms: i64,
    /// true = série sectorielle ISIC J, false = repli national (tous secteurs).
    #[serde(default)]
    tarif_web_secteur: bool,
    // Pays ISO3 auquel se rapporte la série ci-dessus. Permet de détecter
    // un changement de pays source sans comparer les contenus de série.
    #[serde(default)]
    tarif_web_pays: String,
    // --- Affichages derived du temps passé et de la valorisation ----------
    // Recalculés à chaque rescan, mais persistés pour que le registre reste
    // cohérent pour toute relecture hors rescan.
    #[serde(default)]
    last_session_context_pct: String,
    #[serde(default)]
    last_duree: String,
    #[serde(default)]
    total_duree: String,
    #[serde(default)]
    last_valorisation: String,
    #[serde(default)]
    total_valorisation: String,
    #[serde(default)]
    tarif_resume: String,
    /// Mention explicite « bibliothèque » : une entrée vivant sous la racine
    /// des bibliothèques l'est de droit, mais la mention permet d'honorer un
    /// projet hors de cette racine.
    #[serde(default)]
    est_bibliotheque: bool,
    /// Typologie du projet (voir [`TYPOLOGIES`]). Vide dans un ancien registre :
    /// migrée à la lecture par auto-détection.
    #[serde(default)]
    typologie: String,
    /// Refus de cohérence conservés (voir [`RefusCoherence`]). Gérés par
    /// [`save_refus`], préservés par [`save_registry_full`].
    #[serde(default)]
    refus_coherence: Vec<RefusCoherence>,
}

fn default_true() -> bool {
    true
}

pub(crate) const STATUSES: &[&str] = &[
    "Study",
    "Alpha",
    "Beta",
    "Debug",
    "Beta-Release",
    "Release",
    "Stable",
    "Deprecated",
];

pub(crate) fn validate_version(v: &str) -> bool {
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

pub(crate) fn validate_status(s: &str) -> bool {
    STATUSES.contains(&s)
}

/// Typologie d'un projet : sa nature, pas sa variante. Fermée et stable, elle
/// décide notamment si op doit confier à opencode l'écriture des constantes
/// (`Code-*`) ou si la date de cible au mtime suffit.
pub(crate) const TYPOLOGIES: &[&str] = &[
    "Code-App",
    "Code-Lib",
    "Doc-Lib",
    "Doc-office",
    "Other",
];

/// Typologie de repli quand rien ne permet de trancher.
pub(crate) const TYPOLOGIE_DEFAUT: &str = "Other";

pub(crate) fn validate_typologie(s: &str) -> bool {
    TYPOLOGIES.contains(&s)
}

/// Typologie auto-détectée depuis le contenu du dossier : du code s'il y a des
/// sources (`src/`), une application si un binaire est déclaré, une
/// bibliothèque sinon. Sans source, `Other` — la détection ne devine pas un
/// document.
pub(crate) fn typologie_auto(dir: &Path) -> &'static str {
    let a_src = dir.join("src").is_dir();
    let a_manifeste = dir.join("Cargo.toml").is_file();
    if a_src || a_manifeste {
        // `src/main.rs` ou un `[[bin]]` : application ; sinon bibliothèque.
        if dir.join("src/main.rs").is_file() || dir.join("src/bin").is_dir() {
            "Code-App"
        } else {
            "Code-Lib"
        }
    } else {
        TYPOLOGIE_DEFAUT
    }
}

/// Tarif horaire manuel recevable : fini, et positif ou nul.
///
/// `f64::INFINITY` satisfait `>= 0.0`, et `serde_json` le sérialise en `null`
/// **sans signaler d'erreur**. Le registre écrit est alors illisible au
/// rechargement (« invalid type: null, expected f64 »), ce qui bloque toute
/// écriture ultérieure : plus aucun projet n'est alors accessible. `NaN` échoue
/// à toute comparaison, mais par un autre chemin. Les deux sont donc refusés
/// d'entrée, à la saisie comme à l'écriture.
pub(crate) fn tarif_valide(v: f64) -> bool {
    v.is_finite() && v >= 0.0
}

/// Libellés des lignes du panneau de détail, dans leur ordre d'affichage.
///
/// Source unique : le panneau de détail filtre sur ces libellés, et l'onglet
/// Paramètres bascule les mêmes. Ajouter une ligne au tableau impose
/// de l'ajouter ici, sinon l'option « colonnes visibles » ne la verra pas.
pub(crate) const TOUTES_COLONNES: [&str; 9] = [
  "Tokens",
  "Contexte",
  "Coût",
  "Temps",
  "Valorisation",
  "Modèle",
  "Agent",
  "Date / Heure",
  "Sessions",
];

fn colonnes_par_defaut() -> Vec<String> {
  TOUTES_COLONNES.iter().map(|s| s.to_string()).collect()
}

fn defaut_oui() -> bool {
  true
}

/// Racine par défaut des bibliothèques de code réutilisable.
///
/// Stockée avec `~` pour suivre le changement de dossier personnel, comme
/// les chemins de projets.
fn defaut_lib_root() -> String
  {
    "~/projets/Libs".to_string()
  }

/// Ordre de tri mémorisé. Voluntaryment distinct de `SortOrder` (côté clap) :
/// celui-ci dépend de la ligne de commande, celui-là du registre.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Tri {
  /// Par dernière modification, le plus ancien d'abord.
  #[default]
  Date,
  /// Alphabétique.
  Nom,
}

/// Préférences globales, hors projet.
///
/// `#[serde(default)]` sur le champ du registre rend le bloc entièrement
/// optionnel : un registre écrit avant son introduction se charge tel quel,
/// sans migration, et reçoit les valeurs par défaut.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct Preferences {
  /// Ordre de tri de la liste des projets.
  #[serde(default)]
  pub(crate) tri: Tri,
  /// Libellés des lignes visibles dans le panneau de détail.
  #[serde(default = "colonnes_par_defaut")]
  pub(crate) colonnes: Vec<String>,
  /// Lancer par défaut via `sudo -i`.
  #[serde(default)]
  pub(crate) launch_root: bool,
  /// Laisser le wrapper extraire contexte et coûts. Désactiver désactive donc
  /// l'écriture de `context_pct`, `currency`, `exchange_rate` et `cost_local`.
  ///
  /// Vrai par défaut : c'est l'état d'origine du programme, et désactiver la
  /// capture fait perdre des données.
  #[serde(default = "defaut_oui")]
  pub(crate) capture: bool,
  /// Racine des bibliothèques de code réutilisable. Résolue (dégradation du
  /// `~`) par `lib_root_resolu`.
  #[serde(default = "defaut_lib_root")]
  pub(crate) lib_root: String,
}

impl Default for Preferences {
  fn default() -> Self
    {
      Self { tri: Tri::default(), colonnes: colonnes_par_defaut(), launch_root: false, capture: true, lib_root: defaut_lib_root() }
    }
}

impl Preferences {
  /// Dossier racine effectif des bibliothèques.
  pub(crate) fn lib_root_resolu(&self) -> PathBuf
    {
      PathBuf::from(shellexpand::tilde(&self.lib_root).to_string())
    }
  /// Le libellé est-il visible ?
  ///
  /// Une liste vide est interprétée comme « aucune colonne », pas comme « tout » :
  /// c'est le seul cas où l'utilisateur a réellement tout masqué. Une liste
  /// contenant un libellé inconnu (ligne ajoutée par une version ultérieure)
  /// est simplement ignorée.
  pub(crate) fn colonne_visible(&self, libelle: &str) -> bool
    {
      self.colonnes.iter().any(|c| c == libelle)
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct Registry {
    #[serde(default)]
    projects: HashMap<String, Project>,
    // --- Préférences globales (onglet Paramètres) ----------------------
    #[serde(default)]
    preferences: Preferences,
    // --- Cache global web des tarifs OIT/ILOSTAT ------------------------
    // Par pays (ISO3) → série datée récupérée. Évitée des re-téléchargements.
    #[serde(default)]
    web_rates_cache: HashMap<String, Vec<TarifPoint>>,
    // Horodatage (ms) de la dernière récupération web (toutes séries).
    #[serde(default)]
    web_last_fetch_ms: i64,
    // Pays utilisé par défaut pour la source web (ISO3, ex. "FRA").
    #[serde(default)]
    web_country: String,
    // Pays ISO3 → granularité obtenue (true : secteur J, false : national).
    #[serde(default)]
    web_secteur: HashMap<String, bool>,
    // Horodatage (ms) du dernier échec de récupération (0 = aucun échec en cours).
    #[serde(default)]
    web_last_failure_ms: i64,
    // Tout champ que cette version ne connaît pas est conservé tel quel, puis
    // réécrit. Sans cela, réécrire le registre pour changer une préférence
    // effacerait silencieusement les données ajoutées par une version plus
    // récente — le registre est un fichier partagé entre plusieurs versions.
    #[serde(flatten)]
    champs_inconnus: HashMap<String, serde_json::Value>,
}

#[derive(Clone)]
pub(crate) struct ProjectEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) description: String,
    pub(crate) status: String,
    pub(crate) version: String,
    pub(crate) token: String,
    pub(crate) used_pct: String,
    pub(crate) spent: String,
    pub(crate) last_model: String,
    pub(crate) modified: String,
    // Nouveaux champs pour les données d'usage
    pub(crate) total_tokens: String,
    pub(crate) total_cost: String,
    pub(crate) session_count: String,
    pub(crate) last_session_tokens: String,
    pub(crate) last_session_cost: String,
    /// Montant de la dernière session converti en `LOCAL_CURRENCY`.
    /// Volontairement distinct de `last_session_cost`, qui reste le montant
    /// d'origine : l'onglet Coûts en déduit la devise en cherchant son
    /// symbole dans cette chaîne.
    pub(crate) last_session_cost_local: String,
    /// L'entrée est une bibliothèque de code réutilisable.
    pub(crate) est_bibliotheque: bool,
    /// Typologie du projet (voir [`TYPOLOGIES`]).
    pub(crate) typologie: String,
    pub(crate) last_agent: String,
    pub(crate) last_time_created: String,
    pub(crate) last_session_context_pct: String,
    // --- Valorisation du temps passé (champs calculés, non persistés) ---
    /// Durée de la dernière session, formatée (« 1 h 23 min »).
    pub(crate) last_duree: String,
    /// Temps cumulé sur toutes les sessions, formaté.
    pub(crate) total_duree: String,
    /// Valorisation de la dernière session (« 31,20 € (OIT·J·2025) »).
    pub(crate) last_valorisation: String,
    /// Valorisation du temps cumulé.
    pub(crate) total_valorisation: String,
    /// Résumé du tarif en vigueur, affiché dans le popup (touche `t`).
    pub(crate) tarif_resume: String,
    pub(crate) status_debug: String,
    pub(crate) version_debug: String,
    pub(crate) status_release: String,
    pub(crate) version_release: String,
    /// Date de mise à jour déclarée de la variante Debug (`jj-mm-yyyy`).
    pub(crate) date_debug: String,
    /// Date de mise à jour déclarée de la variante Release (`jj-mm-yyyy`).
    pub(crate) date_release: String,
    /// Commande de sonde `-V` de la variante Debug (vide = auto).
    pub(crate) probe_debug: String,
    /// Commande de sonde `-V` de la variante Release (vide = auto).
    pub(crate) probe_release: String,
    /// Commande de build de la variante Debug (vide = auto : Cargo).
    pub(crate) build_debug: String,
    /// Commande de build de la variante Release (vide = auto : Cargo).
    pub(crate) build_release: String,
    pub(crate) debug_modified: String,
    pub(crate) release_modified: String,
    /// Références (epoch secondes) de la dernière compilation détectée par
    /// variante (mtimes de target/debug et target/release). 0 = jamais.
    pub(crate) last_compile_debug_secs: u64,
    pub(crate) last_compile_release_secs: u64,
    /// Epoch secs du dernier mtime de cible `target/<profil>` pris en compte
    /// pour un projet sans sonde `-V` exploitable (référence anti-boucle).
    pub(crate) last_target_debug_secs: u64,
    pub(crate) last_target_release_secs: u64,
    /// Snapshots des métadonnées par variante, posés uniquement par un build
    /// validé (popup de détection, rattrapage, reconstruction documentée).
    /// La variante Release ne suit jamais le Debug : sans build release validé
    /// elle reste « 0.20.24 — Beta-Release », l'unique vérité du disque.
    pub(crate) debug_snapshot: Option<SnapshotCompile>,
    pub(crate) release_snapshot: Option<SnapshotCompile>,
    /// Marque si la version a été éditée manuellement dans le menu
    /// (pour éviter le bump AutoPatch au prochain rescan).
    pub(crate) version_manually_edited: bool,
    /// Hash SHA256 des sources (Cargo.toml + src/**/*.rs) pour détecter
    /// les modifications de code indépendamment du mtime target/debug.
    pub(crate) source_hash: String,
    /// Mode de valorisation : "web" (OIT/ILOSTAT) ou "manuel" (€/h saisi).
    pub(crate) tarif_mode: String,
    /// Tarif horaire manuel en €/h (mode "manuel").
    pub(crate) tarif_manual: f64,
    /// Série datée récupérée de la source web (année → €/h).
    pub(crate) tarif_web_series: Vec<TarifPoint>,
    /// Horodatage (ms) du dernier fetch web pour ce projet.
    pub(crate) tarif_web_fetched_ms: i64,
    /// true = série sectorielle ISIC J, false = repli national (tous secteurs).
    pub(crate) tarif_web_secteur: bool,
    /// Pays ISO3 auquel se rapporte la série du projet.
    pub(crate) tarif_web_pays: String,
}

impl ProjectEntry
  {
    /// Commande de sonde `-V` réglée pour une variante (`debug`/`release`).
    ///
    /// Renvoie `None` quand rien n'est personnalisé : la sonde auto
    /// (`target/<profil>/<binaire>`) reprend la main.
    pub(crate) fn sonde(&self, profil: &str) -> Option<&str>
      {
        let commande = if profil == "release" { &self.probe_release } else { &self.probe_debug };
        let t = commande.trim();
        (!t.is_empty()).then_some(t)
      }
  }

pub(crate) fn load_registry(path: &Path) -> Result<Vec<ProjectEntry>> {
    ensure_regular(path)?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_private(path, r#"{"projects":{}}"#)?;
    }
    let content = fs::read_to_string(path)?;
    let registry: Registry = serde_json::from_str(&content)?;

    let projects = registry
        .projects
        .into_iter()
        .map(|(id, p)| {
            let dir = PathBuf::from(shellexpand::tilde(&p.path).to_string());
            // Une seule traversée pour l'ensemble des horodatages affichés :
            // global, target/debug, target/release et sources.
            let t = project_times(&dir);
            // « Modifié » suit la cible construite quand elle existe (Debug
            // d'abord, Release ensuite), et l'ensemble du dossier sinon : la
            // date affichée reste cohérente avec la vérité du projet, sans être
            // noyée sous les fichiers de `.git` ou de `target/incremental`.
            let modified = t
                .debug
                .or(t.release)
                .or(t.overall)
                .map(format_mod_time)
                .unwrap_or_else(|| "—".to_string());
            let debug_modified = t
                .debug
                .map(format_mod_time)
                .unwrap_or_else(|| "jamais compilée".to_string());
            let release_modified = t
                .release
                .map(format_mod_time)
                .unwrap_or_else(|| "jamais compilée".to_string());
            // Migration source_hash : si vide, calculer le hash initial des sources
            let source_hash = if p.source_hash.is_empty() {
                compute_source_hash(&dir)
            } else {
                p.source_hash.clone()
            };
            // Migration des anciens champs uniques status/version : le champ
            // unique ne concerne que la variante élue (Debug). La Release ne
            // lui est JAMAIS empruntée : pour elle, « dernière compilation »
            // reste la vérité du disque, pas celle du Debug — sans build
            // release, elle affiche « — ».
            let status_debug =
                if p.status_debug.is_empty() { p.status.clone() } else { p.status_debug.clone() };
            let version_debug =
                if p.version_debug.is_empty() { p.version.clone() } else { p.version_debug.clone() };
            let status_release = p.status_release.clone();
            let version_release = p.version_release.clone();
            // Migration de la référence unique historique : si les deux
            // références par variante sont absentes (0), le registre étant
            // alors pré-dissociation, l'ancienne référence est attribuée aux
            // deux variantes (aucune n'est considérée « en retard » au départ).
            let (last_debug_secs, last_release_secs) =
                if p.last_compile_debug_secs == 0 && p.last_compile_release_secs == 0 {
                    (p.last_compile_secs, p.last_compile_secs)
                } else {
                    (p.last_compile_debug_secs, p.last_compile_release_secs)
                };
            // Migration des dates déclarées : fiche conservée telle quelle,
            // sinon mtime de build de la variante, sinon mtime du dossier,
            // sinon date du jour. La Release sans build hérite de la date
            // Debug (« jamais buildée » : la date de la source est la seule
            // vérité disponible).
            let date_debug = if !p.date_debug.is_empty() {
                p.date_debug.clone()
            } else {
                t.debug
                    .or(t.overall)
                    .map(date_jj_mm_yyyy)
                    .unwrap_or_else(date_du_jour)
            };
            let date_release = if !p.date_release.is_empty() {
                p.date_release.clone()
            } else if last_release_secs == 0 {
                date_debug.clone()
            } else {
                t.release
                    .or(t.overall)
                    .map(date_jj_mm_yyyy)
                    .unwrap_or_else(date_du_jour)
            };
            // Migration : une typologie absente (ancien registre) est
            // auto-détectée depuis le contenu du dossier.
            let typologie = if validate_typologie(&p.typologie) {
                p.typologie.clone()
            } else {
                typologie_auto(&dir).to_string()
            };
            ProjectEntry {
                id,
                name: p.name,
                path: dir,
                description: p.description,
                status: p.status,
                version: p.version,
                token: p.token,
                used_pct: p.used_pct,
                spent: p.spent,
                last_model: p.last_model,
                modified,
                total_tokens: p.total_tokens,
                total_cost: p.total_cost,
                session_count: p.session_count,
                last_session_tokens: p.last_session_tokens,
                last_session_cost: p.last_session_cost,
                last_session_cost_local: p.last_session_cost_local,
                last_agent: p.last_agent,
                last_time_created: p.last_time_created,
                last_session_context_pct: p.last_session_context_pct,
                last_duree: p.last_duree,
                total_duree: p.total_duree,
                last_valorisation: p.last_valorisation,
                total_valorisation: p.total_valorisation,
                tarif_resume: p.tarif_resume,
                status_debug,
                version_debug,
                status_release,
                version_release,
                date_debug,
                date_release,
                probe_debug: p.probe_debug,
                probe_release: p.probe_release,
                build_debug: p.build_debug,
                build_release: p.build_release,
                debug_modified,
                release_modified,
                last_compile_debug_secs: last_debug_secs,
                last_compile_release_secs: last_release_secs,
                last_target_debug_secs: p.last_target_debug_secs,
                last_target_release_secs: p.last_target_release_secs,
                debug_snapshot: p.debug_snapshot.clone(),
                release_snapshot: p.release_snapshot.clone(),
                version_manually_edited: p.version_manually_edited,
                source_hash,
                tarif_mode: p.tarif_mode,
                tarif_manual: p.tarif_manual,
                tarif_web_series: p.tarif_web_series,
                tarif_web_fetched_ms: p.tarif_web_fetched_ms,
                tarif_web_secteur: p.tarif_web_secteur,
                tarif_web_pays: p.tarif_web_pays.clone(),
                est_bibliotheque: p.est_bibliotheque,
                typologie,
            }
        })
        .collect::<Vec<ProjectEntry>>();

    Ok(projects)
}

/// Marque comme bibliothèques les entrées dont le chemin tombe sous la racine
/// des bibliothèques, sans jamais retirer la mention explicite.
///
/// La mention est cumulative : un projet déclaré bibliothèque par l'utilisateur
/// le reste même s'il est déplacé hors de la racine, et un projet déplacé sous
/// la racine devient bibliothèque dès le chargement.
pub(crate) fn deriver_bibliotheques(projects: &mut [ProjectEntry], lib_root: &Path)
  {
    for p in projects
      {
        if p.path.strip_prefix(lib_root).is_ok()
          {
            p.est_bibliotheque = true;
          }
      }
  }

/// Indique si toutes les colonnes demandées existent déjà dans `table`.
///
/// Vrai lecture via `PRAGMA table_info` : évite d'ouvrir une transaction
/// d'écriture pour rien. Une table absente ou illisible renvoie `false`, ce
/// redonne le comportement d'avant (ALTER TABLE ignorés s'ils échouent).
fn colonnes_presents(conn: &Connection, table: &str, colonnes: &[&str]) -> bool {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(1)) else {
        return false;
    };
    let existantes: Vec<String> = rows.filter_map(|r| r.ok()).collect();
    if existantes.is_empty() {
        return false;
    }
    colonnes.iter().all(|c| existantes.iter().any(|e| e == c))
}

// Nouvelle fonction pour charger les données d'usage (cumulées + dernière session)
pub(crate) fn load_usage(db_path: &Path) -> HashMap<PathBuf, ProjectUsage> {
    let mut usage = HashMap::new();
    if !db_path.exists() {
        return usage;
    }
    // Pas de SQLITE_OPEN_CREATE : ne jamais fabriquer une base vide à la place
    // de celle d'opencode. Repli en lecture seule si l'écriture est refusée.
    let Some(conn) = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .or_else(|_| {
        Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
    })
    .ok() else {
        return usage;
    };
    let _ = conn.busy_timeout(std::time::Duration::from_millis(2_000));
    // Migration : n'ajouter les colonnes que si elles manquent vraiment.
    // Sans cette garde, les quatre ALTER TABLE sont tentés à chaque démarrage
    // alors qu'ils échouent presque toujours (« duplicate column name ») ; sur
    // une base de plus de 1 Go, ces échecs prennent un verrou d'écriture et
    // peuvent propager le délai jusqu'aux lectures qui suivent.
    if !colonnes_presents(
        &conn,
        "session",
        &["context_pct", "currency", "exchange_rate", "cost_local"],
    ) {
        let _ = conn.execute(
            "ALTER TABLE session ADD COLUMN context_pct INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE session ADD COLUMN currency TEXT DEFAULT 'USD'",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE session ADD COLUMN exchange_rate REAL",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE session ADD COLUMN cost_local REAL",
            [],
        );
    }

    // D'abord, récupérer toutes les sessions pour détecter celles sans taux
    // session.id est du TEXTE (`ses_…`) : le lire en i64 renvoyait 0, la garde
    // `id > 0` était donc toujours fausse et tout ce bloc restait inerte.
    let colonnes_ok = colonnes_presents(
        &conn,
        "session",
        &["currency", "exchange_rate", "cost_local"],
    );
    let mut sessions_to_update: Vec<(String, String, f64)> = Vec::new();
    if colonnes_ok {
        let stmt = conn.prepare(
            "SELECT id, currency, cost FROM session WHERE directory IS NOT NULL AND (exchange_rate IS NULL OR exchange_rate = 0) AND cost > 0",
        );
        if let Ok(mut stmt) = stmt
            && let Ok(mut rows) = stmt.query([])
        {
            while let Ok(Some(row)) = rows.next()
                {
                    let id: String = row.get(0).unwrap_or_default();
                    let currency: String = row.get(1).unwrap_or_else(|_| "USD".to_string());
                    let cost: f64 = row.get(2).unwrap_or(0.0);
                    if !id.is_empty() && cost > 0.0 {
                        sessions_to_update.push((id, currency, cost));
                    }
                }
        }
    }

    // Fetch des taux manquants (batch par devise)
    if !sessions_to_update.is_empty() {
        let mut rates_by_currency: HashMap<String, f64> = HashMap::new();
        for (_, currency, _) in &sessions_to_update {
            if !rates_by_currency.contains_key(currency) {
                if let Some(rate) = fallback_exchange_rate(currency, LOCAL_CURRENCY) {
                    rates_by_currency.insert(currency.clone(), rate);
                } else if let Some(rate) = block_on_fetch_exchange_rate(currency, LOCAL_CURRENCY) {
                    rates_by_currency.insert(currency.clone(), rate);
                }
            }
        }

        // Mettre à jour les sessions avec les taux trouvés
        for (id, currency, cost) in sessions_to_update {
            if let Some(rate) = rates_by_currency.get(&currency) {
                let cost_local = cost * rate;
                let _ = conn.execute(
                    "UPDATE session SET exchange_rate = ?, cost_local = ? WHERE id = ?",
                    rusqlite::params![rate, cost_local, id],
                );
            }
        }
    }

    let durations = session_durations(&conn);

    // Colonnes ajoutées par migration : si l'ALTER a échoué (base verrouillée
    // par opencode), on lit quand même le reste au lieu de tout perdre.
    let expr = |col: &str, defaut: &str, vide: &str| -> String {
        if colonnes_presents(&conn, "session", &[col]) {
            defaut.to_string()
        } else {
            vide.to_string()
        }
    };
    let ctx_expr = expr("context_pct", "COALESCE(context_pct,0)", "0");
    let cur_expr = expr("currency", "COALESCE(currency, 'USD')", "'USD'");
    let rate_expr = expr("exchange_rate", "COALESCE(exchange_rate, 0)", "0");
    let loc_expr = expr("cost_local", "COALESCE(cost_local, 0)", "0");
    let requete = format!(
        "SELECT id, directory, COALESCE(cost,0), \
         COALESCE(tokens_input,0) + COALESCE(tokens_output,0) + COALESCE(tokens_reasoning,0), \
         model, agent, time_created, {ctx_expr}, {cur_expr}, {rate_expr}, {loc_expr} \
         FROM session WHERE directory IS NOT NULL ORDER BY time_created DESC"
    );
    let mut stmt = match conn.prepare(&requete) {
        Ok(s) => s,
        Err(_) => return usage,
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
            row.get::<_, i64>(3)? as u64,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?.clamp(0, 255) as u8,
            row.get::<_, String>(8)?,
            row.get::<_, f64>(9)?,
            row.get::<_, f64>(10)?,
        ))
    });
    if let Ok(iter) = rows {
        for r in iter.flatten() {
            let (id, dir, cost, tokens, model, agent, t, context_pct, currency, exchange_rate, cost_local) = r;
            let duree = durations.get(&id).copied().unwrap_or(0);
            let entry = usage
                .entry(PathBuf::from(dir))
                .or_insert_with(|| ProjectUsage {
                    cumulative: CumulativeUsage::default(),
                    last_session: None,
                    sessions: Vec::new(),
                });
            // Cumuler les totaux par devise
            if cost > 0.0 {
                *entry.cumulative.cost_by_currency.entry(currency.clone()).or_insert(0.0) += cost;
                // Même priorité que l'affichage de la dernière session :
                // montant converti stocké, puis taux de la session, puis
                // taux de repli.
                if let Some(local) = montant_local(
                    cost,
                    &currency,
                    Some(exchange_rate),
                    Some(cost_local),
                ) {
                    entry.cumulative.total_cost_local += local;
                }
            }
            entry.cumulative.total_tokens += tokens;
            entry.cumulative.session_count += 1;
            entry.cumulative.total_duration_ms += duree;
            entry.sessions.push(SessionDuree {
                time_created_ms: t,
                duration_ms: duree,
            });
            // Dernière session (la première rencontrée car ORDER BY time_created DESC)
            if entry.last_session.is_none() {
                let model_id = model.and_then(|m| {
                    serde_json::from_str::<serde_json::Value>(&m)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                });
                let agent_cap = agent.map(|a| capitalize(&a));
                entry.last_session = Some(LastSession {
                    tokens,
                    cost,
                    currency,
                    exchange_rate: if exchange_rate > 0.0 { Some(exchange_rate) } else { None },
                    cost_local: if cost_local > 0.0 { Some(cost_local) } else { None },
                    model: model_id.unwrap_or_default(),
                    agent: agent_cap.unwrap_or_default(),
                    time_created: t,
                    context_pct,
                    duration_ms: duree,
                });
            }
        }
    }
    usage
}

/// Agrège les sessions d'un projet : celles lancées à sa racine et celles
/// lancées depuis un sous-dossier (module, touche `m`).
pub(crate) fn usage_du_projet(
    usage: &HashMap<PathBuf, ProjectUsage>,
    projet: &Path,
) -> Option<ProjectUsage> {
    let mut agg: Option<ProjectUsage> = None;
    for (dir, u) in usage {
        if !dir.starts_with(projet) {
            continue;
        }
        if let Some(a) = agg.as_mut() {
            a.cumulative.total_tokens += u.cumulative.total_tokens;
            a.cumulative.total_cost_local += u.cumulative.total_cost_local;
            a.cumulative.session_count += u.cumulative.session_count;
            a.cumulative.total_duration_ms += u.cumulative.total_duration_ms;
            for (dev, m) in &u.cumulative.cost_by_currency {
                *a.cumulative.cost_by_currency.entry(dev.clone()).or_insert(0.0) += *m;
            }
            a.sessions.extend(u.sessions.iter().cloned());
            let plus_recente = match (&a.last_session, &u.last_session) {
                (Some(x), Some(y)) => y.time_created > x.time_created,
                (None, Some(_)) => true,
                _ => false,
            };
            if plus_recente {
                a.last_session = u.last_session.clone();
            }
        } else {
            agg = Some(u.clone());
        }
    }
    agg
}

/// Durée (ms) de chaque session, dérivée des messages de la table `message`
/// (MAX(time_updated) − MIN(time_created) par session). Les sessions sans
/// message (durée inconnue) sont absentes de la map.
fn session_durations(conn: &Connection) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT session_id, MIN(time_created), MAX(time_updated) \
         FROM message \
         WHERE time_updated IS NOT NULL AND time_created IS NOT NULL \
         GROUP BY session_id",
    ) else {
        return out;
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });
    if let Ok(iter) = rows {
        for r in iter.flatten() {
            let (sid, t0, t1) = r;
            let duree = (t1 - t0).max(0) as u64;
            out.insert(sid, duree);
        }
    }
    out
}

fn block_on_fetch_exchange_rate(from: &str, to: &str) -> Option<f64> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().ok()?;
    rt.block_on(fetch_exchange_rate(from, to))
}

// --- Récupération du tarif horaire web (OIT / ILOSTAT via SDMX) ---------
//
// Dataflow : EAR_EHRA_SEX_ECO_CUR_NB — « Average hourly earnings of
// employees by sex, economic activity and currency » (annuel, en devise
// locale CUR_TYPE_LCU). La clé porte la zone de référence (pays ISO3), la
// fréquence, la mesure, le sexe (total), l'activité économique et la devise.
// Le contenu est demandé en CSV (SDMX-CSV 1.0), sans authentification.

/// Base de l'API SDMX de l'OIT.
const ILO_SDMX_BASE: &str = "https://sdmx.ilo.org/rest/data/ILO,";

/// Dataflow sectoriel (par activité économique).
const ILO_DF_SECTEUR: &str = "DF_EAR_EHRA_SEX_ECO_CUR_NB";
/// Dataflow de repli (toutes activités économiques confondues).
const ILO_DF_NATIONAL: &str = "DF_EAR_EHRA_SEX_CUR_NB";

/// Activité économique : section J de la NACE / ISIC Rev.4
/// (Information et communication).
const ILO_ECO_ISIC4_J: &str = "ECO_ISIC4_J";

/// Libellé de la source web, affiché à l'utilisateur.
pub(crate) const TARIF_WEB_LIBELLE: &str = "OIT/ILOSTAT";

/// Durée maximale d'une requête réseau (ms).
const HTTP_TIMEOUT_MS: u64 = 20_000;

/// Nombre d'années d'historique demandées à la source.
const TARIF_ANNEE_MAX: i32 = 12;

/// Construit l'URL SDMX du dataflow sectoriel pour un pays donné.
fn ilostat_url_secteur(pays: &str, annee_deb: i32, annee_fin: i32) -> String {
    format!(
        "{ILO_SDMX_BASE}{ILO_DF_SECTEUR},1.0/{pays}.A.EAR_EHRA_NB.SEX_T.{ILO_ECO_ISIC4_J}.CUR_TYPE_LCU\
         ?startPeriod={annee_deb}&endPeriod={annee_fin}&format=csvfile"
    )
}

/// Construit l'URL SDMX du dataflow national (repli) pour un pays donné.
fn ilostat_url_national(pays: &str, annee_deb: i32, annee_fin: i32) -> String {
    format!(
        "{ILO_SDMX_BASE}{ILO_DF_NATIONAL},1.0/{pays}.A.EAR_EHRA_NB.SEX_T.CUR_TYPE_LCU\
         ?startPeriod={annee_deb}&endPeriod={annee_fin}&format=csvfile"
    )
}

/// Extrait le code devise ISO d'un champ NOTE_INDICATOR de l'OIT
/// (ex. « EUR: Euro » → "EUR" ; « Break in series: … | USD: United States
/// dollar » → "USD"). Les valeurs ISO 4217 sont des trigrammes en
/// majuscules, ce qui les distingue des libellés.
fn devise_depuis_note(note: &str) -> Option<String> {
    // On privilégie la dernière mention (les ruptures de série sont
    // préfixées, la devise est donc en fin de champ).
    for partie in note.split('|').rev() {
        let Some((code, _libelle)) = partie.split_once(':') else {
            continue;
        };
        let code = code.trim();
        if code.len() == 3 && code.chars().all(|c| c.is_ascii_uppercase()) {
            return Some(code.to_string());
        }
    }
    None
}

/// Parse une réponse CSV SDMX (SDMX-CSV 1.0) et en extrait les points
/// (année, valeur, devise). Les colonnes sont localisées par en-tête, la
/// position différant entre les dataflows (19 ou 20 colonnes). Les champs
/// texte libres (SOURCE, NOTE_SOURCE…) contiennent des virgules : seules les
/// colonnes numériques sont indexées de façon fiable, la devise étant lue
/// dans NOTE_INDICATOR via un balayage depuis la fin.
fn parse_sdmx_csv(csv: &str) -> Vec<TarifPointBrut> {
    let mut out = Vec::new();
    let mut colonnes: Option<(usize, usize, Option<usize>)> = None;
    for (num, ligne) in csv.lines().enumerate() {
        let ligne = ligne.trim_end_matches('\r');
        if ligne.trim().is_empty() {
            continue;
        }
        if num == 0 {
            let entetes: Vec<String> = parse_csv_line(ligne)
                .into_iter()
                .map(|s| s.trim().to_string())
                .collect();
            let pos = |nom: &str| entetes.iter().position(|h| *h == nom);
            colonnes = match (pos("TIME_PERIOD"), pos("OBS_VALUE")) {
                (Some(a), Some(v)) => Some((a, v, pos("NOTE_INDICATOR"))),
                _ => None,
            };
            continue;
        }
        let Some((ia, iv, i_note)) = colonnes else {
            continue;
        };
        let champs: Vec<String> = parse_csv_line(ligne);
        if champs.len() <= iv {
            continue;
        }
        let Ok(annee) = champs[ia].trim().parse::<i32>() else {
            continue;
        };
        let Ok(valeur) = champs[iv].trim().parse::<f64>() else {
            continue;
        };
        if valeur <= 0.0 {
            continue;
        }
        // Devise : champ NOTE_INDICATOR si la ligne est assez longue,
        // sinon balayage depuis la fin (champs texte libres à virgules).
        let devise = match i_note {
            Some(inote) if champs.len() > inote => {
                devise_depuis_note(&champs[inote]).unwrap_or_default()
            }
            _ => {
                let mut d = String::new();
                for champ in champs.iter().skip(iv + 1).rev() {
                    if let Some(code) = devise_depuis_note(champ) {
                        d = code;
                        break;
                    }
                }
                d
            }
        };
        out.push(TarifPointBrut {
            annee,
            valeur,
            devise,
        });
    }
    out
}

/// Parse une ligne CSV simple en gérant les guillemets et virgules embarquées.
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                if in_quotes && chars.peek() == Some(&'"') {
                    // Double guillemet = guillemet échappé
                    chars.next();
                    current.push('"');
                } else {
                    in_quotes = !in_quotes;
                }
            }
            ',' if !in_quotes => {
                fields.push(current);
                current = String::new();
            }
            _ => current.push(c),
        }
    }
    fields.push(current);
    fields
}

/// Point brut (avant conversion en EUR).
struct TarifPointBrut {
    annee: i32,
    valeur: f64,
    devise: String,
}

/// Taux de conversion vers l'euro d'une devise source. Tente d'abord le
/// taux courant du jour, puis bascule sur le taux de repli local.
async fn taux_vers_eur(devise: &str) -> Option<f64> {
    if devise.is_empty() {
        return None;
    }
    // Cette fonction est `async` et non `sync` : elle est toujours appelée
    // depuis `fetch_web_rates`, donc déjà dans le runtime tokio. Un `block_on`
    // y paniquerait (« cannot start a runtime from within a runtime »).
    fetch_exchange_rate(devise, LOCAL_CURRENCY)
        .await
        .or_else(|| fallback_exchange_rate(devise, LOCAL_CURRENCY))
        .filter(|t| *t > 0.0)
}

/// Convertit des points bruts en points valorisés en EUR/heure. Les points
/// dont la devise ne peut être convertie sont écartés (la valeur restant
/// dans la devise d'origine n'aurait pas de sens en euros).
async fn convertir_en_eur(bruts: &[TarifPointBrut], maintenant_ms: i64) -> Vec<TarifPoint> {
    let mut out: Vec<TarifPoint> = Vec::new();
    for b in bruts {
        let Some(tx) = taux_vers_eur(&b.devise).await else {
            continue;
        };
        let point = TarifPoint {
            annee: b.annee,
            valeur_eur_h: b.valeur * tx,
            devise_src: b.devise.clone(),
            tx_eur: tx,
            fetched_ms: maintenant_ms,
        };
        // Un même code pays peut produire deux lignes pour la même année
        // (rupture) : on conserve la dernière valeur observée.
        match out.iter_mut().find(|p| p.annee == b.annee) {
            Some(existant) => *existant = point,
            None => out.push(point),
        }
    }
    out.sort_by_key(|p| p.annee);
    out
}

/// Récupère une série CSV SDMX. Une absence de données (HTTP 404 ou corps
/// « No data is found ») n'est pas une erreur réseau : elle renvoie Ok(None)
/// pour permettre l'essai du dataflow de repli.
/// En cas d'erreur réseau (timeout, connexion), un retry unique est tenté.
async fn fetch_sdmx_csv(url: &str) -> Result<Option<String>> {
    const MAX_RETRIES: u32 = 1;
    let mut last_err = None;
    for attempt in 0..=MAX_RETRIES {
        let reponse = HTTP_CLIENT
            .get(url)
            .header("Accept", "text/csv")
            .timeout(std::time::Duration::from_millis(HTTP_TIMEOUT_MS))
            .send()
            .await;
        match reponse {
            Ok(resp) => {
                let statut = resp.status();
                let corps = resp.text().await?;
                if !statut.is_success() {
                    return Ok(None);
                }
                if corps.trim().is_empty() || corps.contains("No data is found") {
                    return Ok(None);
                }
                return Ok(Some(corps));
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < MAX_RETRIES {
                    // Petite pause avant retry
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
            }
        }
    }
    Err(anyhow::Error::from(last_err.unwrap()))
}

/// Récupère la série de tarifs horaires d'un pays (ISO3) auprès de l'OIT.
///
/// Stratégie : on tente d'abord le dataflow sectoriel (activité J —
/// information et communication, celle qui correspond le mieux au
/// développement logiciel). Si le pays ne publie pas cette granularité,
/// on bascule sur le dataflow national (toutes activités), et on l'indique
/// à l'utilisateur via `secteur: false`.
///
/// Le booléen bool `secteur` retourné indique si la série obtenue est
/// sectorielle (true) ou nationale (false).
pub(crate) async fn fetch_web_rates(
    pays: &str,
) -> Result<(Vec<TarifPoint>, bool, Option<String>)> {
    let pays = pays.trim().to_uppercase();
    if pays.len() != 3 || !pays.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(anyhow!("code pays invalide : {pays}"));
    }
    let maintenant_ms = chrono::Utc::now().timestamp_millis();
    let annee_courante = chrono::Utc::now().year();
    let annee_deb = annee_courante - TARIF_ANNEE_MAX;
    let annee_fin = annee_courante + 1;

    // 1) Tentative sectorielle (activité J).
    let mut erreur = None;
    let brut = match fetch_sdmx_csv(&ilostat_url_secteur(&pays, annee_deb, annee_fin)).await {
        Ok(csv) => csv,
        Err(e) => {
            erreur = Some(e);
            None
        }
    };

    let points = if let Some(csv) = brut {
        let bruts = parse_sdmx_csv(&csv);
        convertir_en_eur(&bruts, maintenant_ms).await
    } else {
        Vec::new()
    };

    if !points.is_empty() {
        return Ok((points, true, None));
    }

    // 2) Repli sur le dataflow national si le secteur n'est pas publié
    //    (ou si la tentative sectorielle a échoué).
    let national = fetch_sdmx_csv(&ilostat_url_national(&pays, annee_deb, annee_fin)).await;
    match national {
        Ok(Some(csv)) => {
            let bruts = parse_sdmx_csv(&csv);
            let points = convertir_en_eur(&bruts, maintenant_ms).await;
            if points.is_empty() {
                // Données inexploitables (devise inconnue, valeurs nulles).
                return Err(anyhow!(
                    "série OIT illisible ou devise inconnue pour {pays}"
                ));
            }
            Ok((points, false, None))
        }
        Ok(None) => match erreur {
            Some(e) => Err(anyhow!("OIT indisponible : {e}")),
            None => Err(anyhow!("aucune donnée OIT/ILOSTAT pour {pays}")),
        },
        Err(e) => Err(anyhow!("OIT indisponible : {e}")),
    }
}

fn block_on_fetch_web_rates(pays: &str) -> Result<(Vec<TarifPoint>, bool, Option<String>)> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(fetch_web_rates(pays))
}

/// Formatage d'une durée (ms) en notation compacte lisible : « 1 h 23 min »,
/// « 12 min », « 45 s ». Les durées nulles ou inconnues renvoient « — ».
pub(crate) fn format_duree(duree_ms: u64) -> String {
    if duree_ms == 0 {
        return "—".to_string();
    }
    let total_sec = duree_ms / 1000;
    let heures = total_sec / 3600;
    let minutes = (total_sec % 3600) / 60;
    let secondes = total_sec % 60;
    if heures > 0 {
        if minutes > 0 {
            format!("{heures} h {minutes} min")
        } else {
            format!("{heures} h")
        }
    } else if minutes > 0 {
        format!("{minutes} min")
    } else {
        format!("{secondes} s")
    }
}

/// Formatage d'un horodatage (ms) en « mm/aaaa » localisé (ex. « 09/2026 »),
/// utilisé pour dater la source du tarif.
pub(crate) fn format_mois_annee(ts_ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(ts_ms) {
        Some(dt) => dt.format("%m/%Y").to_string(),
        None => "—".to_string(),
    }
}

/// Millisecondes écoulées depuis l'époque Unix, ou 0 si indisponible.
pub(crate) fn maintenant_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Pays par défaut de la source web, déduit de la locale système.
pub(crate) fn default_web_country() -> String {
    let locale = sys_locale::get_locale().unwrap_or_default();
    let prefix = locale
        .split(['_', '-', '.'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    match prefix.as_str() {
        "fr" => "FRA",
        "de" => "DEU",
        "en" => "USA",
        "gb" | "uk" => "GBR",
        "es" => "ESP",
        "it" => "ITA",
        "nl" => "NLD",
        "pt" => "PRT",
        "pl" => "POL",
        "sv" => "SWE",
        "be" => "BEL",
        "at" => "AUT",
        "ch" => "CHE",
        "ca" => "CAN",
        "jp" => "JPN",
        "in" => "IND",
        "br" => "BRA",
        _ => "FRA",
    }
    .to_string()
}

/// Charge le cache web global depuis le registre (sans échec : un registre
/// illisible ou ancien donne simplement un cache vide avec le pays par défaut).
pub(crate) fn load_web_cache(path: &Path) -> WebCache {
    let mut cache = WebCache {
        country: default_web_country(),
        ..Default::default()
    };
    let Ok(contenu) = fs::read_to_string(path) else {
        return cache;
    };
    let Ok(registry) = serde_json::from_str::<Registry>(&contenu) else {
        return cache;
    };
    if !registry.web_country.trim().is_empty() {
        cache.country = registry.web_country.trim().to_uppercase();
    }
    cache.rates = registry.web_rates_cache;
    cache.last_fetch_ms = registry.web_last_fetch_ms;
    cache.secteur = registry.web_secteur;
    cache.last_failure_ms = registry.web_last_failure_ms;
    if cache.secteur.is_empty() {
        // Registre antérieur à la mémorisation de la granularité : la série
        // sectorielle (J) est celle demandée par défaut.
        cache.secteur = cache.rates.keys().map(|k| (k.clone(), true)).collect();
    }
    cache
}

// --- Valorisation du temps passé -----------------------------------------

/// Résultat d'une valorisation, avec de quoi expliquer le calcul à
/// l'utilisateur (origine du tarif, année, mode).
#[derive(Debug, Clone, Default)]
pub(crate) struct Valorisation {
    /// Montant en euros.
    pub euros: f64,
    /// Tarif horaire appliqué (€/h).
    pub tarif_h: f64,
    /// Année du tarif retenu (mode web).
    pub annee: i32,
    /// Devise d'origine de la source (mode web).
    pub devise_src: String,
    /// Vrai si le tarif provient de la source web.
    pub web: bool,
    /// Vrai si la série est sectorielle (J) et non le repli national.
    pub secteur: bool,
    /// Horodatage (ms) de récupération du tarif web.
    pub fetched_ms: i64,
}

impl Valorisation {
    /// Étiquette compacte affichée dans le tableau : « OIT·J·2024 » ou
    /// « OIT·2024 », « manuel ». La devise d'origine est rappelée lorsqu'elle
    /// diffère de l'euro, afin que l'utilisateur sache qu'une conversion a
    /// eu lieu, et la date d'actualisation de la source est jointe pour que
    /// l'utilisateur sache à quelle date le montant a été calculé.
    pub fn etiquette(&self) -> String {
        if self.web {
            let suffixe = if self.devise_src.is_empty()
                || self.devise_src.eq_ignore_ascii_case(LOCAL_CURRENCY)
            {
                String::new()
            } else {
                format!("·{}", self.devise_src)
            };
            let base = if self.secteur {
                format!("OIT·J·{}{suffixe}", self.annee)
            } else {
                format!("OIT·{}{suffixe}", self.annee)
            };
            if self.fetched_ms > 0 {
                let maj = format_mois_annee(self.fetched_ms);
                if maj != "—" {
                    return format!("{base}·maj {maj}");
                }
            }
            base
        } else {
            "manuel".to_string()
        }
    }
}

/// Choisit la série de tarifs à appliquer pour un projet.
///
/// Privilégie la série la plus fraîche entre la copie locale du projet et le
/// cache global. Si le pays diffère, le cache fait foi (changement de pays).
/// Pour un registre antérieur à `tarif_web_pays`, la copie locale est vide
/// et le cache du pays courant est utilisé.
pub(crate) fn serie_du_projet(
    p: &ProjectEntry,
    cache: &WebCache,
) -> (Vec<TarifPoint>, bool, i64) {
    let pays = &cache.country;
    let cache_serie = cache.rates.get(pays);
    let cache_fetched = cache_serie
        .and_then(|s| s.last())
        .map(|p| p.fetched_ms)
        .unwrap_or(0);
    let cache_secteur = cache.secteur.get(pays).copied().unwrap_or(true);

    // Copie locale valide pour ce pays ?
    let copie_valide = !p.tarif_web_series.is_empty()
        && p.tarif_web_pays.eq_ignore_ascii_case(pays);

    if copie_valide {
        let local_fetched = p.tarif_web_fetched_ms;
        // Préférer la version la plus fraîche : le cache n'est retenu que s'il
        // existe, condition qu'un enchaînement `let` exprime sans retester le
        // même `Option`.
        if let Some(serie) = cache_serie
            && cache_fetched > local_fetched
        {
            return (serie.clone(), cache_secteur, cache_fetched);
        }
        return (
            p.tarif_web_series.clone(),
            p.tarif_web_secteur,
            local_fetched,
        );
    }

    // Pas de copie locale valide → utiliser le cache s'il existe.
    if let Some(serie) = cache_serie
      && !serie.is_empty()
      {
        return (serie.clone(), cache_secteur, cache_fetched);
    }
    (Vec::new(), false, 0)
}

/// Valorise une durée unique (la dernière session) avec le tarif en vigueur
/// à la date de la session. Retourne `None` si aucun tarif n'est applicable.
pub(crate) fn valoriser_session(
    duree_ms: u64,
    date_ms: i64,
    serie: &[TarifPoint],
    secteur: bool,
    fetched_ms: i64,
    mode: &str,
    tarif_manual: f64,
) -> Option<Valorisation> {
    if duree_ms == 0 {
        return None;
    }
    let heures = duree_ms as f64 / 3_600_000.0;
    if mode == "manuel" {
        if tarif_manual <= 0.0 {
            return None;
        }
        return Some(Valorisation {
            euros: heures * tarif_manual,
            tarif_h: tarif_manual,
            web: false,
            ..Default::default()
        });
    }
    let point = tarif_point_for_ms(serie, date_ms)?;
    if point.valeur_eur_h <= 0.0 {
        return None;
    }
    Some(Valorisation {
        euros: heures * point.valeur_eur_h,
        tarif_h: point.valeur_eur_h,
        annee: point.annee,
        devise_src: point.devise_src.clone(),
        web: true,
        secteur,
        fetched_ms: if fetched_ms > 0 { fetched_ms } else { point.fetched_ms },
    })
}

/// Valorise le temps cumulé : chaque session est valorisée au tarif en
/// vigueur à sa propre date (carry-forward), puis la somme est retournée.
/// `cout_moyen` est la moyenne pondérée des tarifs appliqués, affichée
/// comme repère à l'utilisateur.
pub(crate) fn valoriser_cumul(
    sessions: &[SessionDuree],
    serie: &[TarifPoint],
    secteur: bool,
    fetched_ms: i64,
    mode: &str,
    tarif_manual: f64,
) -> Option<Valorisation> {
    let mut total_heures = 0.0;
    let mut total_euros = 0.0;
    let mut total_tailles = 0.0;
    let mut annee_plus_recente = 0;
    let mut date_plus_recente = 0i64;
    let mut derniere_valeur = 0.0;
    let mut devise_dernier = String::new();
    for s in sessions {
        if s.duration_ms == 0 {
            continue;
        }
        let v = valoriser_session(
            s.duration_ms,
            s.time_created_ms,
            serie,
            secteur,
            fetched_ms,
            mode,
            tarif_manual,
        )?;
        total_heures += s.duration_ms as f64 / 3_600_000.0;
        total_euros += v.euros;
        total_tailles += v.tarif_h * s.duration_ms as f64;
        if s.time_created_ms > date_plus_recente {
            date_plus_recente = s.time_created_ms;
            annee_plus_recente = v.annee;
            derniere_valeur = v.tarif_h;
            devise_dernier = v.devise_src.clone();
        }
    }
    if total_heures <= 0.0 {
        return None;
    }
    let tarif_moyen = total_tailles / (total_heures * 3_600_000.0);
    let web = mode != "manuel";
    Some(Valorisation {
        euros: total_euros,
        // En mode web, l'étiquette porte l'année la plus récente utilisée.
        tarif_h: if web && tarif_moyen > 0.0 { tarif_moyen } else { derniere_valeur },
        annee: if web { annee_plus_recente } else { 0 },
        devise_src: devise_dernier,
        web,
        secteur,
        fetched_ms,
    })
}

// --- Cache web et valorisation ------------------------------------------

/// Cache global des séries récupérées depuis la source web, partagé par
/// tous les projets (évite un téléchargement par projet).
#[derive(Debug, Clone, Default)]
pub(crate) struct WebCache {
    /// Pays ISO3 → série datée.
    pub rates: HashMap<String, Vec<TarifPoint>>,
    /// Horodatage (ms) de la dernière récupération.
    pub last_fetch_ms: i64,
    /// Pays utilisé par défaut (ISO3).
    pub country: String,
    /// Dernière erreur rencontrée lors d'un fetch (affichée à l'utilisateur).
    pub last_error: String,
    /// Pays ISO3 → granularité obtenue (true : secteur J, false : national).
    pub secteur: HashMap<String, bool>,
    /// Dernier échec de récupération (ms) : évite de retenter à chaque rescan.
    pub last_failure_ms: i64,
}

/// Durée de fraîcheur du cache web avant re-téléchargement (jours).
pub(crate) const WEB_FETCH_JOURS: i64 = 30;

/// Pause (ms) après un échec avant de retenter automatiquement la source.
const ECHEC_PAUSE_MS: i64 = 3_600_000;

impl WebCache {
    /// Le cache du pays courant est-il absent ou périmé ?
    pub fn a_fetcher(&self, maintenant_ms: i64) -> bool {
        if self.last_failure_ms > 0 && maintenant_ms - self.last_failure_ms < ECHEC_PAUSE_MS {
            return false;
        }
        if let Some(serie) = self.rates.get(&self.country)
            && let Some(p) = serie.last()
        {
            let age_jours = (maintenant_ms - p.fetched_ms) / 86_400_000;
            return age_jours >= WEB_FETCH_JOURS;
        }
        true // série absente → il faut fetcher
    }

    /// Met à jour le cache pour un pays donné. Ne télécharge que si la
    /// série est absente ou périmée, sauf si `force` est vrai.
    /// Retourne `true` si le cache a changé.
    pub fn rafraichir(
        &mut self,
        pays: &str,
        maintenant_ms: i64,
        force: bool,
    ) -> bool {
        let pays = pays.trim().to_uppercase();
        if pays.is_empty() {
            return false;
        }
        let a_jour = !force
            && self
                .rates
                .get(&pays)
                .and_then(|serie| serie.last())
                .map(|p| {
                    (maintenant_ms - p.fetched_ms) / 86_400_000 < WEB_FETCH_JOURS
                })
                .unwrap_or(false);
        if a_jour {
            return false;
        }
        match block_on_fetch_web_rates(&pays) {
            Ok((points, secteur, _)) if !points.is_empty() => {
                self.rates.insert(pays.clone(), points);
                self.last_fetch_ms = maintenant_ms;
                self.last_error.clear();
                self.last_failure_ms = 0;
                // Mémorise la granularité obtenue (secteur J ou national).
                self.secteur.insert(pays, secteur);
                true
            }
            Ok(_) => {
                self.last_error = format!("aucune donnée {TARIF_WEB_LIBELLE} pour {pays}");
                self.last_failure_ms = maintenant_ms;
                false
            }
            Err(e) => {
                self.last_error = e.to_string();
                self.last_failure_ms = maintenant_ms;
                false
            }
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Charge les préférences, ou les valeurs par défaut si le registre est absent,
/// illisible ou sans bloc `preferences`.
///
/// Un registre illisible ne doit pas faire échouer le chargement des
/// préférences : le menu doit démarrer et laisser l'utilisateur réparer, pas
/// refuser de s'ouvrir.
pub(crate) fn load_preferences(path: &Path) -> Preferences {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Registry>(&s).ok())
        .map(|r| r.preferences)
        .unwrap_or_default()
}

/// Écrit le seul bloc `preferences`, en préservant tout le reste du registre.
///
/// Passe par un registre complet lu puis réécrit plutôt que par une
/// réécriture depuis `ProjectEntry` : ce dernier est une vue d'usage, et
 /// repasser par lui perdrait les champs que le registre seul connaît.
pub(crate) fn save_preferences(path: &Path, prefs: &Preferences) -> Result<()> {
    ensure_regular(path)?;
    let existant = fs::read_to_string(path).unwrap_or_default();
    let mut registry: Registry = serde_json::from_str(&existant).unwrap_or_default();
    if registry.preferences == *prefs {
        return Ok(());
    }
    registry.preferences = prefs.clone();
    ecrire_registry_atomique(path, &registry)
}

/// Charge les refus de cohérence de tous les projets (id → refus).
///
/// Un registre illisible ne fait pas échouer le chargement : le menu démarre
/// avec des refus vides, perdant tout au plus une re-notification.
pub(crate) fn load_refus(path: &Path) -> HashMap<String, Vec<RefusCoherence>>
  {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Registry>(&s).ok())
        .map(|r| {
            r.projects
              .into_iter()
              .filter(|(_, p)| !p.refus_coherence.is_empty())
              .map(|(id, p)| (id, p.refus_coherence))
              .collect()
        })
        .unwrap_or_default()
  }

/// Écrit les refus de cohérence par projet, en préservant tout le reste.
///
/// Comme [`save_preferences`], passe par un registre complet lu puis réécrit :
/// `ProjectEntry` est une vue d'usage et n'a pas à porter les refus.
pub(crate) fn save_refus(
    path: &Path,
    refus: &HashMap<String, Vec<RefusCoherence>>,
) -> Result<()>
  {
    ensure_regular(path)?;
    let existant = fs::read_to_string(path).unwrap_or_default();
    let mut registry: Registry = serde_json::from_str(&existant).unwrap_or_default();
    for (id, liste) in refus
      {
        if let Some(p) = registry.projects.get_mut(id)
          {
            p.refus_coherence = liste.clone();
          }
      }
    // Un projet sans refus doit voir les siens effacés, sinon un refus levé
    // resterait écrit.
    let connus: Vec<String> = registry.projects.keys().cloned().collect();
    for id in connus
      {
        if !refus.contains_key(&id)
            && let Some(p) = registry.projects.get_mut(&id)
            && !p.refus_coherence.is_empty()
          {
            p.refus_coherence.clear();
          }
      }
    ecrire_registry_atomique(path, &registry)
}

/// Écrit le registre par fichier temporaire puis renommage.
///
/// Le nom du temporaire porte l'identifiant du processus : deux instances du
/// menu ne se marchent pas dessus, contrairement à un `tmp` fixe.
fn ecrire_registry_atomique(path: &Path, registry: &Registry) -> Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    write_private(&tmp, &serde_json::to_string_pretty(registry)?)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

// --- Garde « vérité du disque » ------------------------------------------
//
// Le registre est un fichier partagé entre versions, et rien ne l'empêche
// d'être édité à la main. La garde protège les métadonnées (statuts, versions,
// références de compilation, snapshots) : toute modification non déclarée par
// [`accepter_modification`] est restaurée à son ancienne valeur à l'écriture,
// et l'alerte est renvoyée par [`save_registry_full`].
//
// Une mutation est légitime quand elle est portée par un chemin documenté du
// menu (bump AutoPatch, rattrapage de build, édition utilisateur, synchroni-
// sation du projet menu, restauration de cohérence au chargement). Toute autre
// mutation — bug ou édition manuelle du fichier — dérive des métadonnées et
// doit être neutralisée.
static METADONNEES_ACCEPTEES: LazyLock<std::sync::Mutex<HashSet<String>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// Déclare une modification de métadonnées comme intentionnelle.
///
/// La clé suit le format `"<id du projet>:<champ>"` (ex. `"menu:version_debug"`).
/// Elle est consommée par la prochaine écriture de [`save_registry_full`].
pub(crate) fn accepter_modification(clef: String) {
    if let Ok(mut acceptees) = METADONNEES_ACCEPTEES.lock() {
        acceptees.insert(clef);
    }
}

/// Application de la garde à un projet à écrire : compare chaque champ protégé
/// (statuts, versions, références de compilation, snapshots) à sa valeur
/// persistée et restaure toute modification non déclarée.
fn garder_metadonnees(
    id: &str,
    ancien: Option<&Project>,
    nouveau: &mut Project,
    acceptees: &HashSet<String>,
    alertes: &mut Vec<String>,
  )
  {
    let Some(ancien) = ancien
      else
      {
        // Projet nouveau (création, résurrection) : la fiche est générée par
        // le menu, aucune comparaison à une quelconque valeur antérieure.
        return;
      };
    macro_rules! aligner {
      ($champ:ident) => {
        if ancien.$champ != nouveau.$champ
          {
            let clef = format!("{id}:{}", stringify!($champ));
            if !acceptees.contains(&clef)
              {
                nouveau.$champ = ancien.$champ.clone();
                alertes.push(format!(
                  "Métadonnée « {clef} » restaurée (modification non déclarée)"
                ));
              }
          }
      };
    }
    // Variante du garde pour les dates déclarées : une date absente du
    // registre persisté (ancienne version, projet neuf) ne protège rien —
    // la valeur migrée au chargement (mtime, date du jour) peut se persister
    // silencieusement. Seule une date déjà posée fait autorité.
    macro_rules! aligner_date {
      ($champ:ident) => {
        if ancien.$champ != nouveau.$champ && !ancien.$champ.is_empty()
          {
            let clef = format!("{id}:{}", stringify!($champ));
            if !acceptees.contains(&clef)
              {
                nouveau.$champ = ancien.$champ.clone();
                alertes.push(format!(
                  "Métadonnée « {clef} » restaurée (modification non déclarée)"
                ));
              }
          }
      };
    }
    aligner!(status);
    aligner!(version);
    aligner!(status_debug);
    aligner!(version_debug);
    aligner!(status_release);
    aligner!(version_release);
    // Les dates déclarées sont des métadonnées de vérité : elles ne peuvent
    // changer que via une déclaration explicite (édition source, rattrapage).
    aligner_date!(date_debug);
    aligner_date!(date_release);
    aligner!(typologie);
    aligner!(last_compile_debug_secs);
    aligner!(last_compile_release_secs);
    aligner!(last_target_debug_secs);
    aligner!(last_target_release_secs);
    aligner!(debug_snapshot);
    aligner!(release_snapshot);
  }

/// Variante conservant le cache web global et les préférences lors de
/// l'écriture du registre.
///
/// Retourne la liste des alertes de la garde : toute métadonnée non déclarée
/// via [`accepter_modification`] a été restaurée à sa valeur persistée et
/// mérite d'être signalée à l'utilisateur.
pub(crate) fn save_registry_full(
    path: &Path,
    projects: &[ProjectEntry],
    web: &WebCache,
    prefs: &Preferences,
) -> Result<Vec<String>> {
    ensure_regular(path)?;
    // Un registre illisible est mis de côté avant d'être remplacé.
    let existant_lisible = match fs::read_to_string(path)
      {
        Ok(existant) =>
          {
            match serde_json::from_str::<Registry>(&existant)
              {
                Ok(r) => Some(r),
                Err(_) =>
                  {
                    let _ = fs::copy(path, path.with_extension("json.corrompu"));
                    None
                  }
              }
          }
        Err(_) => None,
      };
    let inconnu = existant_lisible
        .as_ref()
        .map(|r| r.champs_inconnus.clone())
        .unwrap_or_default();
    // « Copie avant » de la garde : l'état persistant de chaque projet sert de
    // référence, seule une modification déclarée peut s'en écarter.
    let existants = existant_lisible
        .map(|r| r.projects)
        .unwrap_or_default();
    // Les déclarations sont consommées par cette écriture — mais seulement
    // celles portant sur un projet réellement écrit ici : une écriture d'un
    // autre registre (autre test, autre instance) ne doit pas voler une
    // déclaration d'un projet qui ne lui correspond pas.
    let ids: HashSet<&str> = projects.iter().map(|p| p.id.as_str()).collect();
    let mut restantes: HashSet<String> = HashSet::new();
    let mut acceptees: HashSet<String> = HashSet::new();
    {
      let mut garde = METADONNEES_ACCEPTEES
          .lock()
          .unwrap_or_else(|e| e.into_inner());
      for clef in garde.drain() {
        let id = clef.split(':').next().unwrap_or("");
        if ids.contains(id) { acceptees.insert(clef); } else { restantes.insert(clef); }
      }
      *garde = restantes;
    }
    let mut alertes: Vec<String> = Vec::new();
    let mut map = HashMap::new();
    for p in projects {
        let mut projet = Project {
            name: p.name.clone(),
            path: p.path.to_string_lossy().to_string(),
            description: p.description.clone(),
            status: p.status.clone(),
            version: p.version.clone(),
            token: p.token.clone(),
            used_pct: p.used_pct.clone(),
            spent: p.spent.clone(),
            last_model: p.last_model.clone(),
            total_tokens: p.total_tokens.clone(),
            total_cost: p.total_cost.clone(),
            session_count: p.session_count.clone(),
            last_session_tokens: p.last_session_tokens.clone(),
            last_session_cost: p.last_session_cost.clone(),
            last_session_cost_local: p.last_session_cost_local.clone(),
            last_agent: p.last_agent.clone(),
            last_time_created: p.last_time_created.clone(),
            status_debug: p.status_debug.clone(),
            version_debug: p.version_debug.clone(),
            status_release: p.status_release.clone(),
            version_release: p.version_release.clone(),
            date_debug: p.date_debug.clone(),
            date_release: p.date_release.clone(),
            probe_debug: p.probe_debug.clone(),
            probe_release: p.probe_release.clone(),
            build_debug: p.build_debug.clone(),
            build_release: p.build_release.clone(),
            last_compile_debug_secs: p.last_compile_debug_secs,
            last_compile_release_secs: p.last_compile_release_secs,
            last_target_debug_secs: p.last_target_debug_secs,
            last_target_release_secs: p.last_target_release_secs,
            debug_snapshot: p.debug_snapshot.clone(),
            release_snapshot: p.release_snapshot.clone(),
            // Ancien champ : remis à zéro après migration (géré par default).
            last_compile_secs: 0,
            version_manually_edited: p.version_manually_edited,
            source_hash: p.source_hash.clone(),
            active: true,
            tarif_mode: p.tarif_mode.clone(),
            // Dernier rempart contre la corruption du registre : une valeur
            // non finie y serait sérialisée en `null` et rendrait le fichier
            // illisible au rechargement, sans le moindre message d'erreur.
            tarif_manual: if tarif_valide(p.tarif_manual) {
                p.tarif_manual
            } else {
                0.0
            },
            tarif_web_series: p.tarif_web_series.clone(),
            tarif_web_fetched_ms: p.tarif_web_fetched_ms,
            tarif_web_secteur: p.tarif_web_secteur,
            tarif_web_pays: p.tarif_web_pays.clone(),
            last_session_context_pct: p.last_session_context_pct.clone(),
            last_duree: p.last_duree.clone(),
            total_duree: p.total_duree.clone(),
            last_valorisation: p.last_valorisation.clone(),
            total_valorisation: p.total_valorisation.clone(),
            tarif_resume: p.tarif_resume.clone(),
            est_bibliotheque: p.est_bibliotheque,
            typologie: p.typologie.clone(),
            // Les refus de cohérence sont gérés par `save_refus`, jamais par la
            // vue d'usage `ProjectEntry` : on préserve la valeur persistée.
            refus_coherence: existants
                .get(&p.id)
                .map(|o| o.refus_coherence.clone())
                .unwrap_or_default(),
        };
        garder_metadonnees(&p.id, existants.get(&p.id), &mut projet, &acceptees, &mut alertes);
        map.insert(p.id.clone(), projet);
    }
    let registry = Registry {
        projects: map,
        preferences: prefs.clone(),
        web_rates_cache: web.rates.clone(),
        web_last_fetch_ms: web.last_fetch_ms,
        web_country: web.country.clone(),
        web_secteur: web.secteur.clone(),
        web_last_failure_ms: web.last_failure_ms,
        // Conservés : `save_registry_full` reconstruit le registre depuis une
        // vue `ProjectEntry`, cette reconstruction ne doit pas valoir effacement.
        champs_inconnus: inconnu,
    };
    ecrire_registry_atomique(path, &registry)?;
    Ok(alertes)
}

pub(crate) fn is_excluded(path: &Path) -> bool {
    path.file_name()
        .map(|n| {
            let n = n.to_string_lossy();
            contains_casefold(&n, EXCLUDED_DIRS)
        })
        .unwrap_or(false)
}

fn contains_casefold(name: &str, list: &[&str]) -> bool {
    let lower = name.to_lowercase();
    list.iter().any(|e| e.to_lowercase() == lower)
}

const EXCLUDED_DIRS: &[&str] = &["jdk-21"];

pub(crate) fn reindex_from_disk(
    root: &Path,
    lib_root: &Path,
    projects: &mut Vec<ProjectEntry>,
) -> Result<usize> {
    if !root.is_dir() {
        return Ok(0);
    }
    let mut added = 0usize;
    let known: HashSet<PathBuf> = projects.iter().map(|p| p.path.clone()).collect();
    let mut ajouter_entree = |dir: &Path, est_lib: bool| {
        if known.contains(dir) || is_excluded(dir) {
            return;
        }
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            return;
        }
        let debug_time = variant_modified_time(dir, "debug");
        let release_time = variant_modified_time(dir, "release");
        // « Modifié » suit la cible construite quand elle existe, l'ensemble du
        // dossier sinon (voir `load_registry`).
        let modified = debug_time
            .or(release_time)
            .or_else(|| last_modified_time(dir))
            .map(format_mod_time)
            .unwrap_or_else(|| "—".to_string());
        let debug_modified =
            debug_time.map(format_mod_time).unwrap_or_else(|| "jamais compilée".to_string());
        let release_modified =
            release_time.map(format_mod_time).unwrap_or_else(|| "jamais compilée".to_string());
        let date_debug = debug_time
            .or_else(|| last_modified_time(dir))
            .map(date_jj_mm_yyyy)
            .unwrap_or_else(date_du_jour);
        let date_release = release_time
            .map(date_jj_mm_yyyy)
            .unwrap_or_else(|| date_debug.clone());
        projects.push(ProjectEntry {
            id: name.clone(),
            name,
            path: dir.to_path_buf(),
            description: String::new(),
            status: String::new(),
            version: String::new(),
            token: String::new(),
            used_pct: String::new(),
            spent: String::new(),
            last_model: String::new(),
            modified,
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
            date_debug,
            date_release,
            probe_debug: String::new(),
            probe_release: String::new(),
            build_debug: String::new(),
            build_release: String::new(),
            debug_modified,
            release_modified,
            last_compile_debug_secs: 0,
            last_compile_release_secs: 0,
            last_target_debug_secs: 0,
            last_target_release_secs: 0,
            debug_snapshot: None,
            release_snapshot: None,
            version_manually_edited: false,
            source_hash: String::new(),
            tarif_mode: default_tarif_mode(),
            tarif_manual: 0.0,
            tarif_web_series: Vec::new(),
            tarif_web_fetched_ms: 0,
            tarif_web_secteur: false,
            tarif_web_pays: String::new(),
            est_bibliotheque: est_lib,
            typologie: typologie_auto(dir).to_string(),
        });
        added += 1;
    };
    let mut dirs = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    for dir in &dirs {
        // La racine des bibliothèques est un conteneur, pas un projet : elle ne
        // doit jamais apparaître dans la liste comme ses propres filles.
        if dir != lib_root {
            ajouter_entree(dir, false);
        }
    }
    // Les bibliothèques vivent une cascade plus bas, sous `lib_root` : les
    // découvrir demande de balayer cette racine, pas seulement la racine des
    // projets. Balayage réservé au cas où `lib_root` relève bien de la racine
    // indexée : un registre partiel (temp de test, racine archiée) ne doit
    // pas ramasser les bibliothèques d'une autre racine.
    if lib_root.is_dir() && lib_root.strip_prefix(root).is_ok() {
        let mut libs = Vec::new();
        for entry in fs::read_dir(lib_root)? {
            let path = entry?.path();
            if path.is_dir() {
                libs.push(path);
            }
        }
        libs.sort();
        for dir in &libs {
            ajouter_entree(dir, true);
        }
    }
    Ok(added)
}

fn write_private(path: &Path, content: &str) -> Result<()> {
    // Le fichier est créé avec le bon mode dès l'ouverture. Un `fs::write`
    // laissait la création à l'umask : le registre réapparuissait en 0644
    // pendant le temps du `chmod` qui suit, lisible par le groupe et les
    // autres. Fenêtre courte, mais inutile.
    if path.exists() {
        fs::write(path, content)?;
    } else {
        perms::create_private(path, content.as_bytes())?;
        return Ok(());
    }
    perms::private_file(path)?;
    Ok(())
}

fn ensure_regular(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path)
      {
        Ok(meta) if meta.file_type().is_symlink() => {
          Err(anyhow!("refus : {} est un lien symbolique", path.display()))
        }
        _ => Ok(()),
      }
}

pub(crate) fn last_modified_time(dir: &Path) -> Option<std::time::SystemTime> {
    if !dir.exists() {
        return None;
    }
    let mut best: Option<std::time::SystemTime> = None;
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        if let Ok(meta) = entry.metadata()
            && let Ok(mtime) = meta.modified()
            && best.is_none_or(|b| mtime > b)
        {
            best = Some(mtime);
        }
    }
    best.or_else(|| fs::metadata(dir).ok().and_then(|m| m.modified().ok()))
}

fn format_mod_time(t: std::time::SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.format("%d/%m/%Y %H:%M").to_string()
}

/// Date « jj-mm-yyyy » d'un instant, format des dates déclarées de variantes.
pub(crate) fn date_jj_mm_yyyy(t: std::time::SystemTime) -> String {
    let dt: chrono::DateTime<chrono::Local> = t.into();
    dt.format("%d-%m-%Y").to_string()
}

/// Date du jour au format « jj-mm-yyyy », pour initialiser une fiche neuve.
pub(crate) fn date_du_jour() -> String {
    chrono::Local::now().format("%d-%m-%Y").to_string()
}

/// Quatre réponses pour une seule traversée de l'arborescence d'un projet.
///
/// Chaque appel séparé à `last_modified_time` / `variant_modified` reparcourt
/// l'intégralité de `target/`. Sur un projet compilé, c'est des dizaines de
/// milliers d'inodes lus plusieurs fois par lancement ; le regroupement ci
///-dessous ramène le nombre de traversées à une par projet.
pub(crate) struct ProjectTimes {
    /// Fichier le plus récent du projet, `target/` compris.
    pub(crate) overall: Option<std::time::SystemTime>,
    /// Fichier le plus récent sous `target/debug`.
    pub(crate) debug: Option<std::time::SystemTime>,
    /// Fichier le plus récent sous `target/release`.
    pub(crate) release: Option<std::time::SystemTime>,
    /// Fichier le plus récent parmi `Cargo.toml` et `src/**/*.rs`.
    pub(crate) source: Option<std::time::SystemTime>,
}

pub(crate) fn project_times(dir: &Path) -> ProjectTimes {
    let mut t = ProjectTimes {
        overall: None,
        debug: None,
        release: None,
        source: None,
    };
    if !dir.exists() {
        return t;
    }
    let debug_dir = dir.join("target").join("debug");
    let release_dir = dir.join("target").join("release");
    for entree in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
    {
        let chemin = entree.path();
        let Ok(meta) = entree.metadata() else {
            continue;
        };
        let Ok(mtime) = meta.modified() else {
            continue;
        };
        if t.overall.is_none_or(|b| mtime > b) {
            t.overall = Some(mtime);
        }
        if chemin.starts_with(&debug_dir) && t.debug.is_none_or(|b| mtime > b) {
            t.debug = Some(mtime);
        }
        if chemin.starts_with(&release_dir) && t.release.is_none_or(|b| mtime > b) {
            t.release = Some(mtime);
        }
        if est_fichier_source(chemin, dir) && t.source.is_none_or(|b| mtime > b) {
            t.source = Some(mtime);
        }
    }
    t
}

/// `Cargo.toml` ou `.rs` sous `src/` : le périmètre suivi par
/// [`compute_source_hash`], donc celui qui décide d'un bump AutoPatch.
///
/// Doit rester aligné avec [`fichiers_sources`] : les dossiers de sauvegarde
/// sont exclus ici aussi, sinon une sauvegarde récente ferait croire à une
/// modification de code.
fn est_fichier_source(chemin: &Path, dir: &Path) -> bool {
    if chemin == dir.join("Cargo.toml") {
        return true;
    }
    let src_dir = dir.join("src");
    chemin.starts_with(&src_dir)
        && chemin.extension().is_some_and(|e| e == "rs")
        && chemin_suivi(chemin, &src_dir)
}

pub(crate) fn variant_modified(dir: &Path, profile: &str) -> Option<String> {
    last_modified_time(&dir.join("target").join(profile)).map(format_mod_time)
}

/// mtime (SystemTime) du fichier le plus récent sous `target/{profile}`.
pub(crate) fn variant_modified_time(dir: &Path, profile: &str) -> Option<std::time::SystemTime>
  {
    last_modified_time(&dir.join("target").join(profile))
  }

/// mtime de référence d'un projet pour l'affichage et le tri « Modifié » :
/// la cible construite quand elle existe (Debug, puis Release), l'ensemble du
/// dossier sinon. Aligné sur ce que `load_registry`/`reindex` posent dans
/// `ProjectEntry::modified`.
pub(crate) fn cible_modified_time(dir: &Path) -> Option<std::time::SystemTime>
  {
    variant_modified_time(dir, "debug")
        .or_else(|| variant_modified_time(dir, "release"))
        .or_else(|| last_modified_time(dir))
  }

/// Date de modification la plus récente parmi les sources suivies par
/// [`compute_source_hash`] : `Cargo.toml` + `src/**/*.rs`.
///
/// Sert à signaler un binaire plus ancien que le code qu'il est censé
/// exécuter : c'est la cause d'une version « figée », silencieuse sinon.
pub(crate) fn source_modified_time(dir: &Path) -> Option<std::time::SystemTime> {
    let mut best: Option<std::time::SystemTime> = None;
    let mut considere = |p: &Path| {
        if let Ok(meta) = fs::metadata(p)
            && let Ok(mtime) = meta.modified()
            && best.is_none_or(|b| mtime > b)
        {
            best = Some(mtime);
        }
    };
    considere(&dir.join("Cargo.toml"));
    for fichier in fichiers_sources(dir) {
        considere(&fichier);
    }
    best
}

/// Vrai si `nom` désigne un dossier de sauvegarde ou de travail jetable.
///
/// Un dossier de sauvegarde posé sous `src/` est indétectable à la compilation
/// — Rust n'y descend pas — mais il change le hash de sources. Résultat observé :
/// huit bumps de version successifs alors qu'aucune ligne de code n'avait
/// bougé. Ces dossiers sont donc exclus explicitement du périmètre suivi.
///
/// Le nom est retenu tel quel ou avec un préfixe (`src_bck`, `main_Bck`) :
/// c'est sous cette forme que les copies de sauvegarde apparaissent en pratique.
fn est_dossier_sauvegarde(nom: &std::ffi::OsStr) -> bool {
    let n = nom.to_string_lossy().to_ascii_lowercase();
    const SUFFIXES: [&str; 5] = ["_bck", "_bak", "_backup", "_old", "_orig"];
    matches!(
        n.as_str(),
        "bck" | "bak" | "backup" | "backups" | "old" | "orig" | ".bak"
    ) || SUFFIXES.iter().any(|s| n.ends_with(s))
}

/// Sous-arborescence réellementcompilee : `src/` moins les dossiers de
/// sauvegarde. Appliqué à chaque entrée pour ne dépendre d'aucun `skip_pattern`
/// de parcours.
fn chemin_suivi(chemin: &Path, src_dir: &Path) -> bool {
    let Ok(relatif) = chemin.strip_prefix(src_dir) else {
        return true;
    };
    !relatif
        .components()
        .any(|c| est_dossier_sauvegarde(c.as_os_str()))
}

/// Liste triée des fichiers `.rs` réellement compilés sous `src/`, pour un
/// parcours déterministe.
fn fichiers_sources(dir: &Path) -> Vec<PathBuf> {
    let src_dir = dir.join("src");
    if !src_dir.is_dir() {
        return Vec::new();
    }
    let mut files: Vec<PathBuf> = walkdir::WalkDir::new(&src_dir)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || chemin_suivi(e.path(), &src_dir))
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort();
    files
}

/// Calcule le hash SHA256 des sources du projet (Cargo.toml + src/**/*.rs).
/// Utilisé pour détecter les modifications de code indépendamment du mtime
/// de target/debug (build incrémental peu fiable).
pub(crate) fn compute_source_hash(dir: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();

    // Hash Cargo.toml en premier
    let cargo_toml = dir.join("Cargo.toml");
    if let Ok(content) = fs::read(&cargo_toml) {
        hasher.update(&content);
    }

    // Hash tous les fichiers .rs sous src/
    let src_dir = dir.join("src");
    if src_dir.is_dir() {
        let files = fichiers_sources(dir);
        // Trier pour ordre déterministe
        for file in &files {
            if let Ok(content) = fs::read(file) {
                hasher.update(&content);
            }
        }
    }

    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests
  {
    use std::{
        collections::HashMap,
        fs,
        path::{Path, PathBuf},
    };
    use crate::registry::tarif_valide;

    use super::{
        accepter_modification, default_tarif_mode, default_web_country, load_preferences,
        load_refus, load_registry, load_web_cache, montant_local, save_preferences, save_refus,
        save_registry_full, Preferences, RefusCoherence, Result, SnapshotCompile, TarifPoint, Tri,
        validate_status, validate_version, ProjectEntry, STATUSES, WebCache,
    };

    fn temp_registry(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("op-start-menu-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Écrit un registre en ne connaissant que les projets.
    ///
    /// Raccourci des tests : il relit le cache et les préférences depuis le
    /// fichier, qu'un test n'a pas forcément en main. Le programme, lui, passe
    /// toujours l'état en mémoire à `save_registry_full` — passer par une
    /// relecture du disque n'y serait pas équivalent.
    fn save_registry(path: &Path, projects: &[ProjectEntry]) -> Result<()> {
        let web = load_web_cache(path);
        let prefs = load_preferences(path);
        save_registry_full(path, projects, &web, &prefs).map(|_| ())
    }

    /// Un refus enregistré doit survivre à une réécriture du registre, qui ne
    /// connaît que la vue d'usage `ProjectEntry` et ignore les refus.
    #[test]
    fn les_refus_survivent_a_une_reecriture_du_registre()
      {
        let dir = temp_registry("refus-survit");
        let path = dir.join("projects.json");
        let projets = vec![entree("a", "1.0.0", "1.0.0")];
        save_registry(&path, &projets).unwrap();

        let mut refus = HashMap::new();
        refus.insert("a".to_string(), vec![RefusCoherence {
            ecart: "sources>debug".to_string(),
            action: "rebuild-debug".to_string(),
            quand: "15-10-2026".to_string(),
        }]);
        save_refus(&path, &refus).unwrap();

        // Une écriture ultérieure, qui ignore les refus, les préserve.
        save_registry(&path, &projets).unwrap();
        let relus = load_refus(&path);
        assert_eq!(relus.get("a").map(Vec::len), Some(1), "refus préservé");
        assert_eq!(relus["a"][0].action, "rebuild-debug");
        assert_eq!(relus["a"][0].quand, "15-10-2026");

        // Un refus levé disparaît du fichier, sinon il resterait écrit.
        refus.remove("a");
        save_refus(&path, &refus).unwrap();
        assert!(load_refus(&path).is_empty(), "refus levé effacé");
      }

    /// Les dossiers de sauvegarde placés sous `src/` ne doivent pas changer
    /// le hash de sources : Rust ne les compile pas, mais une retouche dans
    /// l'un d'eux déclenchait un bump de version à chaque rescan.
    #[test]
    fn les_dossiers_de_sauvegarde_ne_entrent_pas_dans_le_hash() {
        let dir = temp_registry("hash-sans-bck");
        fs::create_dir_all(dir.join("src/sub")).unwrap();
        fs::create_dir_all(dir.join("src/src_bck")).unwrap();
        fs::create_dir_all(dir.join("src/backup")).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nversion = \"1.0.0\"\n").unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("src/sub/mod.rs"), "// vrai code\n").unwrap();
        let reference = super::compute_source_hash(&dir);

        // On écrit dans les deux emplacements de sauvegarde : aucun ne doit
        // changer le hash.
        fs::write(dir.join("src/src_bck/main.rs"), "vieille version\n").unwrap();
        assert_eq!(super::compute_source_hash(&dir), reference);
        fs::write(dir.join("src/backup/main.rs"), "vieille version\n").unwrap();
        assert_eq!(super::compute_source_hash(&dir), reference);

        // En revanche un vrai fichier du module change bien le hash.
        fs::write(dir.join("src/sub/mod.rs"), "// nouveau code\n").unwrap();
        assert_ne!(super::compute_source_hash(&dir), reference);

        // Et mtime la plus récente : une sauvegarde ne doit pas dater le code.
        let retains: Vec<String> = super::fichiers_sources(&dir)
            .iter()
            .map(|p| {
                p.strip_prefix(&dir)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        assert_eq!(
            retains,
            vec!["src/main.rs".to_string(), "src/sub/mod.rs".to_string()],
            "seuls les vrais fichiers compilés doivent être suivis"
        );
        fs::remove_dir_all(&dir).ok();
    }

    fn assert_0600(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
    }

    #[test]
    fn load_creates_empty_registry_0600() {
        let dir = temp_registry("empty");
        let path = dir.join("opencode/projects.json");

        let projects = load_registry(&path).unwrap();

        assert!(projects.is_empty());
        assert_0600(&path);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Le fichier ne doit jamais exister avec les droits de l'umask. La
    /// création passe désormais par `create_private`, qui porte le mode 0600
    /// dans l'appel `open` : avant, un `fs::write` laissait le registre
    /// lisible par tous le temps du `chmod` qui suivait.
    #[test]
    fn le_registre_ne_se_cree_jamais_en_0644() {
        let dir = temp_registry("mode_creation");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        // Le contenu est bien celui écrit, pas seulement un fichier vide.
        assert!(fs::read_to_string(&path).unwrap().contains("projects"));
        assert_0600(&path);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Le cas de compatibilité : un registre écrit avant l'introduction du bloc
    /// `preferences`. Il doit se charger tel quel et recevoir les valeurs par
    /// défaut — surtout `capture: true`, sinon le simple fait d'ouvrir un menu
    /// ancient désactiverait la capture pour tout le monde.
    #[test]
    fn un_registre_anterieur_aux_preferences_se_charge_avec_les_defauts() {
        let dir = temp_registry("backcompat");
        let path = dir.join("opencode/projects.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
                 "projects": {},
                 "web_rates_cache": {},
                 "web_last_fetch_ms": 0,
                 "web_country": "FRA",
                 "web_secteur": {},
                 "web_last_failure_ms": 0
               }"#,
        )
        .unwrap();

        let prefs = load_preferences(&path);
        assert_eq!(prefs, Preferences::default());
        assert!(prefs.capture, "la capture doit rester active par défaut");
        assert_eq!(prefs.tri, Tri::Date);
        assert_eq!(prefs.colonnes.len(), super::TOUTES_COLONNES.len());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Le registre réel de l'utilisateur est écrit à la main et peut contenir
    /// des champs qu'aucun code ne connaît. Ils doivent survivre à une écriture
    /// de préférences : sinon upgrader le menu perdrait des données.
    #[test]
    fn ecrire_les_preferences_preserve_le_reste_du_registre() {
        let dir = temp_registry("preserve");
        let path = dir.join("opencode/projects.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r#"{
                 "projects": {
                   "demo": {
                     "name": "Démo",
                     "path": "/tmp/demo",
                     "description": "d",
                     "status": "Actif",
                     "version": "1.0.0",
                     "tarif_manual": 42.5,
                     "tarif_mode": "manuel"
                   }
                 },
                 "web_country": "FRA",
                 "champ_inconnu": "a conserver"
               }"#,
        )
        .unwrap();

        let mut prefs = load_preferences(&path);
        prefs.tri = Tri::Nom;
        prefs.launch_root = true;
        save_preferences(&path, &prefs).unwrap();

        let relu = load_preferences(&path);
        assert_eq!(relu.tri, Tri::Nom);
        assert!(relu.launch_root);

        let texte = fs::read_to_string(&path).unwrap();
        let valeur: serde_json::Value = serde_json::from_str(&texte).unwrap();
        assert_eq!(valeur["web_country"], "FRA", "le cache web doit survivre");
        assert_eq!(
            valeur["projects"]["demo"]["tarif_manual"], 42.5,
            "les projets et leurs tarifs doivent survivre"
        );
        assert_eq!(
            valeur["champ_inconnu"], "a conserver",
            "un champ inconnu ne doit pas être écrasé"
        );
        assert_0600(&path);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Une écriture identique ne doit pas toucher le fichier : inutile de
    /// réécrire, et surtout inutile de faire tourner le renommage atomique pour
    /// rien.
    #[test]
    fn preferences_identiques_ne_reecrivent_pas() {
        let dir = temp_registry("noop");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();
        let prefs = load_preferences(&path);
        save_preferences(&path, &prefs).unwrap();

        // Aucun temporaire ne doit subsister.
        let restes: Vec<_> = fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.contains(".tmp"))
            .collect();
        assert!(restes.is_empty(), "temporaires résiduels : {restes:?}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn la_visibilite_des_colonnes_suit_la_liste() {
        let mut prefs = Preferences::default();
        assert!(prefs.colonne_visible("Tokens"));
        assert!(prefs.colonne_visible("Sessions"));

        prefs.colonnes.retain(|c| c != "Tokens");
        assert!(!prefs.colonne_visible("Tokens"));
        assert!(prefs.colonne_visible("Coût"));

        // Libellé inconnu : ignoré, sans faire paniquer.
        assert!(!prefs.colonne_visible("Colonne fantôme"));
    }

    #[test]
    fn save_then_load_round_trip() {
        let dir = temp_registry("roundtrip");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        save_registry(
            &path,
            &[ProjectEntry {
                id: "test".to_string(),
                name: "Projet Test".to_string(),
                path: PathBuf::from("/tmp/nonexistent-proj"),
                description: "desc".to_string(),
                status: "En-cours Beta".to_string(),
                version: "1.0.0".to_string(),
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
                tarif_mode: default_tarif_mode(),
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
            }],
        )
        .unwrap();

        assert_0600(&path);

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "test");
        assert_eq!(loaded[0].name, "Projet Test");
        assert_eq!(loaded[0].path, PathBuf::from("/tmp/nonexistent-proj"));
        assert_eq!(loaded[0].status, "En-cours Beta");
        assert_eq!(loaded[0].version, "1.0.0");
        assert_eq!(loaded[0].modified, "—");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_preserves_0600_after_multiple_writes() {
        let dir = temp_registry("multi");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        for i in 0..3 {
            save_registry(
                &path,
                &[ProjectEntry {
                    id: format!("p{i}"),
                    name: format!("Projet {i}"),
                    path: PathBuf::from("/tmp/nonexistent-proj"),
                    description: String::new(),
                    status: String::new(),
                    version: String::new(),
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
                    tarif_mode: default_tarif_mode(),
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
            }],
            )
            .unwrap();
            assert_0600(&path);
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Entrée minimale de test : seul le chemin compte pour la dérivation.
    fn entree_chemin(path: PathBuf) -> ProjectEntry {
        ProjectEntry {
            id: String::new(),
            name: String::new(),
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
            tarif_mode: default_tarif_mode(),
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

    /// L'entrée devient une bibliothèque dès que son chemin tombe sous la
    /// racine des bibliothèques, qu'elle ait été déclarée ou non.
    #[test]
    fn la_derivation_des_bibliotheques_suit_la_racine() {
        let dir = temp_registry("deriv-libs");
        let libs = dir.join("Libs");
        let mut projets = vec![
            entree_chemin(libs.join("monlib")),
            entree_chemin(dir.join("monapp")),
        ];
        super::deriver_bibliotheques(&mut projets, &libs);
        assert!(projets[0].est_bibliotheque, "sous la racine : bibliothèque");
        assert!(!projets[1].est_bibliotheque, "hors racine : projet ordinaire");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Une bibliothèque déclarée hors de la racine le reste : la dérivation
    /// ajoute, elle ne retire jamais.
    #[test]
    fn la_mention_explicite_dune_bibliotheque_survit_hors_racine() {
        let dir = temp_registry("mention-lib");
        let libs = dir.join("Libs");
        let mut p = entree_chemin(dir.join("maison"));
        p.est_bibliotheque = true;
        super::deriver_bibliotheques(std::slice::from_mut(&mut p), &libs);
        assert!(p.est_bibliotheque, "la mention explicite ne se perd pas");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// La typologie est auto-détectée depuis le contenu : une appli a un
    /// binaire (`src/main.rs`), une lib n'en a pas, un dossier vide est
    /// `Other`.
    #[test]
    fn typologie_auto_distingue_appli_lib_et_other() {
        let dir = temp_registry("typologie-auto");
        let app = dir.join("app");
        fs::create_dir_all(app.join("src")).unwrap();
        fs::write(app.join("src/main.rs"), "fn main() {}").unwrap();
        assert_eq!(super::typologie_auto(&app), "Code-App");

        let lib = dir.join("lib");
        fs::create_dir_all(lib.join("src")).unwrap();
        fs::write(lib.join("src/lib.rs"), "").unwrap();
        assert_eq!(super::typologie_auto(&lib), "Code-Lib");

        let vide = dir.join("vide");
        fs::create_dir_all(&vide).unwrap();
        assert_eq!(super::typologie_auto(&vide), "Other");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Une typologie absente d'un ancien registre est migrée par auto-détection
    /// au chargement.
    #[test]
    fn la_typologie_absente_est_migree_au_chargement() {
        let dir = temp_registry("typologie-migration");
        let projet = dir.join("app");
        fs::create_dir_all(projet.join("src")).unwrap();
        fs::write(projet.join("src/main.rs"), "fn main() {}").unwrap();
        let registre = dir.join("projects.json");
        let json = serde_json::json!({
            "projects": {
                "app": {
                    "name": "app",
                    "path": projet.to_str().unwrap(),
                    "description": ""
                }
            }
        });
        fs::write(&registre, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        let projets = super::load_registry(&registre).unwrap();
        assert_eq!(projets[0].typologie, "Code-App", "auto-détectée à la lecture");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Le conteneur des bibliothèques n'est jamais un projet, mais ses filles
    /// le sont — et marquées comme bibliothèques.
    #[test]
    fn reindex_marque_les_bibliotheques_sans_depouiller_leur_conteneur() {
        let dir = temp_registry("reindex-libs");
        let root = dir.join("projets");
        fs::create_dir_all(root.join("app")).unwrap();
        fs::create_dir_all(root.join("Libs").join("monlib")).unwrap();
        let mut projets = Vec::new();
        let ajoutes = super::reindex_from_disk(&root, &root.join("Libs"), &mut projets).unwrap();
        assert_eq!(ajoutes, 2, "le projet et la bibliothèque, pas le conteneur");
        let lib = projets
            .iter()
            .find(|p| p.path == root.join("Libs").join("monlib"))
            .expect("la bibliothèque découverte");
        assert!(lib.est_bibliotheque, "sous la racine des bibliothèques");
        let app = projets
            .iter()
            .find(|p| p.path == root.join("app"))
            .expect("le projet découvert");
        assert!(!app.est_bibliotheque, "hors de la racine des bibliothèques");
        assert!(
            !projets.iter().any(|p| p.path == root.join("Libs")),
            "le conteneur n'apparaît pas comme projet"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_registry() {
        let dir = temp_registry("symlink");
        let target = dir.join("target.json");
        let link = dir.join("link.json");
        fs::write(&target, "{}").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert!(load_registry(&link).is_err());
        assert!(save_registry(&link, &[]).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validate_version_accepts_x_y_z() {
        assert!(validate_version("1.2.3"));
        assert!(validate_version("0.0.1"));
        assert!(validate_version("123.0.45"));
        assert!(!validate_version("1.2"));
        assert!(!validate_version("1.2.3.4"));
        assert!(!validate_version("1..3"));
        assert!(!validate_version("1.2.beta"));
        assert!(!validate_version(""));
        assert!(!validate_version("1.2-3"));
    }

    #[test]
    fn tarif_valide_refuse_inf_et_nan() {
        assert!(tarif_valide(0.0));
        assert!(tarif_valide(0.5));
        assert!(tarif_valide(42.0));
        // `inf` passe `>= 0.0` : c'est exactement le trou qui corromprait le
        // registre, puisque serde_json l'écrit en `null` sans erreur.
        assert!(!tarif_valide(f64::INFINITY));
        assert!(!tarif_valide(f64::NEG_INFINITY));
        assert!(!tarif_valide(f64::NAN));
        assert!(!tarif_valide(-1.0));
    }

    /// Régression P0-3 : un tarif non fini ne doit pas rendre le registre
    /// illisible. `save_registry_full` neutralise la valeur ; le rechargement
    /// doit donc réussir et rendre un tarif exploitable.
    #[test]
    fn un_tarif_non_fini_ne_peut_pas_corrompre_le_registre() {
        let path = temp_registry("tarif-infini").join("projects.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "projects": { "demo": { "name": "demo", "path": "/tmp/demo", "description": "" } }
            }))
            .unwrap(),
        )
        .unwrap();
        let mut e = load_registry(&path).unwrap().remove(0);
        e.tarif_manual = f64::INFINITY;
        save_registry_full(&path, &[e], &WebCache::default(), &Preferences::default()).unwrap();

        // Le fichier produit ne doit contenir aucun `null` de tarif, et surtout
        // rester relisible.
        let brut = fs::read_to_string(&path).unwrap();
        assert!(
            !brut.contains("\"tarif_manual\": null"),
            "un tarif infini sérialisé en null : {brut}"
        );
        let relu = load_registry(&path).expect("le registre doit rester lisible");
        assert_eq!(relu.len(), 1);
        assert!(
            relu[0].tarif_manual.is_finite(),
            "tarif relu non fini : {}",
            relu[0].tarif_manual
        );
        assert_eq!(relu[0].tarif_manual, 0.0);
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn validate_status_against_fixed_set() {
        assert!(STATUSES.contains(&"Beta"));
        for known in STATUSES {
            assert!(validate_status(known));
        }
        assert!(!validate_status("En cours"));
        assert!(!validate_status("Preview"));
        assert!(!validate_status(""));
    }

    /// Un registre pré-dissociation n'a qu'un seul statut/version, celui du
    /// Debug. La Release reste vierge : recopier le Debug dessus ferait croire
    /// à une compilation release qui n'est jamais mesurée.
    #[test]
    fn load_migrates_legacy_status_version_to_debug_but_not_release() {
        let dir = temp_registry("migrate");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        let legacy = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"En-cours Beta","version":"2.1.0"}}}"#;
        fs::write(&path, legacy).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].status_debug, "En-cours Beta");
        assert_eq!(loaded[0].version_debug, "2.1.0");
        assert_eq!(loaded[0].status_release, "");
        assert_eq!(loaded[0].version_release, "");

        // Une relecture ne doit pas non plus regénérer la Release depuis le
        // Debug : le comportement doit être stable d'un chargement à l'autre.
        let relu = load_registry(&path).unwrap();
        assert_eq!(relu[0].status_release, "");
        assert_eq!(relu[0].version_release, "");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_migrates_legacy_last_compile_secs_to_both_refs() {
        let dir = temp_registry("migrate_compile");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        // Registre ancien avec last_compile_secs non-nul, sans les nouveaux champs
        let legacy = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"Debug","version":"1.0.0","last_compile_secs":1234567890}}}"#;
        fs::write(&path, legacy).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        // L'ancienne référence doit être dupliquée vers les deux variantes
        assert_eq!(loaded[0].last_compile_debug_secs, 1234567890);
        assert_eq!(loaded[0].last_compile_release_secs, 1234567890);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_migrates_version_manually_edited_default_false() {
        let dir = temp_registry("migrate_edited");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        // Registre ancien sans le champ version_manually_edited
        let legacy = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"Debug","version":"1.0.0"}}}"#;
        fs::write(&path, legacy).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        // Le champ doit être par défaut à false
        assert!(!loaded[0].version_manually_edited);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_keeps_existing_debug_release_variants() {
        let dir = temp_registry("keepvar");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        let json = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"legacy","version":"9.9.9","status_debug":"Debug","version_debug":"1.1.1","status_release":"Release","version_release":"2.2.2"}}}"#;
        fs::write(&path, json).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded[0].status_debug, "Debug");
        assert_eq!(loaded[0].version_debug, "1.1.1");
        assert_eq!(loaded[0].status_release, "Release");
        assert_eq!(loaded[0].version_release, "2.2.2");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Un registre sans dates déclarées doit en obtenir deux, lisibles et
    /// stables : mtime de build si présent, sinon date du jour. La Release
    /// sans compilation hérite de la date Debug (« jamais buildée »).
    #[test]
    fn load_migrates_missing_variant_dates() {
        let dir = temp_registry("migrate_dates");
        let proj = dir.join("proj");
        fs::create_dir_all(proj.join("target/debug")).unwrap();
        fs::write(proj.join("target/debug/art.rlib"), b"x").unwrap();

        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();
        let json = format!(
            r#"{{"projects":{{"proj":{{"name":"proj","path":"{}","description":"d","status":"","version":"1.0.0"}}}}}}"#,
            proj.display()
        );
        fs::write(&path, json).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        let attendu = super::date_jj_mm_yyyy(
            super::variant_modified_time(&proj, "debug").expect("mtime debug"),
        );
        assert_eq!(loaded[0].date_debug, attendu);
        // Release jamais compilée : hérite de la date Debug.
        assert_eq!(loaded[0].date_release, loaded[0].date_debug);
        assert!(!loaded[0].date_debug.is_empty());
        // Format jj-mm-yyyy, longueur fixe de 10.
        assert_eq!(loaded[0].date_debug.len(), 10);

        // Relecture : les dates migrées ne doivent pas bouger.
        let relu = load_registry(&path).unwrap();
        assert_eq!(relu[0].date_debug, loaded[0].date_debug);
        assert_eq!(relu[0].date_release, loaded[0].date_release);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Les dates déclarées dans la fiche font foi : la migration ne doit pas
    /// les écraser par les mtimes.
    #[test]
    fn load_keeps_declared_variant_dates() {
        let dir = temp_registry("keep_dates");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        let json = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"","version":"1.0.0","date_debug":"01-02-2024","date_release":"03-04-2024","probe_debug":"./old-debug -V","build_release":"make release"}}}"#;
        fs::write(&path, json).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded[0].date_debug, "01-02-2024");
        assert_eq!(loaded[0].date_release, "03-04-2024");
        assert_eq!(loaded[0].probe_debug, "./old-debug -V");
        assert_eq!(loaded[0].probe_release, "");
        assert_eq!(loaded[0].build_debug, "");
        assert_eq!(loaded[0].build_release, "make release");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn variant_modified_returns_none_without_target_dir() {
        let dir = temp_registry("vartgt");
        assert_eq!(super::variant_modified(&dir, "debug"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_registry_fills_variant_dates() {
        let dir = temp_registry("vardates");
        let proj = dir.join("proj");
        fs::create_dir_all(proj.join("target/debug")).unwrap();
        fs::write(proj.join("target/debug/art.rlib"), b"x").unwrap();

        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();
        let json = format!(
            r#"{{"projects":{{"proj":{{"name":"proj","path":"{}","description":"d","status":"","version":"1.0.0"}}}}}}"#,
            proj.display()
        );
        fs::write(&path, json).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_ne!(loaded[0].debug_modified, "jamais compilée");
        assert_eq!(loaded[0].release_modified, "jamais compilée");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn variant_modified_formats_latest_target_mtime() {
        let dir = temp_registry("vartgt2");
        let debug = dir.join("target/debug");
        fs::create_dir_all(&debug).unwrap();
        fs::write(debug.join("old.rlib"), b"x").unwrap();
        fs::write(debug.join("new.rlib"), b"x").unwrap();
        let new_mtime = fs::metadata(debug.join("new.rlib")).unwrap().modified().unwrap();
        let label = super::variant_modified(&dir, "debug").expect("date debug");
        let expected = super::format_mod_time(new_mtime);
        assert_eq!(label, expected, "doit refléter le mtime le plus récent");
        assert_eq!(super::variant_modified(&dir, "release"), None);
        fs::remove_dir_all(&dir).unwrap();
    }

    // --- Valorisation du temps passé ----------------------------------

    fn serie_test() -> Vec<super::TarifPoint> {
        vec![
            super::TarifPoint {
                annee: 2023,
                valeur_eur_h: 20.0,
                devise_src: "EUR".to_string(),
                tx_eur: 1.0,
                fetched_ms: 1_700_000_000_000,
            },
            super::TarifPoint {
                annee: 2024,
                valeur_eur_h: 25.0,
                devise_src: "EUR".to_string(),
                tx_eur: 1.0,
                fetched_ms: 1_700_000_000_000,
            },
        ]
    }

    /// Timestamp UTC d'un 1er juillet de l'année demandée.
    fn ms_annee(annee: i32) -> i64 {
        use chrono::TimeZone;
        chrono::Utc
            .with_ymd_and_hms(annee, 7, 1, 12, 0, 0)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn tarif_point_choisit_annee_anterieure_ou_egale() {
        let serie = serie_test();
        // Une session de 2023 doit utiliser le tarif 2023, pas celui de 2024.
        let p = super::tarif_point_for_ms(&serie, ms_annee(2023)).unwrap();
        assert_eq!(p.annee, 2023);
        // Une session de 2025 doit basculer sur le dernier point connu.
        let p = super::tarif_point_for_ms(&serie, ms_annee(2025)).unwrap();
        assert_eq!(p.annee, 2024);
    }

    #[test]
    fn tarif_point_replie_sur_premier_point_si_session_anterieure() {
        let serie = serie_test();
        // Session en 2020 : antérieure à toute la série, on prend 2023.
        let p = super::tarif_point_for_ms(&serie, ms_annee(2020)).unwrap();
        assert_eq!(p.annee, 2023, "doit retomber sur le premier point connu");
    }

    #[test]
    fn tarif_point_none_sans_serie() {
        assert!(super::tarif_point_for_ms(&[], ms_annee(2024)).is_none());
    }

    #[test]
    fn valoriser_session_web_utilise_le_tarif_de_la_session() {
        let serie = serie_test();
        // 2 heures de session en 2023 à 20 €/h = 40 €.
        let v = super::valoriser_session(
            7_200_000,
            ms_annee(2023),
            &serie,
            true,
            1_700_000_000_000,
            "web",
            0.0,
        )
        .unwrap();
        assert!((v.euros - 40.0).abs() < 0.001, "obtenu {}", v.euros);
        assert_eq!(v.annee, 2023);
        assert!(v.web);
        // 1_700_000_000_000 ms ≈ nov. 2023 → maj 11/2023
        assert_eq!(v.etiquette(), "OIT·J·2023·maj 11/2023");
    }

    #[test]
    fn valoriser_session_manuel_ignore_la_serie() {
        let serie = serie_test();
        let v = super::valoriser_session(
            3_600_000,
            ms_annee(2024),
            &serie,
            true,
            0,
            "manuel",
            50.0,
        )
        .unwrap();
        assert!((v.euros - 50.0).abs() < 0.001);
        assert!(!v.web);
        assert_eq!(v.etiquette(), "manuel");
    }

    #[test]
    fn valoriser_session_sans_duree_ou_sans_tarif() {
        let serie = serie_test();
        // Durée nulle : rien à valoriser.
        assert!(super::valoriser_session(0, ms_annee(2024), &serie, true, 0, "web", 0.0).is_none());
        // Mode manuel sans tarif saisi : rien à valoriser.
        assert!(
            super::valoriser_session(3_600_000, ms_annee(2024), &serie, true, 0, "manuel", 0.0)
                .is_none()
        );
    }

    #[test]
    fn valoriser_cumul_applique_le_tarif_de_chaque_session() {
        let serie = serie_test();
        // 1 h en 2023 (20 €/h) + 1 h en 2025 (25 €/h, dernier point connu) = 45 €.
        let sessions = vec![
            super::SessionDuree {
                time_created_ms: ms_annee(2023),
                duration_ms: 3_600_000,
            },
            super::SessionDuree {
                time_created_ms: ms_annee(2025),
                duration_ms: 3_600_000,
            },
        ];
        let v =
            super::valoriser_cumul(&sessions, &serie, true, 0, "web", 0.0).unwrap();
        assert!((v.euros - 45.0).abs() < 0.001, "obtenu {}", v.euros);
        assert!((v.tarif_h - 22.5).abs() < 0.001, "moyenne pondérée {}", v.tarif_h);
        assert_eq!(v.annee, 2024, "étiquette = année la plus récente utilisée");
    }

    #[test]
    fn valoriser_cumul_ignore_sessions_sans_duree() {
        let sessions = vec![super::SessionDuree {
            time_created_ms: ms_annee(2023),
            duration_ms: 0,
        }];
        assert!(
            super::valoriser_cumul(&sessions, &serie_test(), true, 0, "web", 0.0).is_none()
        );
    }

    // --- Formatage ----------------------------------------------------

    #[test]
    fn format_duree_sans_zero_et_unites() {
        assert_eq!(super::format_duree(0), "—");
        assert_eq!(super::format_duree(45_000), "45 s");
        assert_eq!(super::format_duree(12 * 60_000), "12 min");
        assert_eq!(super::format_duree(3_600_000), "1 h");
        assert_eq!(super::format_duree(5_000_000), "1 h 23 min");
    }

    #[test]
    fn format_mois_annee_localise() {
        // 2026-09-15 → « 09/2026 ».
        use chrono::TimeZone;
        let ts = chrono::Utc
            .with_ymd_and_hms(2026, 9, 15, 0, 0, 0)
            .unwrap()
            .timestamp_millis();
        assert_eq!(super::format_mois_annee(ts), "09/2026");
        // Date invalide : tiret cadratin plutôt qu'un affichage trompeur.
        assert_eq!(super::format_mois_annee(i64::MAX / 2), "—");
    }

    #[test]
    fn etiquette_signale_la_devise_source() {
        let v = super::Valorisation {
            euros: 10.0,
            tarif_h: 10.0,
            annee: 2024,
            devise_src: "USD".to_string(),
            web: true,
            secteur: false,
            fetched_ms: 0,
        };
        assert_eq!(v.etiquette(), "OIT·2024·USD");
    }

    // --- Source web (OIT/ILOSTAT via SDMX) ---------------------------

    #[test]
    fn parse_sdmx_csv_extrait_annee_valeur_devise() {
        let csv = "DATAFLOW,REF_AREA,FREQ,MEASURE,SEX,ECO,CUR,TIME_PERIOD,OBS_VALUE,OBS_STATUS,UNIT_MEASURE_TYPE,UNIT_MEASURE,UNIT_MULT,SOURCE,NOTE_SOURCE,NOTE_INDICATOR,NOTE_CLASSIF,DECIMALS,UPPER_BOUND,LOWER_BOUND\n\
ILO:DF(1.0),FRA,A,EAR_EHRA_NB,SEX_T,ECO_ISIC4_J,CUR_TYPE_LCU,2023,22.178,,NB,CR,0,LFS,Eur, EUR: Euro,,2,,\n";
        let points = super::parse_sdmx_csv(csv);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].annee, 2023);
        assert!((points[0].valeur - 22.178).abs() < 0.0001);
        assert_eq!(points[0].devise, "EUR");
    }

    #[test]
    fn parse_sdmx_csv_accepte_dataflow_national_19_colonnes() {
        // Le dataflow de repli n'a pas la colonne ECO : 19 colonnes seulement.
        let csv = "DATAFLOW,REF_AREA,FREQ,MEASURE,SEX,CUR,TIME_PERIOD,OBS_VALUE,OBS_STATUS,UNIT_MEASURE_TYPE,UNIT_MEASURE,UNIT_MULT,SOURCE,NOTE_SOURCE,NOTE_INDICATOR,NOTE_CLASSIF,DECIMALS,UPPER_BOUND,LOWER_BOUND\n\
ILO:DF(1.0),FRA,A,EAR_EHRA_NB,SEX_T,CUR_TYPE_LCU,2024,16.189,,NB,CR,0,LFS,Eur, EUR: Euro,,2,,\n";
        let points = super::parse_sdmx_csv(csv);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].annee, 2024);
        assert!((points[0].valeur - 16.189).abs() < 0.0001);
        assert_eq!(points[0].devise, "EUR");
    }

    #[test]
    fn parse_sdmx_csv_ignore_lignes_non_exploitables() {
        // Seules les colonnes utiles sont requises : une ligne tronquée après
        // OBS_VALUE reste exploitable, une ligne sans valeur non plus.
        let csv = "DATAFLOW,REF_AREA,FREQ,MEASURE,SEX,ECO,CUR,TIME_PERIOD,OBS_VALUE,OBS_STATUS,UNIT_MEASURE_TYPE,UNIT_MEASURE,UNIT_MULT,SOURCE,NOTE_SOURCE,NOTE_INDICATOR,NOTE_CLASSIF,DECIMALS,UPPER_BOUND,LOWER_BOUND\n\
ILO:DF(1.0),FRA,A,EAR_EHRA_NB,SEX_T,ECO_ISIC4_J,CUR_TYPE_LCU,2023,22.178\n\
ILO:DF(1.0),FRA,A,EAR_EHRA_NB,SEX_T,ECO_ISIC4_J,CUR_TYPE_LCU,,,,,,,\n\
ILO:DF(1.0),FRA,A,EAR_EHRA_NB,SEX_T,ECO_ISIC4_J,CUR_TYPE_LCU,2024,,,NB,CR,0,LFS,Eur,,,2,,\n\
ILO:DF(1.0),FRA,A,EAR_EHRA_NB,SEX_T,ECO_ISIC4_J,CUR_TYPE_LCU,2025,20.0,,NB,CR,0,LFS,Eur,EUR: Euro,,2,,\n";
        let points = super::parse_sdmx_csv(csv);
        // 2023 (valeur présente, ligne tronquée) et 2025 (ligne complète).
        assert_eq!(points.len(), 2, "les lignes sans valeur sont écartées");
        assert_eq!(points[0].annee, 2023);
        assert!((points[0].valeur - 22.178).abs() < 0.0001);
        assert_eq!(points[1].annee, 2025);
    }

    #[test]
    fn parse_sdmx_csv_vide_sans_donnees() {
        assert!(super::parse_sdmx_csv("").is_empty());
        assert!(super::parse_sdmx_csv("No data is found. Please adjust your query parameters.").is_empty());
    }

    #[test]
    fn devise_depuis_note_isole_le_code_iso() {
        assert_eq!(super::devise_depuis_note("EUR: Euro").as_deref(), Some("EUR"));
        assert_eq!(
            super::devise_depuis_note("Break in series: revised | USD: United States dollar")
                .as_deref(),
            Some("USD")
        );
        // Aucun code ISO valide dans un libellé libre.
        assert_eq!(super::devise_depuis_note("LFS - Enquête sur l'emploi"), None);
    }

    /// Runtime mono-thread pour les tests qui doivent await une fonction
    /// réseau. Indispensable : un `block_on` direct depuis un runtime déjà
    /// actif panique.
    fn runtime_de_test() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn les_affichages_du_temps_passe_sont_persistes()
      {
        // Ces champs sont recalculés à chaque rescan, mais doivent survivre à
        // une écriture/lecture du registre : sinon ils disparaissent du JSON.
        let path = temp_registry("persistance").join("projects.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&serde_json::json!({
                "projects": { "demo": { "name": "demo", "path": "/tmp/demo", "description": "" } }
            }))
            .unwrap(),
        )
        .unwrap();
        let mut e = super::load_registry(&path).unwrap().remove(0);
        e.name = "demo".to_string();
        e.last_session_context_pct = "42%".to_string();
        e.last_duree = "1 h 23 min".to_string();
        e.total_duree = "12 min".to_string();
        e.last_valorisation = "15,00 €".to_string();
        e.total_valorisation = "150,00 €".to_string();
        e.tarif_resume = "web · 21,58 €/h · 2024".to_string();
        super::save_registry_full(&path, &[e.clone()], &super::WebCache::default(), &super::Preferences::default())
            .unwrap();

        let relu = super::load_registry(&path).unwrap();
        assert_eq!(relu.len(), 1);
        assert_eq!(relu[0].last_session_context_pct, "42%");
        assert_eq!(relu[0].last_duree, "1 h 23 min");
        assert_eq!(relu[0].total_duree, "12 min");
        assert_eq!(relu[0].last_valorisation, "15,00 €");
        assert_eq!(relu[0].total_valorisation, "150,00 €");
        assert_eq!(relu[0].tarif_resume, "web · 21,58 €/h · 2024");
        let _ = fs::remove_dir_all(path.parent().unwrap());
      }

    #[test]
    fn convertir_en_eur_applique_le_taux_et_dedoublonne_les_annees() {
        let bruts = vec![
            super::TarifPointBrut {
                annee: 2024,
                valeur: 70.0,
                devise: "USD".to_string(),
            },
            // Rupture de série : la dernière valeur de l'année l'emporte.
            super::TarifPointBrut {
                annee: 2024,
                valeur: 72.0,
                devise: "USD".to_string(),
            },
        ];
        let points = runtime_de_test().block_on(super::convertir_en_eur(&bruts, 1_700_000_000_000));
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].annee, 2024);
        assert_eq!(points[0].devise_src, "USD");
        assert!(points[0].tx_eur > 0.0);
        assert!((points[0].valeur_eur_h - 72.0 * points[0].tx_eur).abs() < 0.0001);
    }

    #[test]
    fn convertir_en_eur_ecarte_les_devises_inconnues() {
        let bruts = vec![super::TarifPointBrut {
            annee: 2024,
            valeur: 10.0,
            devise: "XYZ".to_string(),
        }];
        assert!(runtime_de_test().block_on(super::convertir_en_eur(&bruts, 0)).is_empty());
    }

    #[test]
    fn urls_sdmx_ont_la_bonne_forme_et_les_bonnes_dimensions() {
        let u = super::ilostat_url_secteur("FRA", 2014, 2027);
        assert!(u.starts_with("https://sdmx.ilo.org/rest/data/ILO,DF_EAR_EHRA_SEX_ECO_CUR_NB,1.0/FRA.A.EAR_EHRA_NB.SEX_T.ECO_ISIC4_J.CUR_TYPE_LCU"));
        assert!(u.contains("startPeriod=2014"));
        assert!(u.contains("endPeriod=2027"));

        let n = super::ilostat_url_national("DEU", 2014, 2027);
        assert!(n.starts_with("https://sdmx.ilo.org/rest/data/ILO,DF_EAR_EHRA_SEX_CUR_NB,1.0/DEU.A.EAR_EHRA_NB.SEX_T.CUR_TYPE_LCU"));
    }

    // --- Cache web ---------------------------------------------------

    #[test]
    fn cache_web_perime_au_dela_de_trente_jours() {
        let jour = 86_400_000i64;
        let pays = default_web_country();
        let pays_clone = pays.clone();
        let cache = super::WebCache {
            country: pays,
            rates: {
                let mut m = HashMap::new();
                m.insert(pays_clone, vec![TarifPoint {
                    annee: 2024,
                    valeur_eur_h: 20.0,
                    devise_src: "EUR".to_string(),
                    tx_eur: 1.0,
                    fetched_ms: 1_000_000_000_000,
                }]);
                m
            },
            ..Default::default()
        };
        // Juste avant la limite de 30 jours : pas de rechargement.
        assert!(!cache.a_fetcher(1_000_000_000_000 + 29 * jour));
        // Au-delà : rechargement.
        assert!(cache.a_fetcher(1_000_000_000_000 + 30 * jour));
    }

    #[test]
    fn cache_web_vide_demande_toujours_un_telechargement() {
        assert!(super::WebCache::default().a_fetcher(1_700_000_000_000));
    }

    #[test]
    fn load_web_cache_lit_les_champs_globaux() {
        let dir = temp_registry("webcache");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();
        let json = r#"{"projects":{},"web_country":"DEU","web_last_fetch_ms":1700000000000}"#;
        fs::write(&path, json).unwrap();

        let cache = super::load_web_cache(&path);
        assert_eq!(cache.country, "DEU", "le pays configuré est conservé");
        assert_eq!(cache.last_fetch_ms, 1_700_000_000_000);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn load_web_cache_sur_registre_illisible_ne_panique_pas() {
        let dir = temp_registry("webcache_ko");
        let path = dir.join("opencode/projects.json");
        // Fichier absent : le cache reste vide avec un pays par défaut valide.
        let cache = super::load_web_cache(&path);
        assert!(cache.rates.is_empty());
        assert_eq!(cache.country.len(), 3, "pays ISO3 par défaut");
        // Contenu illisible : même comportement, aucune panique.
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ pas du json").unwrap();
        assert!(super::load_web_cache(&path).rates.is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_web_country_reste_un_code_iso3_valide() {
        let pays = super::default_web_country();
        assert_eq!(pays.len(), 3, "pays : {pays}");
        assert!(pays.chars().all(|c| c.is_ascii_alphabetic()), "pays : {pays}");
    }

    fn projet_avec_serie(pays: &str, serie: Vec<super::TarifPoint>) -> ProjectEntry {
        ProjectEntry {
            id: "p".to_string(),
            name: "p".to_string(),
            path: PathBuf::from("/tmp/p"),
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
            tarif_mode: default_tarif_mode(),
            tarif_manual: 0.0,
            tarif_web_series: serie,
            tarif_web_fetched_ms: 1_700_000_000_000,
            tarif_web_secteur: true,
            tarif_web_pays: pays.to_string(),
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
    fn serie_du_projet_suit_le_pays_courant() {
        let cache = super::WebCache {
            country: "DEU".to_string(),
            rates: HashMap::from([("DEU".to_string(), vec![super::TarifPoint {
                annee: 2021,
                valeur_eur_h: 24.05,
                devise_src: "EUR".to_string(),
                tx_eur: 1.0,
                fetched_ms: 1_800_000_000_000,
            }])]),
            secteur: HashMap::from([("DEU".to_string(), false)]),
            last_fetch_ms: 1_800_000_000_000,
            ..Default::default()
        };
        // La copie du projet se rapporte à FRA : elle ne doit pas être utilisée
        // alors que le pays courant est DEU.
        let projet = projet_avec_serie("FRA", serie_test());
        let (serie, secteur, _) = super::serie_du_projet(&projet, &cache);
        assert_eq!(serie.len(), 1);
        assert_eq!(serie[0].annee, 2021, "la série du pays courant doit primer");
        assert!(!secteur, "repli national signalé");
    }

    #[test]
    fn serie_du_projet_utilise_la_copie_si_pays_identique() {
        let cache = super::WebCache {
            country: "FRA".to_string(),
            ..Default::default()
        };
        let projet = projet_avec_serie("FRA", serie_test());
        let (serie, secteur, _) = super::serie_du_projet(&projet, &cache);
        assert_eq!(serie.len(), 2);
        assert!(secteur);
    }

    #[test]
    fn serie_du_projet_ignore_copie_sans_pays_registre_ancien() {
        // Registre écrit avant l'introduction de tarif_web_pays : la copie
        // ne peut pas être attribuée à un pays, elle est écartée.
        let mut projet = projet_avec_serie("", serie_test());
        projet.tarif_web_pays = String::new();
        let cache = super::WebCache {
            country: "DEU".to_string(),
            rates: HashMap::from([("DEU".to_string(), serie_test())]),
            ..Default::default()
        };
        let (serie, _, _) = super::serie_du_projet(&projet, &cache);
        assert_eq!(serie.len(), 2, "la série du cache est utilisée");
    }

    #[test]
    fn serie_du_projet_vide_sans_cache_ni_copie() {
        let projet = projet_avec_serie("FRA", Vec::new());
        let cache = super::WebCache {
            country: "FRA".to_string(),
            ..Default::default()
        };
        let (serie, secteur, _) = super::serie_du_projet(&projet, &cache);
        assert!(serie.is_empty());
        assert!(!secteur);
    }

    #[test]
    fn rafraichir_force_true_telecharge_meme_si_frais() {
        let jour = 86_400_000i64;
        let pays = "FRA".to_string();
        let cache = super::WebCache {
            country: pays.clone(),
            rates: HashMap::from([(pays.clone(), vec![super::TarifPoint {
                annee: 2024,
                valeur_eur_h: 20.0,
                devise_src: "EUR".to_string(),
                tx_eur: 1.0,
                fetched_ms: 1_000_000_000_000,
            }])]),
            secteur: HashMap::from([(pays.clone(), true)]),
            last_fetch_ms: 1_000_000_000_000,
            ..Default::default()
        };
        // Sans force : pas de rechargement (cache frais) — on teste via a_fetcher
        assert!(!cache.a_fetcher(1_000_000_000_000 + 10 * jour));
        // Le test réel de rafraichir avec force=true nécessiterait un mock HTTP
    }

    #[test]
    fn a_fetcher_par_pays_et_non_global() {
        let jour = 86_400_000i64;
        let pays = "FRA".to_string();
        let mut cache = super::WebCache {
            country: pays.clone(),
            rates: HashMap::from([(pays.clone(), vec![super::TarifPoint {
                annee: 2024,
                valeur_eur_h: 20.0,
                devise_src: "EUR".to_string(),
                tx_eur: 1.0,
                fetched_ms: 1_000_000_000_000,
            }])]),
            secteur: HashMap::from([(pays.clone(), true)]),
            last_fetch_ms: 0,
            ..Default::default()
        };
        // Pays FRA a une série fraîche → pas de fetch
        assert!(!cache.a_fetcher(1_000_000_000_000 + 10 * jour));
        // Changement de pays vers DEU (pas de série) → fetch nécessaire
        cache.country = "DEU".to_string();
        assert!(cache.a_fetcher(1_000_000_000_000 + 10 * jour));
    }

    #[test]
    fn serie_du_projet_prefere_cache_plus_frais() {
        let pays = "FRA".to_string();
        let serie_locale = vec![
            super::TarifPoint { annee: 2023, valeur_eur_h: 20.0, devise_src: "EUR".to_string(), tx_eur: 1.0, fetched_ms: 1_000_000_000_000 },
            super::TarifPoint { annee: 2024, valeur_eur_h: 25.0, devise_src: "EUR".to_string(), tx_eur: 1.0, fetched_ms: 1_000_000_000_000 },
        ];
        // Cache plus frais que la copie locale
        let cache = super::WebCache {
            country: pays.clone(),
            rates: HashMap::from([(pays.clone(), vec![
                super::TarifPoint { annee: 2023, valeur_eur_h: 20.0, devise_src: "EUR".to_string(), tx_eur: 1.0, fetched_ms: 1_200_000_000_000 },
                super::TarifPoint { annee: 2024, valeur_eur_h: 25.0, devise_src: "EUR".to_string(), tx_eur: 1.0, fetched_ms: 1_200_000_000_000 },
                super::TarifPoint { annee: 2025, valeur_eur_h: 30.0, devise_src: "EUR".to_string(), tx_eur: 1.0, fetched_ms: 1_200_000_000_000 },
            ])]),
            secteur: HashMap::from([(pays.clone(), true)]),
            last_fetch_ms: 1_200_000_000_000,
            ..Default::default()
        };
        let mut projet = projet_avec_serie(&pays, serie_locale);
        projet.tarif_web_fetched_ms = 1_000_000_000_000;
        let (serie, _, fetched) = super::serie_du_projet(&projet, &cache);
        assert_eq!(serie.len(), 3, "le cache plus frais doit primer");
        assert_eq!(fetched, 1_200_000_000_000);
    }

    #[test]
    fn valoriser_cumul_garde_annee_plus_recente() {
        let serie = serie_test();
        // Sessions en ordre décroissant (plus récent d'abord) : 2025 puis 2023
        let sessions = vec![
            super::SessionDuree { duration_ms: 3_600_000, time_created_ms: ms_annee(2025) },
            super::SessionDuree { duration_ms: 3_600_000, time_created_ms: ms_annee(2023) },
        ];
        let v = super::valoriser_cumul(&sessions, &serie, true, 1_700_000_000_000, "web", 0.0).unwrap();
        assert_eq!(v.annee, 2024, "l'année doit être celle de la session la plus récente (2025 → tarif 2024)");
    }

    #[test]
    fn usage_du_projet_agrege_racine_et_modules() {
        let mut usage: HashMap<PathBuf, super::ProjectUsage> = HashMap::new();
        let mut a = super::ProjectUsage::default();
        a.cumulative.total_tokens = 10;
        a.cumulative.session_count = 1;
        usage.insert(PathBuf::from("/p/proj"), a);
        let mut b = super::ProjectUsage::default();
        b.cumulative.total_tokens = 5;
        b.cumulative.session_count = 2;
        usage.insert(PathBuf::from("/p/proj/api"), b);
        let mut c = super::ProjectUsage::default();
        c.cumulative.total_tokens = 99;
        usage.insert(PathBuf::from("/p/proj2"), c);
        let agg = super::usage_du_projet(&usage, std::path::Path::new("/p/proj")).unwrap();
        assert_eq!(agg.cumulative.total_tokens, 15);
        assert_eq!(agg.cumulative.session_count, 3);
        assert!(super::usage_du_projet(&usage, std::path::Path::new("/p/autre")).is_none());
    }

    #[test]
    fn parse_csv_line_gerer_guillemets_et_virgules_embarquees() {
        // Ligne avec champ cité contenant une virgule
        let ligne = r#"2024,25.5,"NOTE, avec virgule",EUR"#;
        let champs = super::parse_csv_line(ligne);
        assert_eq!(champs.len(), 4);
        assert_eq!(champs[0], "2024");
        assert_eq!(champs[1], "25.5");
        assert_eq!(champs[2], "NOTE, avec virgule");
        assert_eq!(champs[3], "EUR");
        // Guillemets doubles échappés
        let ligne2 = r#"2024,25.5,"Il a dit ""Bonjour""",EUR"#;
        let champs2 = super::parse_csv_line(ligne2);
        assert_eq!(champs2[2], r#"Il a dit "Bonjour""#);
    }

    /// Les montants en virgule flottante : 10,0 × 0,92 ne vaut pas exactement
    /// 9,2 en binaire, et un `assert_eq!` échouerait sur la seule façon dont
    /// l'ordinateur représente le résultat, pas sur sa valeur.
    fn assert_proche(obtenu: Option<f64>, attendu: f64, quoi: &str)
      {
        match obtenu
          {
            Some(v) => assert!(
              (v - attendu).abs() < 1e-9,
              "{quoi} : {v} devrait valoir {attendu}"
            ),
            None => panic!("{quoi} : aucun montant, {attendu} attendu"),
          }
      }

    /// La conversion d'une session suit trois sources dans l'ordre. Cet ordre
    /// est aussi celui du cumul des totaux : les deux passent par la même
    /// fonction, donc la ligne « Converti » et la ligne TOTAL ne peuvent pas
    /// se contredire.
    #[test]
    fn montant_local_suit_la_priorite_des_taux()
      {
        // Le montant déjà converti prime sur le taux de la session.
        assert_proche(montant_local(10.0, "USD", Some(0.5), Some(9.2)), 9.2, "montant stocké");
        // Sans montant converti, le taux stocké avec la session.
        assert_proche(montant_local(10.0, "USD", Some(0.5), None), 5.0, "taux de session");
        // Sans rien, le taux de repli — celui qu'utiliserait le cumul.
        assert_proche(montant_local(10.0, "USD", None, None), 9.2, "taux de repli");
        // Un taux nul est absent, pas nul : le repli prend le relais.
        assert_proche(montant_local(10.0, "USD", Some(0.0), None), 9.2, "taux nul");
      }

    /// Un taux aberrant ne doit pas rendre le total illisible : il est écarté
    /// au profit du repli, comme le taux nul.
    #[test]
    fn montant_local_ecarte_les_taux_aberrants()
      {
        // Un taux infini est écarté comme un taux nul : le repli prend le relais.
        assert_proche(
          montant_local(10.0, "USD", Some(f64::INFINITY), None),
          9.2,
          "taux infini",
        );
        // Un montant converti infini est écarté lui aussi, puis le taux l'est
        // aussi : il ne reste que le repli.
        assert_proche(
          montant_local(10.0, "USD", Some(f64::INFINITY), Some(f64::INFINITY)),
          9.2,
          "montant et taux infinis",
        );
        // Sans repli, il ne reste rien : aucune conversion plutôt qu'une
        // conversion infinie.
        assert_eq!(montant_local(10.0, "XXX", Some(f64::INFINITY), Some(f64::INFINITY)), None);
      }

    /// opencode écrit `cost = 0` quand le fournisseur ne publie aucune grille
    /// tarifaire. Convertir ce zéro donnerait « 0,00 € », qui se lit comme une
    /// session gratuite : il n'y a pas de montant à convertir.
    #[test]
    fn montant_local_refuse_un_cout_nul()
      {
        assert_eq!(montant_local(0.0, "USD", Some(0.92), Some(0.0)), None);
        assert_eq!(montant_local(0.0, "EUR", Some(1.0), Some(0.0)), None);
      }

    /// Une devise sans taux ni repli n'a rien à afficher : mieux vaut une case
    /// vide qu'un montant inventé.
    #[test]
    fn montant_local_renvoie_none_sans_taux_connu()
      {
        assert_eq!(montant_local(10.0, "XXX", None, None), None);
        assert_eq!(montant_local(10.0, "", None, None), None);
      }

    fn entree(id: &str, version: &str, version_release: &str) -> ProjectEntry {
        ProjectEntry {
            id: id.to_string(),
            name: id.to_string(),
            path: PathBuf::from(format!("/tmp/{id}")),
            description: String::new(),
            status: String::new(),
            version: version.to_string(),
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
            version_debug: version.to_string(),
            status_release: String::new(),
            version_release: version_release.to_string(),
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
            tarif_mode: default_tarif_mode(),
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

    /// Une métadonnée modifiée sans être déclarée doit être restaurée à sa
    /// valeur persistée, avec une alerte — le registre est un fichier partagé
    /// qu'une version ultérieure (ou une main) ne doit pas dériver.
    #[test]
    fn garde_restaure_une_modification_non_declaree()
      {
        let dir = temp_registry("garde-revert");
        let path = dir.join("opencode/projects.json");
        let e = entree("g1", "1.0.0", "1.0.0");
        load_registry(&path).unwrap();
        save_registry_full(&path, std::slice::from_ref(&e), &WebCache::default(), &Preferences::default())
          .unwrap()
          .assert_empty_alertes();

        // Reload puis mutation non déclarée (bug ou édition manuelle interne).
        let mut proj = load_registry(&path).unwrap().remove(0);
        proj.version_debug = "9.9.9".to_string();
        proj.version = "9.9.9".to_string();
        let alertes = save_registry_full(&path, &[proj], &WebCache::default(), &Preferences::default())
          .unwrap();
        assert_eq!(alertes.len(), 2, "version_debug et version restaurées : {alertes:?}");

        let relu = load_registry(&path).unwrap();
        assert_eq!(relu[0].version_debug, "1.0.0", "métadonnée restaurée sur le disque");
        fs::remove_dir_all(&dir).unwrap();
      }

    /// Une mutation déclarée via [`accepter_modification`] doit, elle, survivre.
    #[test]
    fn garde_laisse_passer_une_modification_declaree()
      {
        let dir = temp_registry("garde-ok");
        let path = dir.join("opencode/projects.json");
        let e = entree("g2", "1.0.0", "1.0.0");
        load_registry(&path).unwrap();
        save_registry_full(&path, std::slice::from_ref(&e), &WebCache::default(), &Preferences::default())
          .unwrap()
          .assert_empty_alertes();

        let mut proj = load_registry(&path).unwrap().remove(0);
        accepter_modification(format!("{}:version_debug", proj.id));
        accepter_modification(format!("{}:version", proj.id));
        proj.version_debug = "8.8.8".to_string();
        proj.version = "8.8.8".to_string();
        let alertes = save_registry_full(&path, &[proj], &WebCache::default(), &Preferences::default())
          .unwrap();
        assert!(alertes.is_empty(), "aucune restauration attendue : {alertes:?}");

        let relu = load_registry(&path).unwrap();
        assert_eq!(relu[0].version_debug, "8.8.8", "mutation déclarée persistée");
        fs::remove_dir_all(&dir).unwrap();
      }

    /// Les snapshots de build validé voyagent avec la fiche (`serde`).
    #[test]
    fn snapshots_de_build_passent_l_ecriture_et_la_lecture()
      {
        let dir = temp_registry("snapshots");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();
        let mut e = entree("s1", "0.0.1", "0.0.1");
        e.debug_snapshot = Some(SnapshotCompile {
            version: "0.0.1".to_string(),
            statut: "Release".to_string(),
            date: "09-10-2026".to_string(),
            compile_secs: 1_700_000_000,
            empreinte_v: "abc123".to_string(),
        });
        e.release_snapshot = Some(SnapshotCompile {
            version: "0.20.24".to_string(),
            statut: "Beta-Release".to_string(),
            date: "14-10-2026".to_string(),
            compile_secs: 1_700_000_100,
            empreinte_v: "def456".to_string(),
        });
        save_registry_full(&path, &[e.clone()], &WebCache::default(), &Preferences::default())
          .unwrap()
          .assert_empty_alertes();

        let relu = load_registry(&path).unwrap();
        assert_eq!(relu[0].debug_snapshot, e.debug_snapshot);
        assert_eq!(relu[0].release_snapshot, e.release_snapshot);
        fs::remove_dir_all(&dir).unwrap();
      }

    /// Un registre écrit avant l'introduction des snapshots migre vers
    /// « aucune fiche » — la sonde prendra alors le relais au chargement.
    #[test]
    fn registre_ancien_sans_snapshots_migre_vers_aucune_fiche()
      {
        let dir = temp_registry("snap-vieil");
        let path = dir.join("opencode/projects.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
          &path,
          serde_json::json!({
              "projects": { "v": { "name": "v", "path": "/tmp/v", "description": "",
                  "version": "1.0.0", "version_debug": "1.0.0" } }
          })
          .to_string(),
        )
        .unwrap();
        let relu = load_registry(&path).unwrap();
        assert_eq!(relu[0].debug_snapshot, None);
        assert_eq!(relu[0].release_snapshot, None);
        assert_eq!(relu[0].version_debug, "1.0.0", "champs historiques conservés");
        fs::remove_dir_all(&dir).unwrap();
      }

    /// Une fiche écrite avant l'ajout de `date`/`empreinte_v` se recharge avec
    /// ces champs vides : la garde pourra les compléter depuis le binaire.
    #[test]
    fn fiche_ancienne_sans_date_ni_empreinte_migre_vide()
      {
        let dir = temp_registry("snap-champs-neufs");
        let path = dir.join("opencode/projects.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
          &path,
          serde_json::json!({
              "projects": { "v": {
                  "name": "v", "path": "/tmp/v", "description": "",
                  "version": "1.0.0", "version_debug": "1.0.0",
                  "debug_snapshot": {
                      "version": "1.0.0", "statut": "Debug", "compile_secs": 42
                  } } }
          })
          .to_string(),
        )
        .unwrap();
        let relu = load_registry(&path).unwrap();
        let fiche = relu[0].debug_snapshot.clone().expect("fiche conservée");
        assert_eq!(fiche.version, "1.0.0");
        assert_eq!(fiche.statut, "Debug");
        assert_eq!(fiche.compile_secs, 42);
        assert!(fiche.date.is_empty(), "date migrée vide");
        assert!(fiche.empreinte_v.is_empty(), "empreinte migrée vide");
        fs::remove_dir_all(&dir).unwrap();
      }

    /// Alerte dédiée pour les tests : une écriture ne doit rien restaurer.
    trait AlertesAssert {
        fn assert_empty_alertes(self) -> Vec<String>;
    }
    impl AlertesAssert for Vec<String> {
        fn assert_empty_alertes(self) -> Vec<String> {
            assert!(self.is_empty(), "restauration inattendue : {self:?}");
            self
        }
    }
  }
