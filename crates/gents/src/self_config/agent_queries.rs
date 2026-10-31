use super::*;

pub(super) async fn node_default_agent(
    node: &EmbeddedNode,
    node_did: &str,
) -> Result<Option<String>> {
    let query = format!(
        r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}, limit: 2) {{ default_agent_id }} }}"#,
        escape_graphql_string(node_did)
    );
    let response = crate::store::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "self_config.default_agent",
    )
    .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("Node"))
        .and_then(Value::as_array)
        .context("Node query returned no rows")?;
    anyhow::ensure!(rows.len() <= 1, "ambiguous node identity");
    Ok(rows
        .first()
        .and_then(|row| row.get("default_agent_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

async fn agent_snapshot(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    process_ceiling: &crate::tools::surface::SelfConfigProcessCeiling,
    protected: bool,
    default_agent_id: Option<&str>,
) -> Result<Value> {
    let effective = SelfConfigCore::new(node.clone(), node_did.to_owned(), agent_id.to_owned())?
        .with_process_ceiling(process_ceiling.clone())
        .read_effective_config(&BTreeSet::new(), false, false)
        .await?;
    Ok(json!({
        "agent_id": agent_id,
        "protected": protected,
        "is_default": default_agent_id == Some(agent_id),
        "effective_config": effective,
    }))
}

pub(super) async fn agent_list(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    process_ceiling: &crate::tools::surface::SelfConfigProcessCeiling,
    limit: usize,
    cursor: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        (1..=50).contains(&limit),
        "--limit must be between 1 and 50"
    );
    let catalog = load_agent_catalog(node, node_did).await?;
    let default_agent_id = node_default_agent(node, node_did).await?;
    let total = catalog.agents.len();
    let selected = catalog
        .agents
        .iter()
        .filter(|(agent_id, _)| cursor.is_none_or(|cursor| agent_id.as_str() > cursor))
        .take(limit + 1)
        .collect::<Vec<_>>();
    let truncated = selected.len() > limit;
    let mut agents = Vec::new();
    for (agent_id, reference) in selected.into_iter().take(limit) {
        let snapshot = match agent_snapshot(
            node,
            node_did,
            agent_id,
            process_ceiling,
            catalog.protected_ids.contains(agent_id),
            default_agent_id.as_deref(),
        )
        .await
        {
            Ok(snapshot) => {
                let effective = &snapshot["effective_config"];
                json!({
                    "agent_id": agent_id,
                    "display_name": effective.pointer("/agent/display_name"),
                    "description": effective.pointer("/agent/description"),
                    "enabled": *reference,
                    "protected": catalog.protected_ids.contains(agent_id),
                    "is_default": default_agent_id.as_deref() == Some(agent_id.as_str()),
                    "context_id": effective.pointer("/agent/context_id"),
                    "profile_id": effective.pointer("/agent/inference_profile_id"),
                    "backend_id": effective.pointer("/inference_profile/backend_id"),
                    "model_name": effective.pointer("/inference_profile/model_name"),
                    "reasoning_effort": effective.pointer("/inference_profile/reasoning_effort"),
                    "service_tier": effective.pointer("/inference_profile/service_tier"),
                })
            }
            Err(error) => json!({
                "agent_id": agent_id,
                "enabled": *reference,
                "protected": catalog.protected_ids.contains(agent_id),
                "is_default": default_agent_id.as_deref() == Some(agent_id.as_str()),
                "configuration_error": format!("{error:#}"),
            }),
        };
        agents.push(snapshot);
    }
    let next_cursor = truncated
        .then(|| {
            agents
                .last()?
                .get("agent_id")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .flatten();
    let snapshot = json!({
        "page": {
            "limit": limit,
            "total": total,
            "returned": agents.len(),
            "truncated": truncated,
            "next_cursor": next_cursor,
        },
        "process_ceiling": process_ceiling,
        "permission_presets": builtin_preset_names(),
        "available_profile_ids": catalog.published_profiles,
        "default_agent_id": default_agent_id,
        "agents": agents,
        "activation": {
            "durable_config": "after the admitted transaction commits",
            "running_generation": "after the runtime reconciler generation swap",
            "session": "select the agent in a new session; existing sessions remain bound to their original agent",
            "pairing_and_restart": "canonical documents replicate to authorized paired clients and are reloaded after restart",
        },
    });
    serde_json::to_string_pretty(&snapshot).map_err(|error| anyhow!("serialize catalog: {error}"))
}

pub(super) async fn agent_inspect(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    process_ceiling: &crate::tools::surface::SelfConfigProcessCeiling,
) -> Result<String> {
    let catalog = load_agent_catalog(node, node_did).await?;
    let _ = catalog.agents.get(agent_id).with_context(|| {
        format!("unknown agent_id {agent_id:?}; copy an exact ID from [\"agent\",\"list\"]")
    })?;
    let default_agent_id = node_default_agent(node, node_did).await?;
    let snapshot = agent_snapshot(
        node,
        node_did,
        agent_id,
        process_ceiling,
        catalog.protected_ids.contains(agent_id),
        default_agent_id.as_deref(),
    )
    .await?;
    let Value::Object(mut snapshot) = snapshot else {
        bail!("agent snapshot must be an object");
    };
    let effective = snapshot.remove("effective_config").unwrap_or_default();
    let mut result = Ordered::reading_order(
        Value::Object(snapshot),
        &["agent_id", "is_default", "protected"],
    );
    result.push(Ordered::entry(
        "effective_config",
        &Ordered::reading_order(effective, EFFECTIVE_ORDER),
    ));
    result.pretty()
}

pub(super) async fn guard_agent_profile_choice(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    current_agent: Option<&str>,
    clone_from: Option<&str>,
    next_profile: Option<&str>,
) -> Result<()> {
    let Some(next_profile) = next_profile else {
        return Ok(());
    };
    let current_profile = match current_agent {
        Some(agent_id) => ops::read_owned_doc(txn, SelfConfigTarget::Agent, node_did, agent_id)
            .await?
            .and_then(|(_, doc)| {
                doc.get("inference_profile_id")?
                    .as_str()
                    .map(ToOwned::to_owned)
            }),
        None => None,
    };
    let current_backend = match current_profile {
        Some(profile) => Some(ops::profile_backend_id(txn, node_did, &profile).await?),
        None => None,
    };
    let next_backend = ops::profile_backend_id(txn, node_did, next_profile).await?;
    ops::guard_backend_choice_in_txn(txn, node_did, current_backend.as_deref(), &next_backend)
        .await?;
    let source = match clone_from {
        Some(id) => ops::read_owned_doc(txn, SelfConfigTarget::Agent, node_did, id).await?,
        None => None,
    };
    let Some((_, source)) = source else {
        return Ok(());
    };
    let own =
        serde_json::Map::from_iter([("inference_profile_id".to_owned(), json!(next_profile))]);
    let mut cloned = own.clone();
    if let Some(context) = source.get("context_id") {
        cloned.insert("context_id".to_owned(), context.clone());
    }
    let anchor = ops::AgentAnchor {
        doc: source,
        context: serde_json::Map::new(),
        profile: serde_json::Map::new(),
        execution: serde_json::Map::new(),
    };
    ops::guard_compaction_choice_in_txn(txn, SelfConfigTarget::Agent, &anchor, &own, &cloned).await
}
