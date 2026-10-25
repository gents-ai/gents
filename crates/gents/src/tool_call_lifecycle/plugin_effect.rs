use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::output::{
    reconstruction::{reconstruct_stream, ObservedSegment},
    MessageBlock, MessagePublication, MessageRole, OutputOutcome, OutputSegment, OutputSource,
    OutputWriter, PayloadRef, SegmentRun, SourceClose, StreamDeclaration, StreamPayload,
};

use crate::config_client::{ConfigAccess, ConfigApplyTxn, IdempotentTransactionRetry};
use crate::graphql::{created_doc_id, escape_graphql_string};
use crate::session::canonical_rows::{
    decode_scoped_request_output_segments, output_segment_create_variables,
    request_output_segments_query, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};

use super::{ToolCallLifecycle, ToolCallLifecycleError, ToolCallState};

type Result<T, E = ToolCallLifecycleError> = std::result::Result<T, E>;
use crate::plugin::tool_calls::{MAX_EFFECTS, MAX_PAYLOAD_BYTES};

#[derive(Clone)]
pub(super) struct PluginEffectTerminal {
    pub parent: super::delivery::ToolOutputBinding,
    pub effect: super::PluginEffectBinding,
    pub generation: String,
}

impl ToolCallLifecycle {
    pub(super) fn plugin_effect_terminal(
        &self,
    ) -> Result<Option<PluginEffectTerminal>, ToolCallLifecycleError> {
        self.plugin_effect
            .as_ref()
            .map(|effect| {
                let parent = self.plugin_effect_parent_binding()?;
                Ok(PluginEffectTerminal {
                    parent,
                    effect: effect.clone(),
                    generation: self
                        .execution_generation
                        .clone()
                        .ok_or(ToolCallLifecycleError::PluginEffectLacksGeneration)?,
                })
            })
            .transpose()
    }

    pub(crate) async fn admit_plugin_effect(
        &self,
        ordinal: u32,
        name: &str,
        arguments: &str,
    ) -> Result<Self> {
        if self.state != ToolCallState::Running
            || self.plugin_effect.is_some()
            || self.spawned_by_tool_call_doc_id.is_some()
        {
            return Err(ToolCallLifecycleError::PluginEffectRequiresRunningDirectParent);
        }
        if !((1..=MAX_EFFECTS).contains(&ordinal)
            && !name.trim().is_empty()
            && arguments.len() <= MAX_PAYLOAD_BYTES)
        {
            return Err(ToolCallLifecycleError::PluginEffectExceedsInvocationBounds);
        }
        let _: serde_json::Map<String, serde_json::Value> = serde_json::from_str(arguments)
            .map_err(|source| ToolCallLifecycleError::PluginEffectArgumentsNotObject { source })?;
        let binding = self.tool_output_binding()?;
        let header = self
            .accepted_header_doc_id
            .clone()
            .ok_or(ToolCallLifecycleError::PluginParentLacksAcceptedHeader)?;
        let generation = self
            .execution_generation
            .clone()
            .ok_or(ToolCallLifecycleError::PluginParentLacksGeneration)?;
        let name = name.to_owned();
        let arguments = arguments.to_owned();
        let sequence = self.message_sequence;
        let deadline = gents_loop::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .and_then(|context| context.deadline_at)
            .map_or(self.deadline_at, |deadline| deadline.min(self.deadline_at));
        let key = super::PluginEffectBinding::internal_id(&binding.tool_call_doc_id, ordinal);
        let internal_id = key.clone();
        let child = ConfigAccess::transact_local_idempotent(
            &self.node, None, IdempotentTransactionRetry::Standard, "tool_call.admit_plugin_effect",
            |txn| {
                let binding = binding.clone(); let header = header.clone(); let generation = generation.clone();
                let name = name.clone(); let arguments = arguments.clone();
                let key = key.clone(); let internal_id = internal_id.clone();
                Box::pin(async move {
                    let request_id = validate_parent(txn, &binding, &header, &generation, sequence, deadline, true).await?;
                    let scope = crate::session::session_scope_filter(&binding.node_did, &binding.session_id,
                        binding.requester_did.as_deref());
                    let parent = escape_graphql_string(&binding.tool_call_doc_id);
                    let result = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{ {scope},
                        plugin_parent_tool_call_doc_id: {{ _eq: "{parent}" }}, plugin_effect_ordinal: {{ _eq: {ordinal} }}
                    }}, limit: 2) {{ _docID tool_call_key tool_call_id tool_name request_id request_doc_id message_sequence
                        deadline_at spawned_by_tool_call_doc_id }} }}"#)).await?;
                    let rows = result["data"]["AgentToolCall"].as_array().ok_or(ToolCallLifecycleError::PluginEffectLookupOmittedRows)?;
                    if rows.len() > 1 {
                        return Err(ToolCallLifecycleError::PluginEffectConflictingIdentities);
                    }
                    if let Some(row) = rows.first() {
                        let child = row["_docID"].as_str().ok_or(ToolCallLifecycleError::PluginEffectLacksPhysicalIdentity)?;
                        let immutable = row["tool_call_key"].as_str() == Some(&key)
                            && row["tool_call_id"].as_str() == Some(&internal_id)
                            && row["tool_name"].as_str() == Some(&name)
                            && row["request_id"].as_str() == Some(&request_id)
                            && row["request_doc_id"].as_str() == Some(&binding.request_doc_id)
                            && row["message_sequence"].as_u64() == Some(u64::from(sequence))
                            && row["spawned_by_tool_call_doc_id"].is_null()
                            && DateTime::parse_from_rfc3339(row["deadline_at"].as_str().unwrap_or(""))?
                                .with_timezone(&Utc) == deadline;
                        if !immutable {
                            return Err(ToolCallLifecycleError::PluginEffectReplayChangedAdmission);
                        }
                        let (_, stored) = arguments_in_txn(txn, &binding, child, &generation, &internal_id, &name).await?;
                        if stored != arguments {
                            return Err(ToolCallLifecycleError::PluginEffectReplayChangedArguments);
                        }
                        return Ok(child.to_owned());
                    }
                    let created = txn.execute(&format!(r#"mutation {{ create_AgentToolCall(input: {{
                        tool_call_key: "{}", tool_call_id: "{}", tool_name: "{}", request_id: "{}",
                        request_doc_id: "{}", session_id: "{}", node_did: "{}", {}
                        message_sequence: {sequence}, deadline_at: "{}", status: "pending", lifecycle_state: "pending",
                        await_mode: "foreground", plugin_parent_tool_call_doc_id: "{parent}", plugin_effect_ordinal: {ordinal}
                    }}) {{ _docID }} }}"#,
                        escape_graphql_string(&key), escape_graphql_string(&internal_id), escape_graphql_string(&name),
                        escape_graphql_string(&request_id), escape_graphql_string(&binding.request_doc_id),
                        escape_graphql_string(&binding.session_id), escape_graphql_string(&binding.node_did),
                        crate::session::requester_did_create_field(binding.requester_did.as_deref()),
                        escape_graphql_string(&deadline.to_rfc3339_opts(SecondsFormat::Nanos, true)))).await?;
                    let child = created_doc_id(&created, "AgentToolCall")?;
                    let segment = OutputSegment {
                        node_did: binding.node_did, requester_did: binding.requester_did,
                        session_id: binding.session_id, request_doc_id: binding.request_doc_id,
                        source: OutputSource::ToolEffectArguments { tool_call_doc_id: child.clone() },
                        writer: OutputWriter::RequestExecution { execution_generation: generation },
                        ordinal: Some(0),
                        runs: vec![SegmentRun { stream: 0, bytes: u32::try_from(arguments.len())?,
                            declaration: Some(StreamDeclaration { block_index: 0, part_index: 0,
                                payload: StreamPayload::ToolArguments { id: internal_id, call_id: None, name } }) }],
                        close: Some(SourceClose::Closed { outcome: OutputOutcome::Complete, segments: 1,
                            stream_bytes: vec![arguments.len() as u64] }), payload: arguments,
                        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
                    };
                    txn.execute_with_variables(CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                        &output_segment_create_variables(&segment)?).await?;
                    Ok(child)
                })
            }).await?;
        Self::load_by_doc_id(
            self.node.clone(),
            &child,
            &self.node_did,
            &self.session_id,
            self.requester_did.as_deref(),
        )
        .await?
        .ok_or(ToolCallLifecycleError::PluginEffectAdmittedDisappeared)
    }

    fn plugin_effect_parent_binding(
        &self,
    ) -> Result<super::delivery::ToolOutputBinding, ToolCallLifecycleError> {
        let effect = self
            .plugin_effect
            .as_ref()
            .ok_or(ToolCallLifecycleError::PluginEffectLacksProvenance)?;
        Ok(super::delivery::ToolOutputBinding {
            node: self.node.clone(),
            tool_call_doc_id: effect.parent_tool_call_doc_id.clone(),
            request_doc_id: self
                .request_doc_id
                .clone()
                .ok_or(ToolCallLifecycleError::PluginEffectLacksRequestBinding)?,
            session_id: self.session_id.clone(),
            node_did: self.node_did.clone(),
            requester_did: self.requester_did.clone(),
        })
    }

    pub(crate) fn plugin_effect_internal_id(&self) -> &str {
        &self.tool_call_id
    }

    pub(super) async fn start_running_plugin_effect(&mut self) -> Result<()> {
        let effect = self
            .plugin_effect
            .clone()
            .ok_or(ToolCallLifecycleError::PluginEffectLacksProvenance)?;
        let parent = self.plugin_effect_parent_binding()?;
        let child = self
            .doc_id
            .clone()
            .ok_or(ToolCallLifecycleError::PluginEffectLacksPhysicalIdentity)?;
        let header = self
            .accepted_header_doc_id
            .clone()
            .ok_or(ToolCallLifecycleError::PluginEffectLacksParentHeader)?;
        let generation = self
            .execution_generation
            .clone()
            .ok_or(ToolCallLifecycleError::PluginEffectLacksGeneration)?;
        let sequence = self.message_sequence;
        let deadline = self.deadline_at;
        let internal_id = self.tool_call_id.clone();
        let name = self.tool_name.clone();
        let selected_tool_fields = self.selected_tool_fields_fragment();
        let started = ConfigAccess::transact_local_idempotent(&self.node, None,
            IdempotentTransactionRetry::Standard, "tool_call.start_plugin_effect", |txn| {
            let parent = parent.clone(); let child = child.clone(); let header = header.clone();
            let generation = generation.clone(); let internal_id = internal_id.clone(); let name = name.clone();
            let selected_tool_fields = selected_tool_fields.clone();
            Box::pin(async move {
                validate_parent(txn, &parent, &header, &generation, sequence, deadline, true).await?;
                arguments_in_txn(txn, &parent, &child, &generation, &internal_id, &name).await?;
                let now = Utc::now();
                let scope = crate::session::session_scope_filter(&parent.node_did, &parent.session_id,
                    parent.requester_did.as_deref());
                let result = txn.execute(&format!(r#"mutation {{ update_AgentToolCall(docID: "{}", filter: {{
                    {scope}, request_doc_id: {{ _eq: "{}" }}, plugin_parent_tool_call_doc_id: {{ _eq: "{}" }},
                    plugin_effect_ordinal: {{ _eq: {} }}, tool_call_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }},
                    message_sequence: {{ _eq: {sequence} }}, await_mode: {{ _eq: "foreground" }},
                    spawned_by_tool_call_doc_id: {{ _eq: null }}, lifecycle_state: {{ _eq: "pending" }}
                }}, input: {{ {selected_tool_fields} lifecycle_state: "running", deadline_at: "{}", started_at: "{}" }}) {{ _docID }} }}"#,
                    escape_graphql_string(&child), escape_graphql_string(&parent.request_doc_id),
                    escape_graphql_string(&parent.tool_call_doc_id), effect.ordinal,
                    escape_graphql_string(&internal_id), escape_graphql_string(&name),
                    escape_graphql_string(&deadline.to_rfc3339_opts(SecondsFormat::Nanos, true)),
                    escape_graphql_string(&now.to_rfc3339_opts(SecondsFormat::Nanos, true)))).await?;
                if !result["data"]["update_AgentToolCall"].as_array().is_some_and(|rows| rows.len() == 1) {
                    return Err(ToolCallLifecycleError::PluginEffectNoLongerPending);
                }
                Ok(now)
            })
        }).await?;
        self.state = ToolCallState::Running;
        self.started_at = Some(started);
        Ok(())
    }
}

pub(super) async fn validate_parent(
    txn: &ConfigApplyTxn<'_>,
    binding: &super::delivery::ToolOutputBinding,
    header: &str,
    generation: &str,
    sequence: u32,
    deadline: DateTime<Utc>,
    require_live: bool,
) -> Result<String> {
    let (accepted, _) = crate::session::load_canonical_message_in_txn(
        txn,
        header,
        &binding.node_did,
        binding.requester_did.as_deref(),
    )
    .await?;
    let scope = crate::session::session_scope_filter(
        &binding.node_did,
        &binding.session_id,
        binding.requester_did.as_deref(),
    );
    let rows = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{ {scope}, _docID: {{ _eq: "{}" }},
        request_doc_id: {{ _eq: "{}" }} }}, limit: 2) {{ tool_call_id tool_name message_sequence lifecycle_state
        spawned_by_tool_call_doc_id plugin_parent_tool_call_doc_id plugin_effect_ordinal deadline_at }} }}"#,
        escape_graphql_string(&binding.tool_call_doc_id), escape_graphql_string(&binding.request_doc_id))).await?;
    let rows = rows["data"]["AgentToolCall"]
        .as_array()
        .ok_or(ToolCallLifecycleError::PluginParentLookupOmittedRows)?;
    if rows.len() != 1 {
        return Err(ToolCallLifecycleError::PluginParentMissingOrAmbiguous);
    }
    let parent = &rows[0];
    let parent_deadline = DateTime::parse_from_rfc3339(
        parent["deadline_at"]
            .as_str()
            .ok_or(ToolCallLifecycleError::PluginParentLacksDeadline)?,
    )?
    .with_timezone(&Utc);
    if !(deadline <= parent_deadline && (!require_live || deadline > Utc::now())) {
        return Err(ToolCallLifecycleError::PluginEffectDeadlineExceedsParent);
    }
    let exact_parent = parent["spawned_by_tool_call_doc_id"].is_null()
        && parent["plugin_parent_tool_call_doc_id"].is_null() && parent["plugin_effect_ordinal"].is_null()
        && parent["message_sequence"].as_u64() == Some(u64::from(sequence))
        && (!require_live || parent["lifecycle_state"].as_str() == Some("running"))
        && accepted.session_id == binding.session_id
        && accepted.request_doc_id.as_deref() == Some(&binding.request_doc_id)
        && accepted.role == MessageRole::Assistant && accepted.outcome == OutputOutcome::Complete
        && accepted.sequence == sequence
        && matches!(&accepted.publication, MessagePublication::RequestExecution { execution_generation }
            if execution_generation == generation)
        && accepted.blocks.iter().any(|block| matches!(block, MessageBlock::ToolCall { tool_call_doc_id, id, name, .. }
            if tool_call_doc_id == &binding.tool_call_doc_id && Some(id.as_str()) == parent["tool_call_id"].as_str()
                && Some(name.as_str()) == parent["tool_name"].as_str()));
    if !exact_parent {
        return Err(ToolCallLifecycleError::PluginEffectNoExactAcceptedParent);
    }
    let rows = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ {scope}, _docID: {{ _eq: "{}" }} }}, limit: 2) {{
        request_id lifecycle_state execution_generation execution_lease_expires_at interrupt_requested_at
    }} }}"#,
            escape_graphql_string(&binding.request_doc_id)
        ))
        .await?;
    let rows = rows["data"]["AgentRequest"]
        .as_array()
        .ok_or(ToolCallLifecycleError::PluginRequestLookupOmittedRows)?;
    if rows.len() != 1 {
        return Err(ToolCallLifecycleError::PluginRequestMissingOrAmbiguous);
    }
    let request = &rows[0];
    if require_live {
        let live = request["lifecycle_state"].as_str() == Some("processing")
            && request["execution_generation"].as_str() == Some(generation)
            && request["interrupt_requested_at"].is_null()
            && DateTime::parse_from_rfc3339(
                request["execution_lease_expires_at"].as_str().unwrap_or(""),
            )?
            .with_timezone(&Utc)
                > Utc::now();
        if !live {
            return Err(ToolCallLifecycleError::PluginEffectLostRequestAuthority);
        }
    }
    request["request_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or(ToolCallLifecycleError::PluginRequestLacksLogicalIdentity)
}

pub(super) async fn arguments_in_txn(
    txn: &ConfigApplyTxn<'_>,
    binding: &super::delivery::ToolOutputBinding,
    child: &str,
    generation: &str,
    internal_id: &str,
    name: &str,
) -> Result<(PayloadRef, String)> {
    let response = txn
        .execute(&request_output_segments_query(&binding.request_doc_id))
        .await?;
    let rows = response["data"]["AgentOutputSegment"]
        .as_array()
        .ok_or(ToolCallLifecycleError::PluginArgumentsLookupOmittedSegments)?;
    arguments_from_rows(
        rows,
        &binding.node_did,
        &binding.session_id,
        binding.requester_did.as_deref(),
        &binding.request_doc_id,
        child,
        generation,
        internal_id,
        name,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn arguments_from_rows(
    rows: &[serde_json::Value],
    node_did: &str,
    session_id: &str,
    requester_did: Option<&str>,
    request_doc_id: &str,
    child: &str,
    generation: &str,
    internal_id: &str,
    name: &str,
) -> Result<(PayloadRef, String), ToolCallLifecycleError> {
    let records =
        decode_scoped_request_output_segments(rows, node_did, Some(session_id), requester_did)?;
    let source = OutputSource::ToolEffectArguments {
        tool_call_doc_id: child.to_owned(),
    };
    let records = records
        .iter()
        .filter(|row| row.segment.source == source)
        .collect::<Vec<_>>();
    if records.len() != 1 {
        return Err(ToolCallLifecycleError::PluginEffectArgumentsSourceNotSingle);
    }
    let record = records[0];
    let closed = record.segment.request_doc_id == request_doc_id
        && matches!(&record.segment.writer, OutputWriter::RequestExecution { execution_generation }
        if execution_generation == generation)
        && matches!(&record.segment.close, Some(SourceClose::Closed { outcome: OutputOutcome::Complete,
        segments: 1, stream_bytes }) if stream_bytes.len() == 1);
    if !closed {
        return Err(ToolCallLifecycleError::PluginEffectInvalidArgumentsClosure);
    }
    let reference = PayloadRef {
        close_doc_id: record.doc_id.clone(),
        stream: 0,
    };
    let reconstructed = reconstruct_stream(
        &[ObservedSegment {
            doc_id: &record.doc_id,
            segment: &record.segment,
        }],
        &[],
        &[],
        &reference,
    )?;
    let declaration = StreamDeclaration {
        block_index: 0,
        part_index: 0,
        payload: StreamPayload::ToolArguments {
            id: internal_id.to_owned(),
            call_id: None,
            name: name.to_owned(),
        },
    };
    if reconstructed.declaration != declaration {
        return Err(ToolCallLifecycleError::PluginEffectArgumentDeclarationChanged);
    }
    if reconstructed.text.len() > MAX_PAYLOAD_BYTES {
        return Err(ToolCallLifecycleError::PluginEffectArgumentsExceedBound);
    }
    let _: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&reconstructed.text)
            .map_err(|source| ToolCallLifecycleError::PluginEffectArgumentsNotObject { source })?;
    Ok((reference, reconstructed.text))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission_with_owner, PublishedAdmission, PublishedAdmissionOptions,
    };

    fn payload(segment: &serde_json::Value) -> String {
        let bytes = segment["flush"]["payload"]
            .as_array()
            .unwrap()
            .iter()
            .map(|byte| u8::try_from(byte.as_u64().unwrap()).unwrap())
            .collect();
        String::from_utf8(bytes).unwrap()
    }

    #[tokio::test]
    async fn generated_plugin_effect_admission_matches_execution_owner() {
        let cases =
            &crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases["tool_effects"];
        for case in cases.as_array().expect("generated plugin effect scenarios") {
            let name = case["name"].as_str().unwrap();
            let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
                name: format!("effect-{name}"),
                ..Default::default()
            })
            .await
            .unwrap();
            let PublishedAdmission {
                node,
                path,
                tool: mut parent,
                ..
            } = fixture;
            let ordinal = u32::try_from(case["ordinal"].as_u64().unwrap()).unwrap();
            let tool = case["arguments"]["flush"]["runs"][0]["declaration"]["tool"]["name"]
                .as_str()
                .unwrap();
            let previous = if case["prior"].is_null() {
                None
            } else {
                Some(
                    parent
                        .admit_plugin_effect(ordinal, tool, &payload(&case["prior"]))
                        .await
                        .unwrap(),
                )
            };
            if !case["parent"]["present"].as_bool().unwrap() {
                parent.doc_id = Some("missing-physical-parent".into());
            } else {
                if case["parent"]["request"].as_u64() != Some(10) {
                    parent.request_doc_id = Some("foreign-physical-request".into());
                }
                if case["parent"]["session"].as_u64() != Some(1) {
                    parent.session_id = "foreign-session".into();
                }
                if case["parent"]["provenance"].as_str() == Some("plugin_effect") {
                    parent.plugin_effect = Some(super::super::PluginEffectBinding {
                        parent_tool_call_doc_id: "ancestor".into(),
                        ordinal: 1,
                    });
                }
                if case["parent"]["state"]
                    .as_str()
                    .unwrap()
                    .contains("completed")
                {
                    parent.complete("finished").await.unwrap();
                }
                let extra = case["child"]["deadline"].as_i64().unwrap()
                    - case["parent"]["deadline"].as_i64().unwrap();
                parent.deadline_at += chrono::Duration::seconds(extra);
            }
            if case["generation"].as_u64() != Some(7) {
                parent.execution_generation = Some("stale-generation".into());
            }
            let result = parent
                .admit_plugin_effect(ordinal, tool, &payload(&case["arguments"]))
                .await;
            assert_eq!(
                result.is_ok(),
                case["expected"].as_bool().unwrap(),
                "{name}: {:?}",
                result.as_ref().err()
            );
            if let Ok(child) = result {
                assert_eq!(child.state(), ToolCallState::Pending, "{name}");
                assert!(child.arguments.is_some(), "{name}");
                if let Some(previous) = previous {
                    assert_eq!(
                        child.doc_id(),
                        previous.doc_id(),
                        "replay must retain physical identity"
                    );
                }
            }
            drop(owner);
            node.shutdown().await;
            let _ = std::fs::remove_dir_all(path);
        }
    }
}
