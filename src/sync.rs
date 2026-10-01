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

/// Write the plan's changes to the tree at `root`.
pub fn apply(root: &Path, plan: &Plan) -> io::Result<()> {
    for change in &plan.changes {
        let path = root.join(&change.path);
        match &change.after {
            Some(content) => {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&path, content)?;
            }
            None => std::fs::remove_file(&path)?,
        }
    }
    Ok(())
}
