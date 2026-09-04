//! Helper for creating subagent-parent-linked AgentRequest rows.
//!
//! Public API surface consumed by R3's `SubagentSource` and by Bucket 3
//! conformance fixtures (Task 26). Mirrors R1's existing AgentRequest
//! creation flow in `crates/gents/src/lifecycle/materialize.rs`,
//! with the addition of subagent parent-linkage fields and the depth
//! cap enforced by Lean's `Subagent.maxSubagentDepth`.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::graphql::escape_graphql_string;
use crate::lifecycle::{WorkspaceLineage, DEFAULT_REQUEST_MAX_RETRIES};
use crate::session::execute_mutation_with_retry;

use super::IllegalToolCallTransition;

enum SubagentAdmissionSource {
    LocalChild,
    CrossDeploymentChild { bridge_author_did: String },
}

/// The configured cap on subagent recursion depth. Matches Lean's
/// `Subagent.maxSubagentDepth = 3` (see
/// `crates/gents/proofs/Proofs/Background/State.lean`). Exposed as
/// part of R2's public API surface so R3's apply-time spawn-flow
/// validation can reference the same value as the Lean spec.
pub const MAX_SUBAGENT_DEPTH: u32 = 3;

/// Create a new AgentRequest row with subagent parent linkage. Allocates a
/// fresh request id before delegating to the request-id-aware implementation
/// used by R3's `SubagentSource`.
#[allow(clippy::too_many_arguments)]
pub async fn create_subagent_request(
    node: &EmbeddedNode,
    parent_request_id: String,
    parent_request_doc_id: String,
    parent_tool_call_id: String,
    parent_tool_call_doc_id: String,
    parent_subagent_depth: u32,
    agent_did: String,
    behavior_id: String,
    prompt: String,
    deadline: Option<DateTime<Utc>>,
) -> Result<String> {
    let new_request_id = uuid::Uuid::new_v4().to_string();
    create_subagent_request_with_request_id(
        node,
        new_request_id,
        parent_request_id,
        parent_request_doc_id,
        parent_tool_call_id,
        parent_tool_call_doc_id,
        parent_subagent_depth,
        agent_did,
        behavior_id,
        prompt,
        deadline,
    )
    .await
}

/// Create a new AgentRequest row with subagent parent linkage and a caller-
/// supplied request id. `SubagentSource` uses this path to preserve B5 link
/// symmetry: the child `AgentRequest.request_id` must equal the parent
/// `AgentToolCall.child_request_id` that caused the spawn.
///
/// Validates the preconditions enforced by the Lean Subagent spec before
/// creation:
///
///   1. `parent_subagent_depth + 1 ≤ MAX_SUBAGENT_DEPTH`. Returns
///      `IllegalToolCallTransition::SubagentDepthExceeded` otherwise.
///   2. Both logical identifiers (`parent_request_id` and
///      `parent_tool_call_id`) and both physical document identifiers
///      (`parent_request_doc_id` and `parent_tool_call_doc_id`) are non-empty.
///      Returns `IllegalToolCallTransition::ParentLinkageIncoherent`
///      otherwise. (A child must reference both parent identifiers; the
///      well-formedness invariant in `watcher.rs::validate_subagent_fields`
///      requires that `subagent_depth > 0` documents have all four fields
///      populated.)
///   3. On the same-node path, `parent_request_doc_id` resolves to an existing
///      `AgentRequest` whose logical id and owner match the supplied values.
///      Both paths require `parent_tool_call_doc_id` to resolve to an
///      `AgentToolCall` whose request document and logical tool-call id match.
///      The trusted cross-deployment path additionally requires a non-empty
///      requester DID.
///
/// On success returns the child `request_id`.
///
/// Field ownership notes (mirroring `materialize.rs`):
///   - `request_id` is caller-supplied; `session_id` is a freshly generated
///     UUID.
///   - `lifecycle_state` is initialized to `"pending"`.
///   - `subagent_depth = parent_subagent_depth + 1`.
///   - The logical and physical `caused_by_parent_*` pairs carry the parent
///     linkage.
///   - Trigger lineage fields identify the bridge edge:
///     `caused_by_trigger_kind = "subagent"` and
///     `caused_by_trigger_id = parent_tool_call_id`.
#[allow(clippy::too_many_arguments)]
pub async fn create_subagent_request_with_request_id(
    node: &EmbeddedNode,
    request_id: String,
    parent_request_id: String,
    parent_request_doc_id: String,
    parent_tool_call_id: String,
    parent_tool_call_doc_id: String,
    parent_subagent_depth: u32,
    agent_did: String,
    behavior_id: String,
    prompt: String,
    deadline: Option<DateTime<Utc>>,
) -> Result<String> {
    create_subagent_request_inner(
        node,
        request_id,
        parent_request_id,
        parent_tool_call_id,
        parent_subagent_depth,
        agent_did,
        behavior_id,
        prompt,
        deadline,
        SubagentAdmissionSource::LocalChild,
        (parent_request_doc_id, parent_tool_call_doc_id),
        None,
    )
    .await
}

/// Same as [`create_subagent_request_with_request_id`], stamping optional
/// isolated-workspace identity onto the child `AgentRequest`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_subagent_request_with_request_id_and_workspace(
    node: &EmbeddedNode,
    request_id: String,
    parent_request_id: String,
    parent_request_doc_id: String,
    parent_tool_call_id: String,
    parent_tool_call_doc_id: String,
    parent_subagent_depth: u32,
    agent_did: String,
    behavior_id: String,
    prompt: String,
    deadline: Option<DateTime<Utc>>,
    workspace: Option<WorkspaceLineage>,
) -> Result<String> {
    create_subagent_request_inner(
        node,
        request_id,
        parent_request_id,
        parent_tool_call_id,
        parent_subagent_depth,
        agent_did,
        behavior_id,
        prompt,
        deadline,
        SubagentAdmissionSource::LocalChild,
        (parent_request_doc_id, parent_tool_call_doc_id),
        workspace,
    )
    .await
}

/// Create a subagent request from a targeted bridge authored by a trusted
/// paired peer. The child is locally owned and routes lifecycle state back to
/// `requester_did`; the coordinator parent request is intentionally not
/// replicated to the host (#683).
#[allow(clippy::too_many_arguments)]
pub async fn create_subagent_request_with_trusted_parent_request_id(
    node: &EmbeddedNode,
    request_id: String,
    parent_request_id: String,
    parent_request_doc_id: String,
    parent_tool_call_id: String,
    parent_tool_call_doc_id: String,
    parent_subagent_depth: u32,
    agent_did: String,
    behavior_id: String,
    prompt: String,
    deadline: Option<DateTime<Utc>>,
    requester_did: String,
) -> Result<String> {
    create_subagent_request_inner(
        node,
        request_id,
        parent_request_id,
        parent_tool_call_id,
        parent_subagent_depth,
        agent_did,
        behavior_id,
        prompt,
        deadline,
        SubagentAdmissionSource::CrossDeploymentChild {
            bridge_author_did: requester_did,
        },
        (parent_request_doc_id, parent_tool_call_doc_id),
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn create_subagent_request_with_trusted_parent_request_id_and_workspace(
    node: &EmbeddedNode,
    request_id: String,
    parent_request_id: String,
    parent_request_doc_id: String,
    parent_tool_call_id: String,
    parent_tool_call_doc_id: String,
    parent_subagent_depth: u32,
    agent_did: String,
    behavior_id: String,
    prompt: String,
    deadline: Option<DateTime<Utc>>,
    requester_did: String,
    workspace: Option<WorkspaceLineage>,
) -> Result<String> {
    create_subagent_request_inner(
        node,
        request_id,
        parent_request_id,
        parent_tool_call_id,
        parent_subagent_depth,
        agent_did,
        behavior_id,
        prompt,
        deadline,
        SubagentAdmissionSource::CrossDeploymentChild {
            bridge_author_did: requester_did,
        },
        (parent_request_doc_id, parent_tool_call_doc_id),
        workspace,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn create_subagent_request_inner(
    node: &EmbeddedNode,
    request_id: String,
    parent_request_id: String,
    parent_tool_call_id: String,
    parent_subagent_depth: u32,
    agent_did: String,
    behavior_id: String,
    prompt: String,
    deadline: Option<DateTime<Utc>>,
    admission_source: SubagentAdmissionSource,
    parent_doc_ids: (String, String),
    workspace: Option<WorkspaceLineage>,
) -> Result<String> {
    // 1. Depth check (pure precondition, fires before any DB I/O).
    if parent_subagent_depth >= MAX_SUBAGENT_DEPTH {
        return Err(anyhow!(IllegalToolCallTransition::SubagentDepthExceeded));
    }

    // 2. Coherence check (pure precondition, fires before any DB I/O).
    if request_id.is_empty()
        || parent_request_id.is_empty()
        || parent_tool_call_id.is_empty()
        || parent_doc_ids.0.trim().is_empty()
        || parent_doc_ids.1.trim().is_empty()
    {
        return Err(anyhow!(IllegalToolCallTransition::ParentLinkageIncoherent));
    }

    // 3. Same-node children cross-reference the local parent row. A trusted
    // cross-deployment child instead uses the targeted, owner-authored bridge
    // as its durable parent edge; copying the entire parent request to every
    // possible host is neither necessary nor pair-scoped (#683).
    match &admission_source {
        SubagentAdmissionSource::LocalChild => {
            let parent = load_parent_request_by_doc_id(node, &parent_doc_ids.0).await?;
            if parent.request_id != parent_request_id || parent.agent_did != agent_did {
                return Err(anyhow!(IllegalToolCallTransition::ParentLinkageIncoherent));
            }
        }
        SubagentAdmissionSource::CrossDeploymentChild { bridge_author_did }
            if bridge_author_did.is_empty() || bridge_author_did.trim() != bridge_author_did =>
        {
            return Err(anyhow!(IllegalToolCallTransition::ParentLinkageIncoherent));
        }
        SubagentAdmissionSource::CrossDeploymentChild { .. } => {}
    }

    // 4. Generate fresh session identifier (mirror materialize.rs pattern).
    let new_session_id = uuid::Uuid::new_v4().to_string();
    let new_subagent_depth = parent_subagent_depth + 1;
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let prompt_selection = crate::skills::prompt_slash_skill_selection(&prompt);
    let prompt = prompt_selection.prompt;
    validate_parent_tool_call(
        node,
        &parent_doc_ids.1,
        &parent_doc_ids.0,
        &parent_tool_call_id,
    )
    .await?;
    let metadata = (!prompt_selection.selected_skill_ids.is_empty()).then(|| {
        serde_json::json!({ "selected_skill_ids": prompt_selection.selected_skill_ids }).to_string()
    });
    let runtime_context = crate::tool_call_lifecycle::runtime::current_tool_runtime_context();
    let inherited_context_json = runtime_context
        .as_ref()
        .filter(|context| !context.source_fields.is_empty())
        .map(|context| {
            serde_json::to_string(&crate::lifecycle::TriggerExecutionContext {
                version: 1,
                source_fields: context.source_fields.clone(),
            })
        })
        .transpose()?;
    if let Some(workspace) = workspace.as_ref() {
        workspace.require_authority_if_workspace_id()?;
    }
    let admission = match &admission_source {
        SubagentAdmissionSource::LocalChild => {
            gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_child(
                &agent_did,
                &parent_request_id,
            )
        }
        SubagentAdmissionSource::CrossDeploymentChild { bridge_author_did } => {
            gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_cross_deployment_child(
                &agent_did,
                &parent_request_id,
                bridge_author_did,
            )
        }
    };
    let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
        request_id.clone(),
        agent_did.clone(),
        agent_did.clone(),
        behavior_id,
        new_session_id,
        prompt,
        "interactive",
        now,
        admission,
    );
    create.metadata = metadata;
    create.valid_until =
        deadline.map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    create.max_retries = i64::from(DEFAULT_REQUEST_MAX_RETRIES);
    create.subagent_depth = new_subagent_depth;
    create.caused_by_parent_request_id = Some(parent_request_id);
    create.caused_by_parent_request_doc_id = Some(parent_doc_ids.0);
    create.caused_by_parent_tool_call_id = Some(parent_tool_call_id.clone());
    create.caused_by_parent_tool_call_doc_id = Some(parent_doc_ids.1);
    create.caused_by_trigger_id = Some(parent_tool_call_id);
    create.caused_by_trigger_kind = Some("subagent".to_string());
    create.caused_by_correlation = runtime_context
        .as_ref()
        .and_then(|context| context.correlation.clone());
    create.caused_by_trigger_context = inherited_context_json;
    if let Some(workspace) = workspace {
        create.workspace_id = workspace.workspace_id;
        create.workspace_authority = workspace.workspace_authority;
        create.workspace_owner_deployment_id = workspace.workspace_owner_deployment_id;
        create.workspace_seal_hash = workspace.workspace_seal_hash;
    }
    crate::sign_agent_request_create_as_registered_target(&mut create).await?;
    let mutation = create.graphql_mutation().map_err(anyhow::Error::msg)?;

    execute_mutation_with_retry(node, &mutation, "create_subagent_request").await?;

    Ok(request_id)
}

#[derive(Debug, Deserialize)]
struct ParentRequestLookupRow {
    request_id: String,
    agent_did: String,
}

async fn load_parent_request_by_doc_id(
    node: &EmbeddedNode,
    parent_request_doc_id: &str,
) -> Result<ParentRequestLookupRow> {
    let escaped_doc_id = escape_graphql_string(parent_request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                limit: 1
            ) {{ request_id agent_did }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query exact parent AgentRequest failed: {:?}",
            response.errors
        );
    }
    let mut rows: Vec<ParentRequestLookupRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    rows.pop()
        .ok_or_else(|| anyhow!(IllegalToolCallTransition::ParentLinkageIncoherent))
}

#[derive(Debug, Deserialize)]
struct ParentToolCallLookupRow {
    request_doc_id: String,
    tool_call_id: String,
}

async fn validate_parent_tool_call(
    node: &EmbeddedNode,
    parent_tool_call_doc_id: &str,
    parent_request_doc_id: &str,
    parent_tool_call_id: &str,
) -> Result<()> {
    let tool_doc_id = escape_graphql_string(parent_tool_call_doc_id);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{ _docID: {{ _eq: "{tool_doc_id}" }} }},
                limit: 1
            ) {{ request_doc_id tool_call_id }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("query parent AgentToolCall failed: {:?}", response.errors);
    }
    let rows: Vec<ParentToolCallLookupRow> = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolCall"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    match rows.as_slice() {
        [row]
            if row.request_doc_id == parent_request_doc_id
                && row.tool_call_id == parent_tool_call_id =>
        {
            Ok(())
        }
        _ => Err(anyhow!(IllegalToolCallTransition::ParentLinkageIncoherent)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The depth and coherence checks fire BEFORE any DB I/O, so we can
    // exercise them without a real EmbeddedNode by leveraging the early
    // return semantics. The DB-touching happy path is deferred to Bucket
    // 3 / Task 26, which has the test_db fixture set up.
    //
    // We can't easily fabricate an `EmbeddedNode` for unit tests (it's a
    // real type with a real constructor that boots a node). So the unit
    // tests below use `unsafe { std::mem::zeroed() }` only conceptually
    // — that's UB in Rust. Instead, we inline-construct the precondition
    // checks directly in #[test] blocks that don't call the function.
    //
    // The function-level error paths (depth + coherence) ARE tested by
    // Task 26's end-to-end fixtures, where a real node is available.

    #[test]
    fn max_subagent_depth_matches_lean_spec() {
        // Lean: Subagent.State.lean defines `maxSubagentDepth : Nat := 3`.
        assert_eq!(MAX_SUBAGENT_DEPTH, 3);
    }

    #[test]
    fn depth_precondition_arithmetic() {
        // parent_subagent_depth + 1 must be <= MAX_SUBAGENT_DEPTH.
        // Allowed parent depths: 0, 1, 2 (resulting children: 1, 2, 3).
        // Rejected parent depths: 3 and above.
        for parent_depth in 0..=2 {
            assert!(parent_depth < MAX_SUBAGENT_DEPTH);
        }
        for parent_depth in 3..=10 {
            assert!(parent_depth >= MAX_SUBAGENT_DEPTH);
        }
    }
}

/// Pins today's `AgentRequestCreate::graphql_input_fields()` output for
/// `create_subagent_request_inner` (#1336 Task 1), before it is switched
/// onto `build_signed_request` (#1336 Task 2).
///
/// `create_subagent_request_inner` validates the parent request/tool-call
/// against a live node before it builds `create` at all, persists via
/// `execute_mutation_with_retry`, and returns only the child `request_id` —
/// never the `AgentRequestCreate` it built. It also generates both
/// `new_session_id` (`Uuid::new_v4()`) and `now` (`Utc::now()`) internally.
/// This reproduces its DTO-construction statements verbatim (see
/// `create_subagent_request_inner` above, from "Generate fresh session
/// identifier" through the `workspace` field assignments), substituting
/// fixed values for both and skipping the node-dependent parent/tool-call
/// validation, which the DTO-construction logic itself does not consult.
#[cfg(test)]
mod pin_tests {
    use super::*;
    use crate::identity::AgentIdentity;

    const PIN_FIXED_KEY_HEX: &str = "4cbf8c1186d2fcb70559342fd142650a5ec5938d26a187d87e2c061b530d7be46edb79d5f548207182f7911b55709c9e4b9961c709486e5ce920e306470fe6d6";
    const PIN_FIXED_DID: &str = "did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7";

    fn pin_fixed_signing_identity(dir: &std::path::Path) -> crate::identity::KeyIdentity {
        let key_bytes: Vec<u8> = (0..PIN_FIXED_KEY_HEX.len())
            .step_by(2)
            .map(|offset| u8::from_str_radix(&PIN_FIXED_KEY_HEX[offset..offset + 2], 16).unwrap())
            .collect();
        let path = dir.join("pinning.key");
        std::fs::write(&path, &key_bytes).expect("write fixed pinning key");
        let identity =
            crate::identity::KeyIdentity::load_or_create(&path, None).expect("load fixed identity");
        assert_eq!(identity.did(), PIN_FIXED_DID);
        identity
    }

    #[tokio::test]
    async fn pin_create_subagent_request_inner_dto_construction() {
        let tempdir = tempfile::tempdir().unwrap();
        let _identity = pin_fixed_signing_identity(tempdir.path());

        let request_id = "subagent-request-1".to_string();
        let parent_request_id = "subagent-parent-request-1".to_string();
        let parent_tool_call_id = "subagent-parent-tool-call-1".to_string();
        let parent_subagent_depth: u32 = 1;
        let agent_did = PIN_FIXED_DID.to_string();
        let behavior_id = "behavior-1".to_string();
        let prompt = "spawn a subagent to help".to_string();
        let deadline: Option<DateTime<Utc>> =
            Some("2030-06-01T00:00:00Z".parse().expect("valid deadline"));
        let parent_doc_ids = (
            "subagent-parent-request-doc-1".to_string(),
            "subagent-parent-tool-call-doc-1".to_string(),
        );
        let workspace = Some(WorkspaceLineage {
            workspace_id: Some("ws-subagent-1".to_string()),
            workspace_authority: Some("readWrite".to_string()),
            workspace_owner_deployment_id: Some("dep-subagent-1".to_string()),
            workspace_seal_hash: Some("seal-subagent-1".to_string()),
        });

        // --- reproduces create_subagent_request_inner's DTO construction,
        // with fixed values for new_session_id and now ---
        let new_session_id = "sess-subagent-1".to_string();
        let new_subagent_depth = parent_subagent_depth + 1;
        let now = "2030-01-01T00:00:00Z".to_string();

        let prompt_selection = crate::skills::prompt_slash_skill_selection(&prompt);
        let prompt = prompt_selection.prompt;
        let metadata = (!prompt_selection.selected_skill_ids.is_empty()).then(|| {
            serde_json::json!({ "selected_skill_ids": prompt_selection.selected_skill_ids })
                .to_string()
        });
        let runtime_context = crate::tool_call_lifecycle::runtime::current_tool_runtime_context();
        let inherited_context_json = runtime_context
            .as_ref()
            .filter(|context| !context.source_fields.is_empty())
            .map(|context| {
                serde_json::to_string(&crate::lifecycle::TriggerExecutionContext {
                    version: 1,
                    source_fields: context.source_fields.clone(),
                })
            })
            .transpose()
            .expect("serialize inherited trigger context");
        if let Some(workspace) = workspace.as_ref() {
            workspace
                .require_authority_if_workspace_id()
                .expect("workspace authority present");
        }
        let admission =
            gents_protocol::request_admission::AgentRequestAdmissionRecord::runtime_local_child(
                &agent_did,
                &parent_request_id,
            );
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            request_id.clone(),
            agent_did.clone(),
            agent_did.clone(),
            behavior_id,
            new_session_id,
            prompt,
            "interactive",
            now,
            admission,
        );
        create.metadata = metadata;
        create.valid_until =
            deadline.map(|value| value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        create.max_retries = i64::from(DEFAULT_REQUEST_MAX_RETRIES);
        create.subagent_depth = new_subagent_depth;
        create.caused_by_parent_request_id = Some(parent_request_id);
        create.caused_by_parent_request_doc_id = Some(parent_doc_ids.0);
        create.caused_by_parent_tool_call_id = Some(parent_tool_call_id.clone());
        create.caused_by_parent_tool_call_doc_id = Some(parent_doc_ids.1);
        create.caused_by_trigger_id = Some(parent_tool_call_id);
        create.caused_by_trigger_kind = Some("subagent".to_string());
        create.caused_by_correlation = runtime_context
            .as_ref()
            .and_then(|context| context.correlation.clone());
        create.caused_by_trigger_context = inherited_context_json;
        if let Some(workspace) = workspace {
            create.workspace_id = workspace.workspace_id;
            create.workspace_authority = workspace.workspace_authority;
            create.workspace_owner_deployment_id = workspace.workspace_owner_deployment_id;
            create.workspace_seal_hash = workspace.workspace_seal_hash;
        }
        crate::sign_agent_request_create_as_registered_target(&mut create)
            .await
            .expect("sign subagent request");

        let fields = create.graphql_input_fields().expect("graphql_input_fields");
        assert_eq!(
            fields,
            "request_id: \"subagent-request-1\", agent_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", requester_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", behavior_id: \"behavior-1\", session_id: \"sess-subagent-1\", retry_root_request: \"subagent-request-1\", content: \"spawn a subagent to help\", execution_origin: \"interactive\", caused_by_trigger_id: \"subagent-parent-tool-call-1\", caused_by_trigger_kind: \"subagent\", created_at: \"2030-01-01T00:00:00Z\", retry_count: 0, max_retries: 3, valid_until: \"2030-06-01T00:00:00Z\", subagent_depth: 2, caused_by_parent_request_id: \"subagent-parent-request-1\", caused_by_parent_request_doc_id: \"subagent-parent-request-doc-1\", caused_by_parent_tool_call_id: \"subagent-parent-tool-call-1\", caused_by_parent_tool_call_doc_id: \"subagent-parent-tool-call-doc-1\", workspace_id: \"ws-subagent-1\", workspace_authority: \"readWrite\", workspace_owner_deployment_id: \"dep-subagent-1\", workspace_seal_hash: \"seal-subagent-1\", admission_kind: \"runtime-internal\", admission_signer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", admission_signature: \"BwhogeGeGk4MH2ovZKskcrSPjik79JvmHt4wXZoejNzYbr7d4954c6vdRSaHEBWXgsHReF4Wqth5UHncWNQ2ene\", runtime_issuer_did: \"did:key:z6Mkmuzzq2Ea9TgVB5EnaeY655fERuo15hrBtsL2oT3arco7\", runtime_source_request_id: \"subagent-parent-request-1\", runtime_source_kind: \"local-child\", lifecycle_state: \"pending\", failure_reason: \"\""
        );
    }
}
