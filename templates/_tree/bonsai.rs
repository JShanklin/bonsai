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
use std::sync::Arc;
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
    /// has a `rate`. Generated as `crate::wiring::<branch>::Input`.
    type Input;
    /// Where it sends: `crate::wiring::<branch>::Out`.
    type Out: Default + Outbox;

    /// Setup: the branch's starting state. Runs again after `process` panics.
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
    stats: Arc<stats::BranchStats>,
}

impl<B: Branch> Slot<B> {
    pub fn new(name: &'static str) -> Self {
        Slot {
            name,
            branch: B::setup(),
            stats: stats::branch(name),
        }
    }

    /// Process one input; what it sent, or nothing if it panicked.
    pub fn process(&mut self, input: B::Input) -> B::Out {
        let mut out = B::Out::default();
        let branch = &mut self.branch;
        let outer = log::enter(self.name);
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| branch.process(input, &mut out)));
        let took = started.elapsed();
        if result.is_err() {
            self.branch = B::setup();
        }
        log::enter(outer);
        if result.is_err() {
            self.stats.record(took, 0, true);
            log::write(
                log::Level::Warn,
                Some(self.name),
                format_args!("set up again after a panic"),
            );
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
    loop {
        tokio::select! {
            Some(event) = inbox.recv() => {
                let started = Instant::now();
                tree.handle(event);
                stats::CORE.record(started.elapsed(), inbox.len());
            }
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
    use std::sync::atomic::{AtomicU8, AtomicU64};
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

    /// One edge's counts.
    #[derive(Default)]
    pub struct EdgeStats {
        pub state: AtomicU8,
        pub received: AtomicU64,
        pub sent: AtomicU64,
        pub dropped: AtomicU64,
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
        /// name, inputs, sent, panics, busy µs, max µs
        pub branches: Vec<(String, [u64; 5])>,
        /// name, state, received, sent, dropped, restarts, last error
        pub edges: Vec<(String, &'static str, [u64; 4], String)>,
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
                "branch\t{}\t{}\t{}\t{}\t{}\t{}\n",
                clean(name),
                n[0],
                n[1],
                n[2],
                n[3],
                n[4]
            );
        }
        for (name, state, n, error) in &s.edges {
            o += &format!(
                "edge\t{}\t{state}\t{}\t{}\t{}\t{}\t{}\n",
                clean(name),
                n[0],
                n[1],
                n[2],
                n[3],
                clean(error)
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
                branches: vec![("sensor".into(), [3, 0, 0, 12, 5])],
                edges: vec![("net".into(), "retrying", [1, 2, 0, 1], "bind\tx".into())],
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
                 branch\tsensor\t3\t0\t0\t12\t5\n\
                 edge\tnet\tretrying\t1\t2\t0\t1\tbind x\n\
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

/// Where the core puts what branches send to one edge. It never waits: when
/// the edge falls behind, what it can't take is dropped and counted.
pub struct EdgeOut<T> {
    name: &'static str,
    stats: Arc<stats::EdgeStats>,
    tx: Option<mpsc::Sender<T>>,
    /// Sent before the edge started: what tests check.
    offline: Vec<T>,
    pub dropped: u64,
}

impl<T> EdgeOut<T> {
    pub fn new(name: &'static str) -> Self {
        EdgeOut {
            name,
            stats: stats::edge(name),
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
                if tx.try_send(value).is_ok() {
                    self.stats.sent.fetch_add(1, Relaxed);
                } else {
                    self.stats.dropped.fetch_add(1, Relaxed);
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
    let stats = stats::edge(name);
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
                attempt::<E, I, F>(setup(), attempt_rx, events.clone(), wrap, stats.clone()),
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
            stats.failed(&why);
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
    stats: Arc<stats::EdgeStats>,
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
    stats.state.store(stats::UP, Relaxed);
    info!("up");
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
    /// (a protocol with its own parser).
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
