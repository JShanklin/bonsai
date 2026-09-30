mod graph;
mod tools;
mod top;
mod tree;

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
// Small interactive prompts, reused by the `branch`/`message` TUIs. Each drives an
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

/// `bonsai branch` with no name: ask for it in a TUI, then add the branch.
fn branch_interactive() -> io::Result<()> {
    tree::require_tree("branch add");
    let mut term = ratatui::init();
    let name = prompt_text_valid(
        &mut term,
        "branch name (snake_case)",
        &[],
        ident_char,
        graph::is_branch_name,
        "snake_case (a-z, 0-9, _), and not a Rust keyword",
    );
    ratatui::restore();
    match name? {
        Some(name) => tree::branch_add(&name),
        None => {
            println!("cancelled");
            Ok(())
        }
    }
}

/// `bonsai message` with no name: enter the name and its fields in a TUI.
fn message_interactive() -> io::Result<()> {
    tree::require_tree("message add");
    let mut term = ratatui::init();
    let entered = (|| -> io::Result<Option<(String, Vec<String>)>> {
        let Some(name) = prompt_text_valid(
            &mut term,
            "message name (UpperCamelCase)",
            &[],
            ident_char,
            |t| is_ident(t) && t.starts_with(|c: char| c.is_ascii_uppercase()),
            "an UpperCamelCase identifier",
        )?
        else {
            return Ok(None);
        };
        let mut fields: Vec<String> = Vec::new();
        loop {
            let mut ctx = vec![format!("message: {name}")];
            ctx.extend(fields.iter().map(|f| format!("  {f}")));
            let Some(fname) = prompt_text_valid(
                &mut term,
                "field name (empty = done)",
                &ctx,
                ident_char,
                |t| t.is_empty() || is_ident(t),
                "a snake_case identifier (or empty to finish)",
            )?
            else {
                return Ok(None);
            };
            if fname.is_empty() {
                return Ok(Some((name, fields)));
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
    })();
    ratatui::restore();
    match entered? {
        Some((name, fields)) => tree::message_add(&name, &fields),
        None => {
            println!("cancelled");
            Ok(())
        }
    }
}

fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template_file(board: &str, f: &str) -> &'static str {
        let path = format!("linux/{board}/{f}");
        TEMPLATES
            .get_file(&path)
            .and_then(|f| f.contents_utf8())
            .unwrap_or_else(|| panic!("{path} embedded"))
    }

    // Guards against the embedded template silently dropping dotfiles — a
    // missing .cargo/config.toml would produce a project that can't build.
    // Walks every board, so a board added to BOARDS without its template fails.
    #[test]
    fn embedded_template_includes_all_files() {
        const COMMON: &[&str] = &[
            ".gitignore",
            ".cargo/config.toml",
            "Cargo.toml",
            "cargo-generate.toml",
            "bonsai.toml",
            "src/main.rs",
            "src/bonsai.rs",
            "src/messages.rs",
            "src/settings.rs",
            "src/wiring.rs",
            "src/branches/mod.rs",
            "src/branches/pulse.rs",
            "src/edges/mod.rs",
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

    // Every board's shipped generated files must be exactly what `bonsai sync`
    // writes from that template's own bonsai.toml and messages.rs — otherwise
    // a fresh tree would change on its first command. The branch sources and
    // messages are the same on every board.
    /// `cargo fmt` in a tree must leave the generated files it formats as
    /// they are (src/wiring.rs and src/settings.rs are `#[rustfmt::skip]`),
    /// or `cargo fmt --check` fails on a new tree and `bonsai sync` undoes it.
    #[test]
    fn generated_files_are_rustfmt_clean() {
        use std::io::Write;
        let fmt = |src: &str| -> Option<String> {
            let mut child = Command::new("rustfmt")
                .args(["--edition", "2024", "--emit", "stdout"])
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .ok()?;
            child.stdin.take()?.write_all(src.as_bytes()).ok()?;
            let out = child.wait_with_output().ok()?;
            Some(String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let Some(_) = fmt("") else {
            eprintln!("rustfmt isn't installed; skipping");
            return;
        };
        let cfg = graph::parse(
            "[branch.zeta]\n[branch.alpha]\n[edge.radio]\nkind = \"custom\"\n\
             [edge.gps]\nkind = \"serial\"\ndevice = \"/dev/ttyS0\"\nbaud = 9600\n",
        )
        .unwrap();
        let empty = graph::parse("").unwrap();
        for (what, src) in [
            ("the runtime", tree::RUNTIME.to_string()),
            ("the serial edge", tree::SERIAL.to_string()),
            ("branches/mod.rs", graph::render_mod(&cfg)),
            ("edges/mod.rs", graph::render_edges_mod(&cfg)),
            ("an empty edges/mod.rs", graph::render_edges_mod(&empty)),
        ] {
            assert_eq!(
                fmt(&src).as_deref(),
                Some(src.as_str()),
                "{what} isn't rustfmt-clean"
            );
        }
    }

    #[test]
    fn template_wiring_matches_generator() {
        for &board in &board_names() {
            let file = |f: &str| template_file(board, f);
            let cfg = graph::parse(file("bonsai.toml")).unwrap();
            let report = graph::check(&cfg, &graph::parse_messages(file("src/messages.rs")));
            assert_eq!(report, graph::Report::default(), "{board}");
            let stale = |f: &str| format!("linux/{board}/{f} is stale — run `bonsai sync` there");
            assert!(
                file("src/bonsai.rs") == tree::RUNTIME,
                "{}",
                stale("src/bonsai.rs")
            );
            assert!(
                file("src/wiring.rs") == graph::render_wiring(&cfg),
                "{}",
                stale("src/wiring.rs")
            );
            assert!(
                file("src/settings.rs") == graph::render_settings(&cfg),
                "{}",
                stale("src/settings.rs")
            );
            assert!(
                file("src/branches/mod.rs") == graph::render_mod(&cfg),
                "{}",
                stale("src/branches/mod.rs")
            );
            assert!(
                file("src/edges/mod.rs") == graph::render_edges_mod(&cfg),
                "{}",
                stale("src/edges/mod.rs")
            );
            let main = file("src/main.rs");
            assert!(
                main.contains("#[macro_use]\nmod bonsai;\n")
                    && tree::with_macro_use(main).is_none(),
                "linux/{board}/src/main.rs needs `#[macro_use]` right above `mod bonsai;`"
            );
            for f in ["bonsai.toml", "src/messages.rs", "src/branches/pulse.rs"] {
                assert_eq!(
                    file(f),
                    template_file("host", f),
                    "{board}/{f} differs from host's"
                );
            }
            assert!(
                file("src/messages.rs")
                    .lines()
                    .any(|l| l == tree::MESSAGE_MARKER)
            );
            assert!(
                file("src/branches/pulse.rs")
                    .lines()
                    .any(|l| l.trim() == tree::INPUT_ARM)
            );
        }
    }

    #[test]
    fn branch_scaffold_fills_in_its_names() {
        let src = tree::BRANCH_TEMPLATE
            .replace("{{branch_name}}", "radio_link")
            .replace("{{BranchName}}", &graph::camel("radio_link"));
        assert!(!src.contains("{{"), "{src}");
        assert!(src.contains("pub struct RadioLink {}"), "{src}");
        assert!(
            src.contains("use crate::wiring::radio_link::{Input, Out};"),
            "{src}"
        );
        assert!(src.lines().any(|l| l.trim() == tree::INPUT_ARM), "{src}");
    }

    // A comment mentioning a marker must not be mistaken for it.
    #[test]
    fn marker_matches_whole_line_not_substring() {
        let src = "//! keep the // bonsai:message marker intact\n\n// bonsai:message\n".to_string();
        let out = insert_before_marker(
            Path::new("messages.rs"),
            src,
            "// bonsai:message",
            "pub struct X;\n",
        );
        assert!(
            out.contains("pub struct X;\n// bonsai:message\n"),
            "inserted wrong:\n{out}"
        );
        assert!(
            out.starts_with("//! keep the // bonsai:message marker intact\n"),
            "doc line was split:\n{out}"
        );
    }

    // `insert_indented_before` puts content above a whole-line marker, matching
    // its indentation; no marker → None.
    #[test]
    fn insert_indented_before_matches_indent() {
        let src = "        match input {\n            // bonsai:input-arm\n        }\n";
        let out = insert_indented_before(src, "// bonsai:input-arm", "Input::Tick => {}").unwrap();
        assert!(
            out.contains("            Input::Tick => {}\n            // bonsai:input-arm\n"),
            "inserted wrong (or indent lost):\n{out}"
        );
        assert!(
            insert_indented_before("    match input {}\n", "// bonsai:input-arm", "x").is_none()
        );
    }

    // After `cargo fmt`, the marker trails the last arm; it goes back on its
    // own line, and new arms land above it again.
    #[test]
    fn marker_pulled_up_by_rustfmt_is_put_back() {
        let m = "// bonsai:input-arm";
        let empty_arm =
            "        match input {\n            Input::Tick => {} // bonsai:input-arm\n        }\n";
        let fixed = marker_on_own_line(empty_arm, m);
        assert_eq!(
            fixed,
            "        match input {\n            Input::Tick => {}\n            // bonsai:input-arm\n        }\n"
        );
        let out = insert_indented_before(&fixed, m, "Input::Beat(_beat) => {}").unwrap();
        assert!(out.contains("Input::Tick => {}\n            Input::Beat(_beat) => {}\n            // bonsai:input-arm\n"));
        let block_arm = "    match input {\n        Input::Tick => {\n            go();\n        } // bonsai:input-arm\n    }\n";
        assert_eq!(
            marker_on_own_line(block_arm, m),
            "    match input {\n        Input::Tick => {\n            go();\n        }\n        // bonsai:input-arm\n    }\n"
        );
        // Prose that mentions the marker, and the marker itself, are left alone.
        for same in [
            "/// keep the `// bonsai:input-arm` line\n",
            "    // bonsai:input-arm\n",
        ] {
            assert_eq!(marker_on_own_line(same, m), same);
        }
    }

    // A filled-in arm spanning lines comes out whole.
    #[test]
    fn balanced_span_takes_a_multiline_arm() {
        let src = "match input {\n    Input::Beat(b) => {\n        count(b);\n    }\n    Input::Tick => {}\n}\n";
        let out =
            remove_balanced_span(src, |l| l.trim_start().starts_with("Input::Beat(")).unwrap();
        assert_eq!(out, "match input {\n    Input::Tick => {}\n}\n");
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
            let manifest = template_file(board, "Cargo.toml");
            assert_eq!(parse_board(manifest).as_deref(), Some(board));
            assert!(
                manifest.contains(&format!("for {board} ({chip})")),
                "{board}: stamp names another chip"
            );
        }
    }

    // regrow reads the board back out of the Cargo.toml stamp.
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

    #[test]
    fn bonsai_1_commands_point_at_their_replacements() {
        for old in [
            "snip",
            "feed",
            "starve",
            "tap",
            "untap",
            "release",
            "unrelease",
            "path",
        ] {
            assert!(renamed(old).is_some(), "{old}");
        }
        assert!(renamed("wire").is_none());
    }

    // End-to-end on a real template: add two branches and a message, wire them,
    // give one a rate, then undo it all. Every file must come back exactly as
    // it started — including the arms added to and taken out of branches, and
    // the comments in bonsai.toml.
    //
    // The commands work on the cwd, and set_current_dir is process-wide — keep
    // this the ONLY test that changes cwd (all others use absolute/temp paths).
    #[test]
    fn fs_round_trip_branch_edge_message_wire_rate() {
        let root = std::env::temp_dir().join("bonsai-test-roundtrip");
        let _ = std::fs::remove_dir_all(&root);
        let tmpl = TEMPLATES.get_dir("linux/zero-w").unwrap();
        extract_dir(tmpl, tmpl.path(), &root).unwrap();
        let tracked = [
            "bonsai.toml",
            "src/messages.rs",
            "src/wiring.rs",
            "src/settings.rs",
            "src/bonsai.rs",
            "src/branches/mod.rs",
            "src/branches/pulse.rs",
            "src/edges/mod.rs",
            "Cargo.toml",
        ];
        let fresh: Vec<String> = tracked
            .iter()
            .map(|f| std::fs::read_to_string(root.join(f)).unwrap())
            .collect();
        let here = std::env::current_dir().unwrap();
        std::env::set_current_dir(&root).unwrap();
        let read = |f: &str| std::fs::read_to_string(f).unwrap();

        tree::branch_add("sensor").unwrap();
        tree::branch_add("display").unwrap();
        tree::message_add("Reading", &["temp_c:f32".to_string()]).unwrap();
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        tree::wire("sensor", &args(&["Reading", "display", "pulse"])).unwrap();
        tree::rate("sensor", "10").unwrap();
        tree::sync().unwrap();

        let toml = read("bonsai.toml");
        assert!(toml.contains("[branch.sensor]\nrate = 10\n"), "{toml}");
        assert!(
            toml.contains("[[wire]]\nfrom = \"sensor\"\nmessage = \"Reading\"\nto = [\"display\", \"pulse\"]\n"),
            "{toml}"
        );
        assert!(read("src/branches/display.rs").contains(
            "            Input::Reading(_reading) => {}\n            // bonsai:input-arm\n"
        ));
        assert!(read("src/branches/pulse.rs").contains("Input::Reading(_reading) => {}"));
        assert!(read("src/branches/sensor.rs").contains("            Input::Tick => {}\n"));
        let wiring = read("src/wiring.rs");
        assert!(wiring.contains("impl Sends<Reading> for Out"), "{wiring}");
        assert!(
            read("src/branches/mod.rs")
                .contains("pub mod display;\npub mod pulse;\npub mod sensor;\n")
        );

        // A filled-in arm spanning lines is removed whole by unwire.
        let display = read("src/branches/display.rs").replace(
            "Input::Reading(_reading) => {}",
            "Input::Reading(reading) => {\n                println!(\"{}\", reading.temp_c);\n            }",
        );
        std::fs::write("src/branches/display.rs", display).unwrap();

        // Edges: a UDP link both ways, a serial port in, and a custom edge.
        tree::edge_add(
            "link",
            &args(&["udp", "--bind", "0.0.0.0:6969", "--join", "239.2.3.2"]),
        )
        .unwrap();
        tree::edge_add(
            "gps",
            &args(&["serial", "--device", "/dev/serial0", "--baud", "9600"]),
        )
        .unwrap();
        tree::edge_add("radio", &args(&["--custom"])).unwrap();
        tree::wire("link", &args(&["display"])).unwrap();
        tree::wire("display", &args(&["link", "radio"])).unwrap();
        tree::wire("gps", &args(&["display", "sensor"])).unwrap();
        let toml = read("bonsai.toml");
        assert!(
            toml.contains(
                "[edge.link]\nkind = \"udp\"\nbind = \"0.0.0.0:6969\"\njoin = [\"239.2.3.2\"]\n"
            ),
            "{toml}"
        );
        assert!(
            toml.contains(
                "[edge.gps]\nkind = \"serial\"\ndevice = \"/dev/serial0\"\nbaud = 9600\n"
            ),
            "{toml}"
        );
        assert!(
            toml.contains("[[wire]]\nfrom = \"display\"\nto = [\"link\", \"radio\"]\n"),
            "{toml}"
        );
        let display = read("src/branches/display.rs");
        assert!(display.contains("Input::Link(_link) => {}"), "{display}");
        assert!(display.contains("Input::Gps(_gps) => {}"), "{display}");
        assert!(read("src/branches/sensor.rs").contains("Input::Gps(_gps) => {}"));
        assert!(read("Cargo.toml").contains("tokio-serial = \"5.5\""));
        assert!(read("src/edges/serial.rs").contains("pub struct Serial"));
        assert!(read("src/edges/radio.rs").contains("impl Edge for Radio"));
        assert!(read("src/edges/mod.rs").contains("pub mod serial;"));
        assert!(read("src/wiring.rs").contains("pub fn to_link(&mut self"));

        tree::unwire("display", &args(&["radio"])).unwrap();
        tree::edge_remove("radio").unwrap();
        assert!(!root.join("src/edges/radio.rs").exists());
        tree::edge_remove("gps").unwrap();
        assert!(!read("src/branches/sensor.rs").contains("Input::Gps"));
        assert!(!read("Cargo.toml").contains("tokio-serial"));
        assert!(!root.join("src/edges/serial.rs").exists());
        tree::unwire("link", &args(&[])).unwrap();
        assert!(!read("src/branches/display.rs").contains("Input::Link"));
        tree::edge_remove("link").unwrap();

        tree::unwire("sensor", &args(&["Reading", "pulse"])).unwrap();
        assert!(!read("src/branches/pulse.rs").contains("Input::Reading"));
        tree::unwire("sensor", &args(&["Reading"])).unwrap();
        assert!(!read("src/branches/display.rs").contains("Input::Reading"));
        tree::rate("sensor", "off").unwrap();
        assert!(!read("src/branches/sensor.rs").contains("Input::Tick"));
        tree::message_remove("Reading").unwrap();
        tree::branch_remove("display").unwrap();
        tree::branch_remove("sensor").unwrap();

        let back: Vec<String> = tracked.iter().map(|f| read(f)).collect();
        std::env::set_current_dir(&here).unwrap();
        for ((f, before), after) in tracked.iter().zip(&fresh).zip(&back) {
            assert_eq!(after, before, "{f} didn't round-trip");
        }
        assert!(!root.join("src/branches/sensor.rs").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}

// ---------------------------------------------------------------------------
// Source edits the tree commands share (src/tree.rs): markers, spans, fields.
// ---------------------------------------------------------------------------

/// Insert `content` as a line just above the whole-line `marker`, indented to
/// match it. None if the file has no such marker.
/// `src` with `marker` back on a line of its own where `cargo fmt` has pulled
/// it up behind the code before it (`Input::Tick => {} // bonsai:input-arm`,
/// or `} // bonsai:input-arm` after a block arm): rustfmt makes a comment
/// that follows an arm with no comma into that arm's trailing comment. A
/// line that is itself a comment is prose, and left alone.
fn marker_on_own_line(src: &str, marker: &str) -> String {
    let mut out = String::with_capacity(src.len() + 16);
    for l in src.split_inclusive('\n') {
        let body = l.trim_end_matches(['\n', '\r']);
        if let Some(code) = body.strip_suffix(marker)
            && code.ends_with(' ')
            && !code.trim().is_empty()
            && !code.trim_start().starts_with("//")
        {
            let indent: String = l.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
            out.push_str(code.trim_end());
            out.push('\n');
            out.push_str(&indent);
            out.push_str(marker);
            out.push_str(&l[body.len()..]);
        } else {
            out.push_str(l);
        }
    }
    out
}

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

/// `manifest` without the `name` dependency, or None if it has none.
fn without_dependency(manifest: &str, name: &str) -> io::Result<Option<String>> {
    use std::str::FromStr;
    use toml_edit::DocumentMut;

    let mut doc = DocumentMut::from_str(manifest)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let removed = doc
        .get_mut("dependencies")
        .and_then(|d| d.as_table_like_mut())
        .and_then(|d| d.remove(name));
    Ok(removed.map(|_| doc.to_string()))
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
    tree::require_tree("retarget");
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
    // stamp, the graph, the messages and the generated wiring. A stray
    // directory can't match all of these. (An older, Embassy tree has the
    // stamp and its own trunk files instead.)
    let cargo_toml = std::fs::read_to_string(root.join("Cargo.toml")).unwrap_or_default();
    let is_tree = cargo_toml.contains("Generated by bonsai for")
        && root.join(tree::CONFIG).is_file()
        && (file_contains(&root.join(tree::MESSAGES), tree::MESSAGE_MARKER)
            || file_contains(&root.join("src/trunk.rs"), "// bonsai:nutrient"))
        && (root.join("src/wiring.rs").is_file() || root.join("src/sap.rs").is_file());
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

fn print_help() {
    println!("bonsai — grow a Linux application as a tree: a trunk, and branches you add");
    println!();
    println!("usage:");
    println!("  bonsai                 plant a new tree (interactive wizard)");
    println!("  bonsai init            plant it in the cwd instead of a new folder");
    println!("  bonsai branch add <name>      add a branch (no name → interactive)");
    println!("  bonsai branch remove <name>   remove it, and every wire from or to it");
    println!("  bonsai message add <Name> [field:type ...]  add a message type");
    println!("                         (no name → interactive)");
    println!("  bonsai message remove <Name>  remove an unwired message type");
    println!("  bonsai edge add <name> udp|tcp|serial [--bind A] [--to A] [--join G] [--reply]");
    println!(
        "                         [--connect A] [--listen A] [--device D --baud N] [--framing lines]"
    );
    println!("  bonsai edge add <name> --custom   an edge of your own (src/edges/<name>.rs)");
    println!("  bonsai edge remove <name>     remove it, and every wire from or to it");
    println!("  bonsai wire <from> <Message> <to> [<to> ...]  from sends it to each branch");
    println!("  bonsai wire <from> <to> [<to> ...]   with an edge at one end (no message)");
    println!("  bonsai unwire <from> [<Message>] [<to> ...]   stop (all when none named)");
    println!("  bonsai rate <branch> <hz|off>   tick a branch this many times a second");
    println!("  bonsai sync            regenerate the wiring after editing bonsai.toml");
    println!("  bonsai list            the tree's branches and wires, and any warnings");
    println!("  bonsai top [user@host|local] [--port N] [--once]");
    println!("                         watch a running tree: its branches, edges and log");
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
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        [] => create_device(false),
        ["init"] => create_device(true),
        ["help" | "-h" | "--help"] => {
            print_help();
            Ok(())
        }
        ["branch"] | ["branch", "add"] => branch_interactive(),
        ["branch", "add", name] => tree::branch_add(name),
        ["branch", "remove", name] => tree::branch_remove(name),
        ["message"] | ["message", "add"] => message_interactive(),
        ["message", "add", name, fields @ ..] => {
            let fields: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
            tree::message_add(name, &fields)
        }
        ["message", "remove", name] => tree::message_remove(name),
        ["edge", "add", name, rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|a| a.to_string()).collect();
            tree::edge_add(name, &rest)
        }
        ["edge", "remove", name] => tree::edge_remove(name),
        ["wire", from, rest @ ..] if !rest.is_empty() => {
            let rest: Vec<String> = rest.iter().map(|a| a.to_string()).collect();
            tree::wire(from, &rest)
        }
        ["unwire", from, rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|a| a.to_string()).collect();
            tree::unwire(from, &rest)
        }
        ["rate", branch, hz] => tree::rate(branch, hz),
        ["sync"] => tree::sync(),
        ["list"] => tree::list(),
        ["regrow"] => regrow(),
        ["update"] => update(),
        ["retarget", board] => retarget(board),
        ["top", rest @ ..] => {
            let rest: Vec<String> = rest.iter().map(|a| a.to_string()).collect();
            top::top(&rest).map_err(|e| {
                eprintln!("bonsai top: {e}");
                std::process::exit(1)
            })
        }
        ["tools", names @ ..] => {
            let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
            tools_command(&names)
        }
        [old, ..] if renamed(old).is_some() => {
            eprintln!(
                "`bonsai {old}` is from bonsai 1; now it's {}",
                renamed(old).unwrap_or_default()
            );
            std::process::exit(2);
        }
        _ => {
            eprintln!("unrecognised arguments: {}", args.join(" "));
            print_help();
            std::process::exit(2);
        }
    }
}

/// What a bonsai 1 command became.
fn renamed(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "snip" => "`bonsai branch remove <name>`",
        "feed" => "`bonsai message add <Name> [field:type ...]`",
        "starve" => "`bonsai message remove <Name>`",
        "tap" | "release" => "`bonsai wire <from> <Message> <to>`, a wire in bonsai.toml",
        "untap" | "unrelease" => "`bonsai unwire <from> <Message> [<to>]`",
        "path" => "gone: every message is delivered in order by the core",
        "ide" => "gone, with microcontroller support",
        "roots" => "an edge: `bonsai edge add <name> udp|tcp|serial ...`",
        _ => return None,
    })
}
