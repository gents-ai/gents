use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, PayloadPresentation, PayloadRef,
    PresentedPayload, SourceClose, TranscriptMessage,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

use crate::identity::AgentIdentity;

use crate::lean_vocab_test::{
    CanonicalExecutionAdapter, ExecutionFuture, LeanCanonicalExecutionObservation,
    LeanCanonicalExecutionOperation, LeanCanonicalExecutionSeed, LeanCanonicalMessage,
    LeanCanonicalSegment, LeanCanonicalSource, LeanCanonicalToolAdmission, LeanCanonicalWriter,
    LeanMessageBlock, LeanMessagePublication, LeanMessageRole, LeanOutcome, LeanPayloadKind,
    LeanPayloadSpec, LeanPresentation, LeanPresentationPart, LeanResultPart, LeanTerminalSelection,
};

const FIXTURE_EPOCH_SECONDS: i64 = 1_700_000_000;

fn fixture_time(value: u64) -> Result<DateTime<Utc>> {
    fixture_time_at(FIXTURE_EPOCH_SECONDS, value)
}

fn fixture_time_at(epoch_seconds: i64, value: u64) -> Result<DateTime<Utc>> {
    let seconds = i64::try_from(value).context("modeled time exceeds native range")?;
    let timestamp = epoch_seconds
        .checked_add(seconds)
        .context("modeled time overflows native timestamp")?;
    DateTime::from_timestamp(timestamp, 0).context("modeled time overflows native timestamp")
}

fn symbolic_generation(value: u64) -> String {
    format!("lean-generation-{value}")
}

fn parse_generation(value: Option<&str>) -> Option<u64> {
    value?.strip_prefix("lean-generation-")?.parse().ok()
}

fn modeled_time_at(epoch_seconds: i64, value: &str) -> Result<u64> {
    let elapsed = DateTime::parse_from_rfc3339(value)?
        .timestamp()
        .checked_sub(epoch_seconds)
        .context("native timestamp differs beyond modeled range")?;
    u64::try_from(elapsed).context("native timestamp precedes fixture epoch")
}

fn modeled_principal_did(
    symbolic: u64,
    local: u64,
    physical_local: &str,
    remote_dids: &HashMap<u64, String>,
) -> String {
    if symbolic == local {
        physical_local.to_owned()
    } else {
        remote_dids
            .get(&symbolic)
            .cloned()
            .unwrap_or_else(|| format!("did:test:lean:principal-{symbolic}"))
    }
}

fn seed_workspace_lineage(
    seed: &LeanCanonicalExecutionSeed,
    physical_local: &str,
    remote_dids: &HashMap<u64, String>,
) -> Option<crate::lifecycle::WorkspaceLineage> {
    seed.workspace
        .as_ref()
        .map(|workspace| crate::lifecycle::WorkspaceLineage {
            workspace_id: Some(format!("lean-workspace-{}", workspace.workspace_id)),
            workspace_owner_agent_did: Some(modeled_principal_did(
                workspace.workspace_owner_agent_did,
                seed.principal,
                physical_local,
                remote_dids,
            )),
            workspace_authority: Some(workspace.workspace_authority.clone()),
            workspace_seal_hash: workspace
                .workspace_seal_hash
                .map(|seal| format!("lean-seal-{seal}")),
        })
}

fn seed_delegated_workspace(
    seed: &LeanCanonicalExecutionSeed,
    physical_local: &str,
    remote_dids: &HashMap<u64, String>,
) -> Option<gents_protocol::output::DelegatedWorkspace> {
    let lineage = seed_workspace_lineage(seed, physical_local, remote_dids)?;
    Some(gents_protocol::output::DelegatedWorkspace {
        workspace_id: lineage.workspace_id?,
        workspace_owner_agent_did: lineage.workspace_owner_agent_did?,
        workspace_authority: lineage.workspace_authority?,
        workspace_seal_hash: lineage.workspace_seal_hash,
    })
}

async fn seed_workspace_documents(
    node: &EmbeddedNode,
    seed: &LeanCanonicalExecutionSeed,
    principal: &str,
    remote_dids: &HashMap<u64, String>,
) -> Result<Option<tempfile::TempDir>> {
    let Some(lineage) = seed_workspace_lineage(seed, principal, remote_dids) else {
        return Ok(None);
    };
    let workspace_id = lineage
        .workspace_id
        .context("modeled workspace omitted identity")?;
    let owner = lineage
        .workspace_owner_agent_did
        .context("modeled workspace omitted owner")?;
    let path_guard = tempfile::tempdir().context("native workspace placement directory")?;
    let host_path = path_guard.path().join("workspace");
    std::fs::create_dir(&host_path).context("create native workspace placement")?;
    let workspace = crate::workspace::IsolatedWorkspaceDoc {
        path_capability: crate::workspace::WorkspacePathCapability::exact_paths(vec![])?,
        workspace_id: workspace_id.clone(),
        work_unit_id: format!("lean-work-unit-{}", seed.request_id),
        repository_id: format!("lean-repository-{}", seed.request_id),
        base_sha: "lean-base".to_owned(),
        branch: format!("lean-branch-{}", seed.request_id),
        creation_policy: "git_worktree_diff".to_owned(),
        adapter: "git_worktree".to_owned(),
        owner_agent_did: owner.clone(),
        writer_principal: principal.to_owned(),
        integrator_principal: principal.to_owned(),
        instruction_manifest: "{}".to_owned(),
        seal_hash: lineage.workspace_seal_hash,
        lifecycle_state: "ready".to_owned(),
        caused_by_invocation_id: format!("lean-invocation-{}", seed.request_id),
        caused_by_correlation: format!("lean-correlation-{}", seed.request_id),
    };
    let placement = crate::workspace::WorkspacePlacementDoc {
        workspace_id,
        owner_agent_did: owner,
        host_path: host_path.to_string_lossy().into_owned(),
        repository_placement_id: format!("lean-repository-placement-{}", seed.request_id),
        adapter: "git_worktree".to_owned(),
        adapter_version: "1".to_owned(),
        dirty_base: false,
        dirty_base_summary: String::new(),
        provisioning_state: "ready".to_owned(),
        observed_tree_hash: String::new(),
    };
    for mutation in [
        crate::workspace::isolated_workspace_upsert_mutation(&workspace),
        crate::workspace::workspace_placement_upsert_mutation(
            &placement,
            &Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
    ] {
        let response = node.execute(&mutation).await;
        anyhow::ensure!(
            !response.has_errors(),
            "seed native workspace documents: {:?}",
            response.errors
        );
    }
    Ok(Some(path_guard))
}

async fn install_signed_ancestor_target(node: &EmbeddedNode, principal: &str) -> Result<()> {
    use crate::config_client::{ConfigAccess, DesiredStateApplyDocument, DesiredStateApplyPlan};
    let tools = serde_json::json!({
        "agent_did": principal,
        "tools_id": "general:tools",
        "subagents": {"spawn_enabled": true, "target_ids": ["lean-child-target"]},
    });
    let target = serde_json::json!({
        "target_id": "lean-child-target",
        "agent_did": principal,
        "target_agent_did": principal,
        "behavior_id": "general",
        "name": "child",
    });
    let plan = DesiredStateApplyPlan::new(vec![
        DesiredStateApplyDocument {
            collection: crate::Collection::Tools,
            add: tools.clone(),
            update: tools,
        },
        DesiredStateApplyDocument {
            collection: crate::Collection::SubagentTarget,
            add: target.clone(),
            update: target,
        },
    ])?;
    ConfigAccess::transact_local(node, None, "lean.signed_ancestor_target", |txn| {
        let plan = &plan;
        Box::pin(async move { crate::config_client::apply_desired_state_plan(txn, plan).await })
    })
    .await?;
    Ok(())
}

/// Build every nonzero depth through a signed root, accepted native spawn
/// bridges, and the production child-request owner. Only the last child is
/// left pending for the modeled durable claim in `initialize`.
async fn seed_signed_ancestor_chain(
    node: &Arc<EmbeddedNode>,
    identity: &Arc<dyn AgentIdentity>,
    seed: &LeanCanonicalExecutionSeed,
    remote_dids: &HashMap<u64, String>,
) -> Result<(String, String)> {
    let target_depth = u32::try_from(seed.subagent_depth).context("modeled depth exceeds u32")?;
    anyhow::ensure!(
        target_depth > 0 && target_depth <= crate::tool_call_lifecycle::MAX_SUBAGENT_DEPTH,
        "native ancestor fixture requires a supported nonzero depth"
    );
    let principal = identity.did().to_owned();
    let root_id = format!("lean-ancestor-{}-0", seed.request_id);
    let root_session_id = format!("lean-ancestor-session-{}", seed.request_id);
    let root_spec = crate::lifecycle::RequestSpec {
        workspace: seed_workspace_lineage(seed, &principal, remote_dids),
        ..crate::lifecycle::RequestSpec::new(
            crate::lifecycle::RequestIdentity {
                requester_did: None,
                request_id: root_id.clone(),
                agent_did: principal.clone(),
                behavior_id: "general".to_owned(),
                session_id: root_session_id,
                content: "lean ancestor".to_owned(),
                execution_origin: crate::lifecycle::ExecutionOrigin::Interactive,
                created_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            },
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(&principal),
        )
    };
    let root = crate::lifecycle::build_signed_request(
        root_spec,
        crate::lifecycle::RequestSigner::Identity(identity.as_ref()),
    )
    .await?;
    let response = node
        .execute(&root.graphql_mutation().map_err(anyhow::Error::msg)?)
        .await;
    anyhow::ensure!(
        !response.has_errors(),
        "create signed ancestor: {:?}",
        response.errors
    );
    let root_doc_id = crate::graphql::single_mutation_document(&response, "create_AgentRequest")?
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .context("signed ancestor omitted physical identity")?
        .to_owned();
    let root_row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {} }} }}"#,
            crate::graphql::escape_graphql_string(&root_doc_id),
            crate::watcher::AGENT_REQUEST_FIELDS,
        ))
        .await;
    anyhow::ensure!(
        !root_row.has_errors(),
        "read signed ancestor: {:?}",
        root_row.errors
    );
    let root_row = crate::graphql::first_row::<AgentRequestRow>(&root_row, "AgentRequest")?
        .context("signed ancestor disappeared")?;
    let queued: crate::watcher::AgentRequest = root_row.try_into()?;
    let verified = crate::request_admission::verify_fresh_local_self_request(
        node,
        identity.as_ref(),
        &queued,
        "general",
    )
    .await
    .map_err(anyhow::Error::from)?;
    let mut parent = crate::lifecycle::RequestLifecycle::new_with_agent_did(
        node.clone(),
        "general",
        &principal,
        verified,
        300,
    );
    anyhow::ensure!(
        parent.claim().await? == crate::lifecycle::ClaimOutcome::Claimed,
        "signed ancestor was not claimable"
    );

    for parent_depth in 0..target_depth {
        let tool_call_id = format!("lean-ancestor-tool-{}-{parent_depth}", seed.request_id);
        let child_id = if parent_depth + 1 == target_depth {
            format!("lean-request-{}", seed.request_id)
        } else {
            format!("lean-ancestor-{}-{}", seed.request_id, parent_depth + 1)
        };
        let tool =
            crate::tool_call_lifecycle::admission_fixture::publish_accepted_on_claimed_request(
                node.clone(),
                &mut parent,
                &principal,
                0,
                crate::toolset::SPAWN_SUBAGENT_TOOL_NAME,
                &tool_call_id,
                serde_json::json!({"name":"child", "prompt":"work", "await_mode":"background"}),
                Some(crate::streaming::SpawnAdmissionPlan {
                    tool_call_id: tool_call_id.clone(),
                    child_request_id: child_id.clone(),
                    spawn_target_did: principal.clone(),
                    spawn_behavior_id: "general".to_owned(),
                    delegated_workspace: seed_delegated_workspace(seed, &principal, remote_dids),
                    await_mode: crate::tool_call_lifecycle::AwaitMode::Background,
                }),
                crate::tool_call_lifecycle::AwaitMode::Background,
                crate::tool_call_lifecycle::CancelPolicy::Cascade,
                false,
            )
            .await?;
        let parent_doc_id = parent.request().doc_id.clone();
        let parent_id = parent.request().request_id.clone();
        let tool_doc_id = tool
            .doc_id()
            .context("ancestor spawn omitted tool identity")?
            .to_owned();
        crate::tool_call_lifecycle::create_subagent_request_with_request_id_and_workspace(
            node,
            child_id.clone(),
            parent_id,
            parent_doc_id,
            tool_call_id,
            tool_doc_id,
            parent_depth,
            principal.clone(),
            "general".to_owned(),
            "work".to_owned(),
            None,
            seed_workspace_lineage(seed, &principal, remote_dids),
        )
        .await?;
        let child = node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{ {} }} }}"#,
            crate::graphql::escape_graphql_string(&child_id),
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        anyhow::ensure!(!child.has_errors(), "read signed child: {:?}", child.errors);
        let child = crate::graphql::first_row::<AgentRequestRow>(&child, "AgentRequest")?
            .context("signed child disappeared")?;
        anyhow::ensure!(
            child.subagent_depth == Some(i64::from(parent_depth + 1)),
            "signed child owner did not advance depth"
        );
        if parent_depth + 1 == target_depth {
            return Ok((
                child
                    .doc_id
                    .context("signed child omitted physical identity")?,
                child
                    .session_id
                    .context("signed child omitted session identity")?,
            ));
        }
        let queued: crate::watcher::AgentRequest = child.try_into()?;
        let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity.clone(),
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        );
        let verified = verifier
            .verify_fresh(&queued, "general")
            .await
            .map_err(anyhow::Error::from)?;
        parent = crate::lifecycle::RequestLifecycle::new_with_agent_did(
            node.clone(),
            "general",
            &principal,
            verified,
            300,
        );
        anyhow::ensure!(
            parent.claim().await? == crate::lifecycle::ClaimOutcome::Claimed,
            "signed intermediate child was not claimable"
        );
    }
    unreachable!("nonzero depth returns its final child")
}

// Native session sequences are one-based; the Lean allocator starts at zero.
// Normalize only the representation, preserving order and reservation gaps.
fn modeled_sequence(value: u64) -> Result<u64> {
    value
        .checked_sub(1)
        .context("native sequence must be positive")
}

fn native_sequence(value: u64) -> Result<u32> {
    u32::try_from(value.checked_add(1).context("modeled sequence overflow")?)
        .context("native sequence exceeds u32")
}

pub(crate) struct NativeCanonicalExecutionAdapter;

pub(crate) struct NativeCanonicalExecution {
    node: Arc<EmbeddedNode>,
    _identity_guard: tempfile::TempDir,
    remote_dids: HashMap<u64, String>,
    _workspace_guard: Option<tempfile::TempDir>,
    fixture_epoch_seconds: i64,
    request_doc_id: String,
    request_id: u64,
    principal_id: u64,
    subagent_depth: u64,
    query_document: u64,
    remote_routes: Vec<crate::lean_vocab_test::LeanCanonicalRemoteRoute>,
    transcript_session_id: u64,
    next_sequence: u64,
    session_id: String,
    principal: String,
    segments: Vec<LeanCanonicalSegment>,
    messages: Vec<LeanCanonicalMessage<LeanPayloadSpec>>,
    tool_state: Option<String>,
    physical_tool_request: Option<u64>,
    segment_ids: HashMap<String, u64>,
    message_ids: HashMap<String, u64>,
    modeled_message_keys: HashMap<String, String>,
    canonical_message_keys: HashMap<String, String>,
    tool_ids: HashMap<String, u64>,
    accepted_spawns: HashMap<
        u64,
        (
            crate::streaming::AcceptedToolCall,
            u64,
            crate::tool_call_lifecycle::AwaitMode,
            crate::tool_call_lifecycle::CancelPolicy,
        ),
    >,
}

impl NativeCanonicalExecution {
    fn fixture_time(&self, value: u64) -> Result<DateTime<Utc>> {
        fixture_time_at(self.fixture_epoch_seconds, value)
    }

    fn modeled_time(&self, value: &str) -> Result<u64> {
        modeled_time_at(self.fixture_epoch_seconds, value)
    }

    async fn revoke_corrupt(
        &mut self,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: &str,
        selection: &LeanTerminalSelection,
    ) -> Result<LeanCanonicalExecutionObservation> {
        anyhow::ensure!(
            outcome == "dead",
            "modeled corrupt revocation outcome changed"
        );
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let response = self.node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request}" }} }}, limit: 1) {{ {} lifecycle_state }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        anyhow::ensure!(
            !response.has_errors(),
            "read revocation request: {:?}",
            response.errors
        );
        let observed = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
            .context("native revocation request disappeared")?;
        let result = crate::lifecycle::revoke_execution_preserving_output_at(
            &self.node,
            &observed,
            crate::lifecycle::RequestTerminalOutcome::Dead,
            "canonical output integrity failure",
            &symbolic_generation(expected_generation),
            &symbolic_generation(fresh_generation),
            self.fixture_time(now)?,
        )
        .await?;
        if matches!(result, crate::lifecycle::TerminalizeResult::Lost) {
            return self.observe(false).await;
        }
        let response = self.node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request}" }} }}, limit: 1) {{ {} lifecycle_state terminal_output }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        anyhow::ensure!(
            !response.has_errors(),
            "read revoked request: {:?}",
            response.errors
        );
        let persisted = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
            .context("native revoked request disappeared")?;
        let expected_selection = match selection {
            LeanTerminalSelection::NoMessage => gents_protocol::output::TerminalOutput::NoMessage,
            LeanTerminalSelection::Message { id } => {
                let physical = self
                    .message_ids
                    .iter()
                    .find_map(|(physical, symbolic)| (*symbolic == *id).then_some(physical.clone()))
                    .context("modeled corrupt revocation header has no physical identity")?;
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: physical,
                }
            }
        };
        anyhow::ensure!(
            persisted.terminal_output.as_ref() == Some(&expected_selection),
            "native revocation selected a different immutable header: expected {expected_selection:?}, got {:?}",
            persisted.terminal_output
        );
        self.observe(true).await
    }

    async fn complete_foreground(
        &mut self,
        now: u64,
        document: u64,
        authority_outcome: &str,
        record: &LeanCanonicalSegment,
        message: &LeanCanonicalMessage<LeanPayloadSpec>,
    ) -> Result<LeanCanonicalExecutionObservation> {
        anyhow::ensure!(
            authority_outcome == "complete",
            "native completion requires complete authority"
        );
        anyhow::ensure!(
            matches!(record.coordinate.source, LeanCanonicalSource::Tool { call } if call == document)
                && matches!(record.writer, LeanCanonicalWriter::Tool { call } if call == document),
            "modeled tool closure is not bound to selected physical tool"
        );
        let raw = String::from_utf8(
            record
                .flush
                .as_ref()
                .context("modeled tool closure omitted raw flush")?
                .payload
                .clone(),
        )?;
        let [LeanMessageBlock::ToolResult { doc_id, parts, .. }] = message.blocks.as_slice() else {
            anyhow::bail!("modeled native completion has no sole ToolResult block")
        };
        anyhow::ensure!(*doc_id == document, "modeled result refers to another tool");
        let [LeanResultPart::Text { payload }] = parts.as_slice() else {
            anyhow::bail!("modeled native completion has no sole text result")
        };
        anyhow::ensure!(
            payload.reference.close_id == record.id && payload.reference.stream == 0,
            "modeled result does not reference the closing tool stream"
        );
        let presentation = match &payload.presentation {
            LeanPresentation::Full => PayloadPresentation::Full,
            LeanPresentation::Composed { parts } => PayloadPresentation::Composed {
                parts: parts
                    .iter()
                    .map(|part| -> Result<_> {
                        Ok(match part {
                            LeanPresentationPart::Range { start, end } => {
                                gents_protocol::output::PresentationPart::OutputRange {
                                    start_byte: *start,
                                    end_byte: *end,
                                }
                            }
                            LeanPresentationPart::Literal { bytes } => {
                                gents_protocol::output::PresentationPart::Literal {
                                    text: String::from_utf8(bytes.clone())?,
                                }
                            }
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
            },
        };
        let rendered =
            crate::tool_call_lifecycle::delivery::render_presentation(&raw, &presentation)?;
        let physical = self.physical_tool(document)?.to_owned();
        let mut tool = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
            self.node.clone(),
            &physical,
            &self.principal,
            &self.session_id,
            Some(&self.principal),
        )
        .await?
        .context("modeled tool disappeared before atomic completion")?;
        let accepted = tool
            .complete_raw_with_presentation_at(
                &raw,
                &rendered,
                presentation,
                self.fixture_time(now)?,
            )
            .await?;
        if !accepted {
            return self.observe(false).await;
        }
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let segments = self.node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS,
        )).await;
        anyhow::ensure!(
            !segments.has_errors(),
            "read native completed tool source: {:?}",
            segments.errors
        );
        let closing = segments
            .data
            .as_ref()
            .and_then(|data| data.get("AgentOutputSegment"))
            .and_then(serde_json::Value::as_array)
            .context("native completed tool source query omitted rows")?
            .iter()
            .map(crate::session::canonical_rows::decode_output_segment_row)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|row| {
                matches!(&row.segment.source,
                gents_protocol::output::OutputSource::ToolCall { tool_call_doc_id }
                    if tool_call_doc_id == &physical)
                    && row.segment.close.is_some()
            })
            .collect::<Vec<_>>();
        let [closing] = closing.as_slice() else {
            anyhow::bail!("atomic tool completion did not persist one exact source closure")
        };
        self.segment_ids.insert(closing.doc_id.clone(), record.id);
        let messages = self.node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
        )).await;
        anyhow::ensure!(
            !messages.has_errors(),
            "read native completed tool result: {:?}",
            messages.errors
        );
        let delivered = messages
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(serde_json::Value::as_array)
            .context("native completed tool result query omitted rows")?
            .iter()
            .map(crate::session::canonical_rows::decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter(|row| {
                matches!(&row.message.publication,
                MessagePublication::ToolDelivery { tool_call_doc_id }
                    if tool_call_doc_id == &physical)
            })
            .collect::<Vec<_>>();
        let [delivered] = delivered.as_slice() else {
            anyhow::bail!("atomic tool completion did not publish one exact native result")
        };
        self.message_ids
            .insert(delivered.doc_id.clone(), message.header.id);
        self.modeled_message_keys
            .insert(delivered.doc_id.clone(), message.key.clone());
        self.observe(true).await
    }

    async fn recover_terminal(
        &mut self,
        now: u64,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: &str,
        selection: &LeanTerminalSelection,
        items: &[crate::lean_vocab_test::LeanCanonicalRecoveryItem],
    ) -> Result<LeanCanonicalExecutionObservation> {
        if outcome == "interrupted" {
            crate::interrupt::interrupt_request_by_doc_id(
                &self.node,
                &self.request_doc_id,
                &self.principal,
                Some(&self.principal),
            )
            .await?;
        }
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let response = self.node.execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request}" }} }}, limit: 1) {{ {} lifecycle_state }} }}"#,
            crate::watcher::AGENT_REQUEST_FIELDS,
        )).await;
        anyhow::ensure!(
            !response.has_errors(),
            "read recovery request: {:?}",
            response.errors
        );
        let observed = crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
            .context("native recovery request disappeared")?;
        let expiry = observed
            .execution_lease_expires_at
            .as_deref()
            .context("native recovery request omitted deadline")?;
        let choice = match selection {
            LeanTerminalSelection::NoMessage => {
                crate::lifecycle::RecoverySelectionChoice::NoMessage
            }
            LeanTerminalSelection::Message { id } => {
                let accepted_key = self.message_ids.iter().find_map(|(physical, symbolic)| {
                    (*symbolic == *id)
                        .then(|| self.canonical_message_keys.get(physical))
                        .flatten()
                        .cloned()
                });
                let key = accepted_key
                    .or(items
                        .iter()
                        .find_map(|item| {
                            (item.message.as_ref()?.header.id == *id).then_some(&item.closing)
                        })
                        .map(|closing| -> Result<String> {
                            let source =
                                self.provider_segment(closing, expected_generation)?.source;
                            crate::streaming::canonical::partial_message_key(
                                &self.request_doc_id,
                                &source,
                            )
                        })
                        .transpose()?)
                    .unwrap_or_else(|| format!("lean-unmapped-recovery-message-{id}"));
                crate::lifecycle::RecoverySelectionChoice::MessageKey(key)
            }
        };
        let result = crate::lifecycle::recover_expired_generation_with_facts(
            &self.node,
            &observed,
            &symbolic_generation(expected_generation),
            expiry,
            symbolic_generation(fresh_generation),
            self.fixture_time(now)?,
            Some(choice),
            Some(if outcome == "interrupted" {
                gents_protocol::request_lifecycle::RequestLifecycleState::Interrupted
            } else {
                gents_protocol::request_lifecycle::RequestLifecycleState::Failed
            }),
        )
        .await;
        match result {
            Ok(crate::lifecycle::RecoveryResult::Lost) => return self.observe(false).await,
            Err(error)
                if error
                    .downcast_ref::<crate::lifecycle::RecoverySelectionRejected>()
                    .is_some()
                    || error
                        .downcast_ref::<gents_protocol::output::ReconstructionError>()
                        .is_some() =>
            {
                return self.observe(false).await;
            }
            Err(error) => return Err(error.context("native terminal recovery owner failed")),
            Ok(crate::lifecycle::RecoveryResult::Won { .. }) => {}
        }
        let segments = self.node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS,
        )).await;
        anyhow::ensure!(
            !segments.has_errors(),
            "read recovered segments: {:?}",
            segments.errors
        );
        for row in segments
            .data
            .as_ref()
            .and_then(|data| data.get("AgentOutputSegment"))
            .and_then(serde_json::Value::as_array)
            .context("recovered segment query omitted rows")?
        {
            let row = crate::session::canonical_rows::decode_output_segment_row(row)?;
            if self.segment_ids.contains_key(&row.doc_id) {
                continue;
            }
            let matches = items
                .iter()
                .filter(|item| {
                    self.provider_segment(&item.closing, expected_generation)
                        .is_ok_and(|closing| {
                            closing.source == row.segment.source && row.segment.close.is_some()
                        })
                })
                .collect::<Vec<_>>();
            let [item] = matches.as_slice() else {
                anyhow::bail!("native recovery produced an unmapped or ambiguous closure")
            };
            self.segment_ids.insert(row.doc_id, item.closing.id);
        }
        let messages = self.node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
        )).await;
        anyhow::ensure!(
            !messages.has_errors(),
            "read recovered headers: {:?}",
            messages.errors
        );
        for row in messages
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(serde_json::Value::as_array)
            .context("recovered header query omitted rows")?
        {
            let row = crate::session::canonical_rows::decode_transcript_message_row(row)?;
            if self.message_ids.contains_key(&row.doc_id) {
                continue;
            }
            let matching = items
                .iter()
                .filter_map(|item| {
                    let message = item.message.as_ref()?;
                    let source = self
                        .provider_segment(&item.closing, expected_generation)
                        .ok()?
                        .source;
                    let key = crate::streaming::canonical::partial_message_key(
                        &self.request_doc_id,
                        &source,
                    )
                    .ok()?;
                    (key == row.message.message_key).then_some(message)
                })
                .collect::<Vec<_>>();
            let [message] = matching.as_slice() else {
                anyhow::bail!("native recovery produced an unmapped or ambiguous header")
            };
            self.message_ids
                .insert(row.doc_id.clone(), message.header.id);
            self.modeled_message_keys
                .insert(row.doc_id.clone(), message.key.clone());
            self.canonical_message_keys
                .insert(row.doc_id, row.message.message_key);
        }
        self.observe(true).await
    }

    fn physical_tool(&self, symbolic: u64) -> Result<&str> {
        self.tool_ids
            .iter()
            .find_map(|(physical, mapped)| (*mapped == symbolic).then_some(physical.as_str()))
            .context("modeled tool has no accepted physical lifecycle")
    }

    async fn refresh_output(&mut self) -> Result<()> {
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let segments = self.node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS,
        )).await;
        anyhow::ensure!(
            !segments.has_errors(),
            "read native output segments: {:?}",
            segments.errors
        );
        let mut decoded_segments = segments
            .data
            .as_ref()
            .and_then(|data| data.get("AgentOutputSegment"))
            .and_then(serde_json::Value::as_array)
            .context("native segment query omitted rows")?
            .iter()
            .map(crate::session::canonical_rows::decode_output_segment_row)
            .collect::<Result<Vec<_>>>()?;
        decoded_segments.sort_by_key(|row| {
            self.segment_ids
                .get(&row.doc_id)
                .copied()
                .unwrap_or(u64::MAX)
        });
        self.segments = decoded_segments
            .iter()
            .map(|row| {
                let id = self
                    .segment_ids
                    .get(&row.doc_id)
                    .copied()
                    .context("native segment has no symbolic identity")?;
                self.lean_segment(id, &row.segment)
            })
            .collect::<Result<Vec<_>>>()?;

        let messages = self.node.execute(&format!(
            r#"{{ AgentMessage(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
        )).await;
        anyhow::ensure!(
            !messages.has_errors(),
            "read native message headers: {:?}",
            messages.errors
        );
        let mut decoded_messages = messages
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(serde_json::Value::as_array)
            .context("native message query omitted rows")?
            .iter()
            .map(crate::session::canonical_rows::decode_transcript_message_row)
            .collect::<Result<Vec<_>>>()?;
        decoded_messages.sort_by_key(|row| row.message.sequence);
        self.messages = decoded_messages
            .iter()
            .map(|row| {
                let id = self
                    .message_ids
                    .get(&row.doc_id)
                    .copied()
                    .context("native header has no symbolic identity")?;
                let mut message = self.lean_message(id, &row.message)?;
                if let Some(modeled_key) = self.modeled_message_keys.get(&row.doc_id) {
                    message.key.clone_from(modeled_key);
                }
                Ok(message)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(())
    }

    fn lean_segment(
        &self,
        id: u64,
        segment: &gents_protocol::output::OutputSegment,
    ) -> Result<LeanCanonicalSegment> {
        let source = match &segment.source {
            gents_protocol::output::OutputSource::ProviderTurn {
                scope,
                turn_index,
                attempt,
            } => {
                let scope = scope
                    .to_string()
                    .strip_prefix("inference.")
                    .context("native provider scope is not modeled")?
                    .parse()?;
                LeanCanonicalSource::Provider {
                    scope,
                    turn: u64::from(*turn_index),
                    attempt: u64::from(*attempt),
                }
            }
            gents_protocol::output::OutputSource::ToolCall { tool_call_doc_id } => {
                LeanCanonicalSource::Tool {
                    call: self
                        .tool_ids
                        .get(tool_call_doc_id)
                        .copied()
                        .context("native tool source has no symbolic identity")?,
                }
            }
            _ => anyhow::bail!("native adapter observed unsupported segment source"),
        };
        let writer = match &segment.writer {
            gents_protocol::output::OutputWriter::RequestExecution {
                execution_generation,
            } => LeanCanonicalWriter::Request {
                generation: parse_generation(Some(execution_generation))
                    .context("native segment generation is not symbolic")?,
            },
            gents_protocol::output::OutputWriter::ToolExecution { tool_call_doc_id } => {
                LeanCanonicalWriter::Tool {
                    call: self
                        .tool_ids
                        .get(tool_call_doc_id)
                        .copied()
                        .context("native tool writer has no symbolic identity")?,
                }
            }
        };
        let flush = segment
            .ordinal
            .map(|ordinal| -> Result<_> {
                Ok(crate::lean_vocab_test::LeanCanonicalFlush {
                    ordinal: u64::from(ordinal),
                    runs: segment
                        .runs
                        .iter()
                        .map(|run| {
                            Ok(crate::lean_vocab_test::LeanCanonicalRun {
                                stream: u64::from(run.stream),
                                bytes: u64::from(run.bytes),
                                declaration: run
                                    .declaration
                                    .as_ref()
                                    .map(|declaration| {
                                        let (kind, tool) = match &declaration.payload {
                                        gents_protocol::output::StreamPayload::Text => {
                                            (LeanPayloadKind::Text, None)
                                        }
                                        gents_protocol::output::StreamPayload::Reasoning => {
                                            (LeanPayloadKind::Reasoning, None)
                                        }
                                        gents_protocol::output::StreamPayload::ReasoningSummary => {
                                            (LeanPayloadKind::Summary, None)
                                        }
                                        gents_protocol::output::StreamPayload::ReasoningOpaque => {
                                            (LeanPayloadKind::Opaque, None)
                                        }
                                        gents_protocol::output::StreamPayload::ToolArguments {
                                            id,
                                            call_id,
                                            name,
                                        } => (
                                            LeanPayloadKind::Arguments,
                                            Some(crate::lean_vocab_test::LeanToolIdentity {
                                                id: id.clone(),
                                                call_id: call_id.clone(),
                                                name: name.clone(),
                                            }),
                                        ),
                                        gents_protocol::output::StreamPayload::ToolOutput => {
                                            (LeanPayloadKind::ToolOutput, None)
                                        }
                                        _ => anyhow::bail!(
                                            "native adapter observed unsupported stream payload"
                                        ),
                                    };
                                        Ok(crate::lean_vocab_test::LeanCanonicalDeclaration {
                                            block: u64::from(declaration.block_index),
                                            part: u64::from(declaration.part_index),
                                            kind,
                                            tool,
                                            media_kind: None,
                                        })
                                    })
                                    .transpose()?,
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                    payload: segment.payload.as_bytes().to_vec(),
                })
            })
            .transpose()?;
        let close = segment.close.as_ref().map(|close| match close {
            SourceClose::Closed {
                outcome,
                segments,
                stream_bytes,
            } => crate::lean_vocab_test::LeanCanonicalClosure::Closed {
                outcome: match outcome {
                    OutputOutcome::Complete => LeanOutcome::Complete,
                    OutputOutcome::Partial => LeanOutcome::Partial,
                },
                segments: u64::from(*segments),
                stream_bytes: stream_bytes.clone(),
            },
            SourceClose::Retracted => crate::lean_vocab_test::LeanCanonicalClosure::Retracted,
        });
        Ok(LeanCanonicalSegment {
            id,
            coordinate: crate::lean_vocab_test::LeanCanonicalCoordinate {
                request: self.symbolic_request()?,
                source,
            },
            writer,
            flush,
            close,
            created_at: self.modeled_time(&segment.created_at)?,
        })
    }

    fn symbolic_request(&self) -> Result<u64> {
        Ok(self.request_id)
    }

    fn lean_message(
        &self,
        id: u64,
        message: &TranscriptMessage,
    ) -> Result<LeanCanonicalMessage<LeanPayloadSpec>> {
        let payload_ref = |reference: &PayloadRef| -> Result<LeanPayloadSpec> {
            Ok(LeanPayloadSpec {
                reference: crate::lean_vocab_test::LeanPayloadRef {
                    close_id: self
                        .segment_ids
                        .get(&reference.close_doc_id)
                        .copied()
                        .context("native payload reference has no symbolic closure identity")?,
                    stream: u64::from(reference.stream),
                },
                presentation: LeanPresentation::Full,
            })
        };
        let presented_payload = |value: &PresentedPayload| -> Result<LeanPayloadSpec> {
            let mut payload = payload_ref(&value.output)?;
            payload.presentation = match &value.presentation {
                PayloadPresentation::Full => LeanPresentation::Full,
                PayloadPresentation::Composed { parts } => LeanPresentation::Composed {
                    parts: parts
                        .iter()
                        .map(|part| match part {
                            gents_protocol::output::PresentationPart::OutputRange {
                                start_byte,
                                end_byte,
                            } => LeanPresentationPart::Range {
                                start: *start_byte,
                                end: *end_byte,
                            },
                            gents_protocol::output::PresentationPart::Literal { text } => {
                                LeanPresentationPart::Literal {
                                    bytes: text.as_bytes().to_vec(),
                                }
                            }
                        })
                        .collect(),
                },
            };
            Ok(payload)
        };
        let blocks = message
            .blocks
            .iter()
            .map(|block| -> Result<LeanMessageBlock<LeanPayloadSpec>> {
                Ok(match block {
                    MessageBlock::Text { text } => {
                        anyhow::ensure!(
                            matches!(text.presentation, PayloadPresentation::Full),
                            "native text presentation is not modeled"
                        );
                        LeanMessageBlock::Text {
                            payload: payload_ref(&text.output)?,
                        }
                    }
                    MessageBlock::ToolCall {
                        tool_call_doc_id,
                        id,
                        call_id,
                        name,
                        arguments,
                        signature,
                        additional_params,
                    } => LeanMessageBlock::ToolCall {
                        doc_id: self
                            .tool_ids
                            .get(tool_call_doc_id)
                            .copied()
                            .context("native tool call has no symbolic identity")?,
                        id: id.clone(),
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: payload_ref(arguments)?,
                        signature: signature.clone(),
                        additional_params: additional_params
                            .as_ref()
                            .map(serde_json::to_string)
                            .transpose()?,
                    },
                    MessageBlock::ToolResult {
                        tool_call_doc_id,
                        id,
                        call_id,
                        parts,
                    } => LeanMessageBlock::ToolResult {
                        doc_id: self
                            .tool_ids
                            .get(tool_call_doc_id)
                            .copied()
                            .context("native result has no symbolic tool identity")?,
                        id: id.clone(),
                        call_id: call_id.clone(),
                        parts: parts
                            .iter()
                            .map(|part| match part {
                                gents_protocol::output::ToolResultPart::Text { text } => {
                                    Ok(LeanResultPart::Text {
                                        payload: presented_payload(text)?,
                                    })
                                }
                                gents_protocol::output::ToolResultPart::Media(_) => {
                                    anyhow::bail!("native result media is not in this fixture")
                                }
                            })
                            .collect::<Result<Vec<_>>>()?,
                    },
                    _ => anyhow::bail!("native adapter observed unsupported message block"),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(LeanCanonicalMessage {
            header: crate::lean_vocab_test::LeanCanonicalHeader {
                id,
                session: self.transcript_session_id,
                request: message.request_doc_id.as_ref().map(|_| self.request_id),
                origin: None,
                refs: message
                    .payload_references()
                    .into_iter()
                    .map(|reference| {
                        Ok(crate::lean_vocab_test::LeanPayloadRef {
                            close_id: self
                                .segment_ids
                                .get(&reference.close_doc_id)
                                .copied()
                                .context(
                                    "native header reference has no symbolic closure identity",
                                )?,
                            stream: u64::from(reference.stream),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                outcome: match message.outcome {
                    OutputOutcome::Complete => LeanOutcome::Complete,
                    OutputOutcome::Partial => LeanOutcome::Partial,
                },
                role: match message.role {
                    MessageRole::System => LeanMessageRole::System,
                    MessageRole::User => LeanMessageRole::User,
                    MessageRole::Assistant => LeanMessageRole::Assistant,
                },
                publication: match &message.publication {
                    MessagePublication::RequestExecution {
                        execution_generation,
                    } => LeanMessagePublication::RequestExecution {
                        generation: parse_generation(Some(execution_generation))
                            .context("native header generation is not symbolic")?,
                    },
                    MessagePublication::RequestRecovery {
                        execution_generation,
                    } => LeanMessagePublication::RequestRecovery {
                        generation: parse_generation(Some(execution_generation))
                            .context("native recovery header generation is not symbolic")?,
                    },
                    MessagePublication::ToolDelivery { tool_call_doc_id } => {
                        LeanMessagePublication::ToolDelivery {
                            call: self
                                .tool_ids
                                .get(tool_call_doc_id)
                                .copied()
                                .context("native delivery has no symbolic tool identity")?,
                        }
                    }
                    _ => anyhow::bail!("native adapter observed unsupported publication"),
                },
            },
            key: message.message_key.clone(),
            sequence: modeled_sequence(u64::from(message.sequence))?,
            native_id: message.native_id.clone(),
            blocks,
            created_at: self.modeled_time(&message.created_at)?,
        })
    }

    async fn observe(&mut self, accepted: bool) -> Result<LeanCanonicalExecutionObservation> {
        self.refresh_output().await?;
        let response = self
            .node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {} lifecycle_state }} }}"#,
                crate::graphql::escape_graphql_string(&self.request_doc_id),
                crate::watcher::AGENT_REQUEST_FIELDS,
            ))
            .await;
        anyhow::ensure!(
            !response.has_errors(),
            "observe native request: {:?}",
            response.errors
        );
        let row: AgentRequestRow = crate::graphql::first_row(&response, "AgentRequest")?
            .context("native request disappeared")?;
        let state = row
            .lifecycle_state
            .context("native request omitted lifecycle")?;
        let persisted_generation = parse_generation(row.execution_generation.as_deref());
        let active_lease = !state.is_terminal();
        let lease_deadline = row
            .execution_lease_expires_at
            .as_deref()
            .map(|deadline| self.modeled_time(deadline))
            .transpose()?;
        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let tools = self.node.execute(&format!(
            r#"{{ AgentToolCall(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ _docID request_doc_id lifecycle_state message_sequence await_mode stuck_since cancel_cascade_intent_at }} }}"#,
        )).await;
        anyhow::ensure!(
            !tools.has_errors(),
            "observe native tools: {:?}",
            tools.errors
        );
        let tool_rows = tools
            .data
            .as_ref()
            .and_then(|data| data.get("AgentToolCall"))
            .and_then(serde_json::Value::as_array)
            .context("native tool observation omitted rows")?;
        anyhow::ensure!(
            tool_rows.len() == self.tool_ids.len(),
            "native tool observation contains unmapped physical rows"
        );
        let mut tool_state = None;
        let mut tool_stuck_since = None;
        let mut tool_cancel_intent_at = None;
        let mut in_flight = false;
        let mut physical_tool_request = None;
        let mut accepted_sequence = None;
        let mut reserved_max = None::<u64>;
        for tool in tool_rows {
            let doc_id = tool
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .context("native tool omitted physical identity")?;
            let symbolic = self
                .tool_ids
                .get(doc_id)
                .context("native tool has no symbolic identity")?;
            let state = tool
                .get("lifecycle_state")
                .and_then(serde_json::Value::as_str)
                .context("native tool omitted lifecycle")?;
            anyhow::ensure!(
                tool.get("request_doc_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(self.request_doc_id.as_str()),
                "native tool changed physical request binding"
            );
            let sequence = tool
                .get("message_sequence")
                .and_then(serde_json::Value::as_u64)
                .context("native tool omitted reserved sequence")?;
            let sequence = modeled_sequence(sequence)?;
            reserved_max = Some(reserved_max.map_or(sequence, |current| current.max(sequence)));
            if *symbolic == self.query_document {
                anyhow::ensure!(
                    tool_state.replace(state.to_owned()).is_none(),
                    "queried symbolic tool has multiple physical rows"
                );
                let await_mode = tool
                    .get("await_mode")
                    .and_then(serde_json::Value::as_str)
                    .context("native tool omitted await mode")?;
                tool_stuck_since = tool
                    .get("stuck_since")
                    .and_then(serde_json::Value::as_str)
                    .map(|value| self.modeled_time(value))
                    .transpose()?;
                // Lean's transcript inFlight tracks the accepted parent's
                // unsettled foreground ownership, not a still-running physical
                // executor. The native terminal owner records handoff in
                // `stuck_since`; retaining a running row after that is valid.
                in_flight =
                    state == "running" && await_mode == "foreground" && tool_stuck_since.is_none();
                tool_cancel_intent_at = tool
                    .get("cancel_cascade_intent_at")
                    .and_then(serde_json::Value::as_str)
                    .map(|value| self.modeled_time(value))
                    .transpose()?;
                physical_tool_request = Some(self.request_id);
                accepted_sequence = Some(sequence);
            }
        }
        // Initialization admits no seeded transcript/tool facts and creates a
        // fresh isolated session, so these two authoritative collections are
        // the complete native sequence reservation set for this adapter.
        let message_max = self.messages.iter().map(|message| message.sequence).max();
        self.next_sequence = message_max
            .into_iter()
            .chain(reserved_max)
            .max()
            .map_or(0, |value| value + 1);
        self.tool_state = tool_state;
        self.physical_tool_request = physical_tool_request;
        let compaction = crate::session::load_prompt_compaction_state(
            &self.node,
            &self.session_id,
            &self.principal,
            Some(&self.principal),
            None,
        )
        .await?;
        let compaction_cursor = compaction
            .compacted_through_sequence
            .map(|sequence| modeled_sequence(u64::from(sequence)))
            .transpose()?;
        Ok(LeanCanonicalExecutionObservation {
            accepted,
            generation: active_lease.then_some(persisted_generation).flatten(),
            terminal_generation: state
                .is_terminal()
                .then_some(persisted_generation)
                .flatten(),
            request_state: state.as_str().to_owned(),
            tool_state: self.tool_state.clone(),
            tool_stuck_since,
            tool_cancel_intent_at,
            in_flight,
            next_sequence: self.next_sequence,
            accepted_sequence,
            physical_tool_request: self.physical_tool_request,
            lease_deadline: active_lease.then_some(lease_deadline).flatten(),
            compaction_cursor,
            segments: self.segments.clone(),
            messages: self.messages.clone(),
        })
    }
}

impl CanonicalExecutionAdapter for NativeCanonicalExecutionAdapter {
    type Error = anyhow::Error;
    type Native = NativeCanonicalExecution;

    fn initialize<'a>(
        &'a mut self,
        seed: &'a LeanCanonicalExecutionSeed,
    ) -> ExecutionFuture<'a, Result<Self::Native>> {
        Box::pin(async move {
            anyhow::ensure!(
                seed.segments.is_empty() && seed.messages.is_empty(),
                "seeded durable output is not implemented by the native adapter"
            );
            anyhow::ensure!(
                seed.tool_calls.is_empty() && seed.in_flight.is_empty(),
                "seeded tool execution is not implemented by the native adapter"
            );
            anyhow::ensure!(
                seed.next_sequence == 0,
                "nonzero sequence without seeded durable facts is not implemented"
            );
            anyhow::ensure!(
                seed.lease.request == RequestLifecycleState::Processing
                    && seed.lease.lease.status
                        == crate::lean_vocab_test::LeanRequestExecutionLeaseStatus::Active,
                "native fixture only supports an active processing request"
            );
            let generation = seed
                .lease
                .lease
                .generation
                .context("native fixture requires an active generation")?;
            let duration = seed
                .lease
                .lease
                .duration
                .context("native fixture requires a lease duration")?;
            anyhow::ensure!(
                duration > 0
                    && seed.lease.now.checked_add(duration) == Some(seed.lease.effective_expiry)
                    && seed.lease.used_generations == [generation]
                    && seed.lease.lease.explicit_deadline == Some(seed.lease.effective_expiry)
                    && seed.lease.lease.outcome.is_none()
                    && !seed.lease.continuation_required
                    && !seed.lease.token_charge_required
                    && seed.lease.continuation_count == 0
                    && seed.lease.token_charge_count == 0,
                "native fixture does not support this seeded lease history"
            );
            let seed_creation_time = fixture_time(seed.lease.now)?;
            let request_id = format!("lean-request-{}", seed.request_id);
            let initial_session_id = format!("lean-session-{}", seed.session_id);
            let key_dir = tempfile::tempdir().context("native fixture identity directory")?;
            let identity: Arc<dyn AgentIdentity> = Arc::new(crate::KeyIdentity::load_or_create(
                key_dir.path().join("agent.key"),
                None,
            )?);
            let principal = identity.did().to_owned();
            let mut remote_dids = HashMap::new();
            let mut remote_symbols = seed
                .remote_routes
                .iter()
                .map(|route| route.target)
                .collect::<Vec<_>>();
            if let Some(workspace) = &seed.workspace {
                remote_symbols.push(workspace.workspace_owner_agent_did);
            }
            remote_symbols.sort_unstable();
            remote_symbols.dedup();
            for symbolic in remote_symbols {
                if symbolic != seed.principal {
                    let remote = crate::KeyIdentity::load_or_create(
                        key_dir.path().join(format!("agent-{symbolic}.key")),
                        None,
                    )?;
                    remote_dids.insert(symbolic, remote.did().to_owned());
                }
            }
            let node = Arc::new(EmbeddedNode::builder().build().await?);
            crate::ensure_runtime_schemas(&node).await?;
            crate::test_support::install_test_behavior(&node, &principal, "general").await;
            let workspace_guard =
                seed_workspace_documents(&node, seed, &principal, &remote_dids).await?;
            if seed.subagent_depth > 0 {
                install_signed_ancestor_target(&node, &principal).await?;
            }
            let (request_doc_id, session_id) = if seed.subagent_depth == 0 {
                let create = crate::lifecycle::build_signed_request(
                    crate::lifecycle::RequestSpec {
                        workspace: seed_workspace_lineage(seed, &principal, &remote_dids),
                        ..crate::lifecycle::RequestSpec::new(
                    crate::lifecycle::RequestIdentity {
                        requester_did: None,
                        request_id: request_id.clone(),
                        agent_did: principal.clone(),
                        behavior_id: "general".to_owned(),
                        session_id: initial_session_id.clone(),
                        content: "lean native execution".to_owned(),
                        execution_origin: crate::lifecycle::ExecutionOrigin::Interactive,
                        created_at: seed_creation_time
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    },
                    gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                        &principal,
                    ),
                )
                    },
                    crate::lifecycle::RequestSigner::Identity(identity.as_ref()),
            )
            .await?;
                let response = node
                    .execute(&create.graphql_mutation().map_err(anyhow::Error::msg)?)
                    .await;
                anyhow::ensure!(
                    !response.has_errors(),
                    "initialize native request: {:?}",
                    response.errors
                );
                let request_doc_id =
                    crate::graphql::single_mutation_document(&response, "create_AgentRequest")?
                        .and_then(|row| row.get("_docID"))
                        .and_then(serde_json::Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                        .context("native request create omitted its physical identity")?
                        .to_owned();
                (request_doc_id, initial_session_id)
            } else {
                seed_signed_ancestor_chain(&node, &identity, seed, &remote_dids).await?
            };
            // The real child owner authors ancestors at wall time. Translate
            // only this fixture's modeled clock after those writes, so the
            // modeled final claim cannot precede its signed creation.
            let fixture_epoch_seconds = if seed.subagent_depth == 0 {
                FIXTURE_EPOCH_SECONDS
            } else {
                Utc::now()
                    .timestamp()
                    .checked_add(1)
                    .and_then(|value| value.checked_sub(i64::try_from(seed.lease.now).ok()?))
                    .context("native modeled clock offset overflow")?
            };
            let now = fixture_time_at(fixture_epoch_seconds, seed.lease.now)?;
            let expiry = fixture_time_at(fixture_epoch_seconds, seed.lease.effective_expiry)?;
            let lookup = node.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {} lifecycle_state }} }}"#, crate::graphql::escape_graphql_string(&request_doc_id), crate::watcher::AGENT_REQUEST_FIELDS)).await;
            anyhow::ensure!(
                !lookup.has_errors(),
                "read native pending request: {:?}",
                lookup.errors
            );
            let row = crate::graphql::first_row::<AgentRequestRow>(&lookup, "AgentRequest")?
                .context("native signed request disappeared")?;
            anyhow::ensure!(
                row.lifecycle_state == Some(RequestLifecycleState::Pending),
                "native signed request was not pending"
            );
            let queued: crate::watcher::AgentRequest = row.try_into()?;
            let verified = if seed.subagent_depth == 0 {
                crate::request_admission::verify_fresh_local_self_request(
                    &node,
                    identity.as_ref(),
                    &queued,
                    "general",
                )
                .await
                .map_err(anyhow::Error::from)?
            } else {
                let verifier = crate::request_admission::AgentRequestAdmissionVerifier::new(
                    node.clone(),
                    identity.clone(),
                    crate::agent::p2p_reconcile::enrollment_authority_channel().1,
                );
                verifier
                    .verify_fresh(&queued, "general")
                    .await
                    .map_err(anyhow::Error::from)?
            };
            let mut lifecycle = crate::lifecycle::RequestLifecycle::new_with_agent_did(
                node.clone(),
                "general",
                &principal,
                verified,
                duration,
            );
            lifecycle.set_execution_lease_duration(std::time::Duration::from_secs(duration));
            let claimed_at = now;
            let claim_generation = symbolic_generation(generation);
            let durable_claim = lifecycle
                .claim_pending_durable_with_inputs(|| now, || (claimed_at, claim_generation))
                .await?;
            anyhow::ensure!(
                durable_claim.was_claimed(),
                "native signed request was not claimable"
            );
            // The durable claim owner performed its exact CAS, mailbox claim,
            // and session projection. Do not install a process-local renewal
            // task or execution lease on this fixture-only lifecycle wrapper.
            drop(lifecycle);
            crate::lifecycle::RequestLifecycle::begin_owned_execution_durable_with_clock(
                &node,
                &request_doc_id,
                &symbolic_generation(generation),
                || now,
            )
            .await?;
            let claimed = node.execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {} lifecycle_state }} }}"#, crate::graphql::escape_graphql_string(&request_doc_id), crate::watcher::AGENT_REQUEST_FIELDS)).await;
            anyhow::ensure!(
                !claimed.has_errors(),
                "read native owned request: {:?}",
                claimed.errors
            );
            let claimed = crate::graphql::first_row::<AgentRequestRow>(&claimed, "AgentRequest")?
                .context("native owned request disappeared")?;
            anyhow::ensure!(
                claimed.lifecycle_state == Some(RequestLifecycleState::Processing)
                    && claimed.execution_generation.as_deref()
                        == Some(symbolic_generation(generation).as_str())
                    && claimed.execution_lease_expires_at.as_deref()
                        == Some(expiry.to_rfc3339().as_str()),
                "native claim/begin did not preserve modeled execution authority"
            );
            let scoped_session = node.execute(&format!(
                r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }} }}, limit: 2) {{ session_id agent_did requester_did behavior_id created_at observation }} }}"#,
                crate::graphql::escape_graphql_string(&session_id),
                crate::graphql::escape_graphql_string(&principal),
                crate::graphql::escape_graphql_string(&principal),
            )).await;
            anyhow::ensure!(
                !scoped_session.has_errors(),
                "read claimed requester session: {:?}",
                scoped_session.errors
            );
            let sessions: Vec<gents_protocol::session::AgentSession> =
                crate::graphql::rows(&scoped_session, "AgentSession")?;
            let [session] = sessions.as_slice() else {
                anyhow::bail!("real claim did not create one exact requester-scoped session")
            };
            anyhow::ensure!(
                session.session_id == session_id
                    && session.agent_did == principal
                    && session.requester_did.as_deref() == Some(principal.as_str())
                    && session.behavior_id == "general"
                    && session
                        .observation
                        .as_ref()
                        .and_then(|observation| observation.latest_request.as_ref())
                        .is_some_and(|head| head.request_doc_id == request_doc_id
                            && head.request_id == request_id
                            && head.lifecycle_state == RequestLifecycleState::Claimed),
                "real claim did not project the signed request into its exact session"
            );
            Ok(NativeCanonicalExecution {
                node,
                _identity_guard: key_dir,
                remote_dids,
                _workspace_guard: workspace_guard,
                fixture_epoch_seconds,
                request_doc_id,
                request_id: seed.request_id,
                principal_id: seed.principal,
                subagent_depth: seed.subagent_depth,
                query_document: 0,
                remote_routes: seed.remote_routes.clone(),
                transcript_session_id: seed.transcript_session_id,
                next_sequence: seed.next_sequence,
                session_id,
                principal,
                segments: Vec::new(),
                messages: Vec::new(),
                tool_state: None,
                physical_tool_request: None,
                segment_ids: HashMap::new(),
                message_ids: HashMap::new(),
                modeled_message_keys: HashMap::new(),
                canonical_message_keys: HashMap::new(),
                tool_ids: HashMap::new(),
                accepted_spawns: HashMap::new(),
            })
        })
    }

    fn apply<'a>(
        &'a mut self,
        native: &'a mut Self::Native,
        query_document: u64,
        operation: &'a LeanCanonicalExecutionOperation,
    ) -> ExecutionFuture<'a, Result<LeanCanonicalExecutionObservation>> {
        Box::pin(async move {
            native.query_document = query_document;
            match operation {
                LeanCanonicalExecutionOperation::RenewLease {
                    now,
                    generation,
                    expected_deadline,
                    ..
                } => {
                    let outcome = crate::lifecycle::renew_execution_lease_once_at(
                        &native.node,
                        &native.request_doc_id,
                        &symbolic_generation(*generation),
                        native.fixture_time(*expected_deadline)?,
                        native.fixture_time(*now)?,
                    )
                    .await?;
                    native
                        .observe(matches!(
                            outcome,
                            crate::lifecycle::RenewalAttemptOutcome::Committed
                        ))
                        .await
                }
                LeanCanonicalExecutionOperation::AppendOutput {
                    now,
                    generation,
                    record,
                    ..
                } => {
                    anyhow::ensure!(
                        record.close.is_none(),
                        "append output cannot close a source"
                    );
                    let prepared = native.provider_segment(record, *generation)?;
                    let doc_id = crate::streaming::canonical::append_provider_segment_at(
                        &native.node,
                        &symbolic_generation(*generation),
                        &prepared,
                        native.fixture_time(*now)?,
                    )
                    .await;
                    let doc_id = match doc_id {
                        Ok(value) => value,
                        Err(error)
                            if error
                                .downcast_ref::<crate::streaming::canonical::ProviderAppendRejection>()
                                .is_some() =>
                        {
                            return native.observe(false).await;
                        }
                        Err(error) => return Err(error),
                    };
                    native.segment_ids.insert(doc_id, record.id);
                    native.observe(true).await
                }
                LeanCanonicalExecutionOperation::AppendToolOutput {
                    now,
                    document,
                    record,
                    ..
                } => {
                    anyhow::ensure!(
                        record.close.is_none() && record.flush.is_some(),
                        "tool append requires a data flush without a closure"
                    );
                    let physical = native.physical_tool(*document)?.to_owned();
                    let prepared = native.tool_output_segment(record, *document, &physical)?;
                    let tool = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
                        native.node.clone(),
                        &physical,
                        &native.principal,
                        &native.session_id,
                        Some(&native.principal),
                    )
                    .await?
                    .context("accepted physical tool disappeared before output append")?;
                    let binding = tool.tool_output_binding()?;
                    let receipt = crate::tool_call_lifecycle::delivery::append_tool_output_at(
                        &binding,
                        &prepared.payload,
                        native.fixture_time(record.created_at)?,
                        native.fixture_time(*now)?,
                    )
                    .await;
                    let receipt = match receipt {
                        Ok(receipt) => receipt,
                        Err(error)
                            if error
                                .downcast_ref::<crate::tool_call_lifecycle::delivery::ToolOutputAppendRejection>()
                                .is_some() =>
                        {
                            return native.observe(false).await;
                        }
                        Err(error) => return Err(error),
                    };
                    native.segment_ids.insert(receipt.segment_doc_id, record.id);
                    native.observe(true).await
                }
                LeanCanonicalExecutionOperation::AppendOutputWhileSiblingWaits {
                    now,
                    generation,
                    record,
                    ..
                } => {
                    let prepared = native.provider_segment(record, *generation)?;
                    let node = Arc::clone(&native.node);
                    let nested_node = Arc::clone(&node);
                    let generation = symbolic_generation(*generation);
                    let now = native.fixture_time(*now)?;
                    let attempted = crate::config_client::ConfigAccess::transact_local(
                        &node,
                        None,
                        "lean.same_task_holder",
                        |_| {
                            let prepared = prepared.clone();
                            let generation = generation.clone();
                            let node = Arc::clone(&nested_node);
                            let now = now.clone();
                            Box::pin(async move {
                                crate::streaming::canonical::append_provider_segment_at(
                                    &node,
                                    &generation,
                                    &prepared,
                                    now,
                                )
                                .await
                                .map(|_| ())
                            })
                        },
                    )
                    .await;
                    match attempted {
                        Err(error)
                            if error
                                .downcast_ref::<crate::config_client::ReentrantEmbeddedWrite>()
                                .is_some() =>
                        {
                            native.observe(false).await
                        }
                        Err(error) => {
                            Err(error.context("same-task guarded append failed unexpectedly"))
                        }
                        Ok(()) => anyhow::bail!("same-task guarded append bypassed mutation gate"),
                    }
                }
                LeanCanonicalExecutionOperation::ClosePartial {
                    now,
                    generation,
                    item,
                    ..
                } => native.close_partial(*now, *generation, item).await,
                LeanCanonicalExecutionOperation::RecoverExpiredTerminal {
                    now,
                    expected_generation,
                    fresh_generation,
                    outcome,
                    selection,
                    items,
                    ..
                } => {
                    anyhow::ensure!(
                        matches!(outcome.as_str(), "failed" | "interrupted"),
                        "modeled terminal recovery outcome is not supported by the lease owner"
                    );
                    native
                        .recover_terminal(
                            *now,
                            *expected_generation,
                            *fresh_generation,
                            outcome,
                            selection,
                            items,
                        )
                        .await
                }
                LeanCanonicalExecutionOperation::Dispatch {
                    now,
                    generation,
                    call,
                    cancellation_allows,
                    tool_policy_allows,
                    ..
                } => {
                    anyhow::ensure!(
                        *cancellation_allows && *tool_policy_allows,
                        "native dispatch adapter has no external denial injection"
                    );
                    let physical = native.physical_tool(*call)?.to_owned();
                    let mut tool = if let Some((accepted, deadline, await_mode, cancel_policy)) =
                        native.accepted_spawns.get(call)
                    {
                        anyhow::ensure!(
                            accepted.tool_call_doc_id == physical,
                            "modeled spawn dispatch lost exact accepted row"
                        );
                        crate::tool_call_lifecycle::ToolCallLifecycle::from_accepted(
                            native.node.clone(),
                            native.principal.clone(),
                            Some(native.principal.clone()),
                            accepted.clone(),
                            native.fixture_time(*deadline)?,
                            *await_mode,
                            *cancel_policy,
                        )?
                    } else {
                        crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
                            native.node.clone(),
                            &physical,
                            &native.principal,
                            &native.session_id,
                            Some(&native.principal),
                        )
                        .await?
                        .context("accepted physical tool disappeared before dispatch")?
                    };
                    match tool
                        .start_running_at(native.fixture_time(*now)?, &symbolic_generation(*generation))
                        .await
                    {
                        Ok(()) => native.observe(true).await,
                        Err(error)
                            if error
                                .downcast_ref::<crate::tool_call_lifecycle::delivery::ToolDispatchRejection>()
                                .is_some() =>
                        {
                            native.observe(false).await
                        }
                        Err(error) => Err(error.context("native tool dispatch owner failed")),
                    }
                }
                LeanCanonicalExecutionOperation::TerminalizeCompleted {
                    now,
                    generation,
                    outcome,
                    selection,
                    ..
                } => {
                    let outcome = match outcome.as_str() {
                        "completed" => crate::lifecycle::RequestTerminalOutcome::Completed,
                        "failed" => crate::lifecycle::RequestTerminalOutcome::Failed,
                        "interrupted" => crate::lifecycle::RequestTerminalOutcome::Interrupted,
                        "dead" => crate::lifecycle::RequestTerminalOutcome::Dead,
                        "superseded" => crate::lifecycle::RequestTerminalOutcome::Superseded,
                        other => anyhow::bail!("unknown modeled terminal outcome: {other}"),
                    };
                    let selection = match selection {
                        LeanTerminalSelection::NoMessage => {
                            gents_protocol::output::TerminalOutput::NoMessage
                        }
                        LeanTerminalSelection::Message { id } => {
                            let physical = native
                                .message_ids
                                .iter()
                                .find_map(|(physical, symbolic)| {
                                    (*symbolic == *id).then_some(physical.clone())
                                })
                                .context(
                                    "modeled terminal message has no published physical header",
                                )?;
                            gents_protocol::output::TerminalOutput::Message {
                                message_doc_id: physical,
                            }
                        }
                    };
                    let result = crate::lifecycle::terminalize_owned_at(
                        &native.node,
                        &native.request_doc_id,
                        &symbolic_generation(*generation),
                        outcome,
                        selection,
                        native.fixture_time(*now)?,
                    )
                    .await;
                    let result = match result {
                        Ok(value) => value,
                        Err(error)
                            if error
                                .downcast_ref::<crate::lifecycle::ToolAccountingRejection>()
                                .is_some() =>
                        {
                            return native.observe(false).await;
                        }
                        Err(error) => return Err(error),
                    };
                    native
                        .observe(!matches!(result, crate::lifecycle::TerminalizeResult::Lost))
                        .await
                }
                LeanCanonicalExecutionOperation::CompleteForegroundTool {
                    now,
                    document,
                    authority_outcome,
                    record,
                    message,
                    ..
                } => {
                    native
                        .complete_foreground(*now, *document, authority_outcome, record, message)
                        .await
                }
                LeanCanonicalExecutionOperation::RevokeCorrupt {
                    now,
                    expected_generation,
                    fresh_generation,
                    outcome,
                    selection,
                    ..
                } => {
                    native
                        .revoke_corrupt(
                            *now,
                            *expected_generation,
                            *fresh_generation,
                            outcome,
                            selection,
                        )
                        .await
                }
                LeanCanonicalExecutionOperation::AdmitSpawnedBackground {
                    now,
                    generation,
                    admission,
                    ..
                } => {
                    let physical_parent = native
                        .physical_tool(admission.parent_tool_document)?
                        .to_owned();
                    let mut parent = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
                        Arc::clone(&native.node),
                        &physical_parent,
                        &native.principal,
                        &native.session_id,
                        Some(&native.principal),
                    )
                    .await?
                    .context("spawned admission parent disappeared")?;
                    anyhow::ensure!(
                        parent.execution_generation()
                            == Some(symbolic_generation(*generation).as_str()),
                        "modeled spawned admission generation differs from physical parent"
                    );
                    let child = parent
                        .admit_spawned_background_child_at(
                            crate::tool_call_lifecycle::SpawnedBackgroundToolAdmission {
                                tool_name: admission.operation.clone(),
                                deadline_at: native.fixture_time(admission.deadline)?,
                            },
                            native.fixture_time(*now)?,
                        )
                        .await?;
                    let physical_child = child
                        .doc_id()
                        .context("spawned admission child omitted physical identity")?
                        .to_owned();
                    if let Some(existing_symbolic) = native.tool_ids.get(&physical_child) {
                        anyhow::ensure!(
                            *existing_symbolic == admission.document,
                            "native spawned child maps to symbolic document {existing_symbolic}, not requested {}",
                            admission.document
                        );
                    } else {
                        native.tool_ids.insert(physical_child, admission.document);
                    }
                    native.observe(true).await
                }
                LeanCanonicalExecutionOperation::DeliverReplicatedSegment { record, .. } => {
                    let generation = match &record.writer {
                        LeanCanonicalWriter::Request { generation } => *generation,
                        _ => anyhow::bail!(
                            "replicated native fixture supports provider segments only"
                        ),
                    };
                    let segment = native.provider_segment(record, generation)?;
                    let response = native.node.execute_request_with_retry(
                        defra_node::QueryRequest::new(
                            crate::session::canonical_rows::CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        ).with_variables(
                            crate::session::canonical_rows::output_segment_create_variables(&segment)?,
                        ),
                        defra_node::ExecuteRetryPolicy::default(),
                    ).await;
                    anyhow::ensure!(
                        !response.has_errors(),
                        "replicate native segment: {:?}",
                        response.errors
                    );
                    let doc_id = crate::graphql::created_doc_id(
                        &serde_json::json!({ "data": response.data }),
                        "AgentOutputSegment",
                    )?;
                    native.segment_ids.insert(doc_id, record.id);
                    native.observe(true).await
                }
                LeanCanonicalExecutionOperation::AcceptForeground {
                    now,
                    generation,
                    closing,
                    message,
                    targets,
                    admissions,
                    ..
                }
                | LeanCanonicalExecutionOperation::AcceptTurn {
                    now,
                    generation,
                    closing,
                    message,
                    targets,
                    admissions,
                    ..
                } => {
                    anyhow::ensure!(
                        targets.is_empty(),
                        "foreground acceptance unexpectedly carries remote targets"
                    );
                    native
                        .accept_provider_turn(
                            *now,
                            *generation,
                            closing,
                            message,
                            targets,
                            admissions,
                        )
                        .await
                }
                LeanCanonicalExecutionOperation::AcceptRemote {
                    now,
                    generation,
                    closing,
                    message,
                    targets,
                    admissions,
                    ..
                } => {
                    native
                        .accept_provider_turn(
                            *now,
                            *generation,
                            closing,
                            message,
                            targets,
                            admissions,
                        )
                        .await
                }
                other => anyhow::bail!(
                    "native canonical execution operation is not implemented yet: {other:?}"
                ),
            }
        })
    }
}

impl NativeCanonicalExecution {
    async fn close_partial(
        &mut self,
        now: u64,
        generation: u64,
        item: &crate::lean_vocab_test::LeanCanonicalRecoveryItem,
    ) -> Result<LeanCanonicalExecutionObservation> {
        // Preserve the candidate's writer independently of the actor's lease
        // generation so stale operations reach the native authorization owner.
        let LeanCanonicalWriter::Request {
            generation: writer_generation,
        } = &item.closing.writer
        else {
            anyhow::bail!("provider partial close requires a request writer")
        };
        let closing = self.provider_segment(&item.closing, *writer_generation)?;
        let mut header = item
            .message
            .as_ref()
            .map(|message| self.protocol_message(message, *writer_generation))
            .transpose()?;
        if let Some(header) = header.as_mut() {
            header.message_key = crate::streaming::canonical::partial_message_key(
                &self.request_doc_id,
                &closing.source,
            )?;
        }
        let symbolic_close = format!("lean-segment-{}", item.closing.id);
        let header_factory = header.map(|template| {
            let symbolic_close = symbolic_close.clone();
            Arc::new(move |physical_close: &str| {
                let mut message = template.clone();
                for block in &mut message.blocks {
                    match block {
                        MessageBlock::Text { text } => {
                            if text.output.close_doc_id == symbolic_close {
                                text.output.close_doc_id = physical_close.to_owned();
                            }
                        }
                        MessageBlock::ToolCall { arguments, .. } => {
                            if arguments.close_doc_id == symbolic_close {
                                arguments.close_doc_id = physical_close.to_owned();
                            }
                        }
                        // The bounded adapter currently cannot decode these
                        // modeled shapes, but identity remapping itself is not
                        // a policy gate. Canonical owner validation below owns
                        // whether any such block is legal for Partial output.
                        MessageBlock::Reasoning { .. }
                        | MessageBlock::ToolResult { .. }
                        | MessageBlock::Media(_) => {}
                    }
                }
                Ok(message)
            }) as crate::streaming::canonical::PartialHeaderFactory
        });
        let result = crate::streaming::canonical::close_provider_attempt_at(
            &self.node,
            &symbolic_generation(generation),
            &closing,
            crate::streaming::canonical::ProviderAttemptClose::Partial,
            Some(crate::streaming::canonical::ProviderPartialCandidate {
                closing: closing.clone(),
                header: header_factory,
            }),
            self.fixture_time(now)?,
        )
        .await;
        let message_doc_id = match result {
            Ok(message_doc_id) => message_doc_id,
            Err(error)
                if error
                    .downcast_ref::<crate::streaming::canonical::ProviderCloseRejection>()
                    .is_some()
                    || error
                        .downcast_ref::<gents_protocol::output::ReconstructionError>()
                        .is_some() =>
            {
                return self.observe(false).await;
            }
            Err(error) => return Err(error.context("native ClosePartial owner failed")),
        };

        let request = crate::graphql::escape_graphql_string(&self.request_doc_id);
        let response = self.node.execute(&format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{request}" }} }}) {{ {} }} }}"#,
            crate::session::canonical_rows::AGENT_OUTPUT_SEGMENT_FIELDS,
        )).await;
        anyhow::ensure!(
            !response.has_errors(),
            "read partial closure: {:?}",
            response.errors
        );
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data["AgentOutputSegment"].as_array())
            .context("partial closure query omitted rows")?;
        let closing_row = rows
            .iter()
            .map(crate::session::canonical_rows::decode_output_segment_row)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .find(|row| row.segment.close.is_some())
            .context("accepted partial close did not persist its closure")?;
        self.segment_ids.insert(closing_row.doc_id, item.closing.id);
        if let (Some(doc_id), Some(message)) = (message_doc_id, item.message.as_ref()) {
            self.message_ids.insert(doc_id.clone(), message.header.id);
            self.modeled_message_keys
                .insert(doc_id.clone(), message.key.clone());
            self.canonical_message_keys.insert(
                doc_id,
                crate::streaming::canonical::partial_message_key(
                    &self.request_doc_id,
                    &closing.source,
                )?,
            );
        }
        self.observe(true).await
    }

    async fn accept_provider_turn(
        &mut self,
        now: u64,
        generation: u64,
        closing: &LeanCanonicalSegment,
        message: &LeanCanonicalMessage<LeanPayloadSpec>,
        targets: &[crate::lean_vocab_test::LeanCanonicalRemoteTarget],
        admissions: &[LeanCanonicalToolAdmission],
    ) -> Result<LeanCanonicalExecutionObservation> {
        anyhow::ensure!(
            matches!(
                closing.close,
                Some(crate::lean_vocab_test::LeanCanonicalClosure::Closed {
                    outcome: LeanOutcome::Complete,
                    ..
                })
            ),
            "native publication owner accepts only complete provider closures"
        );
        let sealed = self.provider_segment(closing, generation)?;
        let header = self.protocol_message(message, generation)?;
        // A replay reads the already committed close through `observe`. The
        // candidate is that same physical identity, not a second source row.
        // Verify its immutable facts before excluding it from the input extent
        // used to reconstruct the expected native message.
        for committed in self.segments.iter().filter(|row| row.id == closing.id) {
            anyhow::ensure!(
                committed == closing,
                "modeled replay close differs from its committed physical identity"
            );
        }
        let preceding = self
            .segments
            .iter()
            .filter(|row| row.id != closing.id)
            .collect::<Vec<_>>();
        let mut protocol_segments = preceding
            .iter()
            .map(|segment| self.provider_segment(segment, generation))
            .collect::<Result<Vec<_>>>()?;
        // Build the expected native message from the same committed source
        // bytes the publication owner will inspect. The supplied close remains
        // untouched in `final_flush` below, so an invalid modeled extent is
        // rejected by that owner instead of this transport encoder.
        let mut reconstruction_close = sealed.clone();
        reconstruction_close.close = None;
        let open_doc_ids = preceding
            .iter()
            .map(|segment| format!("lean-segment-{}", segment.id))
            .collect::<Vec<_>>();
        let mut open_observed = open_doc_ids
            .iter()
            .zip(&protocol_segments)
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect::<Vec<_>>();
        let final_doc_id = format!("lean-segment-{}", closing.id);
        if reconstruction_close.ordinal.is_some() {
            open_observed.push(ObservedSegment {
                doc_id: &final_doc_id,
                segment: &reconstruction_close,
            });
        }
        let extent = gents_protocol::output::extent::inspect_open_source(
            &open_observed,
            &reconstruction_close.request_doc_id,
            &reconstruction_close.source,
            &reconstruction_close.writer,
        )?;
        reconstruction_close.close = Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: extent.segments,
            stream_bytes: extent.stream_bytes,
        });
        protocol_segments.push(reconstruction_close);
        let doc_ids = preceding
            .iter()
            .map(|segment| format!("lean-segment-{}", segment.id))
            .chain(std::iter::once(format!("lean-segment-{}", closing.id)))
            .collect::<Vec<_>>();
        let observed = doc_ids
            .iter()
            .zip(&protocol_segments)
            .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
            .collect::<Vec<_>>();
        let expected = reconstruct_message(&observed, &[], &[], &header)?;
        let encoded = Arc::new(crate::streaming::native_encoding::encode_native_message(
            &expected,
        )?);
        let expected_native = expected.clone();
        let final_flush = sealed;
        let spawn_admissions = targets
            .iter()
            .map(|target| -> Result<crate::streaming::SpawnAdmissionPlan> {
                let route = self
                    .remote_routes
                    .iter()
                    .find(|route| {
                        route.call == target.call
                            && route.target == target.target
                            && route.behavior == target.behavior
                    })
                    .context("modeled remote target has no configured route fact")?;
                anyhow::ensure!(
                    target.coordinator == self.principal_id,
                    "modeled remote target coordinator differs from fixture principal"
                );
                let admission = admissions
                    .iter()
                    .find(|admission| admission.document == route.call)
                    .context("modeled remote target has no accepted physical admission")?;
                let call_id = message
                    .blocks
                    .iter()
                    .find_map(|block| match block {
                        LeanMessageBlock::ToolCall { doc_id, id, .. } if *doc_id == target.call => {
                            Some(id.clone())
                        }
                        _ => None,
                    })
                    .context("modeled remote target has no provider-native ToolCall")?;
                let child = admission
                    .child_request_id
                    .context("modeled remote admission omitted immutable child identity")?;
                let admitted_behavior = admission
                    .spawn_behavior_id
                    .context("modeled remote admission omitted immutable behavior")?;
                let await_mode = match admission.await_mode.as_str() {
                    "background" => crate::tool_call_lifecycle::AwaitMode::Background,
                    "foreground" => crate::tool_call_lifecycle::AwaitMode::Foreground,
                    other => anyhow::bail!("unknown modeled await mode: {other}"),
                };
                Ok(crate::streaming::SpawnAdmissionPlan {
                    tool_call_id: call_id,
                    child_request_id: format!("lean-child-{child}"),
                    spawn_target_did: modeled_principal_did(
                        route.target,
                        self.principal_id,
                        &self.principal,
                        &self.remote_dids,
                    ),
                    spawn_behavior_id: format!("lean-behavior-{admitted_behavior}"),
                    delegated_workspace: admission.delegated_workspace.as_ref().map(|workspace| {
                        gents_protocol::output::DelegatedWorkspace {
                            workspace_id: format!("lean-workspace-{}", workspace.workspace_id),
                            workspace_owner_agent_did: modeled_principal_did(
                                workspace.workspace_owner_agent_did,
                                self.principal_id,
                                &self.principal,
                                &self.remote_dids,
                            ),
                            workspace_authority: workspace.workspace_authority.clone(),
                            workspace_seal_hash: workspace
                                .workspace_seal_hash
                                .map(|seal| format!("lean-seal-{seal}")),
                        }
                    }),
                    await_mode,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let tool_deadline_at = self
            .fixture_time(
                admissions
                    .first()
                    .map_or(now, |admission| admission.deadline),
            )?
            .to_rfc3339();
        let published = crate::streaming::canonical::publish_provider_turn_at(
            &self.node,
            &symbolic_generation(generation),
            crate::streaming::canonical::ProviderPublicationPlan {
                final_flush: Some(final_flush),
                message_key: message.key.clone(),
                encoded,
                expected: Arc::new(expected),
                tool_deadline_at,
                spawn_admissions: spawn_admissions.clone(),
            },
            self.fixture_time(now)?,
        )
        .await;
        let published = match published {
            Ok(value) => value,
            Err(error)
                if error
                    .downcast_ref::<crate::streaming::canonical::ProviderCloseRejection>()
                    .is_some() =>
            {
                return self.observe(false).await;
            }
            Err(error)
                if error
                    .downcast_ref::<crate::streaming::canonical::ProviderReplayRejection>()
                    .is_some() =>
            {
                return self.observe(false).await;
            }
            Err(error)
                if error
                    .downcast_ref::<crate::streaming::canonical::ProviderWorkspaceRejection>()
                    .is_some() =>
            {
                return self.observe(false).await;
            }
            Err(error) => {
                let detail = format!("native provider publication owner failed: {error:#}");
                return Err(error.context(detail));
            }
        };
        let (stored_header, stored_native) = crate::session::load_canonical_message_from_node(
            &self.node,
            &published.message_doc_id,
            &self.principal,
            Some(&self.principal),
        )
        .await?;
        anyhow::ensure!(
            stored_native == expected_native,
            "native publication content differs: expected {expected_native:?}, got {stored_native:?}"
        );
        anyhow::ensure!(
            stored_header.message_key == message.key,
            "native publication key differs: expected {:?}, got {:?}",
            message.key,
            stored_header.message_key
        );
        anyhow::ensure!(
            stored_header.sequence == native_sequence(message.sequence)?,
            "native publication sequence differs: expected {}, got {}",
            message.sequence,
            stored_header.sequence
        );
        anyhow::ensure!(
            stored_header.request_doc_id.as_deref() == Some(self.request_doc_id.as_str()),
            "native publication request binding differs: expected {:?}, got {:?}",
            self.request_doc_id,
            stored_header.request_doc_id
        );
        let expected_publication = MessagePublication::RequestExecution {
            execution_generation: symbolic_generation(generation),
        };
        anyhow::ensure!(
            stored_header.publication == expected_publication,
            "native publication authority differs: expected {expected_publication:?}, got {:?}",
            stored_header.publication
        );
        self.message_ids
            .insert(published.message_doc_id.clone(), message.header.id);
        self.modeled_message_keys
            .insert(published.message_doc_id.clone(), message.key.clone());
        self.canonical_message_keys.insert(
            published.message_doc_id.clone(),
            stored_header.message_key.clone(),
        );
        for reference in stored_header.payload_references() {
            self.segment_ids
                .insert(reference.close_doc_id.clone(), closing.id);
        }
        anyhow::ensure!(
            published.accepted_tools.len() == admissions.len(),
            "native publication admission count differs from modeled acceptance"
        );
        for (accepted, admission) in published.accepted_tools.iter().zip(admissions) {
            self.tool_ids
                .insert(accepted.tool_call_doc_id.clone(), admission.document);
            let response = self
                .node
                .execute(&format!(
                    r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_doc_id lifecycle_state child_request_id spawn_target_did spawn_behavior_id delegated_workspace delegated_input await_mode }} }}"#,
                    crate::graphql::escape_graphql_string(&accepted.tool_call_doc_id),
                ))
                .await;
            anyhow::ensure!(
                !response.has_errors(),
                "read accepted native tool: {:?}",
                response.errors
            );
            let rows = response
                .data
                .as_ref()
                .and_then(|data| data.get("AgentToolCall"))
                .and_then(serde_json::Value::as_array)
                .context("accepted native tool query omitted rows")?;
            anyhow::ensure!(
                rows.len() == 1,
                "accepted native tool is not physically unique"
            );
            anyhow::ensure!(
                rows[0]
                    .get("request_doc_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(self.request_doc_id.as_str())
                    && rows[0]
                        .get("lifecycle_state")
                        .and_then(serde_json::Value::as_str)
                        == Some(admission.state.as_str()),
                "accepted native tool readback differs from modeled admission"
            );
            if let Some(plan) = spawn_admissions
                .iter()
                .find(|plan| plan.tool_call_id == accepted.id)
            {
                let stored = &rows[0];
                if plan.spawn_target_did != self.principal {
                    let copied: gents_protocol::output::DelegatedToolInput =
                        serde_json::from_value(stored["delegated_input"].clone())
                            .context("accepted remote tool omitted delegated input")?;
                    anyhow::ensure!(
                        u64::from(copied.parent_subagent_depth) == self.subagent_depth,
                        "accepted remote tool changed modeled parent depth"
                    );
                }
                anyhow::ensure!(
                    stored["child_request_id"].as_str() == Some(plan.child_request_id.as_str())
                        && stored["spawn_target_did"].as_str()
                            == Some(plan.spawn_target_did.as_str())
                        && stored["spawn_behavior_id"].as_str()
                            == Some(plan.spawn_behavior_id.as_str())
                        && stored["delegated_workspace"]
                            == serde_json::to_value(&plan.delegated_workspace)?
                        && stored["await_mode"].as_str() == Some(plan.await_mode.as_str()),
                    "accepted native spawn provenance differs from modeled route and admission"
                );
                self.accepted_spawns.insert(
                    admission.document,
                    (
                        accepted.clone(),
                        admission.deadline,
                        plan.await_mode,
                        crate::tool_call_lifecycle::CancelPolicy::from_persisted(
                            &admission.cancel_policy,
                        )
                        .context("modeled spawn admission has invalid cancel policy")?,
                    ),
                );
            }
        }
        self.observe(true).await
    }

    fn protocol_message(
        &self,
        message: &LeanCanonicalMessage<LeanPayloadSpec>,
        generation: u64,
    ) -> Result<TranscriptMessage> {
        anyhow::ensure!(message.header.session > 0, "message session is blank");
        anyhow::ensure!(
            message.header.request.is_some(),
            "provider header omitted request"
        );
        anyhow::ensure!(
            message.header.origin.is_none(),
            "provider header has fork origin"
        );
        anyhow::ensure!(
            matches!(message.header.publication,
                LeanMessagePublication::RequestExecution { generation: owner }
                | LeanMessagePublication::RequestRecovery { generation: owner }
                if owner == generation),
            "provider header generation conflicts with operation"
        );
        let payload = |value: &LeanPayloadSpec| -> Result<PresentedPayload> {
            Ok(PresentedPayload {
                output: PayloadRef {
                    close_doc_id: format!("lean-segment-{}", value.reference.close_id),
                    stream: u32::try_from(value.reference.stream)?,
                },
                presentation: match &value.presentation {
                    LeanPresentation::Full => PayloadPresentation::Full,
                    LeanPresentation::Composed { .. } => anyhow::bail!(
                        "composed presentation is not implemented by the bounded adapter"
                    ),
                },
            })
        };
        let blocks = message
            .blocks
            .iter()
            .map(|block| -> Result<MessageBlock> {
                Ok(match block {
                    LeanMessageBlock::Text { payload: value } => MessageBlock::Text {
                        text: payload(value)?,
                    },
                    LeanMessageBlock::ToolCall {
                        doc_id,
                        id,
                        call_id,
                        name,
                        arguments,
                        signature,
                        additional_params,
                    } => MessageBlock::ToolCall {
                        tool_call_doc_id: format!("lean-tool-{doc_id}"),
                        id: id.clone(),
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: payload(arguments)?.output,
                        signature: signature.clone(),
                        additional_params: additional_params
                            .as_deref()
                            .map(serde_json::from_str)
                            .transpose()?,
                    },
                    _ => anyhow::bail!(
                        "message block is not implemented by the bounded acceptance adapter"
                    ),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(TranscriptMessage {
            message_key: message.key.clone(),
            session_id: self.session_id.clone(),
            agent_did: self.principal.clone(),
            requester_did: Some(self.principal.clone()),
            request_doc_id: Some(self.request_doc_id.clone()),
            publication: match message.header.publication {
                LeanMessagePublication::RequestExecution { .. } => {
                    MessagePublication::RequestExecution {
                        execution_generation: symbolic_generation(generation),
                    }
                }
                LeanMessagePublication::RequestRecovery { .. } => {
                    MessagePublication::RequestRecovery {
                        execution_generation: symbolic_generation(generation),
                    }
                }
                _ => anyhow::bail!("provider header has unsupported publication authority"),
            },
            outcome: match message.header.outcome {
                LeanOutcome::Complete => OutputOutcome::Complete,
                LeanOutcome::Partial => OutputOutcome::Partial,
            },
            sequence: native_sequence(message.sequence)?,
            role: match message.header.role {
                LeanMessageRole::System => MessageRole::System,
                LeanMessageRole::User => MessageRole::User,
                LeanMessageRole::Assistant => MessageRole::Assistant,
            },
            native_id: message.native_id.clone(),
            blocks,
            created_at: self.fixture_time(message.created_at)?.to_rfc3339(),
        })
    }

    fn provider_segment(
        &self,
        record: &LeanCanonicalSegment,
        generation: u64,
    ) -> Result<gents_protocol::output::OutputSegment> {
        let LeanCanonicalSource::Provider {
            scope,
            turn,
            attempt,
        } = &record.coordinate.source
        else {
            anyhow::bail!("append output currently supports provider sources only")
        };
        anyhow::ensure!(record.coordinate.request > 0, "symbolic request is blank");
        anyhow::ensure!(
            matches!(&record.writer, LeanCanonicalWriter::Request { generation: writer } if *writer == generation),
            "append output writer generation conflicts with operation"
        );
        let runs = record
            .flush
            .as_ref()
            .into_iter()
            .flat_map(|flush| &flush.runs)
            .map(|run| {
                let declaration = run
                    .declaration
                    .as_ref()
                    .map(|declaration| {
                        let payload = match declaration.kind {
                            LeanPayloadKind::Text => gents_protocol::output::StreamPayload::Text,
                            LeanPayloadKind::Reasoning => {
                                gents_protocol::output::StreamPayload::Reasoning
                            }
                            LeanPayloadKind::Summary => {
                                gents_protocol::output::StreamPayload::ReasoningSummary
                            }
                            LeanPayloadKind::Opaque => {
                                gents_protocol::output::StreamPayload::ReasoningOpaque
                            }
                            LeanPayloadKind::Arguments => {
                                let tool = declaration
                                    .tool
                                    .as_ref()
                                    .context("argument stream omitted tool identity")?;
                                gents_protocol::output::StreamPayload::ToolArguments {
                                    id: tool.id.clone(),
                                    call_id: tool.call_id.clone(),
                                    name: tool.name.clone(),
                                }
                            }
                            LeanPayloadKind::ToolOutput => {
                                gents_protocol::output::StreamPayload::ToolOutput
                            }
                            LeanPayloadKind::Media => anyhow::bail!(
                                "media append conversion is not implemented in the bounded adapter"
                            ),
                        };
                        Ok(gents_protocol::output::StreamDeclaration {
                            block_index: u32::try_from(declaration.block)?,
                            part_index: u32::try_from(declaration.part)?,
                            payload,
                        })
                    })
                    .transpose()?;
                Ok(gents_protocol::output::SegmentRun {
                    stream: u32::try_from(run.stream)?,
                    bytes: u32::try_from(run.bytes)?,
                    declaration,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(gents_protocol::output::OutputSegment {
            agent_did: self.principal.clone(),
            requester_did: Some(self.principal.clone()),
            session_id: self.session_id.clone(),
            request_doc_id: self.request_doc_id.clone(),
            source: gents_protocol::output::OutputSource::ProviderTurn {
                scope: format!("inference.{scope}").parse()?,
                turn_index: u32::try_from(*turn)?,
                attempt: u32::try_from(*attempt)?,
            },
            writer: gents_protocol::output::OutputWriter::RequestExecution {
                execution_generation: symbolic_generation(generation),
            },
            ordinal: record
                .flush
                .as_ref()
                .map(|flush| u32::try_from(flush.ordinal))
                .transpose()?,
            runs,
            payload: record
                .flush
                .as_ref()
                .map(|flush| String::from_utf8(flush.payload.clone()))
                .transpose()?
                .unwrap_or_default(),
            close: record.close.as_ref().map(|close| match close {
                crate::lean_vocab_test::LeanCanonicalClosure::Closed {
                    outcome,
                    segments,
                    stream_bytes,
                } => SourceClose::Closed {
                    outcome: match outcome {
                        LeanOutcome::Complete => OutputOutcome::Complete,
                        LeanOutcome::Partial => OutputOutcome::Partial,
                    },
                    segments: u32::try_from(*segments).expect("modeled segment count exceeds u32"),
                    stream_bytes: stream_bytes.clone(),
                },
                crate::lean_vocab_test::LeanCanonicalClosure::Retracted => SourceClose::Retracted,
            }),
            created_at: self.fixture_time(record.created_at)?.to_rfc3339(),
        })
    }

    fn tool_output_segment(
        &self,
        record: &LeanCanonicalSegment,
        symbolic_tool: u64,
        physical_tool: &str,
    ) -> Result<gents_protocol::output::OutputSegment> {
        anyhow::ensure!(
            record.coordinate.request == self.request_id,
            "tool output segment belongs to another request"
        );
        anyhow::ensure!(
            matches!(&record.coordinate.source, LeanCanonicalSource::Tool { call } if *call == symbolic_tool),
            "tool output segment belongs to another source"
        );
        anyhow::ensure!(
            matches!(&record.writer, LeanCanonicalWriter::Tool { call } if *call == symbolic_tool),
            "tool output segment has another writer"
        );
        let runs = record
            .flush
            .as_ref()
            .map(|flush| {
                flush
                    .runs
                    .iter()
                    .map(|run| {
                        let declaration = run
                            .declaration
                            .as_ref()
                            .map(|declaration| {
                                anyhow::ensure!(
                                    declaration.kind == LeanPayloadKind::ToolOutput,
                                    "modeled tool source declared a non-tool payload"
                                );
                                Ok(gents_protocol::output::StreamDeclaration {
                                    block_index: u32::try_from(declaration.block)?,
                                    part_index: u32::try_from(declaration.part)?,
                                    payload: gents_protocol::output::StreamPayload::ToolOutput,
                                })
                            })
                            .transpose()?;
                        Ok(gents_protocol::output::SegmentRun {
                            stream: u32::try_from(run.stream)?,
                            bytes: u32::try_from(run.bytes)?,
                            declaration,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();
        Ok(gents_protocol::output::OutputSegment {
            agent_did: self.principal.clone(),
            requester_did: Some(self.principal.clone()),
            session_id: self.session_id.clone(),
            request_doc_id: self.request_doc_id.clone(),
            source: gents_protocol::output::OutputSource::ToolCall {
                tool_call_doc_id: physical_tool.to_owned(),
            },
            writer: gents_protocol::output::OutputWriter::ToolExecution {
                tool_call_doc_id: physical_tool.to_owned(),
            },
            ordinal: record
                .flush
                .as_ref()
                .map(|flush| u32::try_from(flush.ordinal))
                .transpose()?,
            runs,
            payload: record
                .flush
                .as_ref()
                .map(|flush| String::from_utf8(flush.payload.clone()))
                .transpose()?
                .unwrap_or_default(),
            close: record.close.as_ref().map(|close| match close {
                crate::lean_vocab_test::LeanCanonicalClosure::Closed {
                    outcome,
                    segments,
                    stream_bytes,
                } => SourceClose::Closed {
                    outcome: match outcome {
                        LeanOutcome::Complete => OutputOutcome::Complete,
                        LeanOutcome::Partial => OutputOutcome::Partial,
                    },
                    segments: u32::try_from(*segments).expect("modeled segment count exceeds u32"),
                    stream_bytes: stream_bytes.clone(),
                },
                crate::lean_vocab_test::LeanCanonicalClosure::Retracted => SourceClose::Retracted,
            }),
            created_at: self.fixture_time(record.created_at)?.to_rfc3339(),
        })
    }
}

#[tokio::test]
async fn every_generated_native_execution_script_runs_to_completion() {
    let cases = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .filter(|case| {
            matches!(
                case,
                crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { .. }
            )
        })
        .collect::<Vec<_>>();
    assert!(
        !cases.is_empty(),
        "Lean exported no native execution scripts"
    );
    let mut failures = Vec::new();
    for case in cases {
        let mut adapter = NativeCanonicalExecutionAdapter;
        if let Err(error) =
            crate::lean_vocab_test::assert_native_execution_case(case, &mut adapter).await
        {
            failures.push(error);
        }
    }
    assert!(
        failures.is_empty(),
        "native execution contract gaps:\n{}",
        failures.join("\n")
    );
}

/// Native representation negative controls outside Lean's valid Time domain:
/// a missing durable deadline fails in the append owner, while malformed input
/// is rejected by DefraDB's DateTime scalar before it can become a durable row.
#[tokio::test]
async fn generated_tool_append_rejects_invalid_durable_deadline_without_writing() {
    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| {
            matches!(case,
                crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { name, .. }
                    if name == "tool_output_before_deadline")
        })
        .expect("Lean exports the native pre-deadline tool-output script");
    let crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution {
        seed,
        query_document,
        operations,
        expected_observations,
        ..
    } = case
    else {
        unreachable!()
    };
    let [accept, dispatch, append] = operations.as_slice() else {
        panic!("generated tool-output script must accept, dispatch, then append");
    };
    let LeanCanonicalExecutionOperation::AppendToolOutput {
        now,
        document,
        record,
        ..
    } = append
    else {
        panic!("third generated operation must append tool output");
    };
    let mut adapter = NativeCanonicalExecutionAdapter;
    let mut native = adapter.initialize(seed).await.unwrap();
    native.query_document = *query_document;
    for operation in [accept, dispatch] {
        assert!(
            adapter
                .apply(&mut native, *query_document, operation)
                .await
                .unwrap()
                .accepted
        );
    }
    let before = native.observe(true).await.unwrap();
    assert_eq!(before, expected_observations[1]);
    assert!(expected_observations[2].accepted);
    let physical = native.physical_tool(*document).unwrap().to_owned();
    let prepared = native
        .tool_output_segment(record, *document, &physical)
        .unwrap();
    let tool = crate::tool_call_lifecycle::ToolCallLifecycle::load_by_doc_id(
        native.node.clone(),
        &physical,
        &native.principal,
        &native.session_id,
        Some(&native.principal),
    )
    .await
    .unwrap()
    .expect("dispatched generated tool has a physical lifecycle");
    let binding = tool.tool_output_binding().unwrap();
    let physical = crate::graphql::escape_graphql_string(&physical);
    for (label, deadline_value) in [("missing", None), ("malformed", Some("not-a-deadline"))] {
        let deadline_field = deadline_value
            .map(|value| format!("\"{}\"", crate::graphql::escape_graphql_string(value)))
            .unwrap_or_else(|| "null".to_owned());
        let mutation = format!(
            r#"mutation {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{physical}" }} }}, input: {{ deadline_at: {deadline_field} }}) {{ _docID }} }}"#
        );
        let changed = crate::config_client::ConfigAccess::write_local(
            &native.node,
            "test.generated_tool_append_invalid_deadline",
            &mutation,
        )
        .await;
        if deadline_value.is_some() {
            let error = changed.expect_err("DateTime schema must reject malformed deadline input");
            assert!(
                error.to_string().contains("Invalid DateTime format"),
                "malformed deadline must fail at the schema boundary: {error:#}"
            );
            assert_eq!(native.observe(true).await.unwrap(), before);
            continue;
        }
        let changed = changed.unwrap();
        assert_eq!(
            changed["data"]["update_AgentToolCall"]
                .as_array()
                .map(Vec::len),
            Some(1),
            "{label} deadline fixture must update exactly one physical tool"
        );
        let error = crate::tool_call_lifecycle::delivery::append_tool_output_at(
            &binding,
            &prepared.payload,
            native.fixture_time(record.created_at).unwrap(),
            native.fixture_time(*now).unwrap(),
        )
        .await
        .expect_err("invalid durable deadline cannot authorize a fresh append");
        assert!(
            error
                .downcast_ref::<crate::tool_call_lifecycle::delivery::ToolOutputAppendRejection>()
                .is_none(),
            "{label} deadline is an integrity error, not a modeled rejection: {error:#}"
        );
        let mut expected_unchanged = before.clone();
        expected_unchanged.accepted = false;
        assert_eq!(
            native.observe(false).await.unwrap(),
            expected_unchanged,
            "{label} deadline must leave exact canonical output records unchanged"
        );
    }
    native.node.shutdown().await;
}

/// In-process, fixture-time gate experiment: this binds a generated lease-ordering
/// trace to the real transaction owner, not to host clock jumps or OS suspension.
#[tokio::test]
async fn generated_renewal_holds_write_gate_until_stale_recovery_loses() {
    use crate::config_client::ConfigApplyTxn;
    use crate::lifecycle::{RecoveryResult, RecoverySelectionChoice, RenewalAttemptOutcome};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::Notify;

    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| {
            matches!(case,
                crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { name, .. }
                    if name == "renewal_wins_before_terminal_recovery")
        })
        .expect("Lean exports the native renewal/recovery ordering case");
    let crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution {
        seed,
        query_document,
        operations,
        expected_observations,
        ..
    } = case
    else {
        unreachable!()
    };
    let [renewal, recovery] = operations.as_slice() else {
        panic!("generated gate case must contain renewal then recovery");
    };
    let [expected_after_renewal, expected_after_recovery] = expected_observations.as_slice() else {
        panic!("generated gate case must observe both operations");
    };
    let LeanCanonicalExecutionOperation::RenewLease {
        now: renewal_now,
        generation,
        expected_deadline,
        ..
    } = renewal
    else {
        panic!("first generated gate operation must renew");
    };
    let LeanCanonicalExecutionOperation::RecoverExpiredTerminal {
        now: recovery_now,
        expected_generation,
        fresh_generation,
        outcome,
        selection,
        items,
        ..
    } = recovery
    else {
        panic!("second generated gate operation must recover");
    };
    assert_eq!(expected_generation, generation);
    assert!(
        items.is_empty(),
        "this gate trace has no recovered output items"
    );
    let choice = match selection {
        LeanTerminalSelection::NoMessage => RecoverySelectionChoice::NoMessage,
        LeanTerminalSelection::Message { .. } => {
            panic!("this gate trace requires a no-message recovery selection")
        }
    };
    let target = match outcome.as_str() {
        "failed" => RequestLifecycleState::Failed,
        other => panic!("gate fixture requires failed recovery, got {other}"),
    };

    let mut adapter = NativeCanonicalExecutionAdapter;
    let mut native = adapter.initialize(seed).await.unwrap();
    native.query_document = *query_document;
    let node = Arc::clone(&native.node);
    let request_doc_id = native.request_doc_id.clone();
    let request = crate::graphql::escape_graphql_string(&request_doc_id);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{request}" }} }}, limit: 1) {{ {} }} }}"#,
        crate::watcher::AGENT_REQUEST_FIELDS,
    );
    let row = crate::graphql::graphql_with_transaction_retry(
        &node,
        &query,
        "test.generated_renewal_gate_stale_request",
    )
    .await
    .unwrap();
    let stale: AgentRequestRow = crate::graphql::first_row(&row, "AgentRequest")
        .unwrap()
        .expect("generated native request");
    let stale_expiry = stale.execution_lease_expires_at.as_deref().unwrap();
    let physical_generation = symbolic_generation(*generation);
    let physical_fresh_generation = symbolic_generation(*fresh_generation);
    let stale_deadline = DateTime::parse_from_rfc3339(stale_expiry)
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        stale_deadline,
        native.fixture_time(*expected_deadline).unwrap()
    );
    assert!(
        native.fixture_time(*recovery_now).unwrap() >= stale_deadline,
        "recovery must be due against its stale deadline before renewal wins"
    );
    assert_eq!(
        stale.execution_generation.as_deref(),
        Some(physical_generation.as_str())
    );

    let reached = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut renewal = Box::pin(ConfigApplyTxn::with_successful_mutation_pause_at(
        1,
        Arc::clone(&reached),
        Arc::clone(&release),
        crate::lifecycle::renew_execution_lease_once_at(
            &node,
            &request_doc_id,
            &physical_generation,
            native.fixture_time(*expected_deadline).unwrap(),
            native.fixture_time(*renewal_now).unwrap(),
        ),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            biased;
            completed = &mut renewal => panic!("renewal completed before its held mutation: {completed:?}"),
            _ = reached.notified() => {}
        }
    })
    .await
    .expect("renewal must reach the held write gate");

    let queued = Arc::new(Notify::new());
    let acquired = Arc::new(AtomicBool::new(false));
    let mut recovery = Box::pin(ConfigApplyTxn::with_write_gate_observation(
        Arc::clone(&queued),
        Arc::clone(&acquired),
        crate::lifecycle::recover_expired_generation_with_facts(
            &node,
            &stale,
            &physical_generation,
            stale_expiry,
            physical_fresh_generation,
            native.fixture_time(*recovery_now).unwrap(),
            Some(choice),
            Some(target),
        ),
    ));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::select! {
            biased;
            completed = &mut recovery => panic!("stale recovery completed before queuing: {completed:?}"),
            _ = queued.notified() => {}
        }
    })
    .await
    .expect("stale recovery must queue at the held write gate");
    assert!(
        !acquired.load(Ordering::Acquire),
        "stale recovery must not acquire the gate before renewal commits"
    );

    release.notify_one();
    let (renewed, fired) = tokio::time::timeout(std::time::Duration::from_secs(10), renewal)
        .await
        .expect("held renewal must finish after release");
    assert!(
        fired,
        "renewal must pause after its real successful mutation"
    );
    assert_eq!(renewed.unwrap(), RenewalAttemptOutcome::Committed);
    assert_eq!(native.observe(true).await.unwrap(), *expected_after_renewal);

    let recovered = tokio::time::timeout(std::time::Duration::from_secs(10), recovery)
        .await
        .expect("queued stale recovery must finish after renewal")
        .unwrap();
    assert!(
        acquired.load(Ordering::Acquire),
        "recovery must acquire the gate after renewal releases it"
    );
    assert_eq!(recovered, RecoveryResult::Lost);
    assert_eq!(
        native.observe(false).await.unwrap(),
        *expected_after_recovery
    );
    native.node.shutdown().await;
}

#[tokio::test]
async fn conflicting_spawned_child_document_is_an_adapter_gap_not_a_native_rejection() {
    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| {
            matches!(case,
                crate::lean_vocab_test::LeanCanonicalExecutionCase::ModelExecution { name, .. }
                    if name == "spawned_admission_conflicting_child_document_rejected")
        })
        .expect("Lean exports the conflicting spawned-child document as model-only");
    let crate::lean_vocab_test::LeanCanonicalExecutionCase::ModelExecution {
        seed,
        query_document,
        operations,
        expected_observations,
        ..
    } = case
    else {
        unreachable!()
    };
    assert_eq!(operations.len(), expected_observations.len());
    assert!(operations.len() > 1);
    assert!(
        !expected_observations.last().unwrap().accepted,
        "Lean must reject the conflicting child document"
    );

    let mut adapter = NativeCanonicalExecutionAdapter;
    let mut native = adapter.initialize(seed).await.unwrap();
    for (operation, expected) in operations[..operations.len() - 1]
        .iter()
        .zip(&expected_observations[..expected_observations.len() - 1])
    {
        let observed = adapter
            .apply(&mut native, *query_document, operation)
            .await
            .unwrap();
        assert_eq!(&observed, expected);
    }
    let error = adapter
        .apply(&mut native, *query_document, operations.last().unwrap())
        .await
        .expect_err("the native API cannot take a conflicting candidate child document");
    assert!(
        error
            .to_string()
            .contains("native spawned child maps to symbolic document"),
        "unexpected adapter error: {error}"
    );
    native.node.shutdown().await;
}

// `choice` is exported separately from the accepted provider arguments: this
// composes the real resolver and child-request owners, but does not claim the
// trigger's workspace-argument decoder selected Bind from those arguments.
#[tokio::test]
async fn generated_remote_depth_crosses_publication_and_child_creation_boundaries() {
    let contracts = crate::lean_vocab_test::lean_contract_snapshot();
    for (modeled_name, native_name) in [
        (
            "remote_depth_two_inherit_at_bound",
            "real_spawn_depth_two_copies_parent_depth",
        ),
        (
            "remote_depth_three_rejects_child",
            "real_spawn_depth_three_copies_parent_depth",
        ),
        (
            "readonly_parent_bind_readwrite_attenuates",
            "real_spawn_depth_two_copies_parent_depth",
        ),
    ] {
        let modeled = contracts
            .delegated_child_resolution_cases
            .iter()
            .find(|case| case.name == modeled_name)
            .expect("Lean exports the delegated child boundary");
        let source = modeled
            .delegated_input
            .as_ref()
            .expect("accepted Lean bridge has copied input");
        let native_case = contracts
            .canonical_execution_gate_cases
            .iter()
            .find(|case| {
                matches!(case,
                crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { name, .. }
                    if name == native_name)
            })
            .expect("Lean exports the matching real spawn publication");
        let crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution {
            seed,
            query_document,
            operations,
            ..
        } = native_case
        else {
            unreachable!()
        };
        assert_eq!(seed.subagent_depth, u64::from(modeled.parent_depth));
        assert_eq!(seed.principal, modeled.parent_agent);
        let crate::lean_vocab_test::LeanCanonicalExecutionOperation::AcceptRemote {
            targets,
            admissions,
            ..
        } = &operations[0]
        else {
            panic!("generated child boundary requires a remote publication");
        };
        let [target] = targets.as_slice() else {
            panic!("generated child boundary requires one route");
        };
        let [admission] = admissions.as_slice() else {
            panic!("generated child boundary requires one accepted call");
        };
        assert_eq!(target.target, modeled.child_agent);
        assert_eq!(target.call, admission.document);
        let mut adapter = NativeCanonicalExecutionAdapter;
        let mut native = adapter.initialize(seed).await.unwrap();
        let observed = adapter
            .apply(&mut native, *query_document, &operations[0])
            .await
            .unwrap();
        assert!(observed.accepted, "{native_name} did not publish");
        let tool_doc_id = native
            .tool_ids
            .iter()
            .find_map(|(doc_id, modeled_id)| {
                (*modeled_id == admission.document).then(|| doc_id.clone())
            })
            .expect("accepted native spawn has a physical tool row");
        let response = native
            .node
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ delegated_input delegated_workspace child_request_id spawn_target_did spawn_behavior_id }} }}"#,
                crate::graphql::escape_graphql_string(&tool_doc_id),
            ))
            .await;
        assert!(!response.has_errors(), "{:#?}", response.errors);
        let data = response.data.unwrap();
        let rows = data["AgentToolCall"]
            .as_array()
            .expect("accepted bridge rows");
        assert_eq!(rows.len(), 1, "accepted bridge must be physically unique");
        let copied: gents_protocol::output::DelegatedToolInput =
            serde_json::from_value(rows[0]["delegated_input"].clone()).unwrap();
        assert_eq!(copied.parent_subagent_depth, source.parent_subagent_depth);
        assert_eq!(copied.arguments, source.arguments);
        assert_eq!(u64::from(copied.source.stream), source.source_stream);
        assert_eq!(
            native.segment_ids.get(&copied.source.close_doc_id),
            Some(&source.source_close_doc_id)
        );
        let parent_stamp = modeled
            .parent_workspace
            .as_ref()
            .expect("modeled boundary has a parent workspace stamp");
        let seed_stamp = seed.workspace.as_ref().expect("native seed has workspace");
        assert_eq!(parent_stamp.workspace_id, seed_stamp.workspace_id);
        assert_eq!(
            parent_stamp.workspace_owner_agent_did,
            seed_stamp.workspace_owner_agent_did
        );
        assert_eq!(
            parent_stamp.workspace_authority,
            seed_stamp.workspace_authority
        );
        assert_eq!(
            parent_stamp.workspace_seal_hash,
            seed_stamp.workspace_seal_hash
        );
        let expected_workspace_id = format!("lean-workspace-{}", parent_stamp.workspace_id);
        let expected_owner = modeled_principal_did(
            parent_stamp.workspace_owner_agent_did,
            seed.principal,
            &native.principal,
            &native.remote_dids,
        );
        assert_eq!(
            rows[0]["delegated_workspace"]["workspace_id"].as_str(),
            Some(expected_workspace_id.as_str())
        );
        assert_eq!(
            rows[0]["delegated_workspace"]["workspace_owner_agent_did"].as_str(),
            Some(expected_owner.as_str())
        );
        assert_eq!(
            rows[0]["delegated_workspace"]["workspace_authority"].as_str(),
            Some(parent_stamp.workspace_authority.as_str())
        );
        assert_eq!(
            rows[0]["delegated_workspace"]["workspace_seal_hash"].as_str(),
            parent_stamp
                .workspace_seal_hash
                .map(|seal| format!("lean-seal-{seal}"))
                .as_deref()
        );
        let accepted = &native.accepted_spawns[&admission.document].0;
        let child_id = rows[0]["child_request_id"]
            .as_str()
            .expect("accepted bridge retained child identity")
            .to_owned();
        let child_did = rows[0]["spawn_target_did"]
            .as_str()
            .expect("accepted bridge retained target DID")
            .to_owned();
        let behavior_id = rows[0]["spawn_behavior_id"]
            .as_str()
            .expect("accepted bridge retained behavior")
            .to_owned();
        assert_eq!(
            child_did,
            modeled_principal_did(
                modeled.child_agent,
                seed.principal,
                &native.principal,
                &native.remote_dids,
            )
        );
        let prompt = serde_json::from_str::<serde_json::Value>(&source.arguments).unwrap()
            ["prompt"]
            .as_str()
            .expect("modeled accepted arguments contain a prompt")
            .to_owned();
        if modeled.expected.is_none() {
            let error =
                crate::tool_call_lifecycle::create_subagent_request_with_trusted_parent_request_id(
                    &native.node,
                    child_id,
                    format!("lean-request-{}", seed.request_id),
                    native.request_doc_id.clone(),
                    accepted.id.clone(),
                    tool_doc_id,
                    copied.parent_subagent_depth,
                    child_did,
                    behavior_id,
                    prompt,
                    None,
                    native.principal.clone(),
                )
                .await
                .expect_err("modeled maximum parent depth must reject child creation");
            assert!(matches!(
                error.downcast_ref::<crate::tool_call_lifecycle::IllegalToolCallTransition>(),
                Some(crate::tool_call_lifecycle::IllegalToolCallTransition::SubagentDepthExceeded)
            ));
        } else {
            let stamp =
                crate::tool_call_lifecycle::subagent_workspace::ParentWorkspaceStamp::from_fields(
                    &native.principal,
                    Some(&expected_workspace_id),
                    Some(&expected_owner),
                    Some(&parent_stamp.workspace_authority),
                    parent_stamp
                        .workspace_seal_hash
                        .map(|seal| format!("lean-seal-{seal}"))
                        .as_deref(),
                );
            let workspace_arg = match &modeled.choice {
                crate::lean_vocab_test::LeanDelegatedChildChoice::Inherit { workspace } => {
                    assert_eq!(workspace.workspace_id, parent_stamp.workspace_id);
                    assert_eq!(
                        workspace.workspace_owner_agent_did,
                        parent_stamp.workspace_owner_agent_did
                    );
                    assert_eq!(
                        workspace.workspace_seal_hash,
                        parent_stamp.workspace_seal_hash
                    );
                    assert_eq!(workspace.state, "ready");
                    assert!(workspace.available);
                    crate::background_tools::SpawnWorkspaceArg::Inherit
                }
                crate::lean_vocab_test::LeanDelegatedChildChoice::Bind {
                    workspace,
                    requested_authority,
                } => {
                    assert_eq!(workspace.workspace_id, parent_stamp.workspace_id);
                    assert_eq!(
                        workspace.workspace_owner_agent_did,
                        parent_stamp.workspace_owner_agent_did
                    );
                    assert_eq!(
                        workspace.workspace_seal_hash,
                        parent_stamp.workspace_seal_hash
                    );
                    assert_eq!(workspace.state, "ready");
                    assert!(workspace.available);
                    crate::background_tools::SpawnWorkspaceArg::Bind {
                        id: format!("lean-workspace-{}", workspace.workspace_id),
                        authority: requested_authority.clone(),
                    }
                }
                _ => panic!("this native child binding only covers inherit and bind"),
            };
            let workspace =
                crate::tool_call_lifecycle::subagent_workspace::resolve_child_workspace(
                    &native.node,
                    &stamp,
                    Some(&workspace_arg),
                    None,
                    &child_did,
                    &format!("lean-child-invocation-{}", admission.document),
                    &format!("lean-child-correlation-{}", admission.document),
                    None,
                )
                .await
                .expect("real workspace owner accepted generated choice");
            crate::tool_call_lifecycle::subagent_request::create_subagent_request_with_trusted_parent_request_id_and_workspace(
                &native.node,
                child_id.clone(),
                format!("lean-request-{}", seed.request_id),
                native.request_doc_id.clone(),
                accepted.id.clone(),
                tool_doc_id,
                copied.parent_subagent_depth,
                child_did.clone(),
                behavior_id,
                prompt,
                None,
                native.principal.clone(),
                workspace,
            )
            .await
            .expect("real signed child owner accepted generated boundary");
            let result = native.node.execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{ {} admission_signer_did admission_signature }} }}"#,
                crate::graphql::escape_graphql_string(&child_id),
                crate::watcher::AGENT_REQUEST_FIELDS,
            )).await;
            assert!(!result.has_errors(), "{:?}", result.errors);
            let child = crate::graphql::first_row::<AgentRequestRow>(&result, "AgentRequest")
                .unwrap()
                .expect("signed child persisted");
            let expected = modeled.expected.as_ref().unwrap();
            assert_eq!(child.subagent_depth, Some(i64::from(expected.child_depth)));
            assert_eq!(child.agent_did.as_deref(), Some(child_did.as_str()));
            assert_eq!(
                child.requester_did.as_deref(),
                Some(native.principal.as_str())
            );
            assert_eq!(
                child.admission_signer_did.as_deref(),
                Some(child_did.as_str())
            );
            assert!(
                child
                    .admission_signature
                    .as_deref()
                    .is_some_and(|signature| !signature.is_empty()),
                "registered target did not persist a signed child admission"
            );
            let expected_workspace = expected
                .child_workspace
                .as_ref()
                .expect("modeled child inherits or binds workspace");
            assert_eq!(
                child.workspace_id.as_deref(),
                Some(format!("lean-workspace-{}", expected_workspace.workspace_id).as_str())
            );
            assert_eq!(
                child.workspace_owner_agent_did.as_deref(),
                Some(
                    modeled_principal_did(
                        expected_workspace.workspace_owner_agent_did,
                        seed.principal,
                        &native.principal,
                        &native.remote_dids
                    )
                    .as_str()
                )
            );
            assert_eq!(
                child.workspace_authority.as_deref(),
                Some(expected_workspace.workspace_authority.as_str())
            );
            assert_eq!(
                child.workspace_seal_hash.as_deref(),
                expected_workspace
                    .workspace_seal_hash
                    .map(|seal| format!("lean-seal-{seal}"))
                    .as_deref()
            );
        }
    }
}

// The native fixture supplies the physical IsolatedWorkspace observation; the
// stamped parent comes from the generated source. This binds the modeled
// negative choices to the real resolver, not to a test-local seal policy.
#[tokio::test]
async fn generated_provision_parent_seal_drift_reaches_real_resolver() {
    let contracts = crate::lean_vocab_test::lean_contract_snapshot();
    let native_case = contracts
        .canonical_execution_gate_cases
        .iter()
        .find(|case| {
            matches!(case,
                crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { name, .. }
                    if name == "real_spawn_depth_two_copies_parent_depth")
        })
        .expect("Lean exports a native workspace fixture");
    let crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { seed, .. } =
        native_case
    else {
        unreachable!()
    };
    for name in [
        "provision_rejects_changed_parent_seal",
        "provision_rejects_absent_to_present_parent_seal",
    ] {
        let modeled = contracts
            .delegated_child_resolution_cases
            .iter()
            .find(|case| case.name == name)
            .expect("Lean exports the parent-seal drift case");
        assert!(modeled.delegated_input.is_some());
        assert!(modeled.expected.is_none());
        let parent = modeled
            .parent_workspace
            .as_ref()
            .expect("generated accepted source has a parent workspace");
        let crate::lean_vocab_test::LeanDelegatedChildChoice::Provision {
            observed_parent,
            parent_path_exact,
            created_child,
        } = &modeled.choice
        else {
            panic!("{name} must be a provision choice");
        };
        assert!(*parent_path_exact);
        assert!(created_child.is_some());
        assert!(observed_parent.available);
        assert_ne!(
            parent.workspace_seal_hash,
            observed_parent.workspace_seal_hash
        );
        assert_eq!(parent.workspace_id, observed_parent.workspace_id);
        assert_eq!(
            parent.workspace_owner_agent_did,
            observed_parent.workspace_owner_agent_did
        );
        assert_eq!(seed.principal, modeled.parent_agent);

        let mut adapter = NativeCanonicalExecutionAdapter;
        let native = adapter.initialize(seed).await.unwrap();
        let workspace_id = format!("lean-workspace-{}", parent.workspace_id);
        let owner = modeled_principal_did(
            parent.workspace_owner_agent_did,
            seed.principal,
            &native.principal,
            &native.remote_dids,
        );
        let mut document =
            crate::callback::load_isolated_workspace(&native.node, &workspace_id, &owner)
                .await
                .unwrap()
                .expect("native fixture has an observed parent workspace");
        document.seal_hash = observed_parent
            .workspace_seal_hash
            .map(|seal| format!("lean-seal-{seal}"));
        let response = native
            .node
            .execute(&crate::workspace::isolated_workspace_upsert_mutation(
                &document,
            ))
            .await;
        assert!(!response.has_errors(), "{name}: {:?}", response.errors);
        let stamped_seal = parent
            .workspace_seal_hash
            .map(|seal| format!("lean-seal-{seal}"));
        let stamp =
            crate::tool_call_lifecycle::subagent_workspace::ParentWorkspaceStamp::from_fields(
                &native.principal,
                Some(&workspace_id),
                Some(&owner),
                Some(&parent.workspace_authority),
                stamped_seal.as_deref(),
            );
        let child_did = modeled_principal_did(
            modeled.child_agent,
            seed.principal,
            &native.principal,
            &native.remote_dids,
        );
        let error = crate::tool_call_lifecycle::subagent_workspace::resolve_child_workspace(
            &native.node,
            &stamp,
            Some(&crate::background_tools::SpawnWorkspaceArg::Provision { policy: None }),
            None,
            &child_did,
            &format!("lean-provision-{name}"),
            &format!("lean-provision-correlation-{name}"),
            None,
        )
        .await
        .expect_err("modeled seal drift must reject through the real resolver");
        assert!(
            error.message.contains("parent workspace seal drift"),
            "{name}: {error}"
        );
        native.node.shutdown().await;
    }
}

#[tokio::test]
async fn terminal_request_without_tool_handoff_still_reports_in_flight() {
    let case = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| {
            matches!(case,
            crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { name, .. }
                if name == "running_foreground_terminal_recovery_records_handoff")
        })
        .expect("Lean exports the running foreground terminal-recovery case");
    let crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution {
        seed,
        operations,
        ..
    } = case
    else {
        unreachable!()
    };
    let mut adapter = NativeCanonicalExecutionAdapter;
    let mut native = adapter.initialize(seed).await.unwrap();
    for operation in operations.iter().take(2) {
        let observed = adapter.apply(&mut native, 600, operation).await.unwrap();
        assert!(observed.accepted);
    }
    let before = native.observe(true).await.unwrap();
    assert!(before.in_flight);
    assert_eq!(before.tool_stuck_since, None);

    // Deliberately inject a terminal request without the terminal owner's
    // tool-accounting write. This is a negative control for the observation:
    // request terminality alone must not conceal an unreleased foreground tool.
    let doc_id = crate::graphql::escape_graphql_string(&native.request_doc_id);
    let mutation = format!(
        r#"mutation {{ update_AgentRequest(
        filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
        input: {{ lifecycle_state: "failed" }}) {{ _docID }} }}"#
    );
    crate::config_client::ConfigAccess::write_local(
        &native.node,
        "test.native_in_flight_negative_control",
        &mutation,
    )
    .await
    .unwrap();
    let after = native.observe(true).await.unwrap();
    assert_eq!(after.request_state, "failed");
    assert_eq!(after.tool_state.as_deref(), Some("running"));
    assert_eq!(after.tool_stuck_since, None);
    assert!(after.in_flight);
    native.node.shutdown().await;
}

#[tokio::test]
async fn canonical_tool_output_uses_modeled_physical_source_facts() {
    use crate::session::canonical_rows::{
        output_segment_create_variables, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };

    let witness = crate::lean_vocab_test::lean_r4c_background_work_case(
        "r4c.read_tool_output.canonical_source_reconstruction",
    );
    let crate::lean_vocab_test::LeanR4cBackgroundWorkCase::ReadToolOutputCanonicalSourceReconstruction {
        cases, ..
    } = witness else { unreachable!() };
    assert_eq!(
        cases.len(),
        5,
        "Lean must export every canonical source case"
    );

    let acceptance = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| matches!(case, crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution { name, .. } if name == "running_foreground_terminal_recovery_records_handoff"))
        .expect("foreground acceptance case");
    let crate::lean_vocab_test::LeanCanonicalExecutionCase::NativeExecution {
        seed,
        operations,
        ..
    } = acceptance
    else {
        unreachable!()
    };

    for case in cases {
        let mut adapter = NativeCanonicalExecutionAdapter;
        let mut native = adapter.initialize(seed).await.unwrap();
        adapter
            .apply(&mut native, 600, &operations[0])
            .await
            .unwrap();
        let physical_tool = native.tool_ids.iter().find_map(|(physical, symbolic)| {
            (*symbolic == case.document).then_some(physical.clone())
        });
        if physical_tool.is_none() {
            assert!(
                case.expected_state.is_none(),
                "{} lost its physical tool",
                case.name
            );
            continue;
        }
        let physical_tool = physical_tool.expect("modeled source lacks accepted physical tool");
        for record in &case.segments {
            if !matches!(&record.coordinate.source, LeanCanonicalSource::Tool { call } if *call == case.document)
            {
                continue;
            }
            let segment = native
                .tool_output_segment(record, case.document, &physical_tool)
                .unwrap();
            let response = native
                .node
                .execute_request_with_retry(
                    defra_node::QueryRequest::new(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION)
                        .with_variables(output_segment_create_variables(&segment).unwrap()),
                    defra_node::ExecuteRetryPolicy::default(),
                )
                .await;
            assert!(
                !response.has_errors(),
                "{}: {:?}",
                case.name,
                response.errors
            );
        }
        let actual = crate::background_tools::canonical_tool_output(
            &native.node,
            &physical_tool,
            &native.request_doc_id,
            &native.session_id,
            &native.principal,
            Some(&native.principal),
        )
        .await;
        match &case.expected_payload {
            Some(expected) => assert_eq!(
                actual.unwrap().as_bytes(),
                expected,
                "{} canonical output drifted",
                case.name
            ),
            None => assert!(actual.is_err(), "{} must fail closed", case.name),
        }
    }
}
