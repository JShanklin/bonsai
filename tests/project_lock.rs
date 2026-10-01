//! Commands that change a tree take turns (`src/lock.rs`): run the real
//! `bonsai` against a rendered host tree, overlapping them on purpose. What
//! overlaps is decided by the lock itself (held by the test, or by a sync
//! paused part way with `BONSAI_TEST_PAUSE_APPLY`), not by timing.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BONSAI: &str = env!("CARGO_BIN_EXE_bonsai");

/// A fresh host tree in its own folder.
fn tree(name: &str) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let path = e.path();
            let target = to.join(e.file_name());
            if path.is_dir() {
                copy(&path, &target);
            } else {
                let text = std::fs::read_to_string(&path).unwrap();
                std::fs::write(target, text.replace("{{project-name}}", "locked")).unwrap();
            }
        }
    }
    let root = std::env::temp_dir().join(format!("bonsai-lock-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("templates/linux/host"),
        &root,
    );
    root
}

fn bonsai(root: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(BONSAI);
    cmd.args(args)
        .current_dir(root)
        .env("BONSAI_LOCK_WAIT", "60")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn run(root: &Path, args: &[&str]) -> Output {
    bonsai(root, args).output().unwrap()
}

/// The tree's lock, held by the test (as another bonsai command would).
fn hold(root: &Path) -> File {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join(".bonsai.lock"))
        .unwrap();
    // SAFETY: flock on a descriptor we own.
    assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
    file
}

/// A command started now, and a way to know it's waiting for the lock: its
/// stderr is read as it comes, and `waiting` hears when it says so.
struct Started {
    child: Child,
    waiting: mpsc::Receiver<()>,
    stderr: std::thread::JoinHandle<String>,
}

fn start(root: &Path, args: &[&str]) -> Started {
    let mut child = bonsai(root, args).spawn().unwrap();
    let (tx, waiting) = mpsc::channel();
    let err = child.stderr.take().unwrap();
    let stderr = std::thread::spawn(move || {
        let mut all = String::new();
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            if line.starts_with("waiting for ") {
                let _ = tx.send(());
            }
            all += &line;
            all.push('\n');
        }
        all
    });
    Started {
        child,
        waiting,
        stderr,
    }
}

impl Started {
    fn is_waiting(&self) {
        self.waiting
            .recv_timeout(Duration::from_secs(30))
            .expect("the command never said it was waiting for the lock");
    }
    fn finish(mut self) -> (bool, String) {
        let ok = self.child.wait().unwrap().success();
        (ok, self.stderr.join().unwrap())
    }
}

fn read(root: &Path, file: &str) -> String {
    std::fs::read_to_string(root.join(file)).unwrap_or_default()
}

fn await_file(path: &Path) {
    let until = Instant::now() + Duration::from_secs(60);
    while !path.exists() {
        assert!(Instant::now() < until, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn commands_that_overlap_take_turns_and_lose_nothing() {
    let root = tree("overlap");
    let toml = read(&root, "bonsai.toml");
    let held = hold(&root);
    let a = start(&root, &["branch", "add", "alpha"]);
    let b = start(&root, &["branch", "add", "beta"]);
    let sync = start(&root, &["sync"]);
    for c in [&a, &b, &sync] {
        c.is_waiting();
    }
    // Nothing was read and changed under the holder.
    assert_eq!(read(&root, "bonsai.toml"), toml);
    assert!(!root.join("src/branches/alpha.rs").exists());
    drop(held);
    for c in [a, b, sync] {
        let (ok, err) = c.finish();
        assert!(ok, "{err}");
    }
    let toml = read(&root, "bonsai.toml");
    assert!(
        toml.contains("[branch.alpha]") && toml.contains("[branch.beta]"),
        "{toml}"
    );
    let mods = read(&root, "src/branches/mod.rs");
    assert!(
        mods.contains("pub mod alpha;") && mods.contains("pub mod beta;"),
        "{mods}"
    );
    let doctor = run(&root, &["doctor"]);
    assert!(
        doctor.status.success(),
        "{}",
        String::from_utf8_lossy(&doctor.stdout)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_graph_edit_waits_for_a_sync_part_way_through() {
    let root = tree("mid-sync");
    let pause = root.join("pause");
    // Something for the sync to write: a branch added by hand.
    run(&root, &["branch", "add", "sensor"]);
    let toml = read(&root, "bonsai.toml");
    std::fs::write(
        root.join("bonsai.toml"),
        toml.replace("[branch.sensor]", "[branch.sensor]\nrate = 2"),
    )
    .unwrap();
    let mut sync = bonsai(&root, &["sync"]);
    sync.env("BONSAI_TEST_PAUSE_APPLY", &pause);
    let sync = sync.spawn().unwrap();
    await_file(&pause.with_extension("paused"));
    // The sync holds the lock, part way: the edit waits for it.
    let edit = start(&root, &["branch", "add", "display"]);
    edit.is_waiting();
    std::fs::write(pause.with_extension("go"), "").unwrap();
    let synced = sync.wait_with_output().unwrap();
    assert!(
        synced.status.success(),
        "{}",
        String::from_utf8_lossy(&synced.stderr)
    );
    let (ok, err) = edit.finish();
    assert!(ok, "{err}");
    // Both changes are there: the sync's rate and the edit's branch.
    assert!(read(&root, "src/links.rs").contains("Tick"));
    assert!(read(&root, "bonsai.toml").contains("[branch.display]"));
    assert!(read(&root, "src/branches/mod.rs").contains("pub mod display;"));
    assert!(!root.join(".bonsai-sync").exists());
    let leftovers: Vec<_> = std::fs::read_dir(root.join("src"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".bonsai-new"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_killed_sync_lets_go_of_the_tree_and_the_next_one_finishes_it() {
    let root = tree("killed");
    run(&root, &["branch", "add", "sensor"]);
    let toml = read(&root, "bonsai.toml");
    std::fs::write(
        root.join("bonsai.toml"),
        toml.replace("[branch.sensor]", "[branch.sensor]\nrate = 2"),
    )
    .unwrap();
    let pause = root.join("pause");
    let mut sync = bonsai(&root, &["sync"]);
    sync.env("BONSAI_TEST_PAUSE_APPLY", &pause);
    let mut sync = sync.spawn().unwrap();
    await_file(&pause.with_extension("paused"));
    sync.kill().unwrap();
    sync.wait().unwrap();
    assert!(
        root.join(".bonsai-sync").exists(),
        "the interrupted sync left no mark"
    );
    // Unlocked straight away (no wait), and reported as unfinished.
    let mut doctor = bonsai(&root, &["doctor"]);
    doctor.env("BONSAI_LOCK_WAIT", "0");
    let doctor = doctor.output().unwrap();
    let report = String::from_utf8_lossy(&doctor.stdout);
    assert_eq!(doctor.status.code(), Some(1), "{report}");
    assert!(report.contains("a `bonsai sync` didn't finish"), "{report}");
    let mut finish = bonsai(&root, &["sync"]);
    finish.env("BONSAI_LOCK_WAIT", "0");
    let finish = finish.output().unwrap();
    let said = String::from_utf8_lossy(&finish.stdout);
    assert!(
        finish.status.success() && said.contains("finishing a sync that didn't finish"),
        "{said}"
    );
    assert!(!root.join(".bonsai-sync").exists());
    assert!(run(&root, &["doctor"]).status.success());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_command_that_cant_get_the_lock_in_time_changes_nothing_and_says_why() {
    let root = tree("contended");
    let toml = read(&root, "bonsai.toml");
    let _held = hold(&root);
    let started = Instant::now();
    let mut add = bonsai(&root, &["branch", "add", "late"]);
    add.env("BONSAI_LOCK_WAIT", "0.3");
    let add = add.output().unwrap();
    let err = String::from_utf8_lossy(&add.stderr);
    assert_eq!(add.status.code(), Some(1), "{err}");
    assert!(
        err.contains("is changing this tree; nothing was changed"),
        "{err}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(read(&root, "bonsai.toml"), toml);
    assert!(!root.join("src/branches/late.rs").exists());
    // Readers say they couldn't get a consistent look, and write nothing.
    let mut dry = bonsai(&root, &["sync", "--dry-run"]);
    dry.env("BONSAI_LOCK_WAIT", "0.3");
    let dry = dry.output().unwrap();
    assert_eq!(dry.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&dry.stderr).contains("is changing this tree"));
    let mut doctor = bonsai(&root, &["doctor", "--json"]);
    doctor.env("BONSAI_LOCK_WAIT", "0.3");
    let json = String::from_utf8_lossy(&doctor.output().unwrap().stdout).to_string();
    assert!(
        json.contains("\"status\":\"skipped\"") && json.contains("is changing this tree"),
        "{json}"
    );
    assert_eq!(read(&root, "bonsai.toml"), toml);
    let _ = std::fs::remove_dir_all(&root);
}

/// Every file in the tree but the lock (which a command may create) and
/// build output, with its bytes.
fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, root: &Path, out: &mut std::collections::BTreeMap<PathBuf, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let path = e.path();
            let rel = path.strip_prefix(root).unwrap().to_path_buf();
            if rel == Path::new(".bonsai.lock") || rel == Path::new("target") {
                continue;
            }
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A tree as an older bonsai left it: a link from `a` to `b`, written as a
/// `[[wire]]` table (the comments that mention `[[link]]` stay as they are).
fn legacy_tree(name: &str) -> PathBuf {
    let root = tree(name);
    for args in [
        &["branch", "add", "a"][..],
        &["branch", "add", "b"],
        &["message", "add", "Ping"],
        &["link", "a", "Ping", "b"],
    ] {
        assert!(run(&root, args).status.success(), "{args:?}");
    }
    let toml = read(&root, "bonsai.toml");
    let legacy: String = toml
        .split_inclusive('\n')
        .map(|l| {
            if l.trim() == "[[link]]" {
                l.replacen("[[link]]", "[[wire]]", 1)
            } else {
                l.to_string()
            }
        })
        .collect();
    assert_ne!(legacy, toml);
    std::fs::write(root.join("bonsai.toml"), legacy).unwrap();
    root
}

#[test]
fn a_refused_sync_leaves_a_legacy_tree_byte_for_byte() {
    let root = legacy_tree("legacy-refused");
    // And a graph error: a link carrying a message that doesn't exist.
    let mut toml = read(&root, "bonsai.toml");
    toml.push_str("\n[[wire]]\nfrom = \"a\"\nmessage = \"Nope\"\nto = [\"b\"]\n");
    std::fs::write(root.join("bonsai.toml"), toml).unwrap();
    let before = snapshot(&root);
    let sync = run(&root, &["sync"]);
    let err = String::from_utf8_lossy(&sync.stderr);
    assert_eq!(sync.status.code(), Some(1), "{err}");
    assert!(err.contains("no message `Nope`"), "{err}");
    assert!(
        !String::from_utf8_lossy(&sync.stdout).contains("[[link]] now"),
        "said it migrated"
    );
    assert!(!root.join(".bonsai-sync").exists(), "a journal was made");
    assert!(snapshot(&root) == before, "a refused sync changed the tree");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_legacy_tree_migrates_as_previewed() {
    let root = legacy_tree("legacy-preview");
    let legacy = read(&root, "bonsai.toml");
    let before = snapshot(&root);
    let dry = run(&root, &["sync", "--dry-run"]);
    let preview = String::from_utf8_lossy(&dry.stdout).to_string();
    assert!(dry.status.success(), "{preview}");
    assert!(
        preview.contains("changed  bonsai.toml")
            && preview.contains("-[[wire]]")
            && preview.contains("+[[link]]"),
        "{preview}"
    );
    assert!(snapshot(&root) == before, "a dry run changed the tree");
    let sync = run(&root, &["sync"]);
    let said = String::from_utf8_lossy(&sync.stdout);
    assert!(sync.status.success(), "{said}");
    assert!(said.contains("[[wire]] tables are [[link]] now"), "{said}");
    // What the preview showed, and nothing else: the headers renamed.
    let migrated: String = legacy
        .split_inclusive('\n')
        .map(|l| {
            if l.trim() == "[[wire]]" {
                l.replacen("[[wire]]", "[[link]]", 1)
            } else {
                l.to_string()
            }
        })
        .collect();
    assert_eq!(read(&root, "bonsai.toml"), migrated);
    let after = snapshot(&root);
    let changed: Vec<&PathBuf> = after
        .keys()
        .filter(|p| before.get(*p) != after.get(*p))
        .collect();
    assert_eq!(changed, [Path::new("bonsai.toml")], "{preview}");
    assert!(!root.join(".bonsai-sync").exists());
    let again = run(&root, &["sync", "--dry-run"]);
    assert!(
        String::from_utf8_lossy(&again.stdout).contains("in step"),
        "{}",
        String::from_utf8_lossy(&again.stdout)
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn an_interrupted_migration_is_finished_by_the_next_sync() {
    let root = legacy_tree("legacy-interrupted");
    // A second change besides the migration, so it stops part way.
    let toml = read(&root, "bonsai.toml");
    std::fs::write(
        root.join("bonsai.toml"),
        toml.replace("[branch.a]", "[branch.a]\nrate = 2"),
    )
    .unwrap();
    let pause = root.join("pause");
    let mut sync = bonsai(&root, &["sync"]);
    sync.env("BONSAI_TEST_PAUSE_APPLY", &pause);
    let mut sync = sync.spawn().unwrap();
    await_file(&pause.with_extension("paused"));
    sync.kill().unwrap();
    sync.wait().unwrap();
    assert!(
        root.join(".bonsai-sync").exists(),
        "the interrupted sync left no mark"
    );
    let finish = run(&root, &["sync"]);
    let said = String::from_utf8_lossy(&finish.stdout);
    assert!(
        finish.status.success() && said.contains("finishing a sync that didn't finish"),
        "{said}{}",
        String::from_utf8_lossy(&finish.stderr)
    );
    assert!(!root.join(".bonsai-sync").exists());
    let toml = read(&root, "bonsai.toml");
    assert!(!toml.lines().any(|l| l.trim() == "[[wire]]"), "{toml}");
    assert!(read(&root, "src/links.rs").contains("Tick"));
    assert!(run(&root, &["doctor"]).status.success());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_modern_tree_still_syncs_as_before() {
    let root = tree("modern");
    assert!(run(&root, &["branch", "add", "sensor"]).status.success());
    let toml = read(&root, "bonsai.toml");
    std::fs::write(
        root.join("bonsai.toml"),
        toml.replace("[branch.sensor]", "[branch.sensor]\nrate = 2"),
    )
    .unwrap();
    let sync = run(&root, &["sync"]);
    let said = String::from_utf8_lossy(&sync.stdout);
    assert!(sync.status.success(), "{said}");
    assert!(
        said.contains("updated") && !said.contains("[[wire]]"),
        "{said}"
    );
    assert!(read(&root, "src/links.rs").contains("Tick"));
    assert!(read(&root, "bonsai.toml").contains("[branch.sensor]\nrate = 2"));
    assert!(!root.join(".bonsai-sync").exists());
    let _ = std::fs::remove_dir_all(&root);
}
