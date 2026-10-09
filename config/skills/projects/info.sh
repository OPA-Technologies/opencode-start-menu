#!/bin/bash
# Wrapper: project info
if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <id>"
    exit 1
fi
exec "$HOME/bin/opencode-projects.sh" info "$1"