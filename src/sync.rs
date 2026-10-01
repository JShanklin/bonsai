//! What `bonsai sync` changes, worked out before anything is written: a
//! [`Plan`] of files to create, change or remove, read-only to make. `bonsai
//! sync` applies it, `bonsai sync --dry-run` prints it, and `bonsai doctor`
//! reads it to find generated files that are out of date.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::graph::{self, Config};
use crate::tree::{
    BRANCHES_MOD, CONFIG, EDGES_MOD, LIBC, LINKS_RS, MESSAGES, OLD_WIRING_RS, RUNTIME, RUNTIME_RS,
    SERIAL, SERIAL_RS, SETTINGS_RS, TOKIO_SERIAL,
};

/// One file a sync would write or remove.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// Relative to the tree.
    pub path: String,
    /// What's there now; None when it doesn't exist.
    pub before: Option<String>,
    /// What it would be; None to remove it.
    pub after: Option<String>,
    /// How it's listed after `updated` (None: not listed on its own, as
    /// files a migration touches, summed up by another change's label).
    pub label: Option<String>,
}

impl Change {
    /// `created`, `changed` or `removed`.
    pub fn kind(&self) -> &'static str {
        match (&self.before, &self.after) {
            (None, _) => "created",
            (_, None) => "removed",
            _ => "changed",
        }
    }
}

/// Everything a sync would change, in the order it would apply it, and the
/// graph's warnings.
#[derive(Debug, Default)]
pub struct Plan {
    pub changes: Vec<Change>,
    pub warnings: Vec<String>,
}

/// Why a tree can't be synced: nothing would be generated.
#[derive(Debug, PartialEq)]
pub enum Refused {
    /// bonsai.toml can't be read or parsed.
    Config(String),
    /// The graph's errors, and branch or custom edge files missing.
    Errors(Vec<String>),
}

/// Files read from the tree, and what the plan would make of them.
struct Draft<'a> {
    root: &'a Path,
    files: BTreeMap<String, Change>,
    order: Vec<String>,
}

impl<'a> Draft<'a> {
    fn on_disk(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.root.join(path)).ok()
    }

    /// The file as the plan leaves it so far.
    fn current(&self, path: &str) -> Option<String> {
        match self.files.get(path) {
            Some(change) => change.after.clone(),
            None => self.on_disk(path),
        }
    }

    fn set(&mut self, path: &str, after: Option<String>, label: Option<String>) {
        let before = self.on_disk(path);
        let change = self.files.entry(path.to_string()).or_insert_with(|| {
            self.order.push(path.to_string());
            Change {
                path: path.to_string(),
                before,
                after: None,
                label: None,
            }
        });
        change.after = after;
        if label.is_some() {
            change.label = label;
        }
    }

    /// Write `content` to `path` when it differs from what's there.
    fn write(&mut self, path: &str, content: String) {
        if self.current(path).as_deref() != Some(content.as_str()) {
            self.set(path, Some(content), Some(path.to_string()));
        }
    }

    fn finish(mut self) -> Vec<Change> {
        self.order
            .iter()
            .filter_map(|p| self.files.remove(p))
            .filter(|c| c.before != c.after)
            .collect()
    }
}

fn rust_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            out.extend(rust_files(&path)?);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// The tree's graph, read from `root` (an older tree's `[[wire]]` tables
/// read as `[[link]]`), and the migrated text when it had any.
pub fn read_config(root: &Path) -> Result<(Config, Option<String>), String> {
    let src = std::fs::read_to_string(root.join(CONFIG))
        .map_err(|e| format!("can't read {CONFIG}: {e}"))?;
    let migrated = crate::tree::with_links(&src);
    let cfg = graph::parse(migrated.as_deref().unwrap_or(&src))?;
    Ok((cfg, migrated))
}

/// The message names in the tree's `src/messages.rs`.
pub fn read_messages(root: &Path) -> Vec<String> {
    graph::parse_messages(&std::fs::read_to_string(root.join(MESSAGES)).unwrap_or_default())
}

/// What's missing for the graph to build: a branch's file, a custom edge's.
pub fn missing_sources(root: &Path, cfg: &Config) -> Vec<String> {
    let mut missing = Vec::new();
    for b in &cfg.branches {
        if !root.join(format!("src/branches/{}.rs", b.name)).is_file() {
            missing.push(format!(
                "[branch.{0}] has no src/branches/{0}.rs (`bonsai branch add {0}` makes one)",
                b.name
            ));
        }
    }
    for e in &cfg.edges {
        if matches!(e.kind, graph::EdgeKind::Custom { .. })
            && !root.join(format!("src/edges/{}.rs", e.name)).is_file()
        {
            missing.push(format!(
                "[edge.{0}] is custom but has no src/edges/{0}.rs (`bonsai edge add {0} --custom` makes one)",
                e.name
            ));
        }
    }
    missing
}

/// Work out what `bonsai sync` would change in the tree at `root`, writing
/// nothing. Refused when the graph has errors: then nothing is generated.
pub fn plan(root: &Path) -> Result<Plan, Refused> {
    let (cfg, migrated) = read_config(root).map_err(Refused::Config)?;
    let report = graph::check(&cfg, &read_messages(root));
    let mut errors = report.errors.clone();
    errors.extend(missing_sources(root, &cfg));
    if !errors.is_empty() {
        return Err(Refused::Errors(errors));
    }
    let io_err = |e: io::Error| Refused::Config(e.to_string());
    let mut draft = Draft {
        root,
        files: BTreeMap::new(),
        order: Vec::new(),
    };
    if let Some(src) = migrated {
        draft.set(
            CONFIG,
            Some(src),
            Some(format!("{CONFIG} ([[wire]] tables are [[link]] now)")),
        );
    }
    // Trees from before src/wiring.rs was src/links.rs: move their code across.
    if root.join(OLD_WIRING_RS).exists() {
        let mut moved = 0;
        for path in rust_files(&root.join("src")).map_err(io_err)? {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            if rel == OLD_WIRING_RS {
                continue;
            }
            if let Some(new) = draft
                .current(&rel)
                .and_then(|s| crate::tree::with_links_module(&s))
            {
                draft.set(&rel, Some(new), None);
                moved += 1;
            }
        }
        draft.set(
            OLD_WIRING_RS,
            None,
            Some(format!(
                "{OLD_WIRING_RS} → {LINKS_RS} ({moved} files now use crate::links)"
            )),
        );
    }
    draft.write(RUNTIME_RS, RUNTIME.to_string());
    draft.write(LINKS_RS, graph::render_links(&cfg));
    draft.write(SETTINGS_RS, graph::render_settings(&cfg));
    draft.write(BRANCHES_MOD, graph::render_mod(&cfg));
    draft.write(EDGES_MOD, graph::render_edges_mod(&cfg));
    // Trees from before the log macros: bring them into scope.
    if let Some(new) = draft
        .current("src/main.rs")
        .and_then(|s| crate::tree::with_macro_use(&s))
    {
        draft.set(
            "src/main.rs",
            Some(new),
            Some("src/main.rs (#[macro_use] mod bonsai)".to_string()),
        );
    }
    // The serial edge's code and crate come and go with the tree's serial edges.
    let manifest = draft.current("Cargo.toml").unwrap_or_default();
    let mut notes = Vec::new();
    let mut manifest_after = manifest.clone();
    if cfg.has_serial() {
        draft.write(SERIAL_RS, SERIAL.to_string());
        if let Some(new) = crate::with_dependency(&manifest_after, TOKIO_SERIAL).map_err(io_err)? {
            manifest_after = new;
            notes.push(format!("+{}", TOKIO_SERIAL.0));
        }
    } else {
        if draft.current(SERIAL_RS).is_some() {
            draft.set(SERIAL_RS, None, Some(format!("{SERIAL_RS} (removed)")));
        }
        if let Some(new) =
            crate::without_dependency(&manifest_after, TOKIO_SERIAL.0).map_err(io_err)?
        {
            manifest_after = new;
            notes.push(format!("-{}", TOKIO_SERIAL.0));
        }
    }
    // Trees from before run logs: the runtime needs libc now.
    if let Some(new) = crate::with_dependency(&manifest_after, LIBC).map_err(io_err)? {
        manifest_after = new;
        notes.push(format!("+{}", LIBC.0));
    }
    if manifest_after != manifest {
        draft.set(
            "Cargo.toml",
            Some(manifest_after),
            Some(format!("Cargo.toml ({})", notes.join(", "))),
        );
    }
    Ok(Plan {
        changes: draft.finish(),
        warnings: report.warnings,
    })
}

/// How a plan's changes are listed after `updated`.
pub fn summary(changes: &[Change]) -> String {
    changes
        .iter()
        .filter_map(|c| c.label.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Left in the tree while a sync writes its changes, naming them: a tree
/// that has it was interrupted mid-sync, and `bonsai sync` (which works the
/// changes out again from bonsai.toml) finishes it. `bonsai doctor` says so.
pub const JOURNAL: &str = ".bonsai-sync";

/// Whether a sync of the tree at `root` started and didn't finish.
pub fn interrupted(root: &Path) -> bool {
    root.join(JOURNAL).exists()
}

/// Replace `path` with `content` in one step: written in full to a file
/// beside it, synced, then renamed over it. A reader (or a crash) sees the
/// old file or the new one, never part of one.
pub fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    // A temp file of this write's own: named for this process and a count,
    // and created only if no such file exists, so no two writes share one.
    static COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let (temp, mut file) = loop {
        let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temp = dir.join(format!(".{name}.{}-{n}.bonsai-new", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let result = (|| {
        file.write_all(content)?;
        file.sync_all()?;
        if let Ok(old) = std::fs::metadata(path) {
            file.set_permissions(old.permissions())?;
        }
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Write the plan's changes to the tree at `root`, whose lock the caller
/// holds (and held while the plan was made).
///
/// Each file is replaced atomically. The set isn't: a crash part way leaves
/// some files new and some old. So the journal goes down first and comes
/// off last, and a tree left with it is reported, and finished by the next
/// sync, rather than looking synced. A file that changed since the plan was
/// made (another program, an editor) stops the sync before it's touched.
pub fn apply(root: &Path, plan: &Plan, _lock: &crate::lock::TreeLock) -> io::Result<()> {
    if plan.changes.is_empty() {
        // Nothing to write; a journal left by an interrupted sync is done
        // with, since the tree is now what the plan says it should be.
        return remove_journal(root);
    }
    let names: String = plan
        .changes
        .iter()
        .map(|c| format!("{}\n", c.path))
        .collect();
    write_atomic(
        &root.join(JOURNAL),
        format!("bonsai sync in progress; run `bonsai sync` to finish it\n{names}").as_bytes(),
    )?;
    for (done, change) in plan.changes.iter().enumerate() {
        let path = root.join(&change.path);
        let now = std::fs::read_to_string(&path).ok();
        let step = if now != change.before {
            Err(io::Error::other(format!(
                "{} changed while syncing; nothing more was written",
                change.path
            )))
        } else {
            match &change.after {
                Some(content) => write_atomic(&path, content.as_bytes()),
                None => std::fs::remove_file(&path),
            }
        };
        #[cfg(debug_assertions)]
        if done == 0 {
            pause_for_tests();
        }
        if let Err(e) = step {
            return Err(io::Error::new(
                e.kind(),
                format!(
                    "sync stopped after {done} of {} files: {e}. Run `bonsai sync` again to finish ({JOURNAL} marks the tree until then)",
                    plan.changes.len()
                ),
            ));
        }
    }
    remove_journal(root)
}

/// Debug builds only, for tests: with `BONSAI_TEST_PAUSE_APPLY=<path>`, stop
/// after the first file of a sync is written (lock held, journal down), say
/// so with `<path>.paused`, and go on once `<path>.go` exists.
#[cfg(debug_assertions)]
fn pause_for_tests() {
    let Some(at) = std::env::var_os("BONSAI_TEST_PAUSE_APPLY") else {
        return;
    };
    let at = std::path::PathBuf::from(at);
    let _ = std::fs::write(at.with_extension("paused"), "");
    while !at.with_extension("go").exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn remove_journal(root: &Path) -> io::Result<()> {
    match std::fs::remove_file(root.join(JOURNAL)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A unified diff of `before` to `after` (lines), with `context` lines
/// around each change, or None when they're the same.
pub fn diff(
    path: &str,
    before: Option<&str>,
    after: Option<&str>,
    context: usize,
) -> Option<String> {
    if before == after {
        return None;
    }
    let old: Vec<&str> = before.map(|s| s.lines().collect()).unwrap_or_default();
    let new: Vec<&str> = after.map(|s| s.lines().collect()).unwrap_or_default();
    let ops = line_ops(&old, &new);
    let mut out = format!(
        "--- {}\n+++ {}\n",
        if before.is_some() {
            format!("a/{path}")
        } else {
            "/dev/null".to_string()
        },
        if after.is_some() {
            format!("b/{path}")
        } else {
            "/dev/null".to_string()
        },
    );
    // Group the ops into hunks: changes and `context` lines either side.
    let changed: Vec<usize> = (0..ops.len()).filter(|&i| ops[i].0 != ' ').collect();
    let mut i = 0;
    while i < changed.len() {
        let start = changed[i].saturating_sub(context);
        let mut end = (changed[i] + context + 1).min(ops.len());
        while i + 1 < changed.len() && changed[i + 1] <= end + context {
            i += 1;
            end = (changed[i] + context + 1).min(ops.len());
        }
        let (mut a, mut b) = (1, 1);
        for op in &ops[..start] {
            if op.0 != '+' {
                a += 1;
            }
            if op.0 != '-' {
                b += 1;
            }
        }
        let hunk = &ops[start..end];
        let a_len = hunk.iter().filter(|o| o.0 != '+').count();
        let b_len = hunk.iter().filter(|o| o.0 != '-').count();
        let at = |n: usize, len: usize| if len == 0 { n - 1 } else { n };
        out += &format!(
            "@@ -{},{a_len} +{},{b_len} @@\n",
            at(a, a_len),
            at(b, b_len)
        );
        for (op, line) in hunk {
            out += &format!("{op}{line}\n");
        }
        i += 1;
    }
    Some(out)
}

/// The edit from `old` to `new`, line by line: ' ' kept, '-' removed, '+'
/// added. Common ends are matched first; the middle by longest common
/// subsequence (or wholly replaced, when it's too big to compare).
fn line_ops<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<(char, &'a str)> {
    let head = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (o, n) = (&old[head..old.len() - tail], &new[head..new.len() - tail]);
    let mut ops: Vec<(char, &str)> = old[..head].iter().map(|l| (' ', *l)).collect();
    if o.len().saturating_mul(n.len()) > 4_000_000 {
        ops.extend(o.iter().map(|l| ('-', *l)));
        ops.extend(n.iter().map(|l| ('+', *l)));
    } else {
        // lcs[i][j]: the longest common subsequence of o[i..] and n[j..].
        let mut lcs = vec![vec![0u32; n.len() + 1]; o.len() + 1];
        for i in (0..o.len()).rev() {
            for j in (0..n.len()).rev() {
                lcs[i][j] = if o[i] == n[j] {
                    lcs[i + 1][j + 1] + 1
                } else {
                    lcs[i + 1][j].max(lcs[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < o.len() || j < n.len() {
            if i < o.len() && j < n.len() && o[i] == n[j] {
                ops.push((' ', o[i]));
                i += 1;
                j += 1;
            } else if i < o.len() && (j == n.len() || lcs[i + 1][j] >= lcs[i][j + 1]) {
                ops.push(('-', o[i]));
                i += 1;
            } else {
                ops.push(('+', n[j]));
                j += 1;
            }
        }
    }
    ops.extend(old[old.len() - tail..].iter().map(|l| (' ', *l)));
    ops
}

/// What `bonsai sync --dry-run` prints: each change, with its diff.
pub fn render_dry_run(plan: &Plan) -> String {
    if plan.changes.is_empty() {
        return format!("nothing to change: generated code is in step with {CONFIG}\n");
    }
    let mut s = format!(
        "`bonsai sync` would change {} file(s); nothing has been written:\n",
        plan.changes.len()
    );
    for c in &plan.changes {
        s += &format!("  {:8} {}\n", c.kind(), c.path);
    }
    for c in &plan.changes {
        s += "\n";
        s += &diff(&c.path, c.before.as_deref(), c.after.as_deref(), 3).unwrap_or_default();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn locked(root: &Path) -> crate::lock::TreeLock {
        crate::lock::acquire(root, "test", std::time::Duration::ZERO).unwrap()
    }

    /// A fresh host tree in its own folder, with a branch `sensor` added by
    /// hand (its table and file, not yet synced).
    fn tree_with_a_new_branch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("bonsai-sync-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let tmpl = crate::TEMPLATES.get_dir("linux/host").unwrap();
        crate::extract_dir(tmpl, tmpl.path(), &root).unwrap();
        let toml = std::fs::read_to_string(root.join(CONFIG)).unwrap();
        std::fs::write(
            root.join(CONFIG),
            format!("{toml}\n[branch.sensor]\nrate = 1\n"),
        )
        .unwrap();
        let scaffold = crate::tree::BRANCH_TEMPLATE
            .replace("{{branch_name}}", "sensor")
            .replace("{{BranchName}}", "Sensor");
        std::fs::write(root.join("src/branches/sensor.rs"), scaffold).unwrap();
        root
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out: Vec<_> = rust_files(&root.join("src"))
            .unwrap()
            .into_iter()
            .chain([root.join(CONFIG), root.join("Cargo.toml")])
            .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
            .collect();
        out.push((
            root.join(JOURNAL),
            std::fs::read(root.join(JOURNAL)).unwrap_or_default(),
        ));
        out
    }

    #[test]
    fn the_preview_and_the_sync_are_the_same_plan() {
        let root = tree_with_a_new_branch("same-plan");
        let before = snapshot(&root);
        let planned = plan(&root).unwrap();
        let shown = render_dry_run(&planned);
        assert_eq!(
            snapshot(&root),
            before,
            "planning or previewing wrote something"
        );
        let paths: Vec<&str> = planned.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, ["src/links.rs", "src/branches/mod.rs"]);
        for c in &planned.changes {
            assert!(
                shown.contains(&format!("  changed  {}\n", c.path)),
                "{shown}"
            );
            assert!(shown.contains(&format!("+++ b/{}\n", c.path)), "{shown}");
        }
        assert!(shown.contains("+pub mod sensor;\n"), "{shown}");
        apply(&root, &planned, &locked(&root)).unwrap();
        for c in &planned.changes {
            let now = std::fs::read_to_string(root.join(&c.path)).unwrap();
            assert_eq!(
                Some(now),
                c.after,
                "{} isn't what the preview showed",
                c.path
            );
        }
        assert!(
            plan(&root).unwrap().changes.is_empty(),
            "a second sync would change more"
        );
        assert!(!interrupted(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_invalid_graph_plans_nothing_and_changes_nothing() {
        let root = tree_with_a_new_branch("invalid");
        let toml = std::fs::read_to_string(root.join(CONFIG)).unwrap();
        std::fs::write(
            root.join(CONFIG),
            format!("{toml}\n[[link]]\nfrom = \"sensor\"\nmessage = \"Nope\"\nto = [\"ghost\"]\n"),
        )
        .unwrap();
        let before = snapshot(&root);
        match plan(&root) {
            Err(Refused::Errors(errors)) => {
                assert!(errors.iter().any(|e| e.contains("ghost")), "{errors:?}")
            }
            other => panic!("expected the graph's errors, got {other:?}"),
        }
        std::fs::write(root.join(CONFIG), "[branch.sensor\n").unwrap();
        assert!(matches!(plan(&root), Err(Refused::Config(_))));
        let mut after = snapshot(&root);
        after.retain(|(p, _)| !p.ends_with(CONFIG));
        let mut before = before;
        before.retain(|(p, _)| !p.ends_with(CONFIG));
        assert_eq!(after, before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_sync_that_fails_part_way_is_marked_and_the_next_one_finishes_it() {
        let root = tree_with_a_new_branch("fails");
        let plan1 = plan(&root).unwrap();
        // The second file can't be replaced: a folder is in its way.
        let second = root.join(&plan1.changes[1].path);
        let saved = std::fs::read(&second).unwrap();
        std::fs::remove_file(&second).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(second.join("x"), "").unwrap();
        let err = apply(&root, &plan1, &locked(&root))
            .unwrap_err()
            .to_string();
        assert!(err.contains("sync stopped after 1 of 2 files"), "{err}");
        assert!(interrupted(&root), "the tree looks synced");
        let findings = crate::doctor::check(&root);
        assert!(
            findings.iter().any(|f| f.id == "generated"
                && f.status == crate::doctor::Status::Error
                && f.subject.as_deref() == Some(JOURNAL)),
            "{findings:?}"
        );
        // Put it back; the next sync works the rest out and finishes.
        std::fs::remove_dir_all(&second).unwrap();
        std::fs::write(&second, saved).unwrap();
        let plan2 = plan(&root).unwrap();
        let paths: Vec<&str> = plan2.changes.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            ["src/branches/mod.rs"],
            "only what wasn't written yet"
        );
        apply(&root, &plan2, &locked(&root)).unwrap();
        assert!(!interrupted(&root));
        assert!(plan(&root).unwrap().changes.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_changed_after_planning_is_never_overwritten() {
        let root = tree_with_a_new_branch("raced");
        let planned = plan(&root).unwrap();
        let first = root.join(&planned.changes[0].path);
        std::fs::write(&first, "// edited meanwhile\n").unwrap();
        let err = apply(&root, &planned, &locked(&root))
            .unwrap_err()
            .to_string();
        assert!(err.contains("changed while syncing"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&first).unwrap(),
            "// edited meanwhile\n"
        );
        assert!(interrupted(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_sync_touches_generated_files_only() {
        let root = tree_with_a_new_branch("own-code");
        let generated = [
            RUNTIME_RS,
            LINKS_RS,
            SETTINGS_RS,
            BRANCHES_MOD,
            EDGES_MOD,
            SERIAL_RS,
        ];
        for c in plan(&root).unwrap().changes {
            assert!(
                generated.contains(&c.path.as_str()),
                "{} isn't generated",
                c.path
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn diffs_are_unified_with_context() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let after = "a\nb\nc\nD\ne\nf\ng\nh\ni\n";
        assert_eq!(
            diff("x.rs", Some(before), Some(after), 1).unwrap(),
            "--- a/x.rs\n+++ b/x.rs\n@@ -3,3 +3,3 @@\n c\n-d\n+D\n e\n@@ -8,1 +8,2 @@\n h\n+i\n"
        );
        assert_eq!(
            diff("n.rs", None, Some("one\n"), 3).unwrap(),
            "--- /dev/null\n+++ b/n.rs\n@@ -0,0 +1,1 @@\n+one\n"
        );
        assert_eq!(
            diff("g.rs", Some("one\n"), None, 3).unwrap(),
            "--- a/g.rs\n+++ /dev/null\n@@ -1,1 +0,0 @@\n-one\n"
        );
        assert_eq!(diff("s.rs", Some("same\n"), Some("same\n"), 3), None);
    }

    #[test]
    fn an_atomic_write_leaves_nothing_beside_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("bonsai-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.rs");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        // Another writer's temp file, mid-write, beside it: not used, not removed.
        let theirs = dir.join(".f.rs.1-0.bonsai-new");
        std::fs::write(&theirs, "theirs").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(&theirs).unwrap(), "theirs");
        std::fs::remove_file(&theirs).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["f.rs"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
