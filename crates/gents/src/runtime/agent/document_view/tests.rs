use std::sync::Arc;

use super::*;
use crate::runtime::agent::{DocumentResolveContext, ResolvedRuntimeSnapshot};
use gents_inference::test_support::document_fixtures::*;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_tools::surface::ToolCeiling;

struct DocumentViewEnv {
    _db: gents_store::test_support::TestStore,
    user: KeyIdentity,
    access: ConfigAccess,
    node: Arc<defra_node::Node>,
    identity: Arc<dyn NodeIdentity>,
    agent_did: String,
    resolve_context: DocumentResolveContext,
}

impl DocumentViewEnv {
    async fn new(name: &str) -> Self {
        let signed_home = gents_documents::test_support::SignedHome::enroll(
            crate::support::trial_store(name).await,
            name,
        )
        .await;
        let binding = gents_inference::test_support::default_agent(&signed_home).await;
        let (db, user, access) = signed_home.into_parts();
        let node = db.node.clone();
        let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
        let resolve_context = DocumentResolveContext::for_tests(
            identity.clone(),
            ToolCeiling::readonly(),
            gents_inference::backends::health::BackendHealthMap::new(),
        );
        Self {
            _db: db,
            user,
            access,
            node,
            identity,
            agent_did: binding.agent_did,
            resolve_context,
        }
    }

    fn node_did(&self) -> &str {
        self.identity.did()
    }

    async fn load_view(&self) -> DocumentRuntimeView {
        load_document_runtime_view(&self.node, self.node_did())
            .await
            .expect("document view should load")
    }

    async fn resolve_view(&self, view: &DocumentRuntimeView) -> ResolvedRuntimeSnapshot {
        resolve_document_runtime_snapshot_from_view(&self.node, &self.resolve_context, view)
            .await
            .expect("resolve should succeed")
    }

    async fn resolve(&self) -> ResolvedRuntimeSnapshot {
        let view = self.load_view().await;
        self.resolve_view(&view).await
    }

    async fn bind_subscription(&self, provider: &str, endpoint: &str, model: &str) {
        use gents_documents::{
            apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
            DesiredStateApplyPlan,
        };
        let node_did = self.node_did();
        let agent_did = self.agent_did.as_str();
        self.access
            .transact_write("test.subscription_backend", |txn| {
                Box::pin(async move {
                    let mut documents = Vec::new();
                    for (collection, id, patch) in [
                        (
                            Collection::InferenceBackend,
                            format!("{agent_did}:backend"),
                            serde_json::json!({"provider_kind": provider, "endpoint": endpoint, "auth": {"kind": "node_oauth"}}),
                        ),
                        (
                            Collection::InferenceProfile,
                            format!("{agent_did}:inference"),
                            serde_json::json!({"model_name": model}),
                        ),
                    ] {
                        let (_, mut value) =
                            read_desired_state_record_in_txn(txn, collection, node_did, &id)
                                .await?
                                .expect("installed canonical component");
                        value
                            .as_object_mut()
                            .unwrap()
                            .extend(patch.as_object().unwrap().clone());
                        documents.push(DesiredStateApplyDocument {
                            collection,
                            add: value.clone(),
                            update: value,
                        });
                    }
                    apply_desired_state_plan(
                        txn,
                        &DesiredStateApplyPlan::new(documents)?,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }

    async fn bind_chatgpt(&self) {
        self.bind_subscription(
            "ChatGptCodex",
            "https://chatgpt.com/backend-api/codex",
            "gpt-5.2",
        )
        .await;
    }

    async fn bind_claude(&self) {
        self.bind_subscription(
            "ClaudeCliSubscription",
            gents_inference::clients::claude_subscription::default_backend_endpoint(),
            "default",
        )
        .await;
    }

    async fn upsert_chatgpt_credential(&self) -> String {
        let credential = gents_inference::auth::credential::OAuthCredential {
            doc_id: None,
            credential_id: gents_inference::auth::credential::oauth_credential_id(
                self.node_did(),
                gents_inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER,
            ),
            node_did: self.node_did().to_string(),
            provider: gents_inference::clients::chatgpt_codex::CHATGPT_CODEX_PROVIDER.to_string(),
            access_token: "access-token".to_string(),
            refresh_token: "refresh-token".to_string(),
            id_token: None,
            account_id: None,
            chatgpt_plan_type: None,
            is_fedramp: false,
            access_token_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
            last_refresh: None,
            enabled: true,
            account_ref: None,
            connected_at: None,
            provider_account_key: None,
            label: None,
        };
        gents_inference::auth::credential::upsert_oauth_credential(&self.node, &credential)
            .await
            .expect("publish OAuthCredential")
    }

    async fn sign_in_claude(&self, who: &str) -> gents_inference::auth::credential::SignIn {
        let credential = gents_inference::auth::claude::credential_from_login_tokens(
            self.node_did().to_string(),
            gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
            &gents_inference::auth::claude::ClaudeLoginTokens {
                access_token: format!("access-{who}"),
                refresh_token: format!("refresh-{who}"),
                expires_in: Some(3600),
                scope: None,
                account_id: Some(format!("label-{who}")),
                organization_uuid: Some("org-1".into()),
                account_uuid: Some(format!("account-{who}")),
            },
            chrono::Utc::now(),
        );
        gents_inference::auth::credential::store_sign_in(&self.access, credential, None)
            .await
            .unwrap()
    }

    async fn remove_account(&self, credential_id: &str) {
        let did = self.node_did().to_string();
        let credential_id = credential_id.to_string();
        self.access
            .transact_write("test.remove_account", |txn| {
                let did = did.clone();
                let credential_id = credential_id.clone();
                Box::pin(async move {
                    gents_inference::auth::credential::remove_account_in_txn(
                        txn,
                        &did,
                        &credential_id,
                    )
                    .await
                })
            })
            .await
            .unwrap();
    }

    async fn resolve_with_auth(
        &self,
        auth: gents_documents::BackendAuth,
    ) -> ResolvedRuntimeSnapshot {
        let mut view = self.load_view().await;
        for backend in view.backends.values_mut() {
            backend.value.auth = auth.clone();
        }
        self.resolve_view(&view).await
    }
}

#[tokio::test]
async fn runtime_view_uses_reader_grant_and_hides_unlinked_agents() -> anyhow::Result<()> {
    use gents_store::config_client::NodePermission;

    let env = DocumentViewEnv::new("runtime-view-reader").await;
    let reader = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("runtime-view-reader-actor")
    })
    .await?;
    let reader_did = identity::Did::new(reader.did())?;
    let admin = ConfigAccess::local_as(env.node.clone(), identity::Did::new(env.node_did())?);
    admin
        .grant_node_permission(
            &identity::Did::new(env.user.did())?,
            NodePermission::DacRelationDelete,
        )
        .await?;
    for permission in [NodePermission::CollectionGet, NodePermission::DocumentRead] {
        admin.grant_node_permission(&reader_did, permission).await?;
    }
    let node_doc = find_owned_doc(&env.access, "Node", "node_did", env.node_did()).await;
    let agent_doc = find_owned_doc(&env.access, "Agent", "agent_did", &env.agent_did).await;
    env.access
        .delete_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await?;

    assert!(
        load_document_runtime_view_as(&env.node, env.node_did(), reader.did())
            .await
            .is_err()
    );
    env.access
        .add_actor_relationship("Node", &node_doc, "runtime_reader", &reader_did)
        .await?;
    let unlinked = load_document_runtime_view_as(&env.node, env.node_did(), reader.did()).await?;
    assert!(unlinked.agents.is_empty());
    env.access
        .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
        .await?;
    let linked = load_document_runtime_view_as(&env.node, env.node_did(), reader.did()).await?;
    assert!(linked.agents.contains_key(&env.agent_did));
    env.access
        .delete_actor_relationship("Node", &node_doc, "runtime_reader", &reader_did)
        .await?;
    assert!(
        load_document_runtime_view_as(&env.node, env.node_did(), reader.did())
            .await
            .is_err()
    );
    env.node.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn existing_document_runtime_view_does_not_wait_for_mutation_gate() {
    let env = DocumentViewEnv::new("document-view-mutation-gate").await;
    let principal = gents_documents::ensure_node(&env.node, env.node_did())
        .await
        .unwrap();
    seed_automation(
        &env.access,
        vec![(
            Collection::Skill,
            serde_json::json!({
                "skill_id": "runtime-skill", "node_did": env.node_did(), "name": "Runtime skill",
                "instructions": "Loaded through the snapshot.", "enabled": true,
            }),
        )],
    )
    .await;
    let skill_doc_id = find_owned_doc(&env.access, "Skill", "skill_id", "runtime-skill").await;
    let held = gents_store::config_client::ConfigApplyTxn::begin_local(&env.node, None)
        .await
        .unwrap();
    let view = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        load_document_runtime_view(&env.node, env.node_did()),
    )
    .await
    .expect("runtime configuration reads must not wait for the mutation gate")
    .expect("existing node runtime view");
    assert_eq!(view.node.value, principal);
    assert_eq!(view.skills["runtime-skill"].doc_id, skill_doc_id);
    assert_eq!(
        view.skills["runtime-skill"].value.name.as_deref(),
        Some("Runtime skill")
    );
    held.discard().await.unwrap();
    env.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn load_document_runtime_view_includes_referenced_documents() {
    let env = DocumentViewEnv::new("document-view-load").await;
    let view = env.load_view().await;

    assert_eq!(view.node.value.node_did, env.node_did());
    assert_eq!(view.default_agent_did(), Some(env.agent_did.as_str()));
    assert!(view.agents.contains_key(&env.agent_did));
    let agent_config = &view.agents[&env.agent_did].value;
    let context_id = agent_config
        .context_id
        .as_deref()
        .expect("context reference");
    assert!(view.contexts.contains_key(context_id));
    let context = &view.contexts[context_id].value;
    let tools_id = context.tools_id.as_deref().expect("tools reference");
    assert!(view.tools.contains_key(tools_id));
    let profile = &view.inference_profiles[&agent_config.inference_profile_id].value;
    assert_eq!(profile.backend_id, format!("{}:backend", env.agent_did));
    assert!(view
        .backends
        .contains_key(&format!("{}:backend", env.agent_did)));
}

#[tokio::test]
async fn apply_control_update_reconciles_tool_selection_via_doc_id() {
    let env = DocumentViewEnv::new("document-view-update").await;
    let mut view = env.load_view().await;

    let tools_id = format!("{}:tools", env.agent_did);
    let tools_doc_id = view.tools[&tools_id].doc_id.clone();
    let mut tools_doc = view.tools[&tools_id].value.clone();
    tools_doc.host.get_or_insert_with(Default::default).files = Some(gents_documents::FileTools {
        mode: gents_documents::FileToolMode::ReadOnly,
        ..Default::default()
    });
    gents_documents::write_tools_document(
        &env.access,
        &tools_doc,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    .expect("write read-only Tools patch");

    let behavior_doc_id =
        gents_documents::load_agent_record(env.node.as_ref(), env.node_did(), &env.agent_did)
            .await
            .unwrap()
            .expect("agent record")
            .0;

    assert!(
        apply_control_update(env.node_did(), "Tools", &tools_doc_id, &mut view)
            .await
            .is_ok_and(|outcome| outcome == ControlUpdateOutcome::FullReload)
    );
    assert!(
        apply_control_update(env.node_did(), "Agent", &behavior_doc_id, &mut view)
            .await
            .is_ok_and(|outcome| outcome == ControlUpdateOutcome::FullReload)
    );

    let reloaded = env.load_view().await;
    assert_eq!(
        reloaded.tools[&tools_id]
            .value
            .host
            .as_ref()
            .and_then(|host| host.files.as_ref())
            .map(|files| files.mode),
        Some(gents_documents::FileToolMode::ReadOnly),
        "reloaded view must observe the read-only file-tools patch"
    );
    view = reloaded;

    let snapshot = env.resolve_view(&view).await;
    let resolved = snapshot.agents.get(&env.agent_did).expect("resolved agent");
    let gents_inference::resolved::ResolvedConfiguration::Documents(selected) =
        &resolved.configuration
    else {
        panic!("managed agents require document-bound configuration");
    };
    let context_id = view.agents[&env.agent_did]
        .value
        .context_id
        .as_ref()
        .expect("selected context");
    let profile_id = &view.agents[&env.agent_did].value.inference_profile_id;
    let backend_id = &view.inference_profiles[profile_id].value.backend_id;
    // The selection carries the inference backend between profile and context,
    // matching `append_inference_selection` in document_view/snapshot.rs.
    let expected = [
        &view.agents[&env.agent_did].doc_id,
        &view.inference_profiles[profile_id].doc_id,
        &view.backends[backend_id].doc_id,
        &view.contexts[context_id].doc_id,
        &view.tools[&tools_id].doc_id,
    ];
    assert_eq!(selected.actor.as_str(), env.agent_did);
    assert_eq!(
        selected
            .documents
            .iter()
            .map(|doc| &doc.doc_id)
            .collect::<Vec<_>>(),
        expected
    );
    for scope in [
        gents_loop::rendered_request::CaptureScopeKind::Inference,
        gents_loop::rendered_request::CaptureScopeKind::Title,
        gents_loop::rendered_request::CaptureScopeKind::AccessReview,
        gents_loop::rendered_request::CaptureScopeKind::Compaction,
    ] {
        assert!(
            crate::turn::completion_factory::loop_config(resolved, String::new(), 0, scope)
                .configuration_guard
                .is_some()
        );
    }
    let tool_surface = snapshot
        .tool_surfaces
        .get(&env.agent_did)
        .expect("tool surface for default agent");
    let tool_names = tool_surface.tool_names();
    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(tool_names.contains(&"list_files".to_string()));
}

#[tokio::test]
async fn resolve_composes_explicitly_selected_skill_into_prompt() {
    use crate::turn::prompt::LayeredPromptBuilder;
    use gents_loop::tool::Tool;

    let env = DocumentViewEnv::new("document-view-skill").await;
    seed_automation(
        &env.access,
        vec![(
            Collection::Skill,
            serde_json::json!({
                "skill_id": "skill-research", "node_did": env.node_did(),
                "name": "Research", "description": "Find and cite sources",
                "instructions": "Always cite your sources.",
                "tool_refs": ["read_file", "definitely_not_a_tool"],
                "covers": ["config trigger"], "enabled": true,
            }),
        )],
    )
    .await;
    patch_desired_document(
        &env.access,
        Collection::AgentContext,
        env.node_did(),
        &format!("{}:context", env.agent_did),
        serde_json::json!({"skill_ids": ["skill-research"]}),
    )
    .await;

    let snapshot = env.resolve().await;
    let agent_config = snapshot
        .agents
        .get(&env.agent_did)
        .expect("resolved default agent");
    assert_eq!(
        agent_config.skills.len(),
        1,
        "explicitly selected skill must be available to the agent"
    );
    assert_eq!(agent_config.skills[0].skill_id, "skill-research");
    assert_eq!(agent_config.skills[0].covers, ["config trigger"]);

    let tool_surface = snapshot
        .tool_surfaces
        .get(&env.agent_did)
        .expect("tool surface");
    let preamble = LayeredPromptBuilder::new(agent_config.as_ref(), tool_surface.as_ref(), &[])
        .preamble()
        .to_string();
    assert!(
        preamble.contains("Research"),
        "catalog lists the skill name: {preamble}"
    );
    assert!(
        preamble.contains("Find and cite sources"),
        "catalog lists the skill description: {preamble}"
    );
    assert!(
        preamble.contains("load_skill"),
        "catalog directs the model to load_skill"
    );
    assert!(
        !preamble.contains("Always cite your sources."),
        "skill BODY must NOT be in the catalog (loaded on demand): {preamble}"
    );

    let ceiling =
        gents_documents::skills::skill_tool_ceiling(tool_surface.tool_names(), &[], false);
    let load_skill =
        gents_tools::native::skills::LoadSkillTool::new(agent_config.skills.clone(), ceiling);
    let loaded = load_skill
        .call(
            gents_tools::native::skills::LoadSkillArgs {
                name: "Research".to_string(),
            },
            &gents_loop::tool::ToolCallEnvelope::default(),
        )
        .await
        .expect("load_skill");
    assert!(
        loaded.contains("Always cite your sources."),
        "load_skill returns the full body: {loaded}"
    );
    assert!(
        loaded.contains("definitely_not_a_tool"),
        "load_skill body carries the degrade note for the ungranted tool_ref: {loaded}"
    );
}

#[tokio::test]
async fn apply_control_update_hot_reloads_skill() {
    let env = DocumentViewEnv::new("document-view-skill-reload").await;
    grant_delete_permissions(&env.node, env.identity.as_ref(), &env.user).await;

    let mut view = env.load_view().await;
    assert!(view.skills.is_empty(), "no skills before create");

    seed_automation(
        &env.access,
        vec![(
            Collection::Skill,
            serde_json::json!({
                "skill_id": "s-reload", "node_did": env.node_did(),
                "name": "Reload", "instructions": "Reload me.", "enabled": true,
            }),
        )],
    )
    .await;
    let doc_id = find_owned_doc(&env.access, "Skill", "skill_id", "s-reload").await;

    let outcome = apply_control_update(env.node_did(), "Skill", &doc_id, &mut view)
        .await
        .expect("apply skill create");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let mut view = env.load_view().await;
    assert!(view.skills.contains_key("s-reload"), "skill added to view");

    let foreign = tokio::task::spawn_blocking(|| {
        gents_store::test_support::test_identity("document-view-foreign-node")
    })
    .await
    .unwrap();
    gents_documents::test_support::bootstrap_signed_test_node(&env.access, foreign.did()).await;
    seed_automation(
        &env.access,
        vec![(
            Collection::Skill,
            serde_json::json!({
                "skill_id": "s-foreign", "node_did": foreign.did(), "name": "F", "enabled": true,
            }),
        )],
    )
    .await;
    let foreign_doc_id = find_owned_doc(&env.access, "Skill", "skill_id", "s-foreign").await;
    let outcome = apply_control_update(env.node_did(), "Skill", &foreign_doc_id, &mut view)
        .await
        .expect("apply foreign skill");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    view = env.load_view().await;
    assert!(!view.skills.contains_key("s-foreign"));

    delete_signed_document(&env.access, Collection::Skill, &doc_id).await;
    let outcome = apply_control_update(env.node_did(), "Skill", &doc_id, &mut view)
        .await
        .expect("apply skill delete");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let view = env.load_view().await;
    assert!(
        !view.skills.contains_key("s-reload"),
        "skill removed from view"
    );
}

async fn seed_raw_tools(access: &ConfigAccess, tools_id: &str, agent_target_ids: &[&str]) {
    let doc = find_owned_doc(access, "Tools", "tools_id", tools_id).await;
    update_signed_document(
        access,
        Collection::Tools,
        &doc,
        serde_json::json!({"agents": {"target_ids": agent_target_ids, "enabled": true}}),
    )
    .await;
}

#[tokio::test]
async fn resolve_quarantines_behavior_with_empty_agent_target_id() {
    let env = DocumentViewEnv::new("document-view-invalid-tool-selection").await;
    seed_raw_tools(&env.access, &format!("{}:tools", env.agent_did), &[""]).await;

    let snapshot = env.resolve().await;
    assert!(
        !snapshot.agents.contains_key(&env.agent_did),
        "agent with a blank agent target id must not be active"
    );
    let reason = snapshot
        .unavailable_agents
        .get(&env.agent_did)
        .expect("agent should be quarantined");
    assert!(
        reason.diagnostic.contains("target_ids"),
        "quarantine reason must name the invalid agent target ids, got: {}",
        reason.diagnostic
    );
}

#[tokio::test]
async fn resolve_quarantines_behavior_with_missing_local_agent_target() {
    let env = DocumentViewEnv::new("document-view-missing-agent-target").await;
    seed_raw_tools(
        &env.access,
        &format!("{}:tools", env.agent_did),
        &["target-missing-agent"],
    )
    .await;
    seed_automation(
        &env.access,
        vec![(
            Collection::AgentTarget,
            serde_json::json!({
                "target_id": "target-missing-agent", "node_did": env.node_did(),
                "target_node_did": env.node_did(), "agent_did": "missing-agent", "name": "missing",
            }),
        )],
    )
    .await;

    let snapshot = env.resolve().await;
    assert!(
        !snapshot.agents.contains_key(&env.agent_did),
        "agent with a missing local agent target must not be active"
    );
    let reason = snapshot
        .unavailable_agents
        .get(&env.agent_did)
        .expect("agent should be quarantined");
    assert!(
        reason.diagnostic.contains("missing-agent") && reason.diagnostic.contains("agents"),
        "quarantine reason must name the missing target agent, got: {}",
        reason.diagnostic
    );
}

async fn create_task_bound(
    access: &ConfigAccess,
    node_did: &str,
    task_id: &str,
    agent_did: &str,
    prompt_template: &str,
    enabled: bool,
) {
    seed_automation(
        access,
        vec![(
            Collection::Task,
            serde_json::json!({
                "node_did": node_did,
                "task_id": task_id,
                "agent_did": agent_did,
                "prompt_template": prompt_template,
                "enabled": enabled,
            }),
        )],
    )
    .await;
}

async fn create_schedule_with_concurrency(
    access: &ConfigAccess,
    node_did: &str,
    trigger_id: &str,
    task_id: &str,
    schedule_id: &str,
    interval_secs: i64,
    concurrency: &str,
) {
    seed_automation(
        access,
        vec![
            (
                Collection::Schedule,
                serde_json::json!({
                    "node_did": node_did,
                    "schedule_id": schedule_id,
                    "cadence": {"kind": "interval", "interval_secs": interval_secs},
                }),
            ),
            (
                Collection::Trigger,
                serde_json::json!({
                    "node_did": node_did,
                    "trigger_id": trigger_id,
                    "task_id": task_id,
                    "source": {"kind": "schedule", "schedule_id": schedule_id},
                    "enabled": true,
                    "concurrency": concurrency,
                }),
            ),
        ],
    )
    .await;
}

async fn create_event_trigger(
    access: &ConfigAccess,
    node_did: &str,
    trigger_id: &str,
    task_id: &str,
    source_collection: &str,
    event_kind: &str,
    concurrency: &str,
) -> String {
    let event_source_id = format!("{trigger_id}:source");
    seed_automation(
        access,
        vec![
            (
                Collection::EventSource,
                serde_json::json!({
                    "node_did": node_did,
                    "event_source_id": event_source_id,
                    "source_collection": source_collection,
                    "event_kind": event_kind,
                }),
            ),
            (
                Collection::Trigger,
                serde_json::json!({
                    "node_did": node_did,
                    "trigger_id": trigger_id,
                    "task_id": task_id,
                    "source": {"kind": "event", "event_source_id": event_source_id},
                    "enabled": true,
                    "concurrency": concurrency,
                }),
            ),
        ],
    )
    .await;
    event_source_id
}

#[tokio::test]
async fn retired_reserved_ids_are_never_visible() {
    let env = DocumentViewEnv::new("document-view-retired-ids").await;
    let node_did = env.node_did();
    let graph_trigger = format!("graph-trigger-{}-{}", "a".repeat(64), "b".repeat(16));
    let mut documents = Vec::new();
    for id in [graph_trigger.as_str(), "pkg-x", "ordinary"] {
        documents.extend([
            (
                Collection::Trigger,
                serde_json::json!({
                    "node_did": node_did, "trigger_id": id, "task_id": "task-1",
                    "source": {"kind": "event", "event_source_id": id}, "enabled": false,
                    "concurrency": "serial",
                }),
            ),
            (
                Collection::EventSource,
                serde_json::json!({
                    "node_did": node_did, "event_source_id": id,
                    "source_collection": "CustomerSignup", "event_kind": "created",
                }),
            ),
            (
                Collection::Callback,
                serde_json::json!({
                    "node_did": node_did, "callback_id": id,
                    "handler": {"kind": "built_in", "emitter": "create_workspace"},
                }),
            ),
            (
                Collection::CallbackBinding,
                serde_json::json!({
                    "node_did": node_did, "binding_id": id, "event_source_id": id,
                    "callback_id": id,
                }),
            ),
        ]);
    }
    seed_automation(&env.access, documents).await;

    let view = env.load_view().await;
    fn ids<T>(map: &RapidHashMap<String, DocumentRecord<T>>) -> Vec<&str> {
        let mut ids: Vec<&str> = map.keys().map(String::as_str).collect();
        ids.sort_unstable();
        ids
    }
    assert_eq!(ids(&view.triggers), ["ordinary"]);
    assert_eq!(ids(&view.event_sources), ["ordinary"]);
    env.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn load_document_runtime_view_populates_tasks_and_schedules() {
    let signed_home = gents_documents::test_support::SignedHome::enroll(
        crate::support::trial_store("document-view-tasks-schedules").await,
        "document-view-tasks-schedules",
    )
    .await;
    let agent = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, user, access) = signed_home.into_parts();
    let node = &db.node;
    let node_did = db.node_identity.did();
    assert_eq!(
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &agent)
            .unwrap()
            .did(),
        agent.agent_did
    );
    let response = access
        .execute("{ Node { _docID _version { cid } } }")
        .await
        .unwrap();
    let node_doc = response["data"]["Node"][0]["_docID"].as_str().unwrap();
    let node_cid = response["data"]["Node"][0]["_version"][0]["cid"]
        .as_str()
        .unwrap();
    let (block, signature) = node
        .authorized_signed_block_bytes(node_cid, Some(user.did()))
        .await
        .unwrap();
    let block = defra_core::block::Block::from_dag_cbor(&block).unwrap();
    let signature = defra_core::block::Signature::from_dag_cbor(&signature).unwrap();
    assert_eq!(
        db::block::verify::verified_signature_signer_did(&block, &signature).unwrap(),
        user.did()
    );
    let documents = [
        (
            "Task",
            gents_protocol::schemas::TASK,
            serde_json::json!({"node_did": node_did, "task_id": "task-alpha", "agent_did": "agent-that-never-existed", "prompt_template": "unused", "enabled": false}),
        ),
        (
            "Task",
            gents_protocol::schemas::TASK,
            serde_json::json!({"node_did": node_did, "task_id": "task-beta", "agent_did": "agent-that-never-existed", "prompt_template": "unused", "enabled": false}),
        ),
        (
            "Schedule",
            gents_protocol::schemas::SCHEDULE,
            serde_json::json!({"node_did": node_did, "schedule_id": "schedule-alpha", "cadence": {"kind": "interval", "interval_secs": 60}}),
        ),
        (
            "Trigger",
            gents_protocol::schemas::TRIGGER,
            serde_json::json!({"node_did": node_did, "trigger_id": "trigger-schedule-alpha", "task_id": "task-alpha", "source": {"kind": "schedule", "schedule_id": "schedule-alpha"}, "enabled": true, "concurrency": "serial"}),
        ),
    ];
    for (name, schema, input) in documents {
        let created = access
            .transact_write("test.signed_automation", |txn| {
                let input = &input;
                Box::pin(async move {
                    txn.execute_client_signed_create(
                        &format!("mutation($input: {name}MutationInputArg!) {{ create_{name}(input: $input) {{ _docID }} }}"),
                        &serde_json::json!({"input": input}),
                        schema,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let doc = gents_store::graphql::created_doc_id(&created, name).unwrap();
        access
            .add_resource_relationship(name, &doc, "parent", "nodes", node_doc)
            .await
            .unwrap();
    }

    let view = load_document_runtime_view(node, node_did)
        .await
        .expect("document view should load");

    assert_eq!(view.tasks.len(), 2, "expected two Task documents");
    assert!(view.tasks.contains_key("task-alpha"));
    assert!(view.tasks.contains_key("task-beta"));

    assert_eq!(view.schedules.len(), 1, "expected one Schedule document");
    assert!(view.schedules.contains_key("schedule-alpha"));
    let schedule_record = view
        .schedules
        .get("schedule-alpha")
        .expect("schedule-alpha present");
    assert_eq!(
        schedule_record.value.cadence,
        gents_documents::ScheduleCadence::Interval { interval_secs: 60 },
        "schedule carries its interval cadence"
    );
    assert_eq!(view.triggers.len(), 1, "expected one Trigger document");
    let trigger_record = view
        .triggers
        .get("trigger-schedule-alpha")
        .expect("trigger present");
    assert_eq!(
        trigger_record.value.task_id, "task-alpha",
        "trigger references task-alpha"
    );
    match &trigger_record.value.source {
        gents_documents::TriggerSource::Schedule { schedule_id } => {
            assert_eq!(
                schedule_id, "schedule-alpha",
                "trigger references schedule-alpha"
            );
        }
        other => panic!("trigger must carry a schedule source, got {other:?}"),
    }
}

#[tokio::test]
async fn load_document_runtime_view_populates_event_triggers() {
    let env = DocumentViewEnv::new("document-view-event-triggers").await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "task-1",
        "agent-that-never-existed",
        "unused",
        false,
    )
    .await;
    create_event_trigger(
        &env.access,
        env.node_did(),
        "trig-1",
        "task-1",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let view = env.load_view().await;
    assert_eq!(view.event_sources.len(), 1);
    let source_record = view
        .event_sources
        .get("trig-1:source")
        .expect("trig-1:source present in event_sources");
    assert_eq!(source_record.value.source_collection, "CustomerSignup");
    assert_eq!(source_record.value.event_kind.as_deref(), Some("created"));

    assert_eq!(view.triggers.len(), 1);
    let trigger_record = view
        .triggers
        .get("trig-1")
        .expect("trig-1 present in triggers");
    assert_eq!(trigger_record.value.task_id, "task-1");
    match &trigger_record.value.source {
        gents_documents::TriggerSource::Event { event_source_id } => {
            assert_eq!(event_source_id, "trig-1:source");
        }
        other => panic!("trigger must carry an event source, got {other:?}"),
    }
}

#[tokio::test]
async fn resolve_produces_active_schedule_when_task_and_behavior_exist() {
    let env = DocumentViewEnv::new("document-view-resolve-schedule-active").await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "task-resolve-active",
        &env.agent_did,
        "do the thing",
        true,
    )
    .await;
    create_schedule_with_concurrency(
        &env.access,
        env.node_did(),
        "trigger-resolve-active",
        "task-resolve-active",
        "schedule-resolve-active",
        60,
        "serial",
    )
    .await;

    let snapshot = env.resolve().await;
    assert_eq!(
        snapshot.active_schedules.len(),
        1,
        "expected exactly one active schedule"
    );
    assert!(
        snapshot.unavailable_schedules.is_empty(),
        "expected no unavailable schedules, got {:?}",
        snapshot.unavailable_schedules
    );
    let resolved = snapshot
        .active_schedules
        .get("trigger-resolve-active")
        .expect("trigger-resolve-active present in active_schedules");
    assert_eq!(resolved.schedule_id, "schedule-resolve-active");
    assert_eq!(resolved.task.task_id, "task-resolve-active");
    assert_eq!(resolved.task.agent_did, env.agent_did);
    assert_eq!(resolved.task.prompt_template, "do the thing");
    assert_eq!(
        resolved.cadence,
        gents_documents::ScheduleCadence::Interval { interval_secs: 60 }
    );
    assert_eq!(
        resolved.concurrency,
        gents_documents::ConcurrencyMode::Serial
    );
}

#[tokio::test]
async fn resolve_produces_active_event_trigger_when_task_and_behavior_exist() {
    let env = DocumentViewEnv::new("document-view-resolve-trigger-active").await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "task-trigger-active",
        &env.agent_did,
        "do the thing on event",
        true,
    )
    .await;
    create_event_trigger(
        &env.access,
        env.node_did(),
        "trigger-active",
        "task-trigger-active",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let snapshot = env.resolve().await;
    assert_eq!(
        snapshot.active_event_triggers.len(),
        1,
        "expected exactly one active event trigger"
    );
    assert!(
        snapshot.unavailable_event_triggers.is_empty(),
        "expected no unavailable event triggers, got {:?}",
        snapshot.unavailable_event_triggers
    );
    let resolved = snapshot
        .active_event_triggers
        .get("trigger-active")
        .expect("trigger-active present in active_event_triggers");
    assert_eq!(resolved.trigger_id, "trigger-active");
    assert_eq!(resolved.task.task_id, "task-trigger-active");
    assert_eq!(resolved.task.agent_did, env.agent_did);
    assert_eq!(resolved.task.prompt_template, "do the thing on event");
    assert_eq!(resolved.source_collection, "CustomerSignup");
    assert_eq!(resolved.event_kind, "created");
    assert_eq!(
        resolved.concurrency,
        gents_documents::ConcurrencyMode::Serial
    );
}

#[tokio::test]
async fn persisted_required_source_fill_quarantines_and_recovers_event_trigger() {
    let env = DocumentViewEnv::new("document-view-required-source-fill").await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "fill-task",
        &env.agent_did,
        "process event",
        true,
    )
    .await;
    create_event_trigger(
        &env.access,
        env.node_did(),
        "fill-trigger",
        "fill-task",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let entries = |required| {
        serde_json::json!({"entries": [{
            "tool_name": "write_result",
            "collection": "Result",
            "fields": [{
                "name": "handoff_id",
                "required": required,
                "fill": {"source_field": "handoff_id"}
            }]
        }]})
    };
    seed_automation(
        &env.access,
        vec![(
            Collection::DatastoreToolSurface,
            serde_json::json!({
                "node_did": env.node_did(),
                "surface_id": "fill-surface",
                "enabled": true,
                "entries": entries(true),
            }),
        )],
    )
    .await;

    let tools_doc = find_owned_doc(
        &env.access,
        "Tools",
        "tools_id",
        &format!("{}:tools", env.agent_did),
    )
    .await;
    update_signed_document(
        &env.access,
        Collection::Tools,
        &tools_doc,
        serde_json::json!({"datastore": {"datastore_tool_surface_ids": ["fill-surface"]}}),
    )
    .await;

    let snapshot = env.resolve().await;
    let unavailable = snapshot.unavailable_agents.get(&env.agent_did).unwrap();
    assert!(unavailable
        .diagnostic
        .contains("runtime-filled and cannot be required"));
    assert!(!snapshot.active_event_triggers.contains_key("fill-trigger"));
    assert!(snapshot.unavailable_event_triggers.contains("fill-trigger"));

    let surface_doc = find_owned_doc(
        &env.access,
        "DatastoreToolSurface",
        "surface_id",
        "fill-surface",
    )
    .await;
    update_signed_document(
        &env.access,
        Collection::DatastoreToolSurface,
        &surface_doc,
        serde_json::json!({"entries": entries(false)}),
    )
    .await;
    let snapshot = env.resolve().await;
    assert!(snapshot.agents.contains_key(&env.agent_did));
    assert!(snapshot.active_event_triggers.contains_key("fill-trigger"));
    assert!(!snapshot.unavailable_event_triggers.contains("fill-trigger"));
}

#[tokio::test]
async fn resolve_marks_event_trigger_unavailable_when_task_missing_or_disabled() {
    let env = DocumentViewEnv::new("document-view-resolve-trigger-unavailable").await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "task-trigger-disabled",
        &env.agent_did,
        "disabled task",
        false,
    )
    .await;
    create_event_trigger(
        &env.access,
        env.node_did(),
        "trigger-task-disabled",
        "task-trigger-disabled",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;
    create_event_trigger(
        &env.access,
        env.node_did(),
        "trigger-task-missing",
        "task-that-never-existed",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let snapshot = env.resolve().await;
    assert!(
        snapshot.active_event_triggers.is_empty(),
        "expected no active event triggers, got {:?}",
        snapshot.active_event_triggers.keys().collect::<Vec<_>>()
    );
    for trigger_id in ["trigger-task-missing", "trigger-task-disabled"] {
        assert!(
            snapshot.unavailable_event_triggers.contains(trigger_id),
            "{trigger_id} should be in unavailable_event_triggers: {:?}",
            snapshot.unavailable_event_triggers
        );
    }
}

/// `source_collection` is interpolated into GraphQL identifier positions by
/// the event source, where escaping cannot apply. A trigger document whose
/// `source_collection` is not a valid GraphQL Name (query injection) or is
/// `__`-prefixed (introspection-reserved) must be quarantined at resolve
/// time, never activated. This covers documents that bypass self-config
/// validation entirely (e.g. replicated from a peer).
#[tokio::test]
async fn resolve_quarantines_event_trigger_with_invalid_source_collection() {
    let env = DocumentViewEnv::new("document-view-resolve-trigger-injection").await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "task-trigger-injection",
        &env.agent_did,
        "enabled task",
        true,
    )
    .await;
    create_event_trigger(
        &env.access,
        env.node_did(),
        "trigger-injection",
        "task-trigger-injection",
        "Msg(limit: 1) { _docID } Foo",
        "created",
        "serial",
    )
    .await;
    create_event_trigger(
        &env.access,
        env.node_did(),
        "trigger-introspection",
        "task-trigger-injection",
        "__Type",
        "created",
        "serial",
    )
    .await;

    let snapshot = env.resolve().await;
    assert!(
        snapshot.active_event_triggers.is_empty(),
        "no injection-shaped trigger may activate, got {:?}",
        snapshot.active_event_triggers.keys().collect::<Vec<_>>()
    );
    for trigger_id in ["trigger-injection", "trigger-introspection"] {
        assert!(
            snapshot.unavailable_event_triggers.contains(trigger_id),
            "{trigger_id} should be quarantined in unavailable_event_triggers: {:?}",
            snapshot.unavailable_event_triggers
        );
    }
}

#[tokio::test]
async fn resolve_marks_schedule_unavailable_when_task_missing_or_disabled() {
    let env = DocumentViewEnv::new("document-view-resolve-schedule-unavailable").await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "task-resolve-disabled",
        &env.agent_did,
        "disabled task",
        false,
    )
    .await;
    create_schedule_with_concurrency(
        &env.access,
        env.node_did(),
        "trigger-resolve-task-disabled",
        "task-resolve-disabled",
        "schedule-resolve-task-disabled",
        60,
        "serial",
    )
    .await;
    create_schedule_with_concurrency(
        &env.access,
        env.node_did(),
        "trigger-resolve-task-missing",
        "task-that-never-existed",
        "schedule-resolve-task-missing",
        60,
        "serial",
    )
    .await;

    let snapshot = env.resolve().await;
    assert!(
        snapshot.active_schedules.is_empty(),
        "expected no active schedules, got {:?}",
        snapshot.active_schedules.keys().collect::<Vec<_>>()
    );
    for trigger_id in [
        "trigger-resolve-task-missing",
        "trigger-resolve-task-disabled",
    ] {
        assert!(
            snapshot.unavailable_schedules.contains(trigger_id),
            "{trigger_id} should be in unavailable_schedules: {:?}",
            snapshot.unavailable_schedules
        );
    }
}

#[tokio::test]
async fn resolve_populates_active_tasks_for_enabled_tasks_with_ready_behaviors() {
    let env = DocumentViewEnv::new("document-view-resolve-active-tasks").await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "task-active",
        &env.agent_did,
        "hello",
        true,
    )
    .await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "task-disabled",
        &env.agent_did,
        "disabled",
        false,
    )
    .await;
    create_task_bound(
        &env.access,
        env.node_did(),
        "task-missing-agent",
        "agent-that-never-existed",
        "orphan",
        true,
    )
    .await;

    let snapshot = env.resolve().await;
    assert_eq!(
        snapshot.active_tasks.len(),
        1,
        "expected exactly one active task, got {:?}",
        snapshot.active_tasks.keys().collect::<Vec<_>>()
    );
    assert!(
        snapshot.active_tasks.contains_key("task-active"),
        "task-active should be in active_tasks: {:?}",
        snapshot.active_tasks.keys().collect::<Vec<_>>()
    );
    assert!(
        !snapshot.active_tasks.contains_key("task-disabled"),
        "disabled task must NOT be in active_tasks"
    );
    assert!(
        !snapshot.active_tasks.contains_key("task-missing-agent"),
        "task with unavailable agent must NOT be in active_tasks"
    );

    let resolved = snapshot
        .active_tasks
        .get("task-active")
        .expect("task-active present");
    assert_eq!(resolved.task_id, "task-active");
    assert_eq!(resolved.agent_did, env.agent_did);
    assert_eq!(resolved.prompt_template, "hello");
    assert!(resolved.output_schema_ref.is_none());
}

#[tokio::test]
async fn chatgpt_codex_behavior_without_credential_is_unavailable() {
    let env = DocumentViewEnv::new("document-view-chatgpt-nocred").await;
    env.bind_chatgpt().await;

    let snapshot = env.resolve().await;
    assert!(
        !snapshot.agents.contains_key(&env.agent_did),
        "a ChatGptCodex agent_config without an OAuthCredential must not be runnable (it would hang \
         startup readiness building the client)"
    );
    let reason = snapshot
        .unavailable_agents
        .get(&env.agent_did)
        .expect("agent_config should be reported unavailable");
    assert!(
        reason
            .diagnostic
            .contains("requires enabled OAuthCredential"),
        "unavailable reason should identify the missing canonical credential: {}",
        reason.diagnostic
    );
}

#[tokio::test]
async fn runtime_snapshot_skips_an_unknown_provider_kind_backend() {
    let env = DocumentViewEnv::new("document-view-unknown-kind").await;
    let future = gents_store::identity::provision_agent_identity_authorized(
        &env.node,
        env.identity.as_ref(),
        &identity::Did::new(env.user.did()).unwrap(),
    )
    .await
    .unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
        env.node_did(),
        "future",
        &future,
        &env.access,
    )
    .await;
    let doc = find_owned_doc(
        &env.access,
        "InferenceBackendMetadata",
        "backend_id",
        &format!("{}:backend", future.agent_did),
    )
    .await;
    let doc = escape_graphql_string(&doc);
    env.access
        .transact_write("test.unknown_provider", |txn| {
            let doc = &doc;
            Box::pin(async move {
                txn.execute_client_signed_update(
                    &format!(r#"mutation($input: InferenceBackendMetadataMutationInputArg!) {{ update_InferenceBackendMetadata(docID: "{doc}", input: $input) {{ _docID }} }}"#),
                    &serde_json::json!({"input": {"provider_kind": "FutureProviderKind"}}),
                    gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
                )
                .await
            })
        })
        .await
        .unwrap();

    let view = env.load_view().await;
    assert!(
        !view.has_unresolved_agent_references(),
        "an agent_config on an unknown kind is unavailable, not pending: {:?}",
        view.pending_visibility_details()
    );
    let snapshot = env.resolve_view(&view).await;
    assert!(
        snapshot.agents.contains_key(&env.agent_did),
        "unavailable: {:?}",
        snapshot.unavailable_agents
    );
    let future = snapshot
        .unavailable_agents
        .get(&future.agent_did)
        .expect("future agent_config is reported unavailable");
    assert_eq!(
        future.public_reason,
        gents_protocol::node_readiness::AgentReadinessUnavailableReason::InferenceProfileInvalid
    );
    assert!(
        future.diagnostic.contains("FutureProviderKind"),
        "{}",
        future.diagnostic
    );
}

#[tokio::test]
async fn chatgpt_codex_behavior_with_enabled_credential_is_runnable() {
    let env = DocumentViewEnv::new("document-view-chatgpt-cred").await;
    env.bind_chatgpt().await;
    env.upsert_chatgpt_credential().await;

    let snapshot = env.resolve().await;
    assert!(
        snapshot.agents.contains_key(&env.agent_did),
        "a ChatGptCodex agent_config with an enabled OAuthCredential must be runnable; unavailable: {:?}",
        snapshot.unavailable_agents
    );
}

#[tokio::test]
async fn readiness_follows_the_backend_account_reference() {
    let env = DocumentViewEnv::new("document-view-account-ref").await;
    env.bind_chatgpt().await;
    env.upsert_chatgpt_credential().await;
    let mut view = env.load_view().await;
    for backend in view.backends.values_mut() {
        backend.value.auth = gents_documents::BackendAuth::NodeOAuth {
            account_ref: Some("acct-x".to_string()),
        };
    }
    let ready = |view: DocumentRuntimeView| {
        let env = &env;
        async move {
            let snapshot = env.resolve_view(&view).await;
            match snapshot.unavailable_agents.get(&env.agent_did) {
                None => true,
                Some(reason) => {
                    assert_eq!(
                        reason.public_reason,
                        gents_protocol::node_readiness::AgentReadinessUnavailableReason::CredentialsRequired
                    );
                    false
                }
            }
        }
    };

    assert!(
        !ready(view.clone()).await,
        "only the original account is stored, so acct-x does not resolve"
    );

    let original = view
        .oauth_accounts
        .rows_mut()
        .first()
        .expect("original row")
        .clone();
    let mut account = original.clone();
    account.credential_id = format!("{}:acct-x", original.credential_id);
    account.account_ref = Some("acct-x".to_string());
    view.oauth_accounts.rows_mut().push(account);
    assert!(ready(view.clone()).await, "an enabled acct-x row resolves");

    view.oauth_accounts
        .rows_mut()
        .last_mut()
        .expect("acct-x row")
        .enabled = false;
    assert!(!ready(view).await, "a disabled acct-x row never resolves");
}

#[tokio::test]
async fn disabling_or_removing_an_account_stops_only_its_behaviors() {
    use gents_inference::auth::credential::set_account_enabled;

    let env = DocumentViewEnv::new("document-view-account-lifecycle").await;
    env.bind_claude().await;
    env.sign_in_claude("a").await;
    let b = env.sign_in_claude("b").await;
    let b_ref = b.credential.account_ref.clone().expect("b has a reference");
    let ready_on = |account_ref: Option<String>| {
        let env = &env;
        async move {
            let snapshot = env
                .resolve_with_auth(gents_documents::BackendAuth::NodeOAuth { account_ref })
                .await;
            !snapshot.unavailable_agents.contains_key(&env.agent_did)
        }
    };
    assert!(ready_on(None).await && ready_on(Some(b_ref.clone())).await);

    set_account_enabled(
        &env.access,
        env.node_did(),
        &b.credential.credential_id,
        false,
    )
    .await
    .unwrap();
    assert!(
        !ready_on(Some(b_ref.clone())).await,
        "a disabled account stops"
    );
    assert!(ready_on(None).await, "the other account keeps running");

    env.remove_account(&b.credential.credential_id).await;
    assert!(!ready_on(Some(b_ref)).await, "a removed account stops");
    assert!(ready_on(None).await, "the other account keeps running");
}

#[tokio::test]
async fn an_account_pool_agent_follows_its_enabled_accounts() {
    use gents_inference::auth::credential::set_account_enabled;

    let env = DocumentViewEnv::new("document-view-account-pool").await;
    env.bind_claude().await;
    let pool_with = |auth: gents_documents::BackendAuth| {
        let env = &env;
        async move {
            let snapshot = env.resolve_with_auth(auth).await;
            let agent = snapshot
                .agents
                .get(&env.agent_did)
                .unwrap_or_else(|| panic!("agent runs: {:?}", snapshot.unavailable_agents));
            agent.oauth_pool.clone()
        }
    };
    let pool = gents_documents::BackendAuth::NodeOAuthPool {
        allow_overage: false,
    };
    let a_id = env.sign_in_claude("a").await.credential.credential_id;
    assert_eq!(pool_with(pool.clone()).await, vec![a_id.clone()]);

    let b_id = env.sign_in_claude("b").await.credential.credential_id;
    let grown = pool_with(pool.clone()).await;
    assert_eq!(grown.len(), 2, "a sign-in joins the pool: {grown:?}");
    assert!(grown.contains(&a_id) && grown.contains(&b_id));

    set_account_enabled(&env.access, env.node_did(), &b_id, false)
        .await
        .unwrap();
    assert_eq!(
        pool_with(pool).await,
        vec![a_id],
        "a disabled account leaves the pool"
    );
    assert!(
        pool_with(gents_documents::BackendAuth::NodeOAuth { account_ref: None })
            .await
            .is_empty(),
        "a single-account backend carries no pool"
    );
}

#[tokio::test]
async fn an_unavailable_account_is_a_behavior_unavailable_rejection() {
    use gents_inference::auth::credential::set_account_enabled;
    use gents_protocol::node_readiness::is_behavior_unavailable_rejection;
    use gents_protocol::node_readiness::AgentReadinessUnavailableReason as Reason;

    let env = DocumentViewEnv::new("document-view-account-rejection").await;
    env.bind_claude().await;
    env.sign_in_claude("a").await;
    let b = env.sign_in_claude("b").await;
    let b_backend = format!(
        "{}-{}",
        gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        b.credential
            .account_ref
            .as_deref()
            .expect("b has a reference")
    );
    gents_inference::backends::registry::set_backend_probe_status_as(
        &ConfigAccess::local(env.node.clone()),
        env.node_did(),
        &b_backend,
        "healthy",
    )
    .await
    .unwrap();
    let x = env.agent_did.clone();
    let on_a = format!("{x}:inference");
    let on_b = format!("{x}:inference-b");
    // Y is a second real Agent: its DID must derive from the runtime root and
    // its creation nonce, or its identity resolution rejects the configuration.
    let y_binding = gents_store::identity::provision_agent_identity(env.identity.as_ref()).unwrap();
    let y = y_binding.agent_did.clone();
    let reasons = |compaction_on_b: bool| {
        let env = &env;
        let (x, y, y_nonce, on_a, on_b, b_backend) = (
            x.clone(),
            y.clone(),
            y_binding.creation_nonce.clone(),
            on_a.clone(),
            on_b.clone(),
            b_backend.clone(),
        );
        async move {
            let mut view = env.load_view().await;
            let mut b_profile = view.inference_profiles[&on_a].clone();
            b_profile.value.profile_id = on_b.clone();
            b_profile.value.backend_id = b_backend;
            view.inference_profiles.insert(on_b.clone(), b_profile);
            let mut y_record = view.agents[&x].clone();
            y_record.value.agent_did = y.clone();
            y_record.value.creation_nonce = y_nonce;
            view.agents.insert(y.clone(), y_record);
            let x_record = view.agents.get_mut(&x).unwrap();
            if compaction_on_b {
                let compaction: gents_documents::CompactionConfig =
                    serde_json::from_value(serde_json::json!({
                        "compaction_id": "compaction-b", "node_did": env.node_did(),
                        "inference_profile_id": on_b,
                    }))
                    .unwrap();
                view.compactions.insert(
                    "compaction-b".into(),
                    DocumentRecord {
                        version: Some("fixture-compaction-version".into()),
                        doc_id: "compaction-b".into(),
                        value: compaction,
                    },
                );
                let mut context =
                    view.contexts[x_record.value.context_id.as_deref().unwrap()].clone();
                context.value.context_id = "context-x".into();
                context.value.compaction_id = Some("compaction-b".into());
                view.contexts.insert("context-x".into(), context);
                x_record.value.context_id = Some("context-x".into());
            } else {
                x_record.value.inference_profile_id = on_b;
            }
            let snapshot = env.resolve_view(&view).await;
            let reason = |id: &str| {
                snapshot
                    .unavailable_agents
                    .get(id)
                    .map(|unavailable| unavailable.public_reason)
            };
            (reason(&x), reason(&y))
        }
    };
    assert_eq!(reasons(false).await, (None, None));
    assert_eq!(reasons(true).await, (None, None));

    let rejected_only_x = |(x_reason, y_reason): (Option<Reason>, Option<Reason>),
                           expected: Reason| {
        assert_eq!(x_reason, Some(expected));
        assert!(is_behavior_unavailable_rejection(expected.public_message()));
        assert_eq!(y_reason, None, "Y on A keeps running");
    };
    set_account_enabled(
        &env.access,
        env.node_did(),
        &b.credential.credential_id,
        false,
    )
    .await
    .unwrap();
    rejected_only_x(reasons(false).await, Reason::CredentialsRequired);
    rejected_only_x(reasons(true).await, Reason::ToolConfigurationInvalid);

    env.remove_account(&b.credential.credential_id).await;
    rejected_only_x(reasons(false).await, Reason::CredentialsRequired);
    rejected_only_x(reasons(true).await, Reason::ToolConfigurationInvalid);
}

#[tokio::test]
async fn claude_subscription_behavior_requires_enabled_credential() {
    let env = DocumentViewEnv::new("document-view-claude-cred").await;
    env.bind_claude().await;

    let snapshot = env.resolve().await;
    assert!(
        !snapshot.agents.contains_key(&env.agent_did),
        "a ClaudeCliSubscription agent_config without an OAuthCredential must not be runnable"
    );
    let reason = snapshot
        .unavailable_agents
        .get(&env.agent_did)
        .expect("agent_config should be reported unavailable");
    assert_eq!(
        reason.public_reason,
        gents_protocol::node_readiness::AgentReadinessUnavailableReason::CredentialsRequired
    );
    assert!(
        reason
            .diagnostic
            .contains("requires enabled OAuthCredential"),
        "unavailable reason should identify the missing canonical credential: {}",
        reason.diagnostic
    );

    let credential = gents_inference::auth::claude::credential_from_login_tokens(
        env.node_did(),
        gents_inference::auth::claude::CLAUDE_OAUTH_PROVIDER,
        &gents_inference::auth::claude::ClaudeLoginTokens {
            access_token: "access-token".to_string(),
            refresh_token: "refresh-token".to_string(),
            expires_in: Some(3600),
            scope: None,
            account_id: None,
            organization_uuid: None,
            account_uuid: None,
        },
        chrono::Utc::now(),
    );
    gents_inference::auth::credential::upsert_oauth_credential(&env.node, &credential)
        .await
        .expect("upsert credential");

    let view = env.load_view().await;
    let snapshot = env.resolve_view(&view).await;
    assert!(
        snapshot.agents.contains_key(&env.agent_did),
        "a ClaudeCliSubscription agent_config with an enabled OAuthCredential must be runnable; unavailable: {:?}",
        snapshot.unavailable_agents
    );
    for state in [
        gents_inference::backends::health::BackendHealthState::CredentialsRequired,
        gents_inference::backends::health::BackendHealthState::SubscriptionNotEntitled,
        gents_inference::backends::health::BackendHealthState::Unhealthy,
        gents_inference::backends::health::BackendHealthState::Healthy,
    ] {
        for backend in view.backends.values() {
            env.resolve_context
                .backend_health
                .set_for_test(&backend.value.backend_id, state, 1)
                .await;
        }
        let snapshot = env.resolve_view(&view).await;
        let expected = state.readiness_reason().or_else(|| {
            state.blocks_routing().then_some(
                gents_protocol::node_readiness::AgentReadinessUnavailableReason::BackendTemporarilyUnavailable,
            )
        });
        assert_eq!(
            snapshot
                .unavailable_agents
                .get(&env.agent_did)
                .map(|agent| agent.public_reason),
            expected
        );
        assert_eq!(
            snapshot.agents.contains_key(&env.agent_did),
            expected.is_none()
        );
    }
}

#[tokio::test]
async fn apply_control_update_admits_chatgpt_behavior_when_credential_added() {
    let env = DocumentViewEnv::new("document-view-chatgpt-apply").await;
    env.bind_chatgpt().await;

    let mut view = env.load_view().await;
    let before = env.resolve_view(&view).await;
    assert!(
        !before.agents.contains_key(&env.agent_did),
        "agent_config must start unavailable without a credential"
    );

    let doc_id = env.upsert_chatgpt_credential().await;
    let outcome = apply_control_update(env.node_did(), "OAuthCredential", &doc_id, &mut view)
        .await
        .expect("apply control update");
    assert_eq!(
        outcome,
        ControlUpdateOutcome::FullReload,
        "creating an OAuthCredential must drive a full reload reconcile"
    );

    let after = env.resolve().await;
    assert!(
        after.agents.contains_key(&env.agent_did),
        "agent_config must become runnable once the credential exists; unavailable: {:?}",
        after.unavailable_agents
    );
}

fn finding_decl() -> gents_documents::WriteToolDecl {
    gents_documents::WriteToolDecl {
        notification: None,
        tool_name: "write_experiment_finding".to_string(),
        collection: "ExperimentFinding".to_string(),
        description: "Record a finding document for the next pipeline stage.".to_string(),
        fields: ["job_id", "finding_id", "content", "stage"]
            .into_iter()
            .map(|name| gents_documents::WriteToolField {
                name: name.to_string(),
                required: true,
                fill: None,
            })
            .collect(),
        output_obligation: None,
        key_fields: None,
    }
}

#[tokio::test]
async fn apply_control_update_evicts_surface_when_ownership_moves_away() {
    let env = DocumentViewEnv::new("document-view-surface-revoke").await;
    let mut view = env.load_view().await;

    seed_automation(
        &env.access,
        vec![(
            Collection::DatastoreToolSurface,
            serde_json::json!({
                "surface_id": "experiment-writes", "node_did": env.node_did(), "enabled": true,
                "entries": {"entries": [finding_decl()]},
            }),
        )],
    )
    .await;
    let doc_id = find_owned_doc(
        &env.access,
        "DatastoreToolSurface",
        "surface_id",
        "experiment-writes",
    )
    .await;

    let outcome = apply_control_update(env.node_did(), "DatastoreToolSurface", &doc_id, &mut view)
        .await
        .expect("apply surface create");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let mut view = env.load_view().await;
    assert!(view
        .datastore_tool_surfaces
        .contains_key("experiment-writes"));

    update_signed_document(
        &env.access,
        Collection::DatastoreToolSurface,
        &doc_id,
        serde_json::json!({"node_did": "did:key:zOtherOwner"}),
    )
    .await;

    let outcome = apply_control_update(env.node_did(), "DatastoreToolSurface", &doc_id, &mut view)
        .await
        .expect("apply surface reassign");
    assert_eq!(outcome, ControlUpdateOutcome::FullReload);
    let view = env.load_view().await;
    assert!(
        view.datastore_tool_surfaces.is_empty(),
        "surface must be evicted once it is owned by another node"
    );
}

#[tokio::test]
async fn watcher_scan_identities_follow_lean_candidates() {
    let env = DocumentViewEnv::new("watcher-scan-root").await;
    let cases = gents_lean_vocab::families::watcher_actors_scanned::cases();
    let agents = cases
        .iter()
        .map(|case| case.input.candidates.len())
        .max()
        .unwrap_or(0);
    let mut dids = vec![env.agent_did.clone()];
    while dids.len() < agents {
        let extra = gents_store::identity::provision_agent_identity_authorized(
            &env.node,
            env.identity.as_ref(),
            &identity::Did::new(env.user.did()).unwrap(),
        )
        .await
        .unwrap();
        gents_inference::test_support::install_test_agent_binding_with_access(
            env.node_did(),
            &format!("scan-{}", dids.len()),
            &extra,
            &env.access,
        )
        .await;
        dids.push(extra.agent_did);
    }
    let original = env.load_view().await;
    for case in cases {
        let name = case.input.name.as_str();
        let candidates = &case.input.candidates;
        let mut view = original.clone();
        view.agents
            .retain(|did, _| dids[..candidates.len()].contains(did));
        for (candidate, did) in candidates.iter().zip(&dids) {
            let agent = &mut view.agents.get_mut(did).unwrap().value;
            agent.enabled = candidate.enabled;
            if !candidate.configured {
                agent.node_did = "did:key:zWatcherScanForeignNode".into();
            }
            if !candidate.identity_resolved {
                agent.creation_nonce = "invalid".into();
            }
            if !candidate.available {
                let backend = view.inference_profiles[&agent.inference_profile_id]
                    .value
                    .backend_id
                    .clone();
                view.unknown_kind_backends
                    .insert(backend, "WatcherScanUnavailableKind".into());
            }
        }
        let snapshot = env.resolve_view(&view).await;
        for (candidate, did) in candidates.iter().zip(&dids) {
            assert_eq!(
                snapshot.agents.contains_key(did),
                candidate.available,
                "{name}: the fixture did not build {}'s availability; unavailable: {:?}",
                candidate.did,
                snapshot.unavailable_agents
            );
        }
        let active = snapshot.activate(1, Default::default());
        let identities = active.watcher_identities().unwrap();
        let mut scanned = Vec::new();
        for (candidate, did) in candidates.iter().zip(&dids) {
            match identities
                .iter()
                .filter(|identity| identity.did() == did.as_str())
                .count()
            {
                0 => {}
                1 => scanned.push(candidate.did.clone()),
                n => panic!("{name}: {} is scanned {n} times", candidate.did),
            }
        }
        assert_eq!(
            identities.len(),
            scanned.len(),
            "{name}: a foreign identity is scanned"
        );
        case.check(scanned);
    }
    env.node.shutdown().await.unwrap();
}

/// A command Task resolves on its schedule with no Agent document, and an
/// event Trigger of it stays unavailable: an event source reads as an Agent.
#[tokio::test]
async fn resolve_schedules_a_command_task_without_an_agent_and_holds_its_event_trigger() {
    let env = DocumentViewEnv::new("document-view-resolve-command-task").await;
    seed_automation(
        &env.access,
        vec![(
            Collection::Task,
            serde_json::json!({
                "node_did": env.node_did(),
                "task_id": "task-command",
                "command": {"argv": ["true"], "timeout_secs": 30},
            }),
        )],
    )
    .await;
    create_schedule_with_concurrency(
        &env.access,
        env.node_did(),
        "trigger-command-schedule",
        "task-command",
        "schedule-command",
        60,
        "serial",
    )
    .await;
    create_event_trigger(
        &env.access,
        env.node_did(),
        "trigger-command-event",
        "task-command",
        "CustomerSignup",
        "created",
        "serial",
    )
    .await;

    let snapshot = env.resolve().await;
    let resolved = snapshot
        .active_schedules
        .get("trigger-command-schedule")
        .expect("the command Task's schedule is active");
    assert_eq!(
        resolved.task.command,
        Some(gents_documents::TaskCommand {
            argv: vec!["true".into()],
            timeout_secs: Some(30),
        })
    );
    assert!(resolved.task.agent_did.is_empty());
    assert!(snapshot.active_event_triggers.is_empty());
    assert!(
        snapshot
            .unavailable_event_triggers
            .contains("trigger-command-event"),
        "{:?}",
        snapshot.unavailable_event_triggers
    );
}

/// The slot of a running Agent is built from the heads its view read: an
/// edit to a selected document moves that head, and the Agent the next view
/// resolves no longer matches the slot's fingerprint, so the supervisor
/// rebuilds the slot. A home on the node's own store verifies no body.
#[tokio::test]
async fn an_edit_to_a_selected_document_changes_the_slot_fingerprint() -> anyhow::Result<()> {
    use gents_store::collection::Collection;
    use gents_store::config_client::body_verification::BodyVerification;

    let signed_home = gents_documents::test_support::SignedHome::enroll(
        crate::support::trial_store("body-slot-heads").await,
        "body-slot-heads",
    )
    .await;
    let binding = gents_inference::test_support::default_agent(&signed_home).await;
    let (db, _user, access) = signed_home.into_parts();
    let (node, identity) = (db.node.clone(), db.node_identity.clone());
    let context = DocumentResolveContext::for_tests(
        identity.clone(),
        ToolCeiling::readonly(),
        gents_inference::backends::health::BackendHealthMap::new(),
    );
    let context_id = format!("{}:context", binding.agent_did);
    let resolved = || async {
        let view = load_document_runtime_view(&node, identity.did()).await?;
        let snapshot = resolve_document_runtime_snapshot_from_view(&node, &context, &view).await?;
        anyhow::Ok((
            snapshot
                .agent_with_tools(&binding.agent_did)
                .expect("the default Agent resolves")
                .slot_fingerprint(),
            view.contexts[&context_id]
                .version
                .clone()
                .expect("a loaded record has a head"),
            view.agents[&binding.agent_did].doc_id.clone(),
            snapshot.placement_bodies,
        ))
    };

    let (slot, context_head, agent_doc, bodies) = resolved().await?;
    seed_automation(
        &access,
        vec![(
            Collection::Skill,
            serde_json::json!({"skill_id": "body-skill", "node_did": identity.did(), "name": "Body", "enabled": true}),
        )],
    )
    .await;
    patch_desired_document(
        &access,
        Collection::AgentContext,
        identity.did(),
        &context_id,
        serde_json::json!({"skill_ids": ["body-skill"]}),
    )
    .await;
    let (edited_slot, edited_context_head, _, edited_bodies) = resolved().await?;

    assert!(slot.contains(&context_head));
    assert_ne!(edited_context_head, context_head);
    assert!(edited_slot.contains(&edited_context_head) && !edited_slot.contains(&context_head));
    assert_eq!(bodies.get(&agent_doc), Some(&BodyVerification::NotStrict));
    assert_eq!(edited_bodies, bodies);
    Ok(())
}
