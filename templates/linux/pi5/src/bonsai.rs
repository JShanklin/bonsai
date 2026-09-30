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
#![allow(dead_code)]

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
        if catch_unwind(AssertUnwindSafe(|| branch.process(input, &mut out))).is_err() {
            eprintln!("bonsai: {} panicked; setting it up again", self.name);
            self.branch = B::setup();
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
            eprintln!(
                "bonsai: one event set off over {MAX_DELIVERIES} messages; dropping the {} left",
                queue.len() + 1
            );
            queue.clear();
            return;
        }
        deliver(message, queue);
    }
}

/// Run the tree until Ctrl-C or SIGTERM.
pub async fn run<T: Tree>(mut tree: T) {
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
                        eprintln!("bonsai: edge {} isn't keeping up; dropping what's sent to it", self.name);
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
    tokio::spawn(async move {
        let mut backoff = Duration::from_millis(100);
        loop {
            let started = Instant::now();
            // Each attempt is its own task, so a panic ends only the attempt.
            // What branches send is forwarded to whichever attempt is running.
            let (to_attempt, attempt_rx) = mpsc::channel::<E::Out>(64);
            let mut attempt = tokio::spawn(attempt::<E, I, F>(setup(), attempt_rx, events.clone(), wrap));
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
            eprintln!("bonsai: edge {name}: {why}; retrying in {backoff:?}");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(5));
        }
    });
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
