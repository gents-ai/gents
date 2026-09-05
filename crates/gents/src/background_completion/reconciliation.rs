use super::*;

#[derive(Debug, Deserialize)]
struct UnclaimedBridgeRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    request_doc_id: Option<String>,
    tool_call_id: String,
    child_request_id: String,
    started_at: Option<String>,
    deadline_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CancelPendingBridgeRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    request_doc_id: Option<String>,
    tool_call_id: String,
    child_request_id: String,
    cancel_cascade_intent_at: Option<String>,
    stuck_since: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChildAckProbeRow {
    lifecycle_state: Option<String>,
    interrupt_requested_at: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct AgentToolCallDateTimeRow {
    pub(crate) started_at: Option<String>,
    pub(crate) deadline_at: Option<String>,
    pub(crate) completed_at: Option<String>,
    pub(crate) unclaimed_deadline_at: Option<String>,
    pub(crate) cancel_cascade_intent_at: Option<String>,
    pub(crate) stuck_since: Option<String>,
}

pub async fn reconcile_unclaimed_cross_deployment_spawns(
    node: Arc<EmbeddedNode>,
    local_did: &str,
) -> Result<Vec<UnclaimedSpawnReconcileOutcome>> {
    let now = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let now = escape_graphql_string(&now);
    let query = format!(
        r#"{{
            AgentToolCall(
                filter: {{
                    _and: [
                        {{ lifecycle_state: {{ _eq: "running" }} }},
                        {{ await_mode: {{ _eq: "background" }} }},
                        {{ child_request_id: {{ _ne: "" }} }},
                        {{ unclaimed_deadline_at: {{ _lt: "{now}" }} }}
                    ]
                }}
            ) {{
                _docID
                request_id
                request_doc_id
                tool_call_id
                child_request_id
                started_at
                deadline_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "unclaimed-spawn reconcile query failed: {:?}",
            response.errors
        );
    }
    let rows: Vec<UnclaimedBridgeRow> = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentToolCall"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut outcomes = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(request_doc_id) = non_empty(row.request_doc_id.as_deref()) else {
            tracing::warn!(
                tool_call_doc_id = %row.doc_id,
                request_id = %row.request_id,
                "skipping unclaimed bridge without physical request provenance"
            );
            continue;
        };
        if !request_is_locally_owned(node.as_ref(), &row.request_id, request_doc_id, local_did)
            .await?
        {
            continue;
        }

        if child_request_exists_locally(node.as_ref(), &row.child_request_id).await? {
            clear_unclaimed_deadline_at(node.as_ref(), &row.doc_id).await?;
            outcomes.push(UnclaimedSpawnReconcileOutcome::Linked {
                parent_tool_call_id: row.tool_call_id,
                parent_request_id: row.request_id,
            });
            continue;
        }

        let payload = subagent_tool_not_allowed_payload(
            "spawn_subagent",
            "/behavior_id",
            "<unknown>",
            "no_peer_claimed_spawn: no paired peer claimed the cross-deployment spawn within unclaimed_spawn_timeout_seconds",
            &[],
        );
        fail_running_subagent_tool_call(
            node.as_ref(),
            &row.doc_id,
            row.started_at.as_deref(),
            row.deadline_at.as_deref(),
            &payload,
            FailureClass::ServiceUnavailable,
        )
        .await?;
        outcomes.push(UnclaimedSpawnReconcileOutcome::Failed {
            parent_tool_call_id: row.tool_call_id,
            parent_request_id: row.request_id,
        });
    }
    Ok(outcomes)
}

pub async fn observe_cancel_cascade_ack(
    node: Arc<EmbeddedNode>,
    local_did: &str,
) -> Result<Vec<CancelAckOutcome>> {
    let now = Utc::now();
    let query = r#"{
        AgentToolCall(filter: { cancel_pending_remote_ack: { _eq: true } }) {
            _docID
            request_id
            request_doc_id
            tool_call_id
            child_request_id
            cancel_cascade_intent_at
            stuck_since
        }
    }"#;
    let response = node.execute(query).await;
    if response.has_errors() {
        anyhow::bail!("cancel-ack observer query failed: {:?}", response.errors);
    }
    let rows: Vec<CancelPendingBridgeRow> = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentToolCall"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let mut outcomes = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(request_doc_id) = non_empty(row.request_doc_id.as_deref()) else {
            tracing::warn!(
                tool_call_doc_id = %row.doc_id,
                request_id = %row.request_id,
                "skipping cancel bridge without physical request provenance"
            );
            continue;
        };
        if !request_is_locally_owned(node.as_ref(), &row.request_id, request_doc_id, local_did)
            .await?
        {
            continue;
        }

        let probe = load_child_ack_probe(node.as_ref(), &row.child_request_id).await?;
        let child_done = probe
            .as_ref()
            .is_some_and(|p| request_terminal_or_interrupted(p));

        if child_done {
            clear_cancel_pending_ack(node.as_ref(), &row.doc_id).await?;
            outcomes.push(CancelAckOutcome::Acked {
                parent_tool_call_id: row.tool_call_id,
            });
            continue;
        }

        let intent_at = row
            .cancel_cascade_intent_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));
        if let Some(intent_at) = intent_at {
            let age = (now - intent_at).num_seconds();
            if age >= STUCK_CANCEL_THRESHOLD_SECS && row.stuck_since.is_none() {
                set_stuck_since(node.as_ref(), &row.doc_id, now).await?;
                outcomes.push(CancelAckOutcome::Stuck {
                    parent_tool_call_id: row.tool_call_id,
                    since: now,
                });
                continue;
            }
        }

        outcomes.push(CancelAckOutcome::Pending {
            parent_tool_call_id: row.tool_call_id,
        });
    }
    Ok(outcomes)
}

async fn child_request_exists_locally(node: &EmbeddedNode, child_request_id: &str) -> Result<bool> {
    Ok(
        crate::request_binding::resolve_request_doc_id(node, child_request_id)
            .await?
            .is_some(),
    )
}

async fn clear_unclaimed_deadline_at(node: &EmbeddedNode, doc_id: &str) -> Result<()> {
    let escaped = escape_graphql_string(doc_id);
    let datetime_fields =
        agent_tool_call_datetime_update_fragment(node, doc_id, &["unclaimed_deadline_at"]).await?;
    let mutation = format!(
        r#"mutation {{
            update_AgentToolCall(
                filter: {{ _docID: {{ _eq: "{escaped}" }} }},
                input: {{ unclaimed_deadline_at: null{datetime_fields} }}
            ) {{ _docID }}
        }}"#
    );
    crate::graphql::graphql_mutation_with_transaction_retry(
        node,
        &mutation,
        "clear unclaimed_deadline_at",
    )
    .await?;
    Ok(())
}

async fn load_child_ack_probe(
    node: &EmbeddedNode,
    child_request_id: &str,
) -> Result<Option<ChildAckProbeRow>> {
    let Some(child_request_doc_id) =
        crate::request_binding::resolve_request_doc_id(node, child_request_id).await?
    else {
        return Ok(None);
    };
    let escaped = escape_graphql_string(&child_request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                lifecycle_state
                interrupt_requested_at
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!("child ack probe failed: {:?}", response.errors);
    }
    let rows: Vec<ChildAckProbeRow> = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();
    Ok(rows.into_iter().next())
}

fn request_terminal_or_interrupted(row: &ChildAckProbeRow) -> bool {
    RequestLifecycleState::is_terminal_str(row.lifecycle_state.as_deref())
        || row.interrupt_requested_at.is_some()
}

pub(super) async fn request_is_locally_owned(
    node: &EmbeddedNode,
    request_id: &str,
    request_doc_id: &str,
    local_did: &str,
) -> Result<bool> {
    let escaped_request_doc_id = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_request_doc_id}" }} }},
                limit: 1
            ) {{ request_id agent_did }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        anyhow::bail!(
            "query parent AgentRequest owner {request_id} failed: {:?}",
            response.errors
        );
    }
    let Some(row) = response
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
    else {
        // Cross-deployment bridges intentionally replicate without their
        // physical parent request. Absence is therefore an ownership-negative
        // result, not a malformed local row or a fatal observer error.
        return Ok(false);
    };
    Ok(
        row.get("request_id").and_then(|v| v.as_str()) == Some(request_id)
            && row.get("agent_did").and_then(|v| v.as_str()) == Some(local_did),
    )
}
