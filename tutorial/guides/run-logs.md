# Run logs

Keep a record of each run: when it started, the events you choose, what
went wrong, and when and why it stopped. Each run gets its own folder,
named by the local time it started, with a file for each kind of line.

**Needs:** [chapter 5](../foundations/05-branches-and-messages.md).

## What a run leaves behind

A tree launched at 20:07:45 and stopped with Ctrl-C three seconds later:

```
logs/2026-09-30_20-07-45/
├── events.log
└── panics.log
```

`events.log`:

```
2026-09-30 20:07:45.933 START greenhouse 0.1.0 on vm (pid 6993, UTC+02:00)
2026-09-30 20:07:46.436 sensor: launch: altitude 120 m
2026-09-30 20:07:48.436 sensor: launch: altitude 120 m
2026-09-30 20:07:49.233 END Ctrl-C, after 3s
```

`panics.log`:

```
2026-09-30 20:07:45.933 START greenhouse 0.1.0 on vm (pid 6993, UTC+02:00)
2026-09-30 20:07:47.436 sensor: panicked at src/branches/sensor.rs:35: sensor unplugged
2026-09-30 20:07:49.233 END Ctrl-C, after 3s
```

Every file opens with a **START** line (the tree, its version, the
computer, the process id and the time zone) and closes with an **END**
line: `Ctrl-C`, or `SIGTERM` when systemd stops the service, and how long
it ran. Times are local, to the millisecond.

## Log an event

`record!` works like `info!`: the line goes to the normal log (and
`bonsai top`), and into `events.log`, tagged with the branch or edge that
wrote it:

```rust
Input::Tick => {
    self.ticks += 1;
    if self.ticks == 2 {
        record!("launch: altitude {}", Meters(120.0));
    }
    if self.ticks == 4 {
        panic!("sensor unplugged");
    }
}
```

(This sensor panics on its fourth tick, to show `panics.log`; bonsai sets
it up again, so it counts from 0 and records the launch a second time.)

Use `record!` for what you'll want to find later: a launch, a mode
change, a limit crossed. Everyday detail stays in `info!`/`debug!`.

## Choose what's kept

`[record]` in `bonsai.toml` says where the folders go and which files each
run gets:

```toml
[record]
dir = "logs"        # relative to where the tree runs
events = true       # record!("…") from a branch or an edge
panics = true       # a branch or an edge panicked
errors = false      # every error!/warn! line
edges = false       # edges coming up and going down
keep_runs = 100     # run folders kept, this one included (0: all)
keep_days = 0       # delete run folders older than this (0: never)
max_file_kb = 10240 # past this, a file moves to <kind>.1.log (0: never)
```

Switch one with `bonsai record`, which keeps the comments:

```sh
bonsai record errors on
```

```
record: events, panics, errors → logs/
updated src/links.rs
```

| file | holds |
|------|-------|
| `events.log` | every `record!(..)` |
| `panics.log` | each panic in a branch or an edge, with where and why |
| `errors.log` | every `error!` and `warn!` line, like `hub: WARN connect 127.0.0.1:1: Connection refused (os error 111); retrying in 100ms`, whatever `BONSAI_LOG` shows on the console |
| `edges.log` | each edge coming `up`, or going `down` and why |

`bonsai record` alone shows what's kept; `bonsai list` shows it too.
`bonsai record dir /var/log/greenhouse` moves the folders.

## How much is kept

Each run starts by tidying up after the older ones:

- **`keep_runs`** (default 100): the oldest run folders go until at most
  that many are left, this one included (`bonsai record keep_runs 20`).
- **`keep_days`** (default 0, off): run folders older than this go too.
- **`max_file_kb`** (default 10240, 10 MiB): a file that grows past it
  moves to `<kind>.1.log` (replacing the one there) and a new one starts
  with `(continued from events.1.log: this file reached max_file_kb)`. So a
  long run keeps at most twice that per kind.

Only run folders are ever deleted: a folder named like a run
(`2026-09-30_14-00-05`), holding only the files a run writes, and not in
use. A folder with anything else in it (your notes, a copy you made) is
left alone, and so is a run still going. Trees sharing a `dir` don't
delete each other's runs: a running tree holds a lock on the `.running`
file in its folder. The run that tidied up says so under its START
(`removed 6 old run folder(s) (keep_runs 3, keep_days 0)`).

`0` means no limit for any of the three. A tree whose `[record]` doesn't
name them (one planted before they existed) gets the defaults.

## A run that didn't end

A run that didn't shut down cleanly (the program was killed with `kill -9`,
crashed outside a branch, or the computer lost power or was reset) writes
no END line. The next run notices, and says so under its START line:

```
2026-09-30 23:53:27.878 START greenhouse 0.1.0 on vm (pid 13424, UTC+02:00)
2026-09-30 23:53:27.878 previous run 2026-09-30_21-53-25 did not shut down cleanly (it has no END line)
```

It can't tell why, so it doesn't guess. It reads only the last 4 KiB of
each of that run's files, however big they are, and an END cut off
mid-line doesn't count. A run still going (another tree sharing the
folder) isn't mistaken for one that died.

## Where the folders go

`dir` is relative to the folder the tree runs in: the tree's own folder
with `cargo local`, and the service's `WorkingDirectory=` when it runs as
a service ([Deploy](deploy.md)). A new tree's `.gitignore` leaves `logs/`
out of git.

For one run, `BONSAI_RECORD` overrides it:

```sh
BONSAI_RECORD=off cargo local           # keep nothing this time
BONSAI_RECORD=/tmp/runs cargo local     # folders go here instead
```

## How lines get to the disk

The tree never waits for the disk. Each line goes into a queue of 1024
(`record::QUEUE`), and one thread of its own writes them, in the order they
were taken, and also makes the run's folder and checks the last one. So:

- **A slow disk** (an SD card, a busy USB stick) doesn't slow branches,
  edges or Ctrl-C down.
- **When the queue is full**, a new line is dropped, not waited for, and
  counted: the file gets a line saying how many were dropped, and END says
  how many in all. Bounded, never-waiting logging can't promise to keep
  everything while it's overwhelmed for long; it promises not to take the
  tree down with it, and to say when it lost something.
- **A line** is capped at 8 KiB (`record::MAX_LINE`); a longer one is cut
  short with `…`.
- **On the way out** (Ctrl-C, SIGTERM), the writer writes what it has
  taken, then END, and syncs the files. The tree waits 2 s for that at most
  (`record::END_WAIT`); if the disk is too slow, it says `END may be
  missing` on stderr and exits anyway.
- **Durability:** lines are handed to the OS within 0.1 s of being taken,
  and synced to the disk every 5 s, at START and at END. A crash of the
  program keeps what the OS already has; a power cut can lose up to the
  last few seconds, and can leave the last line half-written.
- **A full disk or a read-only folder** never stops the tree: bonsai says
  so once on stderr (`bonsai: run logs: can't write events.log: …; stopped
  writing it`) and stops writing that file.

## Older trees

A tree planted before run logs has no `[record]` table, so it keeps
nothing; `record!` still logs like `info!`. `bonsai record events on` adds
the table.
