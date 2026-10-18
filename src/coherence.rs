//! Contrôle de cohérence des projets.
//!
//! À chaque lancement et à chaque retour au menu, op compare ce que chaque
//! projet déclare (sources, binaire Debug, binaire Release) à ce que la base
//! — le registre — enregistre. Les écarts sont exposés à l'utilisateur, qui
//! choisit l'action à mener, ou la refuse ; un refus est conservé et
//! re-notifié tant que l'écart reste identique.
//!
//! La détection ne fait pas de choix à la place de l'utilisateur : elle
//! décrit les écarts et propose les actions qui les résorberaient. Seule
//! l'action « mettre à jour la base » est exécutée par op ; les reconstructions
//! et la conformité du code sont proposées mais relèvent d'une phase
//! ultérieure.
//!
//! Le module est scindé en deux : un moteur **pur** (`diagnostiquer`), qui ne
//! dépend que d'observations déjà rassemblées et se teste sans disque ni
//! binaire, et un rassembleur (`observer`) qui interroge le disque et les
//! binaires pour produire ces observations.

use crate::registry::{self, ProjectEntry};
use crate::variante::EtatVariante;

/// Variante binaire d'un projet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variante
  {
    Debug,
    Release,
  }

impl Variante
  {
    /// Profil Cargo (`debug`/`release`), aussi nom de dossier sous `target/`.
    pub(crate) fn profil(self) -> &'static str
      {
        match self
          {
            Variante::Debug => "debug",
            Variante::Release => "release",
          }
      }

    /// Libellé affiché.
    pub(crate) fn libelle(self) -> &'static str
      {
        match self
          {
            Variante::Debug => "Debug",
            Variante::Release => "Release",
          }
      }
  }

/// Un écart de cohérence détecté pour un projet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ecart
  {
    /// Le code des sources a changé depuis le dernier build Debug : le binaire
    /// Debug est en retard sur les sources.
    SourcesAvantDebug,
    /// Le binaire Debug déclare autre chose que ce que la base enregistre.
    DebugDivergeDeBase { base: String, binaire: String },
    /// La version Debug enregistrée ne suit pas la version des sources
    /// (manifeste) : la base a divergé du code.
    BaseDivergeDesSources { base: String, sources: String },
    /// Les métadonnées Debug et Release enregistrées diffèrent — normal tant
    /// que la Release n'a pas été reconstruite.
    DebugDivergeDeRelease { debug: String, release: String },
    /// Un binaire présent ne publie pas le contrat `-V` (version, statut, date).
    BinaireHorsContrat { variante: Variante },
    /// Un statut déclaré n'appartient pas à la liste connue d'op.
    StatutHorsListe { variante: Variante, statut: String },
  }

impl Ecart
  {
    /// Clé stable : identifie l'écart tant qu'il reste identique. Les refus
    /// s'y rattachent, de sorte qu'un écart qui change n'est plus « refusé ».
    pub(crate) fn cle(&self) -> String
      {
        match self
          {
            Ecart::SourcesAvantDebug => "sources>debug".to_string(),
            Ecart::DebugDivergeDeBase { base, binaire } =>
              format!("debug>base:{base}->{binaire}"),
            Ecart::BaseDivergeDesSources { base, sources } =>
              format!("base>sources:{base}->{sources}"),
            Ecart::DebugDivergeDeRelease { debug, release } =>
              format!("debug<>release:{debug}->{release}"),
            Ecart::BinaireHorsContrat { variante } =>
              format!("hors-contrat:{}", variante.profil()),
            Ecart::StatutHorsListe { variante, statut } =>
              format!("statut:{}:{statut}", variante.profil()),
          }
      }

    /// Action qui résorberait cet écart.
    pub(crate) fn action(&self) -> Action
      {
        match self
          {
            Ecart::SourcesAvantDebug => Action::RebuildDebug,
            Ecart::DebugDivergeDeBase { .. } => Action::MajBase,
            Ecart::BaseDivergeDesSources { .. } => Action::MajBase,
            Ecart::DebugDivergeDeRelease { .. } => Action::RebuildRelease,
            Ecart::BinaireHorsContrat { .. } => Action::ConformerCode,
            Ecart::StatutHorsListe { .. } => Action::CorrigerStatut,
          }
      }

    /// Description lisible de l'écart.
    pub(crate) fn description(&self) -> String
      {
        match self
          {
            Ecart::SourcesAvantDebug =>
              "les sources sont plus récentes que le binaire Debug".to_string(),
            Ecart::DebugDivergeDeBase { base, binaire } =>
              format!("le binaire Debug ({binaire}) diffère de la base ({base})"),
            Ecart::BaseDivergeDesSources { base, sources } =>
              format!("la base ({base}) ne suit pas les sources ({sources})"),
            Ecart::DebugDivergeDeRelease { debug, release } =>
              format!("Debug ({debug}) et Release ({release}) diffèrent"),
            Ecart::BinaireHorsContrat { variante } =>
              format!("le binaire {} ne publie pas le contrat -V", variante.libelle()),
            Ecart::StatutHorsListe { variante, statut } =>
              format!("statut {} inconnu (« {statut} »)", variante.libelle()),
          }
      }
  }

/// Action proposée pour résorber un écart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action
  {
    /// Aligner la base (registre) sur la vérité du disque.
    MajBase,
    /// Reconstruire la variante Debug.
    RebuildDebug,
    /// Reconstruire la variante Release (confirmation dédiée exigée).
    RebuildRelease,
    /// Conformer le module principal au contrat `-V`.
    ConformerCode,
    /// Corriger un statut hors de la liste connue.
    CorrigerStatut,
  }

impl Action
  {
    /// Libellé affiché.
    pub(crate) fn libelle(self) -> &'static str
      {
        match self
          {
            Action::MajBase => "mettre à jour la base",
            Action::RebuildDebug => "reconstruire Debug",
            Action::RebuildRelease => "reconstruire Release",
            Action::ConformerCode => "conformer le code au contrat -V",
            Action::CorrigerStatut => "corriger le statut",
          }
      }

    /// Code stable, utilisé pour persister un refus.
    pub(crate) fn code(self) -> &'static str
      {
        match self
          {
            Action::MajBase => "maj-base",
            Action::RebuildDebug => "rebuild-debug",
            Action::RebuildRelease => "rebuild-release",
            Action::ConformerCode => "conformer-code",
            Action::CorrigerStatut => "corriger-statut",
          }
      }

    /// Toute action touchant la Release exige une confirmation dédiée.
    pub(crate) fn touche_release(self) -> bool
      {
        matches!(self, Action::RebuildRelease)
      }
  }

/// Faits observés pour un projet, indépendants du registre et déjà rassemblés.
///
/// Le moteur pur ([`diagnostiquer`]) ne lit que ce bloc : c'est ce qui le rend
/// testable sans disque ni binaire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Observations
  {
    /// Le projet contient-il du code (un manifeste) ? Sans code, il n'a pas de
    /// contrat `-V` : aucun écart n'est examiné.
    pub(crate) a_du_code: bool,
    /// Version du manifeste des sources (ce que le code déclare).
    pub(crate) sources_version: String,
    /// Hash des sources sur disque et hash enregistré dans la base.
    pub(crate) sources_hash: String,
    pub(crate) base_hash: String,
    /// Horodatages (epoch secondes) des sources et des binaires.
    pub(crate) sources_mtime: Option<u64>,
    pub(crate) debug_mtime: Option<u64>,
    pub(crate) release_mtime: Option<u64>,
    /// État `-V` des binaires (issu de la fiche quand elle est cohérente, d'une
    /// sonde sinon). `None` = binaire absent ou hors contrat.
    pub(crate) debug: Option<EtatVariante>,
    pub(crate) release: Option<EtatVariante>,
    /// Base enregistrée.
    pub(crate) base_version: String,
    pub(crate) base_version_debug: String,
    pub(crate) base_status_debug: String,
    pub(crate) base_version_release: String,
    pub(crate) base_status_release: String,
  }

/// Diagnostic de cohérence d'un projet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Diagnostic
  {
    pub(crate) ecarts: Vec<Ecart>,
  }

impl Diagnostic
  {
    /// Aucun écart : le projet est cohérent.
    pub(crate) fn est_vide(&self) -> bool
      {
        self.ecarts.is_empty()
      }

    /// Actions proposées, sans doublon, dans l'ordre des écarts.
    pub(crate) fn actions(&self) -> Vec<Action>
      {
        let mut actions: Vec<Action> = Vec::new();
        for e in &self.ecarts
          {
            let a = e.action();
            if !actions.contains(&a)
              {
                actions.push(a);
              }
          }
        actions
      }
  }

/// Étiquette `version_statut` d'un état `-V`, pour l'affichage des écarts.
fn etiquette(etat: &EtatVariante) -> String
  {
    if etat.statut.is_empty()
      {
        etat.version.clone()
      }
    else
      {
        format!("{}_{}", etat.version, etat.statut)
      }
  }

/// Moteur pur : déduit les écarts d'un projet à partir des faits observés.
pub(crate) fn diagnostiquer(obs: &Observations) -> Diagnostic
  {
    let mut ecarts: Vec<Ecart> = Vec::new();
    if !obs.a_du_code
      {
        return Diagnostic { ecarts };
      }

    // Binaires présents mais muets au contrat `-V`.
    for (variante, mtime, etat) in [
        (Variante::Debug, obs.debug_mtime, &obs.debug),
        (Variante::Release, obs.release_mtime, &obs.release),
      ]
      {
        if mtime.is_some() && etat.is_none()
          {
            ecarts.push(Ecart::BinaireHorsContrat { variante });
          }
      }

    // Statuts déclarés hors de la liste connue.
    for (variante, etat) in [
        (Variante::Debug, &obs.debug),
        (Variante::Release, &obs.release),
      ]
      {
        if let Some(e) = etat
            && !e.statut.is_empty()
            && !registry::validate_status(&e.statut)
          {
            ecarts.push(Ecart::StatutHorsListe {
                variante,
                statut: e.statut.clone(),
            });
          }
      }

    // Sources en avance sur le binaire Debug : hash ou mtime. Un binaire plus
    // ancien que les sources est la marque d'une reconstruction à faire — et
    // non d'une base à réaligner.
    let hash_avance = !obs.sources_hash.is_empty() && obs.sources_hash != obs.base_hash;
    let mtime_avance = match (obs.sources_mtime, obs.debug_mtime)
      {
        (Some(s), Some(d)) => s > d,
        _ => false,
      };
    let debug_en_retard = hash_avance || mtime_avance;
    if debug_en_retard
      {
        ecarts.push(Ecart::SourcesAvantDebug);
      }

    // Binaire Debug différent de la base. On ne le signale pas si le binaire
    // est simplement en retard (l'écart « sources avant Debug » couvre ce cas,
    // et la base peut légitimement être en avance d'un bump non reconstruit).
    if !debug_en_retard
        && let Some(d) = &obs.debug
        && !d.version.is_empty()
        && (obs.base_version_debug != d.version || obs.base_status_debug != d.statut)
      {
        ecarts.push(Ecart::DebugDivergeDeBase {
            base: etiquette(&EtatVariante {
                version: obs.base_version_debug.clone(),
                statut: obs.base_status_debug.clone(),
                date: String::new(),
            }),
            binaire: etiquette(d),
        });
      }

    // La base enregistrée ne suit pas le manifeste des sources.
    if !obs.sources_version.is_empty()
        && !obs.base_version_debug.is_empty()
        && obs.sources_version != obs.base_version_debug
      {
        ecarts.push(Ecart::BaseDivergeDesSources {
            base: obs.base_version_debug.clone(),
            sources: obs.sources_version.clone(),
        });
      }

    // Debug et Release enregistrés diffèrent : normal, mais exposé.
    if !obs.base_version_debug.is_empty()
        && !obs.base_version_release.is_empty()
        && (obs.base_version_debug != obs.base_version_release
          || obs.base_status_debug != obs.base_status_release)
      {
        ecarts.push(Ecart::DebugDivergeDeRelease {
            debug: format!("{}_{}", obs.base_version_debug, obs.base_status_debug),
            release: format!("{}_{}", obs.base_version_release, obs.base_status_release),
        });
      }

    Diagnostic { ecarts }
  }

/// Rassemble les faits d'un projet : sources, binaires, base.
///
/// Pour chaque variante, l'état est lu depuis la fiche quand son horodatage
/// correspond au binaire présent — la fiche est alors la preuve du build —, et
/// par sonde `-V` sinon. Ce choix évite de relancer un binaire inchangé à
/// chaque contrôle.
pub(crate) fn observer(entry: &ProjectEntry) -> Observations
  {
    let a_du_code = entry.path.join("Cargo.toml").is_file();
    if !a_du_code
      {
        return Observations::default();
      }
    let secs = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
    };
    let debug_mtime = registry::variant_modified_time(&entry.path, "debug").and_then(secs);
    let release_mtime = registry::variant_modified_time(&entry.path, "release").and_then(secs);

    // État d'une variante : la fiche fait foi si elle décrit le binaire présent
    // (mtime égal) ; sinon on interroge le binaire.
    let etat = |variante: Variante, mtime: Option<u64>, fiche: &Option<registry::SnapshotCompile>| {
        match fiche
          {
            Some(f) if Some(f.compile_secs) == mtime && !f.version.is_empty() =>
              {
                Some(EtatVariante {
                    version: f.version.clone(),
                    statut: f.statut.clone(),
                    date: f.date.clone(),
                })
              }
            _ =>
              {
                crate::variante::lire_variante_avec_sonde(
                  &entry.path,
                  variante.profil(),
                  entry.sonde(variante.profil()),
                )
                .map(|l| l.etat)
              }
          }
    };
    let debug = etat(Variante::Debug, debug_mtime, &entry.debug_snapshot);
    let release = etat(Variante::Release, release_mtime, &entry.release_snapshot);

    let sources_mtime = registry::source_modified_time(&entry.path).and_then(secs);
    Observations {
        a_du_code,
        sources_version: entry.version.clone(),
        sources_hash: registry::compute_source_hash(&entry.path),
        base_hash: entry.source_hash.clone(),
        sources_mtime,
        debug_mtime,
        release_mtime,
        debug,
        release,
        base_version: entry.version.clone(),
        base_version_debug: entry.version_debug.clone(),
        base_status_debug: entry.status_debug.clone(),
        base_version_release: entry.version_release.clone(),
        base_status_release: entry.status_release.clone(),
    }
  }

#[cfg(test)]
mod tests
  {
    use super::*;

    fn obs_projet_coherent() -> Observations
      {
        Observations {
            a_du_code: true,
            sources_version: "1.0.0".to_string(),
            sources_hash: "abc".to_string(),
            base_hash: "abc".to_string(),
            sources_mtime: Some(100),
            debug_mtime: Some(200),
            release_mtime: Some(200),
            debug: Some(EtatVariante {
                version: "1.0.0".to_string(),
                statut: "Release".to_string(),
                date: "14-10-2026".to_string(),
            }),
            release: Some(EtatVariante {
                version: "1.0.0".to_string(),
                statut: "Release".to_string(),
                date: "14-10-2026".to_string(),
            }),
            base_version: "1.0.0".to_string(),
            base_version_debug: "1.0.0".to_string(),
            base_status_debug: "Release".to_string(),
            base_version_release: "1.0.0".to_string(),
            base_status_release: "Release".to_string(),
        }
      }

    #[test]
    fn un_projet_coherent_n_a_aucun_ecart()
      {
        let d = diagnostiquer(&obs_projet_coherent());
        assert!(d.est_vide(), "écarts inattendus : {:?}", d.ecarts);
      }

    #[test]
    fn un_projet_sans_code_n_est_pas_examine()
      {
        let o = Observations { a_du_code: false, ..obs_projet_coherent() };
        assert!(diagnostiquer(&o).est_vide());
      }

    #[test]
    fn des_sources_plus_recentes_que_debug_demandent_une_reconstruction()
      {
        let o = Observations { sources_mtime: Some(300), ..obs_projet_coherent() };
        let d = diagnostiquer(&o);
        assert_eq!(d.ecarts, vec![Ecart::SourcesAvantDebug]);
        assert_eq!(d.actions(), vec![Action::RebuildDebug]);
      }

    #[test]
    fn un_hash_de_sources_different_trahit_une_source_plus_recente()
      {
        let o = Observations { base_hash: "ancien".to_string(), ..obs_projet_coherent() };
        let d = diagnostiquer(&o);
        assert!(d.ecarts.contains(&Ecart::SourcesAvantDebug));
      }

    /// Une base en avance d'un bump non reconstruit (binaire plus ancien) ne
    /// doit PAS être réalignée : c'est une reconstruction qu'il faut proposer.
    #[test]
    fn une_base_en_avance_avec_binaire_plus_vieux_demande_une_reconstruction()
      {
        let o = Observations {
            sources_mtime: Some(300),
            sources_version: "1.0.1".to_string(),
            base_version_debug: "1.0.1".to_string(),
            base_version: "1.0.1".to_string(),
            debug: Some(EtatVariante {
                version: "1.0.0".to_string(),
                statut: "Release".to_string(),
                date: String::new(),
            }),
            ..obs_projet_coherent()
        };
        let d = diagnostiquer(&o);
        assert!(d.ecarts.contains(&Ecart::SourcesAvantDebug));
        assert!(
          !d.actions().contains(&Action::MajBase),
          "aucun réalignement de base attendu : {:?}",
          d.ecarts
        );
      }

    #[test]
    fn un_binaire_debug_different_de_la_base_propose_de_realigner()
      {
        let o = Observations {
            base_version_debug: "9.9.9".to_string(),
            ..obs_projet_coherent()
        };
        let d = diagnostiquer(&o);
        assert!(d.ecarts.iter().any(|e| matches!(e, Ecart::DebugDivergeDeBase { .. })));
        assert!(d.actions().contains(&Action::MajBase));
      }

    #[test]
    fn une_base_qui_ne_suit_pas_les_sources_est_signalee()
      {
        let o = Observations {
            base_version_debug: "1.0.1".to_string(),
            base_version: "1.0.1".to_string(),
            sources_version: "1.0.0".to_string(),
            // Binaire Debug aligné sur les sources : pas de retard.
            debug: Some(EtatVariante {
                version: "1.0.0".to_string(),
                statut: "Release".to_string(),
                date: String::new(),
            }),
            ..obs_projet_coherent()
        };
        let d = diagnostiquer(&o);
        assert!(d.ecarts.iter().any(|e| matches!(e, Ecart::BaseDivergeDesSources { .. })));
      }

    #[test]
    fn debug_et_release_differents_proposent_une_reconstruction_release()
      {
        let o = Observations {
            base_version_release: "0.9.0".to_string(),
            release: Some(EtatVariante {
                version: "0.9.0".to_string(),
                statut: "Release".to_string(),
                date: String::new(),
            }),
            ..obs_projet_coherent()
        };
        let d = diagnostiquer(&o);
        let a = d.actions();
        assert!(a.contains(&Action::RebuildRelease));
        assert!(Action::RebuildRelease.touche_release());
        assert!(a.iter().any(|x| x.touche_release()));
      }

    #[test]
    fn un_binaire_muet_au_contrat_est_signale()
      {
        let o = Observations {
            debug: None,
            ..obs_projet_coherent()
        };
        let d = diagnostiquer(&o);
        assert!(d.ecarts.contains(&Ecart::BinaireHorsContrat { variante: Variante::Debug }));
        assert!(d.actions().contains(&Action::ConformerCode));
      }

    #[test]
    fn un_statut_hors_liste_est_signale()
      {
        let o = Observations {
            debug: Some(EtatVariante {
                version: "1.0.0".to_string(),
                statut: "Zorglub".to_string(),
                date: String::new(),
            }),
            base_status_debug: "Zorglub".to_string(),
            ..obs_projet_coherent()
        };
        let d = diagnostiquer(&o);
        assert!(d.ecarts.contains(&Ecart::StatutHorsListe {
            variante: Variante::Debug,
            statut: "Zorglub".to_string(),
        }));
        assert!(d.actions().contains(&Action::CorrigerStatut));
      }

    #[test]
    fn les_actions_sont_dedoublonnees_dans_l_ordre()
      {
        // Deux écarts proposant la même action (maj base).
        let o = Observations {
            base_version_debug: "9.9.9".to_string(),
            sources_version: "1.0.0".to_string(),
            ..obs_projet_coherent()
        };
        let d = diagnostiquer(&o);
        let actions = d.actions();
        let nb_maj = actions.iter().filter(|a| **a == Action::MajBase).count();
        assert_eq!(nb_maj, 1, "action dédoublonnée");
      }

    #[test]
    fn la_cle_d_un_ecart_change_avec_ses_valeurs()
      {
        let a = Ecart::DebugDivergeDeBase {
            base: "1.0.0".to_string(),
            binaire: "1.0.1".to_string(),
        };
        let b = Ecart::DebugDivergeDeBase {
            base: "1.0.0".to_string(),
            binaire: "1.0.2".to_string(),
        };
        assert_ne!(a.cle(), b.cle());
        assert_eq!(a.cle(), a.clone().cle(), "clé stable");
      }

    #[test]
    fn chaque_ecart_a_une_description_et_une_action()
      {
        let ecarts = vec![
            Ecart::SourcesAvantDebug,
            Ecart::DebugDivergeDeBase {
                base: "1.0.0_Release".to_string(),
                binaire: "1.0.1_Release".to_string(),
            },
            Ecart::BaseDivergeDesSources {
                base: "1.0.1".to_string(),
                sources: "1.0.0".to_string(),
            },
            Ecart::DebugDivergeDeRelease {
                debug: "1.0.0_Release".to_string(),
                release: "0.9.0_Release".to_string(),
            },
            Ecart::BinaireHorsContrat { variante: Variante::Release },
            Ecart::StatutHorsListe {
                variante: Variante::Debug,
                statut: "Zorglub".to_string(),
            },
        ];
        for e in ecarts
          {
            assert!(!e.description().is_empty(), "description manquante : {e:?}");
            assert!(!e.cle().is_empty(), "clé manquante : {e:?}");
            let _ = e.action();
          }
      }

    #[test]
    fn chaque_action_a_un_libelle()
      {
        for a in [
            Action::MajBase,
            Action::RebuildDebug,
            Action::RebuildRelease,
            Action::ConformerCode,
            Action::CorrigerStatut,
          ]
          {
            assert!(!a.libelle().is_empty());
          }
      }
  }
