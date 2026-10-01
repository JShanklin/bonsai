# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`bonsai` is a **host-side CLI** that scaffolds Linux applications in Rust (a
Raspberry Pi or the computer it runs on). The crate itself is an ordinary binary
(a ratatui TUI + subcommands). The generated applications live entirely under
`templates/linux/*/src/` and are **data** (baked into the binary, rendered for
the user's new project); they are never compiled as part of this crate. Keep
this distinction in mind: editing `src/main.rs` changes the CLI; editing
`templates/**` changes what the CLI emits. Microcontroller support (Pico,
ESP32, defmt, `bonsai ide`) was removed: bonsai is a Linux tool.

Vocabulary (tree / trunk / branch / message / link / edge / rate / `src/links.rs`) is
defined in `README.md`'s Concepts table. A tree is a deterministic core on
tokio: branches decide, edges do the I/O. The old (Embassy) bonsai's
vocabulary (nutrient, sap, tap/release, feed/starve, graft/snip, roots,
paths) is gone; `renamed()` in `main.rs` points its commands at their
replacements, and the commands refuse its trees. Links were once called
wires: `renamed()` maps `wire`/`unwire`, and `require_tree` renames an older
tree's `[[wire]]` headers to `[[link]]` (`tree::with_links`); `graph::parse`
refuses `[[wire]]`. A new tree has no branches
(there's no built-in heartbeat: it logs `INFO bonsai: running`, and any
branch can have a `rate`).

## Commands

```sh
cargo build                 # build the CLI
cargo run                   # launch the interactive wizard (plant a tree)
cargo run -- init           # the wizard, planting into the cwd (no new folder)
cargo run -- branch add <name>            # add a branch (needs a bonsai tree in cwd)
cargo run -- branch remove <name>         # remove it and every link from/to it
cargo run -- branch         # no name → interactive TUI (name)
cargo run -- message add <Name> [field:type ...]   # a struct in src/messages.rs
cargo run -- message remove <Name>        # refused while linked
cargo run -- message        # no args → interactive TUI (name + fields)
cargo run -- edge add <name> udp|tcp|serial [--bind A --to A --join G --reply --connect A --listen A --device D --baud N --framing lines]
cargo run -- edge add <name> --custom     # src/edges/<name>.rs, an Edge of your own
cargo run -- edge remove <name>           # and every link from/to it
cargo run -- link <from> <Message> <to> [<to> ...]  # branch → branches, a [[link]] in bonsai.toml
cargo run -- link <from> <to> [<to> ...]  # an edge at one end: no message
cargo run -- unlink <from> [<Message>] [<to> ...]   # all receivers when none named
cargo run -- rate <branch> <hz|off>       # Input::Tick at that rate
cargo run -- record [<kind> on|off]       # run logs: events, panics, errors, edges ([record])
cargo run -- record dir <folder>          # where each run's folder goes
cargo run -- record keep_runs|keep_days|max_file_kb <n>   # retention (0: no limit)
cargo run -- sync [--dry-run]   # (--dry-run: each change as a diff, nothing written) regenerate src/{bonsai,links,settings}.rs, branches/ and edges/mod.rs
cargo run -- list           # branches, links, errors and warnings
cargo run -- doctor [--json]   # read-only checks, each with a fix; exit 1 on an error
cargo run -- retarget <board>  # move the tree to another board (pi5, zero-2w, zero-w, host)
cargo run -- top [user@host|local] [--port N] [--once]   # watch a running tree
cargo test                  # run the unit tests (main.rs, graph.rs, tree.rs, tools.rs, top/)
cargo test -- --ignored runtime   # the runtime's regression tests, in a rendered host tree
BONSAI_RUNTIME_MODULES=tcp BONSAI_RUNTIME_TESTS=slow cargo test -- --ignored runtime   # narrowed
scripts/sync-templates.sh [--check]   # regenerate every board (--check: fail if stale)
scripts/check-boards.sh [board ...]   # a representative tree per board, cargo check for its target
cargo test marker_matches_whole_line_not_substring   # run a single test by name
cargo install --path .      # put `bonsai` on PATH (embeds templates/ into the binary)
```

There is no separate lint config; use `cargo clippy` / `cargo fmt`.

`BONSAI_TEMPLATES=/path/to/templates` overrides template resolution so you can
edit templates and generate without rebuilding.

## Architecture

The CLI is `src/main.rs` (the wizard, TUIs, `tools`/`regrow`/`retarget`/
`update`, shared text helpers, tests), `src/tree.rs` (the commands that grow a
tree), `src/graph.rs` (pure: `bonsai.toml` in, generated code out),
`src/tools.rs` and `src/top/`. Entry points are dispatched in `main()`:

- **Wizard** (`create_device` / `Wizard` / `run_wizard`): a ratatui TUI that
  walks `Board → Tools → Where → project name` (`BOARD_STEP`…`NAME_STEP`), then
  shells out to `cargo-generate` to render `templates/linux/<board>/` with
  `-d chip=… -d board=…` (`template_defines`, shared with `regrow`/`update`/
  `retarget`). *Where*
  is `here` (cwd, `cargo generate --init`: no subfolder, no git init; the name is
  prefilled from the folder via `crate_name`) or `new folder`; the cursor
  defaults to `here` when the cwd holds only dotfiles (`only_dotfiles`).
  `bonsai init` = `create_device(true)`: here only, Where skipped. Planting here
  never overwrites: `refuse_planting_in` rejects `/`, `$HOME` and an existing
  tree, and `plant_here_conflicts` lists template files already in the cwd
  (skipping `cargo-generate.toml`) and aborts before generating. `ratatui::init`
  uses the alternate screen, so `ratatui::restore()` **must** run before any
  cargo-generate output is printed (see the comment in `create_device`).
- **The tree model.** A tree's graph is `bonsai.toml`: `[branch.<name>]`
  tables (in core order; `rate` → `Input::Tick`s per second; every other key a
  setting), `[edge.<name>]` tables (`kind` = udp/tcp/serial/custom; built-in
  kinds' keys are checked when parsed, `graph::EDGE_KINDS`, and
  bind/to/connect/listen must be HOST:PORT, `is_address`; a UDP edge takes
  `bind`, `to` or both: send only binds `0.0.0.0:0`; a custom edge's
  other keys are settings) and `[[link]]`s (`from`, optional `message`,
  `to = [..]`: with a message, branch → branches; without, an edge at exactly
  one end: edge → branches, or branch → edges), and an optional `[record]`
  (`dir`, then `events`/`panics`/`errors`/`edges` bools, `graph::RecordCfg`,
  `RECORD_KINDS`, and `keep_runs`/`keep_days`/`max_file_kb` whole numbers,
  `RECORD_LIMITS`, rendered as the runtime's defaults when absent; no table
  keeps nothing). Messages are
  top-level `pub struct`s in `src/messages.rs` (`graph::parse_messages`).
  `graph::parse` → `graph::check` (errors refuse generation: unknown
  names, self-link, duplicates, bad names, `Tick`, the link rules above, an
  edge whose CamelCase name is a message's; warnings: loops via `cycles`,
  branches with no inputs and no rate, unlinked edges) → `render_links`,
  `render_settings`, `render_mod`, `render_edges_mod`. `tree::sync_tree` writes
  those plus `src/bonsai.rs` (`tree::RUNTIME`, from
  `templates/_tree/bonsai.rs`), only when changed; while the tree has a serial
  edge it also writes `src/edges/serial.rs` (`templates/_tree/serial.rs`) and
  adds `tokio-serial` (its defaults pull in no libudev, so it cross-builds
  for the Zero W) with `with_dependency`, and removes both with the last one
  (`without_dependency`). It also adds `libc` (`LIBC`, the runtime's local
  time) to an older tree's manifest. Every graph command ends with it.
- **The runtime** (`templates/_tree/bonsai.rs`, a tree's `src/bonsai.rs`):
  `trait Branch { type Input; type Out: Default; fn setup() -> Self; fn
  process(&mut self, Input, &mut Out) }`, `trait Sends<M>`, `Slot<B>`
  (catch_unwind around `process`; a panic drops that input's sends and
  re-runs `setup`, itself under catch_unwind, as at startup in `Slot::new`;
  a `setup` that panics puts the branch out of service: `branch: None`,
  inputs discarded and counted, `setup` retried on an input after
  `SETUP_RETRY` (1 s) doubling to `SETUP_RETRY_MAX` (60 s); `BranchStats`
  `failed`/`discarded`, sent on the top `branch` row after the old fields,
  shown red "out of service" by `bonsai top`), `drain`
  (run-to-completion with a `MAX_DELIVERIES` runaway cap) and `run()`: one
  tokio interval task per rate feeding an mpsc of `Event<EdgeIn>`s (`Tick` or
  `Edge`), `Tree::start_edges`, a core loop that handles one event at a time,
  and shutdown on Ctrl-C/SIGTERM (the shutdown future is made once and
  pinned: rebuilt per iteration, it missed signals). Edges: `trait Edge { type
  In; type Out; recv (cancel-safe: it's dropped whenever something goes out);
  execute }`, opened by an inherent `setup` (built-ins take their config);
  `spawn_edge` supervises one (each attempt its own task, so a panic or `Err`
  restarts only that edge, backoff 0.1 s → 5 s, reset after 30 s healthy; a
  forwarder keeps the outbound queue across restarts); `EdgeOut<T>` is the
  core's side (`try_send`, never waits: drops and counts when full; before
  `start_edges` it keeps what's sent, which `drain_<edge>()` returns for
  tests). Built-ins: `Udp` (bind, to, join on iface, reply-to-last),
  `Tcp` (client, reconnect = restart; server: `max_clients`, default
  `MAX_CLIENTS` 64, counting closed clients still draining (`closing`,
  finished writers reaped by `held`): at the limit it cuts off the one
  draining longest, else the longest-lingering client, before refusing;
  a reader task and a writer task (`write_to`) per client, the writer fed by
  a bounded queue (`CLIENT_QUEUE` 64; `execute` only `try_send`s, a full
  queue discards that copy) with each write under `WRITE_TIMEOUT` (5 s; on
  timeout or error the connection is dropped, never re-written, the rest
  counted as discarded, `WriteFailed` sent to the server);
  the reader reports `ReadEnded`; a client that stopped
  sending lingers `LINGER` (2 s) for replies, broadcasts skip it; `close`
  then gives its writer `DRAIN_TIMEOUT` (5 s, a oneshot deadline the writer
  enforces itself) for what's queued; each client's `outstanding` counts
  copies queued or being written, and whoever cuts it off (the writer
  giving up, `cut_off` on eviction or `Drop`) swaps it to 0 into
  discarded, so nothing is counted twice; `Drop` aborts every client task), `Framed<S>` (raw / lines, `max_frame`,
  default `MAX_FRAME` 1 MiB, measured on the payload without `\n`/`\r\n`:
  every complete frame is checked as it's handed out, and the incomplete
  tail after every read by `frame_len` (a last `\r` not counted yet), so
  validity doesn't depend on how reads split the bytes; an over-long line is
  `InvalidData` and clears what's buffered; `pending_len`) shared with `Serial`; all carry
  `Packet { bytes, peer }`. `max_frame`/`max_clients` are optional keys in
  `[edge.<name>]` (`graph::EdgeKind`), rendered as the runtime constants
  when absent.
  Logs: `error!`/`warn!`/`info!`/`debug!` are `macro_rules!` at the top of
  the runtime, in scope everywhere through `#[macro_use] mod bonsai;` (first
  in `main.rs`; `sync` adds it to older trees, `tree::with_macro_use`).
  `mod log`: each line is `HH:MM:SS.mmmZ LEVEL source: msg` on stderr
  (`write_all`, so a closed stderr can't panic; `eprint!` under
  `cfg(test)`, which `cargo test` captures; level coloured
  on a terminal unless `NO_COLOR`); the source is the edge's `task_local!`
  `EDGE` (set by `spawn_edge` on every task of an edge), else the thread-local
  branch `Slot::process` sets, else `bonsai`. `BONSAI_LOG` (`warn,gps=debug`,
  default `info`) is read once. The last `KEEP` lines stay for `recent()`
  (for `bonsai top`). `run()` installs a panic hook: one `ERROR` line, and
  the backtrace only when `RUST_BACKTRACE` asks.
  Stats (`mod stats`): atomics in a registry keyed by name (the same name
  gets the same counts), registered by `Slot::new` and `EdgeOut::new`, so in
  bonsai.toml order: per branch inputs/sent/panics/busy/max (`Slot::process`
  times `process`; a panicked input isn't timed; sent comes from `Outbox::count`,
  which the generated `Out` implements, bound on `Branch::Out`), per edge
  state/received/accepted/sent/dropped/failed/discarded/restarts/last error
  (`EdgeOut::send` counts accepted into the edge's one queue, `EDGE_QUEUE`,
  or dropped when full or closed, warning once per episode; each attempt
  locks that queue, so it survives restarts with nothing lost in between;
  `attempt` counts sent or failed per message, a panic mid-`execute` via its
  `executing` flag; discarded is a copy an edge took but couldn't deliver),
  and the core's events/slowest/inbox (`run`). The edge row puts the newer
  counts after the error, so older `bonsai top`s still read it; top shows
  failed + discarded as **lost**. `mod top`:
  `run()` serves them on `BONSAI_TOP` (default `127.0.0.1:7777`, a bare port,
  or `off`; a bind failure is one WARN) as `stats::render` text every 500 ms:
  `bonsai-top 1\t…` then `branch`/`edge`/`link`/`sys`/`log` rows
  (tab-separated) and `end`. Links: the generated `Core::new` registers a
  `LINKS` table (from, message or `""`, to…; `stats::links`, first call
  wins) and `deliver`/`handle` bump `stats::link(i)` per message link,
  edge→branch event and branch→edge send (lock-free counters in a
  `OnceLock`). `sys`: `parse_sys` over /proc/self/status, /proc/self/stat,
  /proc/loadavg and /proc/meminfo, read only while a client is connected;
  new log lines come from `log::since(n)` (the ring counts every line).
  Observation only: it never changes what a branch sends.
  Run logs (`mod record`): the generated `Core::new` calls
  `record::configure(RECORD_CONFIG)` (from `[record]`); `run()` calls
  `record::start()` (a folder per run under `dir`, named by the local start
  time via `libc::localtime_r`, `YYYY-MM-DD_HH-MM-SS`, `-2` on a clash; one
  append-only file per kind that's on, each opened with a START line: package,
  version, host, pid, UTC offset; a `.running` file flock'd for the run's
  life marks it active; the folder is made as `.new-<pid>-<n>`, locked, then
  renamed to its run name (`new_run`, `rename_new`: renameat2 NOREPLACE), all
  under a flock on `dir/.bonsai-record.lock` (`DirLock`, `DIR_LOCK_WAIT` 5 s)
  held through `prune`; without that lock (open error, or a timeout) the run
  records nothing, with one stderr line, so `.new-*` folders are only ever
  made and cleared under it;
  `prune` clears stale unlocked `.new-*` folders and deletes old run folders past `keep_runs`
  (`KEEP_RUNS` 100, this one counted) or `keep_days` (0: off), oldest
  first, never the current one, a `running` one, or one holding anything
  but run files (`only_run_files`); a note when the last run folder, by
  mtime, isn't running and has a file whose last whole line (read from the
  last `TAIL` 4 KiB) isn't END: "did not shut down cleanly", no cause
  claimed; a file past `max_file_kb` (`MAX_FILE_KB` 10 MiB) is renamed to
  `<kind>.1.log` and a new one started) and, on Ctrl-C/SIGTERM
  (`shutdown()` returns which), `record::end(why)` (END and how long it ran).
  Lines: `record!` (`record::event`: an INFO log line plus `events.log`),
  panics from the panic hook, `error!`/`warn!` from `log::write` (before
  the `BONSAI_LOG` filter, so errors.log keeps them whatever the console
  shows), and edge up/down
  from `attempt`/`spawn_edge`. Callers only format the line (capped at
  `MAX_LINE`) and `try_send` it to a bounded queue (`QUEUE` 1024; full:
  dropped and counted per kind, `DROPPED`); one `bonsai-record` thread
  (`Writer`) owns the files and does all the disk work, START included, in
  the order lines were taken: buffered, flushed within `FLUSH_EVERY` (0.1 s),
  synced every `SYNC_EVERY` (5 s), at START and at END, notes drops in the
  file and in END. `end` (async) stops producers (`ACCEPTING`), hands the
  writer the reason with a oneshot, and awaits it at most `END_WAIT` (2 s),
  so the runtime thread never blocks. An I/O error prints once on stderr
  (never through the logger) and closes that file (`FAILED`). `fault`
  (`cfg(test)`) slows or fails writes for the runtime tests. `BONSAI_RECORD`
  (`off`, or a folder) overrides for one run. `bonsai record` edits the
  table (`set_keeping_comment` keeps each value's comment in its column) and
  `bonsai list` prints `record_summary`. Nothing opens without `run()`, so
  tests write no files. Observation only, like logs.
  Units (`mod units`): `f32` newtypes made by `macro_rules! unit` (same-unit
  arithmetic/compare, scaling, ratio, `Display` with the symbol and
  precision), `convert!` (`From` both ways within a kind) and `product!`
  (`a × b = c` and its inverses): temperature, length, speed, `Seconds`
  (↔ `Duration`), `Hertz`, angles (`sin`/`cos`), electrical, pressure,
  `Percent`. Every board's `src/messages.rs` has `pub use
  crate::bonsai::units::*;`, so branches get them through `use
  crate::messages::*`; `message add` adds that line to an older tree when a
  field names a unit (`tree::with_units_import`, `uses_units`).
  `tree::UNITS` lists them (reserved as message names;
  `every_unit_is_in_the_runtime` keeps it in step: one `unit!(Name, ` line
  each).
  Templates build with `flavor = "current_thread"` and **no**
  `panic = "abort"` (unwinding is what makes the reset possible).
- **The generated links** (`src/links.rs`; `src/wiring.rs` in older trees,
  which `sync_tree` moves across with `tree::with_links_module`): `enum EdgeIn` (one variant per
  edge, its CamelCase name), `enum Msg` (per message link `<FromCamel><Message>`,
  per branch → edge `<FromCamel>To<EdgeCamel>`), and per branch `mod <name> {
  enum Input (in `graph::input_variants` order: Tick if rated, then each linked
  message or edge); struct Out { sent: Vec<Msg> } }` with an inherent generic
  `out.send(m)` bounded on `Sends<M>` (implemented only for that branch's
  message links) and `out.to_<edge>(v)` per edge it's linked to. Built-in edges'
  settings become `UdpConfig`/`TcpConfig`/`SerialConfig` consts. `Core` holds a
  `Slot` per branch and an `EdgeOut` per edge, delivers each `Msg` and each
  `Event::Edge` to its `to` list in order (cloning for all but the last), and
  `start_edges` spawns every edge. A branch's struct is
  `branches::<name>::<CamelName>`, a custom edge's `edges::<name>::<CamelName>`
  (its `In`/`Out` used as `<T as Edge>::In`). Runtime types are written fully
  qualified, so a message can't shadow them; `Packet` and friends are also
  reserved message names (`tree::RESERVED`), and `serial` an edge/branch name.
- **Sync plan and doctor.** `src/sync.rs`: `sync::plan(root)` works out
  every file `bonsai sync` would create, change or remove (`Change`
  before/after, a `label` for the `updated …` line; migrations included:
  `[[wire]]`, `src/wiring.rs`, `#[macro_use]`, Cargo.toml deps), reading
  only, refused (`Refused`) on config or graph errors or missing
  branch/custom-edge files; `tree::sync_tree` applies it (`sync::apply`:
  each file by `write_atomic`, temp beside it, fsync, rename; a file that
  differs from the plan's `before` stops it; the set isn't atomic, so the
  `.bonsai-sync` journal (`JOURNAL`) is written first and removed last: a
  tree left with it is "interrupted", reported by doctor and finished by
  the next sync, which plans again from bonsai.toml) and `--dry-run`
  prints `render_dry_run` (`diff`: unified, LCS on lines, 3 lines of
  context) from the same plan. Graph commands say when they remove an arm
  with code in it, a branch's or custom edge's file, or an edge other
  branches still send to. `src/doctor.rs`:
  `bonsai doctor [--json]`, read-only `Finding`s (`id` stable: tree, config,
  graph, sources, generated, markers, toolchain.cargo/target/linker,
  tools.<name>, deploy.address; `status` ok/skipped/warning/error;
  optional `subject`, `fix`); stale generated files are the plan's changes;
  exit 0 without errors (warnings and skips allowed), 1 with any, 2 usage.
  Nothing is written, installed or contacted (rustup is asked which targets
  are installed). Tests use `TEMPLATES`' host tree in temp folders.
- **Graph commands** (`src/tree.rs`): `branch add` writes the scaffold
  (`templates/_branch/branch.rs`, `{{branch_name}}`/`{{BranchName}}` by plain
  replace) and an empty `[branch.x]`; `branch remove` also drops its links,
  takes it out of `to` lists, and removes arms no longer fed. `link` merges
  into an existing (from, message) link and inserts `Input::M(_m) => {}` at
  `// bonsai:input-arm` in each new receiver. Arms follow one rule
  (`reconcile_arms`): every command that edits the graph compares each
  branch's `input_variants` before and after, adds an arm for each gained and
  removes (`remove_balanced_span`, so filled-in multi-line arms go whole) each
  lost — so `unlink`, `rate`, `branch remove` and `edge remove` all stay in
  step. `edge add` builds the table from `--key value` flags (the pure
  `edge_table`: `--reply`, repeatable `--join`, a flag followed by nothing or
  another flag refused with what it takes, `wants`) and saves only if
  `graph::parse` accepts it; an edge may be linked one way only;
  `--custom` writes `templates/_edge/edge.rs` to `src/edges/<name>.rs`.
  `message add` inserts a `#[derive(Clone, Debug)]` struct above
  `// bonsai:message`; `message remove` (refused while linked) takes it with
  its attributes/docs (`without_struct`). toml_edit keeps `bonsai.toml`'s
  comments; the fs round-trip test checks every file comes back byte-identical.
  `require_tree` refuses the old bonsai's trees (`src/sap.rs` or
  `embassy-executor`). Comments in `bonsai.toml` stay put: with no tables
  yet, toml_edit keeps them all as trailing text, so `save_doc`
  (`header_first`) puts them above the first new table; removing a table
  moves the comments above it to the first table left, or the end
  (`keep_comments`).
- **Every template builds for its device by default** (`.cargo/config.toml`
  `[build] target`): the Pi boards hard-code theirs (`aarch64-unknown-linux-gnu`,
  Zero W `arm-unknown-linux-gnueabihf`) with an inline `sh -c` runner that scp's the
  binary to `$BONSAI_PI` (set in `[env]`) and runs it over `ssh -t`, or runs in
  place when `uname -m` matches. `cargo local` / `cargo local-test` (aliases,
  `--target host-tuple`) run on the host, which the tutorial uses throughout.
  The Zero W sets no linker: Debian's armhf gcc emits ARMv7 startup code that
  crashes on ARMv6, so it builds with zig (the zigbuild build tool, below).
  The `host` template's config is only a comment: no target, so plain `cargo
  run` builds and runs on this computer (`parse_target` → None; size estimates
  then use the CLI's own pointer width).
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
  tools it reads back with `tools::configured`. `Tool::fits(board)`: zigbuild
  fits the Pi boards only; on `host` mold links the tree's own (native) builds.
  `configure` drops a `[build]` table it leaves empty (a host tree has none).
- **Footprint defaults** live in the templates' release profile (`lto`,
  `codegen-units = 1`, `strip`, `opt-level = "s"`; `3` on pi5 and host), and
  in tokio's trimmed features (`rt`, `macros`, `time`, `sync`, `signal`).
  No proc macros: the glue is generated source.
- **`bonsai top`** (`src/top/`: `mod.rs` the data, connecting and
  `--once`; `view.rs` the tabs; `graph.rs` the layout): reads a tree's top
  server. Where
  (`destination`): an arg (`local` = this computer), else `$BONSAI_PI`, else
  a Pi-targeting tree's `[env] BONSAI_PI` (`parse_target`/`parse_scoped_key`),
  else 127.0.0.1. Remote goes through `ssh -o BatchMode=yes -W
  127.0.0.1:<port> <host>` (stdio forwarding: nothing on the Pi but sshd;
  ssh's last stderr line becomes the error). A reader thread parses snapshots
  (`read_snapshot`/`parse`, pure; version in the first field, unknown row
  kinds skipped) and reconnects every second. The view (`view::App`) is a
  header (the core) over five tabs, switched by `1`-`5`, Tab/Shift-Tab or a
  click (mouse capture on while it runs; `Hits` records where the last frame
  put tabs and nodes): Graph, Branches, Edges, Log (scroll, `/` search, `l`
  level, source filter from `enter` on a node) and System (gauges from the
  `sys` row). Rates are counter deltas over the tree's uptime (`per_sec`)
  across a `WINDOW_MS` (2 s) history of snapshots, so a 1 Hz branch doesn't
  flicker. The graph: `graph::arrows`/`back_arrows` (a DFS from the edges
  that feed the tree, then unfed nodes) /`columns` (longest path over
  forward links; receive-only edges at least as far right as the last
  branch) place boxes (square branches, round edges, coloured by state: red
  for a panic in the last 10 s or a retrying edge); a `Canvas` joins line
  segments into box-drawing junctions; next-column links run through the
  gap, longer ones over lanes above the boxes, back links along lanes
  below. A tree without `link`/`sys` rows (older runtime) shows nodes
  without arrows and a hint to `bonsai sync`. `--once` prints tables (and
  the links) from snapshots 2 s apart. The format must match
  `templates/_tree/bonsai.rs`'s `stats::render` (both sides are unit-tested
  against the same text).
- **`regrow`** (`regrow`): wipes the cwd back to a fresh template (destructive,
  y/N confirmed). Recovers the device from the tree's own `Cargo.toml` stamp
  (`parse_board`, then `chip_of` from `BOARDS`) and its name
  (`parse_package_name`), so it needs no args. Guardrails: refuses unless the dir
  has the bonsai signature (Cargo.toml stamp + `bonsai.toml` + the messages
  marker + `src/links.rs` (or an older tree's `src/wiring.rs`), or an old
  bonsai tree's equivalents, so an old tree
  can be regrown into a new one) and is not `/` or `$HOME`; renders into a `.bonsai-regrow` staging dir
  and only wipes on success (a failed regen leaves the tree intact); preserves
  `.git/`. The pure helpers (`parse_board`, `parse_package_name`, `chip_of`)
  are unit-tested; the fs orchestration is not.
- **`retarget <board>`** (`retarget`): moves a tree to any other board,
  `host` included. Renders the new board's template
  to a temp dir, then takes only its board-owned parts: `retargeted_manifest`
  (the stamp line, template dependencies via `updated_manifest`, `[profile]`),
  `retargeted_config` (the whole `.cargo/config.toml`, keeping the tree's
  `[env]` values: moving a Pi tree to `host` keeps `BONSAI_PI`, placed under
  the host template's comment, for moving back) and `retargeted_main` (main.rs's
  first doc line, only
  while it's still the generated one). Lists the changed files and asks y/N,
  then reapplies the tree's build tools. Branches and the rest of `src/` are
  untouched. The pure helpers are unit-tested (round trips between every
  board's templates); the fs orchestration is not.

### Two independent templating mechanisms (a gotcha)

1. **Trunk templates** (`templates/linux/<board>/`) are rendered by
   cargo-generate through **Liquid** (`{{ project-name }}`, `{% if %}`). Each
   hard-codes its own target; `chip`/`board` are passed as defines.
2. **The branch scaffold** (`templates/_branch/branch.rs`) is **not** rendered
   by cargo-generate. The CLI does a plain `.replace` of `{{branch_name}}` and
   `{{BranchName}}`, a bonsai convention, not Liquid.
3. **The generated files** (`src/bonsai.rs`, `src/links.rs`,
   `src/settings.rs`, `src/branches/mod.rs`) in each trunk template are
   *written by the CLI* (run `bonsai sync` inside `templates/linux/<board>/`
   after touching that template's `bonsai.toml`/`messages.rs`, or after
   changing `graph::render_*` or `templates/_tree/bonsai.rs`). They hold no
   Liquid. `template_links_match_generator` fails if any board's copy is
   stale, and checks `bonsai.toml` and `messages.rs` are identical across
   boards and that a new tree has no branches.

### Template resolution (`template_dir`)

Order: `$BONSAI_TEMPLATES` → `./templates` (running from the repo) → the copy
**embedded in the binary** via `include_dir!` (installed, run from anywhere). The
embedded copy is extracted to a temp dir for cargo-generate, then deleted.
The branch scaffold and the runtime are embedded separately with
`include_str!` (with the serial edge and the custom-edge scaffold) so the
graph commands work inside a tree where `templates/` isn't present.

## Runtime tests and CI

The runtime (`templates/_tree/bonsai.rs`) is data to this crate, so its
behaviour is tested in a rendered tree: `runtime-tests/` (not part of the
crate; formatted with `rustfmt --edition 2024`) is copied into
`templates/linux/host` rendered at `target/runtime-tests/tree` as its
`runtime_tests` module by the ignored `runtime_regression_tests_pass_in_a_rendered_host_tree`
(`render_runtime_test_tree`), and `cargo test` runs there
(`CARGO_TARGET_DIR=target/runtime-tests/target`). Tests that touch
process-wide state (the logger, the panic hook, the recorder, signals) run
a scenario in a child process (`support::spawn`/`child`: the test binary
re-run with one `#[ignore]`d scenario and `BONSAI_RUNTIME_TEST_CHILD`; its
output is drained as it comes). `record::fault` (`cfg(test)` only) injects
a slow or failing disk. `.github/workflows/ci.yml` runs fmt, clippy, the CLI
tests, `scripts/sync-templates.sh --check`, the runtime tests and
`scripts/check-boards.sh` (cross `cargo check` for aarch64/armv6: a compile
check, not a hardware test).

## Invariants to preserve

- **Marker lines** `// bonsai:input-arm` (inside every branch's `match
  input`) and `// bonsai:message` (in `src/messages.rs`) are where the
  commands insert. `insert_before_marker`/`insert_indented_before` match them
  as a **whole trimmed line**, so never remove them and never let a template's
  only occurrence be inside prose. A missing arm marker only prints a note
  (the compiler still demands the arm). `cargo fmt` pulls the arm marker up
  behind the last arm (`} // bonsai:input-arm`: a comment after an arm with
  no comma becomes its trailing comment), so `add_arm`/`remove_arm` first put
  it back on its own line (`marker_on_own_line`; a line that is itself a
  comment is prose and left alone).
- **Generated files stay rustfmt-clean.** `cargo fmt` formats
  `src/bonsai.rs`, `src/branches/mod.rs` and `src/edges/mod.rs` (only
  `links`/`settings` are `#[rustfmt::skip]`), so the runtime is kept
  formatted and `mod.rs` lists are sorted, or `cargo fmt --check` fails on a
  new tree and `bonsai sync` undoes the formatting;
  `generated_files_are_rustfmt_clean` checks it, and the branch and edge
  scaffolds as rendered (their `use` lines stay in rustfmt's order:
  `crate::links::…` before `crate::messages::*`).
- **Generated files are output, never input.** The sources of truth are
  `bonsai.toml` and `src/messages.rs`; branch files are scanned for nothing
  (the compiler checks them against the generated `Input`/`Out`). Arms are
  found with `is_input_arm` (`Input::M(` / `Input::Tick` then a delimiter).
- **Determinism.** `process` must stay sync and I/O-free, and the core must
  deliver in `bonsai.toml` order, run-to-completion per event. Anything that
  talks to the outside world belongs on an edge, never in `process`; the core
  never awaits an edge (`EdgeOut::send` is `try_send`). Logging is the one
  side effect `process` may have: it changes nothing a branch sends.
- **The board list** — `BOARDS` in `src/main.rs` (board, chip, description) —
  is the single place the CLI encodes supported hardware. Adding a board means
  editing it **and** adding `templates/linux/<board>/`; `bonsai.toml`,
  and `src/messages.rs` are the same on every board.
- **Embedded-template completeness**: dotfiles like `.cargo/config.toml` are easy
  to drop from the `include_dir!` set, producing a project that can't build. The
  `embedded_template_includes_all_files` test guards this — extend it when a
  template gains a new required file.

## Virtual boards (`containers/`)

Not part of the CLI: shell + Podman Quadlet files that run a virtual Pi as a
rootful container (`sudo containers/install.sh <board> [remove]`). One shared
`Containerfile`, `board.container` template, and two shared networks
(`bonsai-host` bridge 10.89.0.0/24 for the host, `bonsai-lan` for the LAN:
macvlan on Ethernet, ipvlan on Wi-Fi, since access points drop frames from other
MACs; `install.sh` fills in `@DRIVER@`, falls back to macvlan when netavark is
older than 1.5, and recreates the network, restarting its boards, when its
driver/parent/subnet changed). On Wi-Fi, incoming multicast often never
reaches an ipvlan board (the driver drops it), so `VIRTUAL_PI_RELAY="group:port
…"` makes one `bonsai-relay@<board>-<n>` host service each (socat joins the
group on the Wi-Fi interface and sends every datagram to the board's host
link; settings in `/etc/bonsai/relay-*.env`, kept across installs when the
variable is unset, removed by an empty value, an Ethernet install or `remove`); per-board CPU/memory/cores/addresses in `containers/boards/<board>.conf`,
named after the Pi boards in `BOARDS`. `install.sh` also adds the qemu `C`
binfmt flag (sudo inside), builds with `--network host`, and writes the ssh
config + known_hosts entry. Boards boot systemd (`CMD /sbin/init`): the
Containerfile copies `containers/rootfs/` over `/` and enables every regular
unit file in `rootfs/etc/systemd/system/` (links are skipped: Debian's
`sshd.service` alias), plus ssh, so user services are unit files dropped there;
`example.service` + `/etc/example.conf` show the pattern. Drop-ins there clear
`ImportCredential=` on systemd's tmpfiles/sysusers units, whose credential
mounts fail under qemu-user. rootfs must never hold a real `lib/` or `bin/`
(Debian links those into `/usr`). See `containers/README.md`.

## Docs

- `README.md` explains what bonsai is. How-to material lives in `tutorial/`:
  `foundations/` (8 chapters, read in order; builds the running
  **greenhouse** project on the zero-2w template, run with `cargo local` so
  it runs on a PC: sensor → watchdog → display with `Celsius`/`Percent`
  fields, a `limit` setting, a UDP `uplink` edge with a text protocol, tests,
  logs and `bonsai top`) and `guides/` (short self-contained recipes: edges
  over UDP/TCP/serial and custom edges, units, binary messages, testing, run
  logs, deploy, build tools, a virtual Pi
  (arm64 Podman container, qemu, macvlan/ipvlan), CI, troubleshooting).
- Build knowledge up in order: no syntax appears in a chapter before
  `02-rust-essentials.md` (or an earlier chapter) has introduced it. Scaffold
  comments are one short line saying what to change and why; placeholders
  (`let _ = out; // delete once it sends`, the custom edge's `pending()`) say
  to delete them once used.
- Tutorial code and command output are taken from real runs. When you change a
  scaffold, a generated file or any CLI message, update the snippets and
  outputs that quote it, and re-run the affected chapter or guide.

## Reference

See `templates/README.md` for template internals (variables, rendering, adding a
board).
