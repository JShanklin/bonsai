mod flow;
mod tools;

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};
use ratatui::{DefaultTerminal, Frame};

// ---------------------------------------------------------------------------
// The boards. This is the only thing you edit when adding hardware (plus its
// template in templates/linux/<board>/).
// ---------------------------------------------------------------------------

/// Every board bonsai grows trees for: its name, its chip (stamped into the
/// tree's Cargo.toml) and what it is.
const BOARDS: &[(&str, &str, &str)] = &[
    ("pi5", "bcm2712", "Raspberry Pi 5 (64-bit)"),
    ("zero-2w", "bcm2710a1", "Raspberry Pi Zero 2 W (64-bit)"),
    ("zero-w", "bcm2835", "Raspberry Pi Zero W (32-bit ARMv6)"),
    ("host", "native", "this computer (a native build)"),
];

/// The chip of a known board. Used by `regrow` and `retarget` to recover a
/// tree's device from its board alone.
fn chip_of(board: &str) -> Option<&'static str> {
    BOARDS
        .iter()
        .find(|(name, ..)| *name == board)
        .map(|(_, chip, _)| *chip)
}

/// The board names, for messages.
fn board_names() -> Vec<&'static str> {
    BOARDS.iter().map(|(name, ..)| *name).collect()
}

/// The wizard's steps: the board, then the build tools the tree uses
/// (`src/tools.rs`), then plant here (the cwd) or in a new folder, then the name.
const BOARD_STEP: usize = 0;
const TOOLS_STEP: usize = 1;
const WHERE_STEP: usize = 2;
const NAME_STEP: usize = 3;

// The whole templates/ tree is baked into the binary, so an installed `bonsai`
// carries its templates and works from any directory.
static TEMPLATES: include_dir::Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/templates");

// The branch scaffolds, also embedded, so `bonsai branch` works from inside a
// grown tree (where the templates/ dir isn't around). All of them draw on the
// generated `src/sap.rs`: taps via `sap::<branch>::Taps`, releases via `Sap`.
const BRANCH_TEMPLATE: &str = include_str!("../templates/_branch/branch.rs");

// The producer variant: releases nutrients instead of tapping them. Has no
// `match Nutrient`, so `tap` refuses it and `starve` has no arm to patch.
const BRANCH_PRODUCER_TEMPLATE: &str = include_str!("../templates/_branch/branch_producer.rs");

// The duplex variant: taps and releases. Keeps the `match Nutrient` (and its
// `// bonsai:nutrient-arm` marker) plus the `// bonsai:emit` marker.
const BRANCH_DUPLEX_TEMPLATE: &str = include_str!("../templates/_branch/branch_duplex.rs");

// The roots variant (std trees only): `start` opens the link and hands its
// blocking receive/send to `roots::bridge`; `run` selects over the inbox and
// the taps. Carries both markers (release → inbox arm, tap → nutrient arm).
const BRANCH_ROOTS_TEMPLATE: &str = include_str!("../templates/_branch/branch_roots.rs");

// The shared bridge every root uses (OS threads ⇄ the executor). Written to
// `src/roots.rs` by the first `branch --roots`, removed by the last `snip`.
const ROOTS_BRIDGE: &str = include_str!("../templates/_branch/roots.rs");
const ROOTS_RS: &str = "src/roots.rs";
const ROOTS_MOD: &str = "mod roots;";
/// `select` for a root's `run` loop.
const EMBASSY_FUTURES: (&str, &str) = ("embassy-futures", "0.1.2");

/// Which scaffold `branch` lays down.
#[derive(Clone, Copy, PartialEq)]
enum BranchMode {
    Consumer,
    Producer,
    Duplex,
    Roots,
}

impl BranchMode {
    fn template(self) -> &'static str {
        match self {
            BranchMode::Consumer => BRANCH_TEMPLATE,
            BranchMode::Producer => BRANCH_PRODUCER_TEMPLATE,
            BranchMode::Duplex => BRANCH_DUPLEX_TEMPLATE,
            BranchMode::Roots => BRANCH_ROOTS_TEMPLATE,
        }
    }

    fn label(self) -> &'static str {
        match self {
            BranchMode::Consumer => "branch",
            BranchMode::Producer => "producer branch",
            BranchMode::Duplex => "duplex branch",
            BranchMode::Roots => "roots branch",
        }
    }
}

/// Locate the trunk template for `board`. Returns its path and whether that
/// path is a temp dir we extracted (and should delete afterwards).
/// Resolution order:
///   1. `$BONSAI_TEMPLATES/linux/<board>` — dev/override, edit without rebuilding
///   2. `./templates/linux/<board>`       — running from the repo
///   3. the copy embedded in this binary  — installed, run from anywhere
fn template_dir(board: &str) -> io::Result<(PathBuf, bool)> {
    let sub = format!("linux/{board}");
    if let Some(root) = std::env::var_os("BONSAI_TEMPLATES") {
        let p = Path::new(&root).join(&sub);
        if p.is_dir() {
            return Ok((p, false));
        }
        eprintln!(
            "BONSAI_TEMPLATES is set but {} is not a directory",
            p.display()
        );
        std::process::exit(1);
    }

    let local = Path::new("templates").join(&sub);
    if local.is_dir() {
        return Ok((local, false));
    }

    let Some(dir) = TEMPLATES.get_dir(&sub) else {
        eprintln!("no template for {sub}");
        std::process::exit(1);
    };
    let dest = std::env::temp_dir().join(format!("bonsai-tmpl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest); // clear a stale run, ignore if absent
    extract_dir(dir, dir.path(), &dest)?;
    Ok((dest, true))
}

/// Write an embedded `dir` out to `dest`, with `strip` removed from each entry's
/// path (so the board folder's contents land at the root of `dest`).
fn extract_dir(dir: &include_dir::Dir, strip: &Path, dest: &Path) -> io::Result<()> {
    for file in dir.files() {
        let rel = file
            .path()
            .strip_prefix(strip)
            .unwrap_or_else(|_| file.path());
        let out = dest.join(rel);
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(out, file.contents())?;
    }
    for sub in dir.dirs() {
        extract_dir(sub, strip, dest)?;
    }
    Ok(())
}

/// The immediate subdirectory names of `root` (empty on any read error). Used to
/// diff cwd before/after cargo-generate and find the folder it created.
fn subdirs(root: &Path) -> std::collections::HashSet<std::ffi::OsString> {
    let mut names = std::collections::HashSet::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                names.insert(entry.file_name());
            }
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Wizard: create a new device (the trunk).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Wizard {
    /// BOARD_STEP, TOOLS_STEP and WHERE_STEP are menus; NAME_STEP is the name.
    step: usize,
    /// Cursor position per menu step, remembered so going back restores it.
    cursor: [usize; 3],
    /// The picked board.
    board: String,
    /// The build tools checklist.
    tools: ToolPicker,
    name: String,
    /// Plant into the cwd (`cargo generate --init`) instead of a new folder.
    here: bool,
    /// `bonsai init`: always here, so the Where step is skipped.
    here_only: bool,
    /// The cwd's folder name, offered as the project name when planting here.
    folder: String,
    /// The cwd holds nothing but dotfiles, so "here" is the likely choice.
    cwd_empty: bool,
    /// Set when the user confirms the final step.
    finished: bool,
    /// Set when the user bails out.
    aborted: bool,
}

impl Wizard {
    fn new(here_only: bool, cwd: &Path) -> Self {
        Wizard {
            here: here_only,
            here_only,
            folder: cwd
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            cwd_empty: only_dotfiles(cwd),
            ..Default::default()
        }
    }

    /// What each menu line shows.
    fn options(&self) -> Vec<String> {
        match self.step {
            BOARD_STEP => BOARDS
                .iter()
                .map(|(name, _, about)| format!("{name:<8} {about}"))
                .collect(),
            TOOLS_STEP => self.tools.labels(),
            WHERE_STEP => vec![
                format!("here: in this folder ({}/)", self.folder),
                "new folder: ./<project name>/".to_string(),
            ],
            _ => Vec::new(),
        }
    }

    /// Move on from the board: to the build tools, offered for this board.
    /// Tools already installed start out picked.
    fn after_board(&mut self) {
        if self.tools.board != self.board {
            self.tools = ToolPicker::new(&self.board, tools::Tool::installed);
            self.cursor[TOOLS_STEP] = 0;
        }
        self.step = TOOLS_STEP;
    }

    /// Move on from the tools: to the Where menu, or straight to the name.
    fn after_tools(&mut self) {
        if self.here_only {
            self.enter_name();
        } else {
            self.step = WHERE_STEP;
            self.cursor[WHERE_STEP] = if self.cwd_empty { 0 } else { 1 };
        }
    }

    /// Go to the name entry, offering the folder's name when planting here.
    fn enter_name(&mut self) {
        if self.here && self.name.is_empty() {
            self.name = crate_name(&self.folder);
        }
        self.step = NAME_STEP;
    }

    /// Back out of the name entry.
    fn leave_name(&mut self) {
        self.step = if self.here_only {
            TOOLS_STEP
        } else {
            WHERE_STEP
        };
    }

    fn on_key(&mut self, key: KeyCode) {
        // The text-entry step behaves completely differently from the menus.
        if self.step == NAME_STEP {
            match key {
                KeyCode::Char(c) if !c.is_whitespace() => self.name.push(c),
                KeyCode::Backspace => {
                    if self.name.pop().is_none() {
                        self.leave_name(); // empty field + backspace = go back
                    }
                }
                KeyCode::Esc => self.leave_name(),
                KeyCode::Enter if !self.name.is_empty() => self.finished = true,
                _ => {}
            }
            return;
        }

        // Copy the index out rather than holding &mut into self.cursor -
        // otherwise the borrow is still live when we call self.options() below.
        let len = self.options().len();
        let cur = self.cursor[self.step];

        // `q` and back must work even if the option list is somehow empty, so
        // they're not gated on `len`; only movement and select are.
        match key {
            KeyCode::Char('q') => self.aborted = true,
            KeyCode::Char(' ') if self.step == TOOLS_STEP => self.tools.toggle(cur),
            KeyCode::Up | KeyCode::Char('k') if len > 0 => {
                self.cursor[self.step] = if cur == 0 { len - 1 } else { cur - 1 };
            }
            KeyCode::Down | KeyCode::Char('j') if len > 0 => {
                self.cursor[self.step] = (cur + 1) % len;
            }
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right if len > 0 => match self.step {
                BOARD_STEP => {
                    self.board = BOARDS[cur].0.to_string();
                    self.after_board();
                }
                TOOLS_STEP => self.after_tools(),
                _ => {
                    self.here = cur == 0;
                    self.enter_name();
                }
            },
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('h') | KeyCode::Left => {
                if self.step == BOARD_STEP {
                    self.aborted = true;
                } else {
                    self.step -= 1;
                }
            }
            _ => {}
        }
    }

    fn draw(&self, frame: &mut Frame) {
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .areas(frame.area());

        // --- breadcrumb -----------------------------------------------------
        let picked = Style::new().fg(Color::Green).add_modifier(Modifier::BOLD);
        let mut crumbs: Vec<Span> = Vec::new();
        if self.step > BOARD_STEP {
            crumbs.push(Span::raw("Board: "));
            crumbs.push(Span::styled(self.board.clone(), picked));
            crumbs.push(Span::raw("   "));
        }
        if self.step > TOOLS_STEP {
            let tools = self.tools.picked();
            crumbs.push(Span::raw("Tools: "));
            crumbs.push(Span::styled(
                if tools.is_empty() {
                    "none".to_string()
                } else {
                    tools
                        .iter()
                        .map(|t| t.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                picked,
            ));
            crumbs.push(Span::raw("   "));
        }
        if self.step > WHERE_STEP {
            crumbs.push(Span::raw("Where: "));
            let place = if self.here {
                format!("{}/", self.folder)
            } else {
                "new folder".to_string()
            };
            crumbs.push(Span::styled(place, picked));
        }
        if crumbs.is_empty() {
            crumbs.push(Span::styled(
                "no selections yet",
                Style::new().fg(Color::DarkGray),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(crumbs)).block(Block::bordered().title(" bonsai ")),
            header,
        );

        // --- body -----------------------------------------------------------
        if self.step == NAME_STEP {
            let field = format!("{}_", self.name);
            frame.render_widget(
                Paragraph::new(field).block(Block::bordered().title(" Project name ")),
                body,
            );
        } else {
            let opts = self.options();
            let items: Vec<ListItem> = opts.iter().map(|o| ListItem::new(o.as_str())).collect();
            let title = match self.step {
                BOARD_STEP => " Select the board it runs on ",
                TOOLS_STEP => " Build tools: missing ones install once ",
                _ => " Where to plant it ",
            };
            let list = List::new(items)
                .block(Block::bordered().title(title))
                .highlight_style(
                    Style::new()
                        .bg(Color::Indexed(238))
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▸ ");

            let mut state = ListState::default();
            state.select(Some(self.cursor[self.step]));
            frame.render_stateful_widget(list, body, &mut state);
        }

        // --- footer ---------------------------------------------------------
        let help = if self.step == NAME_STEP {
            "type name   enter confirm   esc back"
        } else if self.step == TOOLS_STEP {
            "↑↓/jk move   space pick   enter confirm   esc back   q quit"
        } else {
            "↑↓/jk move   enter select   esc/backspace back   q quit"
        };
        frame.render_widget(
            Paragraph::new(help).style(Style::new().fg(Color::DarkGray)),
            footer,
        );
    }
}

/// A checklist of build tools: space picks, enter confirms. The wizard's Build
/// tools step and `bonsai tools` both use it.
#[derive(Default)]
struct ToolPicker {
    /// The board the rows were made for.
    board: String,
    /// Each tool that fits the board: picked, and already installed.
    rows: Vec<(tools::Tool, bool, bool)>,
}

impl ToolPicker {
    fn new(board: &str, picked: impl Fn(tools::Tool) -> bool) -> Self {
        let rows = tools::Tool::ALL
            .into_iter()
            .filter(|t| t.fits(board))
            .map(|t| (t, picked(t), t.installed()))
            .collect();
        ToolPicker {
            board: board.to_string(),
            rows,
        }
    }

    fn labels(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|(t, picked, installed)| {
                let mark = if *picked { "x" } else { " " };
                let note = if *installed { "" } else { "  (installs once)" };
                format!("[{mark}] {:<9} {}{note}", t.name(), t.about())
            })
            .collect()
    }

    fn toggle(&mut self, i: usize) {
        if let Some(row) = self.rows.get_mut(i) {
            row.1 = !row.1;
        }
    }

    fn picked(&self) -> Vec<tools::Tool> {
        self.rows.iter().filter(|r| r.1).map(|r| r.0).collect()
    }
}

fn run_wizard(mut term: DefaultTerminal, mut wiz: Wizard) -> io::Result<Wizard> {
    loop {
        term.draw(|f| wiz.draw(f))?;
        if wiz.finished || wiz.aborted {
            return Ok(wiz);
        }
        // KeyEventKind::Press filter matters on Windows, where release events
        // would otherwise double every keystroke.
        if let Event::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
        {
            wiz.on_key(k.code);
        }
    }
}

/// Drive the wizard, then hand off to cargo-generate to lay down the trunk:
/// into a new folder, or with `here_only` (`bonsai init`) into the cwd.
fn create_device(here_only: bool) -> io::Result<()> {
    let cwd = std::env::current_dir()?;
    if here_only {
        refuse_planting_in(&cwd);
    }

    let terminal = ratatui::init();
    let result = run_wizard(terminal, Wizard::new(here_only, &cwd));
    // Restore before doing anything else, or cargo-generate's output lands in
    // the alternate screen and vanishes when we exit.
    ratatui::restore();

    let wiz = result?;
    if wiz.aborted || !wiz.finished {
        println!("cancelled");
        return Ok(());
    }

    // Missing build tools install first (sudo may ask for your password).
    let picked = wiz.tools.picked();
    let usable = if picked.is_empty() {
        Vec::new()
    } else {
        tools::install(&picked)
    };

    let (template_path, is_temp) = template_dir(&wiz.board)?;
    let cleanup = || {
        if is_temp {
            let _ = std::fs::remove_dir_all(&template_path);
        }
    };

    // Planting here never overwrites: refuse up front if any file the template
    // would write is already in the cwd.
    if wiz.here {
        refuse_planting_in(&cwd);
        let taken = plant_here_conflicts(&cwd, &template_path);
        if !taken.is_empty() {
            cleanup();
            eprintln!(
                "can't plant here: these files already exist in {}:",
                cwd.display()
            );
            for path in taken {
                eprintln!("  {}", path.display());
            }
            eprintln!("move them aside, or pick \"new folder\".");
            std::process::exit(1);
        }
    }

    let mut args = vec![
        "generate".to_string(),
        "--path".to_string(),
        template_path.display().to_string(),
        "--name".to_string(),
        wiz.name.clone(),
    ];
    if wiz.here {
        // No subfolder, and no `git init` (keep the folder's own repo, if any).
        args.push("--init".to_string());
    }
    args.extend(template_defines(&wiz.board));

    // Snapshot cwd's folders so we can find the one cargo-generate creates —
    // it may sanitize `wiz.name` (case, punctuation) into a different folder name.
    let before = subdirs(&cwd);

    println!("cargo {}", args.join(" "));
    let status = Command::new("cargo").args(&args).status()?;

    // The extracted template was only scratch space for cargo-generate.
    cleanup();

    // Fail loudly: scripts must see a non-zero exit, matching `regrow`'s behavior.
    if !status.success() {
        eprintln!("cargo-generate failed");
        std::process::exit(1);
    }

    // Use the folder cargo-generate actually created, not wiz.name, which it
    // may have rewritten (falling back to wiz.name if the diff finds nothing).
    let project = if wiz.here {
        cwd.clone()
    } else {
        subdirs(&cwd)
            .into_iter()
            .find(|d| !before.contains(d))
            .map(|d| cwd.join(d))
            .unwrap_or_else(|| cwd.join(&wiz.name))
    };

    if !usable.is_empty() {
        apply_tools(&project, &usable)?;
    }
    Ok(())
}

/// The `-d key=value` pairs a trunk template renders from: the board and its chip.
fn template_defines(board: &str) -> Vec<String> {
    let chip = chip_of(board).unwrap_or("native");
    [format!("chip={chip}"), format!("board={board}")]
        .into_iter()
        .flat_map(|d| ["-d".to_string(), d])
        .collect()
}

/// Switch the tree in `dir` to `picked` build tools, and say what changed.
fn apply_tools(dir: &Path, picked: &[tools::Tool]) -> io::Result<()> {
    let config = std::fs::read_to_string(dir.join(".cargo/config.toml"))?;
    let target = parse_target(&config).unwrap_or_default();
    tools::apply(dir, picked, &target)?;
    let names: Vec<&str> = picked.iter().map(|t| t.name()).collect();
    if names.is_empty() {
        println!("build tools: none");
    } else {
        println!("build tools: {} (in .cargo/config.toml)", names.join(", "));
    }
    Ok(())
}

/// `bonsai tools [<tool> ...]`: pick the tree's build tools, installing missing
/// ones once. With names, no menu: exactly those are on, the rest off.
fn tools_command(names: &[String]) -> io::Result<()> {
    let root = std::env::current_dir()?;
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    let Some(board) = parse_board(&manifest).filter(|b| chip_of(b).is_some()) else {
        eprintln!("not a bonsai tree — run `bonsai tools` inside a generated project.");
        std::process::exit(1);
    };
    let config = std::fs::read_to_string(root.join(".cargo/config.toml")).unwrap_or_default();
    let current = tools::configured(&config);

    let picked = if names.is_empty() {
        // Bacon has no settings, so it shows as on whenever it's installed.
        let mut picker = ToolPicker::new(&board, |t| {
            current.contains(&t) || (t == tools::Tool::Bacon && t.installed())
        });
        let mut term = ratatui::init();
        let done = pick_tools(&mut term, &mut picker);
        ratatui::restore();
        if !done? {
            println!("cancelled");
            return Ok(());
        }
        picker.picked()
    } else {
        let mut picked = Vec::new();
        for name in names {
            match tools::Tool::parse(name) {
                Some(t) if t.fits(&board) => picked.push(t),
                Some(_) => {
                    eprintln!("{name} is for Raspberry Pi trees, and this is a {board} tree.");
                    std::process::exit(2);
                }
                None => {
                    let all: Vec<&str> = tools::Tool::ALL.iter().map(|t| t.name()).collect();
                    eprintln!("no tool `{name}`; one of: {}", all.join(", "));
                    std::process::exit(2);
                }
            }
        }
        picked
    };
    let usable = tools::install(&picked);
    apply_tools(&root, &usable)
}

/// The build tools checklist on its own screen. Ok(true) on Enter, Ok(false) on Esc.
fn pick_tools(term: &mut DefaultTerminal, picker: &mut ToolPicker) -> io::Result<bool> {
    let mut cur = 0usize;
    let context = ["Build tools for this tree. Missing ones install once.".to_string()];
    loop {
        let labels = picker.labels();
        term.draw(|f| {
            let body = prompt_frame_with(
                f,
                &context,
                "bonsai tools",
                "↑↓/jk move · space pick · enter confirm · esc cancel",
            );
            let items: Vec<ListItem> = labels.iter().map(|l| ListItem::new(l.as_str())).collect();
            let list = List::new(items)
                .block(Block::bordered().title(" tools "))
                .highlight_style(
                    Style::new()
                        .bg(Color::Indexed(238))
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▸ ");
            let mut state = ListState::default();
            state.select(Some(cur));
            f.render_stateful_widget(list, body, &mut state);
        })?;
        if let Event::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
        {
            let len = labels.len().max(1);
            match k.code {
                KeyCode::Up | KeyCode::Char('k') => cur = (cur + len - 1) % len,
                KeyCode::Down | KeyCode::Char('j') => cur = (cur + 1) % len,
                KeyCode::Char(' ') => picker.toggle(cur),
                KeyCode::Enter => return Ok(true),
                KeyCode::Esc | KeyCode::Char('q') => return Ok(false),
                _ => {}
            }
        }
    }
}

/// Exit with a message if `dir` must not become a tree: the filesystem root,
/// the home directory, or a folder that already holds one.
fn refuse_planting_in(dir: &Path) {
    if dir.parent().is_none() {
        eprintln!("refusing to plant a tree in the filesystem root");
        std::process::exit(1);
    }
    if std::env::var_os("HOME").is_some_and(|h| dir == Path::new(&h)) {
        eprintln!("refusing to plant a tree in your home directory — make a folder for it first");
        std::process::exit(1);
    }
    if file_contains(&dir.join("Cargo.toml"), "Generated by bonsai for") {
        eprintln!(
            "this folder already holds a bonsai tree. `bonsai regrow` resets it to a fresh one."
        );
        std::process::exit(1);
    }
}

/// The files a template would write that already exist in `dir`. cargo-generate's
/// own config and hooks are skipped: they aren't part of the output.
fn plant_here_conflicts(dir: &Path, template: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, at: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(at) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if let Ok(rel) = path.strip_prefix(root) {
                out.push(rel.to_path_buf());
            }
        }
    }
    let mut files = Vec::new();
    walk(template, template, &mut files);
    let mut taken: Vec<PathBuf> = files
        .into_iter()
        .filter(|rel| !matches!(rel.to_str(), Some("cargo-generate.toml")))
        .filter(|rel| dir.join(rel).exists())
        .collect();
    taken.sort();
    taken
}

/// Whether `dir` holds nothing but dotfiles (`.git`, `.gitignore` …).
fn only_dotfiles(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries
            .flatten()
            .all(|e| e.file_name().to_string_lossy().starts_with('.'))
    })
}

/// A folder name made into a crate name: lowercase ASCII letters, digits, `-`
/// and `_`; anything else becomes `-`.
fn crate_name(folder: &str) -> String {
    folder
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9' | '-' | '_') => c,
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

// ---------------------------------------------------------------------------
// Small interactive prompts, reused by the `branch`/`feed` TUIs. Each drives an
// already-init'd ratatui terminal; the caller runs `ratatui::init`/`restore`.
// ---------------------------------------------------------------------------

/// Draw a bordered header (`context` lines) above a `body` widget and a footer.
fn prompt_frame(frame: &mut Frame, context: &[String], title: &str) -> ratatui::layout::Rect {
    prompt_frame_with(
        frame,
        context,
        title,
        "↑↓/jk move · enter select · esc cancel",
    )
}

/// `prompt_frame` with its own key help in the footer.
fn prompt_frame_with(
    frame: &mut Frame,
    context: &[String],
    title: &str,
    help: &str,
) -> ratatui::layout::Rect {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length((context.len() as u16).max(1) + 2),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let lines: Vec<Line> = if context.is_empty() {
        vec![Line::from(Span::styled(
            "bonsai",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        context.iter().map(|c| Line::from(c.clone())).collect()
    };
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(format!(" {title} "))),
        header,
    );
    frame.render_widget(
        Paragraph::new(help).style(Style::new().fg(Color::DarkGray)),
        footer,
    );
    body
}

/// Single-line text entry. `accept` filters typed chars. Some(text) on Enter,
/// None on Esc.
fn prompt_text(
    term: &mut DefaultTerminal,
    prompt: &str,
    context: &[String],
    accept: impl Fn(char) -> bool,
) -> io::Result<Option<String>> {
    let mut text = String::new();
    loop {
        term.draw(|f| {
            let body = prompt_frame(f, context, prompt);
            f.render_widget(
                Paragraph::new(format!("{text}_")).block(Block::bordered().title(" input ")),
                body,
            );
        })?;
        if let Event::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
        {
            match k.code {
                KeyCode::Char(c) if accept(c) => text.push(c),
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Enter => return Ok(Some(text)),
                KeyCode::Esc => return Ok(None),
                _ => {}
            }
        }
    }
}

/// Menu select. Some(index) on Enter, None on Esc.
fn prompt_menu(
    term: &mut DefaultTerminal,
    prompt: &str,
    context: &[String],
    options: &[&str],
) -> io::Result<Option<usize>> {
    let mut cur = 0usize;
    loop {
        term.draw(|f| {
            let body = prompt_frame(f, context, prompt);
            let items: Vec<ListItem> = options.iter().map(|o| ListItem::new(*o)).collect();
            let list = List::new(items)
                .block(Block::bordered().title(" select "))
                .highlight_style(
                    Style::new()
                        .bg(Color::Indexed(238))
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▸ ");
            let mut state = ListState::default();
            state.select(Some(cur));
            f.render_stateful_widget(list, body, &mut state);
        })?;
        if let Event::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
        {
            match k.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    cur = if cur == 0 { options.len() - 1 } else { cur - 1 };
                }
                KeyCode::Down | KeyCode::Char('j') => cur = (cur + 1) % options.len(),
                KeyCode::Enter => return Ok(Some(cur)),
                KeyCode::Esc => return Ok(None),
                _ => {}
            }
        }
    }
}

fn ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Re-prompt until `valid` accepts the text (or the user cancels). Keeps bad
/// input inside the TUI instead of exiting after the whole flow.
fn prompt_text_valid(
    term: &mut DefaultTerminal,
    prompt: &str,
    context: &[String],
    accept: impl Fn(char) -> bool,
    valid: impl Fn(&str) -> bool,
    hint: &str,
) -> io::Result<Option<String>> {
    let mut ctx = context.to_vec();
    loop {
        let Some(text) = prompt_text(term, prompt, &ctx, &accept)? else {
            return Ok(None);
        };
        if valid(&text) {
            return Ok(Some(text));
        }
        ctx = context.to_vec();
        ctx.push(format!("invalid: {hint}"));
    }
}

/// `bonsai branch` with no name: pick the name and kind in a TUI, then graft.
fn branch_interactive() -> io::Result<()> {
    require_managed("branch");
    let mut term = ratatui::init();
    let picked = (|| -> io::Result<Option<(String, BranchMode)>> {
        let Some(name) = prompt_text_valid(
            &mut term,
            "branch name (snake_case)",
            &[],
            ident_char,
            is_ident,
            "must be a snake_case identifier",
        )?
        else {
            return Ok(None);
        };
        let ctx = vec![format!("branch: {name}")];
        let Some(kind) = prompt_menu(
            &mut term,
            "branch kind",
            &ctx,
            &[
                "consumer — taps nutrients (subscriber)",
                "producer — feeds nutrients (publisher)",
                "duplex — both",
                "roots — I/O bridge: OS threads ⇄ the tree (std trees)",
            ],
        )?
        else {
            return Ok(None);
        };
        let mode = [
            BranchMode::Consumer,
            BranchMode::Producer,
            BranchMode::Duplex,
            BranchMode::Roots,
        ][kind];
        Ok(Some((name, mode)))
    })();
    ratatui::restore();

    match picked? {
        Some((name, mode)) => add_branch(&name, mode),
        None => {
            println!("cancelled");
            Ok(())
        }
    }
}

/// `bonsai feed` with no name: enter the nutrient name and its fields in a TUI.
fn add_nutrient_interactive() -> io::Result<()> {
    require_managed("feed");
    let mut term = ratatui::init();
    // (name, field specs, path)
    type Entered = (String, Vec<String>, Option<flow::PathCfg>);
    let entered = (|| -> io::Result<Option<Entered>> {
        let Some(name) = prompt_text_valid(
            &mut term,
            "nutrient name (UpperCamelCase)",
            &[],
            ident_char,
            is_variant,
            "must be an UpperCamelCase identifier",
        )?
        else {
            return Ok(None);
        };
        let mut fields: Vec<String> = Vec::new();
        loop {
            let mut ctx = vec![format!("nutrient: {name}")];
            ctx.extend(fields.iter().map(|f| format!("  {f}")));
            let Some(fname) = prompt_text_valid(
                &mut term,
                "field name (empty = done)",
                &ctx,
                ident_char,
                |t| t.is_empty() || is_ident(t),
                "must be a snake_case identifier (or empty to finish)",
            )?
            else {
                return Ok(None);
            };
            if fname.is_empty() {
                break;
            }
            ctx.push(format!("  {fname}: …"));
            let Some(ftype) = prompt_text(&mut term, &format!("type for `{fname}`"), &ctx, |c| {
                !c.is_whitespace()
            })?
            else {
                return Ok(None);
            };
            fields.push(format!("{fname}:{ftype}"));
        }
        let mut ctx = vec![format!("nutrient: {name}")];
        ctx.extend(fields.iter().map(|f| format!("  {f}")));
        let Some(shape) = prompt_menu(
            &mut term,
            "path shape",
            &ctx,
            &[
                "broadcast — one → many; never waits, laggards lose the oldest (events)",
                "directed — many → one; waits while full, never drops (commands, TX)",
                "state — latest value; late tappers still get it (status, mode)",
            ],
        )?
        else {
            return Ok(None);
        };
        let shape = flow::SHAPES[shape];
        if shape == flow::Shape::State {
            return Ok(Some((name, fields, Some(flow::PathCfg::new(shape, None)))));
        }
        ctx.push(format!("  path: {}", shape.name()));
        let Some(cap) = prompt_text_valid(
            &mut term,
            &format!("capacity (empty = {})", shape.default_cap()),
            &ctx,
            |c| c.is_ascii_digit(),
            |t| t.is_empty() || t.parse::<usize>().is_ok_and(|n| n >= 1),
            "a positive number (or empty for the default)",
        )?
        else {
            return Ok(None);
        };
        let cfg = flow::PathCfg::new(shape, cap.parse().ok());
        Ok(Some((name, fields, Some(cfg))))
    })();
    ratatui::restore();

    match entered? {
        Some((name, fields, cfg)) => add_nutrient(&name, &fields, cfg),
        None => {
            println!("cancelled");
            Ok(())
        }
    }
}

// ---------------------------------------------------------------------------
// `branch <name>`: graft a new subsystem onto the trunk of the tree in the cwd.
// ---------------------------------------------------------------------------

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    // Guards against the embedded template silently dropping dotfiles — a
    // missing .cargo/config.toml would produce a project that can't build.
    // Walks every board, so a board added to BOARDS without its template fails.
    #[test]
    fn embedded_template_includes_all_files() {
        const COMMON: &[&str] = &[
            ".gitignore",
            "Cargo.toml",
            "cargo-generate.toml",
            "src/main.rs",
            "src/trunk.rs",
            "src/pulse.rs",
            "src/branches/mod.rs",
            "src/sap.rs",
            "bonsai.toml",
            ".cargo/config.toml",
        ];
        for &board in &board_names() {
            let sub = format!("linux/{board}");
            let dir = TEMPLATES
                .get_dir(&sub)
                .unwrap_or_else(|| panic!("{sub} embedded in the binary"));
            let dest = std::env::temp_dir().join(format!("bonsai-test-extract-{board}"));
            let _ = std::fs::remove_dir_all(&dest);
            extract_dir(dir, dir.path(), &dest).unwrap();
            for f in COMMON {
                assert!(dest.join(f).is_file(), "embedded {sub} is missing {f}");
            }
            let _ = std::fs::remove_dir_all(&dest);
        }
    }

    // The empty registry's doc comment mentions `// bonsai:mod`; grafting must
    // still insert before the real marker line, not inside the comment.
    #[test]
    fn marker_matches_whole_line_not_substring() {
        let src = "//! keep the // bonsai:mod marker intact\n\n// bonsai:mod\n".to_string();
        let out = insert_before_marker(Path::new("mod.rs"), src, "// bonsai:mod", "pub mod x;\n");
        assert!(
            out.contains("pub mod x;\n// bonsai:mod\n"),
            "inserted wrong:\n{out}"
        );
        assert!(
            out.starts_with("//! keep the // bonsai:mod marker intact\n"),
            "doc line was split:\n{out}"
        );
    }

    // snip removes only the targeted module line, leaving siblings and the
    // marker untouched.
    #[test]
    fn snip_removes_module_line() {
        let src = "//! registry\npub mod imu;\npub mod gps;\n// bonsai:mod\n".to_string();
        let out = remove_line(src, |l| l.trim() == "pub mod imu;").unwrap();
        assert_eq!(out, "//! registry\npub mod gps;\n// bonsai:mod\n");
    }

    // A start call hand-edited to hand over hardware is still matched by the
    // `branches::<name>::start(` prefix, and the marker survives.
    #[test]
    fn snip_removes_hand_edited_start_line() {
        let src = "    branches::imu::start(&spawner, &trunk, Output::new(p.PIN_25, Level::Low));\n    // bonsai:start\n";
        let out =
            remove_balanced_span(src, |l| l.trim_start().starts_with("branches::imu::start("))
                .unwrap();
        assert_eq!(out, "    // bonsai:start\n");
    }

    // A rustfmt-wrapped multi-line start call comes out whole — no dangling args.
    #[test]
    fn snip_removes_wrapped_start_call() {
        let src = "    branches::imu::start(\n        &spawner,\n        &trunk,\n        Output::new(p.PIN_25, Level::Low),\n    );\n    // bonsai:start\n";
        let out =
            remove_balanced_span(src, |l| l.trim_start().starts_with("branches::imu::start("))
                .unwrap();
        assert_eq!(out, "    // bonsai:start\n");
    }

    // `imu` must not match `imu2` — the `::start(` boundary guards this.
    #[test]
    fn snip_start_prefix_does_not_match_sibling() {
        let src = "    branches::imu2::start(&spawner, &trunk);\n";
        assert!(
            remove_balanced_span(src, |l| l.trim_start().starts_with("branches::imu::start("))
                .is_none()
        );
    }

    // No match → None, which drives the fatal (mod) / warn (start) branches.
    #[test]
    fn snip_missing_line_returns_none() {
        let src = "//! nothing here\n".to_string();
        assert!(remove_line(src, |l| l.trim() == "pub mod imu;").is_none());
    }

    // regrow recovers the chip from the board alone.
    #[test]
    fn regrow_chip_from_board() {
        assert_eq!(chip_of("zero-2w"), Some("bcm2710a1"));
        assert_eq!(chip_of("zero-w"), Some("bcm2835"));
        assert_eq!(chip_of("pi5"), Some("bcm2712"));
        assert_eq!(chip_of("host"), Some("native"));
        assert_eq!(chip_of("pico"), None);
    }

    // Every board's template stamps its own board and chip into Cargo.toml.
    #[test]
    fn templates_stamp_their_board_and_chip() {
        for &(board, chip, _) in BOARDS {
            let dir = TEMPLATES.get_dir(format!("linux/{board}")).unwrap();
            let manifest = dir
                .get_file(format!("linux/{board}/Cargo.toml"))
                .unwrap()
                .contents_utf8()
                .unwrap();
            assert_eq!(parse_board(manifest).as_deref(), Some(board));
            assert!(
                manifest.contains(&format!("for {board} ({chip})")),
                "{board}: stamp names another chip"
            );
        }
    }

    // regrow reads the board back out of the Cargo.toml stamp (both HAL styles).
    #[test]
    fn regrow_parse_board_from_stamp() {
        let rpi = "# Generated by bonsai for zero-2w (bcm2710a1) — Linux trunk + branches.\n";
        assert_eq!(parse_board(rpi).as_deref(), Some("zero-2w"));
        let pi5 = "# Generated by bonsai for pi5 (bcm2712) — Linux trunk + branches.\n";
        assert_eq!(parse_board(pi5).as_deref(), Some("pi5"));
        assert_eq!(parse_board("no stamp here\n"), None);
    }

    // regrow keeps the project's name, but must not mistake a raw template for one.
    #[test]
    fn regrow_parse_package_name() {
        let toml = "[package]\nname = \"myrover\"\nversion = \"0.1.0\"\n";
        assert_eq!(parse_package_name(toml).as_deref(), Some("myrover"));
        assert!(parse_package_name("name = \"{{project-name}}\"\n").is_none());
    }

    // The build tools read the target from .cargo/config.toml. It must take the
    // [build] target, not the [target.<t>] header, and ignore an unrendered
    // template; a host tree has none.
    #[test]
    fn parse_target_from_cargo_config() {
        let pi = "[build]\ntarget = \"aarch64-unknown-linux-gnu\"\n\n\
                  [target.aarch64-unknown-linux-gnu]\nlinker = \"aarch64-linux-gnu-gcc\"\n";
        assert_eq!(
            parse_target(pi).as_deref(),
            Some("aarch64-unknown-linux-gnu")
        );
        assert!(parse_target(include_str!("../templates/linux/host/.cargo/config.toml")).is_none());
        assert!(parse_target("target = \"{{target}}\"\n").is_none());
        assert!(parse_target("[build]\n").is_none());
    }

    // `feed` inserts a variant above the marker, matched as a whole line (a doc
    // comment mentioning the marker isn't mistaken for it).
    #[test]
    fn feed_inserts_variant_before_marker() {
        let src = "//! keep the // bonsai:nutrient marker\npub enum Nutrient {\n    Beat,\n    // bonsai:nutrient\n}\n".to_string();
        let out = insert_before_marker(
            Path::new("trunk.rs"),
            src,
            "// bonsai:nutrient",
            "    Target,\n",
        );
        assert!(
            out.contains("    Target,\n    // bonsai:nutrient\n"),
            "inserted wrong:\n{out}"
        );
        assert!(
            out.starts_with("//! keep the // bonsai:nutrient marker\n"),
            "doc line split:\n{out}"
        );
    }

    // The matcher `remove_nutrient` uses: name + a delimiter char.
    fn variant_matcher(name: &'static str) -> impl Fn(&str) -> bool {
        move |l: &str| {
            l.trim_start()
                .strip_prefix(name)
                .is_some_and(|rest| matches!(rest.chars().next(), Some(',' | ' ' | '(' | '{')))
        }
    }

    // `starve` removes only the targeted variant, leaving siblings + marker.
    #[test]
    fn starve_removes_variant_line() {
        let src = "pub enum Nutrient {\n    Beat,\n    Target,\n    // bonsai:nutrient\n}\n";
        let out = remove_balanced_span(src, variant_matcher("Target")).unwrap();
        assert_eq!(
            out,
            "pub enum Nutrient {\n    Beat,\n    // bonsai:nutrient\n}\n"
        );
    }

    // A variant hand-edited to carry a payload is still matched by prefix.
    #[test]
    fn starve_removes_payload_variant() {
        let src = "    Beat,\n    Target { pos: i32 },\n";
        let out = remove_balanced_span(src, variant_matcher("Target")).unwrap();
        assert_eq!(out, "    Beat,\n");
    }

    // A variant reformatted across lines comes out whole — no orphaned braces.
    #[test]
    fn starve_removes_multiline_variant() {
        let src = "    Beat,\n    Target {\n        pos: i32,\n        vel: i16,\n    },\n    // bonsai:nutrient\n";
        let out = remove_balanced_span(src, variant_matcher("Target")).unwrap();
        assert_eq!(out, "    Beat,\n    // bonsai:nutrient\n");
    }

    // `Beat` must not match `BeatFast` — the delimiter boundary guards this.
    #[test]
    fn starve_prefix_does_not_match_sibling() {
        let src = "    BeatFast,\n";
        assert!(remove_balanced_span(src, variant_matcher("Beat")).is_none());
    }

    // No such variant → None, which drives the fatal branch.
    #[test]
    fn starve_missing_variant_returns_none() {
        let src = "    Beat,\n    // bonsai:nutrient\n";
        assert!(remove_balanced_span(src, variant_matcher("Target")).is_none());
    }

    // A nutrient name must be a valid UpperCamelCase identifier.
    #[test]
    fn nutrient_name_must_be_uppercase_ident() {
        assert!(is_variant("Target"));
        assert!(is_variant("BeatFast"));
        assert!(!is_variant("target")); // lowercase first
        assert!(!is_variant("2Fast")); // leading digit
        assert!(!is_variant("has space"));
    }

    // `insert_indented_before` puts content above a whole-line marker, matching its
    // indentation; no marker → None.
    #[test]
    fn insert_indented_before_matches_indent() {
        let src = "        match n {\n            // bonsai:nutrient-arm\n            _ => {}\n        }\n";
        let out = insert_indented_before(src, "// bonsai:nutrient-arm", "Nutrient::Target => {}")
            .unwrap();
        assert!(
            out.contains(
                "            Nutrient::Target => {}\n            // bonsai:nutrient-arm\n"
            ),
            "inserted wrong (or indent lost):\n{out}"
        );
    }

    #[test]
    fn insert_indented_before_none_without_marker() {
        assert!(
            insert_indented_before("    match n { _ => {} }\n", "// bonsai:nutrient-arm", "x")
                .is_none()
        );
    }

    // `arm_lhs_for` binds a payload variant with `{ .. }`, a unit one plainly.
    #[test]
    fn arm_lhs_for_unit_and_payload() {
        let trunk = "pub enum Nutrient {\n    Beat,\n    Target { pos: i32 },\n}\n";
        assert_eq!(arm_lhs_for(trunk, "Beat"), "Nutrient::Beat");
        assert_eq!(arm_lhs_for(trunk, "Target"), "Nutrient::Target { .. }");
    }

    // `tap` adds an arm above the marker (before the catch-all); `untap` removes it.
    #[test]
    fn tap_untap_round_trip() {
        let trunk = "pub enum Nutrient {\n    Target { pos: i32 },\n}\n";
        let branch = "        match n {\n            // bonsai:nutrient-arm\n            _ => {}\n        }\n";
        let arm = format!("{} => {{ /* TODO */ }}", arm_lhs_for(trunk, "Target"));
        let tapped = insert_indented_before(branch, "// bonsai:nutrient-arm", &arm).unwrap();
        assert!(
            tapped.contains("Nutrient::Target { .. } => { /* TODO */ }"),
            "{tapped}"
        );
        assert!(tapped.lines().any(|l| is_arm_for(l, "Target")));
        let untapped = remove_balanced_span(&tapped, |l| is_arm_for(l, "Target")).unwrap();
        assert_eq!(untapped, branch);
    }

    // The regression the balanced remover exists for: an arm whose `/* TODO */`
    // grew into a multi-line body is removed whole, not just its first line.
    #[test]
    fn untap_removes_multiline_arm_body() {
        let branch = "        match n {\n            Nutrient::Target { pos } => {\n                info!(\"target {}\", pos);\n                led.toggle();\n            }\n            // bonsai:nutrient-arm\n            _ => {}\n        }\n";
        let out = remove_balanced_span(branch, |l| is_arm_for(l, "Target")).unwrap();
        assert_eq!(
            out,
            "        match n {\n            // bonsai:nutrient-arm\n            _ => {}\n        }\n"
        );
    }

    // `release` adds a publish above the emit marker; `unrelease` removes it. The
    // matcher respects the name boundary (Target ≠ TargetLock).
    #[test]
    fn release_unrelease_round_trip() {
        let loop_src = "        // bonsai:emit\n        let _ = &sap;\n";
        let out = insert_indented_before(
            loop_src,
            "// bonsai:emit",
            "sap.release(Nutrient::Target).await;",
        )
        .unwrap();
        assert!(
            out.contains("        sap.release(Nutrient::Target).await;\n        // bonsai:emit\n"),
            "{out}"
        );
        assert!(out.lines().any(|l| is_release_of(l, "Target")));
        assert!(!out.lines().any(|l| is_release_of(l, "Tar")));
        let back = remove_balanced_span(&out, |l| is_release_of(l, "Target")).unwrap();
        assert_eq!(back, loop_src);
    }

    // A publish whose payload ctor was filled in across lines comes out whole.
    #[test]
    fn unrelease_removes_multiline_publish() {
        let src = "        sap.release(Nutrient::Target {\n            pos: reading,\n        })\n        .await;\n        // bonsai:emit\n        let _ = &sap;\n";
        let out = remove_balanced_span(src, |l| is_release_of(l, "Target")).unwrap();
        assert_eq!(out, "        // bonsai:emit\n        let _ = &sap;\n");
    }

    // `list` reads the variant names out of the enum, skipping docs + the marker,
    // for both unit and payload variants.
    #[test]
    fn parse_nutrients_lists_variants() {
        let trunk = "use x;\npub enum Nutrient {\n    /// doc\n    Beat,\n    Target { pos: i32 },\n    // bonsai:nutrient\n}\nfn other() {}\n";
        assert_eq!(parse_nutrients(trunk), vec!["Beat", "Target"]);
    }

    // `feed`'s field parser trims and pairs name:type (error paths exit the
    // process, so only the happy path is unit-testable).
    // Consumer + duplex + roots have a `match Nutrient` (so they carry the arm
    // marker for tap/starve); producer releases only. All draw on the generated
    // sap. Roots leaves its transport to the user.
    #[test]
    fn branch_mode_templates() {
        for m in [BranchMode::Consumer, BranchMode::Duplex, BranchMode::Roots] {
            assert!(m.template().contains("// bonsai:nutrient-arm"));
            assert!(m.template().contains("sap::{{branch_name}}::Taps::new()"));
        }
        assert!(
            !BranchMode::Producer
                .template()
                .contains("// bonsai:nutrient-arm")
        );
        for m in [BranchMode::Producer, BranchMode::Duplex, BranchMode::Roots] {
            assert!(m.template().contains("// bonsai:emit"));
            assert!(m.template().contains("trunk.sap()"));
        }
        let roots = BranchMode::Roots.template();
        assert!(roots.contains("roots::bridge(") && !roots.contains("TcpStream::connect"));
        assert!(
            !BranchMode::Producer
                .template()
                .contains("Timer::after(Duration")
        );
    }

    // Every board's shipped src/sap.rs must be exactly what `bonsai sync` would
    // generate from that template's own trunk/pulse/bonsai.toml — otherwise a
    // freshly planted tree would rewrite it on its first wiring command.
    #[test]
    fn template_sap_matches_generator() {
        for &board in &board_names() {
            let sub = format!("linux/{board}");
            let file = |f: &str| {
                let path = format!("{sub}/{f}");
                TEMPLATES
                    .get_file(&path)
                    .and_then(|f| f.contents_utf8())
                    .unwrap_or_else(|| panic!("{path} embedded"))
            };
            let variants = flow::parse_variants(file("src/trunk.rs"));
            let nutrients: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
            let pulse = flow::Node::scan("pulse", file("src/pulse.rs"), &nutrients);
            assert_eq!(pulse.taps, vec!["Beat"], "{sub}: pulse taps Beat");
            assert_eq!(pulse.releases, vec!["Beat"], "{sub}: pulse releases Beat");
            let cfg = flow::Config::parse(file("bonsai.toml")).unwrap();
            let g = flow::Graph::build(&variants, &cfg, vec![pulse]);
            assert!(
                flow::render(&g) == file("src/sap.rs"),
                "{sub}/src/sap.rs is stale — run `bonsai sync` in templates/{sub}"
            );
            assert!(g.warnings().is_empty(), "{sub}: {:?}", g.warnings());
            // The sap lives under the trunk, exactly as `migrate_sap_module`
            // places it in older trees.
            assert!(
                file("src/trunk.rs").contains(SAP_MOD_IN_TRUNK),
                "{sub}: trunk.rs declares the sap as SAP_MOD_IN_TRUNK does"
            );
            assert!(
                file("src/main.rs").contains("use trunk::sap;")
                    && !file("src/main.rs").contains("mod sap;"),
                "{sub}: main.rs reaches the sap through the trunk"
            );
        }
    }

    // The graph reads releases wherever they sit on a line; editing commands
    // only ever match a line that *is* the call.
    #[test]
    fn release_detection_loose_and_strict() {
        let l = "        let _ = sap.try_release(Nutrient::Arm);";
        assert!(mentions_release_of(l, "Arm") && !is_release_of(l, "Arm"));
        assert!(!mentions_awaited_release_of(l, "Arm"));
        let l = "            Nutrient::Beat => sap.release(Nutrient::Ack).await,";
        assert!(mentions_awaited_release_of(l, "Ack") && !mentions_release_of(l, "Ac"));
        assert!(is_release_of(
            "    sap.release(Nutrient::Ack).await;",
            "Ack"
        ));
        assert!(!is_release_of(
            "    sap.publish_immediate(Nutrient::Ack);",
            "Ack"
        )); // the pre-sap form isn't a release any more
    }

    #[test]
    fn feed_args_split_flags_from_fields() {
        let args: Vec<String> = ["len:u16", "--directed", "--cap", "64", "buf:[u8;8]"]
            .map(String::from)
            .to_vec();
        let (fields, shape, cap) = parse_feed_args(&args);
        assert_eq!(fields, vec!["len:u16", "buf:[u8;8]"]);
        assert_eq!(shape, Some(flow::Shape::Directed));
        assert_eq!(cap, Some(64));
        let (_, shape, cap) = parse_feed_args(&["--state".to_string(), "--cap=2".to_string()]);
        assert_eq!((shape, cap), (Some(flow::Shape::State), Some(2)));
    }

    #[test]
    fn parse_fields_pairs_name_and_type() {
        let fields = vec!["pos:i32".to_string(), "vel: u16".to_string()];
        assert_eq!(
            parse_fields(&fields),
            vec![
                ("pos".to_string(), "i32".to_string()),
                ("vel".to_string(), "u16".to_string()),
            ]
        );
    }

    // End-to-end fs round trip on a fixture tree: graft → feed → tap → expand the
    // arm body across lines → untap → snip → starve leaves every file exactly as
    // it started. This is the coverage the pure-helper tests can't give: the real
    // command-level edits, including balanced multi-line removal.
    //
    // The commands operate on the cwd, and set_current_dir is process-wide — keep
    // this the ONLY test that changes cwd (all others use absolute/temp paths).
    #[test]
    fn fs_round_trip_graft_wire_snip_starve() {
        let root = std::env::temp_dir().join("bonsai-test-roundtrip");
        let _ = std::fs::remove_dir_all(&root);
        let tmpl = TEMPLATES.get_dir("linux/zero-w").unwrap();
        extract_dir(tmpl, tmpl.path(), &root).unwrap();
        let fresh: Vec<(&str, String)> = [
            "src/main.rs",
            "src/branches/mod.rs",
            "src/trunk.rs",
            "src/sap.rs",
            "bonsai.toml",
        ]
        .into_iter()
        .map(|f| (f, std::fs::read_to_string(root.join(f)).unwrap()))
        .collect();

        let old_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();

        add_branch("imu", BranchMode::Consumer).unwrap();
        let imu_fresh = std::fs::read_to_string(root.join("src/branches/imu.rs")).unwrap();

        add_nutrient("Target", &["pos:i32".to_string()], None).unwrap();
        tap("imu", "Target").unwrap();

        // Grow the tapped arm into a real multi-line body — the regression case.
        let imu = std::fs::read_to_string(root.join("src/branches/imu.rs")).unwrap();
        let imu = imu.replace(
            "Nutrient::Target { .. } => { /* TODO */ }",
            "Nutrient::Target { pos } => {\n                let _ = pos;\n            }",
        );
        std::fs::write(root.join("src/branches/imu.rs"), imu).unwrap();

        untap("imu", "Target").unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("src/branches/imu.rs")).unwrap(),
            imu_fresh,
            "untap must remove the whole multi-line arm, restoring the fresh scaffold"
        );

        remove_branch("imu").unwrap();
        remove_nutrient("Target").unwrap();
        std::env::set_current_dir(&old_cwd).unwrap();

        for (f, before) in &fresh {
            assert_eq!(
                &std::fs::read_to_string(root.join(f)).unwrap(),
                before,
                "{f} not restored"
            );
        }
        assert!(!root.join("src/branches/imu.rs").exists());
        let _ = std::fs::remove_dir_all(&root);

        // Phase 2 — a managed tree (a real template, with its generated sap): the
        // same round trip also regenerates src/sap.rs and bonsai.toml at each step,
        // and unwinding it restores every file byte for byte.
        let root = std::env::temp_dir().join("bonsai-test-roundtrip-sap");
        let _ = std::fs::remove_dir_all(&root);
        let tmpl = TEMPLATES.get_dir("linux/zero-w").unwrap();
        extract_dir(tmpl, tmpl.path(), &root).unwrap();
        let read = |f: &str| std::fs::read_to_string(root.join(f)).unwrap();
        let fresh: Vec<(&str, String)> = [
            "src/main.rs",
            "src/branches/mod.rs",
            "src/trunk.rs",
            "src/sap.rs",
            "bonsai.toml",
        ]
        .into_iter()
        .map(|f| (f, read(f)))
        .collect();
        std::env::set_current_dir(&root).unwrap();

        add_branch("ctl", BranchMode::Duplex).unwrap();
        assert!(read("src/sap.rs").contains("pub mod ctl {"));
        let cmd = flow::PathCfg::new(flow::Shape::Directed, Some(16));
        add_nutrient("Cmd", &[], Some(cmd)).unwrap();
        assert!(read("bonsai.toml").contains("[nutrients.Cmd]\nshape = \"directed\"\ncap = 16\n"));
        tap("ctl", "Cmd").unwrap();
        release("ctl", "Cmd").unwrap();
        let sap = read("src/sap.rs");
        assert!(sap.contains("static CMD_PATH: Channel<M, (), 16>"), "{sap}");
        assert!(
            sap.contains("CMD_PATH.poll_receive(cx).map(cmd_nutrient)"),
            "{sap}"
        );
        assert!(
            sap.contains("//!   Cmd   directed   cap 16   ctl → ctl"),
            "{sap}"
        );
        assert!(read("src/branches/ctl.rs").contains("sap.release(Nutrient::Cmd).await;"));
        // …and the wiring it now has is the self-deadlock the warnings name.
        assert!(
            tree_graph()
                .warnings()
                .iter()
                .any(|w| w.contains("deadlock risk: `ctl`"))
        );

        unrelease("ctl", "Cmd").unwrap();
        untap("ctl", "Cmd").unwrap();
        remove_branch("ctl").unwrap();
        remove_nutrient("Cmd").unwrap();

        // Phase 3 — a tree from the first sap release declares `mod sap;` in
        // main.rs. The next sync moves it under the trunk (the trunk ends up
        // exactly as the template has it), and a second sync changes nothing.
        let main_new = read("src/main.rs");
        let trunk_new = read("src/trunk.rs");
        let main_old = main_new
            .replace("use trunk::sap; // the generated sap lives under the trunk (src/sap.rs)\n", "")
            .replace(
                "mod trunk;\n",
                "#[rustfmt::skip] // generated by bonsai — `bonsai sync` rewrites it\nmod sap;\nmod trunk;\n",
            );
        std::fs::write(root.join("src/main.rs"), &main_old).unwrap();
        std::fs::write(
            root.join("src/trunk.rs"),
            trunk_new.replace(SAP_MOD_IN_TRUNK, "pub use crate::sap::Sap;\n"),
        )
        .unwrap();
        sync_sap().unwrap();
        let main_migrated = read("src/main.rs");
        assert_eq!(read("src/trunk.rs"), trunk_new);
        assert!(!main_migrated.contains("mod sap;"), "{main_migrated}");
        assert!(!main_migrated.contains("rustfmt::skip"), "{main_migrated}");
        assert!(main_migrated.contains("use trunk::sap;"), "{main_migrated}");
        sync_sap().unwrap();
        assert_eq!(read("src/main.rs"), main_migrated);
        std::fs::write(root.join("src/main.rs"), &main_new).unwrap();
        std::env::set_current_dir(&old_cwd).unwrap();
        for (f, before) in &fresh {
            assert_eq!(&read(f), before, "{f} not restored");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    // Auto-patch: `starve` removes the arm by `Nutrient::<name>` + delimiter, so a
    // sibling (`TargetLock`) and a payload arm are handled correctly.
    #[test]
    fn arm_matcher_boundaries() {
        assert!(is_arm_for("            Nutrient::Target => {}", "Target"));
        assert!(is_arm_for("    Nutrient::Target { .. } => {}", "Target")); // payload
        assert!(!is_arm_for(
            "            Nutrient::TargetLock => {}",
            "Target"
        )); // sibling
        assert!(!is_arm_for("            Nutrient::Beat => {}", "Target"));
    }

    #[test]
    fn starve_removes_arm_line() {
        let src = "            Nutrient::Beat => {}\n            Nutrient::Target => {}\n            // bonsai:nutrient-arm\n".to_string();
        let out = remove_line(src, |l| is_arm_for(l, "Target")).unwrap();
        assert_eq!(
            out,
            "            Nutrient::Beat => {}\n            // bonsai:nutrient-arm\n"
        );
    }
}

fn add_branch(name: &str, mode: BranchMode) -> io::Result<()> {
    if !is_ident(name) {
        eprintln!("branch name must be a snake_case identifier, got {name:?}");
        std::process::exit(2);
    }

    let mod_rs = Path::new("src/branches/mod.rs");
    let main_rs = Path::new("src/main.rs");
    if !mod_rs.is_file() || !main_rs.is_file() {
        eprintln!("run this inside a bonsai tree (no src/branches/mod.rs + src/main.rs here)");
        std::process::exit(1);
    }

    // The generated sap names each node's taps `sap::<name>::Taps`, and the
    // pulse is already a node.
    if name == "pulse" {
        eprintln!("`pulse` is the tree's built-in heartbeat (src/pulse.rs) — pick another name.");
        std::process::exit(2);
    }

    let branch_rs = Path::new("src/branches").join(format!("{name}.rs"));
    if branch_rs.exists() {
        eprintln!("branch already exists: {}", branch_rs.display());
        std::process::exit(1);
    }

    require_managed("branch");
    // Register the module in branches/mod.rs, and start it in the trunk
    // (main.rs) where the hardware is handed out. Compute both edits first so a
    // missing marker aborts before we write anything (no half-grafted tree).
    let mod_src = insert_before_marker(
        mod_rs,
        std::fs::read_to_string(mod_rs)?,
        "// bonsai:mod",
        &format!("pub mod {name};\n"),
    );
    let mut main_src = insert_before_marker(
        main_rs,
        std::fs::read_to_string(main_rs)?,
        "// bonsai:start",
        &format!("    branches::{name}::start(&spawner, &trunk);\n"),
    );
    // The first root also brings the shared bridge and the crate its `run` needs.
    let mut manifest = None;
    let new_bridge = mode == BranchMode::Roots && !Path::new(ROOTS_RS).exists();
    if mode == BranchMode::Roots {
        if !main_src.lines().any(|l| l.trim() == ROOTS_MOD) {
            main_src = with_roots_mod(&main_src).unwrap_or_else(|| {
                eprintln!("no `mod pulse;` line in src/main.rs to put `{ROOTS_MOD}` after");
                std::process::exit(1);
            });
        }
        manifest = with_dependency(&std::fs::read_to_string("Cargo.toml")?, EMBASSY_FUTURES)?;
    }

    std::fs::write(&branch_rs, mode.template().replace("{{branch_name}}", name))?;
    std::fs::write(mod_rs, mod_src)?;
    std::fs::write(main_rs, main_src)?;
    if new_bridge {
        std::fs::write(ROOTS_RS, ROOTS_BRIDGE)?;
    }
    if let Some(manifest) = &manifest {
        std::fs::write("Cargo.toml", manifest)?;
    }

    println!("added {} `{name}`:", mode.label());
    println!("  + {}", branch_rs.display());
    if new_bridge {
        println!("  + {ROOTS_RS} (the bridge every root shares)");
    }
    println!("  ~ src/branches/mod.rs (module registered)");
    println!("  ~ src/main.rs (started in the trunk)");
    if manifest.is_some() {
        println!("  ~ Cargo.toml ({} added)", EMBASSY_FUTURES.0);
    }
    sync_sap()
}

/// `snip <name>`: prune a branch off the tree in the cwd — the inverse of
/// `branch`. Deletes `src/branches/<name>.rs` and reverses both wiring edits.
fn remove_branch(name: &str) -> io::Result<()> {
    if !is_ident(name) {
        eprintln!("branch name must be a snake_case identifier, got {name:?}");
        std::process::exit(2);
    }

    let mod_rs = Path::new("src/branches/mod.rs");
    let main_rs = Path::new("src/main.rs");
    if !mod_rs.is_file() || !main_rs.is_file() {
        eprintln!("run this inside a bonsai tree (no src/branches/mod.rs + src/main.rs here)");
        std::process::exit(1);
    }
    require_managed("snip");

    let branch_rs = Path::new("src/branches").join(format!("{name}.rs"));
    if !branch_rs.is_file() {
        eprintln!("no such branch: {}", branch_rs.display());
        std::process::exit(1);
    }

    // Reverse both wiring edits `add_branch` made. Compute them before writing
    // anything so an inconsistent tree aborts before we delete the file.
    let mod_line = format!("pub mod {name};");
    let Some(mod_src) = remove_line(std::fs::read_to_string(mod_rs)?, |l| l.trim() == mod_line)
    else {
        eprintln!(
            "`{mod_line}` not found in {} — is `{name}` really a branch?",
            mod_rs.display()
        );
        std::process::exit(1);
    };
    // The trunk's start call may have been hand-edited to hand over hardware, so
    // match the call by prefix rather than the exact scaffolded line. The
    // `::start(` boundary keeps `imu` from matching e.g. `imu2`. Balanced-span
    // removal takes a rustfmt-wrapped multi-line call out whole.
    let start_call = format!("branches::{name}::start(");
    let main_before = std::fs::read_to_string(main_rs)?;
    let main_src = remove_balanced_span(&main_before, |l| l.trim_start().starts_with(&start_call));
    let start_removed = main_src.is_some();
    let mut main_src = main_src.unwrap_or(main_before.clone());

    std::fs::remove_file(&branch_rs)?;
    std::fs::write(mod_rs, mod_src)?;

    // The last root takes the shared bridge with it.
    let drop_bridge = Path::new(ROOTS_RS).exists()
        && !nutrient_arm_files()
            .iter()
            .any(|p| file_contains(p, "roots::bridge("));
    if drop_bridge {
        main_src = remove_line(main_src.clone(), |l| l.trim() == ROOTS_MOD).unwrap_or(main_src);
        std::fs::remove_file(ROOTS_RS)?;
    }
    if main_src != main_before {
        std::fs::write(main_rs, &main_src)?;
    }

    println!("snipped branch `{name}`:");
    println!("  - {}", branch_rs.display());
    println!("  ~ src/branches/mod.rs (module unregistered)");
    if start_removed {
        println!("  ~ src/main.rs (start call removed)");
    } else {
        // Already gone (hand-removed) — the file + mod entry are still cleaned up.
        println!("  · src/main.rs (no `{start_call}…)` call to remove)");
    }
    if drop_bridge {
        println!("  - {ROOTS_RS} (no roots left)");
    }
    println!(
        "note: any hardware you handed `{name}` in the trunk is left in place — remove it if unused."
    );
    sync_sap()
}

/// Declare the roots bridge module just after `mod pulse;`. None if the trunk
/// has no such line.
fn with_roots_mod(main_src: &str) -> Option<String> {
    let mut offset = 0;
    for l in main_src.split_inclusive('\n') {
        offset += l.len();
        if l.trim() == "mod pulse;" {
            return Some(format!(
                "{}{ROOTS_MOD}\n{}",
                &main_src[..offset],
                &main_src[offset..]
            ));
        }
    }
    None
}

/// `manifest` with `name = "version"` added to `[dependencies]`, or None if
/// it's already there (any version the user picked is kept).
fn with_dependency(manifest: &str, (name, version): (&str, &str)) -> io::Result<Option<String>> {
    use std::str::FromStr;
    use toml_edit::DocumentMut;

    let mut doc = DocumentMut::from_str(manifest)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let deps = doc["dependencies"].as_table_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Cargo.toml has no [dependencies]",
        )
    })?;
    if deps.contains_key(name) {
        return Ok(None);
    }
    deps[name] = toml_edit::value(version);
    Ok(Some(doc.to_string()))
}

// ---------------------------------------------------------------------------
// `feed <Name>` / `starve <Name>`: add/remove a variant of the `Nutrient` enum
// in src/trunk.rs — the tree's shared event vocabulary. Marker-driven like
// `branch`, but a single-file edit. Both print a reminder of the `match Nutrient`
// arms the compiler will now want updated (adding a variant makes existing
// exhaustive matches non-exhaustive; removing one leaves dangling arms).
// ---------------------------------------------------------------------------

/// A `Nutrient` variant must be a valid Rust identifier that starts uppercase
/// (the enum-variant convention).
fn is_variant(name: &str) -> bool {
    is_ident(name) && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
}

/// The trunk in the cwd, or the standard "not a tree" error.
fn trunk_or_exit() -> &'static Path {
    let trunk_rs = Path::new("src/trunk.rs");
    if !trunk_rs.is_file() {
        eprintln!("run this inside a bonsai tree (no src/trunk.rs here)");
        std::process::exit(1);
    }
    trunk_rs
}

/// The files that carry a `match Nutrient` block, each with a
/// `// bonsai:nutrient-arm` marker: the pulse monitor + every branch.
fn nutrient_arm_files() -> Vec<PathBuf> {
    let mut branches: Vec<PathBuf> = std::fs::read_dir("src/branches")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "rs") && p.file_name().is_some_and(|n| n != "mod.rs")
        })
        .collect();
    branches.sort();
    std::iter::once(PathBuf::from("src/pulse.rs"))
        .chain(branches)
        .collect()
}

/// Insert `content` as a line just above the whole-line `marker`, indented to
/// match it. None if the file has no such marker.
fn insert_indented_before(src: &str, marker: &str, content: &str) -> Option<String> {
    let mut offset = 0;
    for l in src.split_inclusive('\n') {
        if l.trim() == marker {
            let indent: String = l.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            let line = format!("{indent}{content}\n");
            let mut out = String::with_capacity(src.len() + line.len());
            out.push_str(&src[..offset]);
            out.push_str(&line);
            out.push_str(&src[offset..]);
            return Some(out);
        }
        offset += l.len();
    }
    None
}

/// Whether a match line is the arm for `Nutrient::<name>` — `name` followed by a
/// pattern delimiter, so `Foo` isn't matched by a `starve Fo`, and a payload arm
/// (`Nutrient::Foo { .. } => …`) is still found.
fn is_arm_for(line: &str, name: &str) -> bool {
    line.trim_start()
        .strip_prefix("Nutrient::")
        .and_then(|r| r.strip_prefix(name))
        .is_some_and(|rest| matches!(rest.chars().next(), Some(' ' | '{' | '(')))
}

/// Parse `field:type` args into `(name, type)` pairs; exit(2) on a malformed one.
fn parse_fields(fields: &[String]) -> Vec<(String, String)> {
    fields
        .iter()
        .map(|f| {
            let Some((fname, fty)) = f.split_once(':') else {
                eprintln!("field must be `name:type`, got {f:?}");
                std::process::exit(2);
            };
            let (fname, fty) = (fname.trim(), fty.trim());
            if !is_ident(fname) || fty.is_empty() || fty.contains(char::is_whitespace) {
                eprintln!("field must be `name:type` with a snake_case name, got {f:?}");
                std::process::exit(2);
            }
            (fname.to_string(), fty.to_string())
        })
        .collect()
}

/// Split `feed`'s args into field specs and path flags: `--broadcast`,
/// `--directed`, `--state`, `--cap N` / `--cap=N`, anywhere in the list.
fn parse_feed_args(args: &[String]) -> (Vec<String>, Option<flow::Shape>, Option<usize>) {
    let usage = || -> ! {
        eprintln!(
            "usage: bonsai feed <Name> [--broadcast|--directed|--state] [--cap N] [field:type ...]"
        );
        std::process::exit(2);
    };
    let (mut fields, mut shape, mut cap) = (Vec::new(), None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let cap_arg = match a.as_str() {
            "--cap" => Some(it.next().unwrap_or_else(|| usage()).as_str()),
            a => a.strip_prefix("--cap="),
        };
        if let Some(n) = cap_arg {
            match n.parse::<usize>() {
                Ok(n) if n >= 1 => cap = Some(n),
                _ => {
                    eprintln!("--cap must be a positive number, got {n:?}");
                    std::process::exit(2);
                }
            }
        } else if let Some(flag) = a.strip_prefix("--") {
            let Some(s) = flow::Shape::parse(flag) else {
                usage()
            };
            if shape.is_some_and(|prev| prev != s) {
                eprintln!("pick one path shape, not several");
                std::process::exit(2);
            }
            shape = Some(s);
        } else {
            fields.push(a.clone());
        }
    }
    (fields, shape, cap)
}

/// `feed <Name> [--broadcast|--directed|--state] [--cap N] [field:type ...]`:
/// add a `Nutrient` variant to src/trunk.rs — a unit variant, or a struct
/// variant when fields are given — and give it a path in bonsai.toml.
fn add_nutrient(name: &str, fields: &[String], path: Option<flow::PathCfg>) -> io::Result<()> {
    if !is_variant(name) {
        eprintln!("nutrient name must be an UpperCamelCase identifier, got {name:?}");
        std::process::exit(2);
    }
    let trunk_rs = trunk_or_exit();
    require_managed("feed");
    let trunk_src = std::fs::read_to_string(trunk_rs)?;
    if parse_nutrients(&trunk_src).iter().any(|n| n == name) {
        eprintln!("`{name}` is already a nutrient (reshape its path with `bonsai path {name} …`).");
        std::process::exit(1);
    }

    // Build the variant declaration: unit, or a struct variant when fields given.
    let variant = if fields.is_empty() {
        format!("    {name},\n")
    } else {
        let body = parse_fields(fields)
            .iter()
            .map(|(f, t)| format!("{f}: {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("    {name} {{ {body} }},\n")
    };

    let src = insert_before_marker(trunk_rs, trunk_src, "// bonsai:nutrient", &variant);
    // Compute the config edit before writing anything, so a broken bonsai.toml
    // aborts cleanly.
    let cfg = path.unwrap_or_default();
    let config = edit_config(|c| flow::set_path(c, name, cfg))?;
    std::fs::write(trunk_rs, &src)?;
    std::fs::write(CONFIG, config)?;

    // Enum-only: every branch match has a `_ => {}` catch-all, so a new variant is
    // safe without touching any branch. Wire it into branches with `tap`/`release`.
    println!("fed nutrient `{name}`:");
    println!("  ~ src/trunk.rs (variant added to Nutrient)");
    let cap = match cfg.shape {
        flow::Shape::State => String::new(),
        _ => format!(", cap {}", cfg.cap),
    };
    println!("  ~ {CONFIG} (path: {}{cap})", cfg.shape.name());
    println!("wire it into branches: `bonsai tap <branch> {name}` / `release <branch> {name}`.");
    size_hint(&src, name);
    sync_sap()
}

/// Warn when a variant makes the `Nutrient` enum large: every slot on every
/// path is sized for the largest variant, so one big payload taxes them all.
fn size_hint(trunk: &str, name: &str) {
    const LARGE: usize = 64;
    let ptr = target_ptr_width();
    let variants = flow::parse_variants(trunk);
    let Some(v) = variants.iter().find(|v| v.name == name) else {
        return;
    };
    let Some((size, _)) = flow::variant_size(v, ptr) else {
        return; // a type we can't size from its name — no guess beats a wrong one
    };
    if size < LARGE {
        return;
    }
    let g = tree_graph();
    let msg = match g.layout {
        // Paths layout: each path's slots hold only its own payload, so the cost
        // stays on this nutrient's path.
        flow::Layout::Paths => {
            let Some(p) = g.paths.iter().find(|p| p.nutrient == name) else {
                return;
            };
            format!(
                "note: `{name}` is ≈ {size} B, so its path ({} slots) holds ≈ {} B",
                p.cfg.cap,
                size * p.cfg.cap
            )
        }
        // Trunk layout: one bus, every slot sized for the largest variant, so a
        // big one taxes all of them.
        flow::Layout::Trunk => {
            let largest = variants
                .iter()
                .filter(|o| o.name != name)
                .filter_map(|o| flow::variant_size(o, ptr))
                .map(|(s, _)| s)
                .max()
                .unwrap_or(0);
            if size <= largest {
                return;
            }
            let mut m = format!(
                "note: `{name}` is ≈ {size} B — and on a single bus every slot is sized for the\n\
                 \x20     largest Nutrient, so each one now costs that much"
            );
            if let Some(bytes) = g.sap_bytes(ptr) {
                m.push_str(&format!(" (the bus: ≈ {bytes} B)"));
            }
            m
        }
    };
    println!(
        "{msg}.\n      Consider boxing the payload (`Box<[u8; N]>`) so a slot holds a pointer, or a lower cap."
    );
}

/// Every slot the sap holds: queued paths hold `cap`, a state path holds one;
/// the trunk layout has just its one bus.
fn total_slots(g: &flow::Graph) -> usize {
    match g.layout {
        flow::Layout::Trunk => g.trunk_cap(),
        flow::Layout::Paths => g.paths.iter().map(|p| p.cfg.cap).sum(),
    }
}

/// The target's pointer width, for size estimates: 8 on a 64-bit target triple,
/// else 4 (32-bit Raspberry Pi OS). A tree with no target builds for this
/// computer, so it has this computer's width.
fn target_ptr_width() -> usize {
    let config = std::fs::read_to_string(".cargo/config.toml").unwrap_or_default();
    match parse_target(&config) {
        Some(t)
            if t.starts_with("aarch64") || t.starts_with("x86_64") || t.starts_with("riscv64") =>
        {
            8
        }
        Some(_) => 4,
        None => size_of::<usize>(),
    }
}

/// `starve <Name>`: remove a `Nutrient` variant from src/trunk.rs.
fn remove_nutrient(name: &str) -> io::Result<()> {
    if !is_variant(name) {
        eprintln!("nutrient name must be an UpperCamelCase identifier, got {name:?}");
        std::process::exit(2);
    }
    // `Beat` is the built-in pulse's nutrient — starving it would gut the pulse
    // (its heartbeat publish and monitor arm) while leaving the tree compiling,
    // a silent loss of the tree's vital sign.
    if name == "Beat" {
        eprintln!(
            "refusing to starve `Beat` — the built-in pulse depends on it (see src/pulse.rs)."
        );
        std::process::exit(1);
    }
    let trunk_rs = trunk_or_exit();
    require_managed("starve");

    // Match the variant by name + a delimiter, so a variant hand-edited to carry
    // a payload (`Target { pos: i32 },`) is still found and a sibling (`BeatFast`)
    // isn't. Balanced-span removal takes a multi-line variant out whole.
    let Some(src) = remove_balanced_span(&std::fs::read_to_string(trunk_rs)?, |l| {
        let t = l.trim_start();
        t.strip_prefix(name)
            .is_some_and(|rest| matches!(rest.chars().next(), Some(',' | ' ' | '(' | '{')))
    }) else {
        eprintln!(
            "no `{name}` variant in {} — is `{name}` really a nutrient?",
            trunk_rs.display()
        );
        std::process::exit(1);
    };
    let config = edit_config(|c| flow::remove_path(c, name))?;
    std::fs::write(trunk_rs, src)?;
    std::fs::write(CONFIG, config)?;

    println!("starved nutrient `{name}`:");
    println!("  ~ src/trunk.rs (variant removed from Nutrient)");
    println!("  ~ {CONFIG} (path removed)");
    // Anything that referenced the variant now dangles: remove tapped arms AND
    // release calls for it across every branch + the pulse, each as a balanced
    // span (multi-line bodies come out whole). A hand-edited body is dropped with
    // its arm — if it held real logic, it's gone (re-`feed` to undo).
    for path in nutrient_arm_files() {
        let Ok(mut src) = std::fs::read_to_string(&path) else {
            continue;
        };
        let mut n = 0;
        while let Some(new) =
            remove_balanced_span(&src, |l| is_arm_for(l, name) || is_release_of(l, name))
        {
            src = new;
            n += 1;
        }
        if n > 0 {
            std::fs::write(&path, src)?;
            println!(
                "  ~ {} ({n} connection{} removed)",
                path.display(),
                if n == 1 { "" } else { "s" }
            );
        }
    }
    println!(
        "note: a `///` doc line above the variant, if any, is left behind — delete it if stale."
    );
    sync_sap()
}

// ---------------------------------------------------------------------------
// Connections: wire a branch to a nutrient (per-branch, explicit).
//   tap/untap <branch> <Nutrient>       — consume (a `match` arm)
//   release/unrelease <branch> <Nutrient> — produce (a `sap.release` call)
// ---------------------------------------------------------------------------

/// Whether `name` carries fields — a struct (`Foo { .. }`) or tuple (`Foo(..)`)
/// variant. Read from the parsed `Nutrient` enum only, so a struct literal
/// elsewhere in trunk.rs that shares the name can't false-positive.
fn nutrient_has_payload(trunk: &str, name: &str) -> bool {
    flow::parse_variants(trunk)
        .iter()
        .any(|v| v.name == name && !v.fields.is_empty())
}

/// The match-arm pattern for `name`: `Nutrient::Foo { .. }` if it carries fields,
/// else `Nutrient::Foo`.
fn arm_lhs_for(trunk: &str, name: &str) -> String {
    if nutrient_has_payload(trunk, name) {
        format!("Nutrient::{name} {{ .. }}")
    } else {
        format!("Nutrient::{name}")
    }
}

/// Whether a line releases `Nutrient::<name>` — name + delimiter, so a sibling
/// isn't matched. Accepts `sap.release(…)` (what `release` writes),
/// and `sap.try_release(…)` (its never-waiting form).
fn is_release_of(line: &str, name: &str) -> bool {
    ["sap.release(", "sap.try_release("]
        .iter()
        .any(|call| release_call_names(line, call, name))
}

/// Whether a line releases `Nutrient::<name>` anywhere in it — `let _ =
/// sap.try_release(…)`, `if let Err(n) = sap.try_release(…)`, a release inside a
/// match arm. Looser than `is_release_of` (which only matches a line that *is*
/// the call, so removal never takes out surrounding code); used to read the
/// wiring graph, never to edit.
fn mentions_release_of(line: &str, name: &str) -> bool {
    ["sap.release(", "sap.try_release("].iter().any(|call| {
        line.match_indices(call)
            .any(|(i, _)| release_call_names(&line[i..], call, name))
    })
}

/// `mentions_release_of`, limited to the awaited `sap.release(…)` form.
fn mentions_awaited_release_of(line: &str, name: &str) -> bool {
    line.match_indices("sap.release(")
        .any(|(i, _)| release_call_names(&line[i..], "sap.release(", name))
}

fn release_call_names(line: &str, call: &str, name: &str) -> bool {
    line.trim_start()
        .strip_prefix(call)
        .and_then(|r| r.strip_prefix("Nutrient::"))
        .and_then(|r| r.strip_prefix(name))
        .is_some_and(|rest| matches!(rest.chars().next(), Some(' ' | ';' | '{' | '(' | ')')))
}

/// Shared guard: validate names, the tree, that the nutrient exists, and the
/// branch file — returning the branch path and the trunk source.
fn connect_guard(branch: &str, nutrient: &str) -> io::Result<(PathBuf, String)> {
    if !is_ident(branch) {
        eprintln!("branch name must be a snake_case identifier, got {branch:?}");
        std::process::exit(2);
    }
    if !is_variant(nutrient) {
        eprintln!("nutrient name must be an UpperCamelCase identifier, got {nutrient:?}");
        std::process::exit(2);
    }
    require_managed("tap/release");
    let trunk = std::fs::read_to_string("src/trunk.rs")?;
    if !parse_nutrients(&trunk).iter().any(|n| n == nutrient) {
        eprintln!("no `{nutrient}` nutrient — add it first with `bonsai feed {nutrient}`.");
        std::process::exit(1);
    }
    let branch_rs = Path::new("src/branches").join(format!("{branch}.rs"));
    if !branch_rs.is_file() {
        eprintln!("no such branch: {}", branch_rs.display());
        std::process::exit(1);
    }
    Ok((branch_rs, trunk))
}

/// `tap <branch> <Nutrient>`: the branch consumes the nutrient (adds a match arm).
fn tap(branch: &str, nutrient: &str) -> io::Result<()> {
    let (branch_rs, trunk) = connect_guard(branch, nutrient)?;
    let src = std::fs::read_to_string(&branch_rs)?;
    if !src.contains("// bonsai:nutrient-arm") {
        eprintln!("`{branch}` doesn't subscribe — tap needs a consumer or duplex branch.");
        std::process::exit(1);
    }
    if src.lines().any(|l| is_arm_for(l, nutrient)) {
        eprintln!("`{branch}` already taps `{nutrient}`.");
        std::process::exit(1);
    }
    let arm = format!("{} => {{ /* TODO */ }}", arm_lhs_for(&trunk, nutrient));
    let new =
        insert_indented_before(&src, "// bonsai:nutrient-arm", &arm).expect("marker checked above");
    std::fs::write(&branch_rs, new)?;
    println!("tapped `{branch}` → `{nutrient}`:");
    println!("  ~ {} (handler arm added)", branch_rs.display());
    sync_sap()
}

/// `untap <branch> <Nutrient>`: drop the branch's handler arm for the nutrient.
fn untap(branch: &str, nutrient: &str) -> io::Result<()> {
    let (branch_rs, _) = connect_guard(branch, nutrient)?;
    // Balanced-span removal: an arm whose body grew to multiple lines comes out
    // whole, not just its first line.
    let Some(new) = remove_balanced_span(&std::fs::read_to_string(&branch_rs)?, |l| {
        is_arm_for(l, nutrient)
    }) else {
        eprintln!("`{branch}` doesn't tap `{nutrient}`.");
        std::process::exit(1);
    };
    std::fs::write(&branch_rs, new)?;
    println!("untapped `{branch}` ↛ `{nutrient}`:");
    println!("  ~ {} (handler arm removed)", branch_rs.display());
    sync_sap()
}

/// `release <branch> <Nutrient>`: the branch produces the nutrient (publish call).
fn release(branch: &str, nutrient: &str) -> io::Result<()> {
    let (branch_rs, trunk) = connect_guard(branch, nutrient)?;
    let src = std::fs::read_to_string(&branch_rs)?;
    if !src.contains("// bonsai:emit") {
        eprintln!("`{branch}` doesn't publish — release needs a producer or duplex branch.");
        std::process::exit(1);
    }
    if src.lines().any(|l| is_release_of(l, nutrient)) {
        eprintln!("`{branch}` already releases `{nutrient}`.");
        std::process::exit(1);
    }
    let payload = nutrient_has_payload(&trunk, nutrient);
    let tuple = flow::parse_variants(&trunk)
        .iter()
        .any(|v| v.name == nutrient && v.names.is_none());
    let ctor = if payload && tuple {
        format!("Nutrient::{nutrient}(/* TODO: fields */)")
    } else if payload {
        format!("Nutrient::{nutrient} {{ /* TODO: fields */ }}")
    } else {
        format!("Nutrient::{nutrient}")
    };
    let call = format!("sap.release({ctor}).await;");
    let new = insert_indented_before(&src, "// bonsai:emit", &call).expect("marker checked above");
    std::fs::write(&branch_rs, new)?;
    println!("`{branch}` releases `{nutrient}`:");
    println!("  ~ {} (release call added)", branch_rs.display());
    if payload {
        println!("note: `{nutrient}` carries fields — fill them in (won't compile until you do).");
    }
    sync_sap()
}

/// `unrelease <branch> <Nutrient>`: drop the branch's publish call for the nutrient.
fn unrelease(branch: &str, nutrient: &str) -> io::Result<()> {
    let (branch_rs, _) = connect_guard(branch, nutrient)?;
    // Balanced-span removal: a publish whose payload ctor was filled in across
    // lines comes out whole.
    let Some(new) = remove_balanced_span(&std::fs::read_to_string(&branch_rs)?, |l| {
        is_release_of(l, nutrient)
    }) else {
        eprintln!("`{branch}` doesn't release `{nutrient}`.");
        std::process::exit(1);
    };
    std::fs::write(&branch_rs, new)?;
    println!("`{branch}` no longer releases `{nutrient}`:");
    println!("  ~ {} (release call removed)", branch_rs.display());
    sync_sap()
}

/// Net bracket depth of a line: `(`/`{` open, `)`/`}` close. Brackets inside
/// string literals aren't parsed — scaffolded code keeps them balanced (`"{}"`
/// nets 0), which is all the span remover below needs.
fn bracket_depth(line: &str) -> i32 {
    line.chars()
        .map(|c| match c {
            '(' | '{' => 1,
            ')' | '}' => -1,
            _ => 0,
        })
        .sum()
}

/// Remove a whole syntactic span: from the first line where `matches` is true,
/// through the line where every bracket that span opened has closed again. A
/// match arm, start call, or enum variant that a user expanded across lines
/// (rustfmt wrapping, a filled-in body) comes out whole instead of leaving
/// orphaned braces. A balanced single line removes exactly that line.
/// Returns None if no line matched.
fn remove_balanced_span(src: &str, matches: impl Fn(&str) -> bool) -> Option<String> {
    let mut offset = 0;
    let mut lines = src.split_inclusive('\n');
    for l in lines.by_ref() {
        let bare = l.strip_suffix('\n').unwrap_or(l);
        if matches(bare) {
            let mut end = offset + l.len();
            let mut depth = bracket_depth(bare);
            while depth > 0 {
                let Some(next) = lines.next() else { break };
                depth += bracket_depth(next.strip_suffix('\n').unwrap_or(next));
                end += next.len();
            }
            // rustfmt puts a chained call after a multi-line one on its own
            // line (`})` then `.await;`); take those continuation lines too.
            while let Some(next) = lines.next().filter(|n| n.trim_start().starts_with('.')) {
                end += next.len();
            }
            let mut out = String::with_capacity(src.len());
            out.push_str(&src[..offset]);
            out.push_str(&src[end..]);
            return Some(out);
        }
        offset += l.len();
    }
    None
}

/// Remove the first line for which `matches` is true, returning the new source.
/// Returns None if no line matched (the caller decides fatal vs. warn).
fn remove_line(src: String, matches: impl Fn(&str) -> bool) -> Option<String> {
    let mut offset = 0;
    for l in src.split_inclusive('\n') {
        if matches(l.strip_suffix('\n').unwrap_or(l)) {
            let mut out = String::with_capacity(src.len());
            out.push_str(&src[..offset]);
            out.push_str(&src[offset + l.len()..]);
            return Some(out);
        }
        offset += l.len();
    }
    None
}

/// Insert `line` immediately above the line containing `marker`. The caller
/// supplies `line` with its own indentation.
fn insert_before_marker(path: &Path, src: String, marker: &str, line: &str) -> String {
    // Match the marker as a whole line (trimmed), so the same text appearing
    // inside a doc comment (e.g. "keep the `// bonsai:mod` marker") isn't
    // mistaken for the marker itself.
    let mut offset = 0;
    for l in src.split_inclusive('\n') {
        if l.trim() == marker {
            let mut out = String::with_capacity(src.len() + line.len());
            out.push_str(&src[..offset]);
            out.push_str(line);
            out.push_str(&src[offset..]);
            return out;
        }
        offset += l.len();
    }
    eprintln!(
        "marker `{marker}` not found in {} — is this a bonsai tree?",
        path.display()
    );
    std::process::exit(1);
}

// ---------------------------------------------------------------------------
// `regrow`: wipe the tree in the cwd back to a fresh template for its device.
// ---------------------------------------------------------------------------

/// The board out of the `# Generated by bonsai for <board> (…` line every
/// trunk's Cargo.toml carries. That comment is the tree's only record of which
/// device it is.
fn parse_board(cargo_toml: &str) -> Option<String> {
    let marker = "Generated by bonsai for ";
    let line = cargo_toml.lines().find(|l| l.contains(marker))?;
    let after = &line[line.find(marker)? + marker.len()..];
    let board = after.split(" (").next()?.trim();
    (!board.is_empty()).then(|| board.to_string())
}

/// The crate name from `[package]`, so a regrown tree keeps its name. Ignores an
/// unrendered `{{project-name}}` (i.e. a raw template, not a real project).
/// The value of `key = "…"` inside `[section]`, ignoring an unrendered `{{…}}`
/// template value. Not a TOML parser — just enough for files bonsai generates.
/// Any table header ends the section scope.
fn parse_scoped_key(src: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for l in src.lines() {
        let t = l.trim();
        if t.starts_with('[') {
            in_section = t == section;
            continue;
        }
        if in_section && let Some(rest) = t.strip_prefix(key) {
            let Some(after) = rest.trim_start().strip_prefix('=') else {
                continue;
            };
            let v = after.trim().trim_matches('"');
            if !v.is_empty() && !v.contains("{{") {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn parse_package_name(cargo_toml: &str) -> Option<String> {
    parse_scoped_key(cargo_toml, "[package]", "name")
}

/// The build target triple from a tree's `.cargo/config.toml` (the `[build]`
/// `target = "..."`). The `[target.<triple>]` section header starts with `[`, so
/// it's skipped; an unrendered `{{target}}` (raw template) is ignored too.
fn parse_target(config: &str) -> Option<String> {
    parse_scoped_key(config, "[build]", "target")
}

/// Refresh dependencies owned by the board template, keeping branch libraries and
/// other user-added manifest entries intact. Cargo then resolves a fresh lockfile.
fn updated_manifest(current: &str, template: &str) -> io::Result<String> {
    use std::str::FromStr;
    use toml_edit::DocumentMut;

    let mut current = DocumentMut::from_str(current)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let template = DocumentMut::from_str(template)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let dependencies = template["dependencies"].as_table().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "template has no dependencies")
    })?;
    let existing = current["dependencies"]
        .as_table_mut()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "project has no dependencies"))?;
    for (name, value) in dependencies.iter() {
        existing[name] = value.clone();
    }
    Ok(current.to_string())
}

#[cfg(test)]
mod retarget_tests {
    use super::*;

    fn rendered(file: &str) -> String {
        file.replace("{{project-name}}", "orb")
    }

    const ZERO_W_TOML: &str = include_str!("../templates/linux/zero-w/Cargo.toml");
    const ZERO_2W_TOML: &str = include_str!("../templates/linux/zero-2w/Cargo.toml");
    const PI5_TOML: &str = include_str!("../templates/linux/pi5/Cargo.toml");
    const ZERO_W_CONFIG: &str = include_str!("../templates/linux/zero-w/.cargo/config.toml");
    const PI5_CONFIG: &str = include_str!("../templates/linux/pi5/.cargo/config.toml");

    #[test]
    fn manifest_takes_the_new_boards_stamp_and_profile() {
        let tree = with_dependency(&rendered(ZERO_2W_TOML), ("mavlink", "0.16"))
            .unwrap()
            .unwrap();
        let moved = retargeted_manifest(&tree, &rendered(PI5_TOML)).unwrap();
        let expected = with_dependency(&rendered(PI5_TOML), ("mavlink", "0.16"))
            .unwrap()
            .unwrap();
        assert_eq!(moved, expected);
        assert_eq!(parse_board(&moved).as_deref(), Some("pi5"));
        // And back again: nothing of the Pi 5 is left.
        let back = retargeted_manifest(&moved, &rendered(ZERO_2W_TOML)).unwrap();
        assert_eq!(back, tree);
    }

    #[test]
    fn manifest_round_trips_across_every_board() {
        const HOST_TOML: &str = include_str!("../templates/linux/host/Cargo.toml");
        let all = [ZERO_W_TOML, ZERO_2W_TOML, PI5_TOML, HOST_TOML];
        for from in all {
            for to in all {
                let moved = retargeted_manifest(&rendered(from), &rendered(to)).unwrap();
                assert_eq!(moved, rendered(to));
            }
        }
    }

    #[test]
    fn config_takes_the_new_target_and_keeps_env() {
        let tree = ZERO_W_CONFIG.replace(
            "BONSAI_PI = \"pi@raspberrypi.local\"",
            "BONSAI_PI = \"virtual-zero-w\"\nMY_SETTING = \"1\"",
        );
        let moved = retargeted_config(&tree, PI5_CONFIG).unwrap();
        assert_eq!(
            parse_target(&moved).as_deref(),
            Some("aarch64-unknown-linux-gnu")
        );
        assert!(moved.contains("BONSAI_PI = \"virtual-zero-w\""));
        assert!(moved.contains("MY_SETTING = \"1\""));
        assert!(!moved.contains("armv6l"));
        // An untouched tree lands exactly on the template.
        assert_eq!(
            retargeted_config(ZERO_W_CONFIG, PI5_CONFIG).unwrap(),
            PI5_CONFIG
        );
    }

    #[test]
    fn config_moves_between_a_pi_and_this_computer() {
        const HOST_CONFIG: &str = include_str!("../templates/linux/host/.cargo/config.toml");
        // Pi → host: the build target and runner go, the Pi's address stays
        // (under the host template's comment) for moving back.
        let tree = PI5_CONFIG.replace("pi@raspberrypi.local", "orb@orb.local");
        let host = retargeted_config(&tree, HOST_CONFIG).unwrap();
        assert!(parse_target(&host).is_none(), "{host}");
        assert!(
            host.starts_with("# This tree builds for the computer it's on"),
            "{host}"
        );
        assert!(host.contains("BONSAI_PI = \"orb@orb.local\""), "{host}");
        // … and back to a Pi keeps it.
        let back = retargeted_config(&host, PI5_CONFIG).unwrap();
        assert_eq!(back, tree);
        // An untouched host tree lands exactly on each template, both ways.
        assert_eq!(
            retargeted_config(HOST_CONFIG, PI5_CONFIG).unwrap(),
            PI5_CONFIG
        );
        assert_eq!(
            retargeted_config(HOST_CONFIG, HOST_CONFIG).unwrap(),
            HOST_CONFIG
        );
    }

    #[test]
    fn main_header_changes_only_while_generated() {
        let pi5 = rendered(include_str!("../templates/linux/pi5/src/main.rs"));
        let zero = rendered(include_str!("../templates/linux/zero-2w/src/main.rs"));
        let moved = retargeted_main(&zero, &pi5).unwrap();
        assert!(moved.starts_with("//! orb — Raspberry Pi 5 Linux application."));
        assert_eq!(moved.lines().count(), zero.lines().count());
        assert_eq!(retargeted_main(&pi5, &pi5), None);
        let edited = zero.replacen("//! orb —", "//! my drone —", 1);
        assert_eq!(retargeted_main(&edited, &pi5), None);
    }
}

#[cfg(test)]
mod plant_here_tests {
    use super::*;

    /// A fresh scratch dir under the system temp dir.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bonsai-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn folder_names_become_crate_names() {
        assert_eq!(crate_name("greenhouse"), "greenhouse");
        assert_eq!(crate_name("My Drone!"), "my-drone");
        assert_eq!(crate_name("gcs_link-2"), "gcs_link-2");
    }

    #[test]
    fn conflicts_list_only_template_files_that_exist() {
        let dir = scratch("here");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        let empty = scratch("empty");
        assert!(only_dotfiles(&empty));
        let _ = std::fs::remove_dir_all(&empty);
        let template = Path::new("templates/linux/zero-2w");
        assert!(plant_here_conflicts(&dir, template).is_empty());
        assert!(!only_dotfiles(&dir));

        std::fs::write(dir.join("Cargo.toml"), "").unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "").unwrap();
        assert_eq!(
            plant_here_conflicts(&dir, template),
            [PathBuf::from("Cargo.toml"), PathBuf::from("src/main.rs")]
        );
        // cargo-generate's own files are never output, so never a conflict.
        std::fs::write(dir.join("cargo-generate.toml"), "").unwrap();
        assert_eq!(plant_here_conflicts(&dir, template).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn pick_board(wiz: &mut Wizard, board: &str) {
        wiz.cursor[BOARD_STEP] = BOARDS.iter().position(|b| b.0 == board).unwrap();
        wiz.on_key(KeyCode::Enter);
        assert_eq!(wiz.board, board);
        assert_eq!(wiz.step, TOOLS_STEP);
    }

    fn pick_rpi(wiz: &mut Wizard) {
        pick_board(wiz, "zero-2w");
        wiz.on_key(KeyCode::Enter); // keep the tools as offered
    }

    #[test]
    fn where_step_defaults_to_here_in_an_empty_folder() {
        let dir = scratch("drone");
        let mut wiz = Wizard::new(false, &dir);
        pick_rpi(&mut wiz);
        assert_eq!(wiz.step, WHERE_STEP);
        assert_eq!(wiz.cursor[WHERE_STEP], 0);
        wiz.on_key(KeyCode::Enter);
        assert!(wiz.here);
        assert_eq!(wiz.step, NAME_STEP);
        assert_eq!(wiz.name, crate_name(&wiz.folder));

        std::fs::write(dir.join("notes.txt"), "").unwrap();
        let mut wiz = Wizard::new(false, &dir);
        pick_rpi(&mut wiz);
        assert_eq!(wiz.cursor[WHERE_STEP], 1);
        wiz.on_key(KeyCode::Enter);
        assert!(!wiz.here && wiz.name.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_skips_the_where_step() {
        let dir = scratch("init");
        let mut wiz = Wizard::new(true, &dir);
        pick_rpi(&mut wiz);
        assert_eq!(wiz.step, NAME_STEP);
        assert!(wiz.here);
        wiz.on_key(KeyCode::Esc);
        assert_eq!(wiz.step, TOOLS_STEP);
        wiz.on_key(KeyCode::Esc);
        assert_eq!(wiz.step, BOARD_STEP);
        wiz.on_key(KeyCode::Esc);
        assert!(wiz.aborted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn back_from_where_lands_on_the_tools() {
        let dir = scratch("back");
        let mut wiz = Wizard::new(false, &dir);
        pick_rpi(&mut wiz);
        assert_eq!(wiz.step, WHERE_STEP);
        wiz.on_key(KeyCode::Esc);
        assert_eq!(wiz.step, TOOLS_STEP);
        wiz.on_key(KeyCode::Esc);
        assert_eq!(wiz.step, BOARD_STEP);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tools_step_picks_with_space() {
        let dir = scratch("tools");
        let mut wiz = Wizard::new(false, &dir);
        pick_board(&mut wiz, "pi5");
        let offered: Vec<&str> = wiz.tools.rows.iter().map(|r| r.0.name()).collect();
        assert_eq!(offered, ["sccache", "mold", "zigbuild", "bacon"]);
        for row in &mut wiz.tools.rows {
            row.1 = false; // start from none, whatever this machine has installed
        }
        wiz.on_key(KeyCode::Down);
        wiz.on_key(KeyCode::Down);
        wiz.on_key(KeyCode::Char(' '));
        assert_eq!(wiz.tools.picked(), [tools::Tool::Zigbuild]);
        assert!(wiz.options()[2].starts_with("[x] zigbuild"));
        wiz.on_key(KeyCode::Char(' '));
        assert!(wiz.tools.picked().is_empty());
        wiz.on_key(KeyCode::Char(' '));
        wiz.on_key(KeyCode::Enter);
        assert_eq!(wiz.step, WHERE_STEP);
        // Going back and forth keeps the picks.
        wiz.on_key(KeyCode::Esc);
        wiz.on_key(KeyCode::Enter);
        assert_eq!(wiz.tools.picked(), [tools::Tool::Zigbuild]);

        // A tree for this computer isn't offered zigbuild: nothing to cross-link.
        let mut wiz = Wizard::new(false, &dir);
        pick_board(&mut wiz, "host");
        let offered: Vec<&str> = wiz.tools.rows.iter().map(|r| r.0.name()).collect();
        assert_eq!(offered, ["sccache", "mold", "bacon"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn board_menu_lists_every_board_with_what_it_is() {
        let wiz = Wizard::new(false, Path::new("/tmp/greenhouse"));
        let options = wiz.options();
        assert_eq!(options.len(), BOARDS.len());
        assert!(options[0].starts_with("pi5") && options[0].contains("Raspberry Pi 5"));
        assert!(options.iter().any(|o| o.starts_with("host")));
    }

    #[test]
    fn defines_name_the_board_and_its_chip() {
        assert_eq!(
            template_defines("pi5"),
            ["-d", "chip=bcm2712", "-d", "board=pi5"]
        );
        assert_eq!(template_defines("host")[1], "chip=native");
    }
}

#[cfg(test)]
mod roots_tests {
    use super::*;

    #[test]
    fn roots_mod_goes_after_pulse() {
        let main = "mod branches;\nmod pulse;\nmod trunk;\n";
        assert_eq!(
            with_roots_mod(main).unwrap(),
            "mod branches;\nmod pulse;\nmod roots;\nmod trunk;\n"
        );
        assert!(with_roots_mod("mod trunk;\n").is_none());
    }

    #[test]
    fn dependency_added_once() {
        let manifest = "[package]\nname = \"x\"\n\n[dependencies]\nembassy-sync = \"0.8.0\"\n";
        let added = with_dependency(manifest, EMBASSY_FUTURES).unwrap().unwrap();
        assert!(added.contains("embassy-futures = \"0.1.2\""));
        assert!(with_dependency(&added, EMBASSY_FUTURES).unwrap().is_none());
    }
}

#[cfg(test)]
mod update_tests {
    use super::updated_manifest;

    #[test]
    fn update_replaces_managed_crates_and_keeps_custom_dependencies() {
        let project = "[package]\nname = \"example\"\nversion = \"0.1.0\"\n\n[dependencies]\nembassy-time = { git = \"https://old.example/embassy\" }\nheapless = \"0.8\"\n";
        let template = "[package]\nname = \"template\"\n\n[dependencies]\nembassy-time = \"0.5.1\"\nembassy-sync = \"0.8.0\"\n";
        let updated = updated_manifest(project, template).unwrap();
        assert!(updated.contains("embassy-time = \"0.5.1\""));
        assert!(updated.contains("embassy-sync = \"0.8.0\""));
        assert!(updated.contains("heapless = \"0.8\""));
        assert!(updated.contains("name = \"example\""));
    }
}

fn update() -> io::Result<()> {
    let root = std::env::current_dir()?;
    let manifest_path = root.join("Cargo.toml");
    let old_manifest = std::fs::read_to_string(&manifest_path)?;
    if !old_manifest.contains("Generated by bonsai for") || !root.join("src/trunk.rs").is_file() {
        eprintln!("not a bonsai tree — run `bonsai update` inside a generated project.");
        std::process::exit(1);
    }
    let board = parse_board(&old_manifest)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing board stamp"))?;
    if chip_of(&board).is_none() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown board"));
    }
    let name = parse_package_name(&old_manifest)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing package name"))?;
    let (template_path, extracted) = template_dir(&board)?;
    let staging = std::env::temp_dir().join(format!("bonsai-update-{}", std::process::id()));
    std::fs::create_dir_all(&staging)?;
    let result = (|| -> io::Result<()> {
        let status = Command::new("cargo")
            .args(["generate", "--path"])
            .arg(&template_path)
            .args(["--name", &name, "--destination"])
            .arg(&staging)
            .args(["--vcs", "none"])
            .args(template_defines(&board))
            .status()?;
        if !status.success() {
            return Err(io::Error::other("cargo generate failed"));
        }
        let rendered = std::fs::read_to_string(staging.join(&name).join("Cargo.toml"))?;
        let new_manifest = updated_manifest(&old_manifest, &rendered)?;
        if new_manifest == old_manifest {
            println!("template dependencies are current; refreshing Cargo.lock");
        } else {
            std::fs::write(&manifest_path, &new_manifest)?;
        }
        let lock_path = root.join("Cargo.lock");
        let old_lock = std::fs::read(&lock_path).ok();
        let status = Command::new("cargo")
            .arg("update")
            .current_dir(&root)
            .status();
        if !status.as_ref().is_ok_and(|s| s.success()) {
            std::fs::write(&manifest_path, old_manifest)?;
            match old_lock {
                Some(bytes) => std::fs::write(&lock_path, bytes)?,
                None if lock_path.exists() => std::fs::remove_file(&lock_path)?,
                None => {}
            }
            return Err(io::Error::other(
                "cargo update failed; manifest and lockfile restored",
            ));
        }
        println!("updated {board} template dependencies and Cargo.lock");
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(staging);
    if extracted {
        let _ = std::fs::remove_dir_all(template_path);
    }
    result
}

/// `current` Cargo.toml moved to the board `template` was rendered for: its
/// board stamp, template dependencies and `[profile]` come from the template;
/// branch dependencies and every other entry stay.
fn retargeted_manifest(current: &str, template: &str) -> io::Result<String> {
    use std::str::FromStr;
    use toml_edit::DocumentMut;

    let mut doc = DocumentMut::from_str(&updated_manifest(current, template)?)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let template_doc = DocumentMut::from_str(template)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    match template_doc.get("profile") {
        Some(profile) => doc["profile"] = profile.clone(),
        None => {
            doc.remove("profile");
        }
    }
    let stamp = |s: &str| {
        s.lines()
            .find(|l| l.contains("Generated by bonsai for"))
            .map(str::to_string)
    };
    let mut out = doc.to_string();
    if let (Some(old), Some(new)) = (stamp(&out), stamp(template)) {
        out = out.replacen(&old, &new, 1);
    }
    Ok(out)
}

/// The template's `.cargo/config.toml`, keeping the `[env]` values the tree set
/// (the Pi's ssh host). Build tools are reapplied afterwards.
fn retargeted_config(current: &str, template: &str) -> io::Result<String> {
    use std::str::FromStr;
    use toml_edit::DocumentMut;

    let current = DocumentMut::from_str(current)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let mut doc = DocumentMut::from_str(template)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if let Some(env) = current.get("env").and_then(|e| e.as_table()) {
        if !doc.contains_table("env") {
            // A template with no tables (host) is only a comment, which
            // toml_edit keeps as trailing text: keep it above the new table.
            let mut table = toml_edit::Table::new();
            if doc.as_table().is_empty() {
                let comment = doc.trailing().as_str().unwrap_or_default().to_string();
                table.decor_mut().set_prefix(comment);
                doc.set_trailing("");
            }
            doc["env"] = toml_edit::Item::Table(table);
        }
        let new_env = doc["env"]
            .as_table_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "[env] isn't a table"))?;
        for (key, item) in env.iter() {
            match (new_env.get_mut(key), item.as_value()) {
                // Keep the template's comment, take the tree's value.
                (Some(slot), Some(value)) => {
                    if let Some(slot) = slot.as_value_mut() {
                        let decor = slot.decor().clone();
                        *slot = value.clone();
                        *slot.decor_mut() = decor;
                    }
                }
                _ => {
                    new_env.insert(key, item.clone());
                }
            }
        }
    }
    Ok(doc.to_string())
}

/// `src/main.rs` with its first doc line (which names the board) from the new
/// render, when it's still the generated one. None when there's nothing to change.
fn retargeted_main(current: &str, template: &str) -> Option<String> {
    let old = current.lines().next()?;
    let new = template.lines().next()?;
    let project = |l: &str| {
        l.strip_prefix("//! ")?
            .split(" — ")
            .next()
            .map(str::to_string)
    };
    if old == new || project(old).is_none() || project(old) != project(new) {
        return None;
    }
    Some(current.replacen(old, new, 1))
}

/// `bonsai retarget <board>`: move the tree in the cwd to another board. Its
/// code stays; the build settings (target, linker, runner,
/// release profile, board crates) become the new board's.
fn retarget(new_board: &str) -> io::Result<()> {
    require_managed("retarget");
    let root = std::env::current_dir()?;
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    let Some(board) = parse_board(&manifest).filter(|b| chip_of(b).is_some()) else {
        eprintln!(
            "couldn't read this tree's board from Cargo.toml's `Generated by bonsai for` line"
        );
        std::process::exit(1);
    };
    if chip_of(new_board).is_none() {
        eprintln!(
            "no board `{new_board}`; one of: {}",
            board_names().join(", ")
        );
        std::process::exit(2);
    }
    if new_board == board {
        println!("already a {board} tree");
        return Ok(());
    }
    let Some(name) = parse_package_name(&manifest) else {
        eprintln!("couldn't read the crate name from [package] in Cargo.toml");
        std::process::exit(1);
    };
    let config_path = root.join(".cargo/config.toml");
    let config = std::fs::read_to_string(&config_path).unwrap_or_default();
    let kept_tools = tools::configured(&config);

    let (template_path, extracted) = template_dir(new_board)?;
    let staging = std::env::temp_dir().join(format!("bonsai-retarget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?;
    let rendered = (|| -> io::Result<Vec<(PathBuf, String)>> {
        let status = Command::new("cargo")
            .args(["generate", "--path"])
            .arg(&template_path)
            .args(["--name", &name, "--destination"])
            .arg(&staging)
            .args(["--vcs", "none"])
            .args(template_defines(new_board))
            .status()?;
        if !status.success() {
            return Err(io::Error::other("cargo generate failed"));
        }
        let fresh = staging.join(&name);
        let read = |rel: &str| std::fs::read_to_string(fresh.join(rel));
        let mut edits = vec![
            (
                PathBuf::from("Cargo.toml"),
                retargeted_manifest(&manifest, &read("Cargo.toml")?)?,
            ),
            (
                PathBuf::from(".cargo/config.toml"),
                retargeted_config(&config, &read(".cargo/config.toml")?)?,
            ),
        ];
        let main = std::fs::read_to_string(root.join("src/main.rs")).unwrap_or_default();
        if let Some(main) = retargeted_main(&main, &read("src/main.rs")?) {
            edits.push((PathBuf::from("src/main.rs"), main));
        }
        Ok(edits)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    if extracted {
        let _ = std::fs::remove_dir_all(&template_path);
    }
    let edits: Vec<_> = rendered?
        .into_iter()
        .filter(|(path, content)| {
            std::fs::read_to_string(root.join(path)).ok().as_deref() != Some(content.as_str())
        })
        .collect();

    println!("retarget `{name}` from {board} to {new_board}. Your code stays; these change:");
    for (path, _) in &edits {
        println!("  ~ {}", path.display());
    }
    println!("  (.cargo/config.toml keeps its [env] values and build tools; other hand edits");
    println!("   there, and in the files above, are replaced by the {new_board} template's)");
    if !confirm("continue?")? {
        println!("cancelled");
        return Ok(());
    }
    for (path, content) in &edits {
        std::fs::write(root.join(path), content)?;
    }
    if !kept_tools.is_empty() {
        apply_tools(&root, &kept_tools)?;
    }
    println!("`{name}` now builds for {new_board}. Pins and devices (serial ports, GPIO) may");
    println!("differ between boards: check the ones your branches open.");
    Ok(())
}

fn file_contains(path: &Path, needle: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.contains(needle))
        .unwrap_or(false)
}

/// Ask a yes/no question on the terminal. Defaults to no on anything but an
/// explicit yes — the safe default for a destructive action.
fn confirm(prompt: &str) -> io::Result<bool> {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes" | "YES"))
}

fn regrow() -> io::Result<()> {
    let root = std::env::current_dir()?;

    // Backstop against catastrophe. The bonsai-tree signature below would reject
    // these too, but refuse the filesystem root and the home dir explicitly.
    if root.parent().is_none() {
        eprintln!("refusing to regrow the filesystem root");
        std::process::exit(1);
    }
    if std::env::var_os("HOME").is_some_and(|h| root == Path::new(&h)) {
        eprintln!("refusing to regrow your home directory");
        std::process::exit(1);
    }

    // Require an unambiguous bonsai tree before wiping anything: the Cargo.toml
    // stamp, both trunk marker files, and the sap/pulse. A stray directory can't
    // match all of these.
    let cargo_toml = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    let is_tree = cargo_toml.contains("Generated by bonsai for")
        && file_contains(&root.join("src/main.rs"), "// bonsai:start")
        && file_contains(&root.join("src/branches/mod.rs"), "// bonsai:mod")
        && root.join("src/trunk.rs").is_file()
        && root.join("src/pulse.rs").is_file();
    if !is_tree {
        eprintln!(
            "not a bonsai tree: {} — refusing to wipe.\nrun regrow from inside a project bonsai grew.",
            root.display()
        );
        std::process::exit(1);
    }

    // Recover the device + name from the tree itself.
    let Some(board) = parse_board(&cargo_toml) else {
        eprintln!("couldn't find the `Generated by bonsai for <board>` line in Cargo.toml");
        std::process::exit(1);
    };
    let Some(chip) = chip_of(&board) else {
        eprintln!("unknown board `{board}` — was it removed from bonsai's hardware list?");
        std::process::exit(1);
    };
    let Some(name) = parse_package_name(&cargo_toml) else {
        eprintln!("couldn't read the crate name from [package] in Cargo.toml");
        std::process::exit(1);
    };

    let kept_tools = tools::configured(
        &std::fs::read_to_string(root.join(".cargo/config.toml")).unwrap_or_default(),
    );
    println!("regrow will wipe this tree back to a fresh template:");
    println!("  dir:    {}", root.display());
    println!("  board:  {board} ({chip})  (project `{name}`)");
    println!("  keeps .git/ — removes everything else (all branches and edits)");
    if !confirm("continue?")? {
        println!("cancelled");
        return Ok(());
    }

    // Render the fresh template into a staging dir *inside* the tree — same
    // filesystem, so the move afterwards is a rename — and only wipe once it
    // succeeds. A failed generate must leave the tree untouched.
    let (template_path, is_temp) = template_dir(&board)?;
    let staging = root.join(".bonsai-regrow");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging)?; // cargo-generate requires the destination to exist
    let mut args = vec![
        "generate".to_string(),
        "--path".to_string(),
        template_path.display().to_string(),
        "--name".to_string(),
        name.clone(),
        "--destination".to_string(),
        staging.display().to_string(),
        "--vcs".to_string(),
        "none".to_string(), // don't init a nested .git in the fresh tree
    ];
    args.extend(template_defines(&board));
    println!("cargo {}", args.join(" "));
    let status = Command::new("cargo").args(&args).status()?;
    if is_temp {
        let _ = std::fs::remove_dir_all(&template_path);
    }
    let fresh = staging.join(&name);
    if !status.success() || !fresh.is_dir() {
        eprintln!("cargo-generate failed — tree left untouched");
        let _ = std::fs::remove_dir_all(&staging);
        std::process::exit(1);
    }

    // Wipe the tree (keeping .git and the staging dir), then move the fresh
    // files up into place. A failure here (locked file, permissions) leaves the
    // tree half-wiped, but the fresh render is intact in the staging dir — tell
    // the user where it is instead of dying with a bare io error.
    let wipe_and_move = || -> io::Result<()> {
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            let entry_name = entry.file_name();
            if entry_name == ".git" || entry_name == ".bonsai-regrow" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        for entry in std::fs::read_dir(&fresh)? {
            let entry = entry?;
            std::fs::rename(entry.path(), root.join(entry.file_name()))?;
        }
        Ok(())
    };
    if let Err(e) = wipe_and_move() {
        eprintln!("regrow failed midway: {e}");
        eprintln!(
            "the fresh tree is intact at {} — move its contents up manually, then delete the staging dir.",
            fresh.display()
        );
        std::process::exit(1);
    }
    std::fs::remove_dir_all(&staging)?;

    println!("regrew `{name}` — a fresh {board} tree. Your branches are gone.");
    if !kept_tools.is_empty() {
        apply_tools(&root, &kept_tools)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `list`: a read-only summary of the tree in the cwd — device, branches, nutrients.
// ---------------------------------------------------------------------------

/// The variant names declared in the `Nutrient` enum of trunk.rs — unit,
/// struct or tuple, single- or multi-line (a wrapped variant's field lines are
/// never mistaken for variants).
fn parse_nutrients(trunk: &str) -> Vec<String> {
    flow::parse_variants(trunk)
        .into_iter()
        .map(|v| v.name)
        .collect()
}

fn list() -> io::Result<()> {
    let cargo_toml = std::fs::read_to_string("Cargo.toml").unwrap_or_default();
    if !cargo_toml.contains("Generated by bonsai for") {
        eprintln!("not a bonsai tree — run `bonsai list` from inside a project bonsai grew.");
        std::process::exit(1);
    }
    require_managed("list");

    let name = parse_package_name(&cargo_toml).unwrap_or_else(|| "?".to_string());
    let device = parse_board(&cargo_toml)
        .and_then(|board| chip_of(&board).map(|chip| format!("{board} ({chip})")))
        .unwrap_or_else(|| "unknown board".to_string());
    let target = parse_target(&std::fs::read_to_string(".cargo/config.toml").unwrap_or_default());

    let branches: Vec<String> = nutrient_arm_files()
        .iter()
        .filter(|p| p.starts_with("src/branches"))
        .map(|p| p.file_stem().unwrap().to_string_lossy().into_owned())
        .collect();

    let trunk = std::fs::read_to_string("src/trunk.rs").unwrap_or_default();
    let nutrients = parse_nutrients(&trunk);

    match target {
        Some(t) => println!("tree: {name}  ({device}, target {t})"),
        None => println!("tree: {name}  ({device})"),
    }

    // The flow graph: producer → path → consumers, with shape and capacity.
    let graph = tree_graph();
    match graph.layout {
        flow::Layout::Paths => println!("sap: paths — one channel per nutrient"),
        flow::Layout::Trunk => println!(
            "sap: trunk — one shared bus (cap {}) carries every nutrient",
            graph.trunk_cap()
        ),
    }
    for l in graph.flow_lines() {
        println!("  {l}");
    }
    if let Some(bytes) = graph.sap_bytes(target_ptr_width()) {
        println!(
            "  ≈ {bytes} B of queued payload across {} slots",
            total_slots(&graph)
        );
    }
    if branches.is_empty() {
        println!("branches: none");
    } else {
        println!("branches:");
        for b in &branches {
            let src = std::fs::read_to_string(format!("src/branches/{b}.rs")).unwrap_or_default();
            let taps = branch_connections(&src, &nutrients, /* release */ false);
            let releases = branch_connections(&src, &nutrients, /* release */ true);
            let mut edges = Vec::new();
            if !taps.is_empty() {
                edges.push(format!("taps {}", taps.join(", ")));
            }
            if !releases.is_empty() {
                edges.push(format!("releases {}", releases.join(", ")));
            }
            let wiring = if edges.is_empty() {
                "—".to_string()
            } else {
                edges.join(" · ")
            };
            println!("  {b}: {wiring}");
        }
    }
    for w in graph.warnings() {
        print_warning(&w);
    }
    Ok(())
}

/// The nutrients a branch taps (match arms) or releases (publish calls), by
/// scanning its source for each known nutrient. Order follows `nutrients`.
fn branch_connections(src: &str, nutrients: &[String], release: bool) -> Vec<String> {
    nutrients
        .iter()
        .filter(|n| {
            src.lines().any(|l| {
                if release {
                    mentions_release_of(l, n)
                } else {
                    is_arm_for(l, n)
                }
            })
        })
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// The generated sap: `src/sap.rs`, rebuilt from the wiring by every wiring
// command (and `sync`); per-nutrient path config lives in `bonsai.toml`.
// ---------------------------------------------------------------------------

/// The tree's path config: shape + capacity per nutrient, and the layout.
const CONFIG: &str = "bonsai.toml";
const SAP_RS: &str = "src/sap.rs";

/// Whether the tree in the cwd has a generated sap. Trees grown before
/// per-nutrient paths (a single `PubSubChannel` in trunk.rs) don't, and every
/// command refuses them via `require_managed`.
fn sap_managed() -> bool {
    Path::new(SAP_RS).is_file()
}

/// The parsed `bonsai.toml` (defaults if it's missing); exit(1) if malformed.
fn load_config() -> flow::Config {
    let Ok(src) = std::fs::read_to_string(CONFIG) else {
        return flow::Config::default();
    };
    flow::Config::parse(&src).unwrap_or_else(|e| {
        eprintln!("{CONFIG}: {e}");
        std::process::exit(1);
    })
}

/// Apply `edit` to bonsai.toml's source (empty if missing), returning the new
/// text without writing it — callers write once every edit has succeeded.
fn edit_config(edit: impl Fn(&str) -> Result<String, String>) -> io::Result<String> {
    let src = std::fs::read_to_string(CONFIG).unwrap_or_default();
    edit(&src).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{CONFIG}: {e}")))
}

/// The tree's wiring graph: every nutrient's path, and each node (the pulse +
/// every branch) with what it taps and releases.
fn tree_graph() -> flow::Graph {
    let trunk = std::fs::read_to_string("src/trunk.rs").unwrap_or_default();
    let variants = flow::parse_variants(&trunk);
    let nutrients: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
    let nodes = nutrient_arm_files()
        .into_iter()
        .filter_map(|p| {
            let src = std::fs::read_to_string(&p).ok()?;
            let name = p.file_stem()?.to_string_lossy().into_owned();
            Some(flow::Node::scan(&name, &src, &nutrients))
        })
        .collect();
    flow::Graph::build(&variants, &load_config(), nodes)
}

/// Regenerate `src/sap.rs` from the wiring, and print any hazards the wiring
/// now has.
fn sync_sap() -> io::Result<()> {
    migrate_sap_module()?;
    let graph = tree_graph();
    let new = flow::render(&graph);
    if std::fs::read_to_string(SAP_RS).ok().as_deref() != Some(new.as_str()) {
        std::fs::write(SAP_RS, new)?;
        println!("  ~ {SAP_RS} (paths regenerated)");
    }
    for w in graph.warnings() {
        print_warning(&w);
    }
    Ok(())
}

/// The sap's module declaration, as the templates place it inside trunk.rs.
const SAP_MOD_IN_TRUNK: &str =
    "// The generated sap (src/sap.rs) is a child of the trunk, so the payload types
// you use in `Nutrient` resolve there exactly as they do here.
#[rustfmt::skip] // generated by bonsai — `bonsai sync` rewrites it
#[path = \"sap.rs\"]
pub mod sap;

pub use sap::Sap;
";

/// Trees from the first per-nutrient-path release declare `mod sap;` in
/// main.rs, beside the trunk. The sap now lives *under* the trunk, so payload
/// types imported in trunk.rs resolve in the generated slot structs. Move the
/// declaration (two bonsai-owned lines in each file); a no-op once moved, and a
/// note — not an error — if the files were hand-edited out of shape.
fn migrate_sap_module() -> io::Result<()> {
    let (main_rs, trunk_rs) = (Path::new("src/main.rs"), Path::new("src/trunk.rs"));
    let main = std::fs::read_to_string(main_rs)?;
    let trunk = std::fs::read_to_string(trunk_rs)?;
    if !main.lines().any(|l| l.trim() == "mod sap;") || trunk.contains("pub mod sap;") {
        return Ok(());
    }
    let reexport = "pub use crate::sap::Sap;\n";
    let has_mod_trunk = main.lines().any(|l| l.trim() == "mod trunk;");
    if !trunk.contains(reexport) || !has_mod_trunk {
        println!(
            "note: src/sap.rs should now be declared inside src/trunk.rs — move `mod sap;` out of\n\
             \x20     main.rs (see \"Migrating an older tree\" in bonsai's README)."
        );
        return Ok(());
    }
    // main.rs: drop `mod sap;` (and the rustfmt::skip line bonsai put above it),
    // and reach the sap through the trunk instead.
    let mut out = String::new();
    let mut lines = main.split_inclusive('\n').peekable();
    while let Some(l) = lines.next() {
        let next_is_mod = lines.peek().is_some_and(|n| n.trim() == "mod sap;");
        if (l.trim_start().starts_with("#[rustfmt::skip]") && next_is_mod) || l.trim() == "mod sap;"
        {
            continue;
        }
        out.push_str(l);
        if l.trim() == "mod trunk;" && !main.contains("use trunk::sap;") {
            // Placed after the module list; `use` order is rustfmt's business.
            out.push_str(
                "\nuse trunk::sap; // the generated sap lives under the trunk (src/sap.rs)\n",
            );
        }
    }
    std::fs::write(trunk_rs, trunk.replacen(reexport, SAP_MOD_IN_TRUNK, 1))?;
    std::fs::write(main_rs, out)?;
    println!("  ~ src/main.rs, src/trunk.rs (the sap moved under the trunk)");
    Ok(())
}

/// Print a wiring warning word-wrapped to the terminal-friendly width, with a
/// hanging indent under the `warning:` label.
fn print_warning(w: &str) {
    const WIDTH: usize = 88;
    let mut line = String::from("warning:");
    for word in w.split_whitespace() {
        if line.len() + 1 + word.len() > WIDTH {
            println!("{line}");
            line = String::from("        ");
        }
        line.push(' ');
        line.push_str(word);
    }
    println!("{line}");
}

/// Exit unless the cwd is a tree with a generated sap.
fn require_managed(cmd: &str) {
    if !Path::new("src/trunk.rs").is_file() {
        eprintln!("run this inside a bonsai tree (no src/trunk.rs here)");
        std::process::exit(1);
    }
    if !sap_managed() {
        eprintln!(
            "`bonsai {cmd}`: this tree predates per-nutrient paths (it has no src/sap.rs), which\n\
             this bonsai no longer supports. regrow it, or migrate it by hand (see \"Migrating an\n\
             older tree\" in bonsai's README)."
        );
        std::process::exit(1);
    }
}

/// `sync`: regenerate src/sap.rs after hand edits (to bonsai.toml, the
/// `Nutrient` enum, or a branch's arms/releases).
fn sync() -> io::Result<()> {
    require_managed("sync");
    let before = std::fs::read_to_string(SAP_RS).unwrap_or_default();
    sync_sap()?;
    if std::fs::read_to_string(SAP_RS).unwrap_or_default() == before {
        println!("{SAP_RS} is already in step with the wiring.");
    }
    Ok(())
}

/// `path <Nutrient> [broadcast|directed|state] [--cap N]`: show or reshape one
/// nutrient's path. Changing the shape without a cap resets the cap to the new
/// shape's default.
fn path(nutrient: &str, args: &[String]) -> io::Result<()> {
    require_managed("path");
    if !is_variant(nutrient) {
        eprintln!("nutrient name must be an UpperCamelCase identifier, got {nutrient:?}");
        std::process::exit(2);
    }
    let trunk = std::fs::read_to_string("src/trunk.rs")?;
    if !parse_nutrients(&trunk).iter().any(|n| n == nutrient) {
        eprintln!("no `{nutrient}` nutrient — add it first with `bonsai feed {nutrient}`.");
        std::process::exit(1);
    }
    let current = load_config().path(nutrient);
    // Accept the shape bare (`directed`) or as feed spells it (`--directed`).
    let flags: Vec<String> = args
        .iter()
        .map(|a| match flow::Shape::parse(a) {
            Some(_) => format!("--{a}"),
            None => a.clone(),
        })
        .collect();
    let (rest, shape, cap) = parse_feed_args(&flags);
    if !rest.is_empty() {
        eprintln!("usage: bonsai path <Nutrient> [broadcast|directed|state] [--cap N]");
        std::process::exit(2);
    }
    if shape.is_none() && cap.is_none() {
        let g = tree_graph();
        let line = g
            .flow_lines()
            .into_iter()
            .zip(&g.paths)
            .find(|(_, p)| p.nutrient == nutrient)
            .map(|(l, _)| l)
            .unwrap_or_default();
        println!("{line}");
        return Ok(());
    }
    let shape = shape.unwrap_or(current.shape);
    if shape == flow::Shape::State && cap.is_some() {
        eprintln!("a state path holds exactly one value (the latest) — it takes no --cap.");
        std::process::exit(2);
    }
    let cap = cap.or((shape == current.shape).then_some(current.cap));
    let cfg = flow::PathCfg::new(shape, cap);
    std::fs::write(CONFIG, edit_config(|c| flow::set_path(c, nutrient, cfg))?)?;
    let cap = match shape {
        flow::Shape::State => String::new(),
        _ => format!(", cap {}", cfg.cap),
    };
    println!("`{nutrient}` flows on a {} path{cap}:", shape.name());
    println!("  ~ {CONFIG}");
    sync_sap()
}

fn print_help() {
    println!("bonsai — grow a Linux application as a tree: a trunk, and branches you add");
    println!();
    println!("usage:");
    println!("  bonsai                 plant a new tree (interactive wizard)");
    println!("  bonsai init            plant it in the cwd instead of a new folder");
    println!("  bonsai branch [--produces|--duplex|--roots] [<name>]  graft a subsystem");
    println!("                         (no name → interactive: name + kind)");
    println!("  bonsai snip <name>     prune a subsystem off the tree in the cwd");
    println!("  bonsai feed [<Name> [--broadcast|--directed|--state] [--cap N] [field:type ...]]");
    println!("                         add a Nutrient variant + its path (no args → interactive)");
    println!("  bonsai starve <Name>   remove a Nutrient event variant from the trunk");
    println!("  bonsai tap <branch> <Nutrient>       branch consumes the nutrient");
    println!("  bonsai untap <branch> <Nutrient>     stop consuming it");
    println!("  bonsai release <branch> <Nutrient>   branch produces the nutrient");
    println!("  bonsai unrelease <branch> <Nutrient> stop producing it");
    println!("  bonsai path <Nutrient> [broadcast|directed|state] [--cap N]");
    println!("                         show or reshape one nutrient's path");
    println!("  bonsai sync            regenerate src/sap.rs after hand edits");
    println!("  bonsai list            summarize the tree (device, flow graph, branches)");
    println!("  bonsai update          refresh template crates and Cargo.lock");
    println!("  bonsai regrow          reset the tree in the cwd to a fresh template");
    println!(
        "  bonsai retarget <board>  move the tree to another board (pi5, zero-2w, zero-w, host)"
    );
    println!("  bonsai tools [<tool> ...]  pick build tools: sccache, mold, zigbuild, bacon");
    println!("                         (no names → interactive; missing ones install once)");
    println!("  bonsai help            show this help");
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => create_device(false),
        [cmd] if cmd == "init" => create_device(true),
        [cmd] if cmd == "help" || cmd == "-h" || cmd == "--help" => {
            print_help();
            Ok(())
        }
        [cmd] if cmd == "branch" => branch_interactive(),
        [branch, flag, name] if branch == "branch" && flag == "--produces" => {
            add_branch(name, BranchMode::Producer)
        }
        [branch, flag, name] if branch == "branch" && flag == "--duplex" => {
            add_branch(name, BranchMode::Duplex)
        }
        [branch, flag, name] if branch == "branch" && flag == "--roots" => {
            add_branch(name, BranchMode::Roots)
        }
        [branch, flag, ..] if branch == "branch" && flag.starts_with("--") => {
            eprintln!("usage: bonsai branch [--produces|--duplex|--roots] <name>");
            std::process::exit(2);
        }
        [branch, name] if branch == "branch" => add_branch(name, BranchMode::Consumer),
        [snip, name] if snip == "snip" => remove_branch(name),
        [cmd] if cmd == "feed" => add_nutrient_interactive(),
        [feed, name, rest @ ..] if feed == "feed" => {
            let (fields, shape, cap) = parse_feed_args(rest);
            if shape == Some(flow::Shape::State) && cap.is_some() {
                eprintln!("a state path holds exactly one value (the latest) — it takes no --cap.");
                std::process::exit(2);
            }
            let path = (shape.is_some() || cap.is_some())
                .then(|| flow::PathCfg::new(shape.unwrap_or(flow::Shape::Broadcast), cap));
            add_nutrient(name, &fields, path)
        }
        [starve, name] if starve == "starve" => remove_nutrient(name),
        [cmd, branch, nutrient] if cmd == "tap" => tap(branch, nutrient),
        [cmd, branch, nutrient] if cmd == "untap" => untap(branch, nutrient),
        [cmd, branch, nutrient] if cmd == "release" => release(branch, nutrient),
        [cmd, branch, nutrient] if cmd == "unrelease" => unrelease(branch, nutrient),
        [cmd, nutrient, rest @ ..] if cmd == "path" => path(nutrient, rest),
        [cmd] if cmd == "sync" => sync(),
        [cmd] if cmd == "list" => list(),
        [cmd] if cmd == "regrow" => regrow(),
        [cmd] if cmd == "update" => update(),
        [cmd, board] if cmd == "retarget" => retarget(board),
        [cmd, names @ ..] if cmd == "tools" => tools_command(names),
        _ => {
            eprintln!("unrecognised arguments: {}", args.join(" "));
            print_help();
            std::process::exit(2);
        }
    }
}
