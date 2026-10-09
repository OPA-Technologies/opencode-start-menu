---
name: projects
description: "Liste et gère les projets opencode. Utilisez quand l'utilisateur demande de lister ses projets, ajouter/supprimer un projet, ou voir les détails d'un projet."
---

# Gestion des projets opencode

Ce skill permet de lister, ajouter, supprimer et naviguer entre les projets.

## Commandes disponibles

### Lister les projets
Lancez le script pour voir tous les projets enregistrés :
```bash
~/.config/opencode/skills/projects/list.sh
```

### Ajouter un projet
```bash
~/.config/opencode/skills/projects/add.sh <id> <nom> <chemin> <description>
```

### Supprimer un projet
```bash
~/.config/opencode/skills/projects/remove.sh <id>
```

### Voir les détails d'un projet
```bash
~/.config/opencode/skills/projects/info.sh <id>
```

## Format d'affichage

Chaque projet affiche :
- **Statut** : ● actif / ○ inactif
- **Nom** : Nom symbolique du projet
- **ID** : Identifiant unique (utilisé pour les commandes)
- **Chemin** : Emplacement sur le disque
- **Description** : Description courte du projet

## Exemples d'utilisation

```
Utilisateur: "liste mes projets"
→ Exécutez: ~/.config/opencode/skills/projects/list.sh

Utilisateur: "ajoute le projet backup-dsm"
→ Exécutez: ~/.config/opencode/skills/projects/add.sh backup-dsm "Backup DSM" ~/projets/backup-dsm "Sauvegarde automatique du DSM"
```