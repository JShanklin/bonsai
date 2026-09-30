//! TCP and framing: bounded buffers and connections, clean-up, and slow
//! clients that can't hold up anyone else.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::support::{fds_to_myself, free_port, open_fds};
use crate::bonsai::{EdgeOut, Event, Framed, Framing, Packet, Tcp, TcpConfig, spawn_edge};

/// A TCP server edge on a free port: its address, where what it receives
/// arrives, and where to send.
fn server(
    name: &'static str,
    framing: Framing,
) -> (SocketAddr, mpsc::Receiver<Event<Packet>>, EdgeOut<Packet>) {
    let addr: &'static str = Box::leak(format!("127.0.0.1:{}", free_port()).into_boxed_str());
    let cfg = TcpConfig {
        connect: None,
        listen: Some(addr),
        framing,
        ..TcpConfig::DEFAULT
    };
    let (events, inbox) = mpsc::channel(1024);
    let tx = spawn_edge::<Tcp, Packet, _>(name, move || Tcp::setup(cfg), events, |p| p);
    let mut out = EdgeOut::new(name);
    out.connect(tx);
    (addr.parse().unwrap(), inbox, out)
}

async fn connect(addr: SocketAddr) -> TcpStream {
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect(addr).await {
            return s;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("can't connect to {addr}");
}

async fn next(inbox: &mut mpsc::Receiver<Event<Packet>>) -> Packet {
    match timeout(Duration::from_secs(3), inbox.recv()).await {
        Ok(Some(Event::Edge(p))) => p,
        other => panic!("expected a packet, got {other:?}"),
    }
}

/// Read `stream` until `marker` arrives; the receiver gets `()` then. It
/// gets nothing, and closes, if the stream ends or fails first.
fn watch_for(
    mut stream: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    marker: &'static [u8],
) -> mpsc::Receiver<()> {
    let (saw, seen_it) = mpsc::channel(1);
    tokio::spawn(async move {
        let mut seen = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return, // ended first: `saw` drops unsent
                Ok(n) => seen.extend_from_slice(&buf[..n]),
            }
            if seen.windows(marker.len()).any(|w| w == marker) {
                let _ = saw.send(()).await;
                return;
            }
            if seen.len() > 1 << 20 {
                seen.drain(..seen.len() - marker.len());
            }
        }
    });
    seen_it
}

/// Whether `watch_for` saw its marker within `within`, and if not, why.
async fn wait_for(seen_it: &mut mpsc::Receiver<()>, within: Duration) -> Result<(), &'static str> {
    match timeout(within, seen_it.recv()).await {
        Ok(Some(())) => Ok(()),
        Ok(None) => Err("its connection ended before the marker arrived"),
        Err(_) => Err("timed out waiting for the marker"),
    }
}

#[tokio::test]
async fn waiting_for_a_marker_fails_unless_it_arrives() {
    // Delivered.
    let (mut near, far) = tokio::io::duplex(64);
    let mut seen = watch_for(far, b"late\n");
    near.write_all(b"x\nlate\n").await.unwrap();
    assert_eq!(wait_for(&mut seen, Duration::from_secs(3)).await, Ok(()));
    // The client disconnects before the marker: a failure, not a pass.
    let (near, far) = tokio::io::duplex(64);
    let mut seen = watch_for(far, b"late\n");
    drop(near);
    assert_eq!(
        wait_for(&mut seen, Duration::from_secs(3)).await,
        Err("its connection ended before the marker arrived")
    );
    // Nothing comes at all: a timeout, told apart from the above.
    let (_near, far) = tokio::io::duplex(64);
    let mut seen = watch_for(far, b"late\n");
    assert_eq!(
        wait_for(&mut seen, Duration::from_millis(200)).await,
        Err("timed out waiting for the marker")
    );
}

#[tokio::test]
async fn an_endless_line_is_refused_before_it_fills_memory() {
    let (mut near, far) = tokio::io::duplex(64 * 1024);
    let mut framed = Framed::new(far, Framing::Lines);
    let writer = tokio::spawn(async move {
        let chunk = vec![b'x'; 64 * 1024];
        // 16 MiB and no newline: far past any sane line.
        for _ in 0..256 {
            if near.write_all(&chunk).await.is_err() {
                return;
            }
        }
        std::future::pending::<()>().await;
    });
    let got = timeout(Duration::from_secs(5), framed.recv()).await;
    writer.abort();
    match got {
        Ok(Err(e)) => assert_eq!(e.kind(), std::io::ErrorKind::InvalidData, "{e}"),
        Ok(Ok(line)) => panic!("got a {}-byte line", line.len()),
        Err(_) => panic!("still buffering after 5 s: the line was never refused"),
    }
    assert!(
        framed.pending_len() <= crate::bonsai::MAX_FRAME,
        "{}",
        framed.pending_len()
    );
}

#[tokio::test]
async fn lines_still_arrive_whole_and_in_order() {
    let (mut near, far) = tokio::io::duplex(1024);
    let mut framed = Framed::new(far, Framing::Lines);
    near.write_all(b"one\r\ntwo\nthr").await.unwrap();
    near.write_all(b"ee\n").await.unwrap();
    assert_eq!(framed.recv().await.unwrap(), b"one");
    assert_eq!(framed.recv().await.unwrap(), b"two");
    assert_eq!(framed.recv().await.unwrap(), b"three");
}

#[tokio::test]
async fn clients_that_come_and_go_leave_nothing_behind() {
    let _turn = fds_to_myself().await;
    let (addr, mut inbox, _out) = server("rt_churn", Framing::Lines);
    // One client first, so the edge is up and the baseline counts it.
    let mut first = connect(addr).await;
    first.write_all(b"hello\n").await.unwrap();
    next(&mut inbox).await;
    let before = open_fds();
    for i in 0..200 {
        let mut c = connect(addr).await;
        c.write_all(format!("{i}\n").as_bytes()).await.unwrap();
        next(&mut inbox).await;
        drop(c);
    }
    // A client that stopped sending keeps its connection for LINGER, for
    // replies; after that, nothing of it is left.
    tokio::time::sleep(crate::bonsai::LINGER + Duration::from_millis(500)).await;
    let after = open_fds();
    assert!(
        after <= before + 5,
        "open descriptors went from {before} to {after}"
    );
}

#[tokio::test]
async fn connections_past_the_limit_are_turned_away() {
    let (addr, mut inbox, _out) = server("rt_crowd", Framing::Lines);
    let limit = TcpConfig::DEFAULT.max_clients;
    let mut kept = Vec::new();
    for i in 0..limit {
        let mut c = connect(addr).await;
        c.write_all(format!("{i}\n").as_bytes()).await.unwrap();
        next(&mut inbox).await;
        kept.push(c);
    }
    // One more: closed straight away.
    let mut extra = connect(addr).await;
    let mut buf = [0u8; 16];
    let read = timeout(Duration::from_secs(3), extra.read(&mut buf)).await;
    assert!(
        matches!(read, Ok(Ok(0)) | Ok(Err(_))),
        "the connection past the limit stayed open: {read:?}"
    );
    // The ones inside the limit still work.
    kept[0].write_all(b"still here\n").await.unwrap();
    assert_eq!(next(&mut inbox).await.bytes, b"still here");
}

#[tokio::test]
async fn a_client_that_stops_sending_still_gets_its_reply() {
    let (addr, mut inbox, mut out) = server("rt_halfclose", Framing::Lines);
    let mut c = connect(addr).await;
    c.write_all(b"question\n").await.unwrap();
    c.shutdown().await.unwrap(); // done sending; still reading
    let asked = next(&mut inbox).await;
    assert_eq!(asked.bytes, b"question");
    // The core answers a moment later.
    tokio::time::sleep(Duration::from_millis(200)).await;
    out.send(asked.reply(b"answer".to_vec()));
    let mut reply = Vec::new();
    timeout(Duration::from_secs(3), c.read_to_end(&mut reply))
        .await
        .expect("the reply came, and then the connection closed")
        .unwrap();
    assert_eq!(reply, b"answer\n");
}

#[tokio::test]
async fn a_client_that_stops_reading_holds_up_no_one() {
    let (addr, mut inbox, mut out) = server("rt_slow", Framing::Lines);
    // A client that never reads.
    let mut slow = connect(addr).await;
    slow.write_all(b"slow\n").await.unwrap();
    next(&mut inbox).await;
    // A healthy client that reads everything, looking for "late".
    let mut healthy = connect(addr).await;
    healthy.write_all(b"healthy\n").await.unwrap();
    next(&mut inbox).await;
    let mut saw_late = watch_for(healthy, b"late\n");
    // Far more than the slow client's socket buffers hold.
    let big = vec![b'x'; 64 * 1024];
    for _ in 0..400 {
        out.send(Packet::new(big.clone()));
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    // The edge still takes in what clients send...
    let mut third = connect(addr).await;
    third.write_all(b"third\n").await.unwrap();
    let got = timeout(Duration::from_secs(3), async {
        loop {
            if let Some(Event::Edge(p)) = inbox.recv().await
                && p.bytes == b"third"
            {
                return;
            }
        }
    })
    .await;
    assert!(
        got.is_ok(),
        "a stalled client held up what the edge receives"
    );
    // ...and the healthy client still gets what's sent.
    out.send(Packet::new(b"late".to_vec()));
    if let Err(why) = wait_for(&mut saw_late, Duration::from_secs(3)).await {
        panic!("the healthy client never got \"late\": {why}");
    }
    // The slow client: once a write to it has waited WRITE_TIMEOUT, it's
    // disconnected and what couldn't reach it is counted.
    tokio::time::sleep(crate::bonsai::WRITE_TIMEOUT + Duration::from_secs(1)).await;
    let s = crate::bonsai::stats::edge("rt_slow");
    let discarded = s.discarded.load(std::sync::atomic::Ordering::Relaxed);
    assert!(discarded > 0, "nothing counted as discarded");
    let mut buf = vec![0u8; 1 << 20];
    let closed = timeout(Duration::from_secs(10), async {
        loop {
            match slow.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {} // what reached its socket before the cut
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the stalled client was never disconnected");
}

#[tokio::test]
async fn dropping_a_server_closes_every_client_and_ends_its_tasks() {
    use crate::bonsai::Edge;
    let addr: &'static str = Box::leak(format!("127.0.0.1:{}", free_port()).into_boxed_str());
    let cfg = TcpConfig {
        listen: Some(addr),
        framing: Framing::Lines,
        ..TcpConfig::DEFAULT
    };
    let mut edge = Tcp::setup(cfg).await.unwrap();
    let (got_tx, mut got) = mpsc::channel(8);
    // The edge's own task, as spawn_edge runs it; aborting it drops the edge,
    // as a restart does.
    let task = tokio::spawn(async move {
        while let Ok(p) = edge.recv().await {
            let _ = got_tx.send(p).await;
        }
    });
    let mut a = connect(addr.parse().unwrap()).await;
    let mut b = connect(addr.parse().unwrap()).await;
    a.write_all(b"a\n").await.unwrap();
    b.write_all(b"b\n").await.unwrap();
    for _ in 0..2 {
        timeout(Duration::from_secs(3), got.recv())
            .await
            .unwrap()
            .unwrap();
    }
    task.abort();
    for c in [&mut a, &mut b] {
        let mut buf = [0u8; 8];
        let read = timeout(Duration::from_secs(3), c.read(&mut buf)).await;
        assert!(
            matches!(read, Ok(Ok(0)) | Ok(Err(_))),
            "a client of a dropped server stayed connected: {read:?}"
        );
    }
}

#[tokio::test]
async fn a_client_sending_an_endless_line_is_cut_off_alone() {
    let addr: &'static str = Box::leak(format!("127.0.0.1:{}", free_port()).into_boxed_str());
    let cfg = TcpConfig {
        listen: Some(addr),
        framing: Framing::Lines,
        max_frame: 1024,
        ..TcpConfig::DEFAULT
    };
    let (events, mut inbox) = mpsc::channel(64);
    let tx = spawn_edge::<Tcp, Packet, _>("rt_endless", move || Tcp::setup(cfg), events, |p| p);
    let mut out = EdgeOut::new("rt_endless");
    out.connect(tx);
    let addr: SocketAddr = addr.parse().unwrap();
    let mut good = connect(addr).await;
    good.write_all(b"good\n").await.unwrap();
    assert_eq!(next(&mut inbox).await.bytes, b"good");
    let mut bad = connect(addr).await;
    bad.write_all(&[b'x'; 4096]).await.unwrap();
    // The bad one is closed once its linger is over...
    let mut buf = [0u8; 8];
    let read = timeout(
        crate::bonsai::LINGER + Duration::from_secs(3),
        bad.read(&mut buf),
    )
    .await;
    assert!(matches!(read, Ok(Ok(0)) | Ok(Err(_))), "{read:?}");
    // ...and the good one is untouched.
    good.write_all(b"still good\n").await.unwrap();
    assert_eq!(next(&mut inbox).await.bytes, b"still good");
}

/// What the churn test asks of the server's task.
enum Ask {
    Send(Packet),
    Connections(tokio::sync::oneshot::Sender<(usize, usize)>),
}

/// Clients that ask for a lot, stop sending and read slowly, one after
/// another: they can't take more than `max_clients` connections, each is
/// gone by `LINGER` + `DRAIN_TIMEOUT` at most, every copy that never
/// reached one is counted, and a healthy client is served throughout.
#[tokio::test]
async fn clients_that_ask_and_read_slowly_stay_within_the_limit() {
    use crate::bonsai::{DRAIN_TIMEOUT, Edge, LINGER};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    const NAME: &str = "rt_drain";
    const MAX: usize = 4;
    const CHURN: usize = 16;
    const COPIES: usize = 48; // within CLIENT_QUEUE: all of them are queued
    const SIZE: usize = 128 * 1024; // each copy, its newline included
    let _turn = fds_to_myself().await; // it opens many sockets

    let addr: &'static str = Box::leak(format!("127.0.0.1:{}", free_port()).into_boxed_str());
    let cfg = TcpConfig {
        listen: Some(addr),
        framing: Framing::Lines,
        max_clients: MAX,
        ..TcpConfig::DEFAULT
    };
    let addr: SocketAddr = addr.parse().unwrap();
    let (asks, mut asked) = mpsc::channel::<Ask>(64);
    let (got_tx, mut inbox) = mpsc::channel::<Packet>(1024);
    // The edge's own task, as spawn_edge runs it, plus a way to ask how
    // many connections it holds.
    tokio::spawn(crate::bonsai::log::EDGE.scope(NAME, async move {
        let mut edge = Tcp::setup(cfg).await.unwrap();
        loop {
            tokio::select! {
                got = edge.recv() => {
                    let _ = got_tx.send(got.unwrap()).await;
                }
                Some(ask) = asked.recv() => match ask {
                    Ask::Send(p) => edge.execute(p).await.unwrap(),
                    Ask::Connections(tell) => {
                        let _ = tell.send(edge.connections());
                    }
                },
            }
        }
    }));
    async fn connections(asks: &mpsc::Sender<Ask>) -> (usize, usize) {
        let (tell, told) = tokio::sync::oneshot::channel();
        let _ = asks.send(Ask::Connections(tell)).await;
        told.await.unwrap()
    }
    async fn expect(inbox: &mut mpsc::Receiver<Packet>, bytes: &[u8]) -> Packet {
        loop {
            match timeout(Duration::from_secs(3), inbox.recv()).await {
                Ok(Some(p)) if p.bytes == bytes => return p,
                Ok(Some(_)) => {}
                other => panic!(
                    "expected {:?}, got {other:?}",
                    String::from_utf8_lossy(bytes)
                ),
            }
        }
    }
    let tasks = || {
        tokio::runtime::Handle::current()
            .metrics()
            .num_alive_tasks()
    };

    let mut healthy = connect(addr).await;
    healthy.write_all(b"hello\n").await.unwrap();
    expect(&mut inbox, b"hello").await;
    let baseline = tasks(); // the healthy client's two tasks included
    let slow = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let readers_alive = Arc::new(AtomicUsize::new(0));
    let mut readers = Vec::new();
    for i in 0..CHURN {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        let mut c = socket.connect(addr).await.unwrap();
        let ask = format!("ask {i}");
        c.write_all(format!("{ask}\n").as_bytes()).await.unwrap();
        c.shutdown().await.unwrap(); // done sending; still reading
        let asked = expect(&mut inbox, ask.as_bytes()).await;
        for _ in 0..COPIES {
            let copy = asked.reply(vec![b'x'; SIZE - 1]);
            asks.send(Ask::Send(copy)).await.unwrap();
        }
        // Read slowly (a few KiB at a time), then, once told, to the end.
        let (slow, alive) = (slow.clone(), readers_alive.clone());
        alive.fetch_add(1, Relaxed);
        readers.push(tokio::spawn(async move {
            let mut buf = vec![0u8; 1 << 16];
            let mut total = 0;
            loop {
                let want = if slow.load(Relaxed) { 4096 } else { buf.len() };
                match c.read(&mut buf[..want]).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => total += n,
                }
                if slow.load(Relaxed) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            alive.fetch_sub(1, Relaxed);
            total
        }));
        // The healthy client is still heard...
        let ping = format!("ping {i}");
        healthy
            .write_all(format!("{ping}\n").as_bytes())
            .await
            .unwrap();
        expect(&mut inbox, ping.as_bytes()).await;
        // ...and the server never holds more than MAX connections, nor more
        // than two tasks for each.
        let (clients, draining) = connections(&asks).await;
        assert!(
            clients + draining <= MAX,
            "{clients} clients and {draining} draining"
        );
        let server_tasks = tasks() - readers_alive.load(Relaxed);
        assert!(
            server_tasks <= baseline + 2 * (MAX - 1),
            "{server_tasks} tasks, {baseline} to start with"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    // Every slow client is let go by LINGER + DRAIN_TIMEOUT, however slowly
    // it's still reading: only the healthy one is left.
    tokio::time::sleep(LINGER + DRAIN_TIMEOUT + Duration::from_secs(1)).await;
    assert_eq!(connections(&asks).await, (1, 0));
    assert!(
        tasks() - readers_alive.load(Relaxed) <= baseline,
        "{} tasks, {baseline} to start with",
        tasks() - readers_alive.load(Relaxed)
    );
    // The healthy client still gets a broadcast.
    let mut saw_late = watch_for(healthy, b"late\n");
    asks.send(Ask::Send(Packet::new(b"late".to_vec())))
        .await
        .unwrap();
    if let Err(why) = wait_for(&mut saw_late, Duration::from_secs(3)).await {
        panic!("the healthy client never got \"late\": {why}");
    }
    // Each copy either reached its client whole or was counted as discarded,
    // once.
    slow.store(false, Relaxed);
    let mut delivered = 0;
    for reader in readers {
        let total = timeout(Duration::from_secs(20), reader)
            .await
            .expect("a slow client's connection never ended")
            .unwrap();
        delivered += total / SIZE;
    }
    let discarded = crate::bonsai::stats::edge(NAME).discarded.load(Relaxed) as usize;
    assert!(discarded > 0, "the slow clients were never cut off");
    assert_eq!(
        discarded + delivered,
        CHURN * COPIES,
        "{discarded} discarded, {delivered} delivered"
    );
}
