//! Loopback tests for the HTTP/1.1 client: exact bytes, framing, decoding,
//! streaming, keep-alive reuse and drop safety.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use super::{Body, Client, ClientOptions, Target, TransportError};

pub(super) fn client() -> Client {
    Client::new(ClientOptions::default()).unwrap()
}

pub(super) async fn listener() -> (TcpListener, Target) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = Target {
        host: "127.0.0.1".into(),
        port: l.local_addr().unwrap().port(),
        tls: false,
    };
    (l, target)
}

pub(super) fn head(body_len: usize) -> Vec<u8> {
    format!("POST /v1/messages HTTP/1.1\r\nHost: x\r\nContent-Length: {body_len}\r\n\r\n")
        .into_bytes()
}

/// Reads one full request (head plus Content-Length body); `None` on a clean close.
pub(super) async fn read_request<S>(s: &mut S) -> Option<Vec<u8>>
where
    S: AsyncRead + Unpin,
{
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let text = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
            let len = text
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .map_or(0, |v| v.trim().parse::<usize>().unwrap());
            if buf.len() >= end + 4 + len {
                return Some(buf);
            }
        }
        let n = s.read(&mut tmp).await.unwrap_or(0);
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

pub(super) async fn collect(body: &mut Body) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(b) = body.chunk().await.unwrap() {
        out.extend_from_slice(&b);
    }
    out
}

pub(super) fn chunked(data: &[u8]) -> Vec<u8> {
    [format!("{:x}\r\n", data.len()).as_bytes(), data, b"\r\n"].concat()
}

/// Serves one connection: reads a request, writes `response`, then closes.
pub(super) async fn serve_once(response: Vec<u8>) -> Target {
    let (l, t) = listener().await;
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        read_request(&mut s).await;
        let _ = s.write_all(&response).await;
    });
    t
}

pub(super) fn with_encoding(encoding: &str, payload: &[u8]) -> Vec<u8> {
    [
        format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: {encoding}\r\nContent-Length: {}\r\n\r\n",
            payload.len()
        )
        .as_bytes(),
        payload,
    ]
    .concat()
}

pub(super) fn big() -> Vec<u8> {
    b"abcdefghij".repeat(20_000)
}

pub(super) async fn fetch(t: &Target) -> Result<super::Response, TransportError> {
    client().send(t, &head(0), b"").await
}

#[tokio::test]
async fn sends_head_and_body_exactly_and_reads_content_length() {
    let (l, t) = listener().await;
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        tx.send(read_request(&mut s).await.unwrap()).unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nX-Test: Yes\r\nContent-Length: 5\r\n\r\nhello")
            .await
            .unwrap();
    });
    let body = br#"{"a":1}"#;
    let h = head(body.len());
    let mut r = client().send(&t, &h, body).await.unwrap();
    assert_eq!(rx.await.unwrap(), [h.as_slice(), body].concat());
    assert_eq!(r.status, 200);
    assert_eq!(r.header("x-TEST"), Some("Yes"));
    assert_eq!(r.headers[0].0, "X-Test");
    assert_eq!(collect(&mut r.body).await, b"hello");
}

#[tokio::test]
async fn chunked_events_arrive_before_the_stream_ends() {
    let (l, t) = listener().await;
    let (go_tx, go_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        read_request(&mut s).await;
        let first = [
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".as_slice(),
            &chunked(b"data: 1\n\n"),
        ]
        .concat();
        s.write_all(&first).await.unwrap();
        go_rx.await.unwrap();
        s.write_all(&[chunked(b"data: 2\n\n"), b"0\r\n\r\n".to_vec()].concat())
            .await
            .unwrap();
    });
    let mut r = fetch(&t).await.unwrap();
    assert_eq!(r.body.chunk().await.unwrap().unwrap(), "data: 1\n\n");
    go_tx.send(()).unwrap();
    assert_eq!(r.body.chunk().await.unwrap().unwrap(), "data: 2\n\n");
    assert!(r.body.chunk().await.unwrap().is_none());
}

#[tokio::test]
async fn chunk_extensions_and_trailers_are_skipped() {
    let t = serve_once(
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n0\r\nTrailer: x\r\n\r\n"
            .to_vec(),
    )
    .await;
    let mut r = fetch(&t).await.unwrap();
    assert_eq!(collect(&mut r.body).await, b"hello");
}

#[tokio::test]
async fn gzip_deflate_zstd_and_brotli_bodies_decode() {
    let data = big();
    let gz = {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&data).unwrap();
        e.finish().unwrap()
    };
    let zl = {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&data).unwrap();
        e.finish().unwrap()
    };
    // `brotli -q 5` of `abcdefghij` x 20000.
    let br: [u8; 22] = [
        0x53, 0x3f, 0x0d, 0x03, 0x40, 0x54, 0x38, 0x06, 0xc4, 0xcb, 0x47, 0x1c, 0x18, 0x0a, 0xf0,
        0x04, 0x03, 0xa0, 0x9c, 0xce, 0xde, 0x03,
    ];
    let cases = [
        ("gzip", gz),
        ("deflate", zl),
        ("zstd", zstd::encode_all(&data[..], 3).unwrap()),
        ("br", br.to_vec()),
    ];
    for (name, payload) in cases {
        let t = serve_once(with_encoding(name, &payload)).await;
        let mut r = fetch(&t).await.unwrap();
        let mut got = Vec::new();
        loop {
            match r.body.chunk().await {
                Ok(Some(b)) => got.extend_from_slice(&b),
                Ok(None) => break,
                Err(e) => panic!("{name}: {e:?} after {} bytes", got.len()),
            }
        }
        assert!(got == data, "{name} body differs ({} bytes)", got.len());
    }
}

#[tokio::test]
async fn gzip_with_every_header_field_split_into_tiny_chunks() {
    let data = big();
    let mut e = flate2::GzBuilder::new()
        .filename("a.txt")
        .comment("note")
        .extra(vec![1, 2, 3])
        .write(Vec::new(), flate2::Compression::default());
    e.write_all(&data).unwrap();
    let gz = e.finish().unwrap();
    let mut resp =
        b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    for piece in gz.chunks(3) {
        resp.extend(chunked(piece));
    }
    resp.extend(b"0\r\n\r\n");
    let t = serve_once(resp).await;
    let mut r = fetch(&t).await.unwrap();
    assert!(collect(&mut r.body).await == data);
}

#[tokio::test]
async fn corrupt_and_truncated_gzip_are_errors() {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(&big()).unwrap();
    let mut gz = e.finish().unwrap();
    let truncated = gz[..gz.len() - 4].to_vec();
    let n = gz.len();
    gz[n - 8] ^= 0xff;
    for payload in [gz, truncated] {
        let t = serve_once(with_encoding("gzip", &payload)).await;
        let mut r = fetch(&t).await.unwrap();
        let outcome = loop {
            match r.body.chunk().await {
                Ok(Some(_)) => {}
                other => break other,
            }
        };
        assert!(outcome.is_err(), "damaged gzip decoded cleanly");
    }
}

#[tokio::test]
async fn unknown_content_encoding_is_rejected() {
    let t = serve_once(with_encoding("compress", b"x")).await;
    assert!(matches!(
        fetch(&t).await,
        Err(TransportError::UnsupportedEncoding)
    ));
}

#[tokio::test]
async fn body_without_length_reads_to_close() {
    let t =
        serve_once(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nstream to the end".to_vec()).await;
    let mut r = fetch(&t).await.unwrap();
    assert_eq!(collect(&mut r.body).await, b"stream to the end");
}

#[tokio::test]
async fn short_content_length_body_is_truncated() {
    let t = serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc".to_vec()).await;
    let mut r = fetch(&t).await.unwrap();
    assert_eq!(r.body.chunk().await.unwrap().unwrap(), "abc");
    assert!(matches!(
        r.body.chunk().await,
        Err(TransportError::Truncated)
    ));
}

#[tokio::test]
async fn conflicting_content_lengths_are_rejected() {
    let t =
        serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nab".to_vec())
            .await;
    assert!(matches!(fetch(&t).await, Err(TransportError::BadFraming)));
}

async fn counting_server(accepts: Arc<AtomicUsize>, close_each: bool) -> Target {
    let (l, t) = listener().await;
    tokio::spawn(async move {
        loop {
            let (mut s, _) = l.accept().await.unwrap();
            accepts.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                while read_request(&mut s).await.is_some() {
                    let conn = if close_each {
                        "Connection: close\r\n"
                    } else {
                        ""
                    };
                    let resp = format!("HTTP/1.1 200 OK\r\n{conn}Content-Length: 2\r\n\r\nok");
                    if s.write_all(resp.as_bytes()).await.is_err() || close_each {
                        break;
                    }
                }
            });
        }
    });
    t
}

#[tokio::test]
async fn keep_alive_connection_is_reused() {
    let accepts = Arc::new(AtomicUsize::new(0));
    let t = counting_server(accepts.clone(), false).await;
    let c = client();
    for _ in 0..3 {
        let mut r = c.send(&t, &head(1), b"x").await.unwrap();
        assert_eq!(collect(&mut r.body).await, b"ok");
    }
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn connection_close_is_not_reused() {
    let accepts = Arc::new(AtomicUsize::new(0));
    let t = counting_server(accepts.clone(), true).await;
    let c = client();
    for _ in 0..2 {
        let mut r = c.send(&t, &head(0), b"").await.unwrap();
        assert_eq!(collect(&mut r.body).await, b"ok");
    }
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn request_connection_close_is_not_pooled() {
    let accepts = Arc::new(AtomicUsize::new(0));
    let t = counting_server(accepts.clone(), false).await;
    let c = client();
    let h = b"POST / HTTP/1.1\r\nConnection: Close\r\nContent-Length: 0\r\n\r\n";
    for _ in 0..2 {
        let mut r = c.send(&t, h, b"").await.unwrap();
        collect(&mut r.body).await;
    }
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn pool_respects_max_idle_per_host() {
    let accepts = Arc::new(AtomicUsize::new(0));
    let t = counting_server(accepts.clone(), false).await;
    let c = Client::new(ClientOptions {
        max_idle_per_host: 0,
        ..ClientOptions::default()
    })
    .unwrap();
    for _ in 0..2 {
        let mut r = c.send(&t, &head(0), b"").await.unwrap();
        collect(&mut r.body).await;
    }
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn stale_pooled_connection_is_retried_once() {
    let (l, t) = listener().await;
    let accepts = Arc::new(AtomicUsize::new(0));
    let (closed_tx, closed_rx) = oneshot::channel();
    let n = accepts.clone();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        n.fetch_add(1, Ordering::SeqCst);
        read_request(&mut s).await.unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\none")
            .await
            .unwrap();
        drop(s);
        closed_tx.send(()).unwrap();
        let (mut s, _) = l.accept().await.unwrap();
        n.fetch_add(1, Ordering::SeqCst);
        read_request(&mut s).await.unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\ntwo")
            .await
            .unwrap();
    });
    let c = client();
    let mut r = c.send(&t, &head(0), b"").await.unwrap();
    assert_eq!(collect(&mut r.body).await, b"one");
    closed_rx.await.unwrap();
    let mut r = c.send(&t, &head(0), b"").await.unwrap();
    assert_eq!(collect(&mut r.body).await, b"two");
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn oversized_head_is_rejected() {
    let long = format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "a".repeat(70_000));
    let t = serve_once(long.into_bytes()).await;
    assert!(matches!(fetch(&t).await, Err(TransportError::HeadTooLarge)));
}

#[tokio::test]
async fn too_many_headers_are_rejected() {
    let mut resp = String::from("HTTP/1.1 200 OK\r\n");
    for i in 0..129 {
        resp.push_str(&format!("H{i}: v\r\n"));
    }
    resp.push_str("\r\n");
    let t = serve_once(resp.into_bytes()).await;
    assert!(matches!(fetch(&t).await, Err(TransportError::HeadTooLarge)));
}

#[tokio::test]
async fn malformed_chunk_sizes_are_rejected() {
    for bad in [
        "zz\r\nhello\r\n",
        "fffffffffffffffff\r\n",
        "5x\r\nhello\r\n",
        "\r\n",
    ] {
        let resp = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{bad}");
        let t = serve_once(resp.into_bytes()).await;
        let mut r = fetch(&t).await.unwrap();
        assert!(
            matches!(r.body.chunk().await, Err(TransportError::MalformedChunk)),
            "{bad:?}"
        );
    }
}

#[tokio::test]
async fn missing_chunk_terminator_is_rejected() {
    let t =
        serve_once(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhelloXX".to_vec())
            .await;
    let mut r = fetch(&t).await.unwrap();
    assert_eq!(r.body.chunk().await.unwrap().unwrap(), "hello");
    assert!(matches!(
        r.body.chunk().await,
        Err(TransportError::MalformedChunk)
    ));
}

#[tokio::test]
async fn dropping_a_response_mid_body_discards_its_connection() {
    let (l, t) = listener().await;
    let accepts = Arc::new(AtomicUsize::new(0));
    let n = accepts.clone();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        n.fetch_add(1, Ordering::SeqCst);
        read_request(&mut s).await.unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n0123456789")
            .await
            .unwrap();
        let mut sink = [0u8; 16];
        let _ = s.read(&mut sink).await;
        let (mut s, _) = l.accept().await.unwrap();
        n.fetch_add(1, Ordering::SeqCst);
        read_request(&mut s).await.unwrap();
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
    });
    let c = client();
    let mut r = c.send(&t, &head(0), b"").await.unwrap();
    assert_eq!(r.body.chunk().await.unwrap().unwrap(), "0123456789");
    drop(r);
    let mut r = c.send(&t, &head(0), b"").await.unwrap();
    assert_eq!(collect(&mut r.body).await, b"ok");
    assert_eq!(accepts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn interim_100_continue_is_skipped() {
    let t = serve_once(
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
    )
    .await;
    let mut r = fetch(&t).await.unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(collect(&mut r.body).await, b"ok");
}

#[tokio::test]
async fn connection_refused_is_a_connect_error() {
    let (l, t) = listener().await;
    drop(l);
    assert!(matches!(fetch(&t).await, Err(TransportError::Connect(_))));
}

fn gzip_of(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

#[tokio::test]
async fn concatenated_gzip_members_are_decoded() {
    let payload = [gzip_of(b"first "), gzip_of(b""), gzip_of(&big())].concat();
    let t = serve_once(with_encoding("gzip", &payload)).await;
    let mut r = fetch(&t).await.unwrap();
    assert!(collect(&mut r.body).await == [b"first ".to_vec(), big()].concat());
}

#[tokio::test]
async fn trailing_bytes_after_a_stream_are_errors() {
    let data = b"payload".to_vec();
    let zl = {
        let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        e.write_all(&data).unwrap();
        e.finish().unwrap()
    };
    let hello_br = [
        0x8f, 0x05, 0x80, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x20, 0x62, 0x72, 0x6f, 0x74, 0x6c, 0x69,
        0x03,
    ];
    let cases = [
        (
            "gzip",
            [gzip_of(&data), b"JUNKJUNKJUNKJUNK".to_vec()].concat(),
        ),
        ("gzip", [gzip_of(&data), b"JUNK".to_vec()].concat()),
        ("deflate", [zl, b"JUNK".to_vec()].concat()),
        ("br", [hello_br.to_vec(), b"JUNK".to_vec()].concat()),
        (
            "zstd",
            [
                zstd::encode_all(&data[..], 3).unwrap(),
                b"JUNKJUNK".to_vec(),
            ]
            .concat(),
        ),
    ];
    for (name, payload) in cases {
        let t = serve_once(with_encoding(name, &payload)).await;
        let mut r = fetch(&t).await.unwrap();
        let outcome = loop {
            match r.body.chunk().await {
                Ok(Some(_)) => {}
                other => break other,
            }
        };
        assert!(outcome.is_err(), "{name} accepted trailing bytes");
    }
}

#[tokio::test]
async fn early_response_to_a_large_upload_is_returned() {
    let (l, t) = listener().await;
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut head = [0u8; 64];
        let _ = s.read(&mut head).await;
        s.write_all(b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 3\r\n\r\nbig")
            .await
            .unwrap();
        // Dropping with unread request bytes resets the connection.
    });
    let body = vec![7u8; 128 * 1024 * 1024];
    let mut r = client().send(&t, &head(body.len()), &body).await.unwrap();
    assert_eq!(r.status, 413);
    assert_eq!(collect(&mut r.body).await, b"big");
}

#[tokio::test]
async fn write_failure_without_a_reply_is_the_error() {
    let (l, t) = listener().await;
    tokio::spawn(async move {
        let (s, _) = l.accept().await.unwrap();
        drop(s);
    });
    let body = vec![7u8; 128 * 1024 * 1024];
    let err = client()
        .send(&t, &head(body.len()), &body)
        .await
        .err()
        .unwrap();
    assert!(
        matches!(err, TransportError::Io(_) | TransportError::Closed),
        "{err:?}"
    );
}
