use gents_desktop_core::client::{ClientCore, EnrollmentRequestResult};
use gents_desktop_core::local_runtime::fetch_runtime_connection_payload;
use gents_protocol::serve_lifecycle::{outdated_runtime_message, ObservedServeLifecycle};
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
pub(crate) struct ManagedPairingTarget {
    pub(crate) node_name: String,
    pub(crate) node_did: String,
    pub(crate) graphql: String,
}

pub(crate) const MANAGED_PAIRING_ATTEMPTS: u32 = 4;
pub(crate) const MANAGED_PAIRING_RETRY_DELAY: Duration = Duration::from_secs(2);
pub(crate) const MANAGED_PAIRING_APPROVAL_WINDOW: Duration = Duration::from_secs(30);
pub(crate) const MANAGED_PAIRING_POLL_INTERVAL: Duration = Duration::from_millis(250);
pub(crate) const MANAGED_PAIRING_CANCELLED: &str = "managed runtime pairing was cancelled";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PairingFailure {
    Cancelled,
    Permanent(String),
    Transient(String),
}

impl std::fmt::Display for PairingFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str(MANAGED_PAIRING_CANCELLED),
            Self::Permanent(message) | Self::Transient(message) => f.write_str(message),
        }
    }
}

pub(crate) async fn retry_managed_pairing<A, AF>(
    cancel: &tokio_util::sync::CancellationToken,
    attempts: u32,
    delay: Duration,
    mut attempt: A,
) -> Result<(), PairingFailure>
where
    A: FnMut() -> AF,
    AF: Future<Output = Result<(), PairingFailure>>,
{
    let mut failure = String::new();
    for number in 1..=attempts {
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(_) if cancel.is_cancelled() => return Err(PairingFailure::Cancelled),
            Err(PairingFailure::Transient(error)) => failure = error,
            Err(terminal) => return Err(terminal),
        }
        if number == attempts {
            break;
        }
        tracing::info!(
            target: "gents_desktop::managed_server",
            attempt = number,
            attempts,
            error = %failure,
            "managed runtime pairing attempt failed; retrying"
        );
        tokio::select! {
            () = tokio::time::sleep(delay * number) => {}
            () = cancel.cancelled() => return Err(PairingFailure::Cancelled),
        }
    }
    Err(PairingFailure::Transient(format!(
        "{failure} (after {attempts} attempts)"
    )))
}

pub(crate) enum PairingApproval {
    Committed,
    NotVisibleYet,
}

pub(crate) async fn grant_then_approve_managed<G, GF, A, AF>(
    grant: G,
    approve: A,
) -> Result<PairingApproval, String>
where
    G: FnOnce() -> GF,
    GF: Future<Output = Result<bool, String>>,
    A: FnOnce() -> AF,
    AF: Future<Output = Result<PairingApproval, String>>,
{
    if !grant().await? {
        return Ok(PairingApproval::NotVisibleYet);
    }
    approve().await
}

pub(crate) async fn await_managed_pairing_approval<P, PF, A, AF, R, RF>(
    cancel: &tokio_util::sync::CancellationToken,
    window: Duration,
    interval: Duration,
    mut paired: P,
    mut approve: A,
    mut repair: R,
) -> Result<(), PairingFailure>
where
    P: FnMut() -> PF,
    PF: Future<Output = bool>,
    A: FnMut() -> AF,
    AF: Future<Output = Result<PairingApproval, String>>,
    R: FnMut() -> RF,
    RF: Future<Output = Result<(), String>>,
{
    let deadline = tokio::time::Instant::now() + window;
    let mut approval_committed = false;
    loop {
        if cancel.is_cancelled() {
            return Err(PairingFailure::Cancelled);
        }
        if paired().await {
            return Ok(());
        }
        if !approval_committed {
            match approve().await.map_err(PairingFailure::Transient)? {
                PairingApproval::Committed => approval_committed = true,
                PairingApproval::NotVisibleYet => {}
            }
        }
        repair().await.map_err(PairingFailure::Transient)?;
        if tokio::time::Instant::now() >= deadline {
            return Err(PairingFailure::Transient(
                "timed out waiting for managed runtime pairing".to_string(),
            ));
        }
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = cancel.cancelled() => return Err(PairingFailure::Cancelled),
        }
    }
}

pub(crate) const MANAGED_STATUS_READY_WINDOW: Duration = Duration::from_secs(30);

/// The runtime's own `/status` lifecycle types the one permanent refusal here:
/// an outdated runtime never starts accepting this app until it restarts.
pub(crate) async fn fetch_managed_runtime_status(
    graphql: &str,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<serde_json::Value, PairingFailure> {
    let mut status_url = reqwest::Url::parse(graphql).map_err(|error| {
        PairingFailure::Transient(format!(
            "loading managed runtime enrollment offer: parsing managed runtime GraphQL URL: {error}"
        ))
    })?;
    status_url.set_path("/status");
    status_url.set_query(None);
    status_url.set_fragment(None);
    let deadline = tokio::time::Instant::now() + MANAGED_STATUS_READY_WINDOW;
    loop {
        let fetched = tokio::select! {
            fetched = fetch_runtime_connection_payload(status_url.as_str(), None) => {
                fetched.map_err(|error| PairingFailure::Transient(format!(
                    "loading managed runtime enrollment offer: {error:#}"
                )))
            }
            () = cancel.cancelled() => return Err(PairingFailure::Cancelled),
        };
        let observed = fetched.as_ref().ok().map(ObservedServeLifecycle::observe);
        if let Some(ObservedServeLifecycle::Outdated { version }) = observed {
            return Err(PairingFailure::Permanent(format!(
                "loading managed runtime enrollment offer: {}",
                outdated_runtime_message(version.as_deref())
            )));
        }
        if observed.is_some_and(|observed| observed.is_ready())
            || tokio::time::Instant::now() >= deadline
        {
            return fetched;
        }
        tokio::select! {
            () = tokio::time::sleep(Duration::from_millis(500)) => {}
            () = cancel.cancelled() => return Err(PairingFailure::Cancelled),
        }
    }
}

pub(crate) async fn ensure_managed_runtime_pairing(
    core: Arc<ClientCore>,
    node_home: &std::path::Path,
    target: &ManagedPairingTarget,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), PairingFailure> {
    if core.peer_records().await.iter().any(|peer| {
        peer.node_did == target.node_did
            && peer.is_enrollment()
            && peer.is_managed_runtime()
            && peer.is_enrollment_ready_at(chrono::Utc::now())
    }) {
        return Ok(());
    }

    let status = fetch_managed_runtime_status(&target.graphql, cancel).await?;
    if cancel.is_cancelled() {
        return Err(PairingFailure::Cancelled);
    }
    let live_peer = live_server_peer(&status);
    let request_id = resolve_managed_request(
        cancel,
        || async {
            core.active_status_enrollment_requests()
                .await
                .map(|requests| {
                    select_managed_request(requests, &target.node_did, live_peer.as_deref())
                })
                .map_err(|error| format!("{error:#}"))
        },
        |request_id| {
            let core = Arc::clone(&core);
            async move {
                core.resend_status_enrollment(&request_id)
                    .await
                    .map_err(|error| format!("{error:#}"))
            }
        },
        || async {
            core.request_status_enrollment_with_label(&status, Some(&target.node_name))
                .await
                .map(|request| request.request_id)
                .map_err(|error| {
                    PairingFailure::Transient(format!(
                        "requesting managed runtime enrollment: {error:#}"
                    ))
                })
        },
    )
    .await?;

    await_managed_pairing_approval(
        cancel,
        MANAGED_PAIRING_APPROVAL_WINDOW,
        MANAGED_PAIRING_POLL_INTERVAL,
        || async {
            core.peer_records().await.iter().any(|peer| {
                peer.node_did == target.node_did
                    && peer.is_enrollment()
                    && peer.is_managed_runtime()
                    && peer.is_enrollment_ready_at(chrono::Utc::now())
            })
        },
        || async {
            grant_then_approve_managed(
                || async {
                    gents_service::server_host::grant_managed_client_enrollment(
                        node_home,
                        &target.graphql,
                        &request_id,
                        &target.node_did,
                        core.node_identity().did(),
                    )
                    .await
                    .map_err(|error| format!("granting desktop User access to managed Node: {error:#}"))
                },
                || async {
                    match gents_service::server_host::admit_managed_client_enrollment(
                        node_home,
                        &target.graphql,
                        &request_id,
                        &target.node_did,
                        core.node_identity().did(),
                    )
                    .await
                    {
                        Ok(true) => Ok(PairingApproval::Committed),
                        Ok(false) => Ok(PairingApproval::NotVisibleYet),
                        Err(error) => {
                            let message = format!("{error:#}");
                            if enrollment_request_is_not_visible_yet(&error) {
                                tracing::debug!(
                                    request_id = %request_id,
                                    "managed enrollment request is not yet visible to operator approval"
                                );
                                Ok(PairingApproval::NotVisibleYet)
                            } else {
                                Err(format!("approving managed runtime enrollment: {message}"))
                            }
                        }
                    }
                },
            )
            .await
        },
        || async {
            core.request_p2p_repair()
                .await
                .map_err(|error| format!("requesting managed route reconciliation: {error:#}"))
        },
    )
    .await?;
    tracing::info!(
        target: "gents_desktop::managed_server",
        node_did = %target.node_did,
        "managed runtime desktop pairing is ready"
    );
    Ok(())
}

pub(crate) fn live_server_peer(status: &serde_json::Value) -> Option<String> {
    status
        .pointer("/enrollment/token")
        .and_then(serde_json::Value::as_str)
        .and_then(|token| gents_protocol::enrollment::decode_offer(token).ok())
        .map(|offer| offer.server_peer)
        .or_else(|| {
            status
                .get("p2p_peer_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .filter(|peer| !peer.trim().is_empty())
}

pub(crate) fn select_managed_request(
    requests: Vec<EnrollmentRequestResult>,
    node_did: &str,
    live_peer: Option<&str>,
) -> Option<EnrollmentRequestResult> {
    let live_peer = live_peer?;
    requests
        .into_iter()
        .filter(|request| request.owner_node == node_did && request.server_peer == live_peer)
        .max_by(|left, right| {
            left.expires_at
                .cmp(&right.expires_at)
                .then_with(|| left.request_id.cmp(&right.request_id))
        })
}

pub(crate) async fn resolve_managed_request<L, LF, R, RF, A, AF>(
    cancel: &tokio_util::sync::CancellationToken,
    lookup: L,
    resend: R,
    author: A,
) -> Result<String, PairingFailure>
where
    L: FnOnce() -> LF,
    LF: Future<Output = Result<Option<EnrollmentRequestResult>, String>>,
    R: FnOnce(String) -> RF,
    RF: Future<Output = Result<(), String>>,
    A: FnOnce() -> AF,
    AF: Future<Output = Result<String, PairingFailure>>,
{
    let found = lookup().await;
    if cancel.is_cancelled() {
        return Err(PairingFailure::Cancelled);
    }
    match found {
        Err(error) => Err(PairingFailure::Transient(format!(
            "reading this desktop's enrollment requests: {error}"
        ))),
        Ok(None) => author().await,
        Ok(Some(request)) if request.state == "pending_approval" => {
            resend(request.request_id.clone()).await.map_err(|error| {
                PairingFailure::Transient(format!("resending managed runtime enrollment: {error}"))
            })?;
            Ok(request.request_id)
        }
        Ok(Some(_)) => author().await,
    }
}

pub(crate) fn enrollment_request_is_not_visible_yet(error: &anyhow::Error) -> bool {
    gents_service::enrollment_admin::EnrollmentDecisionRejected::in_chain(error.as_ref())
        .is_some_and(|rejected| {
            rejected.code
                == Some(gents_protocol::enrollment::EnrollmentDecisionRefusalCode::NotPending)
        })
}
