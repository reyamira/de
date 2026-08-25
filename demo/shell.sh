#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
export PATH="$repo_root/target/release:$PATH"
export XDG_CONFIG_HOME=/tmp/de-readme-demo/config
export HOME=/tmp/de-readme-demo
export PS1=$'\e[38;5;81mde-demo\e[0m \w $ '

eval "$(command de init bash)"
export -f de

cd /tmp/de-readme-demo/workspace
exec bash --noprofile --norc -i
