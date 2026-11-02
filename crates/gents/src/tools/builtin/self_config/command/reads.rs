use super::*;

impl ConfigCommandTool {
    pub(super) async fn bound_read(
        &self,
        core: &SelfConfigCore,
        target: SelfConfigTarget,
    ) -> Result<String> {
        let effective = core
            .read_effective_config(&self.categories, self.no_lockout, self.preview)
            .await?;
        let value = match target {
            SelfConfigTarget::Tools => effective.pointer("/documents/Tools"),
            SelfConfigTarget::InferenceProfile => effective.get("inference_profile"),
            SelfConfigTarget::InferenceBackend => {
                effective.pointer("/documents/InferenceBackend")
            }
            SelfConfigTarget::InferenceSampling => {
                effective.pointer("/documents/InferenceSampling")
            }
            SelfConfigTarget::InferenceExecution => {
                effective.pointer("/documents/InferenceExecution")
            }
            SelfConfigTarget::InferenceRetryPolicy => {
                effective.pointer("/documents/InferenceRetryPolicy")
            }
            SelfConfigTarget::Compaction => effective.pointer("/documents/Compaction"),
            _ => None,
        }
        .with_context(|| {
            format!(
                "selected agent does not reference a {}; inspect config agent get {} to see its exact reference chain",
                target.collection_name(),
                core.agent_id()
            )
        })?;
        if target == SelfConfigTarget::InferenceBackend {
            let observation = self.backend_document_observation(value).await?;
            return ordered! {"resource": target.collection_name(), "document": value, "observation": observation}.pretty();
        }
        if target == SelfConfigTarget::InferenceExecution {
            return ordered! {"resource": target.collection_name(), "effective": super::read::execution_settings(Some(value))?, "document": value}.pretty();
        }
        ordered! {"resource": target.collection_name(), "document": value}.pretty()
    }

    pub(super) async fn backend_observation_view(
        &self,
        backend_id: &str,
        provider_kind: crate::BackendProviderKind,
    ) -> Result<Value> {
        let owner = self.node_did.clone();
        let backend_id = backend_id.to_owned();
        let observation = crate::store::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.backend_observation",
            |txn| {
                let owner = owner.clone();
                let backend_id = backend_id.clone();
                Box::pin(async move {
                    crate::inference::backends::registry::lookup_backend_observation_in_txn(
                        txn,
                        &owner,
                        &backend_id,
                    )
                    .await
                })
            },
        )
        .await?;
        crate::inference::backends::registry::scoped_observation_view(
            observation.as_ref(),
            &self.node_did,
            provider_kind,
        )
    }

    async fn backend_document_observation(&self, document: &Value) -> Result<Value> {
        let backend_id = document
            .get("backend_id")
            .and_then(Value::as_str)
            .context("backend document has no backend_id")?;
        let provider_kind = serde_json::from_value(
            document
                .get("provider_kind")
                .cloned()
                .context("backend document has no provider_kind")?,
        )
        .context("decoding backend provider_kind")?;
        self.backend_observation_view(backend_id, provider_kind)
            .await
    }

    pub(super) async fn exact_read(&self, target: SelfConfigTarget, id: &str) -> Result<String> {
        let id = id.to_owned();
        let lookup_id = id.clone();
        let owner = self.node_did.clone();
        let mut document = crate::store::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.command_read",
            |txn| {
                let owner = owner.clone();
                let lookup_id = lookup_id.clone();
                Box::pin(async move { ops::read_owned_doc(txn, target, &owner, &lookup_id).await })
            },
        )
        .await?
        .map(|(_, document)| document)
        .ok_or(super::ops::MissingConfigDocument { target, id })?;
        if target == SelfConfigTarget::InferenceBackend {
            document.remove("auth");
            document.insert(
                "auth".into(),
                json!({"redacted": true, "owner": "operator credential/login flow"}),
            );
            let document = Value::Object(document);
            let observation = self.backend_document_observation(&document).await?;
            return ordered! {"resource": target.collection_name(), "document": document, "observation": observation}.pretty();
        }
        if target == SelfConfigTarget::AgentTarget {
            return ordered! {"resource": target.collection_name(), "connection": ops::target_destination(&document, &self.node_did), "document": document}.pretty();
        }
        if target == SelfConfigTarget::InferenceExecution {
            let document = Value::Object(document);
            return ordered! {"resource": target.collection_name(), "effective": super::read::execution_settings(Some(&document))?, "document": document}.pretty();
        }
        ordered! {"resource": target.collection_name(), "document": document}.pretty()
    }

    pub(super) async fn automation_read(
        &self,
        core: &SelfConfigCore,
        target: SelfConfigTarget,
        id: &str,
    ) -> Result<String> {
        let effective = core
            .read_effective_config(&self.categories, self.no_lockout, self.preview)
            .await?;
        let document = effective
            .pointer(&format!("/automation/{}", target.collection_name()))
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.get(target.unique_field()).and_then(Value::as_str) == Some(id))
            })
            .with_context(|| {
                format!(
                    "no {} {id:?} is reachable from selected agent {:?}; use options.agent to select its owning agent, then get to read its automation IDs",
                    target.collection_name(),
                    core.agent_id()
                )
            })?;
        ordered! {"resource": target.collection_name(), "document": document}.pretty()
    }
}
