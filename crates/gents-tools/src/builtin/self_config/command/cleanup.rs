use super::*;
use gents_documents::operations::delete;
use gents_protocol::operations::document::{
    DocumentActor, DocumentDeleteError, DocumentDeletePreview,
};
use gents_session::session::output_render::ordered;

impl ConfigCommandTool {
    pub(super) async fn cleanup(&self, argv: &[String]) -> Result<String> {
        let verb = argv
            .first()
            .map(String::as_str)
            .context("cleanup requires preview or remove; see [\"help\",\"cleanup\"]")?;
        anyhow::ensure!(
            matches!(verb, "preview" | "remove"),
            "unknown cleanup command {verb:?}; accepted: preview, remove"
        );
        if verb == "preview" {
            anyhow::ensure!(
                self.preview,
                "cleanup preview is not granted for this agent"
            );
        }
        let parsed = ParsedArgs::parse(&argv[1..])?;
        anyhow::ensure!(
            parsed.positionals.is_empty() && parsed.switches.is_empty(),
            "cleanup accepts only repeated --target RESOURCE=ID"
        );
        for option in parsed.options.keys() {
            anyhow::ensure!(
                matches!(option.as_str(), "target" | "digest"),
                "unknown cleanup option --{option}; accepted: --target RESOURCE=ID, --digest SHA256"
            );
        }
        let expected_digest = parsed.one("digest")?.map(ToOwned::to_owned);
        if verb == "preview" {
            anyhow::ensure!(
                expected_digest.is_none(),
                "cleanup preview does not accept --digest; it returns the digest to authorize remove"
            );
        } else {
            anyhow::ensure!(
                expected_digest.is_some(),
                "cleanup remove requires --digest from cleanup preview"
            );
        }
        let values = parsed
            .options
            .get("target")
            .context("cleanup requires at least one --target RESOURCE=ID")?;
        let mut targets = Vec::with_capacity(values.len());
        let mut seen = BTreeSet::new();
        for value in values {
            let (resource, id) = value
                .split_once('=')
                .context("--target must be RESOURCE=ID")?;
            anyhow::ensure!(
                !id.trim().is_empty(),
                "cleanup target {resource:?} requires a non-empty ID"
            );
            let target = cleanup_target(resource)?;
            let id = if target == SelfConfigTarget::Agent {
                self.resolve_agent_did(id).await?
            } else {
                id.to_owned()
            };
            let id = id.as_str();
            if matches!(
                target,
                SelfConfigTarget::Agent | SelfConfigTarget::AgentContext
            ) {
                anyhow::ensure!(
                    self.categories.contains("node"),
                    "{resource} cleanup requires the agent catalog grant (category node)"
                );
            } else {
                self.ensure_resource(match target.category() {
                    "mcp_service" => "mcp-service",
                    category => category,
                })?;
            }
            anyhow::ensure!(
                seen.insert((target.collection_name(), id.to_owned())),
                "duplicate cleanup target {resource}={id}"
            );
            targets.push((resource.to_owned(), target, id.to_owned()));
        }
        targets.sort_by(|left, right| {
            (left.1.collection_name(), left.2.as_str())
                .cmp(&(right.1.collection_name(), right.2.as_str()))
        });

        let owner = self.node_did.clone();
        let removals = targets
            .iter()
            .map(|(_, target, id)| (target.collection(), id.clone()))
            .collect::<Vec<_>>();
        let preview = verb == "preview";
        if !preview {
            self.execution.enter_mutation();
        }
        let access = self.core.access()?;
        let policy = gents_inference::resolved::ProviderProfilePolicy;
        let receipt = if preview {
            match delete::preview_delete(
                &access,
                "self_config.cleanup_preview",
                &owner,
                &removals,
                DocumentActor::Agent,
                &policy,
            )
            .await
            .map_err(|error| delete_failure(error, &targets))?
            {
                DocumentDeletePreview::Admitted(receipt) => receipt,
                DocumentDeletePreview::Refused { refusal, error } => {
                    let guidance = delete_guidance(&error, &targets);
                    return ordered! {
                        "admitted": false,
                        "committed": false,
                        "refusal": refusal.as_str(),
                        "message": guidance.message,
                        "next_call": guidance.next_call,
                    }
                    .pretty();
                }
            }
        } else {
            delete::delete(
                &access,
                "self_config.cleanup_remove",
                &owner,
                &removals,
                DocumentActor::Agent,
                expected_digest.as_deref(),
                &policy,
            )
            .await
            .map_err(|error| delete_failure(error, &targets))?
        };
        let receipt_targets = receipt
            .targets
            .iter()
            .zip(&targets)
            .map(|(deleted, (resource, _, _))| {
                json!({
                    "resource": resource,
                    "collection": deleted.collection,
                    "id": deleted.id,
                    "content_digest": deleted.content_digest,
                })
            })
            .collect::<Vec<_>>();
        let plan_digest = receipt.plan_digest;
        ordered! {
            "committed": !preview,
            "operation": if preview { "preview cleanup" } else { "cleanup" },
            "targets": receipt_targets,
            "effect": if preview {
                "No documents were changed. Repeat the same exact targets and plan digest with cleanup remove to revalidate and apply atomically."
            } else {
                "The exact target set was removed atomically after retained-reference validation."
            },
            "apply_with": preview.then(|| json!({
                "argv_prefix": ["cleanup", "remove", "--digest", plan_digest],
                "repeat_targets": targets.iter().map(|(resource, _, id)| format!("{resource}={id}")).collect::<Vec<_>>(),
            })),
            "plan_digest": plan_digest,
            "owner": self.node_did,
        }
        .pretty()
    }
}

fn delete_guidance(
    error: &DocumentDeleteError,
    targets: &[(String, SelfConfigTarget, String)],
) -> CommandGuidance {
    let message = match error {
        DocumentDeleteError::StillReferenced {
            target, target_id, ..
        } => format!(
            "Deletion would leave a broken reference to {target} {target_id:?}. Update the referring document to remove or redirect the reference, then retry deletion. Cause: {error}"
        ),
        error => gents_protocol::error_chain::ErrorChain(error).to_string(),
    };
    let requested = |collection: &str| {
        targets
            .iter()
            .find(|(_, target, _)| target.collection().graphql_type() == collection)
            .map(|(resource, _, _)| resource.as_str())
    };
    let next_call = match error {
        DocumentDeleteError::TargetMissing { collection, .. } => {
            requested(collection).map(|resource| json!({"argv":[resource,"list"]}))
        }
        DocumentDeleteError::StillReferenced { collection, id, .. } => HELP_INDEX
            .iter()
            .find(|(resource, _)| {
                cleanup_target(resource)
                    .is_ok_and(|target| target.collection().graphql_type() == *collection)
            })
            .map(|(resource, _)| json!({"argv":[resource,"get"],"target_id":id})),
        _ => None,
    }
    .unwrap_or_else(|| json!({"argv":["help","cleanup"]}));
    CommandGuidance { message, next_call }
}

fn delete_failure(
    error: DocumentDeleteError,
    targets: &[(String, SelfConfigTarget, String)],
) -> anyhow::Error {
    match error {
        DocumentDeleteError::Passthrough(error) => anyhow::anyhow!(error),
        error => delete_guidance(&error, targets).into(),
    }
}

pub(super) fn cleanup_target(name: &str) -> Result<SelfConfigTarget> {
    match name {
        "agent" => Ok(SelfConfigTarget::Agent),
        "context" => Ok(SelfConfigTarget::AgentContext),
        "tools" => Ok(SelfConfigTarget::Tools),
        "agent-target" => Ok(SelfConfigTarget::AgentTarget),
        "profile" => Ok(SelfConfigTarget::InferenceProfile),
        "sampling" => Ok(SelfConfigTarget::InferenceSampling),
        "execution" => Ok(SelfConfigTarget::InferenceExecution),
        "retry-policy" => Ok(SelfConfigTarget::InferenceRetryPolicy),
        "compaction" => Ok(SelfConfigTarget::Compaction),
        "backend" => Ok(SelfConfigTarget::InferenceBackend),
        "mcp-service" => Ok(SelfConfigTarget::ToolServiceRegistry),
        "task" => Ok(SelfConfigTarget::Task),
        "schedule" => Ok(SelfConfigTarget::Schedule),
        "trigger" => Ok(SelfConfigTarget::Trigger),
        "event-source" => Ok(SelfConfigTarget::EventSource),
        "datastore" => Ok(SelfConfigTarget::DatastoreToolSurface),
        "skill" => Ok(SelfConfigTarget::Skill),
        "page" => Ok(SelfConfigTarget::Page),
        "plugin-source" => Ok(SelfConfigTarget::PluginSource),
        other => bail!("unknown cleanup resource {other:?}; see [\"help\",\"cleanup\"]"),
    }
}
