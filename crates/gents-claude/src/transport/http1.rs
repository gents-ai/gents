//! The HTTP/1.1 client. It offers only `http/1.1` through ALPN like Bun's
//! fetch (`src/http/lib.rs`), writes the caller's request head and body
//! untouched, and reuses connections whose response ended cleanly.

use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use super::body::{Body, ChunkState, Framing, Release};
use super::conn::{read_head, Conn, Head, MAX_HEADERS};
use super::decode::Decoder;
use super::error::{is_peer_gone, TransportError};
use super::idle::{IdlePool, Key};
use super::proxy::{tunnel, Proxy};

/// Default time allowed to open a connection, proxy tunnel and TLS included.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Default time an idle connection stays pooled.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Default idle connections kept per origin.
pub const DEFAULT_MAX_IDLE_PER_HOST: usize = 8;

/// Which certificate authorities the client trusts.
#[derive(Clone, Debug, Default)]
pub enum Roots {
    /// The operating system trust store.
    #[default]
    Platform,
    /// Exactly these DER certificates (an operator-supplied CA).
    Custom(Vec<CertificateDer<'static>>),
}

/// Client settings.
#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// Time allowed to open a connection (TCP, proxy tunnel, TLS).
    pub connect_timeout: Duration,
    /// Time an idle connection stays pooled.
    pub idle_timeout: Duration,
    /// Idle connections kept per origin; zero disables reuse.
    pub max_idle_per_host: usize,
    /// Optional HTTP proxy used for TLS targets.
    pub proxy: Option<Proxy>,
    /// Trusted certificate authorities.
    pub roots: Roots,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_idle_per_host: DEFAULT_MAX_IDLE_PER_HOST,
            proxy: None,
            roots: Roots::Platform,
        }
    }
}

/// Where a request goes.
#[derive(Clone, Debug)]
pub struct Target {
    /// Host name or address literal (no brackets).
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// Whether to use TLS; plain text exists for loopback tests.
    pub tls: bool,
}

/// A response whose body is still streaming.
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Headers in received order and case.
    pub headers: Vec<(String, String)>,
    /// The decoded body stream.
    pub body: Body,
}

impl Response {
    /// The first header named `name`, compared ASCII case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        header(&self.headers, name)
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Whether a comma list header value holds `token`.
fn has_token(value: &str, token: &str) -> bool {
    value
        .split(',')
        .any(|t| t.trim().eq_ignore_ascii_case(token))
}

/// Whether the request head asks for `Connection: close`.
fn request_wants_close(head: &[u8]) -> bool {
    let mut raw = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut req = httparse::Request::new(&mut raw);
    req.parse(head).is_ok()
        && req.headers.iter().any(|h| {
            h.name.eq_ignore_ascii_case("connection")
                && std::str::from_utf8(h.value).is_ok_and(|v| has_token(v, "close"))
        })
}

/// An HTTP/1.1 client with a keep-alive pool; clones share the pool.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
}

struct Inner {
    tls: TlsConnector,
    idle: Arc<IdlePool>,
    connect_timeout: Duration,
    proxy: Option<Proxy>,
}

/// Why one attempt failed.
enum Failure {
    /// A pooled connection was dead before any response byte arrived.
    Stale,
    Fatal(TransportError),
}

impl Client {
    /// Builds a client. Loads the platform trust roots, so call it at startup.
    pub fn new(options: ClientOptions) -> Result<Self, TransportError> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let builder = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(TransportError::TlsConfig)?;
        let builder = match options.roots {
            Roots::Platform => builder
                .with_platform_verifier()
                .map_err(TransportError::TlsConfig)?,
            Roots::Custom(certs) => {
                let mut store = RootCertStore::empty();
                for cert in certs {
                    store.add(cert).map_err(TransportError::TlsConfig)?;
                }
                builder.with_root_certificates(store)
            }
        };
        let mut config = builder.with_no_client_auth();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            inner: Arc::new(Inner {
                tls: TlsConnector::from(Arc::new(config)),
                idle: Arc::new(IdlePool::new(
                    options.max_idle_per_host,
                    options.idle_timeout,
                )),
                connect_timeout: options.connect_timeout,
                proxy: options.proxy,
            }),
        })
    }

    /// Sends `head` then `body` verbatim and returns once the response head is read.
    ///
    /// A pooled connection that the server closed while idle shows up as a
    /// close or reset before any response byte; the request was then written
    /// in full to a dead socket, so it is resent once on a fresh connection.
    pub async fn send(
        &self,
        target: &Target,
        head: &[u8],
        body: &[u8],
    ) -> Result<Response, TransportError> {
        let key = Key {
            host: target.host.clone(),
            port: target.port,
            tls: target.tls,
        };
        let close = request_wants_close(head);
        if let Some(conn) = self.inner.idle.take(&key) {
            match self.exchange(conn, &key, head, body, close, true).await {
                Ok(r) => return Ok(r),
                Err(Failure::Fatal(e)) => return Err(e),
                Err(Failure::Stale) => {}
            }
        }
        let conn = self.connect(target).await?;
        match self.exchange(conn, &key, head, body, close, false).await {
            Ok(r) => Ok(r),
            Err(Failure::Fatal(e)) => Err(e),
            Err(Failure::Stale) => Err(TransportError::Closed),
        }
    }

    async fn exchange(
        &self,
        mut conn: Conn,
        key: &Key,
        head: &[u8],
        body: &[u8],
        close: bool,
        reused: bool,
    ) -> Result<Response, Failure> {
        // A server may answer and close before reading a large body; the
        // write then fails, but a complete head already received is the reply.
        let write_err = conn.write_all(head, body).await.err();
        let parsed = loop {
            let h = match read_head(&mut conn).await {
                Ok(h) => h,
                Err(_) if write_err.is_some() => {
                    return Err(Failure::Fatal(TransportError::Io(
                        write_err.unwrap_or_else(|| std::io::ErrorKind::BrokenPipe.into()),
                    )));
                }
                Err(e) => return Err(classify(e, reused && conn.buf.is_empty())),
            };
            // Interim responses precede the real one; 101 is final here.
            if !(100..200).contains(&h.status) || h.status == 101 {
                break h;
            }
        };
        let Head {
            status,
            minor,
            headers,
        } = parsed;
        let framing = framing(status, &headers).map_err(Failure::Fatal)?;
        let encoding = if matches!(framing, Framing::Done) {
            None
        } else {
            content_encoding(&headers).map_err(Failure::Fatal)?
        };
        let decoder = Decoder::for_encoding(encoding).map_err(Failure::Fatal)?;
        let keep_alive = minor == 1
            && write_err.is_none()
            && !close
            && !matches!(framing, Framing::UntilClose)
            && !headers
                .iter()
                .any(|(n, v)| n.eq_ignore_ascii_case("connection") && has_token(v, "close"));
        let release = keep_alive.then(|| Release {
            pool: self.inner.idle.clone(),
            key: key.clone(),
        });
        let mut body = Body::new(conn, framing, decoder, release);
        body.settle();
        Ok(Response {
            status,
            headers,
            body,
        })
    }

    async fn connect(&self, target: &Target) -> Result<Conn, TransportError> {
        let inner = &self.inner;
        let open = async {
            let proxy = inner.proxy.as_ref().filter(|_| target.tls);
            let tcp = match proxy {
                Some(p) => {
                    let mut s = TcpStream::connect((p.host.as_str(), p.port))
                        .await
                        .map_err(TransportError::Connect)?;
                    tunnel(
                        &mut s,
                        &target.host,
                        target.port,
                        p.authorization.as_deref(),
                    )
                    .await?;
                    s
                }
                None => TcpStream::connect((target.host.as_str(), target.port))
                    .await
                    .map_err(TransportError::Connect)?,
            };
            tcp.set_nodelay(true).map_err(TransportError::Connect)?;
            if !target.tls {
                return Ok(Conn::new(Box::new(tcp)));
            }
            let name = ServerName::try_from(target.host.clone())
                .map_err(|_| TransportError::InvalidServerName)?;
            let tls = inner
                .tls
                .connect(name, tcp)
                .await
                .map_err(TransportError::Tls)?;
            Ok(Conn::new(Box::new(tls)))
        };
        tokio::time::timeout(inner.connect_timeout, open)
            .await
            .map_err(|_| TransportError::ConnectTimeout)?
    }
}

/// Maps a head-read error; a dead pooled socket with nothing read is stale.
fn classify(e: TransportError, nothing_read: bool) -> Failure {
    let dead = match &e {
        TransportError::Closed => true,
        TransportError::Io(io) => is_peer_gone(io),
        _ => false,
    };
    if dead && nothing_read {
        Failure::Stale
    } else {
        Failure::Fatal(e)
    }
}

/// The `Content-Encoding` value; several such headers are not supported.
fn content_encoding(headers: &[(String, String)]) -> Result<Option<&str>, TransportError> {
    let mut it = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("content-encoding"));
    let first = it.next().map(|(_, v)| v.as_str());
    if it.next().is_some() {
        return Err(TransportError::UnsupportedEncoding);
    }
    Ok(first)
}

/// Body framing from the status and headers (RFC 9112 section 6.3).
fn framing(status: u16, headers: &[(String, String)]) -> Result<Framing, TransportError> {
    if status == 101 || status == 204 || status == 304 {
        return Ok(Framing::Done);
    }
    if let Some(te) = header(headers, "transfer-encoding") {
        let last = te.rsplit(',').next().unwrap_or("").trim();
        return Ok(if last.eq_ignore_ascii_case("chunked") {
            Framing::Chunked(ChunkState::Size)
        } else {
            Framing::UntilClose
        });
    }
    let mut lengths = headers
        .iter()
        .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        .map(|(_, v)| {
            let v = v.trim();
            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                return Err(TransportError::BadFraming);
            }
            v.parse::<u64>().map_err(|_| TransportError::BadFraming)
        });
    let Some(first) = lengths.next() else {
        return Ok(Framing::UntilClose);
    };
    let first = first?;
    for other in lengths {
        if other? != first {
            return Err(TransportError::BadFraming);
        }
    }
    Ok(if first == 0 {
        Framing::Done
    } else {
        Framing::Length(first)
    })
}
