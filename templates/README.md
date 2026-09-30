# templates/

Templates for [cargo-generate]. Each one is a Linux application built as a
tree: **branches** that decide, run one event at a time by a deterministic
**core**, and **edges** that do the I/O, on the tokio runtime.

- **Trunk**: the per-board project. `src/main.rs` starts the core, which
  runs every branch. It stays small.
- **Branch**: a module that keeps its own state and decides what to do with
  each input (`bonsai branch add <name>`). Branches are board-portable.

## Layout

```
templates/
  _tree/
    bonsai.rs              # the runtime every tree carries as src/bonsai.rs:
                           # the Branch and Edge traits, the core loop, ticks,
                           # edge supervision, UDP/TCP edges, logs, stats and
                           # the server `bonsai top` reads, shutdown. Baked
                           # into the binary; `bonsai sync` keeps each tree's
                           # copy current.
    serial.rs              # the serial edge: written to src/edges/serial.rs
                           # (and tokio-serial added) only while a tree has one.
  _edge/
    edge.rs                # the custom-edge scaffold (`bonsai edge add --custom`).
  _branch/
    branch.rs              # the branch scaffold. `{{branch_name}}` and
                           # `{{BranchName}}` are plain .replace, not Liquid.
  linux/
    <board>/               # a trunk project (cargo-generate template)
      cargo-generate.toml
      Cargo.toml
      .cargo/config.toml   # the build target and runner (none for host)
      bonsai.toml          # the graph: branches, settings, rates, links; [record]
      .gitignore           # target/ and logs/ (run logs)
      src/
        main.rs            # trunk: `bonsai::run(links::Core::new())`
        messages.rs        # the message structs (has `// bonsai:message`), units in scope
        bonsai.rs          # GENERATED: the runtime (copy of _tree/bonsai.rs)
        links.rs           # GENERATED: each branch's Input/Out, and the core
        settings.rs        # GENERATED: [branch.<name>] values as constants
        branches/
          mod.rs           # GENERATED: one `pub mod` per branch (none in a new tree)
        edges/
          mod.rs           # GENERATED: custom edges (+ serial when used)
```

Boards covered: `linux/{zero-w,zero-2w,pi5,host}`. There's no HAL: Linux owns
the hardware. A Pi template's `.cargo/config.toml` builds for the Pi, and its
runner copies the binary to the Pi over ssh (`BONSAI_PI`) and runs it there;
`cargo local` runs on the host. The `host` template sets no target, so it
builds and runs on the computer it's on. The release profile unwinds on panic
(no `panic = "abort"`): the core catches a panic in a branch's `process` and
sets that branch up again.

## The branch contract

A branch is a struct implementing `bonsai::Branch`:

```rust
impl Branch for Display {
    type Input = Input;   // crate::links::display::Input, generated
    type Out = Out;       // crate::links::display::Out, generated

    fn setup() -> Self { … }                                 // starting state
    fn process(&mut self, input: Input, out: &mut Out) {     // no I/O, no waiting
        match input {
            Input::Reading(reading) => out.send(Alarm { … }),
            // bonsai:input-arm
        }
    }
}
```

`Input` has one variant per message linked to the branch (plus `Tick` with a
`rate`), so the compiler makes a branch handle each new link. `out.send(m)`
compiles only for messages the branch is linked to send. The struct is
`branches::<name>::<CamelName>`, which the generated core refers to.

`info!`, `warn!`, `error!` and `debug!` log a line tagged with the branch (or
edge) that wrote it. They're defined in `src/bonsai.rs`, which `main.rs`
declares first, with `#[macro_use]`, so every module can use them without an
import; `bonsai sync` adds the attribute to a tree that lacks it.

While a tree runs, it counts what each branch and edge does, and serves the
counts and the log on `127.0.0.1:7777` (`BONSAI_TOP`) for `bonsai top`.
The generated `Out` implements `bonsai::Outbox`, so the core can count sends.

Two markers, matched as whole lines (a comment mentioning one in prose isn't
mistaken for it), keep them intact:

- `// bonsai:input-arm` inside each branch's `match input`: `bonsai link` and
  `bonsai rate` add an arm above it; `unlink` and `rate … off` remove it.
- `// bonsai:message` in `src/messages.rs`: `bonsai message add` puts a struct
  above it.

## Generated files

`src/bonsai.rs`, `src/links.rs`, `src/settings.rs`, `src/branches/mod.rs` and
`src/edges/mod.rs` (plus `src/edges/serial.rs` while a tree has a serial edge)
are written by `bonsai sync` (every graph command runs it). After changing a
template's `bonsai.toml` or `src/messages.rs`, run `bonsai sync` inside
`templates/linux/<board>/`. The `template_links_match_generator` test fails
if a board's copy is stale. They contain no Liquid, so cargo-generate passes
them through untouched. `main.rs` marks `settings` and `links` with
`#[rustfmt::skip]` so `cargo fmt` doesn't reformat them.

## Variables in a trunk template

Passed by the wizard via `-d`: `chip` and `board` (`template_defines` in
`src/main.rs`, from `BOARDS`). Built in by cargo-generate: `project-name`,
`crate_name`. Each template stamps its board and chip into `Cargo.toml`
(`# Generated by bonsai for <board> (<chip>)`); `regrow`, `update`,
`retarget` and `tools` read the board back from it.

## Adding a board

1. Add it to `BOARDS` in `src/main.rs`.
2. Create `templates/linux/<board>/`: copy an existing board and adjust the
   target bits (`Cargo.toml`'s stamp and profile, `.cargo/config.toml`, the
   first doc line of `src/main.rs`). `bonsai.toml` and `src/messages.rs`
   are the same on every board (tested), and a new tree has no branches.

## Rendering

Every file passes through [Liquid], so `{{ project-name }}`,
`{% if board == "pi5" %}...{% endif %}`, etc. work. The branch scaffold
(`_branch/branch.rs`) is *not* rendered by cargo-generate: the CLI does a
plain substitution when you run `bonsai branch add`.

[cargo-generate]: https://cargo-generate.github.io/cargo-generate/
[Liquid]: https://shopify.github.io/liquid/
