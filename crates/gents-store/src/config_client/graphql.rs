use gents_protocol::delegation_audience::HeldRecord;
use gents_protocol::graphql::GraphqlRequestOptions;
use serde_json::Value;

use super::retry;
use super::write_telemetry::ConflictSource;
use super::{graphql_api_base, ConfigClientError, GraphqlEndpoint};

type Result<T, E = ConfigClientError> = std::result::Result<T, E>;

/// The client of every HTTP request to the store, read or write: it waits
/// for the server's answer or a connection failure, never a client clock. A
/// commit abandoned by a total request timeout while a loaded server is still
/// committing has an unknown outcome that only a dead server justifies, and
/// a read under one fails a live server that is only slow
/// (`tla/CommitResponseWait.tla`, `ClientWindow`). Keepalive probes end a
/// connection whose host is gone (`NoKeepalive`); a live server that stops
/// answering holds the request until the server bounds it (`Wedged`).
pub(super) async fn http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: tokio::sync::OnceCell<reqwest::Client> = tokio::sync::OnceCell::const_new();
    CLIENT
        .get_or_try_init(|| async {
            tokio::task::spawn_blocking(|| {
                gents_protocol::graphql::store_http_client_builder()
                    .build()
                    .map_err(ConfigClientError::HttpClientBuild)
            })
            .await
            .map_err(ConfigClientError::HttpClientJoin)?
        })
        .await
}

pub(super) async fn auto_commit(
    endpoint: &GraphqlEndpoint,
    mutation: &str,
    variables: Option<&Value>,
) -> Result<Value> {
    if !super::client_signed_agent::ordinary_mutation_allowed(
        endpoint.client_signed_agent_creates,
        true,
    ) {
        return Err(ConfigClientError::ClientUnsignedMutationUnsupported);
    }
    let body = match variables {
        Some(variables) => serde_json::json!({"query": mutation, "variables": variables}),
        None => serde_json::json!({"query": mutation}),
    };
    match auto_commit_once(endpoint, &body).await? {
        (Err(refused), false, presented)
            if recovers_window(endpoint, &refused, &presented).await? =>
        {
            auto_commit_once(endpoint, &body).await?.0
        }
        (answered, _, _) => answered,
    }
}

/// Whether `error` is a delegation DefraDB refused for its window, which the
/// endpoint recovers by minting it again at its home's clock and sending the
/// request once more (`Identity.HeldAudience.resend`).
/// External premise: such a write was refused before anything was written.
async fn recovers_window(
    endpoint: &GraphqlEndpoint,
    error: &ConfigClientError,
    presented: &HeldRecord,
) -> Result<bool> {
    match error.credential_answer() {
        Some(answer @ gents_protocol::delegation_audience::CredentialAnswer::OutsideWindow) => {
            endpoint.recovers(answer, presented).await
        }
        _ => Ok(false),
    }
}

/// One mutation, sent once more for a refused credential, whether it was,
/// and the record its last sending was minted from.
async fn auto_commit_once(
    endpoint: &GraphqlEndpoint,
    body: &Value,
) -> Result<(Result<Value>, bool, HeldRecord)> {
    let client = http_client().await?;
    let sent = endpoint
        .send_counting_resends(|endpoint| {
            Ok(endpoint.authorize(client.post(endpoint.url()))?.json(body))
        })
        .await?;
    Ok((
        answer_mutation(endpoint, sent.response).await,
        sent.resent,
        sent.presented,
    ))
}

async fn answer_mutation(
    endpoint: &GraphqlEndpoint,
    response: std::result::Result<reqwest::Response, reqwest::Error>,
) -> Result<Value> {
    let response = response
        .map_err(|source| ConfigClientError::PostMutation {
            endpoint: endpoint.to_string(),
            source,
        })?
        .error_for_status()
        .map_err(|source| ConfigClientError::ReadMutationResponse {
            endpoint: endpoint.to_string(),
            source,
        })?;
    let value: Value =
        response
            .json()
            .await
            .map_err(|source| ConfigClientError::DecodeMutationResponse {
                endpoint: endpoint.to_string(),
                source,
            })?;
    if graphql_response_has_errors(&value) {
        return Err(gents_protocol::database_error::GraphqlFailure::new(
            "config mutation",
            value["errors"].clone(),
        )
        .into());
    }
    Ok(value)
}

pub(super) async fn query_with_options(
    endpoint: &GraphqlEndpoint,
    query: &str,
    variables: Option<&Value>,
    options: GraphqlRequestOptions,
) -> Result<Value> {
    ensure_query_document(query)?;
    let (answered, presented) = query_once(endpoint, query, variables, options).await;
    let refused = match &answered {
        Err(ConfigClientError::Protocol(error)) => {
            error.transport_error().and_then(reqwest::Error::status)
        }
        _ => None,
    };
    let recovered = match (refused, &answered) {
        (Some(status), _) => endpoint.recovers_refusal(status, &presented).await?,
        (None, Err(error)) => recovers_window(endpoint, error, &presented).await?,
        (None, Ok(_)) => false,
    };
    if recovered {
        query_once(endpoint, query, variables, options).await.0
    } else {
        answered
    }
}

/// One query, and the record its credential was minted from.
async fn query_once(
    endpoint: &GraphqlEndpoint,
    query: &str,
    variables: Option<&Value>,
    options: GraphqlRequestOptions,
) -> (Result<Value>, HeldRecord) {
    if let Err(error) = endpoint.runtime_authentication().await {
        return (Err(error), endpoint.delegation_audience.record());
    }
    let presented = endpoint.delegation_audience.record();
    (
        send_query(endpoint, query, variables, options).await,
        presented,
    )
}

async fn send_query(
    endpoint: &GraphqlEndpoint,
    query: &str,
    variables: Option<&Value>,
    options: GraphqlRequestOptions,
) -> Result<Value> {
    let authorization = endpoint.authorization()?;
    Ok(gents_protocol::graphql::execute_graphql_encoded(
        endpoint.memory.request_encoding(),
        endpoint.url(),
        authorization.as_deref(),
        query,
        variables,
        options,
    )
    .await?)
}

pub(super) async fn txn_begin(
    endpoint: &GraphqlEndpoint,
    read_only: bool,
) -> Result<(reqwest::Client, String, u64)> {
    let client = http_client().await?.clone();
    let path = format!("{}/tx", graphql_api_base(endpoint.url())?);
    let url = if read_only {
        format!("{path}?read_only=true")
    } else {
        path
    };
    let response = endpoint
        .send_resolving(|endpoint| endpoint.authorize(client.post(&url)))
        .await?
        .map_err(|source| ConfigClientError::PostTxBegin {
            endpoint: endpoint.to_string(),
            source,
        })?;
    let status = response.status();
    let fence_epoch = response
        .headers()
        .get(super::relationship_fence::FENCE_EPOCH_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let bytes = response
        .bytes()
        .await
        .map_err(ConfigClientError::ReadTxBegin)?;
    if !status.is_success() {
        if let Some(source) = storage_full_response(&bytes) {
            return Err(ConfigClientError::TxBeginStorageFull {
                status,
                endpoint: endpoint.to_string(),
                source,
            });
        }
        return Err(ConfigClientError::TxBeginStatus {
            status,
            endpoint: endpoint.to_string(),
            body: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    let body: Value = serde_json::from_slice(&bytes).map_err(ConfigClientError::DecodeTxBegin)?;
    let id = body
        .get("id")
        .and_then(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| value.as_u64().map(|id| id.to_string()))
        })
        .ok_or(ConfigClientError::TxBeginMissingId)?;
    Ok((client, id, fence_epoch))
}

pub(super) async fn txn_execute(
    endpoint: &GraphqlEndpoint,
    id: &str,
    client: &reqwest::Client,
    query: &str,
    variables: &Value,
) -> Result<Value> {
    let response = endpoint
        .authorize_in_transaction(client.post(endpoint.url()), id)?
        .json(&serde_json::json!({"query": query, "variables": variables}))
        .send()
        .await
        .map_err(|source| ConfigClientError::PostTxGraphql {
            endpoint: endpoint.to_string(),
            source,
        })?
        .error_for_status()
        .map_err(|source| ConfigClientError::ReadTxGraphql {
            endpoint: endpoint.to_string(),
            source,
        })?;
    let value: Value =
        response
            .json()
            .await
            .map_err(|source| ConfigClientError::DecodeTxGraphql {
                endpoint: endpoint.to_string(),
                source,
            })?;
    validate_txn_response(value)
}

pub(super) fn validate_txn_response(value: Value) -> Result<Value> {
    if retry::graphql_value_is_transaction_conflict(&value) {
        return Err(retry::transaction_conflict(ConflictSource::StructuredCode));
    }
    if graphql_response_has_errors(&value) {
        return Err(gents_protocol::database_error::GraphqlFailure::new(
            "config mutation",
            value["errors"].clone(),
        )
        .into());
    }
    Ok(value)
}

pub(super) enum TxnCommitError {
    /// The request may not have reached DefraDB, so the transaction can still
    /// be open and needs best-effort cleanup.
    CleanupRequired(ConfigClientError),
    /// DefraDB returned a commit response. Its pinned transaction registry
    /// consumes the handle before attempting the storage commit.
    TransactionConsumed(ConfigClientError),
    /// DefraDB's commit-conflict response confirms no transaction writes committed.
    ConfirmedConflict(ConfigClientError),
}

pub(super) async fn txn_commit(
    endpoint: &GraphqlEndpoint,
    id: &str,
    client: &reqwest::Client,
) -> std::result::Result<(), TxnCommitError> {
    let api_base = graphql_api_base(endpoint.url()).map_err(TxnCommitError::CleanupRequired)?;
    let response = endpoint
        .authorize_runtime(client.post(format!("{api_base}/tx/{id}")))
        .await
        .map_err(TxnCommitError::CleanupRequired)?
        .send()
        .await
        .map_err(|source| {
            TxnCommitError::CleanupRequired(ConfigClientError::PostTxCommit {
                endpoint: endpoint.to_string(),
                source,
            })
        })?;
    let status = response.status();
    let bytes = response.bytes().await.map_err(|source| {
        TxnCommitError::TransactionConsumed(ConfigClientError::ReadTxCommit(source))
    })?;
    if status.is_success() {
        return Ok(());
    }
    let body = String::from_utf8_lossy(&bytes);
    if let Some(source) = storage_full_response(&bytes) {
        return Err(TxnCommitError::TransactionConsumed(
            ConfigClientError::TxCommitStorageFull {
                status,
                endpoint: endpoint.to_string(),
                body: body.into_owned(),
                source,
            },
        ));
    }
    if status == reqwest::StatusCode::CONFLICT && is_confirmed_commit_conflict(&bytes) {
        return Err(TxnCommitError::ConfirmedConflict(
            retry::transaction_conflict(ConflictSource::HttpStatusTextFallback),
        ));
    }
    if status == reqwest::StatusCode::CONFLICT && retry::is_transaction_conflict_text(&body) {
        return Err(TxnCommitError::TransactionConsumed(
            retry::transaction_conflict(ConflictSource::HttpStatusTextFallback),
        ));
    }
    Err(TxnCommitError::TransactionConsumed(
        ConfigClientError::TxCommitStatus {
            status,
            endpoint: endpoint.to_string(),
            body: body.into_owned(),
        },
    ))
}

/// DefraDB emits this outcome at the commit endpoint only for the registry's
/// typed conflict, which Regolith decides before writing a commit record.
/// Older servers' text-classified 409 responses do not establish finality.
fn is_confirmed_commit_conflict(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes).is_ok_and(|value| {
        value.as_object().is_some_and(|body| {
            body.get("error").and_then(Value::as_str) == Some(query::executor::TXN_CONFLICT_MESSAGE)
                && body.get("outcome").and_then(Value::as_str)
                    == Some("transaction_conflict_aborted")
        })
    })
}

pub(super) async fn txn_discard(
    endpoint: &GraphqlEndpoint,
    id: &str,
    client: &reqwest::Client,
) -> Result<()> {
    let response = endpoint
        .authorize_runtime(client.delete(format!("{}/tx/{id}", graphql_api_base(endpoint.url())?)))
        .await?
        .send()
        .await
        .map_err(|source| ConfigClientError::PostTxDiscard {
            endpoint: endpoint.to_string(),
            source,
        })?;
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(ConfigClientError::ReadTxDiscard)?;
    if !status.is_success() {
        return Err(ConfigClientError::TxDiscardStatus {
            status,
            endpoint: endpoint.to_string(),
            body: String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    Ok(())
}

pub(super) fn affected_documents(response: &Value) -> u64 {
    response
        .get("data")
        .and_then(Value::as_object)
        .map(|fields| {
            fields
                .values()
                .map(|value| match value {
                    Value::Array(rows) => rows.len() as u64,
                    Value::Object(_) => 1,
                    _ => 0,
                })
                .sum()
        })
        .unwrap_or_default()
}

pub(super) fn graphql_response_has_errors(response: &Value) -> bool {
    match response.get("errors") {
        None => false,
        Some(Value::Array(errors)) => !errors.is_empty(),
        Some(_) => true,
    }
}

pub(super) fn ensure_mutation_document(document: &str) -> Result<()> {
    if document.trim_start().starts_with("mutation") {
        Ok(())
    } else {
        Err(ConfigClientError::MutationDocumentRequired)
    }
}

pub(crate) fn ensure_query_document(document: &str) -> Result<()> {
    let document = document.trim_start();
    if document.starts_with('{') || document.starts_with("query") {
        Ok(())
    } else {
        Err(ConfigClientError::QueryDocumentRequired)
    }
}

fn storage_full_response(bytes: &[u8]) -> Option<gents_protocol::database_error::StorageFull> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    gents_protocol::database_error::StorageFull::from_code(
        value.get("code").and_then(Value::as_str),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_commit_conflict_requires_the_pinned_error_body() {
        assert!(is_confirmed_commit_conflict(
            br#"{"error":"transaction conflict. Please retry","outcome":"transaction_conflict_aborted"}"#
        ));
        assert!(!is_confirmed_commit_conflict(
            br#"{"error":"transaction conflict. Please retry"}"#
        ));
        assert!(!is_confirmed_commit_conflict(
            br#"{"error":"unrelated transaction conflict; please retry"}"#
        ));
        assert!(!is_confirmed_commit_conflict(
            b"transaction conflict. Please retry"
        ));
        assert!(!is_confirmed_commit_conflict(
            br#"{"error":"transaction conflict. Please retry","outcome":"unknown"}"#
        ));
        assert!(!is_confirmed_commit_conflict(
            br#"{"error":"transaction conflict. please retry"}"#
        ));
    }

    #[tokio::test]
    async fn transaction_http_preserves_variables_and_transaction_header() {
        use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
        let expected =
            serde_json::json!({"input": {"a-b": [], "nested": [{"": {}, "雪": [null, []]}]}});
        let app =
            Router::new()
                .route(
                    "/graphql",
                    post(
                        |State(expected): State<Value>,
                         headers: HeaderMap,
                         Json(body): Json<Value>| async move {
                            assert_eq!(headers["x-defradb-tx"], "same-transaction");
                            assert_eq!(
                                body["query"],
                                "mutation($input: JSON) { test(input: $input) { _docID } }"
                            );
                            assert_eq!(body["variables"], expected);
                            Json(serde_json::json!({"data": {"test": [{"_docID": "written"}]}}))
                        },
                    ),
                )
                .with_state(expected.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/graphql", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = http_client().await.unwrap().clone();
        let result = txn_execute(
            &GraphqlEndpoint::anonymous(endpoint),
            "same-transaction",
            &client,
            "mutation($input: JSON) { test(input: $input) { _docID } }",
            &expected,
        )
        .await;
        server.abort();
        assert_eq!(result.unwrap()["data"]["test"][0]["_docID"], "written");
    }

    /// DefraDB binds a transaction to the DID, not to one token, so every
    /// request of a transaction carries a bearer current when it is sent: a
    /// bearer is reused only until half its lifetime has passed. With
    /// 2-second bearers, the statement sent 1.1 s after the begin carries a
    /// newer one.
    #[tokio::test]
    async fn http_transaction_renews_a_half_spent_bearer() {
        use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
        use base64::Engine as _;
        use std::sync::{Arc, Mutex};

        type Seen = Arc<Mutex<Vec<(&'static str, String)>>>;
        fn record(seen: &Seen, step: &'static str, headers: &HeaderMap) {
            let bearer = headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            seen.lock().unwrap().push((step, bearer));
        }
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route(
                "/api/v0/tx",
                post(|State(seen): State<Seen>, headers: HeaderMap| async move {
                    record(&seen, "begin", &headers);
                    Json(serde_json::json!({"id": 7}))
                }),
            )
            .route(
                "/api/v0/graphql",
                post(|State(seen): State<Seen>, headers: HeaderMap| async move {
                    record(&seen, "execute", &headers);
                    Json(serde_json::json!({"data": {"create_X": [{"_docID": "x"}]}}))
                }),
            )
            .route(
                "/api/v0/tx/{id}",
                post(|State(seen): State<Seen>, headers: HeaderMap| async move {
                    record(&seen, "commit", &headers);
                    ""
                }),
            )
            .with_state(Arc::clone(&seen));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let key = tempfile::tempdir().unwrap();
        let identity =
            crate::identity::KeyIdentity::load_or_create(key.path().join("principal.key"), None)
                .unwrap();
        let did = crate::identity::NodeIdentity::did(&identity).to_string();
        let capability = identity.client_signing_capability().unwrap();
        super::super::ConfigAccess::Graphql(
            super::super::GraphqlEndpoint::as_principal(endpoint, &did)
                .with_client_signing_capability(capability)
                .unwrap()
                .with_bearer_lifetime(std::time::Duration::from_secs(2)),
        )
        .transact_write("test.per_request_bearer", |txn| {
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
                txn.execute("mutation { create_X(input: {}) { _docID } }")
                    .await
            })
        })
        .await
        .unwrap();
        server.abort();

        let issued_at = |bearer: &str| -> u64 {
            let payload = bearer
                .strip_prefix("Bearer ")
                .and_then(|token| token.split('.').nth(1))
                .expect("a JWT bearer");
            let claims: Value = serde_json::from_slice(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(payload)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(claims["iss"], did.as_str());
            claims["iat"].as_u64().unwrap()
        };
        let seen = seen.lock().unwrap().clone();
        let steps: Vec<_> = seen.iter().map(|(step, _)| *step).collect();
        assert_eq!(steps, ["begin", "execute", "commit"]);
        assert!(issued_at(&seen[1].1) > issued_at(&seen[0].1), "{seen:?}");
        assert!(issued_at(&seen[2].1) >= issued_at(&seen[1].1), "{seen:?}");
    }

    #[tokio::test]
    async fn readonly_transaction_sends_defradb_read_only_begin_flag() {
        use axum::{extract::State, http::Uri, routing::post, Json, Router};
        use std::sync::{Arc, Mutex};

        let begins: Arc<Mutex<Vec<Option<String>>>> = Arc::default();
        let app = Router::new()
            .route(
                "/api/v0/tx",
                post(
                    |State(begins): State<Arc<Mutex<Vec<Option<String>>>>>, uri: Uri| async move {
                        begins
                            .lock()
                            .unwrap()
                            .push(uri.query().map(ToOwned::to_owned));
                        Json(serde_json::json!({"id": 17}))
                    },
                ),
            )
            .route(
                "/api/v0/graphql",
                post(|| async { Json(serde_json::json!({"data": {"Probe": [{"id": "x"}]}})) }),
            )
            .route("/api/v0/tx/{id}", post(|| async { "" }))
            .with_state(Arc::clone(&begins));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let result = super::super::ConfigAccess::graphql(endpoint.clone())
            .transact_readonly("test.http_readonly", |txn| {
                Box::pin(async move { txn.execute("query { Probe { id } }").await })
            })
            .await
            .unwrap();
        assert_eq!(result["data"]["Probe"][0]["id"], "x");
        super::super::ConfigAccess::graphql(endpoint)
            .transact_write("test.http_write", |txn| {
                Box::pin(async move {
                    txn.execute("mutation { create_Probe(input: {id: \"y\"}) { id } }")
                        .await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            begins.lock().unwrap().as_slice(),
            &[Some("read_only=true".to_owned()), None]
        );
        server.abort();
    }

    /// A loaded server may take any time to answer a commit; the client's
    /// verdict is the server's, not its own clock's
    /// (`tla/CommitResponseWait.tla`, `ClientWindow`). Once the connection is
    /// up the clock is paused, and a paused clock jumps to the next timer
    /// whenever both sides wait, so a client timer of any length would fire
    /// before the server's hour.
    #[tokio::test]
    async fn a_commit_the_server_answers_late_is_confirmed() {
        use axum::{
            routing::{get, post},
            Router,
        };

        let app = Router::new()
            .route("/api/v0/version", get(|| async { "" }))
            .route(
                "/api/v0/tx/{id}",
                post(|| async {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    ""
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = http_client().await.unwrap().clone();
        let version = endpoint.replace("/graphql", "/version");
        client.get(version).send().await.unwrap();
        tokio::time::pause();
        let started = tokio::time::Instant::now();
        let result = txn_commit(&GraphqlEndpoint::anonymous(endpoint), "17", &client).await;
        server.abort();
        if let Err(
            TxnCommitError::CleanupRequired(error)
            | TxnCommitError::TransactionConsumed(error)
            | TxnCommitError::ConfirmedConflict(error),
        ) = result
        {
            panic!("the server's commit is confirmed: {error:?}");
        }
        assert!(started.elapsed() >= std::time::Duration::from_secs(3600));
    }
}
