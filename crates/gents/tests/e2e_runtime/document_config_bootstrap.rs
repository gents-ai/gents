use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess, DesiredStateApplyPlan,
};
use gents::document_config::PackConfig;
use gents::{ensure_agent_principal, Collection};
use serde_json::json;

use crate::support::test_db;

#[tokio::test]
async fn canonical_config_roundtrips_scoped_references_and_preserves_explicit_default() {
    let db = test_db("document-config-scoped-roundtrip").await;
    let access = ConfigAccess::Local(db.node.clone());
    // Equal logical IDs under different principals must resolve independently.
    for (owner, model) in [
        ("did:test:roundtrip", "model-a"),
        ("did:test:other", "model-b"),
    ] {
        let config: PackConfig = serde_json::from_value(json!({
            "agent_principal": {
                "agent_did": owner, "display_name": "Explicit configuration",
                "default_behavior_id": "general", "tags": ["authored"]
            },
            "agent_behaviors": [{
                "agent_did": owner, "behavior_id": "general",
                "context_id": "context", "inference_profile_id": "balanced"
            }],
            "contexts": [{
                "agent_did": owner, "context_id": "context",
                "system_prompt": "Be precise. {{literal}}", "compaction_id": "compact"
            }],
            "compactions": [{
                "agent_did": owner, "compaction_id": "compact",
                "strategy": "StripThenSummarize", "threshold": 0.6
            }],
            "inference_backends": [{
                "agent_did": owner, "backend_id": "local", "name": "Local",
                "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                "auth": {"kind": "unauthenticated"}, "max_concurrent": 1, "max_queue_depth": 0
            }],
            "inference_profiles": [{
                "agent_did": owner, "profile_id": "balanced", "display_name": "Balanced",
                "backend_id": "local", "model_name": model,
                "context_window": 32768, "max_output_tokens": 4096,
                "reasoning_effort": "max", "sampling_id": "sampling", "execution_id": "execution"
            }],
            "inference_sampling": [{
                "agent_did": owner, "sampling_id": "sampling",
                "temperature": 0.2, "top_p": 0.95, "top_k": 40, "seed": 1234,
                "min_p": 0.05, "frequency_penalty": 0.5,
                "presence_penalty": -0.25, "repetition_penalty": 1.1
            }],
            "inference_execution": [{
                "agent_did": owner, "execution_id": "execution", "max_turns": 8,
                "stream_batch_ms": 500, "stream_liveness_timeout_secs": 45,
                "deadline_duration_secs": 120
            }]
        }))
        .unwrap();
        let plan = DesiredStateApplyPlan::from_pack_config(&config).unwrap();
        access
            .transact("test.canonical_config.apply", |txn| {
                let plan = plan.clone();
                Box::pin(async move { apply_desired_state_plan(txn, &plan).await })
            })
            .await
            .unwrap();
        let expected = config.clone();
        access
            .transact("test.canonical_config.read", |txn| {
                let expected = expected.clone();
                Box::pin(async move {
                    for (collection, id, value) in [
                        (
                            Collection::AgentPrincipal,
                            owner,
                            serde_json::to_value(&expected.agent_principal)?,
                        ),
                        (
                            Collection::AgentBehavior,
                            "general",
                            serde_json::to_value(&expected.agent_behaviors[0])?,
                        ),
                        (
                            Collection::AgentContext,
                            "context",
                            serde_json::to_value(&expected.contexts[0])?,
                        ),
                        (
                            Collection::Compaction,
                            "compact",
                            serde_json::to_value(&expected.compactions[0])?,
                        ),
                        (
                            Collection::InferenceBackend,
                            "local",
                            serde_json::to_value(&expected.inference_backends[0])?,
                        ),
                        (
                            Collection::InferenceProfile,
                            "balanced",
                            serde_json::to_value(&expected.inference_profiles[0])?,
                        ),
                        (
                            Collection::InferenceSampling,
                            "sampling",
                            serde_json::to_value(&expected.inference_sampling[0])?,
                        ),
                        (
                            Collection::InferenceExecution,
                            "execution",
                            serde_json::to_value(&expected.inference_execution[0])?,
                        ),
                    ] {
                        let (_, actual) =
                            read_desired_state_record_in_txn(txn, collection, owner, id)
                                .await?
                                .unwrap();
                        // Compare canonical values, including every configured sampling,
                        // execution and context property, through the public read owner.
                        let expected = DesiredStateApplyPlan::new(vec![
                            gents::config_client::DesiredStateApplyDocument {
                                collection,
                                add: value.clone(),
                                update: value,
                            },
                        ])?;
                        assert_eq!(
                            actual,
                            expected.documents()[0].add,
                            "{owner} {collection:?}"
                        );
                    }
                    Ok(())
                })
            })
            .await
            .unwrap();
        let principal = ensure_agent_principal(db.node.as_ref(), owner)
            .await
            .unwrap();
        assert_eq!(
            principal, config.agent_principal,
            "identity bootstrap must preserve authored configuration"
        );
    }
    let rows = access.execute("{ AgentPrincipal { agent_did } AgentBehavior { behavior_id } InferenceProfile { model_name } }").await.unwrap();
    for collection in ["AgentPrincipal", "AgentBehavior", "InferenceProfile"] {
        assert_eq!(rows["data"][collection].as_array().unwrap().len(), 2);
    }
    let mut models = rows["data"]["InferenceProfile"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["model_name"].as_str().unwrap())
        .collect::<Vec<_>>();
    models.sort_unstable();
    assert_eq!(models, ["model-a", "model-b"]);
}

#[tokio::test]
async fn session_create_returns_its_physical_receipt() {
    let db = test_db("session-create-receipt").await;
    ConfigAccess::Local(db.node.clone()).transact("test.session.create_receipt", |txn| {
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input:AgentSessionMutationInputArg!){create_AgentSession(input:$input){_docID}}",
                &json!({"input":{"session_id":"receipt-session","agent_did":"did:test:receipt","requester_did":null,"behavior_id":"general","created_at":"2026-01-01T00:00:00Z","title":null,"provenance":{"fork":{"source_session_id":"parent","at_user_turn":0}}}}),
            ).await?;
            let response = gents::defra_node::QueryResponse::success(response["data"].clone());
            let created = gents::graphql::single_mutation_document(&response, "create_AgentSession")?.expect("create must return a physical document");
            let id = created["_docID"].as_str().expect("physical ID");
            let stored = txn.execute("{ AgentSession { _docID session_id provenance } }").await?;
            assert_eq!(stored["data"]["AgentSession"].as_array().unwrap().len(), 1);
            assert_eq!(stored["data"]["AgentSession"][0]["_docID"], id);
            assert_eq!(stored["data"]["AgentSession"][0]["provenance"]["fork"]["at_user_turn"], 0);
            Ok(())
        })
    }).await.unwrap();
}
