//! TCP and framing: bounded buffers and connections, clean-up, and slow
//! clients that can't hold up anyone else.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::support::{free_port, open_fds};
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
    let (saw_late_tx, mut saw_late) = mpsc::channel::<()>(1);
    tokio::spawn(async move {
        let mut seen = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match healthy.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(n) => seen.extend_from_slice(&buf[..n]),
            }
            if seen.windows(5).any(|w| w == b"late\n") {
                let _ = saw_late_tx.send(()).await;
                return;
            }
            if seen.len() > 1 << 20 {
                seen.drain(..seen.len() - 8);
            }
        }
    });
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
    assert!(
        timeout(Duration::from_secs(3), saw_late.recv())
            .await
            .is_ok(),
        "a stalled client held up the healthy one"
    );
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
