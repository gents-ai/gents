use crate::call::lifecycle::ToolCallLifecycle;
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_session::session::tool_call_state::ToolCallState;
use gents_store::config_client::IdempotentTransactionRetry;
use gents_store::graphql::created_doc_id;

impl ToolCallLifecycle {
    pub(in crate::call::lifecycle) async fn start_running_spawned_with_time(
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
        let started = self
            .access()?
            .transact_idempotent(
                IdempotentTransactionRetry::Standard,
                "tool_call.start_running_spawned_background",
                move |txn| {
                    let doc_id = doc_id.clone();
                    let parent_doc_id = parent_doc_id.clone();
                    let request_doc_id = request_doc_id.clone();
                    let session_id = session_id.clone();
                    let node_did = node_did.clone();
                    let requester_did = requester_did.clone();
                    let tool_call_id = tool_call_id.clone();
                    let tool_name = tool_name.clone();
                    Box::pin(async move {
                        let now = fixture_now.unwrap_or_else(Utc::now);
                        let mutation = {
                            use gents_protocol::document_query::{
                                fields::agent_tool_call as fields, Filter, Input, Update,
                            };
                            let requester = match requester_did.as_deref() {
                                Some(value) => Filter::eq(fields::REQUESTER_DID, value),
                                None => Filter::is_null(fields::REQUESTER_DID),
                            };
                            let filter = Filter::eq(fields::DOC_ID, &doc_id)
                                .and(Filter::eq(
                                    fields::SPAWNED_BY_TOOL_CALL_DOC_ID,
                                    &parent_doc_id,
                                ))
                                .and(Filter::eq(fields::REQUEST_DOC_ID, &request_doc_id))
                                .and(Filter::eq(fields::SESSION_ID, &session_id))
                                .and(Filter::eq(fields::NODE_DID, &node_did))
                                .and(Filter::eq(fields::TOOL_CALL_ID, &tool_call_id))
                                .and(Filter::eq(fields::TOOL_NAME, &tool_name))
                                .and(Filter::eq(fields::MESSAGE_SEQUENCE, message_sequence))
                                .and(Filter::eq(fields::AWAIT_MODE, "background"))
                                .and(Filter::eq(fields::LIFECYCLE_STATE, "pending"))
                                .and(requester);
                            let input = Input::new().set(fields::LIFECYCLE_STATE, "running").set(
                                fields::STARTED_AT,
                                now.to_rfc3339_opts(SecondsFormat::Nanos, true),
                            );
                            let rendered = Update::doc(&doc_id, input).filter(filter).render();
                            txn.run(&rendered).await?
                        };
                        Ok::<_, anyhow::Error>(
                            mutation["data"]["update_AgentToolCall"]
                                .as_array()
                                .is_some_and(|rows| !rows.is_empty())
                                .then_some(now),
                        )
                    })
                },
            )
            .await?;
        let started = started.context("spawned background child is no longer pending")?;
        self.state = ToolCallState::Running;
        self.started_at = Some(started);
        Ok(())
    }

    pub(crate) async fn admit_spawned_background(
        &mut self,
        admission: crate::call::lifecycle::SpawnedBackgroundToolAdmission,
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

    #[cfg(any(test, feature = "test-fixtures"))]
    pub async fn admit_spawned_background_child_at(
        &mut self,
        admission: crate::call::lifecycle::SpawnedBackgroundToolAdmission,
        now: DateTime<Utc>,
    ) -> Result<ToolCallLifecycle> {
        self.admit_spawned_background_child_with_time(admission, Some(now))
            .await
    }

    async fn admit_spawned_background_child_with_time(
        &mut self,
        admission: crate::call::lifecycle::SpawnedBackgroundToolAdmission,
        fixture_now: Option<DateTime<Utc>>,
    ) -> Result<ToolCallLifecycle> {
        anyhow::ensure!(
            matches!(
                self.state,
                ToolCallState::Running | ToolCallState::Completed
            ) && !self.is_spawned_background()
                && self.tool_name == gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME,
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

        let created = self.access()?.transact_idempotent(
            IdempotentTransactionRetry::Standard,
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
                    let existing = {
                        use gents_protocol::document_query::{
                            fields::agent_tool_call as fields, Filter, Query, Selection,
                        };
                        let requester = match requester_did.as_deref() {
                            Some(value) => Filter::eq(fields::REQUESTER_DID, value),
                            None => Filter::is_null(fields::REQUESTER_DID),
                        };
                        let filter = Filter::eq(fields::SPAWNED_BY_TOOL_CALL_DOC_ID, &parent_doc_id)
                            .and(Filter::eq(fields::REQUEST_DOC_ID, &request_doc_id))
                            .and(Filter::eq(fields::SESSION_ID, &session_id))
                            .and(Filter::eq(fields::NODE_DID, &node_did))
                            .and(requester);
                        let rendered = Query::select(
                            Selection::doc_id()
                                .field(fields::TOOL_CALL_KEY)
                                .field(fields::TOOL_CALL_ID)
                                .field(fields::TOOL_NAME)
                                .field(fields::MESSAGE_SEQUENCE)
                                .field(fields::LIFECYCLE_STATE)
                                .field(fields::AWAIT_MODE)
                                .field(fields::SPAWNED_BY_TOOL_CALL_DOC_ID)
                                .field(fields::DEADLINE_AT)
                                .field(fields::SELECTED_SERVICE_ID)
                                .field(fields::SELECTED_TOOL_NAME),
                        )
                        .filter(filter)
                        .limit(2)
                        .render();
                        txn.run(&rendered).await?
                    };
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
                    let accepted = ToolCallLifecycle::load_accepted_binding_in_txn(
                        txn, &parent_doc_id, &node_did, &session_id, requester_did.as_deref(), true,
                    ).await?;
                    anyhow::ensure!(
                        accepted.session_id == session_id
                            && accepted.request_doc_id == request_doc_id
                            && accepted.message_sequence == message_sequence
                            && accepted.execution_generation == generation
                            && accepted.accepted_header_doc_id == accepted_header_doc_id
                            && accepted.id == parent_tool_call_id
                            && accepted.tool_name == gents_protocol::tool_names::SPAWN_PROCESS_TOOL_NAME,
                        "spawned admission parent is not exactly bound by its accepted provider header"
                    );
                    let parent_row = {
                        use gents_protocol::document_query::{
                            fields::agent_tool_call as fields, Filter, Query, Selection,
                        };
                        let requester = match requester_did.as_deref() {
                            Some(value) => Filter::eq(fields::REQUESTER_DID, value),
                            None => Filter::is_null(fields::REQUESTER_DID),
                        };
                        let filter = Filter::eq(fields::DOC_ID, &parent_doc_id)
                            .and(Filter::eq(fields::REQUEST_DOC_ID, &request_doc_id))
                            .and(Filter::eq(fields::SESSION_ID, &session_id))
                            .and(Filter::eq(fields::NODE_DID, &node_did))
                            .and(Filter::eq(fields::TOOL_CALL_ID, &parent_tool_call_id))
                            .and(Filter::eq(fields::TOOL_NAME, "spawn_process"))
                            .and(Filter::eq(fields::MESSAGE_SEQUENCE, message_sequence))
                            .and(Filter::eq(fields::LIFECYCLE_STATE, "running"))
                            .and(requester);
                        let rendered = Query::select(
                            Selection::doc_id().field(fields::SPAWNED_BY_TOOL_CALL_DOC_ID),
                        )
                        .filter(filter)
                        .limit(2)
                        .render();
                        txn.run(&rendered).await?
                    };
                    anyhow::ensure!(parent_row["data"]["AgentToolCall"].as_array().is_some_and(|rows|
                        rows.len() == 1 && rows[0]["spawned_by_tool_call_doc_id"].is_null()),
                        "spawned admission lost its exact running accepted parent");
                    let request_row = &crate::call::hook::persistence::reserve_background_slot_in_txn(
                        txn, &request_doc_id, crate::call::hook::persistence::BackgroundSlot::Requested,
                        "node_did session_id request_id requester_did lifecycle_state execution_generation execution_lease_expires_at interrupt_requested_at",
                    ).await?;
                    anyhow::ensure!(
                        request_row["node_did"].as_str() == Some(node_did.as_str())
                            && request_row["session_id"].as_str() == Some(session_id.as_str())
                            && request_row["requester_did"].as_str() == requester_did.as_deref(),
                        "spawned admission parent request is out of scope"
                    );
                    let expires_at = request_row["execution_lease_expires_at"].as_str()
                        .context("spawned admission request lacks lease")?;
                    // A spawned process spends the live request lease (Lean
                    // `admitSpawnedBackgroundCore`), even for a script that outlives it.
                    if RequestLifecycleState::parse_opt(request_row["lifecycle_state"].as_str())
                        != Some(RequestLifecycleState::Processing)
                        || request_row["execution_generation"].as_str() != Some(generation.as_str())
                        || !request_row["interrupt_requested_at"].is_null()
                        || DateTime::parse_from_rfc3339(expires_at)?.with_timezone(&Utc) <= now
                    {
                        return Err(crate::call::hook::persistence::BackgroundSlotRefusal::RequestStopped.into());
                    }
                    let request_id = request_row["request_id"].as_str()
                        .filter(|id| !id.trim().is_empty()).context("spawned admission request lacks logical identity")?;
                    let (session_doc_id, selected_agent) = gents_store::config_client::session_children::agent_session_child_binding_in_txn(
                        txn, &node_did, &session_id, requester_did.as_deref()).await?;
                    let created = {
                        use gents_protocol::document_query::{
                            fields::agent_tool_call as fields, Create, Input,
                        };
                        let mut input = Input::new()
                            .set(fields::SESSION_DOC_ID, &session_doc_id)
                            .set(fields::AGENT_DID, &selected_agent)
                            .set(fields::CREATOR_DID, &selected_agent)
                            .set(fields::PARENT_LINKED, false)
                            .set(fields::TOOL_CALL_KEY, &stable_key)
                            .set(fields::REQUEST_ID, request_id)
                            .set(fields::REQUEST_DOC_ID, &request_doc_id)
                            .set(fields::SESSION_ID, &session_id)
                            .set(fields::NODE_DID, &node_did)
                            .set(fields::MESSAGE_SEQUENCE, message_sequence)
                            .set(fields::TOOL_NAME, &tool_name)
                            .set(fields::TOOL_CALL_ID, &stable_id)
                            .set(fields::LIFECYCLE_STATE, "pending")
                            .set(fields::STATUS, "pending")
                            .set(fields::DEADLINE_AT, deadline_at.to_rfc3339_opts(SecondsFormat::Nanos, true))
                            .set(fields::AWAIT_MODE, "background")
                            .set(fields::SPAWNED_BY_TOOL_CALL_DOC_ID, &parent_doc_id);
                        if let Some(did) = requester_did.as_deref().map(str::trim).filter(|did| !did.is_empty()) {
                            input = input.set(fields::REQUESTER_DID, did);
                        }
                        if let Some((service, tool)) = &selected {
                            input = input
                                .set(fields::SELECTED_SERVICE_ID, service)
                                .set(fields::SELECTED_TOOL_NAME, tool);
                        }
                        let rendered = Create::new(input).render();
                        txn.run(&rendered).await?
                    };
                    Ok(created_doc_id(&created, "AgentToolCall")?)
                })
            }
        ).await?;

        gents_store::config_client::session_children::publish_agent_session_child_relationships_with_access(
            &self.access()?,
            &self.node_did,
            &self.actor,
            &self.session_id,
        )
        .await?;

        let access = self.access()?;
        let node = self.node.clone();
        let node_did = &self.node_did;
        let session_id = &self.session_id;
        let requester_did = self.requester_did.as_deref();
        access
            .transact_readonly("tool_call.rehydrate_spawned_background", |txn| {
                let node = node.clone();
                let created = &created;
                Box::pin(async move {
                    ToolCallLifecycle::load_by_doc_id_in_txn(
                        txn,
                        node,
                        created,
                        node_did,
                        session_id,
                        requester_did,
                    )
                    .await
                })
            })
            .await?
            .context(
                "spawned admission created child could not be rehydrated by physical provenance",
            )
    }
}
