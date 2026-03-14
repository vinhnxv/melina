#!/usr/bin/env bash
# Session-End Hook for Melina Auto-Cleanup
#
# This hook runs when Claude Code sessions end and cleans up zombie processes.
# It ONLY kills dead-owner zombies — NOT live teams. Safe by design.
#
# Installation:
#   cp session-end.sh ~/.claude/hooks/session-end.sh
#   chmod +x ~/.claude/hooks/session-end.sh
#
# Or add to existing session-end.sh:
#   source /path/to/melina/hooks/session-end.sh

set -euo pipefail

# Check if melina-cli is available
if ! command -v melina-cli &>/dev/null; then
    # Silently skip if not installed
    exit 0
fi

# Run zombie cleanup (suppress all output)
# This only kills teams where the owner process is dead
melina-cli --kill-zombies 2>/dev/null || true