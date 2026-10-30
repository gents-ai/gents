use super::*;

impl ConfigCommandTool {
    /// Usage endpoint reads select no account and expose no credential identity.
    pub(super) async fn account_inventory(&self) -> Result<String> {
        use crate::inference::auth::credential::backend_account;
        use crate::inference::backends::provider::BackendProviderOauthExt;
        use crate::inference::usage_observation::{read_node_usage, UsageTrigger};
        let access = crate::config_client::ConfigAccess::local(self.node.clone());
        let node_did = self.node_did.as_str();
        let accounts = crate::inference::auth::credential::list_accounts(&access, node_did).await?;
        let (backends, profiles) = crate::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.account_inventory",
            |txn| {
                Box::pin(async move {
                    Ok::<_, anyhow::Error>((
                        crate::config_client::list_inference_backends_in_txn(txn, node_did).await?,
                        crate::config_client::list_inference_profiles_in_txn(txn, node_did).await?,
                    ))
                })
            },
        )
        .await?;
        let profiles_on = |backend_ids: &[&str]| {
            profiles
                .iter()
                .filter(|profile| backend_ids.contains(&profile.backend_id.as_str()))
                .map(|profile| profile.profile_id.clone())
                .collect::<Vec<_>>()
        };
        let now = chrono::Utc::now();
        let reads = read_node_usage(
            &self.usage_cache,
            self.node.clone(),
            node_did,
            UsageTrigger::Open,
            None,
            self.usage_cache.endpoints(),
            now,
        )
        .await?;
        let mut items = Vec::new();
        for account in &accounts {
            let serving: Vec<_> = backends
                .iter()
                .filter(|backend| {
                    matches!(
                        backend_account(backend, std::slice::from_ref(account)),
                        Some(Some(_))
                    )
                })
                .map(|backend| backend.backend_id.as_str())
                .collect();
            let kind = crate::BackendProviderKind::ALL
                .into_iter()
                .find(|kind| kind.oauth_provider() == Some(account.provider.as_str()));
            let usage = kind.filter(|_| account.enabled).and_then(|_| {
                reads
                    .iter()
                    .find(|read| {
                        read.backend_id.is_none()
                            && read.provider == account.provider
                            && read.account_ref == account.account_ref
                    })
                    .and_then(|read| read.usage.clone())
            });
            items.push(json!({
                "provider": account.provider,
                "label": account.label,
                "state": if account.enabled { "enabled" } else { "disabled" },
                "profiles": profiles_on(&serving),
                "usage": usage,
            }));
        }
        for backend in &backends {
            let state = match backend_account(backend, &accounts) {
                Some(Some(_)) => continue,
                Some(None) => "account not on this node",
                None if backend.enabled => "enabled",
                None => "disabled",
            };
            let usage = (state == "enabled")
                .then(|| {
                    reads
                        .iter()
                        .find(|read| read.matches_backend(backend))
                        .and_then(|read| read.usage.clone())
                })
                .flatten();
            items.push(json!({
                "provider": backend.provider_kind.as_str(),
                "label": backend.name,
                "state": state,
                "profiles": profiles_on(&[backend.backend_id.as_str()]),
                "usage": usage,
            }));
        }
        ordered! {
            "resource": "accounts",
            "items": items,
            "note": "Read-only. Accounts are managed by the operator with `gents accounts`; a profile may keep its account, use an account-free backend or move to another provider's default account, never to another account of one provider. Usage endpoints are read when due; header-only limits use the latest runtime observation.",
        }
        .pretty()
    }

    pub(super) async fn inference_inventory(
        &self,
        target: SelfConfigTarget,
        limit: usize,
        cursor: Option<&str>,
    ) -> Result<String> {
        anyhow::ensure!(
            (1..=50).contains(&limit),
            "--limit must be between 1 and 50"
        );
        let (collection, id_field, fields) = match target {
            SelfConfigTarget::InferenceProfile => (
                "InferenceProfile",
                "profile_id",
                "profile_id display_name description backend_id model_name reasoning_effort service_tier context_window max_output_tokens sampling_id execution_id",
            ),
            SelfConfigTarget::InferenceBackend => (
                "InferenceBackend",
                "backend_id",
                "backend_id name provider_kind openai_wire_api endpoint connect_timeout_secs discovery_timeout_secs max_concurrent max_queue_depth enabled catalogs catalog_freshness probe_status last_probe",
            ),
            _ => (
                target.collection_name(),
                target.unique_field(),
                "",
            ),
        };
        let all_fields = target
            .all_fields()
            .iter()
            .copied()
            .filter(|field| {
                matches!(
                    target,
                    SelfConfigTarget::AgentTarget | SelfConfigTarget::InferenceExecution
                ) || *field == target.unique_field()
                    || [
                        "display_name",
                        "name",
                        "description",
                        "enabled",
                        "tags",
                        "context_id",
                        "tools_id",
                        "inference_profile_id",
                        "backend_id",
                        "model_name",
                        "sampling_id",
                        "execution_id",
                        "retry_policy_id",
                        "compaction_id",
                        "agent_id",
                        "task_id",
                        "source_collection",
                    ]
                    .contains(field)
            })
            .collect::<Vec<_>>()
            .join(" ");
        let fields = if fields.is_empty() {
            all_fields.as_str()
        } else {
            fields
        };
        let owner = escape_graphql_string(&self.node_did);
        let query =
            format!("{{ {collection}(filter: {{node_did: {{_eq: \"{owner}\"}}}}) {{{fields}}} }}");
        let response = crate::config_client::ConfigAccess::transact_local_readonly(
            &self.node,
            Some(self.core.identity()?),
            "self_config.inference_inventory",
            |txn| {
                let query = query.clone();
                Box::pin(async move { txn.execute(&query).await })
            },
        )
        .await?;
        let mut rows = response
            .get("data")
            .and_then(|data| data.get(collection))
            .and_then(Value::as_array)
            .context("inventory query returned no rows array")?
            .clone();
        if target == SelfConfigTarget::InferenceBackend {
            for row in &mut rows {
                let observation: crate::document_config::InferenceBackendObservation =
                    crate::inference::backends::registry::decode_backend_observation(row.clone())
                        .context("decoding backend observation")?;
                let provider_kind = serde_json::from_value(
                    row.get("provider_kind")
                        .cloned()
                        .context("backend row has no provider_kind")?,
                )
                .context("decoding backend provider_kind")?;
                let view = crate::inference::backends::registry::scoped_observation_view(
                    Some(&observation),
                    &self.node_did,
                    provider_kind,
                )?;
                let object = row
                    .as_object_mut()
                    .context("backend row must be an object")?;
                object.remove("catalogs");
                object.remove("catalog_freshness");
                object.insert("catalog".into(), view["catalog"].clone());
            }
        }
        rows.sort_by(|left, right| {
            left.get(id_field)
                .and_then(Value::as_str)
                .cmp(&right.get(id_field).and_then(Value::as_str))
        });
        let total = rows.len();
        let mut selected = rows
            .into_iter()
            .filter(|row| {
                row.get(id_field)
                    .and_then(Value::as_str)
                    .is_some_and(|id| cursor.is_none_or(|cursor| id > cursor))
            })
            .take(limit + 1)
            .collect::<Vec<_>>();
        let truncated = selected.len() > limit;
        selected.truncate(limit);
        if target == SelfConfigTarget::InferenceExecution {
            for row in &mut selected {
                let effective = super::read::execution_settings(Some(row))?;
                row.as_object_mut()
                    .context("execution row must be an object")?
                    .insert("effective".into(), effective);
            }
        }
        let next_cursor = truncated
            .then(|| {
                selected
                    .last()?
                    .get(id_field)?
                    .as_str()
                    .map(ToOwned::to_owned)
            })
            .flatten();
        ordered! {
            "resource": collection,
            "items": selected,
            "page": json!({
                "limit": limit,
                "total": total,
                "returned": selected.len(),
                "truncated": truncated,
                "next_cursor": next_cursor,
            }),
            "note": "Inventory is read-only and does not rebind any agent. Backend credentials are excluded.",
        }
        .pretty()
    }
}
