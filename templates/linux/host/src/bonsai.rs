//! bonsai's runtime: the core loop this tree runs on, and the edges that
//! connect it to the outside world. Written by `bonsai sync`, which rewrites
//! it; don't edit it.
//!
//! Every branch runs in one core loop, one event at a time. An event (a tick,
//! or something an edge received) goes to the branches wired to it, and every
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

use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::panic::{AssertUnwindSafe, catch_unwind};
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
    /// What it receives: one variant per wire into it, plus `Tick` when it
    /// has a `rate`. Generated as `crate::wiring::<branch>::Input`.
    type Input;
    /// Where it sends: `crate::wiring::<branch>::Out`.
    type Out: Default;

    /// Setup: the branch's starting state. Runs again after `process` panics.
    fn setup() -> Self;

    /// Process: decide what to do with one input, and `out.send(..)` the
    /// results. No I/O and no waiting here, so the same inputs always give
    /// the same outputs.
    fn process(&mut self, input: Self::Input, out: &mut Self::Out);
}

/// `out.send(message)`: there's one for each message a branch is wired to send.
pub trait Sends<M> {
    fn send(&mut self, message: M);
}

/// Something from outside the core.
#[derive(Clone, Debug, PartialEq)]
pub enum Event<E> {
    /// A branch's `rate` ticked: its index in the tree.
    Tick(usize),
    /// An edge received something (`crate::wiring::EdgeIn`).
    Edge(E),
}

/// The generated tree, `crate::wiring::Core`.
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

/// One branch in the core, set up again if its `process` panics.
pub struct Slot<B: Branch> {
    name: &'static str,
    branch: B,
}

impl<B: Branch> Slot<B> {
    pub fn new(name: &'static str) -> Self {
        Slot {
            name,
            branch: B::setup(),
        }
    }

    /// Process one input; what it sent, or nothing if it panicked.
    pub fn process(&mut self, input: B::Input) -> B::Out {
        let mut out = B::Out::default();
        let branch = &mut self.branch;
        let outer = log::enter(self.name);
        let result = catch_unwind(AssertUnwindSafe(|| branch.process(input, &mut out)));
        if result.is_err() {
            self.branch = B::setup();
        }
        log::enter(outer);
        if result.is_err() {
            log::write(log::Level::Warn, Some(self.name), format_args!("set up again after a panic"));
            return B::Out::default();
        }
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
    log::write(log::Level::Info, Some("bonsai"), format_args!("running"));
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
    loop {
        tokio::select! {
            Some(event) = inbox.recv() => tree.handle(event),
            _ = &mut shutdown => break,
        }
    }
    log::write(log::Level::Info, Some("bonsai"), format_args!("stopping"));
    drop(events);
}

/// Resolves on Ctrl-C, or on SIGTERM (systemd stopping the service).
async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .unwrap_or_else(|_| panic!("bonsai: can't listen for SIGTERM"));
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

// ---------------------------------------------------------------------------
// Logs
// ---------------------------------------------------------------------------

/// The logger behind `error!`, `warn!`, `info!` and `debug!`. Lines go to
/// stderr (journald keeps them for a service) as
/// `14:05:03.123Z  INFO pulse: beat`: UTC time, level, who wrote it.
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
            let mut filter = Filter { default: Level::Info, sources: Vec::new() };
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
    static RECENT: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

    /// The last `KEEP` lines logged, oldest first.
    pub fn recent() -> Vec<String> {
        RECENT.lock().map(|r| r.iter().cloned().collect()).unwrap_or_default()
    }

    /// Log one line, from `source` (or whoever is running, when `None`).
    pub fn write(level: Level, source: Option<&str>, message: fmt::Arguments) {
        let source = source.unwrap_or_else(|| self::source());
        let settings = settings();
        if !settings.filter.allows(level, source) {
            return;
        }
        let line = format!("{}{}", time(SystemTime::now()), line(level, source, message));
        let shown = if settings.color {
            let (time, rest) = line.split_at(13);
            let (label, rest) = rest.split_at(6);
            format!("\x1b[2m{time}\x1b[0m{}{label}\x1b[0m{rest}\n", level.color())
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
            if recent.len() == KEEP {
                recent.pop_front();
            }
            recent.push_back(line);
        }
    }

    /// Everything after the time: `  INFO pulse: beat`.
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
            match info.location() {
                Some(at) => error!("panicked at {}:{}: {message}", at.file(), at.line()),
                None => error!("panicked: {message}"),
            }
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
            let (f, bad) = Filter::parse("warn, gps=debug,tak=off");
            assert!(bad.is_empty());
            assert!(f.allows(Level::Error, "pulse"));
            assert!(!f.allows(Level::Info, "pulse"));
            assert!(f.allows(Level::Debug, "gps"));
            assert!(!f.allows(Level::Error, "tak"));
            let (f, bad) = Filter::parse("");
            assert!(bad.is_empty());
            assert!(f.allows(Level::Info, "pulse") && !f.allows(Level::Debug, "pulse"));
            let (f, bad) = Filter::parse("loud,gps=chatty");
            assert_eq!(bad, ["loud", "gps=chatty"]);
            assert!(f.allows(Level::Info, "gps"));
        }

        #[test]
        fn lines_carry_utc_time_level_and_source() {
            let t = UNIX_EPOCH + std::time::Duration::from_millis(86_400_000 * 3 + 50_703_123);
            assert_eq!(time(t), "14:05:03.123Z");
            assert_eq!(line(Level::Info, "pulse", format_args!("beat")), "  INFO pulse: beat");
            assert_eq!(line(Level::Error, "gps", format_args!("x")), " ERROR gps: x");
        }

        #[test]
        fn a_branch_being_processed_tags_its_lines() {
            assert_eq!(source(), "bonsai");
            let outer = enter("pulse");
            assert_eq!(source(), "pulse");
            enter(outer);
            assert_eq!(source(), "bonsai");
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
    /// What it receives, handed to the branches wired from it.
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

/// Where the core puts what branches send to one edge. It never waits: when
/// the edge falls behind, what it can't take is dropped and counted.
pub struct EdgeOut<T> {
    name: &'static str,
    tx: Option<mpsc::Sender<T>>,
    /// Sent before the edge started: what tests check.
    offline: Vec<T>,
    pub dropped: u64,
}

impl<T> EdgeOut<T> {
    pub fn new(name: &'static str) -> Self {
        EdgeOut {
            name,
            tx: None,
            offline: Vec::new(),
            dropped: 0,
        }
    }

    pub fn connect(&mut self, tx: mpsc::Sender<T>) {
        self.tx = Some(tx);
    }

    pub fn send(&mut self, value: T) {
        match &self.tx {
            Some(tx) => {
                if tx.try_send(value).is_err() {
                    if self.dropped == 0 {
                        log::write(
                            log::Level::Warn,
                            Some(self.name),
                            format_args!("isn't keeping up; dropping what's sent to it"),
                        );
                    }
                    self.dropped += 1;
                }
            }
            None => self.offline.push(value),
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
    let (tx, mut outbound) = mpsc::channel::<E::Out>(64);
    // Every line an edge logs, from any of its tasks, is tagged with its name.
    tokio::spawn(log::EDGE.scope(name, async move {
        let mut backoff = Duration::from_millis(100);
        loop {
            let started = Instant::now();
            // Each attempt is its own task, so a panic ends only the attempt.
            // What branches send is forwarded to whichever attempt is running.
            let (to_attempt, attempt_rx) = mpsc::channel::<E::Out>(64);
            let mut attempt = tokio::spawn(log::EDGE.scope(
                name,
                attempt::<E, I, F>(setup(), attempt_rx, events.clone(), wrap),
            ));
            let why = loop {
                tokio::select! {
                    done = &mut attempt => break match done {
                        Ok(Ok(())) => return, // the core is gone: shutting down
                        Ok(Err(e)) => e.to_string(),
                        Err(_) => "panicked".to_string(),
                    },
                    Some(out) = outbound.recv() => {
                        let _ = to_attempt.try_send(out);
                    }
                }
            };
            if started.elapsed() > Duration::from_secs(30) {
                backoff = Duration::from_millis(100);
            }
            warn!("{why}; retrying in {backoff:?}");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    }));
    tx
}

async fn attempt<E, I, F>(
    setup: F,
    mut outbound: mpsc::Receiver<E::Out>,
    events: mpsc::Sender<Event<I>>,
    wrap: fn(E::In) -> I,
) -> io::Result<()>
where
    E: Edge,
    F: Future<Output = io::Result<E>>,
{
    enum Step<In, Out> {
        In(io::Result<In>),
        Out(Out),
    }
    let mut edge = setup.await?;
    info!("up");
    loop {
        let step = tokio::select! {
            got = edge.recv() => Step::In(got),
            Some(out) = outbound.recv() => Step::Out(out),
        };
        match step {
            Step::In(got) => {
                if events.send(Event::Edge(wrap(got?))).await.is_err() {
                    return Ok(());
                }
            }
            Step::Out(out) => edge.execute(out).await?,
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
    /// (MAVLink, anything with its own parser).
    Raw,
    /// One packet per line, without its `\n` (or `\r\n`); sends get a `\n`.
    Lines,
}

/// A byte stream cut into packets.
pub struct Framed<S> {
    io: S,
    framing: Framing,
    buf: Vec<u8>,
    /// Read but not yet handed out (lines framing).
    pending: Vec<u8>,
}

impl<S: AsyncRead + AsyncWrite + Unpin + Send> Framed<S> {
    pub fn new(io: S, framing: Framing) -> Self {
        Framed {
            io,
            framing,
            buf: vec![0; 4096],
            pending: Vec::new(),
        }
    }

    /// The next packet. Cancel-safe: nothing is kept between awaits but
    /// `pending`, which is updated only once a read has finished.
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
                Framing::Lines => self.pending.extend_from_slice(&self.buf[..n]),
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

/// A TCP edge's settings: a client (`connect`) or a server (`listen`).
#[derive(Clone, Copy, Debug)]
pub struct TcpConfig {
    pub connect: Option<&'static str>,
    pub listen: Option<&'static str>,
    pub framing: Framing,
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
                Ok(Tcp::Client(Framed::new(stream, cfg.framing), peer))
            }
            (None, Some(addr)) => {
                let listener = TcpListener::bind(addr)
                    .await
                    .map_err(|e| io::Error::new(e.kind(), format!("listen {addr}: {e}")))?;
                let (tx, rx) = mpsc::channel(1024);
                Ok(Tcp::Server(Server {
                    listener,
                    framing: cfg.framing,
                    clients: HashMap::new(),
                    from_clients: (tx, rx),
                }))
            }
            (None, None) => Err(invalid("a tcp edge needs `connect` or `listen`")),
        }
    }
}

/// A TCP server edge: every client's packets come in with its address; a
/// packet out goes to its `peer`, or to every client.
pub struct Server {
    listener: TcpListener,
    framing: Framing,
    clients: HashMap<SocketAddr, OwnedWriteHalf>,
    from_clients: (mpsc::Sender<Packet>, mpsc::Receiver<Packet>),
}

impl Server {
    async fn recv(&mut self) -> io::Result<Packet> {
        loop {
            tokio::select! {
                accepted = self.listener.accept() => {
                    let (stream, peer) = accepted?;
                    let (read, write) = stream.into_split();
                    self.clients.insert(peer, write);
                    let to_edge = self.from_clients.0.clone();
                    let mut framed = Framed::new(ReadOnly(read), self.framing);
                    tokio::spawn(async move {
                        while let Ok(bytes) = framed.recv().await {
                            let packet = Packet { bytes, peer: Some(peer) };
                            if to_edge.send(packet).await.is_err() {
                                return;
                            }
                        }
                    });
                }
                Some(packet) = self.from_clients.1.recv() => return Ok(packet),
            }
        }
    }

    async fn execute(&mut self, out: Packet) -> io::Result<()> {
        let mut bytes = out.bytes;
        if self.framing == Framing::Lines && bytes.last() != Some(&b'\n') {
            bytes.push(b'\n');
        }
        let peers: Vec<SocketAddr> = match out.peer {
            Some(peer) => vec![peer],
            None => self.clients.keys().copied().collect(),
        };
        for peer in peers {
            let gone = match self.clients.get_mut(&peer) {
                Some(client) => client.write_all(&bytes).await.is_err(),
                None => false,
            };
            if gone {
                self.clients.remove(&peer);
            }
        }
        Ok(())
    }
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
