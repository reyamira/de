#!/usr/bin/env bash
set -euo pipefail

demo_root=/tmp/de-readme-demo

case "${1:-}" in
  setup)
    rm -rf -- "$demo_root"
    mkdir -p \
      "$demo_root/config/de" \
      "$demo_root/workspace/api/src" \
      "$demo_root/workspace/api/tests" \
      "$demo_root/workspace/docs/guides" \
      "$demo_root/workspace/frontend/components" \
      "$demo_root/workspace/frontend/pages"
    touch \
      "$demo_root/workspace/api/Cargo.toml" \
      "$demo_root/workspace/api/README.md" \
      "$demo_root/workspace/api/src/main.rs" \
      "$demo_root/workspace/api/tests/health.rs" \
      "$demo_root/workspace/docs/guides/getting-started.md" \
      "$demo_root/workspace/frontend/package.json"
    printf '%s\n' \
      '[theme]' \
      'selected = "ocean"' \
      '' \
      '[display]' \
      'modified = false' \
      > "$demo_root/config/de/config.toml"
    ;;
  cleanup)
    rm -rf -- "$demo_root"
    ;;
  *)
    printf 'usage: %s setup|cleanup\n' "$0" >&2
    exit 2
    ;;
esac
