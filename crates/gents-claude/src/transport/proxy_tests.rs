//! Tests for proxy selection and the CONNECT handshake.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::proxy::tunnel;
use super::{proxy_from_env, Client, ClientOptions, Proxy, ProxyError, Target, TransportError};

#[test]
fn no_proxy_matching_table() {
    let cases = [
        ("api.anthropic.com", 443, "*", true),
        ("api.anthropic.com", 443, "anthropic.com", true),
        ("api.anthropic.com", 443, ".anthropic.com", true),
        ("api.anthropic.com", 443, "*.anthropic.com", true),
        ("API.Anthropic.com", 443, "anthropic.COM", true),
        ("api.anthropic.com", 443, "example.org, anthropic.com", true),
        ("anthropic.com", 443, "anthropic.com", true),
        ("notanthropic.com", 443, "anthropic.com", false),
        ("api.anthropic.com", 443, "api", false),
        ("api.anthropic.com", 443, "", false),
        ("api.anthropic.com", 443, " , ,", false),
        ("localhost", 443, "localhost,127.0.0.1", true),
        ("api.anthropic.com", 443, "anthropic.com:443", true),
        ("api.anthropic.com", 8443, "anthropic.com:443", false),
        ("api.anthropic.com", 443, "anthropic.com:x", false),
        ("::1", 443, "::1", true),
        ("::1", 443, "[::1]:443", true),
        ("::1", 80, "[::1]:443", false),
    ];
    for (host, port, list, bypass) in cases {
        let got = proxy_from_env(host, port, Some("http://proxy.local:3128"), Some(list)).unwrap();
        assert_eq!(got.is_none(), bypass, "{host}:{port} vs {list:?}");
    }
}

#[test]
fn proxy_url_parsing() {
    let p = proxy_from_env("h", 443, Some("http://u%40x:p%3Aw@proxy.local:8080"), None)
        .unwrap()
        .unwrap();
    assert_eq!((p.host.as_str(), p.port), ("proxy.local", 8080));
    // base64("u@x:p:w")
    assert_eq!(p.authorization.as_deref(), Some("Basic dUB4OnA6dw=="));
    let p = proxy_from_env("h", 443, Some(" http://proxy.local "), None)
        .unwrap()
        .unwrap();
    assert_eq!(p.port, 80);
    assert!(p.authorization.is_none());
}

#[test]
fn unset_blank_or_excluded_proxy_is_direct() {
    assert!(proxy_from_env("h", 443, None, None).unwrap().is_none());
    assert!(proxy_from_env("h", 443, Some(""), None).unwrap().is_none());
    assert!(proxy_from_env("h", 443, Some("  "), Some("x"))
        .unwrap()
        .is_none());
    assert!(proxy_from_env("h", 443, Some("http://p"), Some("h"))
        .unwrap()
        .is_none());
    // An excluded host never reports a bad proxy value.
    assert!(proxy_from_env("h", 443, Some("junk"), Some("*"))
        .unwrap()
        .is_none());
}

#[test]
fn bad_proxy_settings_fail_loudly() {
    assert!(matches!(
        proxy_from_env("h", 443, Some("https://proxy.local"), None),
        Err(ProxyError::UnsupportedScheme)
    ));
    assert!(matches!(
        proxy_from_env("h", 443, Some("socks5://proxy.local"), None),
        Err(ProxyError::UnsupportedScheme)
    ));
    for bad in ["not a url", "proxy.local:3128x", "http://", "http://:80"] {
        assert!(
            proxy_from_env("h", 443, Some(bad), None).is_err(),
            "{bad:?}"
        );
    }
}

async fn peer_reply(
    reply: &'static [u8],
) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<Vec<u8>>) {
    let (a, mut b) = tokio::io::duplex(4096);
    let h = tokio::spawn(async move {
        let mut got = Vec::new();
        let mut tmp = [0u8; 256];
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = b.read(&mut tmp).await.unwrap();
            got.extend_from_slice(&tmp[..n]);
        }
        b.write_all(reply).await.unwrap();
        got
    });
    (a, h)
}

#[tokio::test]
async fn tunnel_writes_connect_and_accepts_2xx() {
    let (mut io, peer) = peer_reply(b"HTTP/1.1 200 Connection established\r\n\r\n").await;
    tunnel(&mut io, "api.anthropic.com", 443, Some("Basic abc"))
        .await
        .unwrap();
    assert_eq!(
        peer.await.unwrap(),
        b"CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: api.anthropic.com:443\r\nProxy-Authorization: Basic abc\r\n\r\n"
    );
}

#[tokio::test]
async fn tunnel_brackets_ipv6_and_omits_missing_auth() {
    let (mut io, peer) = peer_reply(b"HTTP/1.1 204 No Content\r\n\r\n").await;
    tunnel(&mut io, "::1", 443, None).await.unwrap();
    assert_eq!(
        peer.await.unwrap(),
        b"CONNECT [::1]:443 HTTP/1.1\r\nHost: [::1]:443\r\n\r\n"
    );
}

#[tokio::test]
async fn tunnel_reports_refusal_and_bad_replies() {
    let (mut io, _p) = peer_reply(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n").await;
    assert!(matches!(
        tunnel(&mut io, "h", 443, None).await,
        Err(TransportError::ProxyRefused(407))
    ));
    let (mut io, _p) = peer_reply(b"HTTP/1.1 200 OK\r\n\r\nTLS-bytes").await;
    assert!(matches!(
        tunnel(&mut io, "h", 443, None).await,
        Err(TransportError::ProxyReply)
    ));
    let (mut io, _p) = peer_reply(b"garbage\r\n\r\n").await;
    assert!(matches!(
        tunnel(&mut io, "h", 443, None).await,
        Err(TransportError::ProxyReply)
    ));
}

#[tokio::test]
async fn tunnel_rejects_header_injection() {
    let (mut io, _p) = peer_reply(b"").await;
    assert!(matches!(
        tunnel(&mut io, "h\r\nX: y", 443, None).await,
        Err(TransportError::InvalidServerName)
    ));
    assert!(matches!(
        tunnel(&mut io, "h", 443, Some("Basic a\r\nX: y")).await,
        Err(TransportError::InvalidProxy)
    ));
}

#[tokio::test]
async fn client_tunnels_tls_targets_through_the_proxy() {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut s, _) = l.accept().await.unwrap();
        let mut got = Vec::new();
        let mut tmp = [0u8; 256];
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut tmp).await.unwrap();
            got.extend_from_slice(&tmp[..n]);
        }
        tx.send(got).unwrap();
        s.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
            .await
            .unwrap();
    });
    let c = Client::new(ClientOptions {
        proxy: Some(Proxy {
            host: "127.0.0.1".into(),
            port,
            authorization: Some("Basic abc".into()),
        }),
        ..ClientOptions::default()
    })
    .unwrap();
    let target = Target {
        host: "example.com".into(),
        port: 443,
        tls: true,
    };
    let err = c
        .send(&target, b"POST / HTTP/1.1\r\n\r\n", b"")
        .await
        .err()
        .unwrap();
    assert!(matches!(err, TransportError::ProxyRefused(407)));
    assert_eq!(
        rx.await.unwrap(),
        b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\nProxy-Authorization: Basic abc\r\n\r\n"
    );
}
