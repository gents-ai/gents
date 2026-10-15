//! Shared canonical admission fixtures for tool-call transition tests.
//!
//! These fixtures factor out the request construction previously duplicated
//! by `delivery.rs::spawned_background_tests` (`claimed_request`,
//! `published_spawn_parent`, `published_background_bridge`). They drive only
//! real runtime surfaces — a claimed `RequestLifecycle` (real
//! `claim()`/`begin_owned_execution`), a `DefraStreamWriter` native turn
//! publication (`publish_native_turn`) — and build tool-call lifecycles with `ToolCallLifecycle::from_accepted`
//! from the publication's accepted calls. No hand-built `AcceptedToolCall`
//! values, headers, or fixture policy machines.
//!
//! This module is `cfg(test)`-only shared scaffolding: it defines no public
//! production API and no alternate policy. It is wired by the root owner as
//! `#[cfg(test)] mod admission_fixture;` inside `tool_call_lifecycle.rs`;
//! tests then use `crate::tool_call_lifecycle::admission_fixture::{...}`.
//!
//! ## Replacement handoff (for the root integration owner)
//!
//! When moving low-level lifecycle tests into their owner modules, replace
//! the local helpers with this module's exports and delete the originals.
//! The exact call shapes (physical request/tool/native IDs, canonical
//! publication arguments) are preserved, so assertions need no rewrites.
//!
//! Legacy constructors in `delivery.rs::spawned_background_tests` and their
//! intended replacements:
//!
//! | Legacy local helper | Replacement |
//! |---------------------|-------------|
//! | `claimed_request(node, request_id, session_id, node_did)` | [`claimed_request`] (same signature) |
//! | `published_spawn_parent(name)` | [`published_spawn_parent`] (same return shape; same defaults: `AwaitMode::Foreground`, `start_running()` invoked) |
//! | `published_background_bridge(name)` | [`published_background_bridge`] (a running background `agent_new` row with a real `KeyIdentity` (`test-agent.key`)) |
//!
//! Many conformance files also use these constructor shapes; they must be
//! migrated to this shared fixture by their own owners. Deletion of the
//! legacy copies above happens only after the root wires this module and
//! those migrations land.

use std::{path::PathBuf, sync::Arc, time::Duration};

use defra_node::EmbeddedNode;

use crate::identity::NodeIdentity;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::streaming::DefraStreamWriter;
use crate::tool_call_lifecycle::{AwaitMode, ToolCallLifecycle};

/// Options for the published admission fixtures.
///
/// Every field default matches the legacy `delivery.rs` constructors so
/// existing assertions keep their exact physical IDs and policies.
pub struct PublishedAdmissionOptions {
    /// Logical name seeded into the physical `request-{name}` /
    /// `session-{name}` IDs (preserving the legacy `claimed_request` shape).
    pub name: String,
    /// When true, the fixture loads or creates a real
    /// `KeyIdentity::load_or_create(<node data path>/test-agent.key, None)`
    /// (the legacy `published_background_bridge` shape) and uses its DID for
    /// the request, writer, plan target, and tool-call lifecycle. When false,
    /// the fixture pins the legacy literal `"did:test:test"`.
    pub real_identity: bool,
    /// Awaiting mode handed to `ToolCallLifecycle::from_accepted`. Only a
    /// `agent_new`/`agent_message` call publishes in background.
    pub await_mode: AwaitMode,
    /// Whether the fixture starts the tool call running before returning.
    /// When false, the returned lifecycle is left in the canonical `Pending`
    /// state after `from_accepted`, so tests can drive their own
    /// `start_running()` transition themselves.
    pub start_running: bool,
    /// Optional immutable request creation time for modeled historical-head
    /// fixtures. The default keeps the ordinary real-time admission shape.
    pub request_created_at: Option<String>,
    /// Native tool name; `None` publishes `SPAWN_PROCESS_TOOL_NAME`. Only a
    /// `agent_new`/`agent_message` name publishes in background.
    pub tool_name: Option<String>,
    /// With `real_identity`, the calling request is an enrolled request of a
    /// paired client DID, in a session that client owns.
    pub paired_client: bool,
}

impl Default for PublishedAdmissionOptions {
    fn default() -> Self {
        Self {
            name: String::new(),
            real_identity: false,
            await_mode: AwaitMode::Foreground,
            start_running: true,
            request_created_at: None,
            tool_name: None,
            paired_client: false,
        }
    }
}

/// Publish one accepted native tool call onto an existing claimed request.
/// Test-only callers use this when a conformance premise needs multiple
/// accepted calls on one physical parent; it delegates all header, segment,
/// and tool-row creation to the production publication owner.
pub(crate) async fn publish_accepted_on_claimed_request(
    node: Arc<EmbeddedNode>,
    request: &mut RequestLifecycle,
    node_did: &str,
    turn: usize,
    tool_name: &str,
    tool_call_id: &str,
    arguments: serde_json::Value,
    await_mode: AwaitMode,
    start_running: bool,
) -> anyhow::Result<ToolCallLifecycle> {
    let writer = DefraStreamWriter::new(node.clone(), node_did, Duration::from_millis(1));
    if turn == 0 {
        request.begin_owned_execution(&writer).await?;
    } else {
        request.validate_owned_execution().await?;
    }
    writer
        .start_provider_attempt(
            &request.request().doc_id,
            turn,
            0,
            format!("inference.{}", turn + 1).parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        id: Some(format!("fixture-provider-message-{turn}")),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                id: tool_call_id.to_owned(),
                call_id: Some(format!("fixture-provider-call-{turn}")),
                function: gents_protocol::message::ToolFunction::new(
                    tool_name.to_owned(),
                    arguments,
                ),
                signature: None,
                additional_params: None,
            },
        )],
    };
    let mut published = writer
        .publish_native_turn(request, turn, 0, &message)
        .await?;
    anyhow::ensure!(
        published.accepted_tools.len() == 1,
        "fixture publication must accept exactly one tool call"
    );
    let accepted = published.accepted_tools.pop().unwrap();
    let deadline = request
        .claimed_deadline_at()
        .ok_or_else(|| anyhow::anyhow!("claimed fixture request has no deadline"))?;
    let mut tool = ToolCallLifecycle::from_accepted(
        node,
        node_did.to_owned(),
        request.request().requester_did.clone(),
        accepted,
        deadline,
        await_mode,
    )?;
    if start_running {
        tool.start_running().await?;
    }
    Ok(tool)
}

/// Physical artifacts of one published admission, produced by
/// [`published_admission`].
pub struct PublishedAdmission {
    /// Embedded node holding the durable documents.
    pub node: Arc<EmbeddedNode>,
    /// Tempdir backing the node (and, with `real_identity`, the node key).
    /// The caller owns cleanup (`std::fs::remove_dir_all(path)` after
    /// `node.shutdown().await`); the fixture retains it so tests can inspect
    /// or migrate files first.
    pub path: PathBuf,
    /// Tool-call lifecycle built via `from_accepted` from the canonical
    /// publication's accepted call (no hand-built state).
    pub tool: ToolCallLifecycle,
    /// Node DID actually used (derived from a real `KeyIdentity` when
    /// `real_identity`, else the legacy `"did:test:test"` literal).
    pub node_did: String,
}

pub(crate) async fn complete_child(
    node: &Arc<EmbeddedNode>,
    child_id: &str,
    node_did: &str,
    text: &str,
) {
    let child_id_escaped = crate::graphql::escape_graphql_string(child_id);
    let response = node.execute(&format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_id_escaped}" }} }}) {{ {} }} }}"#,
        crate::watcher::AGENT_REQUEST_FIELDS,
    )).await;
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&response, "AgentRequest")
            .unwrap()
            .unwrap();
    let mut request = RequestLifecycle::new_with_node_did(
        node.clone(),
        "general",
        node_did,
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(request.claim().await.unwrap(), ClaimOutcome::Claimed);
    let writer = DefraStreamWriter::new(node.clone(), node_did, Duration::ZERO);
    request.begin_owned_execution(&writer).await.unwrap();
    writer
        .start_provider_attempt(
            &request.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    let message = gents_protocol::message::Message::Assistant {
        id: Some(format!("answer-{child_id}")),
        content: vec![gents_protocol::message::AssistantContent::Text(
            gents_protocol::message::Text {
                text: text.to_owned(),
            },
        )],
    };
    let published = writer
        .publish_native_turn(&request, 0, 0, &message)
        .await
        .unwrap();
    let terminal = request
        .terminalize_owned(
            crate::lifecycle::RequestTerminalOutcome::Completed,
            gents_protocol::output::TerminalOutput::Message {
                message_doc_id: published.message_doc_id,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(terminal, crate::lifecycle::TerminalizeResult::Won);
}

async fn ensure_fixture_session(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    created_at: &str,
) {
    let session = crate::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{session}" }} }}, limit: 2) {{ _docID node_did requester_did }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let rows = response.data.unwrap()["AgentSession"]
        .as_array()
        .unwrap()
        .clone();
    assert!(rows.len() <= 1, "fixture session identity must be unique");
    if let Some(row) = rows.first() {
        assert_eq!(row["node_did"].as_str(), Some(node_did));
        assert_eq!(row["requester_did"].as_str(), requester_did);
        return;
    }
    let agent = crate::graphql::escape_graphql_string(node_did);
    let requester = requester_did.map_or_else(
        || "null".to_owned(),
        |did| format!("\"{}\"", crate::graphql::escape_graphql_string(did)),
    );
    let created = crate::graphql::escape_graphql_string(created_at);
    let response = node.execute(&format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "{session}", node_did: "{agent}", requester_did: {requester}, agent_id: "general", created_at: "{created}" }}) {{ _docID }} }}"#)).await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
}

/// A claimed `RequestLifecycle` on a real durable AgentRequest document.
///
/// Creates the AgentRequest row through DefraDB, loads it with the canonical
/// `AgentRequestRow`, and runs the real `RequestLifecycle::claim()` path
/// (`ClaimOutcome::Claimed`). This is the generalized
/// `spawned_background_tests::claimed_request` replacement.
pub async fn claimed_request(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    node_did: &str,
) -> RequestLifecycle {
    let now = chrono::Utc::now().to_rfc3339();
    ensure_fixture_session(node, session_id, node_did, None, &now).await;
    let now = crate::graphql::escape_graphql_string(&now);
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let node_did = crate::graphql::escape_graphql_string(node_did);
    let created = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", purpose: "normal", node_did: "{node_did}", agent_id: "general", session_id: "{session_id}", retry_parent_request: "", retry_root_request: "{request_id}", superseded_by_request: "", content: "spawn", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, request_hop: 0 }}) {{ _docID }} }}"#)).await;
    assert!(!created.has_errors(), "{:#?}", created.errors);
    let row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow = crate::graphql::first_row(&row, "AgentRequest")
        .unwrap()
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        "general",
        &node_did,
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

pub(crate) async fn claimed_signed_request(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    identity: &dyn NodeIdentity,
    created_at: Option<&str>,
) -> RequestLifecycle {
    claimed_signed_request_with_trigger(node, request_id, session_id, identity, created_at, None)
        .await
}

/// [`claimed_signed_request`] started by the scheduled trigger `trigger_id`.
pub(crate) async fn claimed_signed_request_with_trigger(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    identity: &dyn NodeIdentity,
    created_at: Option<&str>,
    trigger_id: Option<&str>,
) -> RequestLifecycle {
    let now = created_at
        .map(str::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    ensure_fixture_session(node, session_id, identity.did(), Some(identity.did()), &now).await;
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        identity.did(),
        identity.did(),
        "general",
        session_id,
        "spawn",
        "interactive",
        now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(identity.did()),
    );
    if let Some(trigger_id) = trigger_id {
        create.caused_by_trigger_id = Some(trigger_id.to_owned());
        create.caused_by_trigger_kind = Some("schedule".to_owned());
    }
    crate::sign_agent_request_create(identity, &mut create)
        .await
        .expect("sign canonical admission fixture request");
    let created = node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!created.has_errors(), "{:#?}", created.errors);
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow = crate::graphql::first_row(&row, "AgentRequest")
        .unwrap()
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        "general",
        identity.did(),
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

/// A claimed enrolled request of a paired client (`paired-client.key`) in a
/// session that client owns on `agent`.
async fn claimed_paired_client_request(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    session_id: &str,
    agent: &dyn NodeIdentity,
    path: &std::path::Path,
) -> RequestLifecycle {
    let client = crate::KeyIdentity::load_or_create(path.join("paired-client.key"), None)
        .expect("paired client identity");
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    ensure_fixture_session(node, session_id, agent.did(), Some(client.did()), &now).await;
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        gents_protocol::request_admission::RequestPurpose::Normal,
        request_id,
        agent.did(),
        client.did(),
        "general",
        session_id,
        "spawn",
        "interactive",
        now,
        gents_protocol::request_admission::AgentRequestAdmissionRecord::enrollment(
            client.did(),
            "enrollment",
            "digest",
            agent.did(),
            1,
            "2099-01-01T00:00:00Z",
        ),
    );
    crate::sign_agent_request_create(&client, &mut create)
        .await
        .expect("sign paired client request");
    let created = node.execute(&create.graphql_mutation().unwrap()).await;
    assert!(!created.has_errors(), "{:#?}", created.errors);
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    let row: gents_protocol::row::AgentRequestRow = crate::graphql::first_row(&row, "AgentRequest")
        .unwrap()
        .unwrap();
    let mut lifecycle = RequestLifecycle::new_with_node_did(
        node.clone(),
        "general",
        agent.did(),
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(lifecycle.claim().await.unwrap(), ClaimOutcome::Claimed);
    lifecycle
}

/// Generalized publication fixture.
///
/// Starts an embedded node under a fresh tempdir with runtime schemas, claims
/// a request, begins owned execution with a real `DefraStreamWriter`, opens a
/// provider attempt, publishes one canonical native turn, and builds the tool-call
/// lifecycle via `ToolCallLifecycle::from_accepted` from the publication's
/// single accepted call. When `options.start_running` is false the lifecycle
/// is returned in canonical `Pending` state so callers can drive the
/// `start_running` transition themselves.
///
/// This is the general form of the legacy `published_spawn_parent` and
/// `published_background_bridge` constructors. It is the default shared
/// fixture for tool-call admission transition tests; use it instead of
/// duplicating node/request/writer/publication setup in owner modules.
///
/// On success the caller owns teardown: `node.shutdown().await` then
/// `std::fs::remove_dir_all(path)` (the tempdir is retained, matching legacy
/// behavior, so tests can inspect or migrate files first).
pub async fn published_admission(
    options: PublishedAdmissionOptions,
) -> anyhow::Result<PublishedAdmission> {
    let (admission, _owner) = published_admission_with_owner(options).await?;
    Ok(admission)
}

/// Keep the actual claimed owner alive when a test exercises its subsequent
/// terminal transition. The ordinary fixture deliberately relinquishes it.
pub async fn published_admission_with_owner(
    options: PublishedAdmissionOptions,
) -> anyhow::Result<(PublishedAdmission, RequestLifecycle)> {
    let name = options.name;
    let path =
        std::env::temp_dir().join(format!("admission-fixture-{name}-{}", uuid::Uuid::new_v4()));
    let node = Arc::new(EmbeddedNode::builder().data_path(&path).build().await?);
    crate::schema::ensure_runtime_schemas(&node).await?;
    let identity = if options.real_identity {
        Some(crate::KeyIdentity::load_or_create(
            path.join("test-agent.key"),
            None,
        )?)
    } else {
        None
    };
    let node_did = identity.as_ref().map_or_else(
        || "did:test:test".to_owned(),
        |identity| identity.did().to_owned(),
    );
    let mut request = match identity.as_ref() {
        Some(identity) if options.paired_client => {
            claimed_paired_client_request(
                &node,
                &format!("request-{name}"),
                &format!("session-{name}"),
                identity,
                &path,
            )
            .await
        }
        Some(identity) => {
            claimed_signed_request(
                &node,
                &format!("request-{name}"),
                &format!("session-{name}"),
                identity,
                options.request_created_at.as_deref(),
            )
            .await
        }
        None => {
            claimed_request(
                &node,
                &format!("request-{name}"),
                &format!("session-{name}"),
                &node_did,
            )
            .await
        }
    };
    let writer = DefraStreamWriter::new(node.clone(), &node_did, Duration::from_millis(1));
    request.begin_owned_execution(&writer).await?;
    writer
        .start_provider_attempt(
            &request.request().doc_id,
            0,
            0,
            "inference.1".parse().unwrap(),
        )
        .await;
    // One canonical assistant message carrying the tool call; the physical
    // tool/native IDs match the legacy fixtures.
    let tool_name = options
        .tool_name
        .clone()
        .unwrap_or_else(|| crate::toolset::SPAWN_PROCESS_TOOL_NAME.into());
    let (native_id, arguments) = if crate::toolset::is_session_message_tool(&tool_name) {
        (
            "bridge-native-tool",
            serde_json::json!({"agent": "general", "prompt": "work"}),
        )
    } else {
        ("spawn-native-tool", serde_json::json!({"command": "work"}))
    };
    let message = gents_protocol::message::Message::Assistant {
        id: Some("spawn-provider-message".into()),
        content: vec![gents_protocol::message::AssistantContent::ToolCall(
            gents_protocol::message::ToolCall {
                id: native_id.into(),
                call_id: Some("spawn-provider-call".into()),
                function: gents_protocol::message::ToolFunction::new(tool_name, arguments),
                signature: None,
                additional_params: None,
            },
        )],
    };
    let mut published = writer.publish_native_turn(&request, 0, 0, &message).await?;
    let accepted = published
        .accepted_tools
        .pop()
        .expect("canonical publication accepted the tool call");
    let deadline = request
        .claimed_deadline_at()
        .expect("claimed request deadline");
    let mut tool = ToolCallLifecycle::from_accepted(
        node.clone(),
        node_did.clone(),
        request.request().requester_did.clone(),
        accepted,
        deadline,
        options.await_mode,
    )?;
    if options.start_running {
        tool.start_running().await?;
    }
    Ok((
        PublishedAdmission {
            node,
            path,
            tool,
            node_did,
        },
        request,
    ))
}

/// Native `spawn_process` parent fixture — the exact replacement for
/// `spawned_background_tests::published_spawn_parent`, preserving its
/// physical IDs, `AwaitMode::Foreground`, and `start_running`.
pub async fn published_spawn_parent(name: &str) -> (Arc<EmbeddedNode>, PathBuf, ToolCallLifecycle) {
    let PublishedAdmission {
        node, path, tool, ..
    } = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        ..Default::default()
    })
    .await
    .expect("publish canonical native tool admission");
    (node, path, tool)
}

/// A running background `agent_new` row with a real `KeyIdentity`
/// (`test-agent.key`), before its request is materialized.
pub async fn published_background_bridge(
    name: &str,
) -> (Arc<EmbeddedNode>, PathBuf, ToolCallLifecycle) {
    let PublishedAdmission {
        node, path, tool, ..
    } = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        tool_name: Some(crate::toolset::AGENT_NEW_TOOL_NAME.to_owned()),
        ..Default::default()
    })
    .await
    .expect("publish canonical background agent_new admission");
    (node, path, tool)
}

/// Dispatch a pending accepted `agent_new` row and materialize its
/// request on `target_node_did`'s `general` agent through the
/// session-message owner, publishing the row's receipt.
pub(crate) async fn materialize_session_message(
    node: &Arc<EmbeddedNode>,
    request: &RequestLifecycle,
    tool: &mut ToolCallLifecycle,
    target_node_did: &str,
    prompt: &str,
) -> anyhow::Result<crate::session_message::SessionMessageReceipt> {
    let caller = request.request();
    let cause = crate::lifecycle::SessionMessageCause {
        caller_node_did: caller.node_did.clone(),
        caller_request_id: caller.request_id.clone(),
        caller_request_doc_id: caller.doc_id.clone(),
        caller_hop: caller.request_hop,
        tool_call_id: tool.tool_call_id().to_owned(),
        tool_call_doc_id: tool
            .doc_id()
            .expect("accepted session-message row")
            .to_owned(),
        correlation: None,
    };
    let target = crate::lifecycle::SessionMessageTarget {
        node_did: target_node_did.to_owned(),
        agent_id: "general".to_owned(),
        session_id: uuid::Uuid::new_v4().to_string(),
    };
    let plan = crate::session_message::plan(
        node,
        &cause,
        &target,
        crate::session_message::RenderedBody {
            content: prompt.to_owned(),
            goal: None,
        },
        None,
        false,
    )
    .await?
    .map_err(anyhow::Error::msg)?;
    crate::session_message::commit(node, &cause, tool, plan, false).await
}

/// An accepted `agent_new` call and the request it caused.
pub struct PublishedSessionMessage {
    pub admission: PublishedAdmission,
    pub caused_request_id: String,
    pub caused_request_doc_id: String,
}

/// Publish an accepted background `agent_new` call on this node's
/// `general` agent and materialize its caused request, with the row's
/// receipt, through the session-message owner (`lifecycle::materialize`).
pub async fn published_session_message(
    options: PublishedAdmissionOptions,
) -> anyhow::Result<PublishedSessionMessage> {
    Ok(published_session_message_with_owner(options).await?.0)
}

/// [`published_session_message`], keeping the calling request's owner alive
/// for later turns on the same request.
pub async fn published_session_message_with_owner(
    options: PublishedAdmissionOptions,
) -> anyhow::Result<(PublishedSessionMessage, RequestLifecycle)> {
    anyhow::ensure!(
        options.real_identity && options.await_mode == AwaitMode::Background,
        "a session message is signed by a real node and runs in background"
    );
    let (mut admission, request) = published_admission_with_owner(PublishedAdmissionOptions {
        tool_name: Some(crate::toolset::AGENT_NEW_TOOL_NAME.to_owned()),
        start_running: false,
        ..options
    })
    .await?;
    let receipt = materialize_session_message(
        &admission.node,
        &request,
        &mut admission.tool,
        &admission.node_did.clone(),
        "work",
    )
    .await?;
    Ok((
        PublishedSessionMessage {
            admission,
            caused_request_id: receipt.request_id,
            caused_request_doc_id: receipt.request_doc_id,
        },
        request,
    ))
}

/// A `Pending` native parent — `from_accepted` without `start_running` — for
/// tests that drive the pending→running transition themselves. Same physical
/// IDs and defaults as [`published_spawn_parent`] otherwise.
pub async fn pending_spawn_parent(name: &str) -> (Arc<EmbeddedNode>, PathBuf, ToolCallLifecycle) {
    let PublishedAdmission {
        node, path, tool, ..
    } = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        start_running: false,
        ..Default::default()
    })
    .await
    .expect("publish pending canonical native tool admission");
    (node, path, tool)
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::tool_call_lifecycle::{CancelCause, FailureClass};

    async fn row(node: &EmbeddedNode, session_id: &str) -> serde_json::Value {
        let session = crate::graphql::escape_graphql_string(session_id);
        let response = node
            .execute(&format!(r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ request_id request_doc_id lifecycle_state tool_failure_class cancel_cause deadline_at await_mode }} }}"#))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        let data = response.data.unwrap();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "exactly one admitted physical tool row");
        rows.first().cloned().expect("one admitted tool call")
    }

    async fn teardown(node: Arc<EmbeddedNode>, path: PathBuf) {
        node.shutdown().await;
        std::fs::remove_dir_all(path).unwrap();
    }

    #[tokio::test]
    async fn admitted_native_lifecycle_persists_completed_terminal() {
        let (node, path, mut tool) = pending_spawn_parent("native-complete").await;
        let request_doc_id = tool.request_doc_id().unwrap().to_owned();
        tool.start_running().await.unwrap();
        let running = row(&node, "session-native-complete").await;
        assert_eq!(running["lifecycle_state"], "running");
        assert_eq!(running["request_doc_id"], request_doc_id);
        assert!(running["deadline_at"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        tool.complete("ok").await.unwrap();
        assert_eq!(
            row(&node, "session-native-complete").await["lifecycle_state"],
            "completed"
        );
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_lifecycle_persists_failure_class() {
        let (node, path, mut tool) = published_spawn_parent("native-fail").await;
        tool.fail("error", FailureClass::ToolReturnedError)
            .await
            .unwrap();
        let row = row(&node, "session-native-fail").await;
        assert_eq!(row["lifecycle_state"], "failed");
        assert_eq!(row["tool_failure_class"], "toolReturnedError");
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_terminal_is_irreversible() {
        let (node, path, mut tool) = published_spawn_parent("native-terminal").await;
        tool.complete("done").await.unwrap();
        let error = tool.fail("late", FailureClass::External).await.unwrap_err();
        assert!(format!("{error:#}").contains("illegal tool call transition"));
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_rejects_duplicate_dispatch_without_duplicate_row() {
        let (node, path, mut tool) = published_spawn_parent("native-duplicate").await;
        let error = tool.start_running().await.unwrap_err();
        assert!(format!("{error:#}").contains("cannot re-dispatch"));
        assert_eq!(
            row(&node, "session-native-duplicate").await["lifecycle_state"],
            "running"
        );
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_load_preserves_state_deadline_and_terminal_updates() {
        let (node, path, tool) = published_spawn_parent("native-load").await;
        let expected_deadline = tool.deadline_at;
        let mut loaded =
            ToolCallLifecycle::load(node.clone(), "session-native-load", "spawn-native-tool")
                .await
                .unwrap()
                .expect("admitted tool loads");
        loaded.timeout().await.unwrap();
        let row = row(&node, "session-native-load").await;
        assert_eq!(row["lifecycle_state"], "timedOut");
        assert_eq!(row["cancel_cause"], "deadline");
        let observed = chrono::DateTime::parse_from_rfc3339(row["deadline_at"].as_str().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(observed, expected_deadline);
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_load_returns_persisted_failure() {
        let (node, path, mut tool) = published_spawn_parent("native-load-failure").await;
        tool.fail("oops", FailureClass::Transport).await.unwrap();
        drop(tool);
        let loaded = ToolCallLifecycle::load(
            node.clone(),
            "session-native-load-failure",
            "spawn-native-tool",
        )
        .await
        .unwrap()
        .expect("terminal admitted tool loads");
        drop(loaded);
        let row = row(&node, "session-native-load-failure").await;
        assert_eq!(row["lifecycle_state"], "failed");
        assert_eq!(row["tool_failure_class"], "transport");
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_native_cancel_during_run_and_after_load_persist_causes() {
        let (node, path, mut tool) = published_spawn_parent("native-cancel").await;
        tool.cancel_during_run(CancelCause::UserCancelled)
            .await
            .unwrap();
        let persisted = row(&node, "session-native-cancel").await;
        assert_eq!(persisted["lifecycle_state"], "cancelled");
        assert_eq!(persisted["cancel_cause"], "userCancelled");
        teardown(node, path).await;

        let (node, path, tool) = published_spawn_parent("native-cancel-load").await;
        assert!(row(&node, "session-native-cancel-load").await["cancel_cause"].is_null());
        drop(tool);
        let mut loaded = ToolCallLifecycle::load(
            node.clone(),
            "session-native-cancel-load",
            "spawn-native-tool",
        )
        .await
        .unwrap()
        .unwrap();
        loaded
            .cancel_during_run(CancelCause::Interrupted)
            .await
            .unwrap();
        assert_eq!(
            row(&node, "session-native-cancel-load").await["cancel_cause"],
            "interrupted"
        );
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_pending_native_cancel_persists_cause() {
        let (node, path, mut tool) = pending_spawn_parent("native-pending-cancel").await;
        tool.cancel_before_dispatch(CancelCause::Interrupted)
            .await
            .unwrap();
        let row = row(&node, "session-native-pending-cancel").await;
        assert_eq!(row["lifecycle_state"], "cancelled");
        assert_eq!(row["cancel_cause"], "interrupted");
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn timeout_adopts_terminal_written_by_terminal_parent_sweep() {
        let (node, path, mut tool) = published_spawn_parent("timeout-cas").await;
        let request = crate::graphql::escape_graphql_string("request-timeout-cas");
        let response = node
            .execute(&format!(r#"mutation {{ update_AgentRequest(filter: {{ request_id: {{ _eq: "{request}" }} }}, input: {{ lifecycle_state: "interrupted" }}) {{ _docID }} }}"#))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);

        let report =
            ToolCallLifecycle::reconcile_terminal_parent_owned_tools(&node, "did:test:test")
                .await
                .unwrap();
        assert_eq!(report.tool_calls_terminalized, 1);
        assert!(!tool.timeout().await.unwrap());
        let row = row(&node, "session-timeout-cas").await;
        assert_eq!(row["lifecycle_state"], "cancelled");
        assert_eq!(row["cancel_cause"], "interrupted");
        assert!(row["tool_failure_class"].is_null());
        teardown(node, path).await;
    }
}
