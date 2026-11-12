use super::*;

pub(super) fn outcome_text(outcome: &PatchOutcome) -> Result<String> {
    gents_session::session::output_render::Ordered::reading_order(
        serde_json::to_value(outcome)?,
        &[
            "committed",
            "collection",
            "target_id",
            "agent_did",
            "connection",
            "created",
            "changed",
            "effect",
        ],
    )
    .pretty()
}

pub fn anchored_request(
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
#[cfg(any(test, feature = "test-fixtures"))]
pub fn agent_request(core: &SelfConfigCore, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let id = core.agent_did().to_owned();
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
///
/// `acknowledged` is passed in rather than read from `request.shared_ack`:
/// reading the field at call time made the answer depend on whether the caller
/// had already assigned it, which is how the bound `context update` path
/// silently ignored `options.shared = "acknowledge"`.
pub fn protect_working_agent(
    mut request: ApplyRequest<'static>,
    acknowledged: bool,
    holds_catalog_grant: bool,
) -> ApplyRequest<'static> {
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
            // A Context or Tools that more than one agent selects is refused
            // unless the caller acknowledged it; the refusal names every agent
            // the write would reach. The acknowledgement waives only that
            // refusal: it never widens who may write, so a shared edit still
            // needs the agent catalog grant.
            if target == SelfConfigTarget::AgentContext || target == SelfConfigTarget::Tools {
                let context_id = anchor.context.get("context_id").and_then(Value::as_str)
                    .context("selected agent has no context; create and select a Context before editing it or its Tools")?;
                ops::guard_shared_authority_in_txn(
                    txn,
                    SelfConfigTarget::AgentContext,
                    owner,
                    context_id,
                    acknowledged,
                    holds_catalog_grant,
                )
                .await?;
                if target == SelfConfigTarget::Tools {
                    let tools_id = stored
                        .get("tools_id")
                        .and_then(Value::as_str)
                        .context("selected Tools is missing tools_id")?;
                    ops::guard_shared_authority_in_txn(
                        txn,
                        SelfConfigTarget::Tools,
                        owner,
                        tools_id,
                        acknowledged,
                        holds_catalog_grant,
                    )
                    .await?;
                }
            }
            ops::guard_compaction_choice_in_txn(txn, target, anchor, stored, merged).await?;
            validation.await
        })
    });
    request
}

fn stored_host_root(stored: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    serde_json::from_value::<gents_documents::Tools>(serde_json::Value::Object(stored.clone()))
        .ok()?
        .host?
        .root
}

pub fn tools_request(core: &SelfConfigCore, patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = anchored_request(SelfConfigTarget::Tools, "tools_id", patch);
    let ceiling_root = core.process_ceiling().root.clone();
    let node_did = core.node_did().to_owned();
    let held = core.held_grants().clone();
    request.normalize = Box::new(move |txn, _, stored, merged| {
        let ceiling_root = ceiling_root.clone();
        let node_did = node_did.clone();
        Box::pin(async move {
            let policy = crate::surface::load_workspace_root_policy_in_txn(
                txn,
                ceiling_root.as_deref(),
                &node_did,
            )
            .await?;
            let stored_root = stored_host_root(stored);
            let mut tools = decode_merged::<gents_documents::Tools>("Tools", merged)?;
            crate::surface::canonicalize_tools_root(&mut tools, &policy, stored_root.as_deref())?;
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
/// Refuses a Tools write that newly selects a plugin this node has not
/// installed or that silently drops a stored setting (Lean
/// `SelfConfig.ToolsSave.outcome`), for every agent the write targets. The
/// refusals are the ones `tools_operation::admit_tools_save` raises for any
/// client; here they are spelled as the config tool's calls. The sharing rule
/// is admitted by `protect_working_agent` from the anchored agent's referrers,
/// and the no-lockout rule by the patch engine, which runs this after it.
pub(super) fn refuse_unadmitted_tools_save<F>(
    mut request: ApplyRequest<'static>,
    held: OperatorGrants,
    allow_drop: BTreeSet<String>,
    installed: F,
) -> ApplyRequest<'static>
where
    F: Fn(&str) -> Result<()> + Send + Sync + 'static,
{
    use gents_protocol::operations::document::OperationActor;
    use gents_protocol::operations::tools::ToolsOperationError;
    let groups = request
        .patch
        .iter()
        .map(|(field, _)| field.clone())
        .collect::<Vec<_>>();
    request.admit = Box::new(move |stored, merged| {
        let admitted = tools_operation::admit_tools_save(
            &tools_operation::ToolsActor {
                actor: OperationActor::Agent,
                agent_held: held.clone(),
                holds_catalog: true,
                installed: Some(&installed),
            },
            tools_operation::ToolsCandidate {
                groups: &groups,
                allow_drop: &allow_drop,
                shared_acknowledged: true,
                stored: Some(stored),
                merged,
                agents: &[],
                own_control: Ok(()),
            },
        )?;
        match admitted {
            Ok(()) => Ok(()),
            Err(ToolsOperationError::DropsSettings { dropped, groups }) => bail!(
                "this would drop existing settings from these Tools: {}. Send them back in set (see [\"tools\",\"get\"]), or to drop them on purpose add options.allow-drop = {:?}",
                dropped.join(", "),
                groups.join(",")
            ),
            Err(ToolsOperationError::PluginNotInstalled { plugin, reason }) => {
                Err(command::CommandGuidance {
                    message: format!(
                        "nothing was changed: these Tools would select plugin {plugin}, which is not installed on this node ({reason}). An agent whose Tools select a plugin that is not installed cannot load its tools and fails its next request. Install the pack that ships {plugin}, then select it with pack wire, which pins the installed digest and writes its job block"
                    ),
                    next_call: json!({"argv":["pack","preview","install","<PACK>"]}),
                }
                .into())
            }
            Err(other) => Err(other.into()),
        }
    });
    request
}

pub fn profile_request(patch: SelfConfigPatch) -> ApplyRequest<'static> {
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
pub fn profile_create_request(
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
pub fn profile_target_request(
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
pub fn backend_request(patch: SelfConfigPatch) -> ApplyRequest<'static> {
    let mut request = anchored_request(SelfConfigTarget::InferenceBackend, "backend_id", patch);
    request.validate = Box::new(move |_, _, stored, merged| {
        let merged = merged.clone();
        let stored = stored.clone();
        Box::pin(async move {
            let backend: gents_documents::InferenceBackend =
                decode_merged("InferenceBackend", &merged)?;
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
    wire_api: Option<gents_loop::openai_wire::OpenAiWireApi>,
) -> ApplyRequest<'static> {
    let mut patch = vec![
        (
            "name".into(),
            Some(json!(name.unwrap_or_else(|| format!("Local {backend_id}")))),
        ),
        (
            "provider_kind".into(),
            Some(json!(
                gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible
            )),
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
            let backend: gents_documents::InferenceBackend =
                decode_merged("InferenceBackend", &merged)?;
            anyhow::ensure!(
                backend.provider_kind == gents_inference::backends::provider::BackendProviderKind::OpenAiCompatible
                    && matches!(
                        backend.auth,
                        gents_documents::BackendAuth::Unauthenticated
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
pub(super) fn automation_request(
    core: &SelfConfigCore,
    target: SelfConfigTarget,
    id: String,
    patch: SelfConfigPatch,
) -> ApplyRequest<'static> {
    let owner = core.node_did().to_owned();
    let agent = core.agent_did().to_owned();
    let mut request = ApplyRequest::new(target, patch);
    request.allow_create = true;
    request.resolve_unique = Box::new(move |_| Ok(id.clone()));
    request.on_create = Box::new(move |id, merged| {
        merged.insert(target.unique_field().into(), json!(id));
        merged.insert("node_did".into(), json!(owner));
        if target == SelfConfigTarget::Task {
            merged.insert("agent_did".into(), json!(agent));
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
                    merged.get("agent_did").and_then(Value::as_str) == Some(core.agent_did()),
                    "task belongs to agent {:?}, not {:?}; retry with options.agent set to its owner; to run a different agent, create a separate task",
                    merged.get("agent_did").and_then(Value::as_str),
                    core.agent_did()
                );
            }
            // Packs may publish an event source before installing its schema;
            // a collection missing here is a typo that would never fire.
            if target == SelfConfigTarget::EventSource {
                if let Some(collection) = merged.get("source_collection").and_then(Value::as_str) {
                    anyhow::ensure!(
                        gents_documents::collection_is_installed(txn, collection).await?,
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
                        core.agent_did()
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
