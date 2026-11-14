use super::*;
use gents_session::session::output_render::ordered;

impl ConfigCommandTool {
    /// Usage endpoint reads select no account and expose no credential identity.
    pub(super) async fn account_inventory(&self) -> Result<String> {
        use gents_inference::auth::credential::backend_account;
        use gents_inference::backends::provider::BackendProviderOauthExt;
        use gents_inference::usage_observation::{read_node_usage, UsageTrigger};
        let node_did = self.node_did.as_str();
        let access =
            gents_inference::auth::credential::runtime_credential_access(&self.node, node_did)?;
        let accounts = gents_inference::auth::credential::list_accounts(&access, node_did).await?;
        let (backends, profiles) =
            gents_store::config_client::ConfigAccess::transact_local_readonly(
                &self.node,
                Some(self.core.identity()?),
                "self_config.account_inventory",
                |txn| {
                    Box::pin(async move {
                        let backends = txn
                            .read_view(&gents_inference::views::backends::BackendList::new(
                                node_did,
                            ))
                            .await?
                            .into_iter()
                            .map(|row| row.metadata.into_backend().map_err(anyhow::Error::from))
                            .collect::<Result<Vec<_>>>()?;
                        let profiles: Vec<_> = txn
                            .read_view(&gents_inference::views::profiles::ProfileList::new(
                                node_did,
                            ))
                            .await?
                            .into_iter()
                            .map(|row| row.profile)
                            .collect();
                        Ok::<_, anyhow::Error>((backends, profiles))
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
            let kind = gents_inference::backends::provider::BackendProviderKind::ALL
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
        if matches!(
            target,
            SelfConfigTarget::Task
                | SelfConfigTarget::Trigger
                | SelfConfigTarget::Schedule
                | SelfConfigTarget::EventSource
        ) {
            return self.automation_inventory(target, limit, cursor).await;
        }
        if target == SelfConfigTarget::ToolServiceRegistry {
            return self.mcp_inventory(limit, cursor).await;
        }
        if matches!(
            target,
            SelfConfigTarget::InferenceProfile
                | SelfConfigTarget::InferenceBackend
                | SelfConfigTarget::InferenceSampling
                | SelfConfigTarget::InferenceExecution
                | SelfConfigTarget::InferenceRetryPolicy
        ) {
            return self.typed_inference_inventory(target, limit, cursor).await;
        }
        let (mut selected, total) =
            gents_store::config_client::ConfigAccess::transact_local_readonly(
                &self.node,
                Some(self.core.identity()?),
                "self_config.configuration_inventory",
                |txn| {
                    Box::pin(async move {
                        macro_rules! read {
                            ($view:expr) => {{
                                let all = txn.read_view(&$view).await?;
                                let total = all.len();
                                let mut view = $view.limit((limit + 1) as u64);
                                if let Some(cursor) = cursor {
                                    view = view.after(cursor);
                                }
                                let rows = txn.read_view(&view).await?;
                                let rows = rows
                                    .into_iter()
                                    .map(serde_json::to_value)
                                    .collect::<std::result::Result<Vec<_>, _>>()?;
                                Ok::<_, anyhow::Error>((rows, total))
                            }};
                        }
                        use gents_documents::views::config;
                        match target {
                            SelfConfigTarget::Agent => read!(
                                gents_documents::views::agents::AgentList::new(&self.node_did)
                            ),
                            SelfConfigTarget::AgentContext => {
                                read!(config::AgentContextList::new(&self.node_did))
                            }
                            SelfConfigTarget::Compaction => {
                                read!(config::CompactionConfigList::new(&self.node_did))
                            }
                            SelfConfigTarget::Tools => {
                                read!(config::ToolsList::new(&self.node_did))
                            }
                            SelfConfigTarget::AgentTarget => {
                                read!(config::AgentTargetList::new(&self.node_did))
                            }
                            SelfConfigTarget::DatastoreToolSurface => {
                                read!(config::DatastoreToolSurfaceList::new(&self.node_did))
                            }
                            SelfConfigTarget::Skill => {
                                read!(config::SkillList::new(&self.node_did))
                            }
                            SelfConfigTarget::Page => read!(config::PageList::new(&self.node_did)),
                            SelfConfigTarget::PluginSource => {
                                read!(config::PluginSourceList::new(&self.node_did))
                            }
                            _ => unreachable!(
                                "specialized inventory dispatched before configuration views"
                            ),
                        }
                    })
                },
            )
            .await?;
        let truncated = selected.len() > limit;
        selected.truncate(limit);
        if target != SelfConfigTarget::AgentTarget {
            for row in &mut selected {
                if let Some(fields) = row.as_object_mut() {
                    fields.retain(|field, _| {
                        field == target.unique_field()
                            || matches!(
                                field.as_str(),
                                "display_name"
                                    | "name"
                                    | "description"
                                    | "enabled"
                                    | "tags"
                                    | "context_id"
                                    | "tools_id"
                                    | "inference_profile_id"
                                    | "backend_id"
                                    | "model_name"
                                    | "sampling_id"
                                    | "execution_id"
                                    | "retry_policy_id"
                                    | "compaction_id"
                                    | "agent_did"
                                    | "task_id"
                                    | "source_collection"
                            )
                    });
                }
            }
        }
        let next_cursor = truncated
            .then(|| {
                selected
                    .last()
                    .and_then(|row| row.get(target.unique_field()))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .flatten();
        ordered! {
            "resource": target.collection_name(),
            "items": selected,
            "page": json!({
                "limit": limit,
                "total": total,
                "returned": selected.len(),
                "truncated": truncated,
                "next_cursor": next_cursor,
            }),
            "note": "Inventory is read-only and does not rebind any agent.",
        }
        .pretty()
    }
}
