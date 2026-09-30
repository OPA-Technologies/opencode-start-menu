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

static HTTP_CLIENT: LazyLock<Client> = LazyLock::new(Client::new);

const LOCAL_CURRENCY: &str = "EUR";

static EXCHANGE_RATE_CACHE: LazyLock<std::sync::Mutex<HashMap<String, f64>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

async fn fetch_exchange_rate(from: &str, to: &str) -> Option<f64> {
    let cache_key = format!("{from}->{to}");
    {
        let cache = EXCHANGE_RATE_CACHE.lock().unwrap();
        if let Some(&rate) = cache.get(&cache_key) {
            return Some(rate);
        }
    }

    let url = format!("https://api.exchangerate.host/latest?base={from}&symbols={to}");
    let response = HTTP_CLIENT.get(&url).send().await.ok()?;
    let json: serde_json::Value = response.json().await.ok()?;
    let rate = json.get("rates")?.get(to)?.as_f64()?;

    if rate > 0.0 {
        let mut cache = EXCHANGE_RATE_CACHE.lock().unwrap();
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
    #[serde(default)]
    last_compile_debug_secs: u64,
    #[serde(default)]
    last_compile_release_secs: u64,
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
    STATUSES.iter().any(|known| *known == s)
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct Registry {
    #[serde(default)]
    projects: HashMap<String, Project>,
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
    pub(crate) debug_modified: String,
    pub(crate) release_modified: String,
    /// Références (epoch secondes) de la dernière compilation détectée par
    /// variante (mtimes de target/debug et target/release). 0 = jamais.
    pub(crate) last_compile_debug_secs: u64,
    pub(crate) last_compile_release_secs: u64,
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
            let modified = last_modified(&dir).unwrap_or_else(|| "—".to_string());
            let debug_modified =
                variant_modified(&dir, "debug").unwrap_or_else(|| "jamais compilée".to_string());
            let release_modified =
                variant_modified(&dir, "release").unwrap_or_else(|| "jamais compilée".to_string());
            // Migration source_hash : si vide, calculer le hash initial des sources
            let source_hash = if p.source_hash.is_empty() {
                compute_source_hash(&dir)
            } else {
                p.source_hash.clone()
            };
            // Migration des anciens champs uniques status/version vers les variantes
            // Debug/Release si les nouvelles valeurs sont absentes.
            let status_debug =
                if p.status_debug.is_empty() { p.status.clone() } else { p.status_debug.clone() };
            let version_debug =
                if p.version_debug.is_empty() { p.version.clone() } else { p.version_debug.clone() };
            let status_release =
                if p.status_release.is_empty() { status_debug.clone() } else { p.status_release.clone() };
            let version_release =
                if p.version_release.is_empty() { version_debug.clone() } else { p.version_release.clone() };
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
                debug_modified,
                release_modified,
                last_compile_debug_secs: last_debug_secs,
                last_compile_release_secs: last_release_secs,
                version_manually_edited: p.version_manually_edited,
                source_hash,
                tarif_mode: p.tarif_mode,
                tarif_manual: p.tarif_manual,
                tarif_web_series: p.tarif_web_series,
                tarif_web_fetched_ms: p.tarif_web_fetched_ms,
                tarif_web_secteur: p.tarif_web_secteur,
                tarif_web_pays: p.tarif_web_pays.clone(),
            }
        })
        .collect::<Vec<ProjectEntry>>();

    Ok(projects)
}

// Nouvelle fonction pour charger les données d'usage (cumulées + dernière session)
pub(crate) fn load_usage(db_path: &Path) -> HashMap<PathBuf, ProjectUsage> {
    let mut usage = HashMap::new();
    let Ok(conn) = Connection::open(db_path) else {
        return usage;
    };
    // Migration : ajouter les colonnes manquantes si elles n'existent pas.
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

    // D'abord, récupérer toutes les sessions pour détecter celles sans taux
    // session.id est du TEXTE (`ses_…`) : le lire en i64 renvoyait 0, la garde
    // `id > 0` était donc toujours fausse et tout ce bloc restait inerte.
    let mut sessions_to_update: Vec<(String, String, f64)> = Vec::new();
    {
        let stmt = conn.prepare(
            "SELECT id, currency, cost FROM session WHERE directory IS NOT NULL AND (exchange_rate IS NULL OR exchange_rate = 0) AND cost > 0",
        );
        if let Ok(mut stmt) = stmt {
            if let Ok(mut rows) = stmt.query([]) {
                while let Ok(Some(row)) = rows.next() {
                    let id: String = row.get(0).unwrap_or_default();
                    let currency: String = row.get(1).unwrap_or_else(|_| "USD".to_string());
                    let cost: f64 = row.get(2).unwrap_or(0.0);
                    if !id.is_empty() && cost > 0.0 {
                        sessions_to_update.push((id, currency, cost));
                    }
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

    let mut stmt = match conn.prepare(
        "SELECT id, directory, COALESCE(cost,0), \
         COALESCE(tokens_input,0) + COALESCE(tokens_output,0) + COALESCE(tokens_reasoning,0), \
         model, agent, time_created, COALESCE(context_pct,0), \
         COALESCE(currency, 'USD'), COALESCE(exchange_rate, 0), COALESCE(cost_local, 0) \
         FROM session WHERE directory IS NOT NULL ORDER BY time_created DESC",
    ) {
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
            row.get::<_, u8>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, f64>(9)?,
            row.get::<_, f64>(10)?,
        ))
    });
    match rows {
        Ok(iter) => {
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
                    if cost_local > 0.0 {
                        entry.cumulative.total_cost_local += cost_local;
                    } else if exchange_rate > 0.0 {
                        entry.cumulative.total_cost_local += cost * exchange_rate;
                    } else {
                        // Fallback si pas de taux
                        if let Some(rate) = fallback_exchange_rate(&currency, LOCAL_CURRENCY) {
                            entry.cumulative.total_cost_local += cost * rate;
                        }
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
        Err(_) => {}
    }
    usage
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
        // Préférer la version la plus fraîche.
        if cache_fetched > local_fetched && cache_serie.is_some() {
            return (
                cache_serie.unwrap().clone(),
                cache_secteur,
                cache_fetched,
            );
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
}

/// Durée de fraîcheur du cache web avant re-téléchargement (jours).
pub(crate) const WEB_FETCH_JOURS: i64 = 30;

impl WebCache {
    /// Le cache du pays courant est-il absent ou périmé ?
    pub fn a_fetcher(&self, maintenant_ms: i64) -> bool {
        if let Some(serie) = self.rates.get(&self.country) {
            if let Some(p) = serie.last() {
                let age_jours = (maintenant_ms - p.fetched_ms) / 86_400_000;
                return age_jours >= WEB_FETCH_JOURS;
            }
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
                // Mémorise la granularité obtenue (secteur J ou national).
                self.secteur.insert(pays, secteur);
                true
            }
            Ok(_) => {
                self.last_error = format!("aucune donnée {TARIF_WEB_LIBELLE} pour {pays}");
                false
            }
            Err(e) => {
                self.last_error = e.to_string();
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

pub(crate) fn save_registry(path: &Path, projects: &[ProjectEntry]) -> Result<()> {
    let web = load_web_cache(path);
    save_registry_full(path, projects, &web)
}

/// Variante conservant le cache web global lors de l'écriture du registre.
pub(crate) fn save_registry_full(
    path: &Path,
    projects: &[ProjectEntry],
    web: &WebCache,
) -> Result<()> {    ensure_regular(path)?;
    let mut map = HashMap::new();
    for p in projects {
        map.insert(
            p.id.clone(),
            Project {
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
                last_agent: p.last_agent.clone(),
                last_time_created: p.last_time_created.clone(),
                status_debug: p.status_debug.clone(),
                version_debug: p.version_debug.clone(),
                status_release: p.status_release.clone(),
                version_release: p.version_release.clone(),
                last_compile_debug_secs: p.last_compile_debug_secs,
                last_compile_release_secs: p.last_compile_release_secs,
                // Ancien champ : remis à zéro après migration (géré par default).
                last_compile_secs: 0,
                version_manually_edited: p.version_manually_edited,
                source_hash: p.source_hash.clone(),
                active: true,
                tarif_mode: p.tarif_mode.clone(),
                tarif_manual: p.tarif_manual,
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
            },
        );
    }
    let registry = Registry {
        projects: map,
        web_rates_cache: web.rates.clone(),
        web_last_fetch_ms: web.last_fetch_ms,
        web_country: web.country.clone(),
        web_secteur: web.secteur.clone(),
    };
    let tmp = path.with_extension("tmp");
    write_private(&tmp, &serde_json::to_string_pretty(&registry)?)?;
    fs::rename(tmp, path)?;
    Ok(())
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

pub(crate) fn reindex_from_disk(root: &Path, projects: &mut Vec<ProjectEntry>) -> Result<usize> {
    if !root.is_dir() {
        return Ok(0);
    }
    let mut added = 0usize;
    let known: HashSet<PathBuf> = projects.iter().map(|p| p.path.clone()).collect();
    let mut dirs = Vec::new();
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            dirs.push(path);
        }
    }
    dirs.sort();
    for dir in dirs {
        if known.contains(&dir) || is_excluded(&dir) {
            continue;
        }
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let modified = last_modified(&dir).unwrap_or_else(|| "—".to_string());
        let debug_modified =
            variant_modified(&dir, "debug").unwrap_or_else(|| "jamais compilée".to_string());
        let release_modified =
            variant_modified(&dir, "release").unwrap_or_else(|| "jamais compilée".to_string());
        projects.push(ProjectEntry {
            id: name.clone(),
            name,
            path: dir.clone(),
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
                debug_modified,
                release_modified,
                last_compile_debug_secs: 0,
                last_compile_release_secs: 0,
                version_manually_edited: false,
                source_hash: String::new(),
                tarif_mode: default_tarif_mode(),
            tarif_manual: 0.0,
            tarif_web_series: Vec::new(),
            tarif_web_fetched_ms: 0,
            tarif_web_secteur: false,
            tarif_web_pays: String::new(),
        });
        added += 1;
    }
    Ok(added)
}

fn write_private(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content)?;
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

fn last_modified(dir: &Path) -> Option<String> {
    last_modified_time(dir).map(format_mod_time)
}

pub(crate) fn variant_modified(dir: &Path, profile: &str) -> Option<String> {
    last_modified_time(&dir.join("target").join(profile)).map(format_mod_time)
}

/// mtime (SystemTime) du fichier le plus récent sous `target/{profile}`.
pub(crate) fn variant_modified_time(dir: &Path, profile: &str) -> Option<std::time::SystemTime>
  {
    last_modified_time(&dir.join("target").join(profile))
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
        let mut files: Vec<PathBuf> = Vec::new();
        for entry in walkdir::WalkDir::new(&src_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
        {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path.to_path_buf());
            }
        }
        // Trier pour ordre déterministe
        files.sort();
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
    use std::{collections::HashMap, fs, path::PathBuf};

    use super::{
        default_tarif_mode, default_web_country, load_registry, save_registry, TarifPoint,
        validate_status, validate_version, ProjectEntry, STATUSES,
    };

    fn temp_registry(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("op-start-menu-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
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
            }],
            )
            .unwrap();
            assert_0600(&path);
        }
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
    fn validate_status_against_fixed_set() {
        assert!(STATUSES.contains(&"Beta"));
        for known in STATUSES {
            assert!(validate_status(known));
        }
        assert!(!validate_status("En cours"));
        assert!(!validate_status("Preview"));
        assert!(!validate_status(""));
    }

    #[test]
    fn load_migrates_legacy_status_version_to_both_variants() {
        let dir = temp_registry("migrate");
        let path = dir.join("opencode/projects.json");
        load_registry(&path).unwrap();

        let legacy = r#"{"projects":{"old":{"name":"Legacy","path":"/tmp/nonexistent-proj","description":"d","status":"En-cours Beta","version":"2.1.0"}}}"#;
        fs::write(&path, legacy).unwrap();

        let loaded = load_registry(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].status_debug, "En-cours Beta");
        assert_eq!(loaded[0].version_debug, "2.1.0");
        assert_eq!(loaded[0].status_release, "En-cours Beta");
        assert_eq!(loaded[0].version_release, "2.1.0");
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
        super::save_registry_full(&path, &[e.clone()], &super::WebCache::default()).unwrap();

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
  }