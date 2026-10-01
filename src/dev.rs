//! `bonsai dev`: run the tree on this computer, and rebuild and restart it
//! whenever its sources change.
//!
//! On a change (edits are let settle first), the graph is checked, the tree
//! built, and only a build that succeeded replaces the running program: it's
//! stopped with SIGTERM (as systemd would; SIGKILL after `STOP_WAIT`) and the
//! new one started. A failed build leaves the previous one running. Generated
//! files are synced only with `--sync`; otherwise a stale tree isn't built.
//! What's watched is the tree's sources (`src/**/*.rs`, bonsai.toml,
//! Cargo.toml, .cargo/config.toml, build.rs), never target/, logs or
//! Cargo.lock, and what `--sync` writes isn't a change. Ctrl-C stops the
//! program (it gets SIGINT, as if run directly) and then `bonsai dev`; the
//! program runs in its own process group and, on Linux, is ended if `bonsai
//! dev` itself is killed, so nothing is left running.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// How often the sources are looked at.
const POLL: Duration = Duration::from_millis(200);
/// Edits are taken together until the sources stay unchanged this long.
pub const SETTLE: Duration = Duration::from_millis(300);
/// How long a program gets to stop before it's killed.
pub const STOP_WAIT: Duration = Duration::from_secs(5);
/// How long a build being cancelled gets to stop before it's killed.
pub const BUILD_STOP_WAIT: Duration = Duration::from_secs(2);

/// The state of the watched files: path → (modified, length).
pub type Snapshot = BTreeMap<PathBuf, (Option<SystemTime>, u64)>;

/// Whether a path (relative to the tree) is one of its sources.
pub fn watched(rel: &Path) -> bool {
    let s = rel.to_string_lossy();
    matches!(
        s.as_ref(),
        "bonsai.toml" | "Cargo.toml" | ".cargo/config.toml" | "build.rs"
    ) || (s.starts_with("src/") && s.ends_with(".rs") && !s.contains("/."))
}

/// The tree's sources as they are now.
pub fn snapshot(root: &Path) -> Snapshot {
    fn walk(root: &Path, dir: &Path, out: &mut Snapshot) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
            if path.is_dir() {
                walk(root, &path, out);
            } else if watched(&rel)
                && let Ok(m) = e.metadata()
            {
                out.insert(rel, (m.modified().ok(), m.len()));
            }
        }
    }
    let mut out = Snapshot::new();
    walk(root, &root.join("src"), &mut out);
    for f in [
        "bonsai.toml",
        "Cargo.toml",
        ".cargo/config.toml",
        "build.rs",
    ] {
        if let Ok(m) = std::fs::metadata(root.join(f)) {
            out.insert(PathBuf::from(f), (m.modified().ok(), m.len()));
        }
    }
    out
}

/// The files that differ between two snapshots (changed, added or gone).
pub fn changed(before: &Snapshot, after: &Snapshot) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = after
        .iter()
        .filter(|(p, v)| before.get(*p) != Some(v))
        .map(|(p, _)| p.clone())
        .collect();
    out.extend(before.keys().filter(|p| !after.contains_key(*p)).cloned());
    out.sort();
    out
}

/// The program a `cargo build --message-format=json-render-diagnostics`
/// built: the last binary artifact's `executable`.
pub fn executable(cargo_json: &str) -> Option<PathBuf> {
    cargo_json
        .lines()
        .filter(|l| l.contains("\"reason\":\"compiler-artifact\""))
        .filter_map(|l| {
            let at = l.find("\"executable\":\"")? + "\"executable\":\"".len();
            let end = l[at..].find('"')?;
            Some(PathBuf::from(l[at..at + end].replace("\\\\", "\\")))
        })
        .next_back()
}

/// What happened, for the person watching (and the tests).
#[derive(Debug, Clone, PartialEq)]
pub enum Note {
    Watching,
    Changed(Vec<PathBuf>),
    Synced(String),
    /// The graph can't be built from; nothing was built.
    Refused(Vec<String>),
    /// Another command is changing the tree; it waits for it.
    Waiting(String),
    /// Another command held the tree past `lock::wait()`; not built.
    Busy(String),
    /// Generated files are out of date and `--sync` wasn't asked for.
    Stale(Vec<String>),
    Building,
    /// The build failed; whether the previous program is still running.
    BuildFailed {
        still_running: bool,
    },
    Started(u32),
    Stopping(u32),
    /// It didn't stop within `STOP_WAIT`, and was killed.
    Killed(u32),
    /// The program ended by itself.
    Exited(String),
    Done,
}

impl Note {
    fn text(&self) -> String {
        match self {
            Note::Watching => {
                "watching src/, bonsai.toml, Cargo.toml and .cargo/config.toml (Ctrl-C to stop)"
                    .to_string()
            }
            Note::Changed(files) => {
                let names: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
                format!("changed: {}", names.join(", "))
            }
            Note::Synced(what) => format!("synced: updated {what}"),
            Note::Refused(errors) => format!(
                "not building: the tree has errors\n{}",
                errors
                    .iter()
                    .map(|e| format!("  error: {e}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
            Note::Stale(files) => format!(
                "not building: generated code is out of date ({}); run `bonsai sync`, or `bonsai dev --sync` to sync on every change",
                files.join(", ")
            ),
            Note::Waiting(who) => format!("waiting for {who} to finish changing the tree…"),
            Note::Busy(why) => format!("not building: {why}; trying again on the next change"),
            Note::Building => "building…".to_string(),
            Note::BuildFailed {
                still_running: true,
            } => "build failed (above); the previous build is still running".to_string(),
            Note::BuildFailed {
                still_running: false,
            } => "build failed (above); nothing is running".to_string(),
            Note::Started(pid) => format!("started (pid {pid})"),
            Note::Stopping(pid) => format!("stopping pid {pid} for the new build"),
            Note::Killed(pid) => format!(
                "pid {pid} didn't stop within {}s; killed it",
                STOP_WAIT.as_secs()
            ),
            Note::Exited(how) => format!("the tree exited ({how}); waiting for a change"),
            Note::Done => "stopped".to_string(),
        }
    }
}

/// How `bonsai dev` runs.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Run `bonsai sync` on every change.
    pub sync: bool,
    /// Arguments for the program.
    pub args: Vec<String>,
    /// What builds it: `cargo`, unless a test says otherwise.
    pub cargo: Option<PathBuf>,
}

/// A process `bonsai dev` started as the leader of its own process group,
/// and everything it starts. The group is signalled only while its leader
/// is ours and not yet reaped (its exit is seen without reaping it), so its
/// id can't have passed to anyone else's processes. A descendant that leaves
/// the group (`setsid`, a daemon) is out of reach.
struct Group {
    child: Child,
}

impl Group {
    /// Start `cmd` as a new group's leader. On Linux it also gets SIGTERM if
    /// `bonsai dev` itself dies (its own children don't: see `stop`).
    fn spawn(cmd: &mut Command) -> std::io::Result<Group> {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
        #[cfg(target_os = "linux")]
        {
            // SAFETY: getpid is async-signal-safe.
            let parent = unsafe { libc::getpid() };
            // SAFETY: prctl and getppid are async-signal-safe, and nothing
            // else runs between fork and exec.
            unsafe {
                cmd.pre_exec(move || {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                    // Already orphaned before the prctl took: don't start.
                    if libc::getppid() != parent {
                        return Err(std::io::Error::other("bonsai dev has gone"));
                    }
                    Ok(())
                });
            }
        }
        Ok(Group {
            child: cmd.spawn()?,
        })
    }

    /// Whether the leader has exited (not reaped: its id stays ours).
    fn leader_exited(&self) -> bool {
        // SAFETY: waitid on our own child, with WNOWAIT (it stays a zombie).
        unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let found = libc::waitid(
                libc::P_PID,
                self.child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            );
            found == 0 && info.si_pid() != 0
        }
    }

    /// Send `signal` to every process in the group.
    fn signal(&self, signal: i32) {
        // SAFETY: killpg on the group whose leader we hold unreaped.
        unsafe {
            libc::killpg(self.child.id() as i32, signal);
        }
    }

    /// Whether anything in the group is still running (a zombie isn't).
    fn alive(&self) -> bool {
        !members(self.child.id()).is_empty()
    }

    /// Stop the whole group: `first` (SIGTERM, or SIGINT for Ctrl-C), then,
    /// whatever's left after `grace`, SIGKILL; then reap the leader. Returns
    /// whether it took SIGKILL, and how the leader ended.
    fn stop(mut self, first: i32, grace: Duration) -> (bool, Option<std::process::ExitStatus>) {
        self.signal(first);
        let until = Instant::now() + grace;
        while self.alive() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let killed = self.alive();
        if killed {
            self.signal(libc::SIGKILL);
            let until = Instant::now() + Duration::from_secs(2);
            while self.alive() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        (killed, self.child.wait().ok())
    }
}

/// The processes in process group `pgid` that are still running (zombies
/// left out), from /proc.
fn members(pgid: u32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                return false;
            };
            // `pid (comm) state ppid pgrp …`; comm may hold spaces and parens.
            let Some(rest) = stat.rfind(')').map(|at| &stat[at + 1..]) else {
                return false;
            };
            let fields: Vec<&str> = rest.split_whitespace().collect();
            fields.len() > 2 && fields[0] != "Z" && fields[2] == pgid.to_string()
        })
        .collect()
}

/// The `cargo build` arguments for this computer: a Pi tree builds for the
/// host (as `cargo local` does), a host tree as it is.
fn build_args(root: &Path) -> Vec<String> {
    let mut args = vec![
        "build".to_string(),
        "--message-format=json-render-diagnostics".to_string(),
    ];
    let config = std::fs::read_to_string(root.join(".cargo/config.toml")).unwrap_or_default();
    if crate::parse_target(&config).is_some() {
        args.extend(["--target".to_string(), "host-tuple".to_string()]);
    }
    args
}

/// How a build ended.
#[derive(Debug, PartialEq)]
enum Built {
    Program(PathBuf),
    Failed,
    /// `stop` was set: the build was stopped, and everything it started.
    Stopped,
}

/// Build the tree in its own process group. Cargo's diagnostics go to
/// stderr as they come; its JSON (stdout) is read on a thread of its own,
/// so a build that stalls, or leaves something holding its stdout open,
/// can't keep `stop` from being seen. When `stop` is set the group is
/// stopped (SIGTERM, then SIGKILL after `BUILD_STOP_WAIT`) and reaped.
fn build(root: &Path, cargo: &Path, stop: &AtomicBool) -> Built {
    use std::io::BufRead;
    let mut cmd = Command::new(cargo);
    cmd.args(build_args(root))
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let Ok(mut group) = Group::spawn(&mut cmd) else {
        return Built::Failed;
    };
    let (lines, out_lines) = std::sync::mpsc::channel::<String>();
    if let Some(stdout) = group.child.stdout.take() {
        // Ends when every writer of the pipe has gone; nobody waits for it.
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                if lines.send(line).is_err() {
                    return;
                }
            }
        });
    }
    let mut out = String::new();
    loop {
        while let Ok(line) = out_lines.try_recv() {
            out += &line;
            out.push('\n');
        }
        if stop.load(Ordering::SeqCst) {
            group.stop(libc::SIGTERM, BUILD_STOP_WAIT);
            return Built::Stopped;
        }
        if group.leader_exited() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Cargo is done: anything it left in its group goes, then the rest of
    // its output (the pipe closes with them; a moment at most otherwise).
    let (_, status) = group.stop(libc::SIGTERM, BUILD_STOP_WAIT);
    let until = Instant::now() + Duration::from_secs(1);
    while let Ok(line) = out_lines.recv_timeout(until.saturating_duration_since(Instant::now())) {
        out += &line;
        out.push('\n');
    }
    if !status.is_some_and(|s| s.success()) {
        return Built::Failed;
    }
    executable(&out).map_or(Built::Failed, Built::Program)
}

/// Start the program in its own process group (so Ctrl-C reaches `bonsai
/// dev`, which stops it in order) that ends with `bonsai dev` on Linux.
fn start(program: &Path, root: &Path, args: &[String]) -> std::io::Result<Child> {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(root).process_group(0);
    #[cfg(target_os = "linux")]
    // SAFETY: prctl is async-signal-safe, and nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd.spawn()
}

/// Ask `child` to stop with `signal`; kill it after `STOP_WAIT`. True when
/// it had to be killed.
fn stop_child(child: &mut Child, signal: i32) -> bool {
    // SAFETY: kill on a pid we started and haven't reaped.
    unsafe {
        libc::kill(child.id() as i32, signal);
    }
    let until = Instant::now() + STOP_WAIT;
    while Instant::now() < until {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    true
}

/// Wait for the sources to change from `base` and then settle; the new
/// snapshot, or None when told to stop. Meanwhile, a program that ends by
/// itself is reported.
fn next_change(
    root: &Path,
    base: &Snapshot,
    running: &mut Option<Child>,
    stop: &AtomicBool,
    note: &mut dyn FnMut(Note),
) -> Option<Snapshot> {
    loop {
        if stop.load(Ordering::SeqCst) {
            return None;
        }
        if let Some(child) = running
            && let Ok(Some(status)) = child.try_wait()
        {
            note(Note::Exited(status.to_string()));
            *running = None;
        }
        let now = snapshot(root);
        if now != *base {
            // Let a burst of edits (a save-all, a formatter) finish.
            let mut last = now;
            let mut quiet_since = Instant::now();
            while quiet_since.elapsed() < SETTLE {
                if stop.load(Ordering::SeqCst) {
                    return None;
                }
                std::thread::sleep(POLL.min(SETTLE / 3));
                let again = snapshot(root);
                if again != last {
                    last = again;
                    quiet_since = Instant::now();
                }
            }
            return Some(last);
        }
        std::thread::sleep(POLL);
    }
}

/// The loop: build and run, then rebuild and restart on every change, until
/// `stop` is set. `note` hears what happens.
pub fn run(root: &Path, opts: &Options, stop: &AtomicBool, note: &mut dyn FnMut(Note)) {
    let mut running: Option<Child> = None;
    let mut base = snapshot(root);
    note(Note::Watching);
    let mut first = true;
    loop {
        if !first {
            match next_change(root, &base, &mut running, stop, note) {
                None => break,
                Some(now) => {
                    note(Note::Changed(changed(&base, &now)));
                    base = now;
                }
            }
        }
        first = false;
        // The graph first: errors, then generated files. With --sync, under
        // the tree's lock from planning to writing; without, read under a
        // shared one, so no other command's change is seen half made.
        let wait = crate::lock::wait();
        let held = if opts.sync {
            crate::lock::acquire_telling(root, "dev --sync", wait, &mut |who| {
                note(Note::Waiting(who.to_string()))
            })
            .map(Some)
        } else {
            Ok(None)
        };
        let planned = match &held {
            Ok(Some(_)) => Ok(crate::sync::plan(root)),
            Ok(None) => crate::lock::read_consistent(root, wait, || crate::sync::plan(root)),
            Err(e) => Err(e.clone()),
        };
        let planned = match planned {
            Ok(planned) => planned,
            Err(why) => {
                note(Note::Busy(why));
                continue;
            }
        };
        match planned {
            Err(crate::sync::Refused::Config(e)) => {
                note(Note::Refused(vec![e]));
                continue;
            }
            Err(crate::sync::Refused::Errors(errors)) => {
                note(Note::Refused(errors));
                continue;
            }
            Ok(plan) if !plan.changes.is_empty() => {
                if !opts.sync {
                    note(Note::Stale(
                        plan.changes.iter().map(|c| c.path.clone()).collect(),
                    ));
                    continue;
                }
                let Ok(Some(lock)) = &held else {
                    continue; // --sync holds the lock (above)
                };
                if let Err(e) = crate::sync::apply(root, &plan, lock) {
                    note(Note::Refused(vec![e.to_string()]));
                    continue;
                }
                note(Note::Synced(crate::sync::summary(&plan.changes)));
                // What sync wrote isn't a change to react to.
                base = snapshot(root);
            }
            Ok(_) => {}
        }
        drop(held);
        note(Note::Building);
        let cargo = opts.cargo.clone().unwrap_or_else(|| PathBuf::from("cargo"));
        let program = match build(root, &cargo, stop) {
            Built::Stopped => break,
            Built::Failed => {
                note(Note::BuildFailed {
                    still_running: running.is_some(),
                });
                continue;
            }
            Built::Program(program) => program,
        };
        if stop.load(Ordering::SeqCst) {
            break;
        }
        // Edited again while building: build that instead of starting this.
        if snapshot(root) != base {
            continue;
        }
        if let Some(mut old) = running.take() {
            note(Note::Stopping(old.id()));
            if stop_child(&mut old, libc::SIGTERM) {
                note(Note::Killed(old.id()));
            }
        }
        match start(&program, root, &opts.args) {
            Ok(child) => {
                note(Note::Started(child.id()));
                running = Some(child);
            }
            Err(e) => note(Note::Exited(format!(
                "couldn't start {}: {e}",
                program.display()
            ))),
        }
    }
    if let Some(mut child) = running.take()
        && stop_child(&mut child, libc::SIGINT)
    {
        note(Note::Killed(child.id()));
    }
    note(Note::Done);
}

/// Set by SIGINT/SIGTERM.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// `bonsai dev [--sync] [-- <args for the tree>]`.
pub fn dev(args: &[String]) -> ! {
    let mut opts = Options::default();
    let mut rest = args.iter();
    while let Some(a) = rest.next() {
        match a.as_str() {
            "--sync" => opts.sync = true,
            "--" => {
                opts.args = rest.by_ref().cloned().collect();
            }
            _ => {
                eprintln!("usage: bonsai dev [--sync] [-- <args for the tree>]");
                std::process::exit(2);
            }
        }
    }
    crate::tree::require_tree_unchanged("dev");
    // SAFETY: the handler only stores to an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
    run(Path::new("."), &opts, &STOP, &mut |n| {
        eprintln!("bonsai dev: {}", n.text());
    });
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sources_are_watched() {
        for p in [
            "bonsai.toml",
            "Cargo.toml",
            ".cargo/config.toml",
            "build.rs",
            "src/main.rs",
            "src/branches/sensor.rs",
        ] {
            assert!(watched(Path::new(p)), "{p}");
        }
        for p in [
            "Cargo.lock",
            "target/debug/tree",
            "logs/2026-10-01_10-00-00/events.log",
            "src/branches/.sensor.rs.swp",
            "src/.links.rs.bonsai-new",
            "src/notes.txt",
            ".bonsai-sync",
            "README.md",
        ] {
            assert!(!watched(Path::new(p)), "{p}");
        }
    }

    #[test]
    fn output_folders_are_never_walked() {
        let root = std::env::temp_dir().join(format!("bonsai-dev-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["src/branches", "target/debug", "logs/run"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("bonsai.toml"), "").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        let before = snapshot(&root);
        std::fs::write(root.join("target/debug/tree"), "binary").unwrap();
        std::fs::write(root.join("logs/run/events.log"), "START").unwrap();
        std::fs::write(root.join("Cargo.lock"), "").unwrap();
        assert_eq!(changed(&before, &snapshot(&root)), Vec::<PathBuf>::new());
        std::fs::write(root.join("src/branches/sensor.rs"), "// new").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() { }").unwrap();
        assert_eq!(
            changed(&before, &snapshot(&root)),
            [
                PathBuf::from("src/branches/sensor.rs"),
                PathBuf::from("src/main.rs")
            ]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_program_is_the_last_binary_cargo_built() {
        let json = "{\"reason\":\"compiler-artifact\",\"target\":{\"kind\":[\"lib\"]},\"executable\":null}\n\
                    {\"reason\":\"compiler-artifact\",\"target\":{\"kind\":[\"bin\"]},\"executable\":\"/t/target/debug/greenhouse\",\"fresh\":false}\n\
                    {\"reason\":\"build-finished\",\"success\":true}\n";
        assert_eq!(
            executable(json),
            Some(PathBuf::from("/t/target/debug/greenhouse"))
        );
        assert_eq!(
            executable("{\"reason\":\"build-finished\",\"success\":false}\n"),
            None
        );
    }

    /// A host tree in target/dev-loop/tree (its own target/ kept between
    /// runs, so only the first build is slow).
    fn render_tree() -> PathBuf {
        fn copy(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for e in std::fs::read_dir(from).unwrap().flatten() {
                let path = e.path();
                let target = to.join(e.file_name());
                if path.is_dir() {
                    copy(&path, &target);
                } else {
                    let text = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(target, text.replace("{{project-name}}", "dev_loop_tree"))
                        .unwrap();
                }
            }
        }
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = repo.join("target/dev-loop/tree");
        for old in ["src", "logs", "bonsai.toml", "Cargo.toml", ".cargo"] {
            let _ = std::fs::remove_dir_all(root.join(old));
            let _ = std::fs::remove_file(root.join(old));
        }
        copy(&repo.join("templates/linux/host"), &root);
        root
    }

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks the pid exists.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    /// The whole loop on a real tree: a first build and start, output
    /// folders and the tree's own logs ignored, a compile error that keeps
    /// the old program running, a fix that replaces it, a burst of edits
    /// taken as one, stale generated code not built, and a clean stop.
    #[test]
    #[ignore = "builds a real tree: cargo test -- --ignored dev_loop"]
    fn dev_loop_end_to_end() {
        use std::sync::{Arc, Mutex};
        let root = render_tree();
        let notes: Arc<Mutex<Vec<Note>>> = Arc::default();
        let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let (r, n) = (root.clone(), notes.clone());
        let looping = std::thread::spawn(move || {
            run(&r, &Options::default(), stop, &mut |note| {
                eprintln!("bonsai dev: {}", note.text());
                n.lock().unwrap().push(note);
            })
        });
        let seen = |from: usize| notes.lock().unwrap()[from..].to_vec();
        let wait_for = |from: usize, within: Duration, what: &str, f: &dyn Fn(&Note) -> bool| {
            let until = Instant::now() + within;
            loop {
                if let Some(n) = seen(from).into_iter().find(|n| f(n)) {
                    return n;
                }
                assert!(Instant::now() < until, "no {what} in {:?}", seen(from));
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let started = |n: &Note| matches!(n, Note::Started(_));
        let pid_of = |n: Note| match n {
            Note::Started(pid) => pid,
            other => panic!("{other:?}"),
        };
        let main_rs = root.join("src/main.rs");
        let main = std::fs::read_to_string(&main_rs).unwrap();

        // Built and started.
        let first = pid_of(wait_for(0, Duration::from_secs(600), "start", &started));
        assert!(alive(first));

        // The tree's run logs, a build's output, Cargo.lock: not changes.
        let mark = notes.lock().unwrap().len();
        std::fs::create_dir_all(root.join("target/extra")).unwrap();
        std::fs::write(root.join("target/extra/x"), "x").unwrap();
        std::fs::create_dir_all(root.join("logs/extra")).unwrap();
        std::fs::write(root.join("logs/extra/events.log"), "x").unwrap();
        std::fs::write(
            root.join("Cargo.lock"),
            std::fs::read(root.join("Cargo.lock")).unwrap_or_default(),
        )
        .unwrap();
        std::thread::sleep(SETTLE * 4);
        assert_eq!(seen(mark), [], "output files set off a rebuild");

        // A compile error: reported, and the running program is kept.
        let mark = notes.lock().unwrap().len();
        std::fs::write(
            &main_rs,
            format!("{main}\ncompile_error!(\"on purpose\");\n"),
        )
        .unwrap();
        let failed = wait_for(mark, Duration::from_secs(120), "build failure", &|n| {
            matches!(n, Note::BuildFailed { .. })
        });
        assert_eq!(
            failed,
            Note::BuildFailed {
                still_running: true
            }
        );
        assert!(alive(first), "a failed build stopped the running program");
        assert!(!seen(mark).iter().any(started));

        // Fixed: the old one is stopped, then the new one started.
        let mark = notes.lock().unwrap().len();
        std::fs::write(&main_rs, format!("{main}\n// fixed\n")).unwrap();
        let second = pid_of(wait_for(
            mark,
            Duration::from_secs(120),
            "restart",
            &started,
        ));
        let order = seen(mark);
        let stopping = order
            .iter()
            .position(|n| *n == Note::Stopping(first))
            .unwrap();
        let start = order
            .iter()
            .position(|n| *n == Note::Started(second))
            .unwrap();
        assert!(stopping < start, "{order:?}");
        assert!(
            second != first && alive(second) && !alive(first),
            "{order:?}"
        );

        // A burst of edits, each inside SETTLE of the last: one build.
        let mark = notes.lock().unwrap().len();
        for i in 0..5 {
            std::fs::write(&main_rs, format!("{main}\n// edit {i}\n")).unwrap();
            std::thread::sleep(SETTLE / 3);
        }
        let third = pid_of(wait_for(
            mark,
            Duration::from_secs(120),
            "restart",
            &started,
        ));
        std::thread::sleep(SETTLE * 4);
        let after = seen(mark);
        let builds = after.iter().filter(|n| **n == Note::Building).count();
        assert_eq!(builds, 1, "{after:?}");
        assert!(alive(third) && !alive(second));

        // Generated code out of date, and no --sync: not built.
        let mark = notes.lock().unwrap().len();
        let toml = std::fs::read_to_string(root.join("bonsai.toml")).unwrap();
        std::fs::write(
            root.join("bonsai.toml"),
            toml.replace("errors = false", "errors = true"),
        )
        .unwrap();
        let stale = wait_for(mark, Duration::from_secs(30), "stale", &|n| {
            matches!(n, Note::Stale(_))
        });
        assert_eq!(stale, Note::Stale(vec!["src/links.rs".to_string()]));
        std::thread::sleep(SETTLE * 2);
        assert!(!seen(mark).contains(&Note::Building));
        assert!(alive(third));
        std::fs::write(root.join("bonsai.toml"), toml).unwrap();

        // Stop: the program ends, then the loop.
        stop.store(true, Ordering::SeqCst);
        looping.join().unwrap();
        assert!(!alive(third), "the program outlived bonsai dev");
        assert_eq!(notes.lock().unwrap().last(), Some(&Note::Done));
        std::fs::write(&main_rs, main).unwrap();
    }

    // -----------------------------------------------------------------
    // A fake cargo and a fake program, to drive the loop's processes.
    // -----------------------------------------------------------------

    /// What the fake cargo does is in `<fake>/mode`: `stall` (ignores
    /// SIGTERM, and leaves a child that ignores it too holding its stdout
    /// open), `fail`, or anything else to build `<fake>/app.sh`.
    const FAKE_CARGO: &str = r#"#!/bin/sh
D="$(dirname "$0")"
echo $$ > "$D/build.pid"
case "$(cat "$D/mode")" in
stall)
    trap '' TERM
    (trap '' TERM; exec sleep 1000) &
    echo $! > "$D/build-child.pid"
    echo started > "$D/building"
    while :; do sleep 1; done ;;
fail)
    echo "error: on purpose" >&2
    exit 101 ;;
*)
    printf '{"reason":"compiler-artifact","target":{"kind":["bin"]},"executable":"%s"}\n' "$D/app.sh" ;;
esac
"#;

    /// The program: each run is generation n. It leaves a child that ignores
    /// SIGTERM and SIGINT, says whether the last generation's child was
    /// still running when it started, and exits by itself once `<fake>/exit<n>`
    /// exists.
    const FAKE_APP: &str = r#"#!/bin/sh
D="$(dirname "$0")"
n=$(( $(cat "$D/gen" 2>/dev/null || echo 0) + 1 ))
echo $n > "$D/gen"
echo $$ > "$D/app$n.pid"
(trap '' TERM INT; exec sleep 1000) &
echo $! > "$D/app$n-child.pid"
p=$(cat "$D/app$((n - 1))-child.pid" 2>/dev/null)
st=$(awk '{print $3}' "/proc/$p/stat" 2>/dev/null)
if [ -n "$p" ] && [ -n "$st" ] && [ "$st" != Z ]; then echo alive; else echo gone; fi > "$D/app$n-saw-old"
trap 'exit 0' TERM INT
while [ ! -f "$D/exit$n" ]; do sleep 0.05; done
exit 3
"#;

    struct Fake {
        root: PathBuf,
        dir: PathBuf,
    }

    impl Fake {
        fn new(name: &str) -> Fake {
            use std::os::unix::fs::PermissionsExt;
            let root =
                std::env::temp_dir().join(format!("bonsai-dev-fake-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let tmpl = crate::TEMPLATES.get_dir("linux/host").unwrap();
            crate::extract_dir(tmpl, tmpl.path(), &root).unwrap();
            let dir = root.join("fake");
            std::fs::create_dir_all(&dir).unwrap();
            for (name, text) in [("cargo.sh", FAKE_CARGO), ("app.sh", FAKE_APP)] {
                std::fs::write(dir.join(name), text).unwrap();
                std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            std::fs::write(dir.join("mode"), "ok").unwrap();
            Fake { root, dir }
        }
        fn mode(&self, mode: &str) {
            std::fs::write(self.dir.join("mode"), mode).unwrap();
        }
        fn pid(&self, file: &str) -> u32 {
            let path = self.dir.join(file);
            wait_until(Duration::from_secs(20), file, || path.exists());
            let until = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(pid) = std::fs::read_to_string(&path)
                    .unwrap_or_default()
                    .trim()
                    .parse()
                {
                    return pid;
                }
                assert!(Instant::now() < until, "{file} holds no pid");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        /// An edit, for the loop to rebuild on.
        fn edit(&self, n: u32) {
            let main = self.root.join("src/main.rs");
            let text = std::fs::read_to_string(&main).unwrap();
            std::fs::write(&main, format!("{text}// edit {n}\n")).unwrap();
        }
        fn options(&self) -> Options {
            Options {
                cargo: Some(self.dir.join("cargo.sh")),
                ..Options::default()
            }
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// Whether `pid` is running (a zombie, or gone, isn't).
    fn running(pid: u32) -> bool {
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
            stat.rfind(')')
                .and_then(|at| stat[at + 1..].split_whitespace().next())
                .is_some_and(|state| state != "Z")
        })
    }

    fn wait_until(within: Duration, what: &str, done: impl Fn() -> bool) {
        let until = Instant::now() + within;
        while !done() {
            assert!(Instant::now() < until, "no {what} within {within:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The loop on its own thread, its notes, and its stop switch.
    struct Looping {
        notes: std::sync::Arc<std::sync::Mutex<Vec<Note>>>,
        stop: &'static AtomicBool,
        done: std::sync::mpsc::Receiver<()>,
    }

    fn start_loop(fake: &Fake) -> Looping {
        let notes: std::sync::Arc<std::sync::Mutex<Vec<Note>>> = Default::default();
        let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
        let (tx, done) = std::sync::mpsc::channel();
        let (root, opts, n) = (fake.root.clone(), fake.options(), notes.clone());
        std::thread::spawn(move || {
            run(&root, &opts, stop, &mut |note| n.lock().unwrap().push(note));
            let _ = tx.send(());
        });
        Looping { notes, stop, done }
    }

    impl Looping {
        fn saw(&self, what: &str, f: impl Fn(&Note) -> bool) {
            wait_until(Duration::from_secs(20), what, || {
                self.notes.lock().unwrap().iter().any(&f)
            });
        }
        fn count(&self, f: impl Fn(&Note) -> bool) -> usize {
            self.notes.lock().unwrap().iter().filter(|n| f(n)).count()
        }
        /// Stop it; how long it took to finish.
        fn stop(self) -> Duration {
            let asked = Instant::now();
            self.stop.store(true, Ordering::SeqCst);
            self.done
                .recv_timeout(Duration::from_secs(30))
                .expect("bonsai dev didn't stop");
            asked.elapsed()
        }
    }

    #[test]
    fn stopping_during_a_stalled_first_build_ends_it_and_all_it_started() {
        let fake = Fake::new("stall-first");
        fake.mode("stall");
        let looping = start_loop(&fake);
        let (build, child) = (fake.pid("build.pid"), fake.pid("build-child.pid"));
        wait_until(Duration::from_secs(20), "stalled build", || {
            fake.dir.join("building").exists()
        });
        let took = looping.stop();
        // Its SIGTERM is ignored: SIGKILL after BUILD_STOP_WAIT, and no longer.
        assert!(took < BUILD_STOP_WAIT + Duration::from_secs(3), "{took:?}");
        assert!(
            !running(build) && !running(child),
            "the build outlived bonsai dev"
        );
    }

    #[test]
    fn stopping_during_a_later_build_ends_the_build_and_the_program() {
        let fake = Fake::new("stall-later");
        let looping = start_loop(&fake);
        looping.saw("start", |n| matches!(n, Note::Started(_)));
        let app = fake.pid("app1.pid");
        fake.mode("stall");
        fake.edit(1);
        wait_until(Duration::from_secs(20), "stalled build", || {
            fake.dir.join("building").exists()
        });
        let (build, child) = (fake.pid("build.pid"), fake.pid("build-child.pid"));
        let took = looping.stop();
        assert!(
            took < BUILD_STOP_WAIT + STOP_WAIT + Duration::from_secs(3),
            "{took:?}"
        );
        assert!(
            !running(build) && !running(child),
            "the build outlived bonsai dev"
        );
        assert!(!running(app), "the program outlived bonsai dev");
    }

    #[test]
    fn stopping_between_builds_and_failed_builds_keep_the_program() {
        let fake = Fake::new("between");
        // Not ours: never signalled.
        let mut stranger = Command::new("sleep").arg("1000").spawn().unwrap();
        let looping = start_loop(&fake);
        looping.saw("start", |n| matches!(n, Note::Started(_)));
        let app = fake.pid("app1.pid");
        fake.mode("fail");
        fake.edit(1);
        looping.saw("build failure", |n| {
            *n == Note::BuildFailed {
                still_running: true,
            }
        });
        assert!(running(app), "a failed build stopped the program");
        assert_eq!(looping.count(|n| matches!(n, Note::Started(_))), 1);
        looping.stop();
        assert!(!running(app));
        assert!(
            running(stranger.id()),
            "a process bonsai dev didn't start was signalled"
        );
        stranger.kill().unwrap();
        stranger.wait().unwrap();
    }
}
