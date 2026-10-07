//! Source de vérité unique des raccourcis clavier.
//!
//! L'aide était jusqu'ici une suite de chaînes écrites à la main dans
//! `render_help`. Elles ont divergé du code : la barre annonçait `e Modifier`
//! et `t Valorisation`, deux touches qui n'ont jamais existé, pendant que les
//! touches réelles (`p`, `s`, `m`, `v`, `c`) n'y figuraient pas. Un aide
//! mentant est pire qu'aucune aide : l'utilisateur cherche une touche absente
//! et conclut que le programme est cassé.
//!
//! Tout part donc d'ici. La barre contextuelle et l'écran `F10` sont deux
//! rendus du même tableau : ils ne peuvent plus diverger, et un test compare
//! la barre affichée aux raccourcis déclarés ici.

use std::borrow::Cow;

use crate::InputMode;

/// La déclaration et son contrat vivent dans le crate partagé : ce fichier ne
/// décrit que *ce que* ce projet sait faire, pas comment l'afficher.
pub(crate) use op_tui::keymap::{raccourci, Cible, Raccourci};

/// L'écran courant, vu par l'aide partagée.
///
/// `None` quand il n'y a pas d'écran d'origine à décrire : l'aide displays
/// alors une liste vide plutôt que celle d'un mode par défaut, qui
/// annoncerait des gestes sans rapport avec ce qui est affiché.
pub(crate) struct Aide<'a>(pub(crate) Option<&'a InputMode>);

impl Cible for Aide<'_>
  {
    fn contextuel(&self) -> Vec<Raccourci>
      {
        self.0.map_or_else(Vec::new, contextuel)
      }

    /// L'aide complète ne dépend pas du mode : elle les liste tous.
    fn complet(&self) -> Vec<(Cow<'static, str>, Vec<Raccourci>)>
      {
        complet()
      }
  }

/// Raccourcis de la barre d'aide contextuelle, pour le mode courant.
///
/// Les libellés sont dans l'ordre d'importance : la barre est une seule ligne,
/// donc ce qui compte le plus doit passer en premier.
pub(crate) fn contextuel(mode: &InputMode) -> Vec<Raccourci> {
    match mode {
        // Libellés courts et ordre d'importance : la barre est une seule ligne,
        // ce qui compte le plus doit passer en premier, et ce qui ne tient pas
        // reste accessible par `F1`.
        InputMode::Normal => vec![
            raccourci("↑/↓", "Naviguer"),
            raccourci("Entrée", "Ouvrir"),
            raccourci("p", "Réglages"),
            raccourci("b", "Bump"),
            raccourci("a", "Archiver"),
            raccourci("A", "Archives"),
            raccourci("m", "Module"),
            raccourci("n", "Nouveau"),
            raccourci("r", "Rafraîchir"),
            raccourci("F1", "Aide"),
        ],
        InputMode::SelectModule { .. } => vec![
            raccourci("↑/↓", "Choisir un module"),
            raccourci("Entrée", "Ouvrir"),
            raccourci("Échap", "Annuler"),
        ],
        InputMode::CreatingProject(state) => {
            match state.active_field {
                // Le statut ouvre une liste : le filtrage prend la place du
                // déplacement de curseur, sinon le texte tapé irait au champ
                // précédent.
                4 if state.status_menu_open => vec![
                    raccourci("↑/↓", "Choisir un statut"),
                    raccourci("Entrée", "Valider"),
                    raccourci("Échap", "Fermer la liste"),
                    raccourci("Frappe", "Filtrer"),
                ],
                4 => vec![
                    raccourci("↑/↓", "Changer de champ"),
                    raccourci("Entrée", "Ouvrir la liste des statuts"),
                    raccourci("Frappe", "Filtrer"),
                ],
                _ => vec![
                    raccourci("↑/↓", "Changer de champ"),
                    raccourci("←/→", "Déplacer le curseur"),
                    raccourci("Entrée", "Suivant"),
                    raccourci("Frappe", "Saisir"),
                ],
            }
        }
        InputMode::Popup(popup) => popup_contextuel(popup),
        // L'aide est écran plein : les seuls gestes qui y répondent sont ceux
        // qui ferment ou qui changent de vue.
        InputMode::Aide { .. } => vec![
            raccourci("F1", "Aide contextuelle"),
            raccourci("F10", "Tous les raccourcis"),
            raccourci("Échap", "Fermer"),
        ],
    }
}

/// Raccourcis du popup, selon l'onglet actif.
///
/// La liste dépend de l'onglet : `←`/`→` ne veulent pas dire la même chose
/// partout, et le champ actif change les gestes disponibles.
fn popup_contextuel(popup: &crate::Popup) -> Vec<Raccourci> {
    // La navigation d'onglet est commune : Tab est global, et le dire ici
    // évite de le répéter trois fois sans pouvoir le rater.
    let navigation = vec![
        raccourci("Tab/⇧Tab", "Onglet suivant/précédent"),
        raccourci("F1", "Aide contextuelle"),
        raccourci("Échap", "Fermer"),
    ];
    let mut propres = match popup.onglet {
        crate::Onglet::Projet => {
            if popup.projet.editing {
                vec![
                    raccourci("←/→", "Déplacer le curseur"),
                    raccourci("Entrée", "Valider"),
                    raccourci("Échap", "Annuler la saisie"),
                ]
            } else {
                vec![
                    raccourci("↑/↓", "Changer de champ"),
                    raccourci("Entrée", "Éditer le champ"),
                ]
            }
        }
        crate::Onglet::Couts => {
            if popup.couts.editing {
                vec![
                    raccourci("←/→", "Déplacer le curseur"),
                    raccourci("Entrée", "Valider"),
                    raccourci("Échap", "Annuler la saisie"),
                ]
            } else {
                vec![
                    raccourci("↑/↓", "Défiler le tableau"),
                    raccourci("←/→", "Changer de champ tarifaire"),
                    raccourci("Entrée", "Basculer le mode ou éditer"),
                    raccourci("r", "Rafraîchir la source"),
                ]
            }
        }
        crate::Onglet::Parametres => {
            if popup.parametres.edition {
                vec![
                    raccourci("←/→", "Déplacer le curseur"),
                    raccourci("Entrée", "Valider le code pays"),
                    raccourci("Échap", "Annuler la saisie"),
                ]
            } else {
                vec![
                    raccourci("↑/↓", "Changer d'option"),
                    raccourci("←/→", "Parcourir les colonnes"),
                    raccourci("Entrée", "Basculer l'option"),
                ]
            }
        }
    };
    propres.extend(navigation);
    propres
}

/// Tous les raccourcis, groupés par écran : le contenu de `F10`.
///
/// `F1` montre `contextuel`, `F10` montre ceci : l'utilisateur qui a oublié
/// une touche la retrouve sans avoir à changer d'écran.
pub(crate) fn complet() -> Vec<(Cow<'static, str>, Vec<Raccourci>)> {
    vec![
        (
            Cow::Borrowed("Liste des projets"),
            vec![
                raccourci("↑/↓ ou j/k", "Naviguer"),
                raccourci("Entrée", "Ouvrir dans opencode"),
                raccourci("v", "Lancer une session à vide"),
                raccourci("m", "Ouvrir un sous-module"),
                raccourci("p", "Réglages (onglet Paramètres)"),
                raccourci("b", "Bumper la version"),
                raccourci("a", "Archiver le projet"),
                raccourci("A", "Vue des archives (a y restaure)"),
                raccourci("n", "Nouveau projet"),
                raccourci("r", "Rafraîchir les données"),
                raccourci("q ou Échap", "Quitter"),
            ],
        ),
        (
            Cow::Borrowed("Choix d'un module"),
            vec![
                raccourci("↑/↓ ou j/k", "Choisir un module"),
                raccourci("Entrée", "Ouvrir"),
                raccourci("Échap", "Annuler"),
            ],
        ),
        (
            Cow::Borrowed("Nouveau projet"),
            vec![
                raccourci("↑/↓", "Changer de champ"),
                raccourci("←/→", "Déplacer le curseur"),
                raccourci("Entrée", "Valider le champ et passer au suivant"),
                raccourci("Frappe", "Saisir le champ"),
            ],
        ),
        (
            Cow::Borrowed("Popup — onglet Projet"),
            vec![
                raccourci("↑/↓", "Changer de champ"),
                raccourci("Entrée", "Éditer le champ"),
                raccourci("←/→", "Déplacer le curseur en saisie"),
                raccourci("Tab/⇧Tab", "Onglet suivant/précédent"),
                raccourci("F1", "Aide contextuelle"),
                raccourci("Échap", "Fermer ou annuler la saisie"),
            ],
        ),
        (
            Cow::Borrowed("Popup — onglet Coûts"),
            vec![
                raccourci("↑/↓", "Défiler le tableau"),
                raccourci("←/→", "Changer de champ tarifaire"),
                raccourci("Entrée", "Basculer web/manuel ou éditer"),
                raccourci("r", "Rafraîchir la source"),
                raccourci("Tab/⇧Tab", "Onglet suivant/précédent"),
                raccourci("Échap", "Fermer ou annuler la saisie"),
            ],
        ),
        (
            Cow::Borrowed("Popup — onglet Paramètres"),
            vec![
                raccourci("↑/↓", "Changer d'option"),
                raccourci("←/→", "Parcourir les colonnes visibles"),
                raccourci("Entrée ou Espace", "Basculer l'option"),
                raccourci("Tab/⇧Tab", "Onglet suivant/précédent"),
                raccourci("Échap", "Fermer ou annuler la saisie"),
            ],
        ),
        (
            Cow::Borrowed("Aide"),
            vec![
                raccourci("F1", "Aide contextuelle"),
                raccourci("F10", "Tous les raccourcis"),
                raccourci("↑/↓", "Défiler la liste"),
                raccourci("Échap", "Fermer"),
            ],
        ),
    ]
}
