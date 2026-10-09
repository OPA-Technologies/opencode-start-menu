#!/bin/bash
# Wrapper: remove a project
if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <id>"
    exit 1
fi
exec "$HOME/bin/opencode-projects.sh" remove "$1"