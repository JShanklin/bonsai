#!/bin/sh
# Regenerate every board template's generated files (src/bonsai.rs,
# src/links.rs, …) with this checkout's `bonsai sync`. Run it after changing
# templates/_tree/bonsai.rs, the generator (src/graph.rs), or a template's
# bonsai.toml or src/messages.rs. `--check` fails if anything was stale.
set -eu
cd "$(dirname "$0")/.."
cargo build --quiet
for board in templates/linux/*/; do
    (cd "$board" && ../../../target/debug/bonsai sync >/dev/null)
done
if [ "${1:-}" = "--check" ]; then
    git diff --exit-code -- templates/linux || {
        echo "board templates were stale: run scripts/sync-templates.sh and commit" >&2
        exit 1
    }
fi
