use std::time::Duration;

use anyhow::{bail, Context, Result};
use defra_agent_desktop_core::client::{
    ClientCore, ClientCoreOptions, DesktopPaths, SubmitRequestOptions,
};
use serde::Deserialize;
use tokio::time::{sleep, Instant};

#[path = "../../defra-agent/src/lean_vocab_test.rs"]
mod lean_vocab_test;

use lean_vocab_test::{assert_lean_transition_is_legal, lean_session_recovery_case};

#[derive(Debug, Deserialize)]
struct SessionRow {
    session_id: String,
    agent_name: String,
    behavior_id: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct ConversationRow {
    session_id: String,
    agent_name: String,
    agent_did: String,
    behavior_id: String,
    title: String,
    preview_text: String,
    status: String,
    latest_request_id: String,
}

#[derive(Debug, Deserialize)]
struct RequestRow {
    request_id: String,
    agent_did: String,
    behavior_id: String,
    session_id: String,
    content: String,
    status: String,
    lifecycle_state: String,
    execution_origin: String,
    retry_root_request: String,
    retry_parent_request: String,
    retry_count: i64,
    max_retries: i64,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_conversation_writes_session_and_conversation() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let created = core
        .create_conversation("did:defra:amy", Some("amy-code"))
        .await?;

    let session: SessionRow = query_single(
        core.node(),
        &format!(
            r#"{{
                AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    agent_name
                    behavior_id
                    status
                }}
            }}"#,
            created.session_id
        ),
        "AgentSession",
    )
    .await?;
    assert_eq!(session.session_id, created.session_id);
    assert_eq!(session.agent_name, "amy");
    assert_eq!(session.behavior_id, "amy-code");
    assert_eq!(session.status, "active");

    let conversation: ConversationRow = query_single(
        core.node(),
        &format!(
            r#"{{
                AgentConversation(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    agent_name
                    agent_did
                    behavior_id
                    title
                    preview_text
                    status
                    latest_request_id
                }}
            }}"#,
            created.session_id
        ),
        "AgentConversation",
    )
    .await?;
    assert_eq!(conversation.session_id, created.session_id);
    assert_eq!(conversation.agent_name, "amy");
    assert_eq!(conversation.agent_did, "did:defra:amy");
    assert_eq!(conversation.behavior_id, "amy-code");
    assert!(conversation.title.is_empty());
    assert!(conversation.preview_text.is_empty());
    assert_eq!(conversation.status, "active");
    assert!(conversation.latest_request_id.is_empty());
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submit_request_writes_request_and_updates_conversation_summary() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let created = core
        .create_conversation("did:defra:amy", Some("amy-code"))
        .await?;
    let submitted = core
        .submit_request(
            &created.session_id,
            "did:defra:amy",
            "  hello   there\noperator  ",
            None,
        )
        .await?;

    let request: RequestRow = query_single(
        core.node(),
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    request_id
                    agent_did
                    behavior_id
                    session_id
                    content
                    status
                    lifecycle_state
                    execution_origin
                    retry_root_request
                    retry_parent_request
                    retry_count
                    max_retries
                }}
            }}"#,
            submitted.request_id
        ),
        "AgentRequest",
    )
    .await?;
    assert_eq!(request.request_id, submitted.request_id);
    assert_eq!(request.agent_did, "did:defra:amy");
    assert_eq!(request.behavior_id, "amy-code");
    assert_eq!(request.session_id, created.session_id);
    assert_eq!(request.content, "hello   there\noperator");
    assert_eq!(request.status, "pending");
    assert_eq!(request.lifecycle_state, "pending");
    assert_eq!(request.execution_origin, "interactive");
    assert_eq!(request.retry_root_request, submitted.request_id);
    assert!(request.retry_parent_request.is_empty());
    assert_eq!(request.retry_count, 0);
    assert_eq!(request.max_retries, 3);

    let conversation: ConversationRow = query_single(
        core.node(),
        &format!(
            r#"{{
                AgentConversation(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    agent_name
                    agent_did
                    behavior_id
                    title
                    preview_text
                    status
                    latest_request_id
                }}
            }}"#,
            created.session_id
        ),
        "AgentConversation",
    )
    .await?;
    assert_eq!(conversation.title, "hello there operator");
    assert_eq!(conversation.preview_text, "hello there operator");
    assert_eq!(conversation.status, "active");
    assert_eq!(conversation.latest_request_id, submitted.request_id);
    assert_eq!(
        core.store().focused_request_id(),
        Some(submitted.request_id.clone())
    );
    assert_eq!(core.store().snapshot().requests.len(), 1);
    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_request_writes_retry_chain_and_updates_conversation_summary() -> Result<()> {
    let legal_case = lean_session_recovery_case("legal_open_budget_latest");
    assert!(legal_case.legal);
    assert_eq!(legal_case.action.as_str(), "reissueFailed");
    assert_lean_transition_is_legal(
        "SessionRecovery",
        &legal_case.pre_latest_state,
        &legal_case.post_latest_state,
    );

    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let created = core
        .create_conversation("did:defra:amy", Some("amy-code"))
        .await?;
    let original = core
        .submit_request(&created.session_id, "did:defra:amy", "first attempt", None)
        .await?;
    let parent = core
        .store()
        .snapshot()
        .requests
        .iter()
        .find(|row| row.request_id == original.request_id)
        .cloned()
        .context("expected submitted parent request in desktop store")?;
    let deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    force_retry_parent_state(
        core.node(),
        &original.request_id,
        legal_case.pre_retry_count as i64,
        legal_case.max_retries as i64,
        &deadline.to_rfc3339(),
    )
    .await?;
    let mut parent = parent;
    parent.status = Some("error".to_string());
    parent.lifecycle_state = Some(legal_case.pre_latest_state.clone());
    parent.deadline = Some(deadline.to_rfc3339());
    parent.retry_count = Some(legal_case.pre_retry_count as i64);
    parent.max_retries = Some(legal_case.max_retries as i64);

    let retried = core.retry_request(&parent).await?;

    let request: RequestRow = query_single(
        core.node(),
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    request_id
                    agent_did
                    behavior_id
                    session_id
                    content
                    status
                    lifecycle_state
                    execution_origin
                    retry_root_request
                    retry_parent_request
                    retry_count
                    max_retries
                }}
            }}"#,
            retried.request_id
        ),
        "AgentRequest",
    )
    .await?;
    assert_eq!(request.request_id, retried.request_id);
    assert_eq!(request.agent_did, "did:defra:amy");
    assert_eq!(request.behavior_id, "amy-code");
    assert_eq!(request.session_id, retried.session_id);
    assert_eq!(request.session_id, created.session_id);
    assert_eq!(request.content, "first attempt");
    assert_eq!(request.status, "pending");
    assert_eq!(
        request.lifecycle_state,
        legal_case.post_latest_state.as_str()
    );
    assert_eq!(request.execution_origin, "interactive");
    assert_eq!(request.retry_parent_request, original.request_id);
    assert_eq!(request.retry_root_request, original.request_id);
    assert_eq!(request.retry_count, legal_case.post_retry_count as i64);
    assert_eq!(request.max_retries, legal_case.max_retries as i64);

    let original_conversation: ConversationRow = query_single(
        core.node(),
        &format!(
            r#"{{
                AgentConversation(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    session_id
                    agent_name
                    agent_did
                    behavior_id
                    title
                    preview_text
                    status
                    latest_request_id
                }}
            }}"#,
            created.session_id
        ),
        "AgentConversation",
    )
    .await?;
    assert_eq!(original_conversation.preview_text, "first attempt");
    assert_eq!(original_conversation.status, "active");
    assert_eq!(original_conversation.latest_request_id, retried.request_id);
    assert_eq!(core.store().focused_request_id(), Some(retried.request_id));

    core.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_request_rejects_generated_illegal_session_recovery_cases() -> Result<()> {
    let source_not_failed = lean_session_recovery_case("illegal_source_not_failed");
    let budget_exhausted = lean_session_recovery_case("illegal_retry_budget_exhausted");
    let deadline_closed = lean_session_recovery_case("illegal_deadline_closed");
    let non_latest = lean_session_recovery_case("illegal_non_latest_failed_request");
    let duplicate_new_id = lean_session_recovery_case("illegal_new_request_id_already_exists");
    let source_not_released = lean_session_recovery_case("illegal_source_not_released");
    for case in [
        source_not_failed,
        budget_exhausted,
        deadline_closed,
        non_latest,
        duplicate_new_id,
        source_not_released,
    ] {
        assert!(!case.legal, "{} should be illegal", case.name.as_str());
    }
    assert!(
        duplicate_new_id.pre_new_request_exists,
        "Lean duplicate-new-id witness must start with the retry id already present"
    );
    assert_eq!(source_not_released.pre_latest_state.as_str(), "failed");
    assert_eq!(
        source_not_released.pre_failed_admission.as_str(),
        "waiting",
        "Lean source-not-released maps to a failed request that is not released for retry"
    );

    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let created = core
        .create_conversation("did:defra:amy", Some("amy-code"))
        .await?;
    let original = core
        .submit_request(&created.session_id, "did:defra:amy", "first attempt", None)
        .await?;
    let mut parent = core
        .store()
        .snapshot()
        .requests
        .iter()
        .find(|row| row.request_id == original.request_id)
        .cloned()
        .context("expected submitted parent request in desktop store")?;

    let err = core.retry_request(&parent).await.unwrap_err().to_string();
    assert!(
        err.contains("failed/error"),
        "source-not-failed guard should reject pending parent: {err}"
    );

    parent.status = Some("processing".to_string());
    parent.lifecycle_state = Some(source_not_released.pre_latest_state.clone());
    let err = core.retry_request(&parent).await.unwrap_err().to_string();
    assert!(
        err.contains("failed/error"),
        "source-not-released guard should reject non-terminal parent status: {err}"
    );

    let past_deadline = chrono::Utc::now() - chrono::Duration::seconds(1);
    force_retry_parent_state(
        core.node(),
        &original.request_id,
        deadline_closed.pre_retry_count as i64,
        deadline_closed.max_retries as i64,
        &past_deadline.to_rfc3339(),
    )
    .await?;
    parent.status = Some("error".to_string());
    parent.lifecycle_state = Some(deadline_closed.pre_latest_state.clone());
    parent.deadline = Some(past_deadline.to_rfc3339());
    parent.retry_count = Some(deadline_closed.pre_retry_count as i64);
    parent.max_retries = Some(deadline_closed.max_retries as i64);
    let err = core.retry_request(&parent).await.unwrap_err().to_string();
    assert!(
        err.contains("deadline is closed"),
        "deadline-closed guard should reject expired parent: {err}"
    );

    let future_deadline = chrono::Utc::now() + chrono::Duration::minutes(5);
    force_retry_parent_state(
        core.node(),
        &original.request_id,
        budget_exhausted.pre_retry_count as i64,
        budget_exhausted.max_retries as i64,
        &future_deadline.to_rfc3339(),
    )
    .await?;
    parent.deadline = Some(future_deadline.to_rfc3339());
    parent.retry_count = Some(budget_exhausted.pre_retry_count as i64);
    parent.max_retries = Some(budget_exhausted.max_retries as i64);
    let err = core.retry_request(&parent).await.unwrap_err().to_string();
    assert!(
        err.contains("exhausted retry budget"),
        "retry-budget guard should reject exhausted parent: {err}"
    );

    force_retry_parent_state(
        core.node(),
        &original.request_id,
        non_latest.pre_retry_count as i64,
        non_latest.max_retries as i64,
        &future_deadline.to_rfc3339(),
    )
    .await?;
    parent.retry_count = Some(non_latest.pre_retry_count as i64);
    parent.max_retries = Some(non_latest.max_retries as i64);
    let newer = core
        .submit_request(&created.session_id, "did:defra:amy", "newer attempt", None)
        .await?;
    let err = core.retry_request(&parent).await.unwrap_err().to_string();
    assert!(
        err.contains("must be latest"),
        "non-latest guard should reject stale parent after {} became latest: {err}",
        newer.request_id
    );

    core.shutdown().await?;
    Ok(())
}

/// Regression test for the "resend drops overrides" bug. Previously,
/// `fetch_request_view` only queried routing/content state fields, so
/// `resend_request` silently rebuilt the new row without the original sampling
/// overrides or metadata. Resend must preserve submitter intent across the
/// retry chain.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resend_preserves_request_overrides_and_metadata() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let created = core
        .create_conversation("did:defra:amy", Some("amy-code"))
        .await?;

    // Submit with explicit sampling overrides + metadata.
    let metadata_value = r#"{"key":"preserve-me"}"#.to_string();
    let options = SubmitRequestOptions {
        temperature: Some(0.7),
        top_p: Some(0.95),
        top_k: Some(40),
        max_tokens: Some(1234),
        metadata: Some(metadata_value.clone()),
        ..SubmitRequestOptions::default()
    };
    let original = core
        .submit_request_with_options(
            &created.session_id,
            "did:defra:amy",
            "please preserve my overrides",
            None,
            options,
        )
        .await?;

    // Force the row to a stale-terminal state (dead + Stale) directly via the
    // embedded node. `resend_request` only accepts rows in that exact shape.
    let force_stale = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{request_id}" }} }},
                input: {{ lifecycle_state: "dead", failure_reason: "Stale" }}
            ) {{ _docID }}
        }}"#,
        request_id = original.request_id,
    );
    let resp = core.node().execute(&force_stale).await;
    assert!(
        !resp.has_errors(),
        "forcing stale state failed: {:?}",
        resp.errors
    );

    // Resend — this is what we're testing.
    let resent = core.resend_request(&original.request_id).await?;

    // The new row must carry identical sampling overrides + metadata.
    let new_row: RequestWithOverridesRow = query_single(
        core.node(),
        &format!(
            r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    request_id
                    session_id
                    retry_parent_request
                    retry_root_request
                    temperature
                    top_p
                    top_k
                    max_tokens
                    metadata
                }}
            }}"#,
            resent.request_id
        ),
        "AgentRequest",
    )
    .await?;
    assert_eq!(new_row.request_id, resent.request_id);
    assert_eq!(new_row.session_id, resent.session_id);
    assert_ne!(new_row.session_id, original.session_id);
    assert_eq!(new_row.retry_parent_request, original.request_id);
    // Root should chain back to the original (this was the root of its own chain).
    assert_eq!(new_row.retry_root_request, original.request_id);
    assert_eq!(new_row.temperature, Some(0.7));
    assert_eq!(new_row.top_p, Some(0.95));
    assert_eq!(new_row.top_k, Some(40));
    assert_eq!(new_row.max_tokens, Some(1234));
    assert_eq!(new_row.metadata.as_deref(), Some(metadata_value.as_str()));

    core.shutdown().await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RequestWithOverridesRow {
    request_id: String,
    session_id: String,
    retry_parent_request: String,
    retry_root_request: String,
    temperature: Option<f64>,
    top_p: Option<f64>,
    top_k: Option<i64>,
    max_tokens: Option<i64>,
    metadata: Option<String>,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_and_remove_peer_persists_peer_directory() -> Result<()> {
    let tempdir_a = tempfile::tempdir()?;
    let tempdir_b = tempfile::tempdir()?;
    let remote = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir_a.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;
    let local = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir_b.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let addr = wait_for_connectable_iroh_addr(&remote).await?;
    let added = local
        .add_peer("Workshop Bay", &addr, "did:defra:workshop", None)
        .await?;

    assert_eq!(added.label, "Workshop Bay");
    assert_eq!(added.addr, addr);
    assert!(added.connected);
    assert_eq!(local.configured_peer_count(), 1);
    assert_eq!(local.dialed_peer_count(), 1);
    assert_eq!(local.peer_records().await.len(), 1);

    let removed = local.remove_peer(&added.peer_id).await?;

    assert_eq!(removed.peer_id, added.peer_id);
    assert!(removed.warning.is_some());
    assert!(local.peer_records().await.is_empty());
    assert_eq!(local.configured_peer_count(), 0);
    local.shutdown().await?;
    remote.shutdown().await?;
    Ok(())
}

async fn query_single<T>(node: &defra_node::EmbeddedNode, query: &str, root: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de>,
{
    let response = node.execute(query).await;
    if response.has_errors() {
        bail!(
            "query {root} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get(root))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .with_context(|| format!("missing row for {root}"))?;
    Ok(serde_json::from_value(row)?)
}

async fn force_retry_parent_state(
    node: &defra_node::EmbeddedNode,
    request_id: &str,
    retry_count: i64,
    max_retries: i64,
    deadline: &str,
) -> Result<()> {
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_deadline = escape_graphql_string(deadline);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{
                    status: "error",
                    lifecycle_state: "failed",
                    retry_count: {retry_count},
                    max_retries: {max_retries},
                    deadline: "{escaped_deadline}"
                }}
            ) {{ _docID }}
        }}"#
    );
    let response = node.execute(&mutation).await;
    if response.has_errors() {
        bail!(
            "force retry parent state failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(())
}

fn escape_graphql_string(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

async fn wait_for_connectable_iroh_addr(core: &ClientCore) -> Result<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let addrs = core.p2p().listen_addresses().await?;
        if let Some(addr) = addrs
            .into_iter()
            .find(|addr| addr.contains("/p2p/") || addr.starts_with("endpoint"))
        {
            return Ok(addr);
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for desktop listen address");
        }
        sleep(Duration::from_millis(100)).await;
    }
}
