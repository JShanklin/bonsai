# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`bonsai` is a **host-side CLI** that scaffolds embedded Rust firmware. The crate
itself is an ordinary `std` binary (a ratatui TUI + a `branch` subcommand) — it
is *not* embedded/`no_std` code. The embedded firmware lives entirely under
`templates/*/src/` and is **data** (baked into the binary, rendered for the
user's new project); it is never compiled as part of this crate. Keep this
distinction in mind: editing `src/main.rs` changes the CLI; editing
`templates/**` changes what firmware the CLI emits.

Domain vocabulary (tree / trunk / branch / graft / sap / pulse / nutrient) is
defined in `README.md` — read it once for the metaphor.

## Commands

```sh
cargo build                 # build the CLI
cargo run                   # launch the interactive wizard (plant a tree)
cargo run -- init           # the wizard, planting into the cwd (no new folder)
cargo run -- branch <name>  # run the `branch` subcommand (needs a bonsai tree in cwd)
cargo run -- branch --produces|--duplex|--roots <name>  # producer / both / std I/O bridge
cargo run -- branch         # no name → interactive TUI (name + kind)
cargo run -- feed <Name> [--broadcast|--directed|--state] [--cap N] [field:type ...]
cargo run -- feed           # no args → interactive TUI (name + fields + path shape/cap)
cargo run -- path <Nutrient> [<shape>] [--cap N]  # show / reshape a nutrient's path
cargo run -- sync           # regenerate src/sap.rs from the wiring
cargo run -- tap <branch> <Nutrient>     # branch consumes it (untap to reverse)
cargo run -- release <branch> <Nutrient> # branch produces it (unrelease to reverse)
cargo run -- list           # summarize the tree (device, flow graph, branches)
cargo test                  # run the unit tests (src/main.rs + src/flow.rs)
cargo test marker_matches_whole_line_not_substring   # run a single test by name
cargo install --path .      # put `bonsai` on PATH (embeds templates/ into the binary)
```

There is no separate lint config; use `cargo clippy` / `cargo fmt`.

`BONSAI_TEMPLATES=/path/to/templates` overrides template resolution so you can
edit templates and generate without rebuilding.

## Architecture

The CLI is `src/main.rs` (commands, TUIs, fs orchestration, ~3k lines incl.
tests) plus `src/flow.rs` (the pure sap generator — see below). Entry points are
dispatched in `main()`:

- **Wizard** (`create_device` / `Wizard` / `run_wizard`): a ratatui TUI that
  walks a fixed hardware cascade `MCU → Chip → Board → WiFi → Tools → Where →
  project name`, then shells out to `cargo-generate` to render the per-board trunk
  template. *WiFi* is asked only when `has_wifi(mcu)` (esp32); it passes
  `-d wifi=true|false` (`template_defines`, shared with `regrow`/`update`, which
  read it back with `tree_has_wifi`: an `esp-radio` dependency). The ESP32
  templates gate WiFi with Liquid `{% if wifi %}` blocks plus
  `[conditional.'!wifi']` dropping `src/wifi.rs` (esp-radio 1.0.0-beta.1 +
  esp-alloc + embassy-net; SSID/password are `env!` values from
  `.cargo/config.toml` `[env]`, whose `build-std` gains `alloc`: Xtensa has no
  prebuilt std, and the C6 builds used to check WiFi don't catch that), so a no-WiFi render is unchanged. *Where*
  is `here` (cwd, `cargo generate --init`: no subfolder, no git init; the name is
  prefilled from the folder via `crate_name`) or `new folder`; the cursor
  defaults to `here` when the cwd holds only dotfiles (`only_dotfiles`).
  `bonsai init` = `create_device(true)`: here only, Where skipped. Planting here
  never overwrites: `refuse_planting_in` rejects `/`, `$HOME` and an existing
  tree, and `plant_here_conflicts` lists template files already in the cwd
  (skipping `cargo-generate.toml`/`pre.rhai`) and aborts before generating. `ratatui::init`
  uses the alternate screen, so `ratatui::restore()` **must** run before any
  cargo-generate output is printed (see the comment in `create_device`).
- **`branch [--produces|--duplex|--roots] <name>`** (`add_branch`, `BranchMode`):
  grafts a subsystem into the tree in the cwd. It does *not* use cargo-generate; it
  writes `src/branches/<name>.rs` from an embedded scaffold — `BRANCH_TEMPLATE`
  (consumer), `BRANCH_PRODUCER_TEMPLATE` (no `match Nutrient`),
  `BRANCH_DUPLEX_TEMPLATE` (both), `BRANCH_ROOTS_TEMPLATE` (std-only I/O bridge,
  still just `start` + `run`: `start` opens the link and passes a blocking
  receive closure and a send closure to `roots::bridge`; `run` does
  `select(inbox.next(), taps.next())`, with `// bonsai:emit` in the inbox arm and
  `// bonsai:nutrient-arm` in the taps arm; the transport and the
  `Inbound`/`Outbound` types are TODOs) — and edits two files via
  `insert_before_marker`. The threads and queues live in one shared
  `src/roots.rs` (`ROOTS_BRIDGE`, from `templates/_branch/roots.rs`): the first
  `branch --roots` writes it, adds `mod roots;` after `mod pulse;` and
  `embassy-futures` to Cargo.toml; `snip` deletes it (and the `mod` line) once
  no branch calls `roots::bridge(`. With no name, `branch_interactive` runs a small ratatui
  TUI (name + kind). Consumer/duplex/roots carry `// bonsai:nutrient-arm` (for
  `tap`); producer/duplex/roots carry `// bonsai:emit` (for `release`). `pulse` is
  refused as a name (it's already a node in the generated sap).
- **The generated sap** (`sync_sap` → `flow::render`): every wiring command
  (branch/snip/feed/starve/tap/untap/release/unrelease/path/sync) ends by
  regenerating `src/sap.rs` from the graph: nutrients (`flow::parse_variants` over
  the enum), per-nutrient shape/cap (`bonsai.toml`, `flow::Config`), and each
  node's taps/releases (`flow::Node::scan` over `pulse.rs` + every branch). Paths
  layout = one `PubSubChannel` (broadcast) / `Channel` (directed) / `Watch` (state)
  per nutrient with SUBS/receivers counted from the graph; `layout = "trunk"` = one
  shared bus. Each node gets `sap::<node>::Taps` (round-robin `next()`, lag
  counter, yields after `BURST` ready polls); releasers get the ZST `Sap`
  (`release().await` / `try_release()` route by variant). `Graph::warnings` flags
  self-deadlock / directed cycles / feedback / directed fan-out; `size_hint` warns
  on big variants. `flow.rs` is pure and unit-tested; fs orchestration is in
  `main.rs`.
- **Slots, not the enum.** In the paths layout each path carries only its own
  variant's payload: a generated `<Nutrient>Slot` struct (field names and types
  from `parse_variants`) or `()` for a unit variant; `<nutrient>_nutrient()`
  rebuilds the enum when `Taps::next()` returns. `Sap::release` is a plain fn
  returning `Release`, a hand-rolled future holding at most one directed path's
  `SendFuture` — never a whole `Nutrient` — so tasks stay small. The trunk layout
  still carries `Nutrient` on its one bus.
- **The sap is a child of the trunk** (`#[path = "sap.rs"] pub mod sap;` in
  trunk.rs, `use trunk::sap;` in main.rs), so slot field types resolve with
  trunk.rs's own imports (`use super::*` in sap.rs). `migrate_sap_module` (run
  by every `sync_sap`) moves the old `mod sap;` out of main.rs; the template's
  trunk.rs must contain `SAP_MOD_IN_TRUNK` verbatim (tested).
- **Every template builds for its device by default** (`.cargo/config.toml`
  `[build] target`): Pico/ESP32 via `pre.rhai`-derived targets with flash
  runners; the rpi boards hard-code theirs (`aarch64-unknown-linux-gnu`, Zero W
  `arm-unknown-linux-gnueabihf`) with an inline `sh -c` runner that scp's the
  binary to `$BONSAI_PI` (set in `[env]`) and runs it over `ssh -t`, or runs in
  place when `uname -m` matches. `cargo local` / `cargo local-test` (aliases,
  `--target host-tuple`) run on the host, which the tutorial uses throughout.
  The Zero W sets no linker: Debian's armhf gcc emits ARMv7 startup code that
  crashes on ARMv6, so it builds with zig (the zigbuild build tool, below).
- **Build tools** (`src/tools.rs`, `bonsai tools [names]` → `tools_command`, and
  the wizard's *Tools* step via `ToolPicker`): sccache, mold, zigbuild, bacon.
  Missing ones install once (`tools::install`: system packages with `sudo`
  pacman/dnf/apt-get, crates with cargo-binstall or `cargo install --locked`).
  Settings are per tree, edited into `.cargo/config.toml` with toml_edit by the
  pure `tools::configure` (unit-tested, including an exact round trip back to
  each template): sccache → `build.rustc-wrapper`; mold → `[target.<host>]`
  clang + `-fuse-ld=mold` (skipped when host == target); zigbuild → the Pi
  target's `linker = ".cargo/zig-cc"`, a script running `cargo-zigbuild zig cc`
  with the right `-target`/`-mcpu` (ARMv6 for the Zero W), so plain `cargo
  build/run` link with zig. `GCC_COMMENT` must match the Pi 5 / Zero 2 W
  templates' linker comment. bacon has no settings. `regrow` reapplies the
  tools it reads back with `tools::configured`. mold/zigbuild fit rpi only.
- **Footprint defaults** live in the templates: Pico `panic-messages` feature
  (default on; `--no-default-features` drops `print-defmt`),
  `DEFMT_RTT_BUFFER_SIZE = "256"`, `codegen-units = 1`; rpi release profile
  (`lto`, `strip`, `panic = "abort"`, `opt-level = "s"`). Generated code panics
  with fixed messages instead of `unwrap()`/`expect()` so no `Debug` formatting
  is linked.
- **Pre-sap trees** (no `src/sap.rs`, grown before per-nutrient paths) are not
  supported: every tree command calls `require_managed`, which refuses them and
  points at README's migration section. Only `migrate_sap_module` remains, for
  trees from the first sap release (`mod sap;` in main.rs).
- **Scaffolds leave application-specific code as TODOs:** the roots transport,
  the producer's wake-up source (a `pending()` placeholder), and where a duplex
  branch releases (the `// bonsai:emit` marker sits after the `match` and is
  flagged to move into the replying arm). The pulse only beats; queue depth is
  opt-in via `sap::depths()`.
- **`snip <name>`** (`remove_branch`): the inverse of `branch` — deletes the
  branch file and reverses both wiring edits via `remove_line`. The `main.rs`
  start call is matched by prefix (`branches::<name>::start(`), so a call
  hand-edited to pass hardware is still found; user-added peripheral setup is
  left untouched.
- **`regrow`** (`regrow`): wipes the cwd back to a fresh template (destructive,
  y/N confirmed). Recovers the device from the tree's own `Cargo.toml` stamp
  (`parse_board` → `device_from_board` reverses the cascade) and its name
  (`parse_package_name`), so it needs no args. Guardrails: refuses unless the dir
  has the full bonsai signature (Cargo.toml stamp + both marker files + trunk/
  pulse) and is not `/` or `$HOME`; renders into a `.bonsai-regrow` staging dir
  and only wipes on success (a failed regen leaves the tree intact); preserves
  `.git/`. The pure helpers (`parse_board`, `parse_package_name`,
  `device_from_board`) are unit-tested; the fs orchestration is not.
- **`ide`** (`ide` → `setup_ide`): generates a project-local Zed rust-analyzer
  setup for **esp/Xtensa** trees only. Xtensa isn't in mainline Rust and Zed's
  rust-analyzer sends `cargo metadata --lockfile-path`, which the esp cargo fork
  rejects — so RA reads no crate graph. `setup_ide` writes `.zed/{settings.json,
  cargo-esp-lockfile-shim, zed-ra-esp}`: a launcher that runs Zed's own RA (or
  rustup's, since Zed stops downloading RA once `binary.path` is set) with
  `CARGO` pointed at a shim that strips `--lockfile-path` and forwards to the esp
  cargo (resolved at runtime via `rustup which --toolchain esp cargo`). Gated on
  the target read back from `.cargo/config.toml` (`parse_target`); a non-`xtensa`
  target is a no-op. Nothing is written outside the tree, so no regrow-style
  host guardrails. The wizard also offers it after planting an esp board.
  `parse_target` is unit-tested; the fs orchestration is not.

### Two independent templating mechanisms (a gotcha)

1. **Trunk templates** (`templates/<mcu>/<board>/`) are rendered by
   cargo-generate through **Liquid** (`{{ project-name }}`, `{% if %}`). A
   per-template `pre.rhai` hook derives build vars (`target`, and `probe_chip`
   for Pico) from the chosen `chip` so the rendered files stay simple.
2. **The branch scaffolds** (`templates/_branch/*.rs`) are **not** rendered by
   cargo-generate. The CLI does a plain `.replace("{{branch_name}}", name)`. So
   `{{branch_name}}` there is a bonsai convention, not Liquid.
3. **`src/sap.rs`** in each trunk template is *generated by the CLI* (run
   `bonsai sync` inside `templates/<mcu>/<board>/` after touching that template's
   `trunk.rs`/`pulse.rs`/`bonsai.toml`). It holds no Liquid, so cargo-generate
   copies it verbatim. `template_sap_matches_generator` fails if any board's copy
   is stale — so changing `flow::render` means re-syncing every template.

### Template resolution (`template_dir`)

Order: `$BONSAI_TEMPLATES` → `./templates` (running from the repo) → the copy
**embedded in the binary** via `include_dir!` (installed, run from anywhere). The
embedded copy is extracted to a temp dir for cargo-generate, then deleted.
The branch scaffolds are embedded separately with `include_str!` so `branch`
works from inside a generated tree where `templates/` isn't present.

## Invariants to preserve

- **Marker lines** `// bonsai:mod` (in `templates/**/src/branches/mod.rs`),
  `// bonsai:start` (in `templates/**/src/main.rs`), `// bonsai:nutrient` (in the
  `Nutrient` enum in `templates/**/src/trunk.rs`), `// bonsai:nutrient-arm` (inside
  each `match Nutrient` block, above the `_ => {}` catch-all — where `tap` inserts a
  handler arm), and `// bonsai:emit` (in producer/duplex branch loops — where
  `release` inserts a publish call) are how the commands find where to insert.
  `insert_before_marker`/`insert_indented_before` match them as a **whole trimmed
  line**, so never remove them and never let a template's only occurrence be inside
  prose. Edits are computed before writing, so a missing marker aborts cleanly.
- **`src/sap.rs` is output, never input.** Nothing reads it back except
  `sap_managed()` (its existence). Its sources of truth are the enum, `bonsai.toml`
  and the branch files. `is_release_of` (strict, line *is* the call) drives edits;
  `mentions_release_of` (loose, anywhere in the line) drives graph reads. Keep that
  split so removal never takes out surrounding code.
- **Connection model (per-branch).** Every `match Nutrient` ends in a `_ => {}`
  catch-all, so a branch only handles the nutrients it's explicitly wired to and
  compiles regardless of which variants exist. So `feed` is **enum-only** (just adds
  the variant; catch-alls absorb it) and `starve` removes the variant *and* any
  dangling `tap` arms / `release` calls for it. Wiring is per-branch: `tap`/`untap`
  add/remove a consumer's arm (`Nutrient::X { .. } => …` for payloads),
  `release`/`unrelease` add/remove a producer/duplex branch's
  `sap.release(Nutrient::X).await`. `remove_balanced_span` also takes a rustfmt
  continuation line (`.await;`) after a multi-line call. **Migration:** trees generated before this
  change lack the catch-all, so enum-only `feed` would break their exhaustive
  matches — `regrow` them first.
- **The hardware cascade** — `MCUS`, `chips()`, `boards()` in `src/main.rs` — is
  the single place the CLI encodes supported hardware. Adding a board means
  editing the cascade **and** adding `templates/<mcu>/<board>/`; the
  `src/trunk.rs` + `src/branches/` template files are MCU-portable and should be
  reused as-is.
- **Embedded-template completeness**: dotfiles like `.cargo/config.toml` are easy
  to drop from the `include_dir!` set, producing a project that can't build. The
  `embedded_template_includes_all_files` test guards this — extend it when a
  template gains a new required file.

## Virtual boards (`containers/`)

Not part of the CLI: shell + Podman Quadlet files that run a virtual Pi as a
rootful container (`sudo containers/install.sh <board> [remove]`). One shared
`Containerfile`, `board.container` template, and two shared networks
(`bonsai-host` bridge 10.89.0.0/24 for the host, `bonsai-lan` macvlan for the
LAN); per-board CPU/memory/cores/addresses in `containers/boards/<board>.conf`,
named after the rpi boards in the cascade. `install.sh` also adds the qemu `C`
binfmt flag (sudo inside), builds with `--network host`, and writes the ssh
config + known_hosts entry. Boards boot systemd (`CMD /sbin/init`): the
Containerfile builds mavlink-router v4 from source in a builder stage, copies
`containers/rootfs/` over `/` and enables every unit in
`rootfs/etc/systemd/system/` (plus ssh and mavlink-router), so user services
are unit files dropped there. Drop-ins there clear `ImportCredential=` on
systemd's tmpfiles/sysusers units, whose credential mounts fail under qemu-user;
mavlink-router installs under `/usr` only (a real `/lib` would replace Debian's
`/lib → usr/lib` link). See `containers/README.md`.

## Docs

- `README.md` explains what bonsai is. How-to material lives in `tutorial/`:
  `foundations/` (read in order; builds the running **greenhouse** project on
  the rpi zero-2w template, so it runs on a PC) and `guides/` (short
  self-contained recipes: roots over UDP/TCP/serial/MAVLink, wire format, GPIO,
  testing, deploy, a virtual Pi (arm64 Podman container, qemu, macvlan), CI,
  troubleshooting).
- Build knowledge up in order: no syntax appears in a chapter before
  `02-rust-essentials.md` (or an earlier chapter) has introduced it. Scaffold
  comments are one short line saying what to change and why; placeholders
  (`let _ = &sap;`, `pending()`) say to delete them once used.
- Tutorial code and command output are taken from real runs. When you change a
  scaffold, a generated file or any CLI message, update the snippets and
  outputs that quote it, and re-run the affected chapter or guide.

## Reference

See `templates/README.md` for template internals (variables, rendering, adding a
board).
