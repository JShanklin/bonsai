//! Lines framing: every frame is held to max_frame, however the stream
//! divides its bytes across reads.

use std::io::ErrorKind;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::time::timeout;

use crate::bonsai::{Framed, Framing, frame_len};

/// What `Framed` hands out, up to and including the first error.
type Got = Vec<Result<Vec<u8>, ErrorKind>>;

/// Feed `chunks` to a lines `Framed` with `max_frame`, each as its own read
/// (a pause between them), and collect frames until an error, or until
/// nothing more comes.
async fn frames(max_frame: usize, chunks: &[&[u8]]) -> Got {
    let (mut near, far) = tokio::io::duplex(1 << 16);
    let mut framed = Framed::with_limit(far, Framing::Lines, max_frame);
    let chunks: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
    let writer = tokio::spawn(async move {
        for chunk in chunks {
            near.write_all(&chunk).await.unwrap();
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        std::future::pending::<()>().await; // stay open: no EOF
    });
    let mut got = Vec::new();
    while let Ok(next) = timeout(Duration::from_millis(300), framed.recv()).await {
        match next {
            Ok(frame) => got.push(Ok(frame)),
            Err(e) => {
                got.push(Err(e.kind()));
                break;
            }
        }
    }
    writer.abort();
    got
}

fn ok(frames: &[&[u8]]) -> Got {
    frames.iter().map(|f| Ok(f.to_vec())).collect()
}

#[tokio::test]
async fn a_long_frame_behind_a_short_one_in_the_same_read_is_refused() {
    let got = frames(4, &[b"a\n123456\n"]).await;
    assert_eq!(got, [Ok(b"a".to_vec()), Err(ErrorKind::InvalidData)]);
}

#[tokio::test]
async fn frames_at_the_limit_pass_with_lf_or_crlf() {
    assert_eq!(frames(4, &[b"1234\n"]).await, ok(&[b"1234"]));
    assert_eq!(frames(4, &[b"1234\r\n"]).await, ok(&[b"1234"]));
    assert_eq!(
        frames(4, &[b"12345\n"]).await,
        [Err(ErrorKind::InvalidData)]
    );
    assert_eq!(
        frames(4, &[b"12345\r\n"]).await,
        [Err(ErrorKind::InvalidData)]
    );
    // A \r anywhere but just before the \n is payload.
    assert_eq!(
        frames(4, &[b"12\r34\n"]).await,
        [Err(ErrorKind::InvalidData)]
    );
    // A \r that arrives before its \n: not counted until the \n shows it's CRLF.
    assert_eq!(frames(4, &[b"1234\r", b"\n"]).await, ok(&[b"1234"]));
}

#[tokio::test]
async fn a_long_frame_in_pieces_is_refused_before_its_newline() {
    let got = frames(4, &[b"12", b"34", b"5"]).await;
    assert_eq!(got, [Err(ErrorKind::InvalidData)]);
}

#[tokio::test]
async fn several_frames_in_one_read_all_arrive_in_order() {
    let got = frames(4, &[b"a\nbb\r\nccc\ndddd\n"]).await;
    assert_eq!(got, ok(&[b"a", b"bb", b"ccc", b"dddd"]));
}

#[tokio::test]
async fn how_the_bytes_are_split_never_changes_what_passes() {
    for input in [
        &b"a\n123456\nbb\n"[..],
        b"ok\r\n1234\r\nabcd\n12345\n",
        b"1234\r\n1234\n12\r34\n",
        b"\n\r\nx\n",
    ] {
        let whole = frames(4, &[input]).await;
        for size in 1..input.len() {
            let chunks: Vec<&[u8]> = input.chunks(size).collect();
            let split = frames(4, &chunks).await;
            assert_eq!(
                split,
                whole,
                "{:?} in {size}-byte reads",
                String::from_utf8_lossy(input)
            );
        }
    }
}

#[test]
fn a_line_still_arriving_is_measured_without_a_last_cr() {
    assert_eq!(frame_len(b"1234"), 4);
    assert_eq!(frame_len(b"1234\r"), 4);
    assert_eq!(frame_len(b"12\r34"), 5);
    assert_eq!(frame_len(b""), 0);
}
