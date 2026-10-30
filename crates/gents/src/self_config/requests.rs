use super::*;

pub(super) fn outcome_text(outcome: &PatchOutcome) -> Result<String> {
    serde_json::to_string_pretty(outcome).map_err(|error| anyhow!("serialize outcome: {error}"))
}

pub(super) fn anchored_request(
    target: SelfConfigTarget,
    field: &'static str,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let mut request = ApplyRequest::new(target, patch);
    request.resolve_unique = Box::new(move |anchor| {
        anchor
            .ref_id(field)
            .ok_or_else(|| anyhow!("bound {field} is missing"))
    });
    request
}
pub(super) fn agent_request(
    core: &SelfConfigCore,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let id = core.agent_id().to_owned();
    let mut request = ApplyRequest::new(SelfConfigTarget::Agent, patch);
    request.resolve_unique = Box::new(move |_| Ok(id.clone()));
    request.guard = Box::new(|_, stored, merged| guard_agent_keeps_reach(stored, merged));
    fence_profile_pick(&mut request);
    request
}
fn fence_profile_pick(request: &mut ApplyRequest<'static>) {
    request.validate = Box::new(|txn, anchor, stored, merged| {
        Box::pin(async move {
            let Some(next) = merged.get("inference_profile_id").and_then(Value::as_str) else {
                return Ok(());
            };
            let current = stored.get("inference_profile_id").and_then(Value::as_str);
            if current == Some(next) {
                return Ok(());
            }
            let owner = merged
                .get("node_did")
                .and_then(Value::as_str)
                .context("document is missing node_did")?;
            let current_backend = match current {
                Some(id) => Some(ops::profile_backend_id(txn, owner, id).await?),
                None => anchor
                    .profile
                    .get("backend_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            };
            let next_backend = ops::profile_backend_id(txn, owner, next).await?;
            ops::guard_backend_choice_in_txn(txn, owner, current_backend.as_deref(), &next_backend)
                .await
        })
    });
}

/// Model-facing patches may target any owned agent, including the invoking
/// configurator itself. Keep the shared-reference check inside the same
/// transaction as validation/publication so a stale preflight cannot
/// authorize a write.
pub(super) fn protect_working_agent(mut request: ApplyRequest<'static>) -> ApplyRequest<'static> {
    let target = request.target;
    let validate = request.validate;
    request.validate = Box::new(move |txn, anchor, stored, merged| {
        let validation = validate(txn, anchor, stored, merged);
        Box::pin(async move {
            let owner = anchor
                .doc
                .get("node_did")
                .and_then(Value::as_str)
                .context("selected agent is missing node_did")?;
            let agent_id = anchor
                .doc
                .get("agent_id")
                .and_then(Value::as_str)
                .context("selected agent is missing agent_id")?;
            let require_only_referrer = |response: &Value,
                                         collection: &str,
                                         unique: &str,
                                         expected: &str| {
                let rows = response
                    .get("data")
                    .and_then(|data| data.get(collection))
                    .and_then(Value::as_array)
                    .with_context(|| format!("{collection} reference query missing rows"))?;
                anyhow::ensure!(
                    rows.len() == 1
                        && rows[0].get(unique).and_then(Value::as_str) == Some(expected),
                    "targeted configuration requires an unshared Context and Tools; clone the working agent before editing shared configuration"
                );
                Ok::<_, anyhow::Error>(())
            };
            if target == SelfConfigTarget::AgentContext || target == SelfConfigTarget::Tools {
                let context_id = anchor.context.get("context_id").and_then(Value::as_str)
                    .context("selected agent has no context; create and select a Context before editing it or its Tools")?;
                let escaped_owner = escape_graphql_string(owner);
                let escaped_context = escape_graphql_string(context_id);
                let response = txn
                    .execute(&format!(
                        r#"{{ Agent(filter: {{node_did: {{_eq: "{escaped_owner}"}}, context_id: {{_eq: "{escaped_context}"}}}}) {{agent_id}} }}"#
                    ))
                    .await?;
                require_only_referrer(&response, "Agent", "agent_id", agent_id)?;
                if target == SelfConfigTarget::Tools {
                    let tools_id = stored
                        .get("tools_id")
                        .and_then(Value::as_str)
                        .context("selected Tools is missing tools_id")?;
                    let escaped_owner = escape_graphql_string(owner);
                    let escaped_tools = escape_graphql_string(tools_id);
                    let response = txn
                    .execute(&format!(
                        r#"{{ AgentContext(filter: {{node_did: {{_eq: "{escaped_owner}"}}, tools_id: {{_eq: "{escaped_tools}"}}}}) {{context_id}} }}"#
                    ))
                    .await?;
                    require_only_referrer(&response, "AgentContext", "context_id", context_id)?;
                }
            }
            ops::guard_compaction_choice_in_txn(txn, target, anchor, stored, merged).await?;
            validation.await
        })
    });
    request
}
fn stored_host_root(stored: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    serde_json::from_value::<crate::documents::Tools>(serde_json::Value::Object(stored.clone()))
        .ok()?
        .host?
        .root
}

pub(super) fn tools_request(
    core: &SelfConfigCore,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let mut request = anchored_request(SelfConfigTarget::Tools, "tools_id", patch);
    let ceiling_root = core.process_ceiling().root.clone();
    let held = core.held_grants().clone();
    request.normalize = Box::new(move |txn, _, stored, merged| {
        let ceiling_root = ceiling_root.clone();
        Box::pin(async move {
            let policy = crate::tool_surface::load_workspace_root_policy_in_txn(
                txn,
                ceiling_root.as_deref(),
            )
            .await?;
            let stored_root = stored_host_root(stored);
            let mut tools = decode_merged::<crate::documents::Tools>("Tools", merged)?;
            crate::tool_surface::canonicalize_tools_root(
                &mut tools,
                &policy,
                stored_root.as_deref(),
            )?;
            let canonical = serde_json::to_value(tools)?
                .as_object()
                .context("canonical Tools document must be an object")?
                .clone();
            *merged = canonical;
            Ok(())
        })
    });
    request.validate = Box::new(move |_, _, stored, merged| {
        let stored = stored.clone();
        let merged = merged.clone();
        let held = held.clone();
        Box::pin(async move {
            validate_merged_selection(&merged)?;
            guard_tools_keep_grants(&held, Some(&stored), &merged)
        })
    });
    request.guard = Box::new(|_, stored, merged| guard_tools_keep_control(stored, merged));
    request
}
pub(super) fn refuse_silent_tools_drops(
    mut request: ApplyRequest<'static>,
    allow_drop: BTreeSet<String>,
) -> ApplyRequest<'static> {
    let groups = request
        .patch
        .iter()
        .map(|(field, _)| field.clone())
        .filter(|field| !allow_drop.contains(field))
        .collect::<Vec<_>>();
    let guard = request.guard;
    request.guard = Box::new(move |anchor, stored, merged| {
        guard(anchor, stored, merged)?;
        let mut dropped = Vec::new();
        for group in &groups {
            dropped_settings(
                group,
                stored.get(group).unwrap_or(&Value::Null),
                merged.get(group).unwrap_or(&Value::Null),
                &mut dropped,
            );
        }
        if dropped.is_empty() {
            return Ok(());
        }
        let mut groups = dropped
            .iter()
            .map(|path| path.split(['.', ' ']).next().unwrap_or_default())
            .collect::<Vec<_>>();
        groups.dedup();
        bail!(
            "this would drop existing settings from your own Tools: {}. Send them back in set (see [\"tools\",\"get\"]), or to drop them on purpose add options.allow-drop = {:?}",
            dropped.join(", "),
            groups.join(",")
        )
    });
    request
}

fn dropped_settings(path: &str, before: &Value, after: &Value, out: &mut Vec<String>) {
    match (before, after) {
        (Value::Null, _) => {}
        (Value::Object(before), Value::Object(after)) => {
            for (key, value) in before {
                dropped_settings(
                    &format!("{path}.{key}"),
                    value,
                    after.get(key).unwrap_or(&Value::Null),
                    out,
                );
            }
        }
        (Value::Array(before), Value::Array(after)) => {
            out.extend(
                before
                    .iter()
                    .filter(|item| !after.contains(item))
                    .map(|item| format!("{path} item {item}")),
            );
        }
        (_, Value::Null) => out.push(path.to_owned()),
        _ => {}
    }
}
pub(super) fn profile_request(patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = anchored_request(
        SelfConfigTarget::InferenceProfile,
        "inference_profile_id",
        patch,
    );
    fence_profile_backend(&mut request);
    request
}
fn fence_profile_backend(request: &mut ApplyRequest<'static>) {
    request.validate = Box::new(|txn, _, stored, merged| {
        Box::pin(async move {
            let Some(next) = merged.get("backend_id").and_then(Value::as_str) else {
                return Ok(());
            };
            let current = stored.get("backend_id").and_then(Value::as_str);
            if current == Some(next) {
                return Ok(());
            }
            let owner = merged
                .get("node_did")
                .and_then(Value::as_str)
                .context("profile is missing node_did")?;
            ops::guard_backend_choice_in_txn(txn, owner, current, next).await
        })
    });
}
pub(super) fn profile_create_request(
    owner: String,
    profile_id: String,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let mut request = ApplyRequest::new(SelfConfigTarget::InferenceProfile, patch);
    request.allow_create = true;
    request.require_create = true;
    request.guard_selected_chain = false;
    request.resolve_unique = Box::new(move |_| Ok(profile_id.clone()));
    request.on_create = Box::new(move |id, merged| {
        merged.insert("profile_id".into(), json!(id));
        merged.insert("node_did".into(), json!(owner));
        Ok(())
    });
    fence_profile_backend(&mut request);
    request
}
pub(super) fn profile_target_request(
    target: Option<&str>,
    patch: SelfConfigPatch,
) -> Result<ApplyRequest<'static>> {
    Ok(match target.unwrap_or("profile") {
        "profile" => profile_request(patch),
        "sampling" => anchored_request(SelfConfigTarget::InferenceSampling, "sampling_id", patch),
        "execution" => {
            anchored_request(SelfConfigTarget::InferenceExecution, "execution_id", patch)
        }
        "retry_policy" => anchored_request(
            SelfConfigTarget::InferenceRetryPolicy,
            "retry_policy_id",
            patch,
        ),
        "compaction" => {
            let mut request =
                anchored_request(SelfConfigTarget::Compaction, "compaction_id", patch);
            fence_profile_pick(&mut request);
            request
        }
        other => bail!("unknown profile target {other:?}"),
    })
}
pub(super) fn backend_request(patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = anchored_request(SelfConfigTarget::InferenceBackend, "backend_id", patch);
    request.validate = Box::new(move |_, _, stored, merged| {
        let merged = merged.clone();
        let stored = stored.clone();
        Box::pin(async move {
            let backend: crate::InferenceBackend = decode_merged("InferenceBackend", &merged)?;
            guard_backend_auth(&stored, &merged)?;
            backend.validate()
        })
    });
    request.guard = Box::new(|_, _, merged| {
        anyhow::ensure!(
            merged.get("enabled").and_then(Value::as_bool) != Some(false),
            "no-lockout guard: backend must remain enabled"
        );
        Ok(())
    });
    request
}
pub(super) fn local_backend_create_request(
    owner: String,
    backend_id: String,
    endpoint: String,
    name: Option<String>,
    wire_api: Option<crate::openai_wire::OpenAiWireApi>,
) -> ApplyRequest<'static> {
    let mut patch = vec![
        (
            "name".into(),
            Some(json!(name.unwrap_or_else(|| format!("Local {backend_id}")))),
        ),
        (
            "provider_kind".into(),
            Some(json!(crate::BackendProviderKind::OpenAiCompatible)),
        ),
        ("endpoint".into(), Some(json!(endpoint))),
        ("auth".into(), Some(json!({"kind": "unauthenticated"}))),
    ];
    if let Some(wire_api) = wire_api {
        patch.push(("openai_wire_api".into(), Some(json!(wire_api))));
    }
    let mut request = ApplyRequest::new(SelfConfigTarget::InferenceBackend, patch);
    request.allow_create = true;
    request.require_create = true;
    request.guard_selected_chain = false;
    request.resolve_unique = Box::new(move |_| Ok(backend_id.clone()));
    request.on_create = Box::new(move |id, merged| {
        merged.insert("backend_id".into(), json!(id));
        merged.insert("node_did".into(), json!(owner));
        Ok(())
    });
    request.validate = Box::new(move |_, _, _, merged| {
        let merged = merged.clone();
        Box::pin(async move {
            let backend: crate::InferenceBackend = decode_merged("InferenceBackend", &merged)?;
            anyhow::ensure!(
                backend.provider_kind == crate::BackendProviderKind::OpenAiCompatible
                    && matches!(
                        backend.auth,
                        crate::documents::BackendAuth::Unauthenticated
                    ),
                "model-facing backend creation is limited to unauthenticated OpenAI-compatible local servers"
            );
            anyhow::ensure!(
                backend.enabled,
                "a newly created local backend must be enabled for discovery"
            );
            backend.validate()
        })
    });
    request
}
pub(super) fn mcp_service_request(
    service_id: String,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let mut request = ApplyRequest::new(SelfConfigTarget::ToolServiceRegistry, patch);
    request.resolve_unique = Box::new(move |_| Ok(service_id.clone()));
    request
}
pub(super) fn automation_request(
    core: &SelfConfigCore,
    target: SelfConfigTarget,
    id: String,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let owner = core.node_did().to_owned();
    let agent = core.agent_id().to_owned();
    let mut request = ApplyRequest::new(target, patch);
    request.allow_create = true;
    request.resolve_unique = Box::new(move |_| Ok(id.clone()));
    request.on_create = Box::new(move |id, merged| {
        merged.insert(target.unique_field().into(), json!(id));
        merged.insert("node_did".into(), json!(owner));
        if target == SelfConfigTarget::Task {
            merged.insert("agent_id".into(), json!(agent));
        }
        Ok(())
    });
    let core = core.clone();
    request.validate = Box::new(move |txn, anchor, stored, merged| {
        let core = core.clone();
        let stored = stored.clone();
        let merged = merged.clone();
        Box::pin(async move {
            if target == SelfConfigTarget::Task {
                anyhow::ensure!(
                    merged.get("agent_id").and_then(Value::as_str) == Some(core.agent_id()),
                    "task belongs to agent {:?}, not {:?}; retry with options.agent set to its owner; to run a different agent, create a separate task",
                    merged.get("agent_id").and_then(Value::as_str),
                    core.agent_id()
                );
            }
            // Packs may publish an event source before installing its schema;
            // a collection missing here is a typo that would never fire.
            if target == SelfConfigTarget::EventSource {
                if let Some(collection) = merged.get("source_collection").and_then(Value::as_str) {
                    anyhow::ensure!(
                        crate::config_client::collection_is_installed(txn, collection).await?,
                        "event source source_collection {collection:?} is not an installed collection; install its schema first with the schema tool (argv:[\"help\",\"collection\"])"
                    );
                }
            }
            if target == SelfConfigTarget::Trigger {
                for doc in [&stored, &merged].into_iter().filter(|doc| !doc.is_empty()) {
                    let task = doc
                        .get("task_id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("Trigger.task_id must name an existing Task; inspect tasks with {{\"argv\":[\"task\",\"list\"]}}"))?;
                    anyhow::ensure!(
                        core.task_owned(txn, anchor, task).await?,
                        "Trigger.task_id {task:?} must reference a Task owned by selected agent {:?}. Inspect it with {{\"argv\":[\"task\",\"get\"],\"target_id\":{task:?}}}. Create a missing Task first with options.agent set to that owner. An existing Trigger cannot switch task owners; create a separate Trigger for a different owner and preserve the current workflow until its replacement is valid",
                        core.agent_id()
                    );
                }
            }
            Ok(())
        })
    });
    request
}
pub(super) fn automation_target(kind: &str) -> Result<SelfConfigTarget> {
    match kind {
        "task" => Ok(SelfConfigTarget::Task),
        "schedule" => Ok(SelfConfigTarget::Schedule),
        "trigger" => Ok(SelfConfigTarget::Trigger),
        "event_source" => Ok(SelfConfigTarget::EventSource),
        _ => {
            bail!("unknown automation kind {kind:?}; use task, schedule, trigger, or event_source")
        }
    }
}
