# 4. Your first tree

You'll plant **greenhouse**, a monitor that you'll grow over the next
chapters. It uses the Raspberry Pi Zero 2 W template, which builds an
ordinary Linux program, so it also runs on your computer. No hardware needed.

## Plant it

In a folder where you keep projects:

```sh
bonsai
```

A menu opens. Use the arrow keys (or `j`/`k`) and Enter:

1. **Board:** `zero-2w`
2. **Build tools:** Enter, to keep what's offered
3. **Where:** `new folder`
4. **Project name:** type `greenhouse`, then Enter.

bonsai hands off to `cargo-generate`, which creates a `greenhouse/` folder
(already a git repository):

```
 Initializing a fresh Git repository
 Done! New project created …/greenhouse
```

Have a Pi 5 or a Zero W? Pick `pi5` or `zero-w` instead. Everything in the
tutorial works the same.

**Already made the folder?** Run `bonsai init` inside it (or pick `here` at
step 3). The tree is planted right there, with no subfolder, and the name
starts out as the folder's. Nothing is overwritten: if a file the tree needs,
like `Cargo.toml`, is already there, bonsai stops and lists it. It also leaves
git alone, so run `git init` yourself if the folder isn't a repository yet.

```sh
mkdir greenhouse && cd greenhouse
bonsai init
```

In an empty folder, the wizard picks `here` for you anyway.

## Run it

```sh
cd greenhouse
cargo local
```

(Skip the `cd` if you used `bonsai init`: you're already there.)

`cargo local` runs the tree on your computer. A Pi tree is set up to build for
the Pi, so plain `cargo run` would copy it to a Pi and run it there. You'll
use that once you have one (see the [deploy guide](../guides/deploy.md)).
Until then, `cargo local` it is.

The first build takes a minute. Then:

```
10:40:50.159Z  INFO bonsai: running
10:40:50.161Z  INFO pulse: beat
10:40:50.661Z  INFO pulse: beat
10:40:51.161Z  INFO pulse: beat
```

twice a second. Each line is a **log line**: the time (UTC), how serious it
is, who wrote it, and what they said. `pulse: beat` is the **pulse**, a
built-in branch whose clock ticks twice a second, proof that the core is
running. Stop it with Ctrl-C:

```
10:41:03.455Z  INFO bonsai: stopping
```

## What's inside

```
greenhouse/
├── .cargo/
│   └── config.toml   # builds for the Pi; `cargo local` runs here instead
├── Cargo.toml        # name, dependencies, build settings
├── bonsai.toml       # the graph: branches, edges, wires (bonsai edits it for you)
└── src/
    ├── main.rs       # the trunk: starts the core
    ├── messages.rs   # the messages branches send each other
    ├── branches/
    │   ├── mod.rs    # GENERATED: the list of branches
    │   └── pulse.rs  # the heartbeat
    ├── edges/
    │   └── mod.rs    # GENERATED: your own edges, if any
    ├── bonsai.rs     # GENERATED: the runtime (the core, edges, logs)
    ├── wiring.rs     # GENERATED: each branch's Input and Out, and the core
    └── settings.rs   # GENERATED: branch settings as constants
```

**GENERATED** files are written by bonsai from `bonsai.toml` and
`src/messages.rs` every time the graph changes. Never edit them: your changes
would be overwritten. Everything else is yours.

`bonsai.toml` starts with one branch:

```toml
[branch.pulse]
rate = 2
```

`rate = 2` gives the pulse an `Input::Tick` twice a second. Its whole
`process`, in `src/branches/pulse.rs`, is:

```rust
fn process(&mut self, input: Input, out: &mut Out) {
    let _ = out;
    match input {
        Input::Tick => info!("beat"),
        // bonsai:input-arm
    }
}
```

The comment `// bonsai:input-arm` marks where bonsai adds an arm when you
wire something new to a branch, and `// bonsai:message` in
`src/messages.rs` marks where it adds messages. **Keep every `// bonsai:…`
comment:** they're where bonsai edits your files.

## Look at the tree

```sh
bonsai list
```

```
tree: greenhouse  (zero-2w (bcm2710a1))
branches, in the order the core runs them:
  pulse  (ticks 2/s)
wires: none yet (`bonsai wire <from> <Message> <to>`)
```

## Save your progress

bonsai edits files for you, so commit before and after each step. `git diff`
then shows exactly what each command changed:

```sh
git add -A && git commit -m "Plant greenhouse"
```

Next: [Branches and messages](05-branches-and-messages.md).
