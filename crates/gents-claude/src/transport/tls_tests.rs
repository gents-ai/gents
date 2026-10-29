//! TLS tests against an in-process rustls server with a fixed test CA:
//! request bytes, ALPN, SNI, reuse, trust failure, tunnelling and timeouts.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;

use super::http1_tests::{collect, head, read_request};
use super::{Client, ClientOptions, Proxy, Roots, Target, TransportError};

const CA: &[u8] = include_bytes!("testdata/ca.pem");
const LEAF: &[u8] = include_bytes!("testdata/leaf.pem");
const KEY: &[u8] = include_bytes!("testdata/leaf.key");

fn ca() -> CertificateDer<'static> {
    CertificateDer::from_pem_slice(CA).unwrap()
}

fn client_trusting_ca() -> Client {
    Client::new(ClientOptions {
        roots: Roots::Custom(vec![ca()]),
        ..ClientOptions::default()
    })
    .unwrap()
}

fn acceptor() -> TlsAcceptor {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from_pem_slice(LEAF).unwrap()],
            PrivateKeyDer::from_pem_slice(KEY).unwrap(),
        )
        .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsAcceptor::from(Arc::new(config))
}

/// What the TLS server saw on one connection.
#[derive(Debug)]
struct Seen {
    alpn: Option<Vec<u8>>,
    sni: Option<String>,
    request: Vec<u8>,
}

/// A keep-alive TLS server answering `ok`; reports each request and counts accepts.
async fn tls_server() -> (Target, mpsc::UnboundedReceiver<Seen>, Arc<AtomicUsize>) {
    let l = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let target = Target {
        host: "localhost".into(),
        port: l.local_addr().unwrap().port(),
        tls: true,
    };
    let (tx, rx) = mpsc::unbounded_channel();
    let accepts = Arc::new(AtomicUsize::new(0));
    let n = accepts.clone();
    let acceptor = acceptor();
    tokio::spawn(async move {
        loop {
            let (tcp, _) = l.accept().await.unwrap();
            n.fetch_add(1, Ordering::SeqCst);
            let (acceptor, tx) = (acceptor.clone(), tx.clone());
            tokio::spawn(async move {
                let Ok(mut s) = acceptor.accept(tcp).await else {
                    return;
                };
                let (alpn, sni) = {
                    let c = s.get_ref().1;
                    (
                        c.alpn_protocol().map(<[u8]>::to_vec),
                        c.server_name().map(str::to_owned),
                    )
                };
                while let Some(request) = read_request(&mut s).await {
                    let _ = tx.send(Seen {
                        alpn: alpn.clone(),
                        sni: sni.clone(),
                        request,
                    });
                    if s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
    });
    (target, rx, accepts)
}

#[tokio::test]
async fn tls_exchange_sends_exact_bytes_with_http11_alpn_and_sni() {
    let (t, mut seen, _) = tls_server().await;
    let body = br#"{"model":"x"}"#;
    let h = head(body.len());
    let mut r = client_trusting_ca().send(&t, &h, body).await.unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(collect(&mut r.body).await, b"ok");
    let s = seen.recv().await.unwrap();
    assert_eq!(s.request, [h.as_slice(), body].concat());
    assert_eq!(s.alpn.as_deref(), Some(b"http/1.1".as_slice()));
    assert_eq!(s.sni.as_deref(), Some("localhost"));
}

#[tokio::test]
async fn tls_keep_alive_is_reused() {
    let (t, _seen, accepts) = tls_server().await;
    let c = client_trusting_ca();
    for _ in 0..3 {
        let mut r = c.send(&t, &head(0), b"").await.unwrap();
        assert_eq!(collect(&mut r.body).await, b"ok");
    }
    assert_eq!(accepts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn untrusted_certificate_is_a_tls_error() {
    let (t, _seen, _) = tls_server().await;
    let none = Client::new(ClientOptions {
        roots: Roots::Custom(Vec::new()),
        ..ClientOptions::default()
    })
    .unwrap();
    assert!(matches!(
        none.send(&t, &head(0), b"").await,
        Err(TransportError::Tls(_))
    ));
}

#[tokio::test]
async fn wrong_server_name_is_a_tls_error() {
    let (mut t, _seen, _) = tls_server().await;
    t.host = "127.0.0.2".into();
    assert!(matches!(
        client_trusting_ca().send(&t, &head(0), b"").await,
        Err(TransportError::Tls(_))
    ));
}

#[tokio::test]
async fn tls_exchange_runs_through_a_connect_tunnel() {
    let (t, mut seen, _) = tls_server().await;
    let upstream = format!("127.0.0.1:{}", t.port);
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_port = l.local_addr().unwrap().port();
    let (conn_tx, conn_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut got = Vec::new();
        let mut tmp = [0u8; 256];
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut tmp).await.unwrap();
            got.extend_from_slice(&tmp[..n]);
        }
        conn_tx.send(got).unwrap();
        let mut up = TcpStream::connect(upstream).await.unwrap();
        s.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .await
            .unwrap();
        let _ = tokio::io::copy_bidirectional(&mut s, &mut up).await;
    });
    let c = Client::new(ClientOptions {
        roots: Roots::Custom(vec![ca()]),
        proxy: Some(Proxy {
            host: "127.0.0.1".into(),
            port: proxy_port,
            authorization: Some("Basic abc".into()),
        }),
        ..ClientOptions::default()
    })
    .unwrap();
    let body = b"through the tunnel";
    let h = head(body.len());
    let mut r = c.send(&t, &h, body).await.unwrap();
    assert_eq!(collect(&mut r.body).await, b"ok");
    assert_eq!(
        conn_rx.await.unwrap(),
        format!(
            "CONNECT localhost:{p} HTTP/1.1\r\nHost: localhost:{p}\r\nProxy-Authorization: Basic abc\r\n\r\n",
            p = t.port
        )
        .into_bytes()
    );
    let s = seen.recv().await.unwrap();
    assert_eq!(s.request, [h.as_slice(), body].concat());
    assert_eq!(s.sni.as_deref(), Some("localhost"));
}

#[tokio::test]
async fn silent_server_times_out_within_the_connect_timeout() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let t = Target {
        host: "localhost".into(),
        port: l.local_addr().unwrap().port(),
        tls: true,
    };
    // Accepts the TCP connection but never answers the ClientHello.
    tokio::spawn(async move {
        let (_held, _) = l.accept().await.unwrap();
        std::future::pending::<()>().await;
    });
    let c = Client::new(ClientOptions {
        connect_timeout: Duration::from_millis(200),
        roots: Roots::Custom(vec![ca()]),
        ..ClientOptions::default()
    })
    .unwrap();
    let start = Instant::now();
    let err = c.send(&t, &head(0), b"").await.err().unwrap();
    assert!(matches!(err, TransportError::ConnectTimeout), "{err:?}");
    let took = start.elapsed();
    assert!(
        took >= Duration::from_millis(200) && took < Duration::from_secs(5),
        "{took:?}"
    );
}
