#!/bin/bash
# Wrapper: add a project
if [[ $# -lt 4 ]]; then
    echo "Usage: $0 <id> <nom> <chemin> <description>"
    exit 1
fi
exec "$HOME/bin/opencode-projects.sh" add "$1" "$2" "$3" "$4"