use super::*;
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
        let preview = verb == "preview";
        if !preview {
            self.execution.enter_mutation();
        }
        let operation = if preview {
            "self_config.cleanup_preview"
        } else {
            "self_config.cleanup_remove"
        };
        let delete_targets = targets
            .iter()
            .map(|(_, target, id)| (*target, id.clone()))
            .collect::<Vec<_>>();
        let receipt = gents_documents::operations::delete::delete(
            &self.core.access()?,
            operation,
            &gents_documents::operations::delete::DocumentDelete {
                actor: gents_protocol::operations::document::OperationActor::Agent,
                owner: &owner,
                targets: &delete_targets,
                digest: expected_digest.as_deref(),
            },
            &gents_inference::resolved::ProviderProfilePolicy,
            !preview,
        )
        .await
        .map_err(delete_refused)?;
        let plan_digest = receipt.plan_digest;
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

/// A refused delete as the model reads it; a kept reference passes on as the
/// reference owner's error for `failure_guidance`.
fn delete_refused(failure: gents_documents::operations::delete::DeleteFailure) -> anyhow::Error {
    use gents_documents::operations::delete::DeleteError;
    use gents_documents::operations::OperationFailure;
    use gents_protocol::operations::document::DocumentOperationError;
    match failure {
        OperationFailure::Other(error) => error,
        OperationFailure::Operation(DeleteError::Referenced { reference, .. }) => reference.into(),
        OperationFailure::Operation(DeleteError::Document(DocumentOperationError::Missing {
            collection,
            id,
            ..
        })) => {
            anyhow!(
                "no owned {collection} with ID {id:?}; list or inspect exact IDs before cleanup"
            )
        }
        OperationFailure::Operation(DeleteError::Document(DocumentOperationError::Changed {
            expected,
            current,
        })) => {
            anyhow!("cleanup target content changed: preview authorized {expected:?}, current plan is {current:?}; preview again")
        }
        OperationFailure::Operation(DeleteError::Document(error)) => error.into(),
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
