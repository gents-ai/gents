use crate::store::config_client::{ConfigAccess, IdempotentTransactionRetry};
use crate::store::graphql::{created_doc_id, escape_graphql_string};
use crate::tools::call::lifecycle::{ToolCallLifecycle, ToolCallState};
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, OutputOutcome};

impl ToolCallLifecycle {
    pub(in crate::tools::call::lifecycle) async fn start_running_spawned_with_time(
        &mut self,
        fixture_now: Option<DateTime<Utc>>,
    ) -> Result<()> {
        let doc_id = self
            .doc_id
            .clone()
            .context("spawned dispatch requires child physical identity")?;
        let parent_doc_id = self
            .spawned_by_tool_call_doc_id
            .clone()
            .context("spawned dispatch requires accepted parent provenance")?;
        let request_doc_id = self
            .request_doc_id
            .clone()
            .context("spawned dispatch requires request binding")?;
        let session_id = self.session_id.clone();
        let node_did = self.node_did.clone();
        let requester_did = self.requester_did.clone();
        let tool_call_id = self.tool_call_id.clone();
        let tool_name = self.tool_name.clone();
        let message_sequence = self.message_sequence;
        let started = ConfigAccess::transact_local_idempotent(
            &self.node, None, IdempotentTransactionRetry::Standard,
            "tool_call.start_running_spawned_background",
            move |txn| {
                let doc_id = doc_id.clone(); let parent_doc_id = parent_doc_id.clone();
                let request_doc_id = request_doc_id.clone(); let session_id = session_id.clone();
                let node_did = node_did.clone(); let requester_did = requester_did.clone();
                let tool_call_id = tool_call_id.clone(); let tool_name = tool_name.clone();
                Box::pin(async move {
                    let now = fixture_now.unwrap_or_else(Utc::now);
                    let doc = escape_graphql_string(&doc_id);
                    let parent = escape_graphql_string(&parent_doc_id);
                    let request = escape_graphql_string(&request_doc_id);
                    let session = escape_graphql_string(&session_id);
                    let agent = escape_graphql_string(&node_did);
                    let requester_filter = requester_did.as_deref().map(|value| format!(r#", requester_did: {{ _eq: "{}" }}"#, escape_graphql_string(value)))
                        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
                    let mutation = txn.execute(&format!(r#"mutation {{ update_AgentToolCall(docID: "{doc}", filter: {{
                        _docID: {{ _eq: "{doc}" }}, spawned_by_tool_call_doc_id: {{ _eq: "{parent}" }},
                        request_doc_id: {{ _eq: "{request}" }}, session_id: {{ _eq: "{session}" }},
                        node_did: {{ _eq: "{agent}" }}, tool_call_id: {{ _eq: "{}" }},
                        tool_name: {{ _eq: "{}" }}, message_sequence: {{ _eq: {message_sequence} }},
                        await_mode: {{ _eq: "background" }},
                        lifecycle_state: {{ _eq: "pending" }}{requester_filter}
                    }}, input: {{ lifecycle_state: "running", started_at: "{}" }}) {{ _docID }} }}"#,
                        escape_graphql_string(&tool_call_id), escape_graphql_string(&tool_name),
                        escape_graphql_string(&now.to_rfc3339_opts(SecondsFormat::Nanos, true)))).await?;
                    Ok::<_, anyhow::Error>(mutation["data"]["update_AgentToolCall"].as_array()
                        .is_some_and(|rows| !rows.is_empty()).then_some(now))
                })
            }
        ).await?;
        let started = started.context("spawned background child is no longer pending")?;
        self.state = ToolCallState::Running;
        self.started_at = Some(started);
        Ok(())
    }

    pub(crate) async fn admit_spawned_background(
        &mut self,
        admission: crate::tools::call::lifecycle::SpawnedBackgroundToolAdmission,
        receipt: &str,
    ) -> Result<ToolCallLifecycle> {
        let child = self
            .admit_spawned_background_child_with_time(admission, None)
            .await?;
        if self.state == ToolCallState::Running {
            self.complete(receipt).await?;
        }
        Ok(child)
    }

    #[cfg(test)]
    pub(crate) async fn admit_spawned_background_child_at(
        &mut self,
        admission: crate::tools::call::lifecycle::SpawnedBackgroundToolAdmission,
        now: DateTime<Utc>,
    ) -> Result<ToolCallLifecycle> {
        self.admit_spawned_background_child_with_time(admission, Some(now))
            .await
    }

    async fn admit_spawned_background_child_with_time(
        &mut self,
        admission: crate::tools::call::lifecycle::SpawnedBackgroundToolAdmission,
        fixture_now: Option<DateTime<Utc>>,
    ) -> Result<ToolCallLifecycle> {
        anyhow::ensure!(
            matches!(
                self.state,
                ToolCallState::Running | ToolCallState::Completed
            ) && !self.is_spawned_background()
                && self.plugin_effect.is_none()
                && self.tool_name == crate::tools::native::SPAWN_PROCESS_TOOL_NAME,
            "spawned background admission requires the accepted spawn_process owner"
        );
        let parent_doc_id = self
            .doc_id
            .clone()
            .context("spawned admission requires parent document")?;
        let request_doc_id = self
            .request_doc_id
            .clone()
            .context("spawned admission requires parent request")?;
        let accepted_header_doc_id = self
            .accepted_header_doc_id
            .clone()
            .context("spawned admission requires parent accepted header")?;
        let generation = self
            .execution_generation
            .clone()
            .context("spawned admission requires parent execution generation")?;
        let session_id = self.session_id.clone();
        let node_did = self.node_did.clone();
        let requester_did = self.requester_did.clone();
        let message_sequence = self.message_sequence;
        let parent_tool_call_id = self.tool_call_id.clone();
        let deadline_at = admission.deadline_at;
        let tool_name = admission.tool_name;
        let persisted_selected = admission.selected_tool_identity;
        let stable_id = format!("spawned:{parent_doc_id}");
        let stable_key = format!("{parent_doc_id}:spawned-background");
        let persisted_parent_doc_id = parent_doc_id.clone();
        let persisted_request_doc_id = request_doc_id.clone();
        let persisted_accepted_header_doc_id = accepted_header_doc_id.clone();
        let persisted_generation = generation.clone();
        let persisted_session_id = session_id.clone();
        let persisted_node_did = node_did.clone();
        let persisted_requester_did = requester_did.clone();
        let persisted_parent_tool_call_id = parent_tool_call_id.clone();
        let persisted_tool_name = tool_name.clone();
        let persisted_stable_id = stable_id.clone();
        let persisted_stable_key = stable_key.clone();

        let created = ConfigAccess::transact_local_idempotent(
            &self.node, None, IdempotentTransactionRetry::Standard,
            "tool_call.admit_spawned_background",
            move |txn| {
                let parent_doc_id = persisted_parent_doc_id.clone();
                let request_doc_id = persisted_request_doc_id.clone();
                let accepted_header_doc_id = persisted_accepted_header_doc_id.clone();
                let generation = persisted_generation.clone();
                let session_id = persisted_session_id.clone();
                let node_did = persisted_node_did.clone();
                let requester_did = persisted_requester_did.clone();
                let parent_tool_call_id = persisted_parent_tool_call_id.clone();
                let tool_name = persisted_tool_name.clone();
                let stable_id = persisted_stable_id.clone();
                let stable_key = persisted_stable_key.clone();
                let selected = persisted_selected.clone();
                Box::pin(async move {
                    let parent = escape_graphql_string(&parent_doc_id);
                    let request = escape_graphql_string(&request_doc_id);
                    let session = escape_graphql_string(&session_id);
                    let agent = escape_graphql_string(&node_did);
                    let requester_filter = requester_did.as_deref().map(|value| format!(
                        r#", requester_did: {{ _eq: "{}" }}"#, escape_graphql_string(value)
                    )).unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
                    let existing = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{
                        spawned_by_tool_call_doc_id: {{ _eq: "{parent}" }}, request_doc_id: {{ _eq: "{request}" }},
                        session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }}{requester_filter}
                    }}, limit: 2) {{ _docID tool_call_key tool_call_id tool_name message_sequence
                        lifecycle_state await_mode spawned_by_tool_call_doc_id deadline_at
                        selected_service_id selected_tool_name }} }}"#)).await?;
                    let rows = existing["data"]["AgentToolCall"].as_array()
                        .context("spawned admission lookup omitted rows")?;
                    anyhow::ensure!(rows.len() <= 1, "accepted spawn_process already has ambiguous background children");
                    if let Some(child) = rows.first() {
                        let persisted_deadline = child["deadline_at"].as_str()
                            .context("spawned replay omitted child deadline")?;
                        let persisted_deadline = DateTime::parse_from_rfc3339(persisted_deadline)?
                            .with_timezone(&Utc);
                        anyhow::ensure!(
                            child["tool_call_key"].as_str() == Some(stable_key.as_str())
                                && child["tool_call_id"].as_str() == Some(stable_id.as_str())
                                && child["tool_name"].as_str() == Some(tool_name.as_str())
                                && child["message_sequence"].as_u64() == Some(u64::from(message_sequence))
                                && child["await_mode"].as_str() == Some("background")
                                && child["selected_service_id"].as_str()
                                    == selected.as_ref().map(|(service, _)| service.as_str())
                                && child["selected_tool_name"].as_str()
                                    == selected.as_ref().map(|(_, tool)| tool.as_str())
                                && persisted_deadline == deadline_at,
                            "spawned admission replay conflicts with immutable parent provenance"
                        );
                        return Ok(child["_docID"].as_str().context("spawned replay omitted child physical ID")?.to_owned());
                    }

                    let now = fixture_now.unwrap_or_else(Utc::now);
                    let (accepted, _) = crate::session::load_canonical_message_in_txn(
                        txn,
                        &accepted_header_doc_id,
                        &node_did,
                        requester_did.as_deref(),
                    )
                    .await?;
                    anyhow::ensure!(
                        accepted.session_id == session_id
                            && accepted.request_doc_id.as_deref() == Some(request_doc_id.as_str())
                            && accepted.role == MessageRole::Assistant
                            && accepted.outcome == OutputOutcome::Complete
                            && accepted.sequence == message_sequence
                            && matches!(&accepted.publication,
                                MessagePublication::RequestExecution { execution_generation }
                                if execution_generation == &generation)
                            && accepted.blocks.iter().any(|block| matches!(block,
                                MessageBlock::ToolCall { tool_call_doc_id, id, name, .. }
                                if tool_call_doc_id == &parent_doc_id
                                    && id == &parent_tool_call_id
                                    && name == crate::tools::native::SPAWN_PROCESS_TOOL_NAME)),
                        "spawned admission parent is not exactly bound by its accepted provider header"
                    );
                    let parent_row = txn.execute(&format!(r#"{{ AgentToolCall(filter: {{
                        _docID: {{ _eq: "{parent}" }}, request_doc_id: {{ _eq: "{request}" }},
                        session_id: {{ _eq: "{session}" }}, node_did: {{ _eq: "{agent}" }},
                        tool_call_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "spawn_process" }},
                        message_sequence: {{ _eq: {message_sequence} }}, lifecycle_state: {{ _eq: "running" }}{requester_filter}
                    }}, limit: 2) {{ _docID spawned_by_tool_call_doc_id }} }}"#, escape_graphql_string(&parent_tool_call_id))).await?;
                    anyhow::ensure!(parent_row["data"]["AgentToolCall"].as_array().is_some_and(|rows|
                        rows.len() == 1 && rows[0]["spawned_by_tool_call_doc_id"].is_null()),
                        "spawned admission lost its exact running accepted parent");
                    let request_row = &crate::tools::call::hook::persistence::reserve_background_slot_in_txn(
                        txn, &request_doc_id, crate::tools::call::hook::persistence::BackgroundSlot::Requested,
                        "node_did session_id request_id requester_did lifecycle_state execution_generation execution_lease_expires_at interrupt_requested_at",
                    ).await?;
                    anyhow::ensure!(
                        request_row["node_did"].as_str() == Some(node_did.as_str())
                            && request_row["session_id"].as_str() == Some(session_id.as_str())
                            && request_row["requester_did"].as_str() == requester_did.as_deref()
                            && request_row["lifecycle_state"].as_str() == Some("processing")
                            && request_row["execution_generation"].as_str() == Some(generation.as_str())
                            && request_row["interrupt_requested_at"].is_null(),
                        "spawned admission lost live request generation ownership"
                    );
                    let expires_at = request_row["execution_lease_expires_at"].as_str()
                        .context("spawned admission request lacks lease")?;
                    anyhow::ensure!(DateTime::parse_from_rfc3339(expires_at)?.with_timezone(&Utc) > now,
                        "spawned admission request lease has expired");
                    let request_id = request_row["request_id"].as_str()
                        .filter(|id| !id.trim().is_empty()).context("spawned admission request lacks logical identity")?;
                    let deadline = escape_graphql_string(&deadline_at.to_rfc3339_opts(SecondsFormat::Nanos, true));
                    let selected_fields = match &selected {
                        Some((service, tool)) => format!(
                            r#"selected_service_id: "{}", selected_tool_name: "{}","#,
                            escape_graphql_string(service), escape_graphql_string(tool)),
                        None => String::new(),
                    };
                    let created = txn.execute(&format!(r#"mutation {{ create_AgentToolCall(input: {{
                        tool_call_key: "{}", request_id: "{}", request_doc_id: "{request}",
                        session_id: "{session}", node_did: "{agent}", {}
                        message_sequence: {message_sequence}, tool_name: "{}", tool_call_id: "{}",
                        lifecycle_state: "pending", status: "pending", deadline_at: "{deadline}",
                        await_mode: "background",
                        {selected_fields} spawned_by_tool_call_doc_id: "{parent}"
                    }}) {{ _docID }} }}"#,
                        escape_graphql_string(&stable_key), escape_graphql_string(request_id),
                        crate::session::requester_did_create_field(requester_did.as_deref()),
                        escape_graphql_string(&tool_name), escape_graphql_string(&stable_id))).await?;
                    Ok(created_doc_id(&created, "AgentToolCall")?)
                })
            }
        ).await?;

        ToolCallLifecycle::load_by_doc_id(
            self.node.clone(),
            &created,
            &self.node_did,
            &self.session_id,
            self.requester_did.as_deref(),
        )
        .await?
        .context("spawned admission created child could not be rehydrated by physical provenance")
    }
}
