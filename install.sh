#!/usr/bin/env bash
# Install the `cr` binary and the /cr-register Claude skill.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

cargo install --path "$here" --locked --quiet
echo "installed: $(command -v cr || echo ~/.cargo/bin/cr)"

skills="${CLAUDE_SKILLS_DIR:-$HOME/.claude/skills}"
mkdir -p "$skills/cr-register"
cp "$here/skill/cr-register/SKILL.md" "$skills/cr-register/SKILL.md"
echo "installed: $skills/cr-register/SKILL.md"
