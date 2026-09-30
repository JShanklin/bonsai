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
11:47:26.319Z  INFO bonsai: running
```

That's a **log line**: the time (UTC), how serious it is, who wrote it, and
what they said. The core is running, and waiting: a new tree has no
branches, so nothing happens until you add some (next chapter). Stop it with
Ctrl-C:

```
11:47:37.156Z  INFO bonsai: stopping
```

## What's inside

```
greenhouse/
├── .cargo/
│   └── config.toml   # builds for the Pi; `cargo local` runs here instead
├── Cargo.toml        # name, dependencies, build settings
├── bonsai.toml       # the graph: branches, edges, links (bonsai edits it for you)
└── src/
    ├── main.rs       # the trunk: starts the core
    ├── messages.rs   # the messages branches send each other
    ├── branches/
    │   └── mod.rs    # GENERATED: the list of branches (none yet)
    ├── edges/
    │   └── mod.rs    # GENERATED: your own edges, if any
    ├── bonsai.rs     # GENERATED: the runtime (the core, edges, logs)
    ├── links.rs      # GENERATED: each branch's Input and Out, and the core
    └── settings.rs   # GENERATED: branch settings as constants
```

**GENERATED** files are written by bonsai from `bonsai.toml` and
`src/messages.rs` every time the graph changes. Never edit them: your changes
would be overwritten. Everything else is yours.

`bonsai.toml` starts with comments only: what goes in it, with an example
of each part. The bonsai commands add to it as you grow the tree, and keep
the comments.

`src/messages.rs` has no messages yet: just a line that brings bonsai's
units (`Celsius`, `Meters`…) into scope, and the line `// bonsai:message`,
which marks where `bonsai message add` puts new ones. Each branch you add
gets a `// bonsai:input-arm` line the same way. **Keep every `// bonsai:…`
comment:** they're where bonsai edits your files.

## Look at the tree

```sh
bonsai list
```

```
tree: greenhouse  (zero-2w (bcm2710a1))
branches: none yet (`bonsai branch add <name>`)
links: none yet (`bonsai link <from> <Message> <to>`)
```

## Save your progress

bonsai edits files for you, so commit before and after each step. `git diff`
then shows exactly what each command changed:

```sh
git add -A && git commit -m "Plant greenhouse"
```

Next: [Branches and messages](05-branches-and-messages.md).
