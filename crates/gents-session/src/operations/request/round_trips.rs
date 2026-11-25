//! What a client send asks its node over HTTP, request by request. A proxy
//! between the client and the node passes every byte through unchanged and
//! keeps what the client sent, so each request can be named afterwards.

use std::sync::{Arc, Mutex};

use gents_protocol::operations::request::MessageSendInput;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_store::identity::NodeIdentity;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One HTTP request the client made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Sent {
    /// Opens a transaction (`POST /tx`), read-only or not.
    Begin {
        read_only: bool,
    },
    /// Commits or discards one (`/tx/{id}`).
    Finish,
    /// A GraphQL request, inside a transaction or not, with its body.
    Graphql {
        in_txn: bool,
        body: String,
    },
    Other(String),
}

/// The bytes one connection sent towards the node, in order.
type ConnectionLog = Arc<Mutex<Vec<u8>>>;

/// What each connection sent.
#[derive(Clone, Default)]
pub(super) struct Wire(Arc<Mutex<Vec<ConnectionLog>>>);

impl Wire {
    /// Forgets what was sent so far; connections stay open.
    pub(super) fn clear(&self) {
        for connection in self.0.lock().unwrap().iter() {
            connection.lock().unwrap().clear();
        }
    }

    pub(super) fn requests(&self) -> Vec<Sent> {
        let mut sent = Vec::new();
        for connection in self.0.lock().unwrap().iter() {
            sent.extend(parse(&connection.lock().unwrap()));
        }
        sent
    }
}

fn parse(bytes: &[u8]) -> Vec<Sent> {
    let mut sent = Vec::new();
    let mut at = 0;
    while let Some(end) = find(&bytes[at..], b"\r\n\r\n") {
        let head = String::from_utf8_lossy(&bytes[at..at + end]).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default().to_owned();
        let headers: Vec<(String, String)> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
            .collect();
        let length = headers
            .iter()
            .find(|(name, _)| name == "content-length")
            .and_then(|(_, value)| value.parse::<usize>().ok())
            .unwrap_or(0);
        let body_start = at + end + 4;
        let body_end = (body_start + length).min(bytes.len());
        let body = String::from_utf8_lossy(&bytes[body_start..body_end]).to_string();
        at = body_end;
        let mut parts = request_line.split(' ');
        let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        let route = path.split('?').next().unwrap_or_default();
        sent.push(if method == "POST" && route.ends_with("/tx") {
            Sent::Begin {
                read_only: path.contains("read_only=true"),
            }
        } else if route.contains("/tx/") {
            Sent::Finish
        } else if route.ends_with("/graphql") {
            Sent::Graphql {
                in_txn: headers.iter().any(|(name, _)| name == "x-defradb-tx"),
                body,
            }
        } else {
            Sent::Other(request_line)
        });
    }
    sent
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Starts a proxy to the node at `url`; returns the URL to use instead.
pub(super) async fn proxy(url: &str) -> (String, Wire) {
    let target = url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap()
        .to_owned();
    let path = url.splitn(4, '/').nth(3).unwrap_or_default().to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let wire = Wire::default();
    let connections = wire.clone();
    tokio::spawn(async move {
        while let Ok((client, _)) = listener.accept().await {
            let node = TcpStream::connect(&target).await.unwrap();
            let log = Arc::new(Mutex::new(Vec::new()));
            connections.0.lock().unwrap().push(log.clone());
            let (mut client_read, mut client_write) = client.into_split();
            let (mut node_read, mut node_write) = node.into_split();
            tokio::spawn(async move {
                let mut buffer = vec![0; 64 * 1024];
                while let Ok(read) = client_read.read(&mut buffer).await {
                    if read == 0 || node_write.write_all(&buffer[..read]).await.is_err() {
                        break;
                    }
                    log.lock().unwrap().extend_from_slice(&buffer[..read]);
                }
            });
            tokio::spawn(async move {
                let _ = tokio::io::copy(&mut node_read, &mut client_write).await;
            });
        }
    });
    (format!("http://{address}/{path}"), wire)
}

fn query(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value["query"].as_str().map(str::to_owned))
        .unwrap_or_else(|| body.to_owned())
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn summary(sent: &[Sent]) -> String {
    sent.iter()
        .map(|request| match request {
            Sent::Begin { read_only } => {
                format!("BEGIN{}", if *read_only { " read-only" } else { "" })
            }
            Sent::Finish => "FINISH".to_owned(),
            Sent::Graphql { in_txn, body } => format!(
                "{} {}",
                if *in_txn { "  txn" } else { "plain" },
                query(body).chars().take(160).collect::<String>()
            ),
            Sent::Other(line) => line.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// How often a send read the session row by its scope (node and session),
/// and how many read-only transactions it opened.
fn counted(sent: &[Sent]) -> (usize, usize) {
    let session_reads = sent
        .iter()
        .filter(|request| match request {
            Sent::Graphql { body, .. } => {
                let query = query(body);
                query.match_indices("AgentSession(filter:").any(|(at, _)| {
                    let filter = &query[at..];
                    let filter = &filter[..filter.find(')').unwrap_or(filter.len())];
                    filter.contains("session_id")
                })
            }
            _ => false,
        })
        .count();
    let read_only = sent
        .iter()
        .filter(|request| matches!(request, Sent::Begin { read_only: true }))
        .count();
    (session_reads, read_only)
}

/// A send reads its session's row once before its transactions, once before
/// submitting to check whether its binding is already published, and once in
/// the read-only transaction that prepares the signed targets; a published
/// session's submit transaction never reads the session row at all.
#[tokio::test]
async fn a_send_reads_its_session_row_once_per_step() -> anyhow::Result<()> {
    let signed_home = gents_documents::test_support::SignedHome::start(
        "send-round-trips",
        crate::test_support::no_lifecycle_plugins(),
    )
    .await;
    let agent = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, user, _) = signed_home.into_parts();
    let (url, wire) = proxy(db.graphql_endpoint()).await;
    let access = ConfigAccess::Graphql(
        GraphqlEndpoint::as_principal(url, user.did())
            .with_client_signing_capability(user.client_signing_capability()?)?
            .with_client_signed_agent_creates(),
    );
    let send = |content: &str| MessageSendInput {
        node_did: db.node_identity.did().to_owned(),
        session_id: Some("round-trips-session".to_owned()),
        agent_did: Some(agent.agent_did.clone()),
        content: content.to_owned(),
        ..Default::default()
    };

    /* the first send checks for a published binding before making the session */
    super::send(&access, &user, send("first")).await?;
    let first = wire.requests();
    assert_eq!(counted(&first), (6, 1), "first send:\n{}", summary(&first));

    /* the second finds the published binding outside the submit transaction */
    wire.clear();
    super::send(&access, &user, send("second")).await?;
    let second = wire.requests();
    assert_eq!(
        counted(&second),
        (3, 1),
        "second send:\n{}",
        summary(&second)
    );
    db.node.shutdown().await?;
    Ok(())
}
