#!/usr/bin/env bash
# Install the `cr` binary; the Claude skill ships as the plugin in plugin/.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"

cargo install --path "$here" --locked --quiet
echo "installed: $(command -v cr || echo ~/.cargo/bin/cr)"

echo "Claude plugin: /plugin install cr --marketplace shhra/command-registry"
