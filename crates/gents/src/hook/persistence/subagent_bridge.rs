use super::*;
use anyhow::Context;

/// A parked continuation is not authority to run again. Read the exact
/// request lease and the physical, accepted bridge before reacquiring a worker.
async fn revalidate_parked_subagent_owner(
    node: std::sync::Arc<defra_node::EmbeddedNode>,
    ticket: crate::agent::worker_capacity::WorkerTicket,
    dependency_doc_id: String,
    request_id: String,
    session_id: String,
    agent_did: String,
    requester_did: Option<String>,
    bridge_doc_id: String,
    bridge_request_doc_id: String,
    child_request_id: String,
    current_wait_doc_id: Option<String>,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        ticket.request_doc_id == bridge_request_doc_id || current_wait_doc_id.is_some(),
        "fresh foreground bridge crossed physical request identity"
    );
    anyhow::ensure!(
        dependency_doc_id == bridge_doc_id,
        "parked bridge document changed"
    );
    let physical = crate::graphql::escape_graphql_string(&ticket.request_doc_id);
    let query = format!(
        "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{physical}\" }} }}, limit: 2) {{ _docID request_id session_id agent_did requester_did lifecycle_state execution_generation execution_lease_expires_at interrupt_requested_at }} }}"
    );
    let response = node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "request lease read failed: {:?}",
        response.errors
    );
    let rows: Vec<gents_protocol::row::AgentRequestRow> =
        crate::graphql::rows(&response, "AgentRequest")?;
    anyhow::ensure!(
        rows.len() == 1,
        "parked request physical owner is missing or ambiguous"
    );
    let row = &rows[0];
    anyhow::ensure!(
        row.doc_id.as_deref() == Some(ticket.request_doc_id.as_str())
            && row.request_id == request_id
            && row.session_id.as_deref() == Some(session_id.as_str())
            && row.agent_did.as_deref() == Some(agent_did.as_str())
            && row.requester_did == requester_did,
        "parked request owner changed"
    );
    anyhow::ensure!(
        row.interrupt_requested_at.is_none(),
        "parked request has a pending interrupt"
    );
    let deadline = chrono::DateTime::parse_from_rfc3339(
        row.execution_lease_expires_at
            .as_deref()
            .context("missing lease deadline")?,
    )?;
    anyhow::ensure!(
        crate::lifecycle::execution_policy::authorize_producer_decision(
            crate::lifecycle::execution_policy::LeaseObservation {
                request: row.lifecycle_state.context("missing request lifecycle")?,
                generation: row
                    .execution_generation
                    .as_deref()
                    .context("missing generation")?,
                deadline_ms: deadline.timestamp_millis(),
            },
            &ticket.execution_generation,
            chrono::Utc::now().timestamp_millis(),
        ),
        "parked request lease is stale"
    );
    let bridge = ToolCallLifecycle::load_by_doc_id(
        node.clone(),
        &bridge_doc_id,
        &agent_did,
        &session_id,
        requester_did.as_deref(),
    )
    .await?
    .context("parked physical bridge disappeared")?;
    anyhow::ensure!(
        bridge.request_doc_id() == Some(bridge_request_doc_id.as_str())
            && bridge.child_request_id.as_deref() == Some(child_request_id.as_str()),
        "parked bridge changed its accepted physical binding"
    );
    // A running bridge can release its parent only after the bridge owner has
    // committed the immutable native background invocation reply. A mode flip
    // alone is not a handoff; the canonical query verifies the exact accepted
    // call, physical ToolDelivery header, and authored output provenance.
    let durable_background_handoff =
        if bridge.is_running() && bridge.await_mode() == AwaitMode::Background {
            crate::tool_call_lifecycle::query::load_tool_call_result(
                &crate::config_client::ConfigAccess::Local(node.clone()),
                &bridge_doc_id,
                &agent_did,
                &session_id,
                requester_did.as_deref(),
            )
            .await
            .context("running background bridge lacks exact canonical receipt")?;
            true
        } else {
            false
        };
    anyhow::ensure!(
        bridge.is_terminal() || durable_background_handoff,
        "parked bridge has neither terminal evidence nor a durable background handoff"
    );
    if let Some(control_doc_id) = current_wait_doc_id {
        let control = ToolCallLifecycle::load_by_doc_id(
            node.clone(),
            &control_doc_id,
            &agent_did,
            &session_id,
            requester_did.as_deref(),
        )
        .await?
        .context("current accepted wait control disappeared")?;
        anyhow::ensure!(
            control.request_doc_id() == Some(ticket.request_doc_id.as_str())
                && control.execution_generation() == Some(ticket.execution_generation.as_str())
                && control.tool_name() == WAIT_SUBAGENT_TOOL_NAME
                && control.is_running(),
            "current wait control lost accepted generation binding"
        );
        let descendant = crate::descendant_graph::resolve_session_descendant_edge(
            DescendantGraphAccess::Local(&node),
            &request_id,
            &child_request_id,
        )
        .await?
        .context("existing descendant is no longer authorized")?;
        anyhow::ensure!(
            descendant.controllable()
                && descendant.immediate_parent_tool_call_doc_id == bridge_doc_id
                && (!durable_background_handoff
                    || (descendant.lifecycle_state == "running"
                        && descendant.await_mode == "background")),
            "existing descendant control or exact bridge binding changed"
        );
    } else {
        anyhow::ensure!(
            bridge.execution_generation() == Some(ticket.execution_generation.as_str()),
            "fresh bridge lost accepted generation binding"
        );
    }
    Ok(())
}

fn exact_literal_presentation(text: &str) -> gents_protocol::output::PayloadPresentation {
    gents_protocol::output::PayloadPresentation::Composed {
        parts: vec![gents_protocol::output::PresentationPart::Literal {
            text: text.to_string(),
        }],
    }
}

impl DefraSessionHook {
    /// Commit the accepted bridge's immutable background invocation reply
    /// before a parked parent attempts to reacquire active worker capacity.
    /// A terminal race is observed again by the outer wait loop.
    async fn publish_foreground_background_handoff(
        &self,
        edge: &crate::background_tools::ChildEdge,
        payload: &str,
    ) -> anyhow::Result<bool> {
        let Some(mut bridge) = ToolCallLifecycle::load_by_doc_id(
            self.node.clone(),
            &edge.parent_tool_call_doc_id,
            &edge.parent_agent_did,
            &edge.parent_session_id,
            edge.parent_requester_did.as_deref(),
        )
        .await?
        else {
            anyhow::bail!("accepted background bridge disappeared before handoff");
        };
        anyhow::ensure!(
            bridge.request_doc_id() == Some(edge.parent_request_doc_id.as_str())
                && bridge.child_request_id.as_deref() == Some(edge.child_request_id.as_str()),
            "background handoff bridge lost exact child binding"
        );
        if !bridge.is_running() || bridge.await_mode() != AwaitMode::Background {
            return Ok(false);
        }
        // A bridge spawned in background mode may already have its one
        // immutable receipt. A later foreground wait can be backgrounded
        // again with a richer presentation, but must never rewrite that
        // original invocation reply. This read distinguishes no delivery
        // from malformed or conflicting delivery through the canonical owner.
        let existing = crate::tool_call_lifecycle::query::load_tool_call_presentation(
            &crate::config_client::ConfigAccess::Local(self.node.clone()),
            &edge.parent_tool_call_doc_id,
            &edge.parent_agent_did,
            &edge.parent_session_id,
            edge.parent_requester_did.as_deref(),
        )
        .await?;
        if existing.result.is_some() {
            return Ok(true);
        }
        bridge.publish_background_receipt(payload).await?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn resume_foreground_worker(
        &self,
        request_id: &str,
        session_id: &str,
        request_doc_id: &str,
        generation: &str,
        bridge_doc_id: &str,
        bridge_request_doc_id: &str,
        child_request_id: &str,
        requester_did: Option<&str>,
        current_wait_doc_id: Option<&str>,
    ) -> anyhow::Result<bool> {
        let cancellation = crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
            .map(|context| context.cancellation_token)
            .unwrap_or_default();
        let node = self.node.clone();
        let request_id = request_id.to_owned();
        let session_id = session_id.to_owned();
        let request_doc_id = request_doc_id.to_owned();
        let generation = generation.to_owned();
        let bridge_doc_id = bridge_doc_id.to_owned();
        let bridge_request_doc_id = bridge_request_doc_id.to_owned();
        let child_request_id = child_request_id.to_owned();
        let agent_did = self.agent_did.clone();
        let requester_did = requester_did.map(str::to_owned);
        let current_wait_doc_id = current_wait_doc_id.map(str::to_owned);
        crate::agent::worker_capacity::resume_current(
            &cancellation,
            move |ticket, dependency| async move {
                anyhow::ensure!(
                    ticket.request_doc_id == request_doc_id
                        && ticket.execution_generation == generation,
                    "parked continuation changed request or generation"
                );
                revalidate_parked_subagent_owner(
                    node,
                    ticket,
                    dependency,
                    request_id,
                    session_id,
                    agent_did,
                    requester_did,
                    bridge_doc_id,
                    bridge_request_doc_id,
                    child_request_id,
                    current_wait_doc_id,
                )
                .await
            },
        )
        .await
        .map_err(anyhow::Error::from)
    }

    pub(super) async fn cancel_live_subagent_descendants(
        &self,
        child_session_id: &str,
        child_agent_did: &str,
        child_requester_did: Option<&str>,
        cause: CancelCause,
    ) -> anyhow::Result<usize> {
        crate::background_tools::subagent_control::cancel_live_subagent_descendants(
            self.node.clone(),
            child_session_id,
            child_agent_did,
            child_requester_did,
            &self.agent_did,
            cause,
        )
        .await
    }

    pub(super) async fn await_foreground_subagent(
        &self,
        internal_call_id: &str,
        parent_context: &ParentSubagentContext,
        child_request_id: &str,
        child_session_id: &str,
        behavior_id: &str,
        parent_deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<String> {
        let mut missing_owner_since = None;
        let mut child_session_id = child_session_id.to_string();

        loop {
            let now = chrono::Utc::now();
            let Some(edge) =
                try_load_authorized_child_edge(&self.node, parent_context, child_request_id)
                    .await?
            else {
                if now >= parent_deadline_at {
                    let payload = foreground_terminal_failure_payload(
                        child_request_id,
                        &child_session_id,
                        "dead",
                        "parent request deadline exceeded while waiting for child subagent",
                        FailureClass::External,
                    );
                    // Terminalize the bridge as timedOut (parent deadline
                    // exceeded before the child was ever materialized),
                    // mirroring the running-edge deadline path so the bridge
                    // does not leak in a `running` state. No child terminal
                    // evidence exists, so `bridge_failure` is not licensed
                    // here (#1002) — the deadline transition is.
                    if let Some(mut lifecycle) =
                        self.take_owned_in_flight_lifecycle(internal_call_id).await
                    {
                        if !lifecycle
                            .timeout_with_presentation(
                                &payload,
                                exact_literal_presentation(&payload),
                            )
                            .await?
                        {
                            return self
                                .foreground_external_bridge_terminal_payload(
                                    parent_context,
                                    internal_call_id,
                                    child_request_id,
                                    &child_session_id,
                                    behavior_id,
                                )
                                .await;
                        }
                    }
                    return Ok(payload);
                }
                let remaining = (parent_deadline_at - now)
                    .to_std()
                    .unwrap_or(Duration::from_millis(0));
                tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
                continue;
            };
            if child_session_id.is_empty() {
                child_session_id = edge.child_session_id.clone();
            }
            let child_session_id = child_session_id.as_str();

            if matches!(
                edge.lifecycle_state.as_str(),
                "cancelled" | "failed" | "timedOut"
            ) {
                self.discard_in_flight_lifecycle(internal_call_id).await;
                return self.foreground_durable_bridge_payload(&edge).await;
            }

            if edge.lifecycle_state == "completed" {
                self.discard_in_flight_lifecycle(internal_call_id).await;
                return self
                    .foreground_completed_bridge_payload(
                        &edge,
                        child_request_id,
                        child_session_id,
                        behavior_id,
                    )
                    .await;
            }

            if edge.await_mode == AwaitMode::Background && edge.lifecycle_state == "running" {
                let receipt =
                    backgrounded_receipt_payload(child_request_id, child_session_id, behavior_id);
                if !self
                    .publish_foreground_background_handoff(&edge, &receipt)
                    .await?
                {
                    continue;
                }
                self.refresh_owned_in_flight_lifecycle_from_storage(
                    &parent_context.session_id,
                    internal_call_id,
                )
                .await?;
                return Ok(receipt);
            }

            if now >= parent_deadline_at {
                let payload = foreground_terminal_failure_payload(
                    child_request_id,
                    child_session_id,
                    "dead",
                    "parent request deadline exceeded while waiting for child subagent",
                    FailureClass::External,
                );
                if edge.lifecycle_state == "running" {
                    let Some(mut lifecycle) =
                        self.take_owned_in_flight_lifecycle(internal_call_id).await
                    else {
                        wait_for_external_lifecycle_owner(
                            &mut missing_owner_since,
                            now,
                            internal_call_id,
                        )
                        .await?;
                        continue;
                    };
                    // The child may still be live: take the licensed deadline
                    // transition (`timedOut`), never a fabricated
                    // `ChildTerminal::Dead` (#1002). The child's own
                    // terminalization belongs to the subagent-liveness sweep.
                    if !lifecycle
                        .timeout_with_presentation(&payload, exact_literal_presentation(&payload))
                        .await?
                    {
                        return self
                            .foreground_external_bridge_terminal_payload(
                                parent_context,
                                internal_call_id,
                                child_request_id,
                                child_session_id,
                                behavior_id,
                            )
                            .await;
                    }
                }
                return Ok(payload);
            }

            if let Some(row) = load_child_terminal_row(&self.node, child_request_id).await? {
                if child_request_completed(&row) {
                    let Some(final_response) = load_child_final_response(&self.node, &edge).await?
                    else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    let payload = json_envelope_with_bounded_result(
                        json!({
                            "ok": true,
                            "child_request_id": child_request_id,
                            "child_session_id": child_session_id,
                            "behavior_id": behavior_id,
                            "await_mode": "foreground",
                            "status": "completed",
                            "final_response": serde_json::Value::Null,
                            "error": null
                        }),
                        "final_response",
                        &final_response,
                        SPAWN_SUBAGENT_TOOL_NAME,
                        &self.truncation_limits,
                    );
                    if edge.lifecycle_state == "running" {
                        let Some(mut lifecycle) =
                            self.take_owned_in_flight_lifecycle(internal_call_id).await
                        else {
                            wait_for_external_lifecycle_owner(
                                &mut missing_owner_since,
                                now,
                                internal_call_id,
                            )
                            .await?;
                            continue;
                        };
                        if !lifecycle
                            .bridge_complete_with_presentation(
                                final_response.clone(),
                                &payload,
                                exact_literal_presentation(&payload),
                            )
                            .await?
                        {
                            return self
                                .foreground_external_bridge_terminal_payload(
                                    parent_context,
                                    internal_call_id,
                                    child_request_id,
                                    child_session_id,
                                    behavior_id,
                                )
                                .await;
                        }
                    } else {
                        self.discard_in_flight_lifecycle(internal_call_id).await;
                    }
                    return Ok(payload);
                }

                if let Some(terminal) = project_child_terminal(&row) {
                    let status = child_terminal_status(&terminal);
                    let (reason, failure_class) = child_terminal_reason(&terminal);
                    let payload = foreground_terminal_failure_payload(
                        child_request_id,
                        child_session_id,
                        status,
                        &reason,
                        failure_class,
                    );
                    if edge.lifecycle_state == "running" {
                        let Some(mut lifecycle) =
                            self.take_owned_in_flight_lifecycle(internal_call_id).await
                        else {
                            wait_for_external_lifecycle_owner(
                                &mut missing_owner_since,
                                now,
                                internal_call_id,
                            )
                            .await?;
                            continue;
                        };
                        if !lifecycle
                            .bridge_failure_with_presentation(
                                terminal,
                                &payload,
                                exact_literal_presentation(&payload),
                            )
                            .await?
                        {
                            return self
                                .foreground_external_bridge_terminal_payload(
                                    parent_context,
                                    internal_call_id,
                                    child_request_id,
                                    child_session_id,
                                    behavior_id,
                                )
                                .await;
                        }
                    } else {
                        self.discard_in_flight_lifecycle(internal_call_id).await;
                    }
                    return Ok(payload);
                }
            }

            let remaining = (parent_deadline_at - now)
                .to_std()
                .unwrap_or(Duration::from_millis(0));
            tokio::time::sleep(remaining.min(Duration::from_millis(250))).await;
        }
    }

    pub(super) async fn await_existing_subagent_bridge(
        &self,
        parent_context: &ParentSubagentContext,
        caller_request_doc_id: &str,
        parent_tool_call_id: &str,
        child_request_id: &str,
        child_session_id: &str,
        behavior_id: &str,
        parent_deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<String> {
        loop {
            let now = chrono::Utc::now();
            let edge =
                load_authorized_child_edge(&self.node, parent_context, child_request_id).await?;

            if matches!(
                edge.lifecycle_state.as_str(),
                "cancelled" | "failed" | "timedOut"
            ) {
                self.discard_in_flight_lifecycle(parent_tool_call_id).await;
                return self.foreground_durable_bridge_payload(&edge).await;
            }

            if edge.lifecycle_state == "completed" {
                self.discard_in_flight_lifecycle(parent_tool_call_id).await;
                return self
                    .foreground_completed_bridge_payload(
                        &edge,
                        child_request_id,
                        child_session_id,
                        behavior_id,
                    )
                    .await;
            }

            if edge.lifecycle_state == "running"
                && crate::interrupt::fetch_interrupt_requested_at_by_doc_id(
                    &self.node,
                    caller_request_doc_id,
                )
                .await?
                .is_some()
            {
                let payload = foreground_terminal_failure_payload(
                    child_request_id,
                    child_session_id,
                    "interrupted",
                    "parent request was cancelled while waiting for child subagent",
                    FailureClass::External,
                );
                if let Some(mut lifecycle) = self
                    .take_or_load_in_flight_lifecycle(
                        &parent_context.session_id,
                        parent_tool_call_id,
                    )
                    .await?
                {
                    let dispatch = match lifecycle
                        .cancel_during_run_with_cascade_dispatch_and_presentation(
                            CancelCause::Interrupted,
                            &self.agent_did,
                            Some((&payload, exact_literal_presentation(&payload))),
                        )
                        .await
                    {
                        Ok(dispatch) => dispatch,
                        Err(error) => {
                            return self
                                .foreground_external_bridge_terminal_or_error(
                                    parent_context,
                                    parent_tool_call_id,
                                    child_request_id,
                                    child_session_id,
                                    behavior_id,
                                    error,
                                )
                                .await;
                        }
                    };
                    if !lifecycle.is_cancelled() {
                        return self
                            .foreground_external_bridge_terminal_payload(
                                parent_context,
                                parent_tool_call_id,
                                child_request_id,
                                child_session_id,
                                behavior_id,
                            )
                            .await;
                    }
                    if let Some(dispatch) = dispatch {
                        if let CascadeDispatch::Local { intent, child } = dispatch {
                            if let Err(error) = crate::interrupt::interrupt_request_by_doc_id(
                                &self.node,
                                child
                                    .doc_id
                                    .as_deref()
                                    .expect("verified physical cascade child"),
                                child
                                    .agent_did
                                    .as_deref()
                                    .expect("verified local child principal"),
                                child.requester_did.as_deref(),
                            )
                            .await
                            {
                                tracing::warn!(
                                    child_request_id = %intent.child_request_id,
                                    error = %error,
                                    "failed to cascade wait_subagent cancellation to child request"
                                );
                            }
                        }
                    }
                }
                self.discard_in_flight_lifecycle(parent_tool_call_id).await;
                return Ok(payload);
            }

            if edge.await_mode == AwaitMode::Background && edge.lifecycle_state == "running" {
                let receipt =
                    backgrounded_receipt_payload(child_request_id, child_session_id, behavior_id);
                if !self
                    .publish_foreground_background_handoff(&edge, &receipt)
                    .await?
                {
                    continue;
                }
                self.refresh_owned_in_flight_lifecycle_from_storage(
                    &parent_context.session_id,
                    parent_tool_call_id,
                )
                .await?;
                return Ok(receipt);
            }

            if now >= parent_deadline_at {
                let payload = foreground_terminal_failure_payload(
                    child_request_id,
                    child_session_id,
                    "dead",
                    "parent request deadline exceeded while waiting for child subagent",
                    FailureClass::External,
                );
                if edge.lifecycle_state == "running" {
                    if let Some(mut lifecycle) = self
                        .take_or_load_in_flight_lifecycle(
                            &parent_context.session_id,
                            parent_tool_call_id,
                        )
                        .await?
                    {
                        // Licensed deadline transition — no fabricated child
                        // terminal evidence (#1002); see
                        // `await_foreground_subagent`'s deadline arm.
                        let projected = match lifecycle
                            .timeout_with_presentation(
                                &payload,
                                exact_literal_presentation(&payload),
                            )
                            .await
                        {
                            Ok(projected) => projected,
                            Err(error) => {
                                return self
                                    .foreground_external_bridge_terminal_or_error(
                                        parent_context,
                                        parent_tool_call_id,
                                        child_request_id,
                                        child_session_id,
                                        behavior_id,
                                        error,
                                    )
                                    .await;
                            }
                        };
                        if !projected {
                            return self
                                .foreground_external_bridge_terminal_payload(
                                    parent_context,
                                    parent_tool_call_id,
                                    child_request_id,
                                    child_session_id,
                                    behavior_id,
                                )
                                .await;
                        }
                    }
                }
                self.discard_in_flight_lifecycle(parent_tool_call_id).await;
                return Ok(payload);
            }

            if let Some(row) = load_child_terminal_row(&self.node, child_request_id).await? {
                if child_request_completed(&row) {
                    let Some(final_response) = load_child_final_response(&self.node, &edge).await?
                    else {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    };
                    let payload = json_envelope_with_bounded_result(
                        json!({
                            "ok": true,
                            "child_request_id": child_request_id,
                            "child_session_id": child_session_id,
                            "behavior_id": behavior_id,
                            "await_mode": "foreground",
                            "status": "completed",
                            "final_response": serde_json::Value::Null,
                            "error": null
                        }),
                        "final_response",
                        &final_response,
                        SPAWN_SUBAGENT_TOOL_NAME,
                        &self.truncation_limits,
                    );
                    if edge.lifecycle_state == "running" {
                        if let Some(mut lifecycle) = self
                            .take_or_load_in_flight_lifecycle(
                                &parent_context.session_id,
                                parent_tool_call_id,
                            )
                            .await?
                        {
                            let projected = match lifecycle
                                .bridge_complete_with_presentation(
                                    final_response.clone(),
                                    &payload,
                                    exact_literal_presentation(&payload),
                                )
                                .await
                            {
                                Ok(projected) => projected,
                                Err(error) => {
                                    return self
                                        .foreground_external_bridge_terminal_or_error(
                                            parent_context,
                                            parent_tool_call_id,
                                            child_request_id,
                                            child_session_id,
                                            behavior_id,
                                            error,
                                        )
                                        .await;
                                }
                            };
                            if !projected {
                                return self
                                    .foreground_external_bridge_terminal_payload(
                                        parent_context,
                                        parent_tool_call_id,
                                        child_request_id,
                                        child_session_id,
                                        behavior_id,
                                    )
                                    .await;
                            }
                        }
                    }
                    self.discard_in_flight_lifecycle(parent_tool_call_id).await;
                    return Ok(payload);
                }

                if let Some(terminal) = project_child_terminal(&row) {
                    let status = child_terminal_status(&terminal);
                    let (reason, failure_class) = child_terminal_reason(&terminal);
                    let payload = foreground_terminal_failure_payload(
                        child_request_id,
                        child_session_id,
                        status,
                        &reason,
                        failure_class,
                    );
                    if edge.lifecycle_state == "running" {
                        if let Some(mut lifecycle) = self
                            .take_or_load_in_flight_lifecycle(
                                &parent_context.session_id,
                                parent_tool_call_id,
                            )
                            .await?
                        {
                            let projected = match lifecycle
                                .bridge_failure_with_presentation(
                                    terminal,
                                    &payload,
                                    exact_literal_presentation(&payload),
                                )
                                .await
                            {
                                Ok(projected) => projected,
                                Err(error) => {
                                    return self
                                        .foreground_external_bridge_terminal_or_error(
                                            parent_context,
                                            parent_tool_call_id,
                                            child_request_id,
                                            child_session_id,
                                            behavior_id,
                                            error,
                                        )
                                        .await;
                                }
                            };
                            if !projected {
                                return self
                                    .foreground_external_bridge_terminal_payload(
                                        parent_context,
                                        parent_tool_call_id,
                                        child_request_id,
                                        child_session_id,
                                        behavior_id,
                                    )
                                    .await;
                            }
                        }
                    }
                    self.discard_in_flight_lifecycle(parent_tool_call_id).await;
                    return Ok(payload);
                }
            }

            let remaining = (parent_deadline_at - now)
                .to_std()
                .unwrap_or(Duration::from_millis(0));
            tokio::time::sleep(remaining.min(Duration::from_millis(250))).await;
        }
    }

    pub(super) async fn foreground_external_bridge_terminal_payload(
        &self,
        parent_context: &ParentSubagentContext,
        internal_call_id: &str,
        child_request_id: &str,
        child_session_id: &str,
        behavior_id: &str,
    ) -> anyhow::Result<String> {
        self.discard_in_flight_lifecycle(internal_call_id).await;
        let edge = load_authorized_child_edge(&self.node, parent_context, child_request_id).await?;

        if !matches!(
            edge.lifecycle_state.as_str(),
            "completed" | "cancelled" | "timedOut" | "failed"
        ) {
            anyhow::bail!(
                "spawn_subagent foreground bridge lost running compare but persisted lifecycle_state is {}",
                edge.lifecycle_state
            );
        }
        self.foreground_durable_bridge_payload(&edge).await
    }

    pub(super) async fn foreground_completed_bridge_payload(
        &self,
        edge: &crate::background_tools::ChildEdge,
        _child_request_id: &str,
        _child_session_id: &str,
        _behavior_id: &str,
    ) -> anyhow::Result<String> {
        self.foreground_durable_bridge_payload(edge).await
    }

    async fn foreground_durable_bridge_payload(
        &self,
        edge: &crate::background_tools::ChildEdge,
    ) -> anyhow::Result<String> {
        let message = load_tool_call_result(
            &crate::config_client::ConfigAccess::Local(self.node.clone()),
            &edge.parent_tool_call_doc_id,
            &edge.parent_agent_did,
            &edge.parent_session_id,
            edge.parent_requester_did.as_deref(),
        )
        .await?;
        crate::tool_call_lifecycle::query::render_tool_result(&message)
    }

    pub(super) async fn take_owned_in_flight_lifecycle(
        &self,
        internal_call_id: &str,
    ) -> Option<ToolCallLifecycle> {
        self.in_flight_lifecycles
            .lock()
            .await
            .remove(internal_call_id)
    }

    pub(super) async fn take_or_load_in_flight_lifecycle(
        &self,
        session_id: &str,
        internal_call_id: &str,
    ) -> anyhow::Result<Option<ToolCallLifecycle>> {
        if let Some(lifecycle) = self.take_owned_in_flight_lifecycle(internal_call_id).await {
            return Ok(Some(lifecycle));
        }

        ToolCallLifecycle::load(self.node.clone(), session_id, internal_call_id).await
    }

    pub(super) async fn discard_in_flight_lifecycle(&self, internal_call_id: &str) {
        self.in_flight_lifecycles
            .lock()
            .await
            .remove(internal_call_id);
    }

    pub(super) async fn foreground_external_bridge_terminal_or_error(
        &self,
        parent_context: &ParentSubagentContext,
        parent_tool_call_id: &str,
        child_request_id: &str,
        child_session_id: &str,
        behavior_id: &str,
        error: anyhow::Error,
    ) -> anyhow::Result<String> {
        let edge = load_authorized_child_edge(&self.node, parent_context, child_request_id).await?;
        if edge.lifecycle_state == "running" {
            return Err(error);
        }

        self.foreground_external_bridge_terminal_payload(
            parent_context,
            parent_tool_call_id,
            child_request_id,
            child_session_id,
            behavior_id,
        )
        .await
    }

    pub(super) async fn foreground_and_track_existing_subagent_bridge(
        &self,
        parent_context: &ParentSubagentContext,
        child_request_id: &str,
        parent_tool_call_id: &str,
    ) -> anyhow::Result<()> {
        let Some(mut lifecycle) = ToolCallLifecycle::load(
            self.node.clone(),
            &parent_context.session_id,
            parent_tool_call_id,
        )
        .await?
        else {
            return Ok(());
        };

        if let Err(error) = lifecycle.foreground().await {
            let refreshed =
                load_authorized_child_edge(&self.node, parent_context, child_request_id).await?;
            if refreshed.lifecycle_state == "running"
                && refreshed.await_mode == AwaitMode::Background
            {
                return Err(error);
            }

            tracing::debug!(
                tool_call_id = %parent_tool_call_id,
                child_request_id = %child_request_id,
                error = %error,
                lifecycle_state = %refreshed.lifecycle_state,
                await_mode = ?refreshed.await_mode,
                "wait_subagent foreground race resolved by refreshed bridge state"
            );
        }

        self.track_in_flight_lifecycle_from_storage(&parent_context.session_id, parent_tool_call_id)
            .await
    }

    pub(super) async fn track_in_flight_lifecycle_from_storage(
        &self,
        session_id: &str,
        internal_call_id: &str,
    ) -> anyhow::Result<()> {
        if let Some(lifecycle) =
            ToolCallLifecycle::load(self.node.clone(), session_id, internal_call_id).await?
        {
            if lifecycle.is_running() {
                self.in_flight_lifecycles
                    .lock()
                    .await
                    .insert(internal_call_id.to_string(), lifecycle);
            }
        }
        Ok(())
    }

    pub(super) async fn refresh_owned_in_flight_lifecycle_from_storage(
        &self,
        session_id: &str,
        internal_call_id: &str,
    ) -> anyhow::Result<()> {
        if !self
            .in_flight_lifecycles
            .lock()
            .await
            .contains_key(internal_call_id)
        {
            return Ok(());
        }

        if let Some(lifecycle) =
            ToolCallLifecycle::load(self.node.clone(), session_id, internal_call_id).await?
        {
            let mut map = self.in_flight_lifecycles.lock().await;
            if map.contains_key(internal_call_id) {
                if lifecycle.is_running() {
                    map.insert(internal_call_id.to_string(), lifecycle);
                } else {
                    map.remove(internal_call_id);
                }
            }
        }
        Ok(())
    }

    pub(super) async fn load_authorized_background_tool(
        &self,
        caller: &ProcessControlScope,
        tool_call_id: &str,
    ) -> anyhow::Result<ToolCallLifecycle> {
        let Some(lifecycle) =
            ToolCallLifecycle::load(self.node.clone(), &caller.session_id, tool_call_id).await?
        else {
            anyhow::bail!("background tool call {tool_call_id} was not found");
        };
        if !caller.authorizes(
            lifecycle.session_id(),
            lifecycle.agent_did(),
            lifecycle.requester_did(),
        ) || lifecycle.await_mode() != AwaitMode::Background
            || lifecycle.is_subagent_bridge()
        {
            anyhow::bail!(
                "background tool call {tool_call_id} is not manageable by this session principal"
            );
        }
        Ok(lifecycle)
    }

    pub(super) async fn await_background_tool(
        &self,
        caller: &ProcessControlScope,
        tool_call_id: &str,
        caller_deadline_at: chrono::DateTime<chrono::Utc>,
        wait_deadline_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<String> {
        loop {
            let now = chrono::Utc::now();
            let lifecycle = self
                .load_authorized_background_tool(caller, tool_call_id)
                .await?;
            if lifecycle.is_terminal() {
                return self.background_tool_envelope(lifecycle, "terminal").await;
            }

            // Waiting is observational. Ending or interrupting this caller's
            // turn must not revoke the separately budgeted background job.
            if crate::interrupt::fetch_interrupt_requested_at_scoped(
                &self.node,
                &caller.request_id,
                &caller.agent_did,
                caller.requester_did.as_deref(),
            )
            .await?
            .is_some()
            {
                return self
                    .background_tool_envelope(lifecycle, "caller_interrupted")
                    .await;
            }

            if now >= caller_deadline_at {
                return self
                    .background_tool_envelope(lifecycle, "caller_deadline_exceeded")
                    .await;
            }

            // Bounded wait (#985): report the process as still running — do
            // NOT cancel it; the run continues and completion is delivered
            // via the background completion notification.
            if now >= wait_deadline_at {
                return self
                    .background_tool_envelope(lifecycle, "wait_timeout")
                    .await;
            }

            let remaining = (caller_deadline_at.min(wait_deadline_at) - now)
                .to_std()
                .unwrap_or(Duration::from_millis(0));
            tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
        }
    }

    pub(super) async fn cancel_background_tool_lifecycle(
        &self,
        mut lifecycle: ToolCallLifecycle,
        cause: CancelCause,
        completion_reason: &str,
    ) -> anyhow::Result<(ToolCallLifecycle, bool)> {
        let won_terminal_compare = if lifecycle.is_running() {
            lifecycle
                .cancel_during_run_owned(cause, completion_reason)
                .await?
        } else {
            false
        };
        // Persist the explicit cancellation before waking the worker. If the
        // token fires first, the worker can win the same running-state compare
        // and replace the user's specific cause with generic `interrupted`.
        if won_terminal_compare {
            self.background_executions
                .cancel(lifecycle.tool_call_id())
                .await;
        }
        Ok((lifecycle, won_terminal_compare))
    }

    pub(super) async fn background_tool_envelope(
        &self,
        lifecycle: ToolCallLifecycle,
        reason: &str,
    ) -> anyhow::Result<String> {
        let tool_doc_id = lifecycle
            .doc_id()
            .context("background result requires physical tool identity")?;
        let result = if lifecycle.is_spawned_background() {
            // A spawned process owns an exact canonical ToolOutput source but,
            // deliberately, no fabricated provider ToolCall block or direct
            // invocation reply. Read its source through the output owner.
            crate::background_tools::canonical_tool_output(
                self.node.as_ref(),
                tool_doc_id,
                lifecycle
                    .request_doc_id()
                    .context("spawned background result requires request identity")?,
                lifecycle.session_id(),
                lifecycle.agent_did(),
                lifecycle.requester_did(),
            )
            .await?
        } else {
            let message = load_tool_call_result(
                &crate::config_client::ConfigAccess::Local(self.node.clone()),
                tool_doc_id,
                lifecycle.agent_did(),
                lifecycle.session_id(),
                lifecycle.requester_did(),
            )
            .await?;
            crate::tool_call_lifecycle::query::render_tool_result(&message)?
        };
        let status = lifecycle.state().as_str();
        let error = if lifecycle.state() == crate::tool_call_lifecycle::ToolCallState::Completed {
            serde_json::Value::Null
        } else {
            json!({
                "reason": reason,
                "failure_class": "external"
            })
        };
        Ok(json_envelope_with_bounded_result(
            json!({
                "ok": lifecycle.state() == crate::tool_call_lifecycle::ToolCallState::Completed,
                "tool_call_id": lifecycle.tool_call_id(),
                "tool_name": lifecycle.tool_name(),
                "await_mode": "background",
                "status": status,
                "result": serde_json::Value::Null,
                "error": error
            }),
            "result",
            &result,
            lifecycle.tool_name(),
            &self.truncation_limits,
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn fail_spawn_subagent_tool_call(
        &self,
        session_id: String,
        request_id: String,
        deadline_at: chrono::DateTime<chrono::Utc>,
        _message_sequence: u32,
        internal_call_id: &str,
        args: &str,
        failure_class: FailureClass,
        result: String,
    ) -> anyhow::Result<ToolCallHookAction> {
        // Rejection still settles the provider-published invocation. Preserve
        // its immutable mode instead of trying to redispatch a background
        // admission as foreground merely because validation failed.
        let await_mode = self
            .accepted_tool_calls
            .lock()
            .await
            .get(internal_call_id)
            .and_then(|call| call.spawn_admission.as_ref())
            .map(|plan| plan.await_mode)
            .unwrap_or(AwaitMode::Foreground);
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                None,
                &request_id,
                &session_id,
                SPAWN_SUBAGENT_TOOL_NAME,
                args,
                deadline_at,
                await_mode,
                crate::tool_call_lifecycle::CancelPolicy::Cascade,
            )
            .await?;
        lifecycle.spawn_failed(failure_class, &result).await?;
        Ok(self.skip_tool_result(SPAWN_SUBAGENT_TOOL_NAME, result))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn fail_background_meta_tool_call(
        &self,
        session_id: String,
        request_id: String,
        deadline_at: chrono::DateTime<chrono::Utc>,
        _message_sequence: u32,
        internal_call_id: &str,
        tool_name: &str,
        args: &str,
        failure_class: FailureClass,
        result: String,
    ) -> anyhow::Result<ToolCallHookAction> {
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                None,
                &request_id,
                &session_id,
                tool_name,
                args,
                deadline_at,
                AwaitMode::Foreground,
                crate::tool_call_lifecycle::CancelPolicy::Cascade,
            )
            .await?;
        lifecycle.spawn_failed(failure_class, &result).await?;
        Ok(self.skip_tool_result(tool_name, result))
    }
}

#[cfg(test)]
mod worker_resume_tests {
    use super::*;
    use crate::agent::worker_capacity::WorkerTicket;
    use crate::tool_call_lifecycle::admission_fixture::{
        claimed_signed_request, complete_child, publish_accepted_on_claimed_request,
        published_admission_with_owner, PublishedAdmissionOptions,
    };

    fn modeled(name: &str, succeeds: bool) {
        let case = crate::lean_vocab_test::lean_canonical_worker_capacity_cases()
            .iter()
            .find(|case| case.name == name)
            .unwrap_or_else(|| panic!("missing modeled worker case {name}"));
        assert_eq!(case.expected.is_some(), succeeds, "{name}");
    }

    #[tokio::test]
    async fn accepted_foreground_bridge_resume_rechecks_lease_and_interrupt() {
        modeled("parent_resume_after_child", true);
        modeled("stale_generation_refused", false);
        modeled("cancelled_parent_refused", false);
        let child_request_id = "worker-resume-child";
        let (mut admission, owner) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "worker-resume-bridge".into(),
            real_identity: true,
            spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
                tool_call_id: "worker-resume-spawn".into(),
                child_request_id: child_request_id.into(),
                spawn_target_did: "fixture-overrides-with-owner".into(),
                spawn_behavior_id: "general".into(),
                delegated_workspace: None,
                await_mode: AwaitMode::Foreground,
            }),
            ..Default::default()
        })
        .await
        .unwrap();
        let request_doc = admission.tool.request_doc_id().unwrap().to_owned();
        let request_id = owner.request().request_id.clone();
        let session_id = admission.tool.session_id().to_owned();
        let bridge_doc = admission.tool.doc_id().unwrap().to_owned();
        let generation = admission.tool.execution_generation().unwrap().to_owned();
        let requester = admission.tool.requester_did().map(str::to_owned);
        assert!(admission
            .tool
            .bridge_complete("child completed".into())
            .await
            .unwrap());

        let check = |ticket: WorkerTicket| {
            revalidate_parked_subagent_owner(
                admission.node.clone(),
                ticket,
                bridge_doc.clone(),
                request_id.clone(),
                session_id.clone(),
                admission.agent_did.clone(),
                requester.clone(),
                bridge_doc.clone(),
                request_doc.clone(),
                child_request_id.into(),
                None,
            )
        };
        check(WorkerTicket::new(&request_doc, &generation))
            .await
            .expect("accepted completed bridge and live exact lease resume");
        assert!(check(WorkerTicket::new(&request_doc, "stale-generation"))
            .await
            .is_err());
        crate::interrupt::interrupt_request_by_doc_id(
            &admission.node,
            &request_doc,
            &admission.agent_did,
            requester.as_deref(),
        )
        .await
        .unwrap();
        assert!(check(WorkerTicket::new(&request_doc, &generation))
            .await
            .is_err());

        admission.node.shutdown().await;
        std::fs::remove_dir_all(&admission.path).unwrap();
    }

    #[tokio::test]
    async fn accepted_running_background_handoff_requires_durable_receipt() {
        modeled("background_mode_without_receipt_refused", false);
        modeled("background_handoff_receipt_resumes", true);
        let child_request_id = "worker-handoff-child";
        let (mut admission, owner) = published_admission_with_owner(PublishedAdmissionOptions {
            name: "worker-background-handoff".into(),
            real_identity: true,
            spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
                tool_call_id: "worker-handoff-spawn".into(),
                child_request_id: child_request_id.into(),
                spawn_target_did: "fixture-overrides-with-owner".into(),
                spawn_behavior_id: "general".into(),
                delegated_workspace: None,
                await_mode: AwaitMode::Foreground,
            }),
            ..Default::default()
        })
        .await
        .unwrap();
        let request_doc = admission.tool.request_doc_id().unwrap().to_owned();
        let request_id = owner.request().request_id.clone();
        let session_id = admission.tool.session_id().to_owned();
        let bridge_doc = admission.tool.doc_id().unwrap().to_owned();
        let generation = admission.tool.execution_generation().unwrap().to_owned();
        let requester = admission.tool.requester_did().map(str::to_owned);
        let node = admission.node.clone();
        let did = admission.agent_did.clone();
        let check = || {
            revalidate_parked_subagent_owner(
                node.clone(),
                WorkerTicket::new(&request_doc, &generation),
                bridge_doc.clone(),
                request_id.clone(),
                session_id.clone(),
                did.clone(),
                requester.clone(),
                bridge_doc.clone(),
                request_doc.clone(),
                child_request_id.into(),
                None,
            )
        };
        admission.tool.background().await.unwrap();
        assert!(
            check().await.is_err(),
            "mode-only background bridge cannot resume a parked parent"
        );
        assert!(admission
            .tool
            .publish_background_receipt("durable handoff")
            .await
            .unwrap());
        check()
            .await
            .expect("exact immutable background receipt permits owner-bound resume");

        admission.node.shutdown().await;
        std::fs::remove_dir_all(&admission.path).unwrap();
    }

    #[tokio::test]
    async fn later_request_wait_control_can_resume_prior_request_bridge() {
        modeled("existing_parent_resume", true);
        modeled("existing_cancelled_current_caller_refused", false);
        modeled("existing_background_mode_without_receipt_refused", false);
        modeled("existing_background_handoff_receipt_resumes", true);
        modeled("existing_unauthorized_owner_refused", false);
        modeled("existing_stale_current_resume_refused", false);
        let child_request_id = "worker-existing-child";
        let (mut admission, mut old_owner) =
            published_admission_with_owner(PublishedAdmissionOptions {
                name: "worker-existing-bridge".into(),
                real_identity: true,
                await_mode: AwaitMode::Background,
                spawn_plan: Some(crate::streaming::SpawnAdmissionPlan {
                    tool_call_id: "worker-existing-spawn".into(),
                    child_request_id: child_request_id.into(),
                    spawn_target_did: "fixture-overrides-with-owner".into(),
                    spawn_behavior_id: "general".into(),
                    delegated_workspace: None,
                    await_mode: AwaitMode::Background,
                }),
                ..Default::default()
            })
            .await
            .unwrap();
        let node = admission.node.clone();
        let did = admission.agent_did.clone();
        let session = admission.tool.session_id().to_owned();
        let old_request_doc = admission.tool.request_doc_id().unwrap().to_owned();
        let old_bridge_doc = admission.tool.doc_id().unwrap().to_owned();
        let old_header_doc = admission.tool.accepted_header_doc_id().unwrap().to_owned();
        crate::test_support::install_test_behavior(&node, &did, "general").await;
        assert!(admission
            .tool
            .publish_background_receipt("child started")
            .await
            .unwrap());
        crate::tool_call_lifecycle::create_subagent_request_with_request_id(
            node.as_ref(),
            child_request_id.into(),
            old_owner.request().request_id.clone(),
            old_request_doc.clone(),
            admission.tool.tool_call_id().into(),
            old_bridge_doc.clone(),
            0,
            did.clone(),
            "general".into(),
            "child work".into(),
            Some(chrono::Utc::now() + chrono::Duration::minutes(4)),
        )
        .await
        .unwrap();
        assert_eq!(
            old_owner
                .terminalize_owned(
                    crate::lifecycle::RequestTerminalOutcome::Completed,
                    gents_protocol::output::TerminalOutput::Message {
                        message_doc_id: old_header_doc,
                    },
                    None,
                )
                .await
                .unwrap(),
            crate::lifecycle::TerminalizeResult::Won
        );

        let identity =
            crate::KeyIdentity::load_or_create(admission.path.join("test-agent.key"), None)
                .unwrap();
        let mut current = claimed_signed_request(
            &node,
            "worker-existing-later-request",
            &session,
            &identity,
            None,
        )
        .await;
        let control = publish_accepted_on_claimed_request(
            node.clone(),
            &mut current,
            &did,
            0,
            WAIT_SUBAGENT_TOOL_NAME,
            "worker-existing-wait",
            serde_json::json!({ "child_request_id": child_request_id }),
            None,
            AwaitMode::Foreground,
            CancelPolicy::Cascade,
            true,
        )
        .await
        .unwrap();
        let current_request_doc = control.request_doc_id().unwrap().to_owned();
        let current_generation = control.execution_generation().unwrap().to_owned();
        let current_control_doc = control.doc_id().unwrap().to_owned();
        let check = || {
            revalidate_parked_subagent_owner(
                node.clone(),
                WorkerTicket::new(&current_request_doc, &current_generation),
                old_bridge_doc.clone(),
                current.request().request_id.clone(),
                session.clone(),
                did.clone(),
                control.requester_did().map(str::to_owned),
                old_bridge_doc.clone(),
                old_request_doc.clone(),
                child_request_id.into(),
                Some(current_control_doc.clone()),
            )
        };
        check()
            .await
            .expect("later wait resumes exact running background bridge with prior receipt");
        complete_child(&node, child_request_id, &did, "child answer").await;
        assert!(admission
            .tool
            .bridge_complete("child answer".into())
            .await
            .unwrap());
        check()
            .await
            .expect("later accepted wait still resumes exact terminal bridge");
        crate::interrupt::interrupt_request_by_doc_id(
            &node,
            &current_request_doc,
            &did,
            control.requester_did(),
        )
        .await
        .unwrap();
        assert!(
            check().await.is_err(),
            "interrupted current caller cannot resume an older completed bridge"
        );
        assert!(
            crate::interrupt::fetch_interrupt_requested_at_by_doc_id(&node, &old_request_doc)
                .await
                .unwrap()
                .is_none(),
            "the older spawning request was not the interrupted caller"
        );

        node.shutdown().await;
        std::fs::remove_dir_all(&admission.path).unwrap();
    }
}
