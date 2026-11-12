use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use gents_lean_vocab::families::usage_read_contracts_passes as family;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Provider {
    Grok,
    Claude,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Grok => GROK,
            Self::Claude => CLAUDE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum BackendKind {
    Account,
    Pool,
    Keyed,
    Unkeyed,
    Unreported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Response {
    Windows,
    Throttled,
    Malformed,
    Unauthorized,
    Failed,
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    Read,
    SkippedRecent,
    NotReported,
    Disabled,
    Unavailable,
}

const CLAUDE_USAGE_BODY: &str =
    r#"{"five_hour":{"utilization":12.0,"resets_at":"2100-01-01T00:00:00Z"}}"#;

struct Upstream {
    origin: String,
    requests: Arc<AtomicU64>,
    response: Arc<std::sync::Mutex<Response>>,
}

fn answer(response: Response, path: &str) -> Option<(u16, &'static str)> {
    Some(match response {
        Response::Windows if path.contains("/billing") => (200, GROK_BILLING_BODY),
        Response::Windows if path.contains("/oauth/usage") => (200, CLAUDE_USAGE_BODY),
        Response::Windows => (200, OPENROUTER_KEY_BODY),
        Response::Throttled => (429, r#"{"error":"rate limited"}"#),
        Response::Malformed => (200, "[]"),
        Response::Unauthorized => (401, "{}"),
        Response::Failed => (500, "{}"),
        Response::Unreachable => return None,
    })
}

async fn upstream() -> Upstream {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(AtomicU64::new(0));
    let response = Arc::new(std::sync::Mutex::new(Response::Windows));
    let (count, current) = (requests.clone(), response.clone());
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let (count, current) = (count.clone(), current.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut chunk = [0u8; 4096];
                while !head.windows(4).any(|end| end == b"\r\n\r\n") {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(read) => head.extend_from_slice(&chunk[..read]),
                    }
                }
                count.fetch_add(1, Ordering::SeqCst);
                let head = String::from_utf8_lossy(&head).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or_default();
                let response = *current.lock().unwrap();
                let Some((status, body)) = answer(response, path) else {
                    return;
                };
                let reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(reply.as_bytes()).await.ok();
                socket.shutdown().await.ok();
            });
        }
    });
    Upstream {
        origin,
        requests,
        response,
    }
}

fn stored_backend(
    did: &str,
    origin: &str,
    index: usize,
    modelled: &family::Backend<BackendKind, Provider>,
) -> InferenceBackend {
    let id = format!("backend-usage-{index}");
    let kind = match modelled.provider {
        Provider::Grok => "XaiGrokOAuth",
        Provider::Claude => "ClaudeCliSubscription",
    };
    let endpoint = format!("{origin}/api/v1");
    let mut stored = match modelled.kind {
        BackendKind::Account => oauth_backend(did, kind, &id, modelled.slot.as_deref(), true),
        BackendKind::Pool => InferenceBackend {
            auth: BackendAuth::NodeOAuthPool {
                allow_overage: false,
            },
            ..oauth_backend(did, kind, &id, None, true)
        },
        BackendKind::Keyed => api_key_backend(
            did,
            endpoint,
            &id,
            json!({ "kind": "api_key", "key": "sk-or-TEST" }),
        ),
        BackendKind::Unkeyed => api_key_backend(
            did,
            endpoint,
            &id,
            json!({ "kind": "environment", "variable": "GENTS_USAGE_TEST_UNSET_KEY" }),
        ),
        BackendKind::Unreported => InferenceBackend {
            auth: BackendAuth::Unauthenticated,
            ..backend(
                did,
                "OpenAiCompatible",
                &id,
                json!({ "kind": "api_key", "key": "key-SECRET" }),
            )
        },
    };
    stored.enabled = modelled.enabled;
    stored
}

fn entry(
    accounts: &[family::Account<Provider>],
    read: AccountUsageRead,
) -> family::Entry<Outcome> {
    let (outcome, reason) = match read.outcome {
        UsageRead::Read => (Outcome::Read, None),
        UsageRead::SkippedRecent => (Outcome::SkippedRecent, None),
        UsageRead::NotReported => (Outcome::NotReported, None),
        UsageRead::Disabled => (Outcome::Disabled, None),
        UsageRead::Unavailable(reason) => (Outcome::Unavailable, Some(reason)),
    };
    let backend = read.backend_id.as_deref().map(|id| {
        id.trim_start_matches("backend-usage-")
            .parse::<u64>()
            .expect("a modelled backend")
    });
    let account = backend.is_none().then(|| {
        accounts
            .iter()
            .position(|account| {
                account.provider.name() == read.provider && account.slot == read.account_ref
            })
            .expect("a modelled account") as u64
    });
    family::Entry {
        account,
        backend,
        outcome,
        reason,
        usage: read.usage.map(|usage| family::Seen {
            windows: !usage.windows.is_empty(),
            error: usage.read_error,
        }),
    }
}

#[tokio::test]
async fn generated_usage_reads_reach_the_modelled_outcomes() {
    use futures::StreamExt;

    let cases = family::cases::<BackendKind, Outcome, Provider, Response, UsageTrigger>();
    futures::stream::iter(cases)
        .for_each_concurrent(4, |case| async move {
            let cache = UsageCache::default();
            let (db, _user, user_access) =
                crate::test_support::signed_test_node_without_agent("usage-scenarios").await;
            let node = db.node.clone();
            let did = db.node_identity.did();
            let upstream = upstream().await;
            for account in &case.input.accounts {
                let provider = account.provider.name();
                let mut row = credential(did, account.slot.as_deref(), None);
                row.provider = provider.to_owned();
                row.credential_id = match &account.slot {
                    Some(slot) => format!("{}:{slot}", oauth_credential_id(did, provider)),
                    None => oauth_credential_id(did, provider),
                };
                row.enabled = account.enabled;
                row.connected_at = Some(Utc::now());
                seed(&node, &row).await;
            }
            let backends: Vec<_> = case
                .input
                .backends
                .iter()
                .enumerate()
                .map(|(index, modelled)| stored_backend(did, &upstream.origin, index, modelled))
                .collect();
            store_backends(&user_access, &backends).await;
            let endpoints = UsageEndpoints {
                grok_billing: format!("{}/v1/billing?format=credits", upstream.origin),
                claude_usage: format!("{}/api/oauth/usage", upstream.origin),
            };
            let started = Utc::now();
            let mut seen = Vec::new();
            for pass in &case.input.passes {
                *upstream.response.lock().unwrap() = pass.response;
                let before = upstream.requests.load(Ordering::SeqCst);
                let reads = read_node_usage(
                    &cache,
                    node.clone(),
                    did,
                    pass.trigger,
                    case.input.filter.map(Provider::name),
                    &endpoints,
                    started + Duration::seconds(pass.second as i64),
                )
                .await
                .expect("node read");
                let mut entries: Vec<_> = reads
                    .into_iter()
                    .map(|read| entry(&case.input.accounts, read))
                    .collect();
                entries.sort_by_key(|entry| (entry.backend.is_some(), entry.account, entry.backend));
                seen.push(family::PassSeen {
                    requests: upstream.requests.load(Ordering::SeqCst) - before,
                    entries,
                });
            }
            case.check(seen);
        })
        .await;
}
