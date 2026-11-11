use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::output::{
    OutputOutcome, OutputSegment, OutputSource, OutputWriter, PayloadRef, SegmentRun, SourceClose,
    StreamDeclaration, StreamPayload,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;

use crate::store::config_client::session_children::output_segment_bound_variables;
use crate::store::config_client::{ConfigApplyTxn, IdempotentTransactionRetry};
use crate::store::graphql::{created_doc_id, escape_graphql_string};
use gents_protocol::output::rows::{
    request_output_segments_filter, AGENT_OUTPUT_SEGMENT_FIELDS,
    CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
};

use super::ToolCallLifecycle;
use crate::session::plugin_effect_rows::{
    arguments_from_rows, receipt_from_rows, receipt_source, PluginEffectBinding,
};
use crate::session::tool_call_state::ToolCallState;
use gents_protocol::plugin_tool_call_limits::{MAX_EFFECTS, MAX_PAYLOAD_BYTES};

#[derive(Clone)]
pub(super) struct PluginEffectTerminal {
    pub parent: super::delivery::ToolOutputBinding,
    pub effect: PluginEffectBinding,
    pub generation: String,
}

impl ToolCallLifecycle {
    pub(super) fn plugin_effect_terminal(&self) -> Result<Option<PluginEffectTerminal>> {
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
                        .context("plugin effect lacks generation")?,
                })
            })
            .transpose()
    }

    #[cfg(test)]
    pub(crate) async fn admit_plugin_effect(
        &self,
        ordinal: u32,
        name: &str,
        arguments: &str,
    ) -> Result<Self> {
        self.admit_plugin_effect_with_mode(
            ordinal,
            name,
            arguments,
            gents_protocol::tool_call::AwaitMode::Foreground,
        )
        .await
    }

    pub(crate) async fn admit_plugin_effect_with_mode(
        &self,
        ordinal: u32,
        name: &str,
        arguments: &str,
        mode: gents_protocol::tool_call::AwaitMode,
    ) -> Result<Self> {
        anyhow::ensure!(
            self.state == ToolCallState::Running && self.plugin_effect.is_none(),
            "plugin effects require a running parent with delegation authority"
        );
        anyhow::ensure!(
            (1..=MAX_EFFECTS).contains(&ordinal)
                && !name.trim().is_empty()
                && arguments.len() <= MAX_PAYLOAD_BYTES,
            "plugin effect exceeds its invocation bounds"
        );
        let _: serde_json::Map<String, serde_json::Value> = serde_json::from_str(arguments)
            .context("plugin effect arguments must be a JSON object")?;
        let binding = self.tool_output_binding()?;
        let header = self
            .accepted_header_doc_id
            .clone()
            .context("plugin parent lacks accepted header")?;
        let generation = self
            .execution_generation
            .clone()
            .context("plugin parent lacks generation")?;
        let name = name.to_owned();
        let arguments = arguments.to_owned();
        let sequence = self.message_sequence;
        let deadline = gents_loop::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .and_then(|context| context.deadline_at)
            .map_or(self.deadline_at, |deadline| deadline.min(self.deadline_at));
        let key = PluginEffectBinding::internal_id(&binding.tool_call_doc_id, ordinal);
        let internal_id = key.clone();
        let mode = mode.as_str();
        let child = self.access()?.transact_idempotent(
            IdempotentTransactionRetry::Standard, "tool_call.admit_plugin_effect",
            |txn| {
                let binding = binding.clone(); let header = header.clone(); let generation = generation.clone();
                let name = name.clone(); let arguments = arguments.clone();
                let key = key.clone(); let internal_id = internal_id.clone();
                Box::pin(async move {
                    let request_id = validate_parent(txn, &binding, &header, &generation, sequence, deadline, true).await?;
                    let parent = escape_graphql_string(&binding.tool_call_doc_id);
                    let result = {
                        use gents_protocol::document_query::{fields::agent_tool_call as fields, session_scope, Filter, Query, Selection};
                        txn.run(&Query::select(
                            Selection::doc_id()
                                .field(fields::TOOL_CALL_KEY)
                                .field(fields::TOOL_CALL_ID)
                                .field(fields::TOOL_NAME)
                                .field(fields::REQUEST_ID)
                                .field(fields::REQUEST_DOC_ID)
                                .field(fields::MESSAGE_SEQUENCE)
                                .field(fields::DEADLINE_AT)
                                .field(fields::SPAWNED_BY_TOOL_CALL_DOC_ID),
                        )
                        .filter(session_scope(&binding.node_did, &binding.session_id, binding.requester_did.as_deref()))
                        .filter(Filter::eq(fields::PLUGIN_PARENT_TOOL_CALL_DOC_ID, &binding.tool_call_doc_id))
                        .filter(Filter::eq(fields::PLUGIN_EFFECT_ORDINAL, ordinal))
                        .limit(2)
                        .render()).await?
                    };
                    let rows = result["data"]["AgentToolCall"].as_array().context("plugin effect lookup omitted rows")?;
                    anyhow::ensure!(rows.len() <= 1, "plugin effect has conflicting physical identities");
                    if let Some(row) = rows.first() {
                        let child = row["_docID"].as_str().context("plugin effect lacks physical identity")?;
                        anyhow::ensure!(row["tool_call_key"].as_str() == Some(&key)
                            && row["tool_call_id"].as_str() == Some(&internal_id)
                            && row["tool_name"].as_str() == Some(&name)
                            && row["request_id"].as_str() == Some(&request_id)
                            && row["request_doc_id"].as_str() == Some(&binding.request_doc_id)
                            && row["message_sequence"].as_u64() == Some(u64::from(sequence))
                            && row["spawned_by_tool_call_doc_id"].is_null()
                            && DateTime::parse_from_rfc3339(row["deadline_at"].as_str().unwrap_or(""))?
                                .with_timezone(&Utc) == deadline,
                            "plugin effect replay changed immutable admission");
                        let (_, stored) = arguments_in_txn(txn, &binding, child, &generation, &internal_id, &name).await?;
                        anyhow::ensure!(stored == arguments, "plugin effect replay changed arguments");
                        return Ok(child.to_owned());
                    }
                    let (session_doc_id, selected_agent) = crate::store::config_client::session_children::agent_session_child_binding_in_txn(
                        txn, &binding.node_did, &binding.session_id, binding.requester_did.as_deref()).await?;
                    let session_doc_id = escape_graphql_string(&session_doc_id);
                    let selected_agent = escape_graphql_string(&selected_agent);
                    let created = txn.execute(&format!(r#"mutation {{ create_AgentToolCall(input: {{
                        session_doc_id: "{session_doc_id}", agent_did: "{selected_agent}", creator_did: "{selected_agent}", parent_linked: false,
                        tool_call_key: "{}", tool_call_id: "{}", tool_name: "{}", request_id: "{}",
                        request_doc_id: "{}", session_id: "{}", node_did: "{}", {}
                        message_sequence: {sequence}, deadline_at: "{}", status: "pending", lifecycle_state: "pending",
                        await_mode: "{mode}", plugin_parent_tool_call_doc_id: "{parent}", plugin_effect_ordinal: {ordinal}
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
                        &output_segment_bound_variables(txn, &segment).await?).await?;
                    Ok(child)
                })
            }).await?;
        crate::store::config_client::session_children::publish_agent_session_child_relationships_with_access(
            &self.access()?,
            &self.node_did,
            &self.actor,
            &self.session_id,
        )
        .await?;
        let mut effect = Self::load_by_doc_id_as(
            self.node.clone(),
            self.actor.clone(),
            &child,
            &self.node_did,
            &self.session_id,
            self.requester_did.as_deref(),
        )
        .await?
        .context("admitted plugin effect disappeared")?;
        effect.inherit_signer(self)?;
        Ok(effect)
    }

    fn plugin_effect_parent_binding(&self) -> Result<super::delivery::ToolOutputBinding> {
        let effect = self
            .plugin_effect
            .as_ref()
            .context("plugin effect lacks provenance")?;
        Ok(super::delivery::ToolOutputBinding {
            actor: self.actor.clone(),
            agent_signer: self.agent_signer.clone(),
            node: self.node.clone(),
            tool_call_doc_id: effect.parent_tool_call_doc_id.clone(),
            request_doc_id: self
                .request_doc_id
                .clone()
                .context("plugin effect lacks request binding")?,
            session_id: self.session_id.clone(),
            node_did: self.node_did.clone(),
            requester_did: self.requester_did.clone(),
        })
    }

    pub(crate) fn plugin_effect_internal_id(&self) -> &str {
        &self.tool_call_id
    }

    /// Whether the background invocation this `spawn_process` call starts
    /// receives a tool dispatcher. One that a plugin child spawned does not:
    /// it would compose with a fresh call budget, and so would whatever it
    /// spawned in turn, unbounded in depth (Lean
    /// `ToolPolicy.PluginCalls.dispatcherFor`).
    pub(crate) fn spawns_composing_invocation(&self) -> bool {
        spawned_may_compose(self.plugin_effect.is_some())
    }

    /// Terminalize one child whose invocation stopped before the child reached
    /// its own terminal write.
    pub(crate) async fn settle_interrupted_plugin_effect(
        &mut self,
        cause: super::CancelCause,
    ) -> Result<()> {
        match self.state {
            ToolCallState::Pending => self.cancel_before_dispatch(cause).await?,
            ToolCallState::Running
                if cause == super::CancelCause::Deadline
                    || self.is_deadline_expired(Utc::now()) =>
            {
                self.timeout().await?;
            }
            ToolCallState::Running => {
                self.cancel_during_run(cause).await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Settle the foreground children this stopped invocation left pending or
    /// running, returning their internal ids. A stopped invocation drops its
    /// in-flight child call, and a drop between a child's admission commit and
    /// its registration leaves a row no in-memory owner knows, so the children
    /// are read from the durable rows. A background child (an asynchronous
    /// receipt, an adopted job) keeps its own owner.
    pub(crate) async fn settle_unfinished_plugin_effects(
        &self,
        cause: super::CancelCause,
    ) -> Result<Vec<String>> {
        let parent = self
            .doc_id()
            .context("stopped plugin parent lacks physical identity")?;
        let request = self
            .request_doc_id
            .as_deref()
            .context("stopped plugin parent lacks request binding")?;
        let scope = gents_protocol::session::session_scope_filter(
            &self.node_did,
            &self.session_id,
            self.requester_did.as_deref(),
        );
        let rows = self
            .access()?
            .execute(&format!(
                r#"{{ AgentToolCall(filter: {{ {scope}, request_doc_id: {{ _eq: "{}" }},
                    plugin_parent_tool_call_doc_id: {{ _eq: "{}" }},
                    lifecycle_state: {{ _in: ["pending", "running"] }},
                    await_mode: {{ _eq: "foreground" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(request),
                escape_graphql_string(parent)
            ))
            .await?;
        let rows = rows["data"]["AgentToolCall"]
            .as_array()
            .context("unfinished plugin effect lookup omitted rows")?;
        let mut settled = Vec::with_capacity(rows.len());
        for row in rows {
            let doc = row["_docID"]
                .as_str()
                .context("unfinished plugin effect lacks physical identity")?;
            let Some(mut child) = Self::load_by_doc_id_as(
                self.node.clone(),
                self.actor.clone(),
                doc,
                &self.node_did,
                &self.session_id,
                self.requester_did.as_deref(),
            )
            .await?
            else {
                continue;
            };
            child.inherit_signer(self)?;
            child.settle_interrupted_plugin_effect(cause).await?;
            settled.push(child.tool_call_id.clone());
        }
        Ok(settled)
    }

    pub(super) async fn start_running_plugin_effect(&mut self) -> Result<()> {
        self.start_running_canonical_with_time(None).await
    }
}

fn spawned_may_compose(spawner_is_plugin_child: bool) -> bool {
    !spawner_is_plugin_child
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
    let accepted = ToolCallLifecycle::load_accepted_binding_in_txn(
        txn,
        &binding.tool_call_doc_id,
        &binding.node_did,
        &binding.session_id,
        binding.requester_did.as_deref(),
        true,
    )
    .await?;
    let rows = {
        use gents_protocol::document_query::{
            fields::agent_tool_call as fields, session_scope, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(
                Selection::new()
                    .field(fields::TOOL_CALL_ID)
                    .field(fields::TOOL_NAME)
                    .field(fields::MESSAGE_SEQUENCE)
                    .field(fields::LIFECYCLE_STATE)
                    .field(fields::SPAWNED_BY_TOOL_CALL_DOC_ID)
                    .field(fields::PLUGIN_PARENT_TOOL_CALL_DOC_ID)
                    .field(fields::PLUGIN_EFFECT_ORDINAL)
                    .field(fields::DEADLINE_AT)
                    .field(fields::AWAIT_MODE),
            )
            .filter(session_scope(
                &binding.node_did,
                &binding.session_id,
                binding.requester_did.as_deref(),
            ))
            .filter(Filter::eq(fields::DOC_ID, &binding.tool_call_doc_id))
            .filter(Filter::eq(fields::REQUEST_DOC_ID, &binding.request_doc_id))
            .limit(2)
            .render(),
        )
        .await?
    };
    let rows = rows["data"]["AgentToolCall"]
        .as_array()
        .context("plugin parent lookup omitted rows")?;
    anyhow::ensure!(rows.len() == 1, "plugin parent is missing or ambiguous");
    let parent = &rows[0];
    let parent_deadline = DateTime::parse_from_rfc3339(
        parent["deadline_at"]
            .as_str()
            .context("plugin parent lacks deadline")?,
    )?
    .with_timezone(&Utc);
    anyhow::ensure!(
        deadline <= parent_deadline && (!require_live || deadline > Utc::now()),
        "plugin effect deadline exceeds parent or has expired"
    );
    anyhow::ensure!(
        parent["plugin_parent_tool_call_doc_id"].is_null()
            && parent["plugin_effect_ordinal"].is_null()
            && parent["message_sequence"].as_u64() == Some(u64::from(sequence))
            && (!require_live || parent["lifecycle_state"].as_str() == Some("running"))
            && accepted.session_id == binding.session_id
            && accepted.request_doc_id == binding.request_doc_id
            && accepted.message_sequence == sequence
            && accepted.accepted_header_doc_id == header
            && accepted.execution_generation == generation
            && Some(accepted.id.as_str()) == parent["tool_call_id"].as_str()
            && Some(accepted.tool_name.as_str()) == parent["tool_name"].as_str(),
        "plugin effect has no exact accepted parent"
    );
    let rows = {
        use gents_protocol::document_query::{
            fields::agent_request as fields, session_scope, Filter, Query, Selection,
        };
        txn.run(
            &Query::select(
                Selection::new()
                    .field(fields::REQUEST_ID)
                    .field(fields::LIFECYCLE_STATE)
                    .field(fields::EXECUTION_GENERATION)
                    .field(fields::EXECUTION_LEASE_EXPIRES_AT)
                    .field(fields::INTERRUPT_REQUESTED_AT),
            )
            .filter(session_scope(
                &binding.node_did,
                &binding.session_id,
                binding.requester_did.as_deref(),
            ))
            .filter(Filter::eq(fields::DOC_ID, &binding.request_doc_id))
            .limit(2)
            .render(),
        )
        .await?
    };
    let rows = rows["data"]["AgentRequest"]
        .as_array()
        .context("plugin request lookup omitted rows")?;
    anyhow::ensure!(rows.len() == 1, "plugin request is missing or ambiguous");
    let request = &rows[0];
    if require_live && parent["await_mode"] != "background" {
        anyhow::ensure!(
            RequestLifecycleState::parse_opt(request["lifecycle_state"].as_str())
                == Some(RequestLifecycleState::Processing)
                && request["execution_generation"].as_str() == Some(generation)
                && request["interrupt_requested_at"].is_null()
                && DateTime::parse_from_rfc3339(
                    request["execution_lease_expires_at"].as_str().unwrap_or("")
                )?
                .with_timezone(&Utc)
                    > Utc::now(),
            "plugin effect lost live request authority"
        );
    }
    request["request_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .context("plugin request lacks logical identity")
}

pub(super) async fn arguments_in_txn(
    txn: &ConfigApplyTxn<'_>,
    binding: &super::delivery::ToolOutputBinding,
    child: &str,
    generation: &str,
    internal_id: &str,
    name: &str,
) -> Result<(PayloadRef, String)> {
    let rows = txn
        .cursor_rows(
            "AgentOutputSegment",
            AGENT_OUTPUT_SEGMENT_FIELDS,
            &request_output_segments_filter(&binding.request_doc_id),
        )
        .await?;
    arguments_from_rows(
        &rows,
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

pub(super) async fn publish_receipt_in_txn(
    txn: &ConfigApplyTxn<'_>,
    binding: &super::delivery::BackgroundReceiptBinding,
    text: &str,
) -> Result<bool> {
    let effect = binding
        .effect
        .as_ref()
        .context("plugin receipt lacks provenance")?;
    anyhow::ensure!(
        text.len() <= MAX_PAYLOAD_BYTES,
        "plugin receipt exceeds its payload bound"
    );
    let scope = gents_protocol::session::session_scope_filter(
        &binding.node_did,
        &binding.session_id,
        binding.requester_did.as_deref(),
    );
    let rows = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }},
        {scope}, request_doc_id: {{ _eq: "{}" }}, plugin_parent_tool_call_doc_id: {{ _eq: "{}" }},
        plugin_effect_ordinal: {{ _eq: {} }}, tool_call_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }},
        lifecycle_state: {{ _eq: "running" }}, await_mode: {{ _eq: "background" }} }}, limit: 2) {{ deadline_at }} }}"#,
        escape_graphql_string(&binding.tool_doc_id), escape_graphql_string(&binding.request_doc_id),
        escape_graphql_string(&effect.parent.tool_call_doc_id), effect.effect.ordinal,
        escape_graphql_string(&binding.tool_call_id), escape_graphql_string(&binding.tool_name))).await?;
    let rows = rows["data"]["AgentToolCall"]
        .as_array()
        .context("plugin receipt lookup omitted rows")?;
    anyhow::ensure!(
        rows.len() == 1,
        "plugin receipt requires its running background row"
    );
    let deadline = DateTime::parse_from_rfc3339(
        rows[0]["deadline_at"]
            .as_str()
            .context("plugin receipt lacks deadline")?,
    )?
    .with_timezone(&Utc);
    validate_parent(
        txn,
        &effect.parent,
        &binding.accepted_header_doc_id,
        &binding.generation,
        binding.message_sequence,
        deadline,
        false,
    )
    .await?;
    let rows = txn
        .cursor_rows(
            "AgentOutputSegment",
            AGENT_OUTPUT_SEGMENT_FIELDS,
            &request_output_segments_filter(&binding.request_doc_id),
        )
        .await?;
    if let Some(prior) = receipt_from_rows(
        &rows,
        &binding.node_did,
        &binding.session_id,
        binding.requester_did.as_deref(),
        &binding.request_doc_id,
        &binding.tool_doc_id,
    )? {
        anyhow::ensure!(prior == text, "plugin receipt replay changed its output");
        return Ok(false);
    }
    let segment = OutputSegment {
        node_did: binding.node_did.clone(),
        requester_did: binding.requester_did.clone(),
        session_id: binding.session_id.clone(),
        request_doc_id: binding.request_doc_id.clone(),
        source: receipt_source(&binding.session_id, &binding.tool_doc_id),
        writer: OutputWriter::ToolExecution {
            tool_call_doc_id: binding.tool_doc_id.clone(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: u32::try_from(text.len())?,
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::ToolOutput,
            }),
        }],
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![text.len() as u64],
        }),
        payload: text.to_owned(),
        created_at: Utc::now().to_rfc3339_opts(SecondsFormat::Nanos, true),
    };
    txn.execute_with_variables(
        CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
        &output_segment_bound_variables(txn, &segment).await?,
    )
    .await?;
    Ok(true)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::call::lifecycle::admission_fixture::{
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

    #[test]
    fn spawned_dispatchers_conform_to_lean() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().plugin_resource_cases["tool_calls"]
            ["spawned_dispatcher"];
        for case in cases.as_array().unwrap() {
            assert_eq!(
                spawned_may_compose(case["spawner"] == "plugin_child"),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
    }

    #[tokio::test]
    async fn generated_plugin_effect_admission_matches_execution_owner() {
        let cases =
            &gents_lean_vocab::lean_contract_snapshot().plugin_resource_cases["tool_effects"];
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
                    parent.plugin_effect = Some(PluginEffectBinding {
                        parent_tool_call_doc_id: "ancestor".into(),
                        ordinal: 1,
                    });
                }
                if case["parent"]["await_mode"].as_str() == Some("background") {
                    parent.background().await.unwrap();
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
                .admit_plugin_effect_with_mode(
                    ordinal,
                    tool,
                    &payload(&case["arguments"]),
                    gents_protocol::tool_call::AwaitMode::from_persisted(
                        case["child"]["await_mode"].as_str().unwrap_or("foreground"),
                    )
                    .unwrap(),
                )
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
            node.shutdown().await.expect("shut down test node");
            let _ = std::fs::remove_dir_all(path);
        }
    }
    #[tokio::test]
    async fn asynchronous_receipts_conform_to_the_execution_owner() {
        let cases = &gents_lean_vocab::lean_contract_snapshot().plugin_resource_cases
            ["tool_effect_receipts"];
        for (index, case) in cases.as_array().unwrap().iter().enumerate() {
            let (fixture, owner) = published_admission_with_owner(PublishedAdmissionOptions {
                name: format!("effect-receipt-{index}"),
                ..Default::default()
            })
            .await
            .unwrap();
            let PublishedAdmission {
                node,
                path,
                tool: parent,
                ..
            } = fixture;
            let mut child = parent
                .admit_plugin_effect_with_mode(
                    1,
                    "query",
                    "{}",
                    gents_protocol::tool_call::AwaitMode::Background,
                )
                .await
                .unwrap();
            child.start_running().await.unwrap();
            let binding = child.background_receipt_binding().unwrap();
            if case["terminal"] == true {
                child.complete("done").await.unwrap();
            }
            if case["replay"] == true {
                child
                    .access()
                    .unwrap()
                    .transact_idempotent(
                        IdempotentTransactionRetry::Standard,
                        "test.effect_receipt",
                        |txn| {
                            let binding = binding.clone();
                            Box::pin(
                                async move { publish_receipt_in_txn(txn, &binding, "{}").await },
                            )
                        },
                    )
                    .await
                    .unwrap();
            }
            let text = if case["changed"] == true {
                "null"
            } else {
                "{}"
            };
            let result = child
                .access()
                .unwrap()
                .transact_idempotent(
                    IdempotentTransactionRetry::Standard,
                    "test.effect_receipt",
                    |txn| {
                        let binding = binding.clone();
                        Box::pin(async move { publish_receipt_in_txn(txn, &binding, text).await })
                    },
                )
                .await;
            assert_eq!(
                result.is_ok(),
                case["expected"].as_bool().unwrap(),
                "{case}: {result:?}"
            );
            drop(owner);
            node.shutdown().await.unwrap();
            let _ = std::fs::remove_dir_all(path);
        }
    }
}
