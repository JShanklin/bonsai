//! bonsai's runtime: the core loop this tree runs on, and the edges that
//! connect it to the outside world. Written by `bonsai sync`, which rewrites
//! it; don't edit it.
//!
//! Every branch runs in one core loop, one event at a time. An event (a tick,
//! or something an edge received) goes to the branches linked to it, and every
//! message they send is delivered, in order, before the next event is taken.
//! The same events in always give the same messages out.
//!
//! Edges do the I/O, each in its own task: what they receive becomes an event
//! for the core, and what branches send to them they carry out. An edge that
//! fails is started again on its own; the core never waits for one.
//!
//! Log with `error!`, `warn!`, `info!` and `debug!` (like `println!`), from a
//! branch or an edge: each line is tagged with who wrote it. `BONSAI_LOG`
//! picks what's shown: `debug`, or `warn,gps=debug` (the default is `info`).
//!
//! `record!("launch at {alt}")` also writes the line to this run's log folder
//! (`[record]` in bonsai.toml), next to the panics, errors and edge changes
//! it's set to keep.
#![allow(dead_code)]

/// Log an error: `error!("lost {name}")`.
#[allow(unused_macros)]
macro_rules! error {
    ($($arg:tt)+) => { $crate::bonsai::log::write($crate::bonsai::log::Level::Error, None, format_args!($($arg)+)) };
}

/// Log a warning: something's wrong, and the tree carries on.
#[allow(unused_macros)]
macro_rules! warn {
    ($($arg:tt)+) => { $crate::bonsai::log::write($crate::bonsai::log::Level::Warn, None, format_args!($($arg)+)) };
}

/// Log what's happening, shown by default.
#[allow(unused_macros)]
macro_rules! info {
    ($($arg:tt)+) => { $crate::bonsai::log::write($crate::bonsai::log::Level::Info, None, format_args!($($arg)+)) };
}

/// Log detail, shown only when asked for: `BONSAI_LOG=<branch>=debug`.
#[allow(unused_macros)]
macro_rules! debug {
    ($($arg:tt)+) => { $crate::bonsai::log::write($crate::bonsai::log::Level::Debug, None, format_args!($($arg)+)) };
}

/// An event worth keeping: logged like `info!`, and written to this run's
/// `events.log` when `[record] events = true`.
#[allow(unused_macros)]
macro_rules! record {
    ($($arg:tt)+) => { $crate::bonsai::record::event(format_args!($($arg)+)) };
}

use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket, tcp::OwnedWriteHalf};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

// ---------------------------------------------------------------------------
// Branches and the core
// ---------------------------------------------------------------------------

/// A branch: its state, and what it decides for each input.
pub trait Branch: Sized {
    /// What it receives: one variant per link into it, plus `Tick` when it
    /// has a `rate`. Generated as `crate::links::<branch>::Input`.
    type Input;
    /// Where it sends: `crate::links::<branch>::Out`.
    type Out: Default + Outbox;

    /// Setup: the branch's starting state. Runs again after `process`
    /// panics. If it panics itself, the branch is out of service (its
    /// inputs dropped) until a later `setup` works: see `Slot`.
    fn setup() -> Self;

    /// Process: decide what to do with one input, and `out.send(..)` the
    /// results. No I/O and no waiting here, so the same inputs always give
    /// the same outputs.
    fn process(&mut self, input: Self::Input, out: &mut Self::Out);
}

/// `out.send(message)`: there's one for each message a branch is linked to send.
pub trait Sends<M> {
    fn send(&mut self, message: M);
}

/// A branch's `Out`: how much it holds, for the branch's stats.
pub trait Outbox {
    fn count(&self) -> usize;
}

/// Something from outside the core.
#[derive(Clone, Debug, PartialEq)]
pub enum Event<E> {
    /// A branch's `rate` ticked: its index in the tree.
    Tick(usize),
    /// An edge received something (`crate::links::EdgeIn`).
    Edge(E),
}

/// The generated tree, `crate::links::Core`.
pub trait Tree {
    /// What the edges hand the core, one variant per edge.
    type EdgeIn: Send + 'static;
    /// The branches with a `rate`: (index, ticks per second).
    fn rates(&self) -> Vec<(usize, f64)>;
    /// Start every edge, each sending what it receives to `events`.
    fn start_edges(&mut self, events: &mpsc::Sender<Event<Self::EdgeIn>>);
    /// Take one event and everything it sets off.
    fn handle(&mut self, event: Event<Self::EdgeIn>);
}

/// How long a branch whose `setup` panicked waits before it's tried again;
/// the wait doubles with each failure, up to `SETUP_RETRY_MAX`.
pub const SETUP_RETRY: Duration = Duration::from_secs(1);
pub const SETUP_RETRY_MAX: Duration = Duration::from_secs(60);

/// One branch in the core. When its `process` panics, what it sent for that
/// input is dropped and it's set up again. When `setup` panics too (or at
/// startup), the branch is out of service: its inputs are discarded (and
/// counted), the rest of the tree carries on, and `setup` is tried again on
/// an input after `SETUP_RETRY`, then twice as long each time it fails, up
/// to `SETUP_RETRY_MAX`: never in a tight loop.
pub struct Slot<B: Branch> {
    name: &'static str,
    /// None while out of service.
    branch: Option<B>,
    /// Out of service: when to try `setup` again, and the wait after that.
    retry: Option<(Instant, Duration)>,
    stats: Arc<stats::BranchStats>,
}

impl<B: Branch> Slot<B> {
    pub fn new(name: &'static str) -> Self {
        let mut slot = Slot {
            name,
            branch: None,
            retry: None,
            stats: stats::branch(name),
        };
        slot.set_up(SETUP_RETRY);
        slot
    }

    /// Run `setup`, catching a panic: in service after it, or out of
    /// service for `wait`.
    fn set_up(&mut self, wait: Duration) -> bool {
        let outer = log::enter(self.name);
        let made = catch_unwind(B::setup);
        log::enter(outer);
        match made {
            Ok(branch) => {
                self.branch = Some(branch);
                self.retry = None;
                self.stats.failed.store(false, Relaxed);
                true
            }
            Err(_) => {
                self.branch = None;
                self.retry = Some((Instant::now() + wait, wait));
                self.stats.failed.store(true, Relaxed);
                log::write(
                    log::Level::Error,
                    Some(self.name),
                    format_args!(
                        "setup panicked: out of service, its inputs dropped; trying again in {wait:?}"
                    ),
                );
                false
            }
        }
    }

    /// Process one input; what it sent, or nothing if it panicked or the
    /// branch is out of service.
    pub fn process(&mut self, input: B::Input) -> B::Out {
        if self.branch.is_none() {
            let Some((at, wait)) = self.retry else {
                return B::Out::default();
            };
            let back = Instant::now() >= at && self.set_up((wait * 2).min(SETUP_RETRY_MAX));
            if !back {
                self.stats.discarded.fetch_add(1, Relaxed);
                return B::Out::default();
            }
            log::write(
                log::Level::Info,
                Some(self.name),
                format_args!("set up again: back in service"),
            );
        }
        let Some(branch) = self.branch.as_mut() else {
            return B::Out::default();
        };
        let mut out = B::Out::default();
        let outer = log::enter(self.name);
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| branch.process(input, &mut out)));
        let took = started.elapsed();
        log::enter(outer);
        if result.is_err() {
            self.stats.record(took, 0, true);
            if self.set_up(SETUP_RETRY) {
                log::write(
                    log::Level::Warn,
                    Some(self.name),
                    format_args!("set up again after a panic"),
                );
            }
            return B::Out::default();
        }
        self.stats.record(took, out.count(), false);
        out
    }
}

/// Messages one event may set off before bonsai assumes branches are sending
/// to each other without end.
pub const MAX_DELIVERIES: usize = 10_000;

/// Deliver everything queued, oldest first, until nothing is left.
pub fn drain<M>(queue: &mut VecDeque<M>, mut deliver: impl FnMut(M, &mut VecDeque<M>)) {
    let mut delivered = 0;
    while let Some(message) = queue.pop_front() {
        delivered += 1;
        if delivered > MAX_DELIVERIES {
            log::write(
                log::Level::Error,
                Some("bonsai"),
                format_args!(
                    "one event set off over {MAX_DELIVERIES} messages; dropping the {} left",
                    queue.len() + 1
                ),
            );
            queue.clear();
            return;
        }
        deliver(message, queue);
    }
}

/// Run the tree until Ctrl-C or SIGTERM.
pub async fn run<T: Tree>(mut tree: T) {
    log::catch_panics();
    stats::start();
    record::start();
    log::write(log::Level::Info, Some("bonsai"), format_args!("running"));
    top::start();
    let (events, mut inbox) = mpsc::channel::<Event<T::EdgeIn>>(1024);
    tree.start_edges(&events);
    for (index, hz) in tree.rates() {
        let events = events.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs_f64(1.0 / hz));
            every.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                every.tick().await;
                if events.send(Event::Tick(index)).await.is_err() {
                    return;
                }
            }
        });
    }
    // Made once: a signal that lands while an event is handled isn't missed.
    let shutdown = shutdown();
    tokio::pin!(shutdown);
    let why = loop {
        tokio::select! {
            Some(event) = inbox.recv() => {
                let started = Instant::now();
                tree.handle(event);
                stats::CORE.record(started.elapsed(), inbox.len());
            }
            why = &mut shutdown => break why,
        }
    };
    log::write(
        log::Level::Info,
        Some("bonsai"),
        format_args!("stopping ({why})"),
    );
    record::end(why).await;
    drop(events);
}

/// Resolves on Ctrl-C, or on SIGTERM (systemd stopping the service): which.
async fn shutdown() -> &'static str {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .unwrap_or_else(|_| panic!("bonsai: can't listen for SIGTERM"));
    tokio::select! {
        _ = tokio::signal::ctrl_c() => "Ctrl-C",
        _ = term.recv() => "SIGTERM",
    }
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

/// The logger behind `error!`, `warn!`, `info!` and `debug!`. Lines go to
/// stderr (journald keeps them for a service) as
/// `14:05:03.123Z  INFO sensor: 26.5 °C`: UTC time, level, who wrote it.
pub mod log {
    use std::cell::Cell;
    use std::collections::VecDeque;
    use std::fmt;
    use std::io::{IsTerminal, Write};
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    pub enum Level {
        Off,
        Error,
        Warn,
        Info,
        Debug,
    }

    impl Level {
        fn parse(word: &str) -> Option<Level> {
            Some(match word.trim().to_ascii_lowercase().as_str() {
                "off" => Level::Off,
                "error" => Level::Error,
                "warn" => Level::Warn,
                "info" => Level::Info,
                "debug" => Level::Debug,
                _ => return None,
            })
        }

        fn label(self) -> &'static str {
            match self {
                Level::Off => "",
                Level::Error => "ERROR",
                Level::Warn => " WARN",
                Level::Info => " INFO",
                Level::Debug => "DEBUG",
            }
        }

        fn color(self) -> &'static str {
            match self {
                Level::Off => "",
                Level::Error => "\x1b[31m",
                Level::Warn => "\x1b[33m",
                Level::Info => "\x1b[32m",
                Level::Debug => "\x1b[34m",
            }
        }
    }

    thread_local! {
        /// The branch whose `process` is running, if any.
        static CURRENT: Cell<&'static str> = const { Cell::new("") };
    }

    tokio::task_local! {
        /// The edge whose task is running.
        pub static EDGE: &'static str;
    }

    /// Tag this thread's lines with `name` (`""`: none); returns the old tag.
    pub fn enter(name: &'static str) -> &'static str {
        CURRENT.with(|c| c.replace(name))
    }

    /// Who's logging: the edge whose task this is, the branch being
    /// processed, or bonsai itself.
    pub fn source() -> &'static str {
        if let Ok(edge) = EDGE.try_with(|e| *e) {
            return edge;
        }
        match CURRENT.with(Cell::get) {
            "" => "bonsai",
            branch => branch,
        }
    }

    /// `BONSAI_LOG`: a default level, then `source=level` for any branch or
    /// edge that should differ: `warn,gps=debug`.
    #[derive(Debug, PartialEq)]
    pub struct Filter {
        default: Level,
        sources: Vec<(String, Level)>,
    }

    impl Filter {
        /// The filter `spec` describes, and the words it didn't understand.
        pub fn parse(spec: &str) -> (Filter, Vec<String>) {
            let mut filter = Filter {
                default: Level::Info,
                sources: Vec::new(),
            };
            let mut bad = Vec::new();
            for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                match part.split_once('=') {
                    Some((source, level)) => match Level::parse(level) {
                        Some(level) => filter.sources.push((source.trim().to_string(), level)),
                        None => bad.push(part.to_string()),
                    },
                    None => match Level::parse(part) {
                        Some(level) => filter.default = level,
                        None => bad.push(part.to_string()),
                    },
                }
            }
            (filter, bad)
        }

        pub fn allows(&self, level: Level, source: &str) -> bool {
            let max = self
                .sources
                .iter()
                .rev()
                .find(|(s, _)| s == source)
                .map_or(self.default, |(_, l)| *l);
            level != Level::Off && level <= max
        }
    }

    struct Settings {
        filter: Filter,
        color: bool,
    }

    fn settings() -> &'static Settings {
        static SETTINGS: OnceLock<Settings> = OnceLock::new();
        SETTINGS.get_or_init(|| {
            let (filter, bad) = Filter::parse(&std::env::var("BONSAI_LOG").unwrap_or_default());
            let color = std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
            if !bad.is_empty() {
                eprintln!(
                    "bonsai: BONSAI_LOG: didn't understand {}; levels are off, error, warn, info, debug",
                    bad.join(", ")
                );
            }
            Settings { filter, color }
        })
    }

    /// Lines kept for `recent`.
    pub const KEEP: usize = 1000;
    /// The lines kept, and how many were ever logged.
    static RECENT: Mutex<(VecDeque<String>, u64)> = Mutex::new((VecDeque::new(), 0));

    /// The last `KEEP` lines logged, oldest first.
    pub fn recent() -> Vec<String> {
        since(0).0
    }

    /// The kept lines numbered `from` on (the first line logged is 0), and the
    /// number of the next line: pass it back to get only what's new.
    pub fn since(from: u64) -> (Vec<String>, u64) {
        let Ok(recent) = RECENT.lock() else {
            return (Vec::new(), from);
        };
        let (lines, total) = &*recent;
        let first = total - lines.len() as u64;
        let skip = from.saturating_sub(first).min(lines.len() as u64) as usize;
        (lines.iter().skip(skip).cloned().collect(), *total)
    }

    /// Log one line, from `source` (or whoever is running, when `None`).
    pub fn write(level: Level, source: Option<&str>, message: fmt::Arguments) {
        let source = source.unwrap_or_else(|| self::source());
        // errors.log keeps every error and warning, whatever the console
        // shows (BONSAI_LOG filters only the console and `bonsai top`).
        if level <= Level::Warn && level != Level::Off {
            super::record::write(
                super::record::Kind::Errors,
                source,
                format_args!("{} {message}", level.label().trim()),
            );
        }
        let settings = settings();
        if !settings.filter.allows(level, source) {
            return;
        }
        let line = format!(
            "{}{}",
            time(SystemTime::now()),
            line(level, source, message)
        );
        let shown = if settings.color {
            let (time, rest) = line.split_at(13);
            let (label, rest) = rest.split_at(6);
            format!(
                "\x1b[2m{time}\x1b[0m{}{label}\x1b[0m{rest}\n",
                level.color()
            )
        } else {
            format!("{line}\n")
        };
        if cfg!(test) {
            eprint!("{shown}"); // what `cargo test` captures
        } else {
            // Not eprint!: that panics once stderr is gone, and a log line
            // mustn't take the tree down.
            let _ = std::io::stderr().write_all(shown.as_bytes());
        }
        if let Ok(mut recent) = RECENT.lock() {
            let (lines, total) = &mut *recent;
            if lines.len() == KEEP {
                lines.pop_front();
            }
            lines.push_back(line);
            *total += 1;
        }
    }

    /// Everything after the time: `  INFO sensor: 26.5 °C`.
    fn line(level: Level, source: &str, message: fmt::Arguments) -> String {
        format!(" {} {source}: {message}", level.label())
    }

    /// `14:05:03.123Z`, UTC.
    pub fn time(now: SystemTime) -> String {
        let since = now.duration_since(UNIX_EPOCH).unwrap_or_default();
        let secs = since.as_secs() % 86_400;
        format!(
            "{:02}:{:02}:{:02}.{:03}Z",
            secs / 3600,
            secs / 60 % 60,
            secs % 60,
            since.subsec_millis()
        )
    }

    /// Log a panic as one line, from whoever panicked. The backtrace follows
    /// only when `RUST_BACKTRACE` asks for it.
    pub fn catch_panics() {
        std::panic::set_hook(Box::new(|info| {
            let message = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                .unwrap_or("(no message)");
            let at = info
                .location()
                .map(|at| format!(" at {}:{}", at.file(), at.line()))
                .unwrap_or_default();
            error!("panicked{at}: {message}");
            super::record::write(
                super::record::Kind::Panics,
                source(),
                format_args!("panicked{at}: {message}"),
            );
            let backtrace = std::backtrace::Backtrace::capture();
            if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
                let _ = writeln!(std::io::stderr(), "{backtrace}");
            }
        }));
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn filter_takes_a_default_and_per_source_levels() {
            let (f, bad) = Filter::parse("warn, gps=debug,beacon=off");
            assert!(bad.is_empty());
            assert!(f.allows(Level::Error, "sensor"));
            assert!(!f.allows(Level::Info, "sensor"));
            assert!(f.allows(Level::Debug, "gps"));
            assert!(!f.allows(Level::Error, "beacon"));
            let (f, bad) = Filter::parse("");
            assert!(bad.is_empty());
            assert!(f.allows(Level::Info, "sensor") && !f.allows(Level::Debug, "sensor"));
            let (f, bad) = Filter::parse("loud,gps=chatty");
            assert_eq!(bad, ["loud", "gps=chatty"]);
            assert!(f.allows(Level::Info, "gps"));
        }

        #[test]
        fn lines_carry_utc_time_level_and_source() {
            let t = UNIX_EPOCH + std::time::Duration::from_millis(86_400_000 * 3 + 50_703_123);
            assert_eq!(time(t), "14:05:03.123Z");
            assert_eq!(
                line(Level::Info, "sensor", format_args!("26.5 °C")),
                "  INFO sensor: 26.5 °C"
            );
            assert_eq!(
                line(Level::Error, "gps", format_args!("x")),
                " ERROR gps: x"
            );
        }

        #[test]
        fn a_branch_being_processed_tags_its_lines() {
            assert_eq!(source(), "bonsai");
            let outer = enter("sensor");
            assert_eq!(source(), "sensor");
            enter(outer);
            assert_eq!(source(), "bonsai");
        }
    }
}

// ---------------------------------------------------------------------------
// Run logs
// ---------------------------------------------------------------------------

/// Run logs: a folder per run, named by the local time it started
/// (`logs/2026-09-30_14-00-05/`), with a file for each kind of line
/// `[record]` in bonsai.toml keeps. Each file opens with a START line and,
/// when the tree stops, closes with an END line saying why. `BONSAI_RECORD`
/// overrides the folder for one run, or turns it `off`.
pub mod record {
    use std::fmt;
    use std::fs::{File, OpenOptions};
    use std::io::{BufWriter, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use tokio::sync::oneshot;

    /// `[record]` from bonsai.toml, generated into `src/links.rs`.
    #[derive(Clone, Copy, Debug)]
    pub struct Config {
        pub dir: &'static str,
        pub events: bool,
        pub panics: bool,
        pub errors: bool,
        pub edges: bool,
        /// Run folders kept, this one included (0: all). Older ones are
        /// deleted when a run starts.
        pub keep_runs: u32,
        /// Run folders older than this many days are deleted (0: never).
        pub keep_days: u32,
        /// A file this big (in KiB) moves to `<kind>.1.log`, replacing the
        /// one there, and a new one starts (0: never).
        pub max_file_kb: u32,
    }

    /// What a line is, and so which file it goes to.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Kind {
        /// `record!(..)`.
        Events,
        /// A branch or an edge panicked.
        Panics,
        /// Every `error!` and `warn!`.
        Errors,
        /// An edge coming up, or going down.
        Edges,
    }

    impl Config {
        /// Everything off, in `logs`.
        pub const DEFAULT: Config = Config {
            dir: "logs",
            events: false,
            panics: false,
            errors: false,
            edges: false,
            keep_runs: KEEP_RUNS,
            keep_days: 0,
            max_file_kb: MAX_FILE_KB,
        };
    }

    const KINDS: [Kind; 4] = [Kind::Events, Kind::Panics, Kind::Errors, Kind::Edges];

    impl Kind {
        fn file(self) -> &'static str {
            match self {
                Kind::Events => "events.log",
                Kind::Panics => "panics.log",
                Kind::Errors => "errors.log",
                Kind::Edges => "edges.log",
            }
        }

        /// Where a full file moves.
        fn old_file(self) -> &'static str {
            match self {
                Kind::Events => "events.1.log",
                Kind::Panics => "panics.1.log",
                Kind::Errors => "errors.1.log",
                Kind::Edges => "edges.1.log",
            }
        }

        fn on(self, config: &Config) -> bool {
            match self {
                Kind::Events => config.events,
                Kind::Panics => config.panics,
                Kind::Errors => config.errors,
                Kind::Edges => config.edges,
            }
        }
    }

    static CONFIG: OnceLock<Config> = OnceLock::new();

    /// Run folders kept unless `keep_runs` says otherwise.
    pub const KEEP_RUNS: u32 = 100;
    /// A file's size, in KiB, before it moves aside, unless `max_file_kb`
    /// says otherwise: 10 MiB, so a kind's two files stay under 20 MiB.
    pub const MAX_FILE_KB: u32 = 10 * 1024;
    /// How much of a previous run's file is read to see how it ended.
    const TAIL: u64 = 4096;
    /// Held (flock) by a running tree in its run folder: a folder whose lock
    /// is held belongs to a run still going.
    const RUNNING: &str = ".running";

    /// Lines waiting for the writer thread, at most: past this, new lines
    /// are dropped (and counted), never waited for.
    pub const QUEUE: usize = 1024;
    /// The longest line kept; a longer one is cut short, ending in `…`.
    pub const MAX_LINE: usize = 8 * 1024;
    /// How soon a line accepted is handed to the OS (flushed).
    pub const FLUSH_EVERY: Duration = Duration::from_millis(100);
    /// How often what's been written is synced to the disk, besides START
    /// and END: what a power loss can take with it.
    pub const SYNC_EVERY: Duration = Duration::from_secs(5);
    /// How long shutdown waits for the writer to finish and write END.
    pub const END_WAIT: Duration = Duration::from_secs(2);

    /// What the writer thread is asked to do, in order.
    enum Job {
        Start {
            dir: PathBuf,
            kinds: Vec<Kind>,
            at: Local,
            config: Config,
        },
        Line(Kind, String),
    }

    /// Taking lines: from `start` until `end`.
    static ACCEPTING: AtomicBool = AtomicBool::new(false);
    static QUEUE_TX: OnceLock<SyncSender<Job>> = OnceLock::new();
    /// Which kinds have a file (set once the writer has opened them).
    static OPEN: [AtomicBool; 4] = [const { AtomicBool::new(false) }; 4];
    /// Lines dropped because the queue was full, per kind; the writer notes
    /// them in the file, and in END.
    static DROPPED: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
    /// Writes that failed (the file is closed after the first).
    static FAILED: AtomicU64 = AtomicU64::new(0);
    /// Set by `end`: why the tree stopped, and who to tell once END is out.
    static ENDING: Mutex<Option<(String, oneshot::Sender<()>)>> = Mutex::new(None);
    static STARTED: OnceLock<Instant> = OnceLock::new();

    #[cfg(test)]
    pub mod fault {
        //! For the runtime's tests: a slow or failing disk.
        use std::sync::atomic::{AtomicBool, AtomicU64};
        /// Each write waits this long first.
        pub static SLOW_MS: AtomicU64 = AtomicU64::new(0);
        /// Each write fails.
        pub static FAIL: AtomicBool = AtomicBool::new(false);
    }

    /// Lines dropped because the recorder fell behind, all told.
    pub fn dropped() -> u64 {
        DROPPED.iter().map(|d| d.load(Relaxed)).sum()
    }

    /// Writes that failed.
    pub fn failed() -> u64 {
        FAILED.load(Relaxed)
    }

    /// Called by the generated `Core::new`; the first call wins.
    pub fn configure(config: Config) {
        let _ = CONFIG.set(config);
    }

    /// Log an event with `info!`, and keep it in `events.log`.
    pub fn event(message: fmt::Arguments) {
        super::log::write(super::log::Level::Info, None, message);
        write(Kind::Events, super::log::source(), message);
    }

    /// Start this run's recorder: a thread that makes the run's folder and
    /// files and writes every line to them. Nothing when `[record]` keeps
    /// nothing, or `BONSAI_RECORD=off`. Never touches the disk itself.
    pub fn start() {
        let Some(config) = CONFIG.get() else { return };
        let dir = match std::env::var("BONSAI_RECORD") {
            Ok(v) if v.trim() == "off" => return,
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
            _ => PathBuf::from(config.dir),
        };
        let kinds: Vec<Kind> = KINDS.into_iter().filter(|k| k.on(config)).collect();
        if kinds.is_empty() {
            return;
        }
        let (tx, rx) = sync_channel(QUEUE);
        let at = Local::now();
        let _ = STARTED.set(Instant::now());
        let config = *config;
        let start = Job::Start {
            dir,
            kinds,
            at,
            config,
        };
        if tx.send(start).is_err() || QUEUE_TX.set(tx).is_err() {
            return; // started already
        }
        let spawned = std::thread::Builder::new()
            .name("bonsai-record".into())
            .spawn(move || Writer::default().run(rx));
        match spawned {
            Ok(_) => ACCEPTING.store(true, Relaxed),
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "bonsai: no run logs: {e}");
            }
        }
    }

    /// Stop taking lines, let the writer write what it took, then END (why
    /// the tree stopped, after how long) in every file, synced. Waits
    /// `END_WAIT` at most, without holding up the runtime's thread.
    pub async fn end(why: &str) {
        if !ACCEPTING.swap(false, Relaxed) {
            return;
        }
        let (done, finished) = oneshot::channel();
        if let Ok(mut ending) = ENDING.lock() {
            *ending = Some((why.to_string(), done));
        }
        if tokio::time::timeout(END_WAIT, finished).await.is_err() {
            let _ = writeln!(
                std::io::stderr(),
                "bonsai: run logs: the writer didn't finish within {END_WAIT:?}; END may be missing"
            );
        }
    }

    /// Keep a line from `source` in the file for `kind`, if it's being kept.
    /// Never waits: when the writer is behind, the line is dropped and counted.
    pub fn write(kind: Kind, source: &str, message: fmt::Arguments) {
        if !ACCEPTING.load(Relaxed) || CONFIG.get().is_none_or(|c| !kind.on(c)) {
            return;
        }
        let Some(tx) = QUEUE_TX.get() else { return };
        let mut text = line(&Local::now(), source, message);
        if text.len() > MAX_LINE {
            let mut cut = MAX_LINE;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push('…');
        }
        if tx.try_send(Job::Line(kind, text)).is_err() {
            DROPPED[kind as usize].fetch_add(1, Relaxed);
        }
    }

    /// The writer thread's files.
    #[derive(Default)]
    struct Writer {
        files: [Option<BufWriter<File>>; 4],
        /// Bytes in each file so far, for `max_file_kb`.
        sizes: [u64; 4],
        /// Drops already noted in each file.
        noted: [u64; 4],
        flushed: Option<Instant>,
        synced: Option<Instant>,
        dirty: bool,
        folder: Option<PathBuf>,
        max_bytes: u64,
        /// This run's lock, held until the writer ends.
        lock: Option<File>,
    }

    impl Writer {
        fn run(mut self, jobs: Receiver<Job>) {
            loop {
                match jobs.recv_timeout(FLUSH_EVERY) {
                    Ok(job) => self.take(job),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                let now = Instant::now();
                if self.flushed.is_none_or(|t| now - t >= FLUSH_EVERY) {
                    self.flush(false);
                }
                if self.dirty && self.synced.is_none_or(|t| now - t >= SYNC_EVERY) {
                    self.flush(true);
                }
                let ending = ENDING.lock().ok().and_then(|mut e| e.take());
                if let Some((why, done)) = ending {
                    // Nothing new is taken now: write what was, then END.
                    while let Ok(job) = jobs.try_recv() {
                        self.take(job);
                    }
                    self.end(&why);
                    let _ = done.send(());
                    return;
                }
            }
        }

        fn take(&mut self, job: Job) {
            match job {
                Job::Start {
                    dir,
                    kinds,
                    at,
                    config,
                } => self.start(&dir, &kinds, &at, &config),
                Job::Line(kind, text) => {
                    self.note_drops(kind);
                    self.put(kind, &text);
                }
            }
        }

        fn start(&mut self, dir: &Path, kinds: &[Kind], at: &Local, config: &Config) {
            // The run before this one, unless it's still going (another
            // tree sharing the folder).
            let unfinished = last_run(dir)
                .filter(|run| !running(run))
                .filter(|run| !finished(run));
            let folder = match new_folder(dir, &folder_name(at)) {
                Ok(folder) => folder,
                Err(e) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "bonsai: no run logs: can't make a folder in {}: {e}",
                        dir.display()
                    );
                    return;
                }
            };
            self.lock = lock(&folder);
            self.max_bytes = u64::from(config.max_file_kb) * 1024;
            let removed = prune(dir, &folder, config);
            let host = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
            let mut start = format!(
                "{} START {} {} on {} (pid {}, {})",
                stamp(at),
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
                host.trim(),
                std::process::id(),
                offset_text(at.offset)
            );
            if let Some(run) = unfinished {
                let name = run.file_name().unwrap_or_default().to_string_lossy();
                start += &format!(
                    "\n{} previous run {name} did not shut down cleanly (it has no END line)",
                    stamp(at)
                );
            }
            if removed > 0 {
                start += &format!(
                    "\n{} removed {removed} old run folder(s) (keep_runs {}, keep_days {})",
                    stamp(at),
                    config.keep_runs,
                    config.keep_days
                );
            }
            self.folder = Some(folder.clone());
            for &kind in kinds {
                let path = folder.join(kind.file());
                match OpenOptions::new().create(true).append(true).open(&path) {
                    Ok(file) => {
                        self.files[kind as usize] = Some(BufWriter::new(file));
                        self.sizes[kind as usize] = 0;
                        OPEN[kind as usize].store(true, Relaxed);
                        self.put(kind, &start);
                    }
                    Err(e) => {
                        let _ = writeln!(
                            std::io::stderr(),
                            "bonsai: run logs: can't open {}: {e}",
                            path.display()
                        );
                    }
                }
            }
            self.flush(true);
        }

        /// Note lines dropped since the last note, before the next one.
        fn note_drops(&mut self, kind: Kind) {
            let dropped = DROPPED[kind as usize].load(Relaxed);
            let new = dropped - self.noted[kind as usize];
            if new > 0 {
                self.noted[kind as usize] = dropped;
                let note = format!(
                    "{} ({new} lines dropped here: the recorder fell behind)",
                    stamp(&Local::now())
                );
                self.put(kind, &note);
            }
        }

        fn put(&mut self, kind: Kind, text: &str) {
            if self.max_bytes > 0 && self.sizes[kind as usize] >= self.max_bytes {
                self.rotate(kind);
            }
            let Some(file) = self.files[kind as usize].as_mut() else {
                return;
            };
            #[cfg(test)]
            {
                let ms = fault::SLOW_MS.load(Relaxed);
                if ms > 0 {
                    std::thread::sleep(Duration::from_millis(ms));
                }
            }
            let result = if cfg!(test) && fault_fail() {
                Err(std::io::Error::other("injected failure"))
            } else {
                file.write_all(text.as_bytes())
                    .and_then(|()| file.write_all(b"\n"))
            };
            match result {
                Ok(()) => {
                    self.dirty = true;
                    self.sizes[kind as usize] += text.len() as u64 + 1;
                }
                Err(e) => self.fail(kind, &e),
            }
        }

        /// Move a full file to `<kind>.1.log` (replacing the one there) and
        /// start a new one that says so.
        fn rotate(&mut self, kind: Kind) {
            let Some(folder) = self.folder.clone() else {
                return;
            };
            let Some(mut file) = self.files[kind as usize].take() else {
                return;
            };
            let _ = file.flush();
            drop(file);
            let path = folder.join(kind.file());
            let old = folder.join(kind.old_file());
            let reopened = std::fs::rename(&path, &old)
                .and_then(|()| OpenOptions::new().create(true).append(true).open(&path));
            match reopened {
                Ok(file) => {
                    self.files[kind as usize] = Some(BufWriter::new(file));
                    self.sizes[kind as usize] = 0;
                    let note = format!(
                        "{} (continued from {}: this file reached max_file_kb)",
                        stamp(&Local::now()),
                        kind.old_file()
                    );
                    self.put(kind, &note);
                }
                Err(e) => self.fail(kind, &e),
            }
        }

        /// Stop writing `kind`'s file, saying so once, on stderr (not through
        /// the logger, whose errors come back here).
        fn fail(&mut self, kind: Kind, e: &std::io::Error) {
            FAILED.fetch_add(1, Relaxed);
            self.files[kind as usize] = None;
            OPEN[kind as usize].store(false, Relaxed);
            let _ = writeln!(
                std::io::stderr(),
                "bonsai: run logs: can't write {}: {e}; stopped writing it",
                kind.file()
            );
        }

        /// Hand what's buffered to the OS; with `sync`, to the disk too.
        fn flush(&mut self, sync: bool) {
            for kind in KINDS {
                let Some(file) = self.files[kind as usize].as_mut() else {
                    continue;
                };
                let done = file.flush().and_then(|()| {
                    if sync {
                        file.get_ref().sync_data()
                    } else {
                        Ok(())
                    }
                });
                if let Err(e) = done {
                    self.fail(kind, &e);
                }
            }
            let now = Instant::now();
            self.flushed = Some(now);
            if sync {
                self.synced = Some(now);
                self.dirty = false;
            }
        }

        fn end(&mut self, why: &str) {
            let ran = STARTED.get().map(|s| s.elapsed().as_secs()).unwrap_or(0);
            for kind in KINDS {
                self.note_drops(kind);
                let dropped = DROPPED[kind as usize].load(Relaxed);
                let lost = if dropped > 0 {
                    format!(" ({dropped} lines dropped)")
                } else {
                    String::new()
                };
                let line = format!(
                    "{} END {why}, after {}{lost}",
                    stamp(&Local::now()),
                    duration_text(ran)
                );
                self.put(kind, &line);
            }
            self.flush(true);
            for kind in KINDS {
                self.files[kind as usize] = None;
                OPEN[kind as usize].store(false, Relaxed);
            }
            if let Some(folder) = &self.folder {
                let _ = std::fs::remove_file(folder.join(RUNNING));
            }
            self.lock = None;
        }
    }

    #[cfg(test)]
    fn fault_fail() -> bool {
        fault::FAIL.load(Relaxed)
    }

    #[cfg(not(test))]
    fn fault_fail() -> bool {
        false
    }

    /// `2026-09-30_14-00-05`, or `…-2` when that one's taken (two runs in a second).
    fn new_folder(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let mut n = 1;
        loop {
            let folder = match n {
                1 => dir.join(name),
                n => dir.join(format!("{name}-{n}")),
            };
            match std::fs::create_dir(&folder) {
                Ok(()) => return Ok(folder),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n += 1,
                Err(e) => return Err(e),
            }
        }
    }

    /// The run folder in `dir` made last (by the clock, not the name: names
    /// go back an hour when the clocks do).
    fn last_run(dir: &Path) -> Option<PathBuf> {
        std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && p.file_name().is_some_and(|n| is_run(&n.to_string_lossy())))
            .max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok())
    }

    /// Whether every file in a run's folder ends with its END line. Reads
    /// only each file's last `TAIL` bytes, however long it is.
    fn finished(run: &Path) -> bool {
        KINDS
            .iter()
            .map(|k| run.join(k.file()))
            .filter(|p| p.is_file())
            .all(|p| tail(&p).is_some_and(|text| has_end(&text)))
    }

    /// The last `TAIL` bytes of a file, as text.
    fn tail(path: &Path) -> Option<String> {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = File::open(path).ok()?;
        let len = file.metadata().ok()?.len();
        file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
        let mut bytes = Vec::with_capacity(TAIL as usize);
        file.take(TAIL).read_to_end(&mut bytes).ok()?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Hold this run's lock in its folder, for as long as the file is open.
    fn lock(folder: &Path) -> Option<File> {
        use std::os::fd::AsRawFd;
        let file = File::create(folder.join(RUNNING)).ok()?;
        // SAFETY: flock on a descriptor we own.
        let held = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        held.then_some(file)
    }

    /// Whether a run folder belongs to a tree still running: its lock is held.
    fn running(run: &Path) -> bool {
        use std::os::fd::AsRawFd;
        let Ok(file) = File::open(run.join(RUNNING)) else {
            return false;
        };
        // SAFETY: flock on a descriptor we own; closing it releases ours.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 }
    }

    /// Whether a folder holds only what a run writes (so it's safe to delete).
    fn only_run_files(run: &Path) -> bool {
        let known = |name: &str| {
            name == RUNNING
                || KINDS
                    .iter()
                    .any(|k| name == k.file() || name == k.old_file())
        };
        std::fs::read_dir(run).is_ok_and(|entries| {
            entries.flatten().all(|e| {
                e.file_type().is_ok_and(|t| t.is_file()) && known(&e.file_name().to_string_lossy())
            })
        })
    }

    /// Delete old run folders past `keep_runs` or `keep_days`, never this
    /// one, one still running, or one holding anything a run didn't write.
    /// Returns how many went.
    fn prune(dir: &Path, this: &Path, config: &Config) -> usize {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return 0;
        };
        let mut runs: Vec<(SystemTime, PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p != this && p.is_dir())
            .filter(|p| p.file_name().is_some_and(|n| is_run(&n.to_string_lossy())))
            .filter_map(|p| Some((p.metadata().and_then(|m| m.modified()).ok()?, p)))
            .collect();
        runs.sort(); // oldest first
        // Oldest first, delete what may be deleted until at most keep_runs
        // are left (this one included); what may not still counts.
        let keep = config.keep_runs.saturating_sub(1) as usize;
        let max_age = Duration::from_secs(u64::from(config.keep_days) * 86_400);
        let now = SystemTime::now();
        let mut left = runs.len();
        let mut removed = 0;
        for (when, run) in &runs {
            let too_many = config.keep_runs > 0 && left > keep;
            let too_old =
                config.keep_days > 0 && now.duration_since(*when).is_ok_and(|age| age > max_age);
            if (too_many || too_old)
                && !running(run)
                && only_run_files(run)
                && std::fs::remove_dir_all(run).is_ok()
            {
                removed += 1;
                left -= 1;
            }
        }
        removed
    }

    /// A run folder's name: `2026-09-30_14-00-05`, maybe with `-2` after it.
    fn is_run(name: &str) -> bool {
        let b = name.as_bytes();
        b.len() >= 19 && b[4] == b'-' && b[10] == b'_' && b[..4].iter().all(u8::is_ascii_digit)
    }

    /// Whether a run's file ended cleanly: its last whole line (one ending
    /// in a newline: a half-written last line doesn't count) is END.
    fn has_end(text: &str) -> bool {
        let Some(whole) = text.strip_suffix('\n') else {
            return false; // cut off mid-line
        };
        whole
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .is_some_and(|l| l.split(' ').nth(2) == Some("END"))
    }

    /// The local date and time, with the zone's offset from UTC.
    #[derive(Clone, Copy, Debug, PartialEq)]
    pub struct Local {
        pub year: i32,
        pub month: u32,
        pub day: u32,
        pub hour: u32,
        pub minute: u32,
        pub second: u32,
        pub milli: u32,
        /// Seconds east of UTC.
        pub offset: i64,
    }

    impl Local {
        /// Now, in the system's time zone (`TZ`, else /etc/localtime).
        pub fn now() -> Local {
            let since = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default();
            let secs = since.as_secs() as libc::time_t;
            // SAFETY: localtime_r only writes the `tm` it's given.
            let mut tm: libc::tm = unsafe { std::mem::zeroed() };
            let ok = !unsafe { libc::localtime_r(&secs, &mut tm) }.is_null();
            if !ok {
                tm.tm_year = 70;
                tm.tm_mday = 1;
            }
            Local {
                year: tm.tm_year + 1900,
                month: tm.tm_mon as u32 + 1,
                day: tm.tm_mday as u32,
                hour: tm.tm_hour as u32,
                minute: tm.tm_min as u32,
                second: tm.tm_sec as u32,
                milli: since.subsec_millis(),
                offset: tm.tm_gmtoff as i64,
            }
        }
    }

    /// `2026-09-30_14-00-05`.
    fn folder_name(t: &Local) -> String {
        format!(
            "{:04}-{:02}-{:02}_{:02}-{:02}-{:02}",
            t.year, t.month, t.day, t.hour, t.minute, t.second
        )
    }

    /// `2026-09-30 14:00:05.120`.
    fn stamp(t: &Local) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            t.year, t.month, t.day, t.hour, t.minute, t.second, t.milli
        )
    }

    /// `2026-09-30 14:00:09.004 watchdog: launch`.
    fn line(t: &Local, source: &str, message: fmt::Arguments) -> String {
        format!("{} {source}: {message}", stamp(t))
    }

    /// `UTC+02:00`, `UTC-05:30`, or `UTC`.
    fn offset_text(offset: i64) -> String {
        if offset == 0 {
            return "UTC".to_string();
        }
        let sign = if offset < 0 { '-' } else { '+' };
        let m = offset.unsigned_abs() / 60;
        format!("UTC{sign}{:02}:{:02}", m / 60, m % 60)
    }

    /// `5s`, `32m12s`, `2h03m`, `3d4h`.
    fn duration_text(secs: u64) -> String {
        let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
        match (d, h, m) {
            (0, 0, 0) => format!("{s}s"),
            (0, 0, _) => format!("{m}m{s:02}s"),
            (0, _, _) => format!("{h}h{m:02}m"),
            _ => format!("{d}d{h}h"),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const T: Local = Local {
            year: 2026,
            month: 9,
            day: 30,
            hour: 14,
            minute: 0,
            second: 5,
            milli: 120,
            offset: 7200,
        };

        #[test]
        fn names_and_lines_use_local_time() {
            assert_eq!(folder_name(&T), "2026-09-30_14-00-05");
            assert!(is_run("2026-09-30_14-00-05") && is_run("2026-09-30_14-00-05-2"));
            assert!(!is_run("notes") && !is_run("2026-09-30"));
            assert_eq!(
                line(&T, "watchdog", format_args!("launch at {}", 120)),
                "2026-09-30 14:00:05.120 watchdog: launch at 120"
            );
            assert_eq!(offset_text(7200), "UTC+02:00");
            assert_eq!(offset_text(-19_800), "UTC-05:30");
            assert_eq!(offset_text(0), "UTC");
        }

        #[test]
        fn durations_read_at_a_glance() {
            assert_eq!(duration_text(5), "5s");
            assert_eq!(duration_text(32 * 60 + 12), "32m12s");
            assert_eq!(duration_text(2 * 3600 + 3 * 60 + 9), "2h03m");
            assert_eq!(duration_text(3 * 86_400 + 4 * 3600), "3d4h");
        }

        #[test]
        fn a_run_ended_when_its_last_line_is_end() {
            let start = "2026-09-30 14:00:05.120 START greenhouse 0.1.0\n";
            assert!(!has_end(start));
            assert!(has_end(&format!(
                "{start}2026-09-30 14:32:17.551 END Ctrl-C, after 32m12s\n\n"
            )));
            // A record!("END …") from a branch isn't the tree's END.
            assert!(!has_end(&format!(
                "{start}2026-09-30 14:01:00.000 sensor: END\n"
            )));
            // An END cut off mid-line (a crash, a power cut) isn't one.
            assert!(!has_end(&format!(
                "{start}2026-09-30 14:32:17.551 END Ctrl-C, aft"
            )));
        }
    }
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

/// Numbers that carry their unit, so a temperature can't be added to a
/// distance, and meters can't be mistaken for feet. Each is an `f32` inside
/// (`.0` gets it out), prints with its symbol (`{:.1}` → `25.7 °C`), and
/// converts to the others of its kind with `.into()`.
pub mod units {
    use std::fmt;
    use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};
    use std::time::Duration;

    macro_rules! unit {
        ($name:ident, $symbol:literal, $what:literal) => {
            #[doc = $what]
            #[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
            pub struct $name(pub f32);

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                    match f.precision() {
                        Some(p) => write!(f, "{:.*}{}", p, self.0, $symbol),
                        None => write!(f, "{}{}", self.0, $symbol),
                    }
                }
            }

            impl Add for $name {
                type Output = $name;
                fn add(self, other: $name) -> $name {
                    $name(self.0 + other.0)
                }
            }

            impl Sub for $name {
                type Output = $name;
                fn sub(self, other: $name) -> $name {
                    $name(self.0 - other.0)
                }
            }

            impl AddAssign for $name {
                fn add_assign(&mut self, other: $name) {
                    self.0 += other.0;
                }
            }

            impl SubAssign for $name {
                fn sub_assign(&mut self, other: $name) {
                    self.0 -= other.0;
                }
            }

            impl Neg for $name {
                type Output = $name;
                fn neg(self) -> $name {
                    $name(-self.0)
                }
            }

            /// Scaled: `Meters(2.0) * 3.0`.
            impl Mul<f32> for $name {
                type Output = $name;
                fn mul(self, by: f32) -> $name {
                    $name(self.0 * by)
                }
            }

            impl Mul<$name> for f32 {
                type Output = $name;
                fn mul(self, value: $name) -> $name {
                    $name(self * value.0)
                }
            }

            impl Div<f32> for $name {
                type Output = $name;
                fn div(self, by: f32) -> $name {
                    $name(self.0 / by)
                }
            }

            /// A ratio: `Meters(6.0) / Meters(2.0)` is `3.0`.
            impl Div for $name {
                type Output = f32;
                fn div(self, other: $name) -> f32 {
                    self.0 / other.0
                }
            }
        };
    }

    /// `a` to `b` and back, for units of the same kind.
    macro_rules! convert {
        ($a:ident => $b:ident: |$x:ident| $to:expr, |$y:ident| $from:expr) => {
            impl From<$a> for $b {
                fn from($a($x): $a) -> $b {
                    $b($to)
                }
            }

            impl From<$b> for $a {
                fn from($b($y): $b) -> $a {
                    $a($from)
                }
            }
        };
    }

    /// `a × b = c` (and `b × a`, `c ÷ a = b`, `c ÷ b = a`).
    macro_rules! product {
        ($a:ident * $b:ident = $c:ident) => {
            impl Mul<$b> for $a {
                type Output = $c;
                fn mul(self, other: $b) -> $c {
                    $c(self.0 * other.0)
                }
            }

            impl Mul<$a> for $b {
                type Output = $c;
                fn mul(self, other: $a) -> $c {
                    $c(self.0 * other.0)
                }
            }

            impl Div<$a> for $c {
                type Output = $b;
                fn div(self, other: $a) -> $b {
                    $b(self.0 / other.0)
                }
            }

            impl Div<$b> for $c {
                type Output = $a;
                fn div(self, other: $b) -> $a {
                    $a(self.0 / other.0)
                }
            }
        };
    }

    unit!(Celsius, " °C", "A temperature in degrees Celsius.");
    unit!(Fahrenheit, " °F", "A temperature in degrees Fahrenheit.");
    unit!(Kelvin, " K", "A temperature in kelvins.");
    unit!(Meters, " m", "A length in meters.");
    unit!(Feet, " ft", "A length in feet.");
    unit!(Kilometers, " km", "A length in kilometers.");
    unit!(MetersPerSecond, " m/s", "A speed in meters per second.");
    unit!(Knots, " kn", "A speed in knots.");
    unit!(KilometersPerHour, " km/h", "A speed in km an hour.");
    unit!(Seconds, " s", "A time in seconds.");
    unit!(Hertz, " Hz", "A frequency: times a second.");
    unit!(Degrees, "°", "An angle in degrees.");
    unit!(Radians, " rad", "An angle in radians.");
    unit!(Volts, " V", "A voltage.");
    unit!(Amps, " A", "A current in amperes.");
    unit!(Watts, " W", "A power in watts.");
    unit!(Pascals, " Pa", "A pressure in pascals.");
    unit!(Hectopascals, " hPa", "A pressure in millibars.");
    unit!(Percent, "%", "A share of a whole, 0 to 100.");

    convert!(Celsius => Fahrenheit: |c| c * 9.0 / 5.0 + 32.0, |f| (f - 32.0) * 5.0 / 9.0);
    convert!(Celsius => Kelvin: |c| c + 273.15, |k| k - 273.15);
    convert!(Fahrenheit => Kelvin: |f| (f - 32.0) * 5.0 / 9.0 + 273.15, |k| (k - 273.15) * 9.0 / 5.0 + 32.0);
    convert!(Meters => Feet: |m| m / 0.3048, |ft| ft * 0.3048);
    convert!(Meters => Kilometers: |m| m / 1000.0, |km| km * 1000.0);
    convert!(Feet => Kilometers: |ft| ft * 0.0003048, |km| km / 0.0003048);
    convert!(MetersPerSecond => Knots: |v| v * 3600.0 / 1852.0, |kn| kn * 1852.0 / 3600.0);
    convert!(MetersPerSecond => KilometersPerHour: |v| v * 3.6, |kmh| kmh / 3.6);
    convert!(Knots => KilometersPerHour: |kn| kn * 1.852, |kmh| kmh / 1.852);
    convert!(Degrees => Radians: |d| d.to_radians(), |r| r.to_degrees());
    convert!(Pascals => Hectopascals: |pa| pa / 100.0, |hpa| hpa * 100.0);

    product!(MetersPerSecond * Seconds = Meters);
    product!(Volts * Amps = Watts);

    /// Times a second: `1.0 / Seconds(0.5)` is `Hertz(2.0)`.
    impl Div<Seconds> for f32 {
        type Output = Hertz;
        fn div(self, period: Seconds) -> Hertz {
            Hertz(self / period.0)
        }
    }

    /// How long each: `1.0 / Hertz(2.0)` is `Seconds(0.5)`.
    impl Div<Hertz> for f32 {
        type Output = Seconds;
        fn div(self, rate: Hertz) -> Seconds {
            Seconds(self / rate.0)
        }
    }

    impl From<Duration> for Seconds {
        fn from(d: Duration) -> Seconds {
            Seconds(d.as_secs_f32())
        }
    }

    /// Negative times become zero.
    impl From<Seconds> for Duration {
        fn from(s: Seconds) -> Duration {
            Duration::from_secs_f32(s.0.max(0.0))
        }
    }

    impl Radians {
        pub fn sin(self) -> f32 {
            self.0.sin()
        }
        pub fn cos(self) -> f32 {
            self.0.cos()
        }
    }

    impl Degrees {
        pub fn sin(self) -> f32 {
            Radians::from(self).sin()
        }
        pub fn cos(self) -> f32 {
            Radians::from(self).cos()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn close(a: f32, b: f32) -> bool {
            (a - b).abs() < 1e-3
        }

        #[test]
        fn a_kind_converts_both_ways() {
            let f: Fahrenheit = Celsius(100.0).into();
            assert_eq!(f, Fahrenheit(212.0));
            assert!(close(Celsius::from(Kelvin(0.0)).0, -273.15));
            assert!(close(Feet::from(Meters(1.0)).0, 3.28084));
            assert!(close(Knots::from(MetersPerSecond(1.0)).0, 1.94384));
            assert!(close(Radians::from(Degrees(180.0)).0, std::f32::consts::PI));
            assert!(close(Degrees(90.0).sin(), 1.0));
        }

        #[test]
        fn units_multiply_into_others() {
            assert_eq!(Meters(120.0) / Seconds(4.0), MetersPerSecond(30.0));
            assert_eq!(MetersPerSecond(3.0) * Seconds(2.0), Meters(6.0));
            assert_eq!(Volts(12.0) * Amps(2.0), Watts(24.0));
            assert_eq!(1.0 / Seconds(0.5), Hertz(2.0));
            assert_eq!(Meters(6.0) / Meters(2.0), 3.0);
            let mut t = Celsius(25.0);
            t += Celsius(1.5);
            assert!(t > Celsius(26.0));
        }

        #[test]
        fn units_print_with_their_symbol() {
            assert_eq!(format!("{:.1}", Celsius(25.66)), "25.7 °C");
            assert_eq!(format!("{}", Percent(55.0)), "55%");
            assert_eq!(format!("{:.0}", Degrees(12.4)), "12°");
            assert_eq!(Duration::from(Seconds(-1.0)), Duration::ZERO);
        }
    }
}

// ---------------------------------------------------------------------------
// Stats, and the server `bonsai top` reads them from
// ---------------------------------------------------------------------------

/// What each branch and edge has done since the tree started. Counting never
/// changes what a branch sends: it's only watched.
pub mod stats {
    use std::sync::atomic::Ordering::Relaxed;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};

    /// One branch's counts.
    #[derive(Default)]
    pub struct BranchStats {
        pub inputs: AtomicU64,
        pub sent: AtomicU64,
        pub panics: AtomicU64,
        /// Time spent in `process`, in all and at most once.
        pub busy_ns: AtomicU64,
        pub max_ns: AtomicU64,
        /// Out of service: its `setup` panicked.
        pub failed: AtomicBool,
        /// Inputs dropped while it was out of service.
        pub discarded: AtomicU64,
    }

    impl BranchStats {
        pub fn record(&self, took: Duration, sent: usize, panicked: bool) {
            let ns = took.as_nanos() as u64;
            self.inputs.fetch_add(1, Relaxed);
            self.sent.fetch_add(sent as u64, Relaxed);
            if panicked {
                // Its time is mostly the panic's own report: not the branch's.
                self.panics.fetch_add(1, Relaxed);
                return;
            }
            self.busy_ns.fetch_add(ns, Relaxed);
            self.max_ns.fetch_max(ns, Relaxed);
        }
    }

    pub const STARTING: u8 = 0;
    pub const UP: u8 = 1;
    pub const RETRYING: u8 = 2;

    /// An edge's counts. What branches send it goes through one queue:
    /// every message is `accepted` into it or `dropped` (the queue was full,
    /// or the edge had stopped); every accepted one is then `sent` (carried
    /// out) or `failed` (its `execute` returned an error or panicked), unless
    /// it's still waiting. `discarded` counts copies an edge took but could
    /// not deliver (a TCP server's slow or departed client).
    #[derive(Default)]
    pub struct EdgeStats {
        pub state: AtomicU8,
        pub received: AtomicU64,
        pub accepted: AtomicU64,
        pub sent: AtomicU64,
        pub dropped: AtomicU64,
        pub failed: AtomicU64,
        pub discarded: AtomicU64,
        pub restarts: AtomicU64,
        pub error: Mutex<String>,
    }

    impl EdgeStats {
        pub fn failed(&self, why: &str) {
            self.state.store(RETRYING, Relaxed);
            self.restarts.fetch_add(1, Relaxed);
            if let Ok(mut error) = self.error.lock() {
                *error = why.to_string();
            }
        }
    }

    /// The core's counts.
    pub struct CoreStats {
        pub events: AtomicU64,
        pub max_ns: AtomicU64,
        /// Events waiting when the last one was handled.
        pub inbox: AtomicU64,
    }

    impl CoreStats {
        pub fn record(&self, took: Duration, inbox: usize) {
            self.events.fetch_add(1, Relaxed);
            self.max_ns.fetch_max(took.as_nanos() as u64, Relaxed);
            self.inbox.store(inbox as u64, Relaxed);
        }
    }

    pub static CORE: CoreStats = CoreStats {
        events: AtomicU64::new(0),
        max_ns: AtomicU64::new(0),
        inbox: AtomicU64::new(0),
    };

    type Registry<T> = Mutex<Vec<(&'static str, Arc<T>)>>;
    static BRANCHES: Registry<BranchStats> = Mutex::new(Vec::new());
    static EDGES: Registry<EdgeStats> = Mutex::new(Vec::new());
    static STARTED: OnceLock<Instant> = OnceLock::new();

    fn find<T: Default>(registry: &Registry<T>, name: &'static str) -> Arc<T> {
        let mut all = registry.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, stats)) = all.iter().find(|(n, _)| *n == name) {
            return stats.clone();
        }
        let stats = Arc::new(T::default());
        all.push((name, stats.clone()));
        stats
    }

    /// A branch's counts (the same ones each time it's asked for by name).
    pub fn branch(name: &'static str) -> Arc<BranchStats> {
        find(&BRANCHES, name)
    }

    pub fn edge(name: &'static str) -> Arc<EdgeStats> {
        find(&EDGES, name)
    }

    /// The tree is running: uptime counts from now.
    pub fn start() {
        STARTED.get_or_init(Instant::now);
    }

    /// Every count at one moment.
    #[derive(Debug, Default, PartialEq)]
    pub struct Snapshot {
        pub uptime_ms: u64,
        pub events: u64,
        pub max_event_us: u64,
        pub inbox: u64,
        /// name, inputs, sent, panics, busy µs, max µs, failed (0/1), discarded
        pub branches: Vec<(String, [u64; 7])>,
        /// name, state, [received, sent, dropped, restarts, accepted, failed,
        /// discarded], last error
        pub edges: Vec<(String, &'static str, [u64; 7], String)>,
        /// from, message (empty for an edge's link), to, deliveries
        pub links: Vec<(&'static str, &'static str, &'static [&'static str], u64)>,
        pub sys: Option<Sys>,
    }

    /// A link in bonsai.toml: from, message (`""` when an edge is at one
    /// end), to. The generated core registers them all.
    pub type LinkInfo = (&'static str, &'static str, &'static [&'static str]);

    static LINKS: OnceLock<(&'static [LinkInfo], Box<[AtomicU64]>)> = OnceLock::new();

    /// The tree's links, in bonsai.toml order (the first call wins).
    pub fn links(list: &'static [LinkInfo]) {
        LINKS.get_or_init(|| (list, list.iter().map(|_| AtomicU64::new(0)).collect()));
    }

    /// One delivery down link `i`.
    pub fn link(i: usize) {
        if let Some(count) = LINKS.get().and_then(|(_, counts)| counts.get(i)) {
            count.fetch_add(1, Relaxed);
        }
    }

    /// The tree's process and its computer, from /proc.
    #[derive(Debug, Default, PartialEq)]
    pub struct Sys {
        pub rss_kb: u64,
        /// CPU time used, user and system.
        pub cpu_ms: u64,
        pub threads: u64,
        /// The one-minute load average, times 100.
        pub load: u64,
        pub mem_total_kb: u64,
        pub mem_available_kb: u64,
    }

    /// A `Key:   123 kB` line's number from /proc/*/status or /proc/meminfo.
    fn field(text: &str, key: &str) -> Option<u64> {
        let line = text.lines().find(|l| l.starts_with(key))?;
        line[key.len()..].split_whitespace().next()?.parse().ok()
    }

    /// From the texts of /proc/self/status, /proc/self/stat, /proc/loadavg
    /// and /proc/meminfo.
    pub fn parse_sys(status: &str, stat: &str, loadavg: &str, meminfo: &str) -> Sys {
        // stat's fields after the command (which may hold spaces) end at ")":
        // utime and stime are the 12th and 13th, in 1/100 s.
        let after = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
        let ticks: u64 = after
            .split_whitespace()
            .skip(11)
            .take(2)
            .filter_map(|t| t.parse::<u64>().ok())
            .sum();
        let load = loadavg
            .split_whitespace()
            .next()
            .and_then(|l| l.parse::<f64>().ok())
            .map_or(0, |l| (l * 100.0).round() as u64);
        Sys {
            rss_kb: field(status, "VmRSS:").unwrap_or(0),
            cpu_ms: ticks * 10,
            threads: field(status, "Threads:").unwrap_or(0),
            load,
            mem_total_kb: field(meminfo, "MemTotal:").unwrap_or(0),
            mem_available_kb: field(meminfo, "MemAvailable:").unwrap_or(0),
        }
    }

    fn sys() -> Option<Sys> {
        let read = |p: &str| std::fs::read_to_string(p).ok();
        Some(parse_sys(
            &read("/proc/self/status")?,
            &read("/proc/self/stat")?,
            &read("/proc/loadavg").unwrap_or_default(),
            &read("/proc/meminfo").unwrap_or_default(),
        ))
    }

    pub fn snapshot() -> Snapshot {
        let branches = BRANCHES.lock().unwrap_or_else(|e| e.into_inner());
        let edges = EDGES.lock().unwrap_or_else(|e| e.into_inner());
        Snapshot {
            uptime_ms: STARTED.get().map_or(0, |s| s.elapsed().as_millis() as u64),
            events: CORE.events.load(Relaxed),
            max_event_us: CORE.max_ns.load(Relaxed) / 1000,
            inbox: CORE.inbox.load(Relaxed),
            branches: branches
                .iter()
                .map(|(name, s)| {
                    let n = [
                        s.inputs.load(Relaxed),
                        s.sent.load(Relaxed),
                        s.panics.load(Relaxed),
                        s.busy_ns.load(Relaxed) / 1000,
                        s.max_ns.load(Relaxed) / 1000,
                        u64::from(s.failed.load(Relaxed)),
                        s.discarded.load(Relaxed),
                    ];
                    (name.to_string(), n)
                })
                .collect(),
            edges: edges
                .iter()
                .map(|(name, s)| {
                    let state = match s.state.load(Relaxed) {
                        UP => "up",
                        RETRYING => "retrying",
                        _ => "starting",
                    };
                    let n = [
                        s.received.load(Relaxed),
                        s.sent.load(Relaxed),
                        s.dropped.load(Relaxed),
                        s.restarts.load(Relaxed),
                        s.accepted.load(Relaxed),
                        s.failed.load(Relaxed),
                        s.discarded.load(Relaxed),
                    ];
                    let error = s.error.lock().map(|e| e.clone()).unwrap_or_default();
                    (name.to_string(), state, n, error)
                })
                .collect(),
            links: LINKS.get().map_or(Vec::new(), |(list, counts)| {
                list.iter()
                    .zip(counts.iter())
                    .map(|((from, label, to), n)| (*from, *label, *to, n.load(Relaxed)))
                    .collect()
            }),
            sys: sys(),
        }
    }

    /// A snapshot and new log lines as `bonsai top` reads them: a line per
    /// row, fields split by tabs, ending in `end`.
    pub fn render(s: &Snapshot, logs: &[String]) -> String {
        let clean = |t: &str| t.replace(['\t', '\n', '\r'], " ");
        let mut o = format!(
            "bonsai-top 1\t{}\t{}\t{}\t{}\n",
            s.uptime_ms, s.events, s.max_event_us, s.inbox
        );
        for (name, n) in &s.branches {
            o += &format!(
                "branch\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                clean(name),
                n[0],
                n[1],
                n[2],
                n[3],
                n[4],
                n[5],
                n[6]
            );
        }
        // New counts go after the error, so an older `bonsai top` (which
        // reads up to it) still understands the row.
        for (name, state, n, error) in &s.edges {
            o += &format!(
                "edge\t{}\t{state}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                clean(name),
                n[0],
                n[1],
                n[2],
                n[3],
                clean(error),
                n[4],
                n[5],
                n[6]
            );
        }
        for (from, label, to, n) in &s.links {
            o += &format!("link\t{from}\t{label}\t{}\t{n}\n", to.join(","));
        }
        if let Some(sys) = &s.sys {
            o += &format!(
                "sys\t{}\t{}\t{}\t{}\t{}\t{}\n",
                sys.rss_kb,
                sys.cpu_ms,
                sys.threads,
                sys.load,
                sys.mem_total_kb,
                sys.mem_available_kb
            );
        }
        for line in logs {
            o += &format!("log\t{}\n", clean(line));
        }
        o + "end\n"
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_snapshot_renders_as_tab_separated_rows() {
            let s = Snapshot {
                uptime_ms: 1500,
                events: 3,
                max_event_us: 40,
                inbox: 0,
                branches: vec![("sensor".into(), [3, 0, 0, 12, 5, 1, 4])],
                edges: vec![(
                    "net".into(),
                    "retrying",
                    [1, 2, 0, 1, 3, 1, 0],
                    "bind\tx".into(),
                )],
                links: vec![("sensor", "Reading", &["net", "log"], 3)],
                sys: Some(Sys {
                    rss_kb: 2048,
                    cpu_ms: 30,
                    threads: 1,
                    load: 12,
                    mem_total_kb: 4000,
                    mem_available_kb: 3000,
                }),
            };
            assert_eq!(
                render(&s, &["a line".into()]),
                "bonsai-top 1\t1500\t3\t40\t0\n\
                 branch\tsensor\t3\t0\t0\t12\t5\t1\t4\n\
                 edge\tnet\tretrying\t1\t2\t0\t1\tbind x\t3\t1\t0\n\
                 link\tsensor\tReading\tnet,log\t3\n\
                 sys\t2048\t30\t1\t12\t4000\t3000\n\
                 log\ta line\n\
                 end\n"
            );
        }

        #[test]
        fn proc_files_give_the_process_and_the_computer() {
            let status = "Name:\tgreenhouse\nVmRSS:\t    2048 kB\nThreads:\t2\n";
            let stat = "42 (green house) S 1 42 42 0 -1 4194560 100 0 0 0 7 3 0 0 20 0 2 0";
            let sys = parse_sys(
                status,
                stat,
                "0.12 0.10 0.05 1/100 42\n",
                "MemTotal: 4000 kB\nMemAvailable: 3000 kB\n",
            );
            assert_eq!(
                sys,
                Sys {
                    rss_kb: 2048,
                    cpu_ms: 100,
                    threads: 2,
                    load: 12,
                    mem_total_kb: 4000,
                    mem_available_kb: 3000
                }
            );
        }

        #[test]
        fn the_same_name_gets_the_same_counts() {
            let a = branch("stats_test_branch");
            a.record(Duration::from_micros(7), 2, false);
            let b = branch("stats_test_branch");
            assert_eq!(b.inputs.load(Relaxed), 1);
            assert_eq!(b.sent.load(Relaxed), 2);
        }
    }
}

/// The server `bonsai top` connects to: `BONSAI_TOP` is where it listens
/// (`127.0.0.1:7777` unless set; a port alone means on 127.0.0.1; `off`
/// turns it off). It listens on this computer only unless told otherwise:
/// from another one, `bonsai top` comes in over ssh.
pub mod top {
    use std::time::Duration;

    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    use super::{log, stats};

    pub const DEFAULT: &str = "127.0.0.1:7777";
    /// Log lines a new client gets from before it connected.
    const BACKLOG: u64 = 200;

    /// Where to listen, from `BONSAI_TOP`'s value; None: don't.
    pub fn address(setting: Option<&str>) -> Option<String> {
        match setting.map(str::trim) {
            None | Some("") => Some(DEFAULT.to_string()),
            Some("off") => None,
            Some(port) if port.parse::<u16>().is_ok() => Some(format!("127.0.0.1:{port}")),
            Some(addr) => Some(addr.to_string()),
        }
    }

    pub fn start() {
        let Some(addr) = address(std::env::var("BONSAI_TOP").ok().as_deref()) else {
            return;
        };
        tokio::spawn(async move {
            let listener = match TcpListener::bind(&addr).await {
                Ok(listener) => listener,
                Err(e) => {
                    warn!(
                        "top: can't listen on {addr} ({e}); set BONSAI_TOP to another port, or off"
                    );
                    return;
                }
            };
            debug!("top: listening on {addr}");
            loop {
                let Ok((mut client, _)) = listener.accept().await else {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                };
                tokio::spawn(async move {
                    let (_, now) = log::since(0);
                    let mut next = now.saturating_sub(BACKLOG);
                    let mut every = tokio::time::interval(Duration::from_millis(500));
                    loop {
                        every.tick().await;
                        let (lines, now) = log::since(next);
                        next = now;
                        let text = stats::render(&stats::snapshot(), &lines);
                        if client.write_all(text.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn bonsai_top_picks_the_address() {
            assert_eq!(address(None).as_deref(), Some(DEFAULT));
            assert_eq!(address(Some("off")), None);
            assert_eq!(address(Some("7000")).as_deref(), Some("127.0.0.1:7000"));
            assert_eq!(
                address(Some("0.0.0.0:7000")).as_deref(),
                Some("0.0.0.0:7000")
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Edges
// ---------------------------------------------------------------------------

/// An edge: the tree's bridge to something outside it (a socket, a port).
/// It's opened by an `async fn setup() -> io::Result<Self>` of its own
/// (built-in edges take their config); an `Err` there is retried with backoff.
pub trait Edge: Sized + Send + 'static {
    /// What it receives, handed to the branches linked from it.
    type In: Clone + Debug + Send + 'static;
    /// What branches send it to carry out.
    type Out: Clone + Debug + Send + 'static;

    /// The next thing it receives. Must be cancel-safe: it's dropped whenever
    /// something is sent out, so keep any partial data in `self`.
    fn recv(&mut self) -> impl Future<Output = io::Result<Self::In>> + Send;

    /// Execute: carry out one thing a branch sent. An `Err` restarts the edge.
    fn execute(&mut self, out: Self::Out) -> impl Future<Output = io::Result<()>> + Send;
}

/// Bytes to or from a built-in edge. `peer` is who sent it, and on the way
/// out who gets it: `None` sends to the edge's default (`to`, the last
/// sender, or every TCP client).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Packet {
    pub bytes: Vec<u8>,
    pub peer: Option<SocketAddr>,
}

impl Packet {
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Packet {
            bytes: bytes.into(),
            peer: None,
        }
    }

    /// A reply: `bytes` to whoever sent `self`.
    pub fn reply(&self, bytes: impl Into<Vec<u8>>) -> Self {
        Packet {
            bytes: bytes.into(),
            peer: self.peer,
        }
    }
}

/// How many messages an edge's queue holds: what branches send it waits
/// there while the edge is busy or restarting.
pub const EDGE_QUEUE: usize = 64;

/// Where the core puts what branches send to one edge. It never waits: a
/// message goes into the edge's queue (`accepted`), or, when the queue is
/// full (the edge is falling behind) or the edge has stopped, is dropped and
/// counted (`dropped`), with one warning each time dropping starts.
pub struct EdgeOut<T> {
    name: &'static str,
    stats: Arc<stats::EdgeStats>,
    tx: Option<mpsc::Sender<T>>,
    /// Sent before the edge started: what tests check.
    offline: Vec<T>,
    /// Dropped by this `EdgeOut`, all told.
    pub dropped: u64,
    /// Dropping now: warned already, until a message gets through again.
    dropping: bool,
}

impl<T> EdgeOut<T> {
    pub fn new(name: &'static str) -> Self {
        EdgeOut {
            name,
            stats: stats::edge(name),
            tx: None,
            offline: Vec::new(),
            dropped: 0,
            dropping: false,
        }
    }

    pub fn connect(&mut self, tx: mpsc::Sender<T>) {
        self.tx = Some(tx);
    }

    pub fn send(&mut self, value: T) {
        let Some(tx) = &self.tx else {
            self.offline.push(value);
            return;
        };
        let why = match tx.try_send(value) {
            Ok(()) => {
                self.stats.accepted.fetch_add(1, Relaxed);
                self.dropping = false;
                return;
            }
            Err(mpsc::error::TrySendError::Full(_)) => "isn't keeping up",
            Err(mpsc::error::TrySendError::Closed(_)) => "has stopped",
        };
        self.stats.dropped.fetch_add(1, Relaxed);
        self.dropped += 1;
        if !self.dropping {
            self.dropping = true;
            log::write(
                log::Level::Warn,
                Some(self.name),
                format_args!("{why}; dropping what's sent to it (counted in bonsai top)"),
            );
        }
    }

    /// Everything sent while the edge wasn't running (in tests: all of it).
    pub fn drain(&mut self) -> Vec<T> {
        std::mem::take(&mut self.offline)
    }
}

/// Start an edge in its own task, and restart it with backoff whenever it
/// fails or panics. What it receives goes to `events` as `wrap(value)`;
/// the returned sender takes what branches send it.
pub fn spawn_edge<E, I, F>(
    name: &'static str,
    setup: impl Fn() -> F + Send + 'static,
    events: mpsc::Sender<Event<I>>,
    wrap: fn(E::In) -> I,
) -> mpsc::Sender<E::Out>
where
    E: Edge,
    I: Send + 'static,
    F: Future<Output = io::Result<E>> + Send + 'static,
{
    // One queue for the edge's whole life: each attempt takes it over, so
    // what waits in it survives a restart, and nothing is lost in between.
    let (tx, outbound) = mpsc::channel::<E::Out>(EDGE_QUEUE);
    let outbound = Arc::new(tokio::sync::Mutex::new(outbound));
    let stats = stats::edge(name);
    // Every line an edge logs, from any of its tasks, is tagged with its name.
    tokio::spawn(log::EDGE.scope(name, async move {
        let mut backoff = Duration::from_millis(100);
        loop {
            let started = Instant::now();
            // Each attempt is its own task, so a panic ends only the attempt.
            let executing = Arc::new(AtomicBool::new(false));
            let attempt = tokio::spawn(log::EDGE.scope(
                name,
                attempt::<E, I, F>(
                    setup(),
                    outbound.clone(),
                    events.clone(),
                    wrap,
                    stats.clone(),
                    executing.clone(),
                ),
            ));
            let why = match attempt.await {
                Ok(Ok(())) => return, // the core is gone: shutting down
                Ok(Err(e)) => e.to_string(),
                Err(_) => {
                    // A panic in `execute` loses the message it was carrying.
                    if executing.load(Relaxed) {
                        stats.failed.fetch_add(1, Relaxed);
                    }
                    "panicked".to_string()
                }
            };
            if started.elapsed() > Duration::from_secs(30) {
                backoff = Duration::from_millis(100);
            }
            stats.failed(&why);
            warn!("{why}; retrying in {backoff:?}");
            record::write(
                record::Kind::Edges,
                name,
                format_args!("down: {why}; retrying in {backoff:?}"),
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    }));
    tx
}

async fn attempt<E, I, F>(
    setup: F,
    outbound: Arc<tokio::sync::Mutex<mpsc::Receiver<E::Out>>>,
    events: mpsc::Sender<Event<I>>,
    wrap: fn(E::In) -> I,
    stats: Arc<stats::EdgeStats>,
    executing: Arc<AtomicBool>,
) -> io::Result<()>
where
    E: Edge,
    F: Future<Output = io::Result<E>>,
{
    enum Step<In, Out> {
        In(io::Result<In>),
        Out(Out),
    }
    // Held until this attempt ends (a panic releases it too).
    let mut outbound = outbound.lock().await;
    let mut edge = setup.await?;
    stats.state.store(stats::UP, Relaxed);
    info!("up");
    record::write(record::Kind::Edges, log::source(), format_args!("up"));
    loop {
        let step = tokio::select! {
            got = edge.recv() => Step::In(got),
            Some(out) = outbound.recv() => Step::Out(out),
        };
        match step {
            Step::In(got) => {
                let got = got?;
                stats.received.fetch_add(1, Relaxed);
                if events.send(Event::Edge(wrap(got))).await.is_err() {
                    return Ok(());
                }
            }
            Step::Out(out) => {
                executing.store(true, Relaxed);
                let done = edge.execute(out).await;
                executing.store(false, Relaxed);
                match done {
                    Ok(()) => stats.sent.fetch_add(1, Relaxed),
                    Err(e) => {
                        stats.failed.fetch_add(1, Relaxed);
                        return Err(e);
                    }
                };
            }
        }
    }
}

fn invalid(what: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, what.to_string())
}

/// How a stream (TCP, serial) is cut into packets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Framing {
    /// Whatever each read returns: for protocols that frame themselves
    /// (a protocol with its own parser).
    Raw,
    /// One packet per line, without its `\n` (or `\r\n`); sends get a `\n`.
    Lines,
}

/// The longest line a stream edge takes (lines framing) unless its
/// `max_frame` says otherwise: 1 MiB. A longer one is refused as soon as it
/// passes the limit, before it's buffered.
pub const MAX_FRAME: usize = 1 << 20;

/// A byte stream cut into packets.
pub struct Framed<S> {
    io: S,
    framing: Framing,
    buf: Vec<u8>,
    /// Read but not yet handed out (lines framing).
    pending: Vec<u8>,
    max_frame: usize,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Framed<S> {
    pub fn new(io: S, framing: Framing) -> Self {
        Framed::with_limit(io, framing, MAX_FRAME)
    }

    /// Lines longer than `max_frame` bytes (without their newline) are
    /// refused with an `InvalidData` error.
    pub fn with_limit(io: S, framing: Framing, max_frame: usize) -> Self {
        Framed {
            io,
            framing,
            buf: vec![0; 4096],
            pending: Vec::new(),
            max_frame,
        }
    }

    /// Bytes read but not handed out yet.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// The next packet. Cancel-safe: nothing is kept between awaits but
    /// `pending`, which is updated only once a read has finished. With lines
    /// framing, a line longer than `max_frame` is an `InvalidData` error the
    /// moment it passes the limit: `pending` never holds more than the limit
    /// plus one read.
    pub async fn recv(&mut self) -> io::Result<Vec<u8>> {
        loop {
            if self.framing == Framing::Lines
                && let Some(end) = self.pending.iter().position(|&b| b == b'\n')
            {
                let mut line: Vec<u8> = self.pending.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            let n = self.io.read(&mut self.buf).await?;
            if n == 0 {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "closed"));
            }
            match self.framing {
                Framing::Raw => return Ok(self.buf[..n].to_vec()),
                Framing::Lines => {
                    // `pending` has no newline here: the line so far runs to
                    // the first newline in what was just read, if any.
                    let upto = self.buf[..n].iter().position(|&b| b == b'\n').unwrap_or(n);
                    if self.pending.len() + upto > self.max_frame {
                        self.pending.clear();
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("a line longer than {} bytes", self.max_frame),
                        ));
                    }
                    self.pending.extend_from_slice(&self.buf[..n]);
                }
            }
        }
    }

    pub async fn send(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.io.write_all(bytes).await?;
        if self.framing == Framing::Lines && bytes.last() != Some(&b'\n') {
            self.io.write_all(b"\n").await?;
        }
        Ok(())
    }
}

/// A UDP edge's settings, from its `[edge.<name>]` table.
#[derive(Clone, Copy, Debug)]
pub struct UdpConfig {
    pub bind: &'static str,
    /// Where sends go when a packet names no peer.
    pub to: Option<&'static str>,
    /// Multicast groups to join, on `iface`.
    pub join: &'static [&'static str],
    pub iface: &'static str,
    /// With no `to`, send back to whoever sent last.
    pub reply: bool,
}

pub struct Udp {
    socket: UdpSocket,
    to: Option<SocketAddr>,
    reply: bool,
    last: Option<SocketAddr>,
    buf: Vec<u8>,
}

impl Udp {
    pub async fn setup(cfg: UdpConfig) -> io::Result<Self> {
        let socket = UdpSocket::bind(cfg.bind)
            .await
            .map_err(|e| io::Error::new(e.kind(), format!("bind {}: {e}", cfg.bind)))?;
        let iface = cfg
            .iface
            .parse()
            .map_err(|_| invalid(format!("iface {:?} isn't an IPv4 address", cfg.iface)))?;
        for group in cfg.join {
            let addr = group
                .parse()
                .map_err(|_| invalid(format!("join {group:?} isn't an IPv4 address")))?;
            socket
                .join_multicast_v4(addr, iface)
                .map_err(|e| io::Error::new(e.kind(), format!("join {group}: {e}")))?;
        }
        let to = match cfg.to {
            Some(to) => Some(
                tokio::net::lookup_host(to)
                    .await?
                    .next()
                    .ok_or_else(|| invalid(format!("to {to:?}: no address")))?,
            ),
            None => None,
        };
        Ok(Udp {
            socket,
            to,
            reply: cfg.reply,
            last: None,
            buf: vec![0; 65_536],
        })
    }
}

impl Edge for Udp {
    type In = Packet;
    type Out = Packet;

    async fn recv(&mut self) -> io::Result<Packet> {
        let (n, from) = self.socket.recv_from(&mut self.buf).await?;
        self.last = Some(from);
        Ok(Packet {
            bytes: self.buf[..n].to_vec(),
            peer: Some(from),
        })
    }

    async fn execute(&mut self, out: Packet) -> io::Result<()> {
        let to = out.peer.or(self.to).or(self.last.filter(|_| self.reply));
        if let Some(to) = to {
            self.socket.send_to(&out.bytes, to).await?;
        }
        Ok(())
    }
}

/// A TCP server's most clients at once unless its `max_clients` says
/// otherwise. A connection past it is closed straight away.
pub const MAX_CLIENTS: usize = 64;

/// A TCP edge's settings: a client (`connect`) or a server (`listen`).
#[derive(Clone, Copy, Debug)]
pub struct TcpConfig {
    pub connect: Option<&'static str>,
    pub listen: Option<&'static str>,
    pub framing: Framing,
    /// The longest line, with lines framing.
    pub max_frame: usize,
    /// A server's most clients at once.
    pub max_clients: usize,
}

impl TcpConfig {
    /// Nothing to connect to, and the default limits.
    pub const DEFAULT: TcpConfig = TcpConfig {
        connect: None,
        listen: None,
        framing: Framing::Raw,
        max_frame: MAX_FRAME,
        max_clients: MAX_CLIENTS,
    };
}

pub enum Tcp {
    Client(Framed<TcpStream>, Option<SocketAddr>),
    Server(Server),
}

impl Tcp {
    pub async fn setup(cfg: TcpConfig) -> io::Result<Self> {
        match (cfg.connect, cfg.listen) {
            (Some(addr), _) => {
                let stream = TcpStream::connect(addr)
                    .await
                    .map_err(|e| io::Error::new(e.kind(), format!("connect {addr}: {e}")))?;
                let peer = stream.peer_addr().ok();
                let framed = Framed::with_limit(stream, cfg.framing, cfg.max_frame);
                Ok(Tcp::Client(framed, peer))
            }
            (None, Some(addr)) => {
                let listener = TcpListener::bind(addr)
                    .await
                    .map_err(|e| io::Error::new(e.kind(), format!("listen {addr}: {e}")))?;
                Ok(Tcp::Server(Server::new(listener, cfg)))
            }
            (None, None) => Err(invalid("a tcp edge needs `connect` or `listen`")),
        }
    }
}

/// How long a client that has stopped sending (shut down its side, or
/// sent a line past `max_frame`) keeps its connection for replies to what it
/// sent. Broadcasts skip it meanwhile; then it's closed.
pub const LINGER: Duration = Duration::from_secs(2);

/// What a TCP server holds for each client while its writer catches up:
/// past this, what's sent to that client is discarded (and counted).
pub const CLIENT_QUEUE: usize = 64;

/// How long one write to a TCP server's client may take. A client that
/// stops reading holds it up until then; then it's disconnected (what was
/// half-written is never re-sent on the same connection) and what's queued
/// for it is discarded.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// What a client's tasks tell its server.
enum FromClient {
    Packet(Packet),
    /// Its reader won't read from this client again.
    ReadEnded(SocketAddr),
    /// Its writer gave up: close it now.
    WriteFailed(SocketAddr),
}

/// One connected client: a reader task, and a writer task with its own
/// bounded queue, so no client waits on another.
struct Client {
    queue: mpsc::Sender<Arc<[u8]>>,
    reader: tokio::task::AbortHandle,
    writer: tokio::task::AbortHandle,
    /// When its reader ended: closed once `LINGER` has passed.
    read_ended: Option<Instant>,
}

/// A TCP server edge: every client's packets come in with its address; a
/// packet out goes to its `peer`, or to every client. It holds at most
/// `max_clients`, and closes a client when its reader ends (after `LINGER`)
/// or a write to it fails. Dropping it (the edge restarting) ends every
/// client's task and closes every connection.
pub struct Server {
    listener: TcpListener,
    framing: Framing,
    max_frame: usize,
    max_clients: usize,
    clients: HashMap<SocketAddr, Client>,
    /// Writers of closed clients, still sending what was queued for them.
    closing: Vec<tokio::task::AbortHandle>,
    from_clients: (mpsc::Sender<FromClient>, mpsc::Receiver<FromClient>),
    /// Turning connections away now: warned already.
    full: bool,
    stats: Arc<stats::EdgeStats>,
}

impl Server {
    fn new(listener: TcpListener, cfg: TcpConfig) -> Self {
        Server {
            listener,
            framing: cfg.framing,
            max_frame: cfg.max_frame,
            max_clients: cfg.max_clients,
            clients: HashMap::new(),
            closing: Vec::new(),
            from_clients: mpsc::channel(1024),
            full: false,
            stats: stats::edge(log::source()),
        }
    }

    async fn recv(&mut self) -> io::Result<Packet> {
        loop {
            let next_close = self
                .clients
                .values()
                .filter_map(|c| c.read_ended)
                .min()
                .map(|t| t + LINGER);
            tokio::select! {
                accepted = self.listener.accept() => {
                    let (stream, peer) = accepted?;
                    self.admit(stream, peer);
                }
                Some(got) = self.from_clients.1.recv() => match got {
                    FromClient::Packet(packet) => return Ok(packet),
                    FromClient::ReadEnded(peer) => {
                        if let Some(client) = self.clients.get_mut(&peer) {
                            client.read_ended = Some(Instant::now());
                        }
                    }
                    FromClient::WriteFailed(peer) => self.drop_client(peer),
                },
                _ = tokio::time::sleep_until(next_close.unwrap_or_else(Instant::now).into()),
                    if next_close.is_some() => self.close_lingering(),
            }
        }
    }

    /// Take a new client. At `max_clients`, the client that stopped sending
    /// longest ago makes room (its linger is cut short); when every client is
    /// still sending, the new one is turned away.
    fn admit(&mut self, stream: TcpStream, peer: SocketAddr) {
        if self.clients.len() >= self.max_clients
            && let Some(oldest) = self
                .clients
                .iter()
                .filter_map(|(p, c)| c.read_ended.map(|t| (t, *p)))
                .min()
                .map(|(_, p)| p)
        {
            self.close(oldest);
        }
        if self.clients.len() >= self.max_clients {
            drop(stream); // closed: the client sees its connection end
            if !self.full {
                self.full = true;
                warn!(
                    "{} clients already; turning new ones away (max_clients)",
                    self.max_clients
                );
            }
            return;
        }
        self.full = false;
        let (read, write) = stream.into_split();
        let to_server = self.from_clients.0.clone();
        let mut framed = Framed::with_limit(ReadOnly(read), self.framing, self.max_frame);
        let edge = log::source();
        let (queue, pending) = mpsc::channel(CLIENT_QUEUE);
        let writer = tokio::spawn(log::EDGE.scope(
            edge,
            write_to(peer, write, pending, to_server.clone(), self.stats.clone()),
        ));
        let reader = tokio::spawn(log::EDGE.scope(edge, async move {
            loop {
                match framed.recv().await {
                    Ok(bytes) => {
                        let packet = Packet {
                            bytes,
                            peer: Some(peer),
                        };
                        if to_server.send(FromClient::Packet(packet)).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        if e.kind() == io::ErrorKind::InvalidData {
                            warn!("{peer}: {e}; no longer reading from it");
                        }
                        let _ = to_server.send(FromClient::ReadEnded(peer)).await;
                        return;
                    }
                }
            }
        }));
        self.clients.insert(
            peer,
            Client {
                queue,
                reader: reader.abort_handle(),
                writer: writer.abort_handle(),
                read_ended: None,
            },
        );
    }

    /// Close a client gracefully: its reader stops, and its writer sends
    /// what's queued for it (each write under `WRITE_TIMEOUT`), then closes
    /// the connection.
    fn close(&mut self, peer: SocketAddr) {
        if let Some(client) = self.clients.remove(&peer) {
            client.reader.abort();
            self.closing.retain(|w| !w.is_finished());
            self.closing.push(client.writer);
            // Dropping `client.queue` tells the writer nothing more is coming.
        }
    }

    /// Close a client now: its writer has already given up.
    fn drop_client(&mut self, peer: SocketAddr) {
        if let Some(client) = self.clients.remove(&peer) {
            client.reader.abort();
            client.writer.abort();
        }
    }

    /// Close the clients whose `LINGER` is over.
    fn close_lingering(&mut self) {
        let now = Instant::now();
        let over: Vec<SocketAddr> = self
            .clients
            .iter()
            .filter(|(_, c)| c.read_ended.is_some_and(|t| now >= t + LINGER))
            .map(|(p, _)| *p)
            .collect();
        for peer in over {
            self.close(peer);
        }
    }

    /// Queue `out` for its peer, or every client, without waiting on any:
    /// a client whose queue is full (it isn't reading) misses this one, and
    /// the copy is counted as discarded.
    async fn execute(&mut self, out: Packet) -> io::Result<()> {
        self.close_lingering();
        let mut bytes = out.bytes;
        if self.framing == Framing::Lines && bytes.last() != Some(&b'\n') {
            bytes.push(b'\n');
        }
        let bytes: Arc<[u8]> = bytes.into();
        // A reply goes to its peer, even one that has stopped sending; a
        // broadcast only to clients still sending.
        let peers: Vec<SocketAddr> = match out.peer {
            Some(peer) => vec![peer],
            None => self
                .clients
                .iter()
                .filter(|(_, c)| c.read_ended.is_none())
                .map(|(p, _)| *p)
                .collect(),
        };
        for peer in peers {
            let Some(client) = self.clients.get(&peer) else {
                continue; // gone already: nothing to deliver to
            };
            if client.queue.try_send(bytes.clone()).is_err() {
                self.stats.discarded.fetch_add(1, Relaxed);
            }
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        for client in self.clients.values() {
            client.reader.abort();
            client.writer.abort();
        }
        for writer in &self.closing {
            writer.abort();
        }
    }
}

/// A server client's writer: what's queued for it, one write at a time,
/// each within `WRITE_TIMEOUT`. When one fails or times out it gives up:
/// the connection is closed (a half-written frame is never re-sent), what's
/// left is discarded and counted, and the server is told. When the server
/// closes the queue, it sends what's left and shuts the connection down.
async fn write_to(
    peer: SocketAddr,
    mut write: OwnedWriteHalf,
    mut queue: mpsc::Receiver<Arc<[u8]>>,
    to_server: mpsc::Sender<FromClient>,
    stats: Arc<stats::EdgeStats>,
) {
    while let Some(bytes) = queue.recv().await {
        let why = match tokio::time::timeout(WRITE_TIMEOUT, write.write_all(&bytes)).await {
            Ok(Ok(())) => continue,
            Ok(Err(e)) => e.to_string(),
            Err(_) => format!("not reading: a write took over {WRITE_TIMEOUT:?}"),
        };
        queue.close();
        let mut lost = 1; // the one being written
        while queue.try_recv().is_ok() {
            lost += 1;
        }
        stats.discarded.fetch_add(lost, Relaxed);
        warn!("{peer}: {why}; disconnecting it ({lost} discarded)");
        drop(write); // closed now: nothing more on this connection
        let _ = to_server.send(FromClient::WriteFailed(peer)).await;
        return;
    }
    let _ = tokio::time::timeout(WRITE_TIMEOUT, write.shutdown()).await;
}

/// The read half of a TCP stream, usable where `Framed` wants both halves
/// (it only ever reads it).
struct ReadOnly(tokio::net::tcp::OwnedReadHalf);

impl AsyncRead for ReadOnly {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for ReadOnly {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::task::Poll::Ready(Err(io::ErrorKind::Unsupported.into()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl Edge for Tcp {
    type In = Packet;
    type Out = Packet;

    async fn recv(&mut self) -> io::Result<Packet> {
        match self {
            Tcp::Client(stream, peer) => Ok(Packet {
                bytes: stream.recv().await?,
                peer: *peer,
            }),
            Tcp::Server(server) => server.recv().await,
        }
    }

    async fn execute(&mut self, out: Packet) -> io::Result<()> {
        match self {
            Tcp::Client(stream, _) => stream.send(&out.bytes).await,
            Tcp::Server(server) => server.execute(out).await,
        }
    }
}
