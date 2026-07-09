//! Conformance for replicated request-state convergence (#664), mirroring the
//! `ReplicatedRequestConvergence.tla` model:
//!
//! - `SingleClaimer` (SAFETY): a non-owning peer never claims a foreign replica.
//!   Fences the `agent_did` filter on both watcher claim seams
//!   (`try_fetch_request`, the `pending_requests` scan reached via
//!   `next_request`).
//! - `TerminalConverges` (LIVENESS): once the owner terminalizes, its owner-side
//!   re-drive re-asserts the terminal state so a lagging replica converges. The
//!   re-drive is owner-scoped, idempotent, and bounded.
//!
//! Plus the recovery-drift fix: `recover_stuck_requests` keys the stale set on
//! `lifecycle_state ∈ {claimed, processing}` (the Lean `requestRecoveryStale`
//! predicate) rather than on `status = "processing"`.

use super::*;

use defra_agent::__test_internals::{
    drain_automated_wakeups, reconcile_coalesced_pending_request, QueueSource,
};
use defra_agent::{DefraWatcher, Watcher, TERMINAL_REDRIVE_CAP};

const CONVERGENCE_CREATED_AT: &str = "2026-03-23T00:00:00Z";
const OWNER_DID: &str = AGENT_DID;
const FOREIGN_DID: &str = "did:defra-agent:foreign-owner";

#[derive(Debug, Deserialize)]
struct ConvergenceRow {
    status: String,
    lifecycle_state: String,
    agent_did: String,
}

/// Create an `AgentRequest` owned by an arbitrary DID with explicit
/// `status`/`lifecycle_state`. Unlike the shared `create_request` helper this
/// lets a test seed a *foreign* replica (non-owning DID) and an arbitrary
/// terminal shape, which the #664 scenarios require.
async fn create_owned_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    status: &str,
    lifecycle_state: &str,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_status = escape_graphql_string(status);
    let escaped_lifecycle_state = escape_graphql_string(lifecycle_state);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "hello",
                status: "{escaped_status}",
                lifecycle_state: "{escaped_lifecycle_state}",
                backend_id: "",
                execution_origin: "interactive",
                created_at: "{CONVERGENCE_CREATED_AT}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create owned request failed: {:?}",
        resp.errors
    );

    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    first_row::<support::DocIdRow>(&node.execute(&query).await, "AgentRequest").doc_id
}

#[derive(Debug, Deserialize)]
struct QueueConvergenceRow {
    status: String,
    lifecycle_state: String,
    agent_did: String,
    superseded_by_request: Option<String>,
}

/// Background-completion coalesce metadata (`is_automated_wakeup`-shaped) keyed
/// on the session, matching what `enqueue_session_request` writes for an
/// automated wake-up.
fn coalesce_wakeup_metadata(session_id: &str) -> String {
    format!(
        r#"{{"queue":{{"source":"background_completion","policy":"coalesce","key":"background_completion:{session_id}","queued_after_request_id":null}}}}"#
    )
}

/// Seed a raw `pending`/`pending` queue request owned by `agent_did` with the
/// given metadata and execution origin. Unlike `create_owned_request` this
/// populates `metadata` + `execution_origin` so the coalesce/drain predicates
/// (`queue_source_and_key_match`, `is_automated_wakeup`) actually match.
async fn create_queue_request(
    node: &EmbeddedNode,
    request_id: &str,
    session_id: &str,
    agent_did: &str,
    execution_origin: &str,
    metadata: &str,
    created_at: &str,
) -> String {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_session_id = escape_graphql_string(session_id);
    let escaped_agent_did = escape_graphql_string(agent_did);
    let escaped_execution_origin = escape_graphql_string(execution_origin);
    let escaped_metadata = escape_graphql_string(metadata);
    let escaped_created_at = escape_graphql_string(created_at);
    let mutation = format!(
        r#"mutation {{
            create_AgentRequest(input: {{
                request_id: "{escaped_request_id}",
                agent_did: "{escaped_agent_did}",
                behavior_id: "{AGENT_NAME}",
                session_id: "{escaped_session_id}",
                retry_parent_request: "",
                retry_root_request: "{escaped_request_id}",
                superseded_by_request: "",
                content: "wake up",
                metadata: "{escaped_metadata}",
                status: "pending",
                lifecycle_state: "pending",
                backend_id: "",
                execution_origin: "{escaped_execution_origin}",
                created_at: "{escaped_created_at}",
                retry_count: 0,
                max_retries: {max_retries}
            }}) {{ _docID }}
        }}"#,
        max_retries = defra_agent::lifecycle::DEFAULT_REQUEST_MAX_RETRIES,
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "create queue request failed: {:?}",
        resp.errors
    );

    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                _docID
            }}
        }}"#
    );
    first_row::<support::DocIdRow>(&node.execute(&query).await, "AgentRequest").doc_id
}

async fn fetch_queue_convergence_row(node: &EmbeddedNode, request_id: &str) -> QueueConvergenceRow {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                status
                lifecycle_state
                agent_did
                superseded_by_request
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentRequest")
}

async fn fetch_convergence_row(node: &EmbeddedNode, request_id: &str) -> ConvergenceRow {
    let escaped_request_id = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}, limit: 1) {{
                status
                lifecycle_state
                agent_did
            }}
        }}"#
    );
    first_row(&node.execute(&query).await, "AgentRequest")
}

/// SAFETY `SingleClaimer`: the watcher never surfaces a foreign-DID replica as
/// claimable — not via the doc-targeted `try_fetch_request` seam, nor via the
/// `pending_requests` scan reached through `next_request`. Non-vacuous: an
/// own-DID pending request IS claimable through the same seam.
pub(super) async fn single_claimer_watcher_never_claims_foreign_replica() {
    let db = test_db("convergence-single-claimer").await;

    // A foreign replica: owner's DID differs from the runtime's own DID, exactly
    // the subagent-host replication shape from the #661 incident.
    let foreign_doc = create_owned_request(
        &db.node,
        "convergence-foreign-req",
        "convergence-foreign-session",
        FOREIGN_DID,
        "pending",
        "pending",
    )
    .await;

    let mut watcher = DefraWatcher::new(db.node.clone(), OWNER_DID);

    // Seam 1 (doc-targeted claim): a foreign replica is never fetched.
    assert!(
        watcher
            .try_fetch_request(&foreign_doc)
            .await
            .unwrap()
            .is_none(),
        "foreign replica must not be claimable via try_fetch_request"
    );

    // Seam 2 (pending scan via next_request): with only the foreign replica
    // present, the watcher must block rather than yield it. A timeout is the
    // observable "never claims" signal.
    let scanned = tokio::time::timeout(Duration::from_millis(750), watcher.next_request()).await;
    assert!(
        scanned.is_err(),
        "watcher scan (next_request) must never yield a foreign replica, got {scanned:?}"
    );

    // Non-vacuity: an OWN pending request IS claimable through the same seam, so
    // the assertions above are not passing merely because nothing is claimable.
    let own_doc = create_owned_request(
        &db.node,
        "convergence-own-req",
        "convergence-own-session",
        OWNER_DID,
        "pending",
        "pending",
    )
    .await;
    let claimed = watcher.try_fetch_request(&own_doc).await.unwrap();
    let claimed = claimed.expect("own pending request must be claimable (guards a vacuous filter)");
    assert_eq!(
        claimed.agent_did, OWNER_DID,
        "the claimable request must be the owner's own"
    );
}

/// LIVENESS `TerminalConverges`: after the owner terminalizes, the owner-side
/// re-drive re-asserts the terminal state so a lagging replica converges. Here
/// the re-assert is the observable owner action (the delivered delta in the
/// model). It is owner-scoped (a foreign terminal replica is never re-driven by
/// us), targets only terminal rows (an active row is skipped), idempotent (the
/// value is unchanged), and bounded (each row self-drops after `CAP` re-asserts).
pub(super) async fn terminal_convergence_redrive_reasserts_unconverged_terminal() {
    let db = test_db("convergence-terminal-redrive").await;

    // The owner reached terminal `failed` on its own request.
    create_owned_request(
        &db.node,
        "convergence-owned-failed",
        "convergence-owned-failed-session",
        OWNER_DID,
        "error",
        "failed",
    )
    .await;
    // A foreign terminal replica — owner-scope: we must NOT re-drive it.
    create_owned_request(
        &db.node,
        "convergence-foreign-failed",
        "convergence-foreign-failed-session",
        FOREIGN_DID,
        "error",
        "failed",
    )
    .await;
    // An own NON-terminal (processing) request — must NOT be re-driven.
    create_owned_request(
        &db.node,
        "convergence-owned-processing",
        "convergence-owned-processing-session",
        OWNER_DID,
        "processing",
        "processing",
    )
    .await;

    let mut budget = HashMap::new();

    // First pass re-asserts exactly the one owned, terminal, unconverged request.
    let first = RequestLifecycle::redrive_terminal_convergence(&db.node, OWNER_DID, &mut budget)
        .await
        .unwrap();
    assert_eq!(
        first.scanned, 1,
        "only the owned terminal request is a candidate (foreign + active excluded)"
    );
    assert_eq!(
        first.reasserted, 1,
        "owner must re-assert its one unconverged terminal request"
    );
    assert!(!first.is_noop());

    // Idempotent: the re-assert leaves the terminal value unchanged.
    let owned = fetch_convergence_row(&db.node, "convergence-owned-failed").await;
    assert_eq!(owned.status, "error");
    assert_eq!(owned.lifecycle_state, "failed");
    // The foreign replica is untouched by our re-drive.
    let foreign = fetch_convergence_row(&db.node, "convergence-foreign-failed").await;
    assert_eq!(foreign.agent_did, FOREIGN_DID);
    assert_eq!(foreign.status, "error");

    // Bounded: the request self-terminates from the re-drive after CAP asserts.
    for _ in 1..TERMINAL_REDRIVE_CAP {
        let more = RequestLifecycle::redrive_terminal_convergence(&db.node, OWNER_DID, &mut budget)
            .await
            .unwrap();
        assert_eq!(more.reasserted, 1, "each re-assert under the cap counts");
    }
    let exhausted =
        RequestLifecycle::redrive_terminal_convergence(&db.node, OWNER_DID, &mut budget)
            .await
            .unwrap();
    assert!(
        exhausted.is_noop(),
        "re-drive must self-terminate after {TERMINAL_REDRIVE_CAP} re-asserts, got {exhausted:?}"
    );

    // Owner-scope is did-keyed, not vacuous: driving as the FOREIGN owner picks
    // up the foreign terminal replica (and only it).
    let mut foreign_budget = HashMap::new();
    let foreign_run =
        RequestLifecycle::redrive_terminal_convergence(&db.node, FOREIGN_DID, &mut foreign_budget)
            .await
            .unwrap();
    assert_eq!(
        foreign_run.scanned, 1,
        "the foreign owner's own terminal replica is its sole candidate"
    );
    assert_eq!(foreign_run.reasserted, 1);
}

/// Recovery-drift fix: `recover_stuck_requests` keys the stale set on
/// `lifecycle_state ∈ {claimed, processing}` (Lean `requestRecoveryStale`), so a
/// stuck `claimed` own-request is recovered. This row carries `status="claimed"`
/// (not `"processing"`), so the pre-fix `status="processing"` filter would have
/// missed it — the concrete red-then-green witness for the drift fix.
pub(super) async fn recover_stuck_requests_recovers_claimed_lifecycle_state() {
    let db = test_db("convergence-recover-claimed").await;

    create_owned_request(
        &db.node,
        "convergence-stuck-claimed",
        "convergence-stuck-claimed-session",
        OWNER_DID,
        "claimed",
        "claimed",
    )
    .await;

    let report = RequestLifecycle::recover_all(&db.node, OWNER_DID)
        .await
        .unwrap();
    assert_eq!(
        report.requests_recovered, 1,
        "a claimed own-request must be recovered (Lean requestRecoveryStale = claimed ∨ processing)"
    );

    let row = fetch_convergence_row(&db.node, "convergence-stuck-claimed").await;
    assert!(
        matches!(row.lifecycle_state.as_str(), "failed" | "completed"),
        "recovered request must be terminal, got {}",
        row.lifecycle_state
    );
}

/// SAFETY (#664, coalesce supersede seam): `reconcile_coalesced_pending_request`
/// scopes both its candidate query and its supersede mutation to the owning
/// `agent_did`. A foreign-DID replica sharing the same `session_id` and coalesce
/// key (the P2P subagent-host replication shape) is never superseded by the
/// owner's reconcile, while the owner's own duplicate IS superseded (non-vacuous).
pub(super) async fn reconcile_coalesce_never_supersedes_foreign_replica() {
    let db = test_db("convergence-coalesce-foreign").await;
    let session_id = "convergence-coalesce-foreign-session";
    let metadata = coalesce_wakeup_metadata(session_id);
    let key = format!("background_completion:{session_id}");

    // Two OWNER coalesce-duplicate pending rows (survivor = earliest created_at).
    create_queue_request(
        &db.node,
        "convergence-coalesce-owner-survivor",
        session_id,
        OWNER_DID,
        "scheduled",
        &metadata,
        "2026-03-23T00:00:00Z",
    )
    .await;
    create_queue_request(
        &db.node,
        "convergence-coalesce-owner-duplicate",
        session_id,
        OWNER_DID,
        "scheduled",
        &metadata,
        "2026-03-23T00:00:01Z",
    )
    .await;
    // A FOREIGN-DID replica in the same session with the same coalesce key.
    create_queue_request(
        &db.node,
        "convergence-coalesce-foreign",
        session_id,
        FOREIGN_DID,
        "scheduled",
        &metadata,
        "2026-03-23T00:00:02Z",
    )
    .await;

    let survivor = reconcile_coalesced_pending_request(
        &db.node,
        session_id,
        OWNER_DID,
        QueueSource::BackgroundCompletion,
        &key,
    )
    .await
    .unwrap()
    .expect("owner survivor");
    assert_eq!(
        survivor.request_id, "convergence-coalesce-owner-survivor",
        "the earliest owner row is the coalesce survivor"
    );

    // Owner survivor stays pending.
    let owner_survivor =
        fetch_queue_convergence_row(&db.node, "convergence-coalesce-owner-survivor").await;
    assert_eq!(owner_survivor.status, "pending");
    assert_eq!(owner_survivor.lifecycle_state, "pending");

    // Owner duplicate IS superseded — the non-vacuity witness that reconcile
    // supersedes owner-owned duplicates through this same seam.
    let owner_duplicate =
        fetch_queue_convergence_row(&db.node, "convergence-coalesce-owner-duplicate").await;
    assert_eq!(owner_duplicate.status, "superseded");
    assert_eq!(owner_duplicate.lifecycle_state, "superseded");
    assert_eq!(
        owner_duplicate.superseded_by_request.as_deref(),
        Some("convergence-coalesce-owner-survivor"),
    );

    // The foreign replica is untouched: still pending/pending, still foreign.
    let foreign = fetch_queue_convergence_row(&db.node, "convergence-coalesce-foreign").await;
    assert_eq!(
        foreign.agent_did, FOREIGN_DID,
        "foreign replica ownership unchanged"
    );
    assert_eq!(
        foreign.status, "pending",
        "foreign replica must not be superseded by the owner's coalesce reconcile"
    );
    assert_eq!(foreign.lifecycle_state, "pending");
    assert_eq!(
        foreign.superseded_by_request.as_deref().unwrap_or(""),
        "",
        "foreign replica must carry no supersede pointer"
    );
}

/// SAFETY (#664, wake-up drain seam): `drain_automated_wakeups` scopes both its
/// pending scan and its interrupt mutation to the owning `agent_did`. A
/// foreign-DID automated-wakeup replica sharing the session is left untouched
/// while the owner's own wake-up is interrupted (non-vacuous).
pub(super) async fn drain_wakeups_never_interrupts_foreign_replica() {
    let db = test_db("convergence-drain-foreign").await;
    let session_id = "convergence-drain-foreign-session";
    let metadata = coalesce_wakeup_metadata(session_id);

    // OWNER automated-wakeup pending row.
    create_queue_request(
        &db.node,
        "convergence-drain-owner",
        session_id,
        OWNER_DID,
        "scheduled",
        &metadata,
        "2026-03-23T00:00:00Z",
    )
    .await;
    // FOREIGN-DID automated-wakeup replica in the same session.
    create_queue_request(
        &db.node,
        "convergence-drain-foreign",
        session_id,
        FOREIGN_DID,
        "scheduled",
        &metadata,
        "2026-03-23T00:00:01Z",
    )
    .await;

    let drained = drain_automated_wakeups(
        &db.node,
        session_id,
        OWNER_DID,
        "automated wake-up drained because active request was interrupted",
    )
    .await
    .unwrap();
    assert_eq!(
        drained, 1,
        "exactly the owner's own automated wake-up is drained"
    );

    // Owner row is interrupted.
    let owner = fetch_queue_convergence_row(&db.node, "convergence-drain-owner").await;
    assert_eq!(owner.status, "interrupted");
    assert_eq!(owner.lifecycle_state, "interrupted");

    // Foreign replica is untouched.
    let foreign = fetch_queue_convergence_row(&db.node, "convergence-drain-foreign").await;
    assert_eq!(foreign.agent_did, FOREIGN_DID);
    assert_eq!(
        foreign.status, "pending",
        "foreign replica must not be interrupted by the owner's wake-up drain"
    );
    assert_eq!(foreign.lifecycle_state, "pending");
}
