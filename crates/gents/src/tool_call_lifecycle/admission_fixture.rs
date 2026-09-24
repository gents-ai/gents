//! Shared canonical admission fixtures for tool-call transition tests.
//!
//! These fixtures factor out the request construction previously duplicated
//! by `delivery.rs::spawned_background_tests` (`claimed_request`,
//! `published_spawn_parent`, `published_background_bridge`). They drive only
//! real runtime surfaces — a claimed `RequestLifecycle` (real
//! `claim()`/`begin_owned_execution`), a `DefraStreamWriter` native turn
//! publication (`publish_native_turn` / `publish_native_turn_with_spawn_admissions`)
//! — and build tool-call lifecycles with `ToolCallLifecycle::from_accepted`
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
//! | `claimed_request(node, request_id, session_id, agent_did)` | [`claimed_request`] (same signature) |
//! | `published_spawn_parent(name)` | [`published_spawn_parent`] (same return shape; same defaults: `AwaitMode::Foreground`, `CancelPolicy::Cascade`, `start_running()` invoked) |
//! | `published_background_bridge(name)` | [`published_background_bridge`] (same return shape: shared tempdir for node data path and `test-agent.key`; same defaults: `AwaitMode::Background`, `CancelPolicy::Cascade`, `start_running()` invoked) |
//!
//! Many conformance files also use these constructor shapes; they must be
//! migrated to this shared fixture by their own owners. Deletion of the
//! legacy copies above happens only after the root wires this module and
//! those migrations land.

use std::{path::PathBuf, sync::Arc, time::Duration};

use defra_node::EmbeddedNode;

use crate::identity::AgentIdentity;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::streaming::DefraStreamWriter;
use crate::tool_call_lifecycle::{AwaitMode, CancelPolicy, ToolCallLifecycle};

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
    /// Awaiting mode handed to `ToolCallLifecycle::from_accepted`.
    pub await_mode: AwaitMode,
    /// Cancellation policy handed to `ToolCallLifecycle::from_accepted`.
    pub cancel_policy: CancelPolicy,
    /// Whether the fixture starts the tool call running before returning.
    /// When false, the returned lifecycle is left in the canonical `Pending`
    /// state after `from_accepted`, so tests can drive their own
    /// `start_running()` transition themselves.
    pub start_running: bool,
    /// Optional immutable request creation time for modeled historical-head
    /// fixtures. The default keeps the ordinary real-time admission shape.
    pub request_created_at: Option<String>,
    /// Subagent spawn admission plan. When `Some`, the fixture routes through
    /// `publish_native_turn_with_spawn_admissions` (the
    /// `SPAWN_SUBAGENT_TOOL_NAME` bridge path) and `plan.spawn_target_did` is
    /// overridden with the fixture's actual agent DID (matching legacy);
    /// when `None`, through the plain native `SPAWN_PROCESS_TOOL_NAME`
    /// parent path.
    pub spawn_plan: Option<crate::streaming::SpawnAdmissionPlan>,
    /// Native tool name published without a spawn plan; `None` publishes
    /// `SPAWN_PROCESS_TOOL_NAME`.
    pub tool_name: Option<String>,
}

impl Default for PublishedAdmissionOptions {
    fn default() -> Self {
        Self {
            name: String::new(),
            real_identity: false,
            await_mode: AwaitMode::Foreground,
            cancel_policy: CancelPolicy::Cascade,
            start_running: true,
            request_created_at: None,
            spawn_plan: None,
            tool_name: None,
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
    agent_did: &str,
    turn: usize,
    tool_name: &str,
    tool_call_id: &str,
    arguments: serde_json::Value,
    spawn_plan: Option<crate::streaming::SpawnAdmissionPlan>,
    await_mode: AwaitMode,
    cancel_policy: CancelPolicy,
    start_running: bool,
) -> anyhow::Result<ToolCallLifecycle> {
    if let Some(plan) = spawn_plan.as_ref() {
        anyhow::ensure!(
            plan.tool_call_id == tool_call_id,
            "spawn admission plan tool_call_id must match the published tool call"
        );
    }
    let writer = DefraStreamWriter::new(node.clone(), agent_did, Duration::from_millis(1));
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
    let mut published = if let Some(plan) = spawn_plan {
        writer
            .publish_native_turn_with_spawn_admissions(request, turn, 0, &message, &[plan])
            .await?
    } else {
        writer
            .publish_native_turn(request, turn, 0, &message)
            .await?
    };
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
        agent_did.to_owned(),
        request.request().requester_did.clone(),
        accepted,
        deadline,
        await_mode,
        cancel_policy,
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
    /// Tempdir backing the node (and, with `real_identity`, the agent key).
    /// The caller owns cleanup (`std::fs::remove_dir_all(path)` after
    /// `node.shutdown().await`); the fixture retains it so tests can inspect
    /// or migrate files first.
    pub path: PathBuf,
    /// Tool-call lifecycle built via `from_accepted` from the canonical
    /// publication's accepted call (no hand-built state).
    pub tool: ToolCallLifecycle,
    /// Agent DID actually used (derived from a real `KeyIdentity` when
    /// `real_identity`, else the legacy `"did:test:test"` literal).
    pub agent_did: String,
}

pub(crate) async fn complete_child(
    node: &Arc<EmbeddedNode>,
    child_id: &str,
    agent_did: &str,
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
    let mut request = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        agent_did,
        row.try_into().unwrap(),
        60,
    );
    assert_eq!(request.claim().await.unwrap(), ClaimOutcome::Claimed);
    let writer = DefraStreamWriter::new(node.clone(), agent_did, Duration::ZERO);
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
    agent_did: &str,
    requester_did: Option<&str>,
    created_at: &str,
) {
    let session = crate::graphql::escape_graphql_string(session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{session}" }} }}, limit: 2) {{ _docID agent_did requester_did }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:#?}", response.errors);
    let rows = response.data.unwrap()["AgentSession"]
        .as_array()
        .unwrap()
        .clone();
    assert!(rows.len() <= 1, "fixture session identity must be unique");
    if let Some(row) = rows.first() {
        assert_eq!(row["agent_did"].as_str(), Some(agent_did));
        assert_eq!(row["requester_did"].as_str(), requester_did);
        return;
    }
    let agent = crate::graphql::escape_graphql_string(agent_did);
    let requester = requester_did.map_or_else(
        || "null".to_owned(),
        |did| format!("\"{}\"", crate::graphql::escape_graphql_string(did)),
    );
    let created = crate::graphql::escape_graphql_string(created_at);
    let response = node.execute(&format!(r#"mutation {{ create_AgentSession(input: {{ session_id: "{session}", agent_did: "{agent}", requester_did: {requester}, behavior_id: "general", created_at: "{created}" }}) {{ _docID }} }}"#)).await;
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
    agent_did: &str,
) -> RequestLifecycle {
    let now = chrono::Utc::now().to_rfc3339();
    ensure_fixture_session(node, session_id, agent_did, None, &now).await;
    let now = crate::graphql::escape_graphql_string(&now);
    let request_id = crate::graphql::escape_graphql_string(request_id);
    let session_id = crate::graphql::escape_graphql_string(session_id);
    let agent_did = crate::graphql::escape_graphql_string(agent_did);
    let created = node.execute(&format!(r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request_id}", purpose: "normal", agent_did: "{agent_did}", behavior_id: "general", session_id: "{session_id}", retry_parent_request: "", retry_root_request: "{request_id}", superseded_by_request: "", content: "spawn", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{now}", retry_count: 0, max_retries: 3, subagent_depth: 0 }}) {{ _docID }} }}"#)).await;
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
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        &agent_did,
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
    identity: &dyn AgentIdentity,
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
    identity: &dyn AgentIdentity,
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
    let mut lifecycle = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        identity.did(),
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
/// provider attempt, publishes one canonical native turn (with spawn
/// admissions when `options.spawn_plan` is set), and builds the tool-call
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
    let agent_did = identity.as_ref().map_or_else(
        || "did:test:test".to_owned(),
        |identity| identity.did().to_owned(),
    );
    let mut request = match identity.as_ref() {
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
                &agent_did,
            )
            .await
        }
    };
    let writer = DefraStreamWriter::new(node.clone(), &agent_did, Duration::from_millis(1));
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
    let (message, plan): (
        gents_protocol::message::Message,
        Option<crate::streaming::SpawnAdmissionPlan>,
    ) = match &options.spawn_plan {
        Some(plan) => {
            let plan = crate::streaming::SpawnAdmissionPlan {
                tool_call_id: plan.tool_call_id.clone(),
                child_request_id: plan.child_request_id.clone(),
                spawn_target_did: agent_did.clone(),
                spawn_behavior_id: plan.spawn_behavior_id.clone(),
                delegated_workspace: plan.delegated_workspace.clone(),
                await_mode: plan.await_mode.clone(),
            };
            let message = gents_protocol::message::Message::Assistant {
                id: Some("bridge-provider-message".into()),
                content: vec![gents_protocol::message::AssistantContent::ToolCall(
                    gents_protocol::message::ToolCall {
                        id: plan.tool_call_id.clone(),
                        call_id: Some("bridge-provider-call".into()),
                        function: gents_protocol::message::ToolFunction::new(
                            crate::toolset::SPAWN_SUBAGENT_TOOL_NAME.into(),
                            serde_json::json!({"name":"child", "prompt":"work", "await_mode":"background"}),
                        ),
                        signature: None,
                        additional_params: None,
                    },
                )],
            };
            (message, Some(plan))
        }
        None => {
            let message = gents_protocol::message::Message::Assistant {
                id: Some("spawn-provider-message".into()),
                content: vec![gents_protocol::message::AssistantContent::ToolCall(
                    gents_protocol::message::ToolCall {
                        id: "spawn-native-tool".into(),
                        call_id: Some("spawn-provider-call".into()),
                        function: gents_protocol::message::ToolFunction::new(
                            options
                                .tool_name
                                .clone()
                                .unwrap_or_else(|| crate::toolset::SPAWN_PROCESS_TOOL_NAME.into()),
                            serde_json::json!({"command": "work"}),
                        ),
                        signature: None,
                        additional_params: None,
                    },
                )],
            };
            (message, None)
        }
    };
    let mut published = match &plan {
        Some(plan) => {
            writer
                .publish_native_turn_with_spawn_admissions(
                    &request,
                    0,
                    0,
                    &message,
                    &[plan.clone()],
                )
                .await?
        }
        None => writer.publish_native_turn(&request, 0, 0, &message).await?,
    };
    let accepted = published
        .accepted_tools
        .pop()
        .expect("canonical publication accepted the tool call");
    let deadline = request
        .claimed_deadline_at()
        .expect("claimed request deadline");
    let mut tool = ToolCallLifecycle::from_accepted(
        node.clone(),
        agent_did.clone(),
        request.request().requester_did.clone(),
        accepted,
        deadline,
        options.await_mode.clone(),
        options.cancel_policy.clone(),
    )?;
    if options.start_running {
        tool.start_running().await?;
    }
    Ok((
        PublishedAdmission {
            node,
            path,
            tool,
            agent_did,
        },
        request,
    ))
}

/// Native `spawn_process` parent fixture — the exact replacement for
/// `spawned_background_tests::published_spawn_parent`, preserving its
/// physical IDs, `AwaitMode::Foreground`, `CancelPolicy::Cascade`, and
/// `start_running`.
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

/// Spawned-subagent bridge fixture — the exact replacement for
/// `spawned_background_tests::published_background_bridge`, preserving the
/// shared tempdir with a real `KeyIdentity` (`test-agent.key`),
/// `SPAWN_SUBAGENT_TOOL_NAME` call, `child_request_id = child-{name}`,
/// `AwaitMode::Background`, `CancelPolicy::Cascade`, and `start_running`.
pub async fn published_background_bridge(
    name: &str,
) -> (Arc<EmbeddedNode>, PathBuf, ToolCallLifecycle) {
    let PublishedAdmission {
        node, path, tool, ..
    } = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        cancel_policy: CancelPolicy::Cascade,
        start_running: true,
        request_created_at: None,
        spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
            tool_call_id: "bridge-native-tool".into(),
            child_request_id: format!("child-{name}"),
            spawn_target_did: "did:test:test".into(),
            spawn_behavior_id: "general".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        tool_name: None,
    })
    .await
    .expect("publish canonical background bridge admission");
    (node, path, tool)
}

#[cfg(test)]
async fn claim_existing_request(
    node: &Arc<EmbeddedNode>,
    request_id: &str,
    agent_did: &str,
) -> RequestLifecycle {
    let escaped = crate::graphql::escape_graphql_string(request_id);
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{ {} }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        ),
        "read local-depth fixture request",
    )
    .await
    .expect("read local-depth fixture request");
    let row: gents_protocol::row::AgentRequestRow =
        crate::graphql::first_row(&response, "AgentRequest")
            .expect("read fixture request")
            .expect("fixture request exists");
    let session_id = row.session_id.as_deref().expect("child session identity");
    ensure_fixture_session(
        node,
        session_id,
        agent_did,
        row.requester_did.as_deref(),
        &chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
    .await;
    let mut request = RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        agent_did,
        row.try_into().expect("decode fixture request"),
        60,
    );
    assert_eq!(
        request.claim().await.expect("claim fixture request"),
        ClaimOutcome::Claimed
    );
    request
}

#[cfg(test)]
async fn publish_depth_target_bridge(
    node: &Arc<EmbeddedNode>,
    request: &mut RequestLifecycle,
    agent_did: &str,
    name: &str,
) -> ToolCallLifecycle {
    let target_tool = "bridge-native-tool";
    publish_accepted_on_claimed_request(
        node.clone(),
        request,
        agent_did,
        0,
        crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
        target_tool,
        serde_json::json!({"name":"child", "prompt":"work", "await_mode":"background"}),
        Some(crate::streaming::SpawnAdmissionPlan {
            tool_call_id: target_tool.into(),
            child_request_id: format!("child-{name}"),
            spawn_target_did: agent_did.to_owned(),
            spawn_behavior_id: "general".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        true,
    )
    .await
    .expect("publish signed parent bridge")
}

/// Publish on a genuinely admitted child with its signed ancestor edges.
#[cfg(test)]
async fn published_background_bridge_at_parent_depth(
    name: &str,
    parent_depth: u32,
) -> (Arc<EmbeddedNode>, PathBuf, ToolCallLifecycle) {
    assert!((1..=2).contains(&parent_depth));
    let ancestor = format!("ancestor-root-{name}");
    let (node, path, root_bridge) = published_background_bridge(&ancestor).await;
    let agent_did = root_bridge.agent_did().to_owned();
    let depth_one_id = root_bridge
        .child_request_id
        .as_deref()
        .expect("root bridge reserved depth-one child")
        .to_owned();
    crate::tool_call_lifecycle::create_subagent_request_with_request_id(
        &node,
        depth_one_id.clone(),
        format!("request-{ancestor}"),
        root_bridge
            .request_doc_id()
            .expect("root request doc")
            .to_owned(),
        root_bridge.tool_call_id().to_owned(),
        root_bridge.doc_id().expect("root tool doc").to_owned(),
        0,
        agent_did.clone(),
        "general".into(),
        "depth-one parent".into(),
        None,
    )
    .await
    .expect("admit signed depth-one parent");
    let mut depth_one = claim_existing_request(&node, &depth_one_id, &agent_did).await;
    if parent_depth == 1 {
        let bridge = publish_depth_target_bridge(&node, &mut depth_one, &agent_did, name).await;
        return (node, path, bridge);
    }
    let depth_two_id = format!("request-{name}");
    let depth_one_tool = format!("ancestor-depth-one-tool-{name}");
    let depth_one_bridge = publish_accepted_on_claimed_request(
        node.clone(),
        &mut depth_one,
        &agent_did,
        0,
        crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
        &depth_one_tool,
        serde_json::json!({"name":"child", "prompt":"depth-two parent", "await_mode":"background"}),
        Some(crate::streaming::SpawnAdmissionPlan {
            tool_call_id: depth_one_tool.clone(),
            child_request_id: depth_two_id.clone(),
            spawn_target_did: agent_did.clone(),
            spawn_behavior_id: "general".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        AwaitMode::Background,
        CancelPolicy::Cascade,
        true,
    )
    .await
    .expect("publish signed depth-one bridge");
    crate::tool_call_lifecycle::create_subagent_request_with_request_id(
        &node,
        depth_two_id.clone(),
        depth_one_id.clone(),
        depth_one_bridge
            .request_doc_id()
            .expect("depth-one request doc")
            .to_owned(),
        depth_one_bridge.tool_call_id().to_owned(),
        depth_one_bridge
            .doc_id()
            .expect("depth-one tool doc")
            .to_owned(),
        1,
        agent_did.clone(),
        "general".into(),
        "depth-two parent".into(),
        None,
    )
    .await
    .expect("admit signed depth-two parent");
    let mut depth_two = claim_existing_request(&node, &depth_two_id, &agent_did).await;
    let target_bridge = publish_depth_target_bridge(&node, &mut depth_two, &agent_did, name).await;
    (node, path, target_bridge)
}

#[cfg(test)]
async fn insert_observed_parent(
    node: &Arc<EmbeddedNode>,
    name: &str,
    agent_did: &str,
    stored_depth: Option<i64>,
) -> String {
    let request_id = format!("request-{name}");
    let session_id = format!("session-{name}");
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    ensure_fixture_session(node, &session_id, agent_did, None, &created_at).await;
    let request = crate::graphql::escape_graphql_string(&request_id);
    let session = crate::graphql::escape_graphql_string(&session_id);
    let agent = crate::graphql::escape_graphql_string(agent_did);
    let created = crate::graphql::escape_graphql_string(&created_at);
    let depth_field =
        stored_depth.map_or_else(String::new, |depth| format!(", subagent_depth: {depth}"));
    let response = crate::config_client::ConfigAccess::write_local(
        node,
        "test.local_depth_observed_parent",
        &format!(
            r#"mutation {{ create_AgentRequest(input: {{ request_id: "{request}", purpose: "normal", agent_did: "{agent}", behavior_id: "general", session_id: "{session}", retry_parent_request: "", retry_root_request: "{request}", superseded_by_request: "", content: "observed parent", lifecycle_state: "pending", backend_id: "", execution_origin: "interactive", failure_reason: "", created_at: "{created}", retry_count: 0, max_retries: 3{depth_field} }}) {{ _docID }} }}"#,
        ),
    )
    .await
    .expect("seed local-depth observed parent");
    crate::graphql::created_doc_id(&response, "AgentRequest").expect("observed parent physical ID")
}

#[cfg(test)]
async fn insert_observed_parent_tool(
    node: &Arc<EmbeddedNode>,
    name: &str,
    agent_did: &str,
    parent_doc_id: &str,
    child_id: &str,
) -> (String, String) {
    let request_id = format!("request-{name}");
    let tool_call_id = format!("observed-parent-tool-{name}");
    let key = crate::graphql::escape_graphql_string(&format!("{request_id}:{tool_call_id}"));
    let request = crate::graphql::escape_graphql_string(&request_id);
    let parent_doc = crate::graphql::escape_graphql_string(parent_doc_id);
    let session = crate::graphql::escape_graphql_string(&format!("session-{name}"));
    let agent = crate::graphql::escape_graphql_string(agent_did);
    let tool = crate::graphql::escape_graphql_string(&tool_call_id);
    let child = crate::graphql::escape_graphql_string(child_id);
    let response = crate::config_client::ConfigAccess::write_local(
        node,
        "test.local_depth_observed_tool",
        &format!(
            r#"mutation {{ create_AgentToolCall(input: {{ tool_call_key: "{key}", request_id: "{request}", request_doc_id: "{parent_doc}", session_id: "{session}", agent_did: "{agent}", message_sequence: 1, tool_name: "spawn_subagent", tool_call_id: "{tool}", lifecycle_state: "running", await_mode: "background", cancel_policy: "cascade", child_request_id: "{child}", spawn_target_did: "{agent}", spawn_behavior_id: "general" }}) {{ _docID }} }}"#,
        ),
    )
    .await
    .expect("seed imported local-depth parent tool observation");
    let tool_doc_id = crate::graphql::created_doc_id(&response, "AgentToolCall")
        .expect("observed parent tool physical ID");
    (tool_call_id, tool_doc_id)
}

#[cfg(test)]
async fn exact_parent_request_id(node: &EmbeddedNode, parent_doc_id: &str) -> String {
    let escaped = crate::graphql::escape_graphql_string(parent_doc_id);
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 2) {{ _docID request_id }} }}"#,
        ),
        "read exact local-depth parent identity",
    )
    .await
    .expect("read exact local-depth parent identity");
    let rows = response.data.expect("exact parent query data")["AgentRequest"]
        .as_array()
        .expect("exact parent rows")
        .clone();
    assert_eq!(rows.len(), 1, "exact local-depth parent is unique");
    rows[0]["request_id"]
        .as_str()
        .expect("exact parent logical request ID")
        .to_owned()
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

/// A `Pending` subagent bridge — `from_accepted` without `start_running` —
/// for transition tests on the subagent path. Same physical IDs and defaults
/// as [`published_background_bridge`] otherwise.
pub async fn pending_background_bridge(
    name: &str,
) -> (Arc<EmbeddedNode>, PathBuf, ToolCallLifecycle) {
    let PublishedAdmission {
        node, path, tool, ..
    } = published_admission(PublishedAdmissionOptions {
        name: name.to_owned(),
        real_identity: true,
        await_mode: AwaitMode::Background,
        cancel_policy: CancelPolicy::Cascade,
        start_running: false,
        request_created_at: None,
        spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
            tool_call_id: "bridge-native-tool".into(),
            child_request_id: format!("child-{name}"),
            spawn_target_did: "did:test:test".into(),
            spawn_behavior_id: "general".into(),
            delegated_workspace: None,
            await_mode: AwaitMode::Background,
        }),
        tool_name: None,
    })
    .await
    .expect("publish pending canonical background bridge admission");
    (node, path, tool)
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::config_client::ConfigAccess;
    use crate::lifecycle::materialize::{
        build_signed_request, ParentLink, RequestIdentity, RequestSigner, RequestSpec,
    };
    use crate::lifecycle::{ExecutionOrigin, WorkspaceLineage};
    use crate::tool_call_lifecycle::{CancelCause, FailureClass};

    async fn canonical_result(node: &Arc<EmbeddedNode>, tool: &ToolCallLifecycle) -> String {
        let message = crate::tool_call_lifecycle::load_tool_call_result(
            &ConfigAccess::Local(node.clone()),
            tool.doc_id().expect("admitted physical tool document"),
            tool.agent_did(),
            tool.session_id(),
            tool.requester_did(),
        )
        .await
        .expect("canonical tool delivery");
        crate::tool_call_lifecycle::render_tool_result(&message)
            .expect("canonical tool delivery renders as text")
    }

    async fn canonical_raw_output(node: &Arc<EmbeddedNode>, tool: &ToolCallLifecycle) -> String {
        crate::background_tools::canonical_tool_output(
            node.as_ref(),
            tool.doc_id().expect("admitted physical tool document"),
            tool.request_doc_id().expect("accepted request document"),
            tool.session_id(),
            tool.agent_did(),
            tool.requester_did(),
        )
        .await
        .expect("canonical raw tool output")
    }

    async fn row(node: &EmbeddedNode, session_id: &str) -> serde_json::Value {
        let session = crate::graphql::escape_graphql_string(session_id);
        let response = node
            .execute(&format!(r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ request_id request_doc_id lifecycle_state tool_failure_class cancel_cause deadline_at await_mode cancel_policy child_request_id }} }}"#))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        let data = response.data.unwrap();
        let rows = data["AgentToolCall"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "exactly one admitted physical tool row");
        rows.first().cloned().expect("one admitted tool call")
    }

    async fn seed_reserved_child(
        node: &EmbeddedNode,
        request_id: &str,
        agent_did: &str,
        behavior_id: &str,
        prompt: &str,
        parent_request_id: &str,
        parent_request_doc_id: &str,
        parent_tool_call_id: &str,
        parent_tool_call_doc_id: &str,
        depth: u32,
        workspace: Option<WorkspaceLineage>,
        admission: gents_protocol::request_admission::AgentRequestAdmissionRecord,
    ) {
        let identity = RequestIdentity {
            requester_did: admission.runtime_bridge_author_did.clone(),
            request_id: request_id.to_owned(),
            agent_did: agent_did.to_owned(),
            behavior_id: behavior_id.to_owned(),
            session_id: uuid::Uuid::new_v4().to_string(),
            content: prompt.to_owned(),
            execution_origin: ExecutionOrigin::Interactive,
            created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        };
        let spec = RequestSpec {
            workspace,
            subagent: Some(ParentLink {
                depth,
                parent_request_id: parent_request_id.to_owned(),
                parent_request_doc_id: parent_request_doc_id.to_owned(),
                parent_tool_call_id: Some(parent_tool_call_id.to_owned()),
                parent_tool_call_doc_id: Some(parent_tool_call_doc_id.to_owned()),
            }),
            ..RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                identity,
                admission,
            )
        };
        let create = build_signed_request(spec, RequestSigner::RegisteredTarget)
            .await
            .expect("sign modeled reserved child seed");
        let response = node
            .execute(
                &create
                    .graphql_mutation()
                    .expect("serialize modeled reserved child seed"),
            )
            .await;
        assert!(
            !response.has_errors(),
            "seed modeled reserved child: {:?}",
            response.errors
        );
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
        assert!(running["child_request_id"].is_null());
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

    #[tokio::test]
    async fn admitted_bridge_round_trip_preserves_mode_policy_and_child_link() {
        let (node, path, mut bridge) = published_background_bridge("bridge-round-trip").await;
        assert_eq!(
            row(&node, "session-bridge-round-trip").await["await_mode"],
            "background"
        );
        bridge.foreground().await.unwrap();
        assert_eq!(
            row(&node, "session-bridge-round-trip").await["await_mode"],
            "foreground"
        );
        let foreground = ToolCallLifecycle::load(
            node.clone(),
            "session-bridge-round-trip",
            "bridge-native-tool",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(foreground.await_mode, AwaitMode::Foreground);
        assert_eq!(foreground.cancel_policy, CancelPolicy::Cascade);
        assert_eq!(
            foreground.child_request_id.as_deref(),
            Some("child-bridge-round-trip")
        );
        let error = bridge.foreground().await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<super::super::IllegalToolCallTransition>(),
            Some(super::super::IllegalToolCallTransition::ModeAlreadyForeground)
        ));
        bridge.background().await.unwrap();
        bridge.detach().await.unwrap();
        let persisted = row(&node, "session-bridge-round-trip").await;
        assert_eq!(persisted["await_mode"], "background");
        assert_eq!(persisted["cancel_policy"], "detach");
        assert_eq!(persisted["child_request_id"], "child-bridge-round-trip");
        assert_eq!(persisted["request_id"], "request-bridge-round-trip");
        let error = bridge.detach().await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<super::super::IllegalToolCallTransition>(),
            Some(super::super::IllegalToolCallTransition::PolicyAlreadyDetach)
        ));
        let loaded = ToolCallLifecycle::load(
            node.clone(),
            "session-bridge-round-trip",
            "bridge-native-tool",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(loaded.await_mode, AwaitMode::Background);
        assert_eq!(loaded.cancel_policy, CancelPolicy::Detach);
        assert_eq!(
            loaded.child_request_id.as_deref(),
            Some("child-bridge-round-trip")
        );
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_bridge_mode_flips_tolerate_stale_same_target_owner() {
        let (node, path, mut first) = published_background_bridge("bridge-mode-stale").await;
        let mut stale = ToolCallLifecycle::load(
            node.clone(),
            "session-bridge-mode-stale",
            "bridge-native-tool",
        )
        .await
        .unwrap()
        .unwrap();
        first.foreground().await.unwrap();
        stale.foreground().await.unwrap();
        assert_eq!(
            row(&node, "session-bridge-mode-stale").await["await_mode"],
            "foreground"
        );
        let mut stale = ToolCallLifecycle::load(
            node.clone(),
            "session-bridge-mode-stale",
            "bridge-native-tool",
        )
        .await
        .unwrap()
        .unwrap();
        first.background().await.unwrap();
        stale.background().await.unwrap();
        assert_eq!(
            row(&node, "session-bridge-mode-stale").await["await_mode"],
            "background"
        );
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_bridge_complete_persists_and_loses_to_foreign_terminal() {
        let (node, path, mut bridge) = published_background_bridge("bridge-complete").await;
        bridge
            .publish_background_receipt("child started")
            .await
            .unwrap();
        assert!(bridge.bridge_complete("child answer".into()).await.unwrap());
        assert_eq!(
            row(&node, "session-bridge-complete").await["lifecycle_state"],
            "completed"
        );
        assert_eq!(canonical_result(&node, &bridge).await, "child started");
        assert_eq!(canonical_raw_output(&node, &bridge).await, "child answer");
        teardown(node, path).await;

        let (node, path, mut stale) = published_background_bridge("bridge-complete-cas").await;
        stale
            .publish_background_receipt("child started")
            .await
            .unwrap();
        let mut winner = ToolCallLifecycle::load(
            node.clone(),
            "session-bridge-complete-cas",
            "bridge-native-tool",
        )
        .await
        .unwrap()
        .unwrap();
        winner
            .cancel_during_run(CancelCause::Interrupted)
            .await
            .unwrap();
        assert!(!stale.bridge_complete("late".into()).await.unwrap());
        let persisted = row(&node, "session-bridge-complete-cas").await;
        assert_eq!(persisted["lifecycle_state"], "cancelled");
        assert_eq!(persisted["cancel_cause"], "interrupted");
        assert_eq!(canonical_result(&node, &stale).await, "child started");
        assert_eq!(
            canonical_raw_output(&node, &stale).await,
            "tool call cancelled"
        );
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_bridge_failure_projects_all_child_terminals_and_preserves_foreign_winner() {
        for (name, terminal, expected, cause, expected_result) in [
            (
                "failed",
                super::super::ChildTerminal::Failed {
                    reason: "child failed".into(),
                    failure_class: FailureClass::External,
                },
                "failed",
                None,
                "child failed",
            ),
            (
                "dead",
                super::super::ChildTerminal::Dead,
                "failed",
                None,
                "linked child did not produce a completed result",
            ),
            (
                "superseded",
                super::super::ChildTerminal::Superseded,
                "failed",
                None,
                "linked child did not produce a completed result",
            ),
            (
                "interrupted",
                super::super::ChildTerminal::Interrupted,
                "cancelled",
                Some("interrupted"),
                "linked child did not produce a completed result",
            ),
        ] {
            let fixture = format!("bridge-failure-{name}");
            let (node, path, mut bridge) = published_background_bridge(&fixture).await;
            bridge
                .publish_background_receipt("child started")
                .await
                .unwrap();
            assert!(bridge.bridge_failure(terminal).await.unwrap());
            let persisted = row(&node, &format!("session-{fixture}")).await;
            assert_eq!(persisted["lifecycle_state"], expected);
            assert_eq!(persisted["cancel_cause"].as_str(), cause);
            if name == "failed" {
                assert_eq!(persisted["tool_failure_class"], "external");
            }
            assert_eq!(canonical_result(&node, &bridge).await, "child started");
            assert_eq!(canonical_raw_output(&node, &bridge).await, expected_result);
            teardown(node, path).await;
        }
        let (node, path, mut stale) = published_background_bridge("bridge-failure-cas").await;
        stale
            .publish_background_receipt("child started")
            .await
            .unwrap();
        let mut winner = ToolCallLifecycle::load(
            node.clone(),
            "session-bridge-failure-cas",
            "bridge-native-tool",
        )
        .await
        .unwrap()
        .unwrap();
        winner.bridge_complete("winner".into()).await.unwrap();
        assert!(!stale
            .bridge_failure(super::super::ChildTerminal::Dead)
            .await
            .unwrap());
        assert_eq!(
            row(&node, "session-bridge-failure-cas").await["lifecycle_state"],
            "completed"
        );
        assert_eq!(canonical_result(&node, &stale).await, "child started");
        assert_eq!(canonical_raw_output(&node, &stale).await, "winner");
        teardown(node, path).await;
    }

    /// Binds the executable Lean bridge completion/failure rows to an
    /// accepted physical bridge. The projection integration test remains the
    /// owner of child-row reconstruction; this test pins the lifecycle owner
    /// selected after that projection without fabricating an unaccepted tool
    /// row.
    #[tokio::test]
    async fn generated_bridge_step_terminal_cases_use_accepted_bridge_owner() {
        for case in crate::lean_vocab_test::lean_bridge_step_cases()
            .iter()
            .filter(|case| case.event != "bridge_cancel_cascade")
        {
            if !case.bridge_committed {
                assert!(!case.legal, "{}", case.name);
                assert!(case.post_tool_state.is_none(), "{}", case.name);
                continue;
            }
            let fixture = format!("generated-{}", case.name);
            let (node, path, mut bridge) = published_background_bridge(&fixture).await;
            bridge
                .publish_background_receipt("child accepted")
                .await
                .unwrap();

            let transitioned = match (case.event.as_str(), case.child_state.as_str()) {
                ("bridge_complete", "completed") => bridge
                    .bridge_complete("child completed".into())
                    .await
                    .unwrap(),
                ("bridge_complete", "processing") => false,
                ("bridge_failure", "interrupted") => bridge
                    .bridge_failure(super::super::ChildTerminal::Interrupted)
                    .await
                    .unwrap(),
                ("bridge_failure", "failed") => bridge
                    .bridge_failure(super::super::ChildTerminal::Failed {
                        reason: "child failed".into(),
                        failure_class: FailureClass::External,
                    })
                    .await
                    .unwrap(),
                ("bridge_failure", "dead") => bridge
                    .bridge_failure(super::super::ChildTerminal::Dead)
                    .await
                    .unwrap(),
                // A completed child is dispatched to bridge_complete by the
                // projection owner, never to bridge_failure.
                ("bridge_failure", "completed") => false,
                other => panic!("unsupported generated bridge terminal case {other:?}"),
            };
            assert_eq!(transitioned, case.legal, "{}", case.name);
            let persisted = row(&node, &format!("session-{fixture}")).await;
            let expected = case.post_tool_state.as_deref().unwrap_or("running");
            assert_eq!(persisted["lifecycle_state"], expected, "{}", case.name);
            teardown(node, path).await;
        }
    }

    #[tokio::test]
    async fn admitted_bridge_cascade_intent_respects_detach_and_native_identity() {
        let (node, path, mut cascade) = published_background_bridge("bridge-cascade").await;
        cascade
            .cancel_during_run(CancelCause::UserCancelled)
            .await
            .unwrap();
        let intent = cascade
            .bridge_cancel_cascade()
            .await
            .unwrap()
            .expect("cascade bridge carries a child cancellation intent");
        assert_eq!(intent.child_request_id, "child-bridge-cascade");
        teardown(node, path).await;

        let (node, path, mut detached) = published_background_bridge("bridge-detached").await;
        detached.detach().await.unwrap();
        detached
            .cancel_during_run(CancelCause::UserCancelled)
            .await
            .unwrap();
        assert!(detached.bridge_cancel_cascade().await.unwrap().is_none());
        teardown(node, path).await;

        let (node, path, mut native) = published_spawn_parent("native-cascade").await;
        native
            .cancel_during_run(CancelCause::UserCancelled)
            .await
            .unwrap();
        assert!(native.bridge_cancel_cascade().await.unwrap().is_none());
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn generated_bridge_step_cascade_cases_use_accepted_bridge_owner() {
        for case in crate::lean_vocab_test::lean_bridge_step_cases()
            .iter()
            .filter(|case| case.event == "bridge_cancel_cascade")
        {
            let fixture = format!("generated-{}", case.name);
            let (node, path, mut bridge) = published_background_bridge(&fixture).await;
            if case.cancel_policy == "detach" {
                bridge.detach().await.unwrap();
            } else {
                assert_eq!(case.cancel_policy, "cascade", "{}", case.name);
            }
            if case.bridge_state == "running" {
                assert!(!case.legal, "{}", case.name);
                assert!(
                    bridge.bridge_cancel_cascade().await.is_err(),
                    "{}",
                    case.name
                );
                teardown(node, path).await;
                continue;
            }
            assert_eq!(case.bridge_state, "cancelled", "{}", case.name);
            assert!(bridge
                .cancel_during_run(CancelCause::UserCancelled)
                .await
                .unwrap());
            let intent = bridge.bridge_cancel_cascade().await.unwrap();
            if case.post_child_interrupt_set {
                assert!(case.legal, "{}", case.name);
                let intent = intent.expect("modeled cascade must select the reserved child");
                assert_eq!(
                    intent.child_request_id,
                    format!("child-{fixture}"),
                    "{}",
                    case.name
                );
            } else {
                assert!(!case.legal, "{}", case.name);
                assert!(intent.is_none(), "{}", case.name);
            }
            teardown(node, path).await;
        }
    }

    #[tokio::test]
    async fn admitted_native_rejects_detach_without_changing_persisted_policy() {
        let (node, path, mut native) = published_spawn_parent("native-detach").await;
        let error = native.detach().await.unwrap_err();
        assert!(matches!(
            error.downcast_ref::<super::super::IllegalToolCallTransition>(),
            Some(super::super::IllegalToolCallTransition::DetachRequiresChildLink)
        ));
        let persisted = row(&node, "session-native-detach").await;
        assert_eq!(persisted["cancel_policy"], "cascade");
        assert!(persisted["child_request_id"].is_null());
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn admitted_parent_creates_child_at_exact_max_depth_with_full_lineage() {
        let (node, path, bridge) =
            published_background_bridge_at_parent_depth("child-depth-boundary", 2).await;
        // `from_accepted` owns the physical request coordinate but deliberately
        // does not invent a logical request ID. Use the request ID authored by
        // this fixture's canonical admission rather than treating the empty
        // in-memory compatibility field as durable lineage.
        let parent_request_id = "request-child-depth-boundary";
        let parent_request_doc_id = bridge
            .request_doc_id()
            .expect("accepted bridge request document")
            .to_owned();
        let parent_tool_call_doc_id = bridge
            .doc_id()
            .expect("accepted bridge tool document")
            .to_owned();
        let child_id = bridge
            .child_request_id
            .clone()
            .expect("accepted bridge reserves an exact child request identity");
        let ensure = || {
            super::super::create_subagent_request_with_request_id(
                &node,
                child_id.clone(),
                parent_request_id.to_owned(),
                parent_request_doc_id.clone(),
                bridge.tool_call_id().to_owned(),
                parent_tool_call_doc_id.clone(),
                super::super::MAX_SUBAGENT_DEPTH - 1,
                bridge.agent_did().to_owned(),
                "general".into(),
                "boundary child".into(),
                None,
            )
        };
        let (left, right) = tokio::join!(ensure(), ensure());
        assert_eq!(left.expect("left concurrent fresh ensure"), child_id);
        assert_eq!(right.expect("right concurrent fresh ensure"), child_id);
        assert_eq!(ensure().await.expect("post-commit exact replay"), child_id);

        let conflict = super::super::create_subagent_request_with_request_id(
            &node,
            child_id.clone(),
            parent_request_id.to_owned(),
            parent_request_doc_id.clone(),
            bridge.tool_call_id().to_owned(),
            parent_tool_call_doc_id.clone(),
            super::super::MAX_SUBAGENT_DEPTH - 1,
            bridge.agent_did().to_owned(),
            "general".into(),
            "different child payload".into(),
            None,
        )
        .await
        .expect_err("reserved child payload rebinding must fail closed");
        assert!(conflict
            .to_string()
            .contains("conflicts with the accepted bridge lineage"));

        let child_id_escaped = crate::graphql::escape_graphql_string(&child_id);
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{child_id_escaped}" }} }}) {{ lifecycle_state subagent_depth caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id caused_by_trigger_id caused_by_trigger_kind }} }}"#
            ))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        let rows = response.data.unwrap()["AgentRequest"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(rows.len(), 1);
        let child = &rows[0];
        assert_eq!(child["lifecycle_state"], "pending");
        assert_eq!(
            child["subagent_depth"].as_u64(),
            Some(u64::from(super::super::MAX_SUBAGENT_DEPTH))
        );
        assert_eq!(child["caused_by_parent_request_id"], parent_request_id);
        assert_eq!(
            child["caused_by_parent_request_doc_id"],
            parent_request_doc_id
        );
        assert_eq!(
            child["caused_by_parent_tool_call_id"],
            bridge.tool_call_id()
        );
        assert_eq!(
            child["caused_by_parent_tool_call_doc_id"],
            parent_tool_call_doc_id
        );
        assert_eq!(child["caused_by_trigger_id"], bridge.tool_call_id());
        assert_eq!(child["caused_by_trigger_kind"], "subagent");
        teardown(node, path).await;
    }

    #[tokio::test]
    async fn generated_local_child_parent_depth_cases_drive_owner() {
        use crate::lean_vocab_test::LeanLocalParentDepthExpected;
        use crate::tool_call_lifecycle::IllegalToolCallTransition;

        let cases = crate::lean_vocab_test::lean_local_parent_depth_cases();
        assert_eq!(cases.len(), 12, "generated local-depth inventory changed");
        for case in cases {
            let name = format!("local-depth-{}", case.name);
            let observed = case
                .stored_parent_depth
                .and_then(|value| u32::try_from(value).ok());
            let (node, path, bridge, raw_observation) = match (case.stored_parent_depth, observed) {
                (Some(_), Some(0)) => {
                    let (node, path, bridge) = published_background_bridge(&name).await;
                    (node, path, bridge, false)
                }
                (Some(_), Some(1 | 2)) => {
                    let (node, path, bridge) =
                        published_background_bridge_at_parent_depth(&name, observed.unwrap()).await;
                    (node, path, bridge, false)
                }
                (None, _) | (Some(_), _) => {
                    let (node, path, bridge) =
                        published_background_bridge(&format!("unrelated-identity-{name}")).await;
                    (node, path, bridge, true)
                }
            };
            let agent_did = bridge.agent_did().to_owned();
            let (
                parent_request_id,
                parent_doc_id,
                parent_tool_call_id,
                parent_tool_call_doc_id,
                child_id,
            ) = if raw_observation {
                let parent_doc_id =
                    insert_observed_parent(&node, &name, &agent_did, case.stored_parent_depth)
                        .await;
                let child_id = format!("observed-child-{name}");
                let (tool_call_id, tool_doc_id) = insert_observed_parent_tool(
                    &node,
                    &name,
                    &agent_did,
                    &parent_doc_id,
                    &child_id,
                )
                .await;
                (
                    format!("request-{name}"),
                    parent_doc_id,
                    tool_call_id,
                    tool_doc_id,
                    child_id,
                )
            } else {
                let parent_doc_id = bridge
                    .request_doc_id()
                    .expect("accepted parent doc")
                    .to_owned();
                (
                    exact_parent_request_id(&node, &parent_doc_id).await,
                    parent_doc_id,
                    bridge.tool_call_id().to_owned(),
                    bridge.doc_id().expect("accepted tool doc").to_owned(),
                    bridge
                        .child_request_id
                        .as_deref()
                        .expect("accepted child identity")
                        .to_owned(),
                )
            };
            let child_query = format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ _docID request_id agent_did subagent_depth caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id admission_kind admission_signature runtime_source_kind }} }}"#,
                crate::graphql::escape_graphql_string(&child_id),
            );
            let before = crate::graphql::graphql_with_transaction_retry(
                &node,
                &child_query,
                "read child before local-depth admission",
            )
            .await
            .unwrap_or_else(|error| panic!("{}: {error:#}", case.name));
            assert_eq!(
                before.data.unwrap()["AgentRequest"]
                    .as_array()
                    .unwrap()
                    .len(),
                0
            );
            let result = super::super::create_subagent_request_with_request_id(
                &node,
                child_id.clone(),
                parent_request_id.clone(),
                parent_doc_id.clone(),
                parent_tool_call_id.clone(),
                parent_tool_call_doc_id.clone(),
                case.supplied_parent_depth,
                agent_did,
                "general".into(),
                "generated local depth".into(),
                None,
            )
            .await;
            let after = crate::graphql::graphql_with_transaction_retry(
                &node,
                &child_query,
                "read child after local-depth admission",
            )
            .await
            .unwrap_or_else(|error| panic!("{}: {error:#}", case.name));
            let rows = after.data.unwrap()["AgentRequest"]
                .as_array()
                .unwrap()
                .clone();
            match &case.expected {
                LeanLocalParentDepthExpected::Admitted { child_depth } => {
                    assert_eq!(
                        result.unwrap_or_else(|error| panic!("{}: {error:#}", case.name)),
                        child_id
                    );
                    assert_eq!(rows.len(), 1, "{}", case.name);
                    let child = &rows[0];
                    assert_eq!(
                        child["subagent_depth"].as_u64(),
                        Some(u64::from(*child_depth)),
                        "{}",
                        case.name
                    );
                    assert_eq!(
                        child["caused_by_parent_request_id"], parent_request_id,
                        "{}",
                        case.name
                    );
                    assert_eq!(
                        child["caused_by_parent_request_doc_id"], parent_doc_id,
                        "{}",
                        case.name
                    );
                    assert_eq!(
                        child["caused_by_parent_tool_call_id"], parent_tool_call_id,
                        "{}",
                        case.name
                    );
                    assert_eq!(
                        child["caused_by_parent_tool_call_doc_id"], parent_tool_call_doc_id,
                        "{}",
                        case.name
                    );
                    assert_eq!(child["admission_kind"], "runtime-internal", "{}", case.name);
                    assert_eq!(child["runtime_source_kind"], "local-child", "{}", case.name);
                    assert!(
                        child["admission_signature"]
                            .as_str()
                            .is_some_and(|sig| !sig.is_empty()),
                        "{}",
                        case.name
                    );
                }
                LeanLocalParentDepthExpected::Rejected { reason } => {
                    let error = result.expect_err("modeled local-depth rejection");
                    let expected = match reason.as_str() {
                        "depth_exceeded" => IllegalToolCallTransition::SubagentDepthExceeded,
                        "parent_linkage_incoherent" => {
                            IllegalToolCallTransition::ParentLinkageIncoherent
                        }
                        other => panic!("unknown generated local-depth rejection {other}"),
                    };
                    assert_eq!(
                        error.downcast_ref::<IllegalToolCallTransition>(),
                        Some(&expected),
                        "{}: {error:#}",
                        case.name
                    );
                    assert!(
                        rows.is_empty(),
                        "{} created child on rejection: {rows:?}",
                        case.name
                    );
                }
            }
            teardown(node, path).await;
        }
    }

    #[tokio::test]
    async fn generated_reserved_child_cases_drive_actual_transaction_owner() {
        use crate::lean_vocab_test::lean_reserved_child_materialization_cases;

        fn mapped(stored: usize, candidate: usize, exact: &str, prefix: &str) -> String {
            if stored == candidate {
                exact.to_owned()
            } else {
                format!("{prefix}-{stored}")
            }
        }
        fn workspace(
            value: Option<&crate::lean_vocab_test::LeanCanonicalDelegatedWorkspace>,
            modeled_agent: usize,
            agent_did: &str,
        ) -> Option<WorkspaceLineage> {
            value.map(|value| WorkspaceLineage {
                workspace_id: Some(format!("workspace-{}", value.workspace_id)),
                workspace_owner_agent_did: Some(
                    if value.workspace_owner_agent_did
                        == u64::try_from(modeled_agent).expect("modeled agent fits native identity")
                    {
                        agent_did.to_owned()
                    } else {
                        format!("agent-{}", value.workspace_owner_agent_did)
                    },
                ),
                workspace_authority: Some(value.workspace_authority.clone()),
                workspace_seal_hash: value.workspace_seal_hash.map(|seal| format!("seal-{seal}")),
            })
        }

        for case in lean_reserved_child_materialization_cases() {
            let fixture = format!("reserved-child-{}", case.name);
            let candidate = &case.candidate;
            let (node, path, bridge) = if candidate.depth == 3 {
                published_background_bridge_at_parent_depth(&fixture, 2).await
            } else {
                published_background_bridge(&fixture).await
            };
            let candidate_depth =
                u32::try_from(candidate.depth).expect("modeled child depth fits native depth");
            let parent_depth = candidate_depth
                .checked_sub(1)
                .expect("modeled child has a parent depth");
            let request_id = bridge
                .child_request_id
                .as_deref()
                .expect("accepted bridge reserves child")
                .to_owned();
            let agent_did = bridge.agent_did().to_owned();
            let behavior_id = "general";
            let prompt = format!("payload-{}", candidate.payload);
            let parent_request_id = format!("request-{fixture}");
            let parent_request_doc_id = bridge.request_doc_id().unwrap().to_owned();
            let parent_tool_call_id = bridge.tool_call_id().to_owned();
            let parent_tool_call_doc_id = bridge.doc_id().unwrap().to_owned();
            let candidate_workspace =
                workspace(candidate.workspace.as_ref(), candidate.agent, &agent_did);
            let bridge_author = bridge
                .requester_did()
                .unwrap_or(bridge.agent_did())
                .to_owned();
            let candidate_admission = if candidate.admission == 7 {
                gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_child(
                    &agent_did,
                    &parent_request_id,
                )
            } else {
                gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_cross_principal_child(
                    &agent_did,
                    &parent_request_id,
                    &bridge_author,
                )
            };

            for stored in &case.stored {
                let stored_workspace = if stored.workspace == candidate.workspace {
                    candidate_workspace.clone()
                } else {
                    workspace(stored.workspace.as_ref(), candidate.agent, &agent_did)
                };
                let stored_admission = if stored.admission == candidate.admission {
                    if candidate.admission == 7 {
                        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_child(
                            &agent_did,
                            &parent_request_id,
                        )
                    } else {
                        gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_cross_principal_child(
                            &agent_did,
                            &parent_request_id,
                            &bridge_author,
                        )
                    }
                } else {
                    gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_child(
                        &agent_did,
                        &parent_request_id,
                    )
                };
                seed_reserved_child(
                    &node,
                    &mapped(stored.child, candidate.child, &request_id, "child"),
                    &mapped(stored.agent, candidate.agent, &agent_did, "agent"),
                    &mapped(stored.behavior, candidate.behavior, behavior_id, "behavior"),
                    &mapped(stored.payload, candidate.payload, &prompt, "payload"),
                    &mapped(
                        stored.parent_request,
                        candidate.parent_request,
                        &parent_request_id,
                        "parent-request",
                    ),
                    &mapped(
                        stored.parent_request_doc,
                        candidate.parent_request_doc,
                        &parent_request_doc_id,
                        "parent-request-doc",
                    ),
                    &mapped(
                        stored.parent_tool,
                        candidate.parent_tool,
                        &parent_tool_call_id,
                        "parent-tool",
                    ),
                    &mapped(
                        stored.parent_tool_doc,
                        candidate.parent_tool_doc,
                        &parent_tool_call_doc_id,
                        "parent-tool-doc",
                    ),
                    u32::try_from(stored.depth)
                        .expect("modeled stored child depth fits native depth"),
                    stored_workspace,
                    stored_admission,
                )
                .await;
            }

            let rows_query = format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{
                    _docID request_id agent_did requester_did behavior_id content input subagent_depth
                    admission_kind runtime_issuer_did runtime_source_request_id runtime_source_kind runtime_bridge_author_did
                    caused_by_parent_request_id caused_by_parent_request_doc_id caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id
                    workspace_id workspace_owner_agent_did workspace_authority workspace_seal_hash
                }} }}"#,
                crate::graphql::escape_graphql_string(&request_id),
            );
            let before = node.execute(&rows_query).await;
            assert!(!before.has_errors(), "{:?}", before.errors);
            let before_rows = before.data.unwrap()["AgentRequest"].clone();

            let result = if candidate.admission == 7 {
                super::super::subagent_request::create_subagent_request_with_request_id_and_workspace(
                    &node,
                    request_id.clone(),
                    parent_request_id.clone(),
                    parent_request_doc_id.clone(),
                    parent_tool_call_id.clone(),
                    parent_tool_call_doc_id.clone(),
                    parent_depth,
                    agent_did.clone(),
                    behavior_id.into(),
                    prompt.clone(),
                    None,
                    candidate_workspace.clone(),
                )
                .await
            } else {
                super::super::subagent_request::create_subagent_request_with_trusted_parent_request_id_and_workspace(
                    &node,
                    request_id.clone(),
                    parent_request_id.clone(),
                    parent_request_doc_id.clone(),
                    parent_tool_call_id.clone(),
                    parent_tool_call_doc_id.clone(),
                    parent_depth,
                    agent_did.clone(),
                    behavior_id.into(),
                    prompt.clone(),
                    None,
                    bridge_author,
                    candidate_workspace.clone(),
                )
                .await
            };
            match case.expected_decision.as_str() {
                "created" | "replayed" => assert_eq!(
                    result.unwrap_or_else(|error| panic!("{}: {error:#}", case.name)),
                    request_id,
                    "{} must return the exact reserved identity",
                    case.name
                ),
                "conflict" => {
                    let error = result.expect_err("modeled conflict must fail closed");
                    let typed = error
                        .downcast_ref::<super::super::subagent_request::ReservedChildEnsureError>()
                        .unwrap_or_else(|| {
                            panic!("{} returned unrelated error: {error:#}", case.name)
                        });
                    if case.stored.len() > 1 {
                        assert!(matches!(
                            typed,
                            super::super::subagent_request::ReservedChildEnsureError::Ambiguous { .. }
                        ));
                    } else {
                        assert!(matches!(
                            typed,
                            super::super::subagent_request::ReservedChildEnsureError::BindingConflict { .. }
                        ));
                    }
                }
                other => panic!("unknown modeled decision {other}"),
            }
            let response = node.execute(&rows_query).await;
            assert!(!response.has_errors(), "{:?}", response.errors);
            let after_rows = response.data.unwrap()["AgentRequest"].clone();
            assert_eq!(
                after_rows.as_array().unwrap().len(),
                case.expected_count,
                "{}",
                case.name
            );
            if case.expected_decision == "conflict" {
                assert_eq!(
                    after_rows, before_rows,
                    "{} conflict must preserve every stored semantic field",
                    case.name
                );
            } else {
                let rows: Vec<gents_protocol::row::AgentRequestRow> =
                    serde_json::from_value(after_rows).expect("decode ensured child rows");
                assert_eq!(
                    super::super::subagent_request::reserved_child_decision(
                        &rows,
                        &agent_did,
                        behavior_id,
                        &prompt,
                        &gents_protocol::request_input::RequestInput::default(),
                        candidate_depth,
                        &parent_request_id,
                        &parent_request_doc_id,
                        &parent_tool_call_id,
                        &parent_tool_call_doc_id,
                        candidate_workspace.as_ref(),
                        &candidate_admission,
                    ),
                    super::super::subagent_request::ReservedChildDecision::Replay,
                    "{} committed child must preserve the complete semantic binding",
                    case.name
                );
            }
            teardown(node, path).await;
        }
    }

    #[tokio::test]
    async fn admitted_parent_rejects_empty_logical_lineage() {
        let (node, path, bridge) = published_background_bridge("child-invalid-lineage").await;
        let parent_request_doc_id = bridge.request_doc_id().unwrap().to_owned();
        let parent_tool_call_doc_id = bridge.doc_id().unwrap().to_owned();
        let arguments = || {
            (
                parent_request_doc_id.clone(),
                parent_tool_call_doc_id.clone(),
                bridge.agent_did().to_owned(),
            )
        };

        for (parent_request_id, parent_tool_call_id) in [
            ("", bridge.tool_call_id()),
            ("request-child-invalid-lineage", ""),
        ] {
            let (request_doc, tool_doc, agent) = arguments();
            let error = super::super::create_subagent_request(
                &node,
                parent_request_id.to_owned(),
                request_doc,
                parent_tool_call_id.to_owned(),
                tool_doc,
                0,
                agent,
                "general".into(),
                "invalid lineage".into(),
                None,
            )
            .await
            .unwrap_err();
            assert!(matches!(
                error.downcast_ref::<super::super::IllegalToolCallTransition>(),
                Some(super::super::IllegalToolCallTransition::ParentLinkageIncoherent)
            ));
        }
        teardown(node, path).await;
    }
}
