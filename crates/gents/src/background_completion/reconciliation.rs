use super::*;

#[derive(Debug, Deserialize)]
struct UnclaimedBridgeRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    request_doc_id: Option<String>,
    tool_call_id: String,
    started_at: Option<String>,
    deadline_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CancelPendingBridgeRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    lifecycle_state: Option<String>,
    request_doc_id: Option<String>,
    tool_call_id: String,
    cancel_cascade_intent_at: Option<String>,
    stuck_since: Option<String>,
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

        match settle_unclaimed_spawn(&node, &row.doc_id).await? {
            UnclaimedSpawnSettlement::Linked => {
                outcomes.push(UnclaimedSpawnReconcileOutcome::Linked {
                    parent_tool_call_id: row.tool_call_id,
                    parent_request_id: row.request_id,
                });
            }
            UnclaimedSpawnSettlement::Abandoned => {
                outcomes.push(UnclaimedSpawnReconcileOutcome::Failed {
                    parent_tool_call_id: row.tool_call_id,
                    parent_request_id: row.request_id,
                });
            }
            UnclaimedSpawnSettlement::AlreadySettled | UnclaimedSpawnSettlement::Unarmed(_) => {}
        }
    }
    Ok(outcomes)
}

#[derive(Debug)]
pub(crate) enum UnclaimedSpawnSettlement {
    Linked,
    Abandoned,
    /// The row is terminal (or gone): another writer settled it.
    AlreadySettled,
    /// The row is still running but carries no due unclaimed bound (a mode
    /// flip or a concurrent link cleared it): nothing to settle; the carried
    /// bound is the row's current one.
    Unarmed(Option<chrono::DateTime<Utc>>),
}

/// The one owner of an expired unclaimed-spawn deadline (Lean
/// `SpawnClaimFence.expire`), shared by the periodic reconciler and restart
/// recovery. Only a child row that corroborates this exact bridge's physical
/// lineage and target principal counts as observed: that child links the
/// bridge and keeps running. Otherwise the bridge is abandoned with a durable
/// cancel intent. A bridge a concurrent writer already settled is left alone,
/// so repeats are no-ops.
pub(crate) async fn settle_unclaimed_spawn(
    node: &Arc<EmbeddedNode>,
    bridge_doc_id: &str,
) -> Result<UnclaimedSpawnSettlement> {
    let Some(mut lifecycle) = ToolCallLifecycle::load_physical(node.clone(), bridge_doc_id).await?
    else {
        return Ok(UnclaimedSpawnSettlement::AlreadySettled);
    };
    // The row was selected on an expired deadline; a mode flip may have
    // cleared the bound since (Lean `enabled`): then there is nothing to settle.
    if !lifecycle.is_running() {
        return Ok(UnclaimedSpawnSettlement::AlreadySettled);
    }
    if !lifecycle
        .unclaimed_deadline_at
        .is_some_and(|due| due <= Utc::now())
    {
        return Ok(UnclaimedSpawnSettlement::Unarmed(
            lifecycle.unclaimed_deadline_at,
        ));
    }
    if lifecycle.unobserved_child_fence().await?.is_none() {
        clear_unclaimed_deadline_at(node.as_ref(), bridge_doc_id).await?;
        return Ok(UnclaimedSpawnSettlement::Linked);
    }
    let payload = crate::background_tools::spawn_unclaimed_payload();
    Ok(if lifecycle.abandon_unclaimed_spawn(&payload).await? {
        UnclaimedSpawnSettlement::Abandoned
    } else if lifecycle.is_running() {
        UnclaimedSpawnSettlement::Unarmed(lifecycle.unclaimed_deadline_at)
    } else {
        UnclaimedSpawnSettlement::AlreadySettled
    })
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
            lifecycle_state
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

        // Only the child that corroborates this bridge's physical lineage can
        // acknowledge its cancel intent; a row reusing the logical id cannot.
        let probe = match crate::descendant_graph::resolve_physical_bridge_child(
            crate::descendant_graph::DescendantGraphAccess::Local(node.as_ref()),
            request_doc_id,
            &row.doc_id,
        )
        .await
        {
            Ok(probe) => probe,
            Err(error) => {
                tracing::warn!(
                    tool_call_doc_id = %row.doc_id,
                    %error,
                    "cancel-ack observer could not corroborate the bridge child; ack stays pending"
                );
                None
            }
        };
        // A cancelled bridge's intent is acknowledged by its host's interrupt
        // latch. A spawn fence (a failed or timed-out bridge; Lean
        // `SpawnClaimFence.observeAck`) waits for the child to be terminal: a latched child that already won its claim may
        // still be running, and the parent must not re-spawn over it.
        let spawn_fence = matches!(row.lifecycle_state.as_deref(), Some("failed" | "timedOut"));
        let child_done = probe.as_ref().is_some_and(|p| {
            if !spawn_fence {
                request_terminal_or_interrupted(p)
            } else {
                p.lifecycle_state
                    .is_some_and(RequestLifecycleState::is_terminal)
            }
        });

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
    crate::config_client::ConfigAccess::write_local_response(
        node,
        "background_completion.clear_unclaimed_deadline",
        &mutation,
    )
    .await?;
    Ok(())
}

fn request_terminal_or_interrupted(row: &AgentRequestRow) -> bool {
    row.lifecycle_state
        .is_some_and(RequestLifecycleState::is_terminal)
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
