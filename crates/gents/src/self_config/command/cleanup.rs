use super::*;

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
                self.resolve_agent_id(id).await?
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
            .map(|(_, target, id)| (target.collection(), owner.clone(), id.clone()))
            .collect::<Vec<_>>();
        let plan = crate::config_client::DesiredStateApplyPlan::new(Vec::new())?
            .with_removals(removals)?;
        let preview = verb == "preview";
        if !preview {
            self.execution.enter_mutation();
        }
        let operation = if preview {
            "self_config.cleanup_preview"
        } else {
            "self_config.cleanup_remove"
        };
        let identity = Some(self.core.identity()?);
        let (receipt_targets, plan_digest) = if preview {
            crate::config_client::ConfigAccess::transact_local_readonly(
                &self.node,
                identity,
                operation,
                |txn| {
                    Box::pin(cleanup_in_txn(
                        txn,
                        &owner,
                        &targets,
                        expected_digest.as_deref(),
                        &plan,
                        preview,
                    ))
                },
            )
            .await?
        } else {
            crate::config_client::ConfigAccess::transact_local_write(
                &self.node,
                identity,
                operation,
                |txn| {
                    Box::pin(cleanup_in_txn(
                        txn,
                        &owner,
                        &targets,
                        expected_digest.as_deref(),
                        &plan,
                        preview,
                    ))
                },
            )
            .await?
        };
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

fn cleanup_plan_digest(owner: &str, targets: &[Value]) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&json!({
            "owner": owner,
            "targets": targets,
        }))?)
    ))
}

async fn cleanup_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    owner: &str,
    targets: &[(String, SelfConfigTarget, String)],
    expected_digest: Option<&str>,
    plan: &crate::config_client::DesiredStateApplyPlan,
    preview: bool,
) -> Result<(Vec<Value>, String)> {
    let mut receipt = Vec::with_capacity(targets.len());
    for (resource, target, id) in targets {
        let document = ops::read_owned_doc(txn, *target, owner, id)
            .await?
            .map(|(_, document)| document)
            .with_context(|| {
                format!(
                    "no owned {} with ID {id:?}; list or inspect exact IDs before cleanup",
                    target.collection_name()
                )
            })?;
        if *target == SelfConfigTarget::Agent {
            let protected = document
                .get("tags")
                .and_then(Value::as_array)
                .is_some_and(|tags| {
                    tags.iter()
                        .any(|tag| tag.as_str() == Some(super::ENGINEER_AGENT_TAG))
                });
            anyhow::ensure!(
                !protected,
                "target agent {id:?} is the protected Engineer configurator and cannot be removed"
            );
        }
        receipt.push(json!({
            "resource": resource,
            "collection": target.collection_name(),
            "id": id,
            "content_digest": crate::config_client::desired_state_document_digest(&Value::Object(document))?,
        }));
    }
    let plan_digest = cleanup_plan_digest(owner, &receipt)?;
    if let Some(expected) = expected_digest {
        anyhow::ensure!(
            expected == plan_digest,
            "cleanup target content changed: preview authorized {expected:?}, current plan is {plan_digest:?}; preview again"
        );
    }
    crate::config_client::validate_desired_state_plan(txn, plan).await?;
    if !preview {
        crate::config_client::apply_desired_state_plan(txn, plan).await?;
        for (_, target, id) in targets {
            anyhow::ensure!(
                ops::read_owned_doc(txn, *target, owner, id)
                    .await?
                    .is_none(),
                "cleanup did not remove {} {id:?}",
                target.collection_name()
            );
        }
    }
    Ok((receipt, plan_digest))
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
        other => bail!("unknown cleanup resource {other:?}; see [\"help\",\"cleanup\"]"),
    }
}
