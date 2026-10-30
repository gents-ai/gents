#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use defra_node::EmbeddedNode;

#[cfg(unix)]
pub(crate) async fn configure_runtime_agent(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    backend_id: &str,
    endpoint: &str,
    model: &str,
    agents_enabled: bool,
) {
    use gents::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    use gents::document_config::{AgentContext, AgentTools, BashTools, HostTools, Tools};
    use gents::Collection;

    seed_test_agent_configuration(node, node_did, agent_id, agent_id, model, true).await;
    gents::ConfigAccess::transact_local_write(node, None, "grok.runtime_fixture", |txn| {
        Box::pin(async move {
            use gents::config_client::read_desired_state_record_in_txn as read;
            let (_, mut agent) = read(txn, Collection::Agent, node_did, agent_id)
                .await?
                .expect("fixture agent");
            let profile_id = agent["inference_profile_id"]
                .as_str()
                .expect("fixture profile")
                .to_owned();
            let (_, mut profile) = read(txn, Collection::InferenceProfile, node_did, &profile_id)
                .await?
                .expect("fixture profile");
            profile["backend_id"] = backend_id.into();
            profile["model_name"] = model.into();
            let tools_id = format!("{agent_id}:tools");
            let context_id = format!("{agent_id}:context");
            let mut tools = Tools {
                tools_id: tools_id.clone(),
                node_did: node_did.into(),
                host: Some(HostTools {
                    bash: Some(BashTools {
                        mode: gents::BashMode::ReadOnly,
                        read_only_commands: Some(vec!["sleep".into(), "printf".into()]),
                        background_enabled: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            };
            let target_id = format!("{agent_id}:self-target");
            if agents_enabled {
                tools.agents = Some(AgentTools {
                    target_ids: vec![target_id.clone()],
                    enabled: Some(true),
                });
            }
            let context = AgentContext {
                context_id: context_id.clone(),
                node_did: node_did.into(),
                display_name: None,
                description: None,
                system_prompt: None,
                tools_id: Some(tools_id),
                compaction_id: None,
                skill_ids: Vec::new(),
                tags: Vec::new(),
            };
            agent["context_id"] = context_id.into();
            let backend = serde_json::json!({
                "node_did": node_did,
                "backend_id": backend_id,
                "name": "grok runtime fixture",
                "provider_kind": "OpenAiCompatible",
                "openai_wire_api": "chat_completions",
                "endpoint": endpoint,
                "auth": {"kind":"unauthenticated"},
                "enabled": true
            });
            let mut docs = vec![
                (Collection::InferenceBackend, backend),
                (Collection::InferenceProfile, profile),
                (Collection::Tools, serde_json::to_value(tools)?),
                (Collection::AgentContext, serde_json::to_value(context)?),
                (Collection::Agent, agent),
            ];
            if agents_enabled {
                docs.push((
                    Collection::AgentTarget,
                    serde_json::to_value(gents::AgentTargetDocument {
                        target_id,
                        node_did: node_did.into(),
                        target_node_did: node_did.into(),
                        agent_id: agent_id.into(),
                        name: agent_id.into(),
                        description: Some("self target for Grok control fixture".into()),
                        tags: Vec::new(),
                    })?,
                ));
            }
            let plan = DesiredStateApplyPlan::new(
                docs.into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
            )?;
            gents::config_client::apply_desired_state_plan(txn, &plan)
                .await
                .map(|_| ())
        })
    })
    .await
    .unwrap();
}

#[cfg(unix)]
pub(in crate::commands) async fn seed_test_agent_configuration(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    agent_id: &str,
    default_agent_id: &str,
    model_name: &str,
    enabled: bool,
) {
    gents::ensure_node(node, node_did)
        .await
        .expect("seed test principal");
    gents::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "grok.test_agent_configuration",
        |txn| {
            Box::pin(async move {
                use gents::config_client::{
                    apply_desired_state_plan, read_desired_state_record_in_txn,
                    DesiredStateApplyDocument, DesiredStateApplyPlan,
                };
                use gents::Collection;

                let (_, mut principal) =
                    read_desired_state_record_in_txn(txn, Collection::Node, node_did, node_did)
                        .await?
                        .expect("seeded principal");
                principal["default_agent_id"] = default_agent_id.into();
                let backend_id = format!("{agent_id}-backend");
                let profile_id = format!("{agent_id}-inference");
                let values = [
                    (Collection::Node, principal),
                    (
                        Collection::InferenceBackend,
                        serde_json::json!({
                            "node_did": node_did,
                            "backend_id": backend_id,
                            "name": format!("{agent_id} test backend"),
                            "provider_kind": "OpenAiCompatible",
                            "openai_wire_api": "chat_completions",
                            "endpoint": "http://127.0.0.1:1/v1",
                            "auth": {"kind": "unauthenticated"},
                            "enabled": true
                        }),
                    ),
                    (
                        Collection::InferenceProfile,
                        serde_json::json!({
                            "node_did": node_did,
                            "profile_id": profile_id,
                            "backend_id": backend_id,
                            "model_name": model_name
                        }),
                    ),
                    (
                        Collection::Agent,
                        serde_json::json!({
                            "node_did": node_did,
                            "agent_id": agent_id,
                            "display_name": agent_id,
                            "inference_profile_id": profile_id,
                            "enabled": enabled
                        }),
                    ),
                ];
                let plan = DesiredStateApplyPlan::new(
                    values
                        .into_iter()
                        .map(|(collection, value)| DesiredStateApplyDocument {
                            collection,
                            add: value.clone(),
                            update: value,
                        })
                        .collect(),
                )?;
                apply_desired_state_plan(txn, &plan).await
            })
        },
    )
    .await
    .expect("seed canonical test agent configuration");
}
