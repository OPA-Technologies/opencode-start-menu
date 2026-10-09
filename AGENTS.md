# Règles de travail (source unique)

Ce fichier est la source unique des règles globales d'assistance. Il est
déployé par `build/setup-config.sh` vers
`~/.config/opencode/instructions.md`, relu à chaque session opencode (voir
`opencode.jsonc` → `instructions`). Ne modifiez jamais le fichier déployé à la
main : toute retouche ici est répercutée au déploiement, et l'inverse serait
écrasé.

## Langue

Toujours répondre en français. Toutes les conversations et interactions
doivent être en français.

## Norme d'accolade (à appliquer à tous les projets, actuels et futurs)

Pour toute fonction et tout groupe d'instructions, l'accolade ouvrante `{` est
placée sur une ligne séparée, indentée de 2 caractères par rapport au groupe.
Le contenu du bloc est indenté de 2 caractères supplémentaires par rapport au
`{` (soit +4 par rapport à la ligne du groupe). L'accolade fermante `}` est
alignée sur le `{`. Chaque niveau d'imbrication ajoute 2 caractères.

Exemple de référence (validé) :

```bash
purge_mensuelle()
  {
    local mois="$1"
    cp "$maillog" "$maillog.old"
  }
```

## Gestion des projets

Un registre de projets est disponible. Il permet de lister, naviguer et gérer
les différents projets opencode.

### Liste des projets

Lors du démarrage, la liste des projets vous est affichée. Vous pouvez aussi la
demander à tout moment avec le skill `projects`.

### Commandes

- Liste des projets : utiliser le skill `projects`
- Ouvrir un projet : `cd <chemin> && opencode`
- Ajouter un projet : `opencode-projects.sh add <id> <nom> <chemin> <description>`
- Supprimer : `opencode-projects.sh remove <id>`

### Ressources

L'utilisateur autorise l'usage maximum des ressources (CPU et mémoire) pour les
calculs de l'assistant, pour cette session et pour toutes les
sessions/projets, maintenant et à l'avenir. Ne pas hésiter à pousser
longuement les raisonnements ou exécutions quand c'est utile. Le contexte est
purgé automatiquement (compaction activée dans `opencode.jsonc`) pour rester
rapide.

### Registre

Le fichier de registre est : `~/.config/opencode/projects.json`
Les projets sont stockés dans : `~/projets/`