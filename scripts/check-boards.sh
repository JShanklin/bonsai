#!/bin/sh
# Render every board template (as cargo-generate would: its only variable is
# the project name), grow a representative tree in it with this checkout's
# CLI (branches, messages, links, a rate, and every kind of edge: udp, tcp
# server and client, serial, custom), and compile it for the board's own
# target with `cargo check`. The host board's tests run too.
#
# A compile check for a Pi target is not a hardware test: it shows the tree
# builds for that CPU, not that it runs there.
#
#   scripts/check-boards.sh [board ...]    # default: every board
set -eu
repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"
cargo build --quiet
bonsai="$repo/target/debug/bonsai"
work="$repo/target/board-check"
boards="${*:-$(ls templates/linux)}"
for board in $boards; do
    echo "== $board"
    tree="$work/$board"
    rm -rf "$tree"
    mkdir -p "$tree"
    cp -r "templates/linux/$board/." "$tree/"
    find "$tree" -type f -exec sed -i 's/{{project-name}}/board_check/g' {} +
    (
        cd "$tree"
        "$bonsai" branch add sensor >/dev/null
        "$bonsai" branch add watchdog >/dev/null
        "$bonsai" message add Reading temp:Celsius humidity:Percent >/dev/null
        "$bonsai" message add Alarm temp:Celsius >/dev/null
        "$bonsai" link sensor Reading watchdog >/dev/null
        "$bonsai" rate sensor 2 >/dev/null
        "$bonsai" edge add uplink udp --bind 0.0.0.0:6969 --to 127.0.0.1:6970 >/dev/null
        "$bonsai" edge add hub tcp --listen 0.0.0.0:7000 --framing lines --max-clients 8 >/dev/null
        "$bonsai" edge add station tcp --connect 127.0.0.1:7100 >/dev/null
        "$bonsai" edge add gps serial --device /dev/serial0 --baud 9600 --framing lines >/dev/null
        "$bonsai" edge add probe --custom >/dev/null
        for edge in uplink hub station gps probe; do
            "$bonsai" link "$edge" watchdog >/dev/null
            "$bonsai" link watchdog "$edge" >/dev/null
        done
        "$bonsai" list
        CARGO_TARGET_DIR="$work/target" cargo check --quiet --all-targets
        if [ "$board" = host ]; then
            CARGO_TARGET_DIR="$work/target" cargo test --quiet
        fi
    )
done
