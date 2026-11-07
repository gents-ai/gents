use gents::store::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
use gents::{ensure_node, Collection};
use serde_json::{json, Value};

use crate::support::agent_requests::RequestActors;
use crate::support::{managed_agent_fixture, test_db};

#[tokio::test]
async fn canonical_config_roundtrips_scoped_references_and_preserves_explicit_default() {
    let db = test_db("document-config-scoped-roundtrip").await;
    // The enrolled User creates the Node and its Agent (with its creation
    // nonce) and authors the rest of the configuration through its client.
    let fixture = managed_agent_fixture(&db).await;
    let access = &fixture.access;
    let owner = db.node_identity.did();
    let agent_did = fixture.binding.agent_did.as_str();
    let read = |collection: Collection, node_did: String, id: String| async move {
        access
            .transact_readonly("test.canonical_config.read", |txn| {
                let (node_did, id) = (&node_did, &id);
                Box::pin(async move {
                    Ok::<_, anyhow::Error>(
                        read_desired_state_record_in_txn(txn, collection, node_did, id)
                            .await?
                            .map(|(_, value)| value),
                    )
                })
            })
            .await
            .unwrap()
    };
    let mut agent = read(Collection::Agent, owner.into(), agent_did.into())
        .await
        .expect("the User's Agent");
    agent["context_id"] = "context".into();
    agent["inference_profile_id"] = "balanced".into();
    let documents: Vec<(Collection, &str, Value)> = vec![
        (
            Collection::Node,
            owner,
            json!({
                "node_did": owner, "display_name": "Explicit configuration",
                "default_agent_did": agent_did, "tags": ["authored"]
            }),
        ),
        (
            Collection::Compaction,
            "compact",
            json!({
                "node_did": owner, "compaction_id": "compact",
                "strategy": "StripThenSummarize", "threshold": 0.6
            }),
        ),
        (
            Collection::AgentContext,
            "context",
            json!({
                "node_did": owner, "context_id": "context",
                "system_prompt": "Be precise. {{literal}}", "compaction_id": "compact"
            }),
        ),
        (
            Collection::InferenceBackend,
            "local",
            json!({
                "node_did": owner, "backend_id": "local", "name": "Local",
                "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                "auth": {"kind": "unauthenticated"}, "max_concurrent": 1, "max_queue_depth": 0
            }),
        ),
        (
            Collection::InferenceSampling,
            "sampling",
            json!({
                "node_did": owner, "sampling_id": "sampling",
                "temperature": 0.2, "top_p": 0.95, "top_k": 40, "seed": 1234,
                "min_p": 0.05, "frequency_penalty": 0.5,
                "presence_penalty": -0.25, "repetition_penalty": 1.1
            }),
        ),
        (
            Collection::InferenceExecution,
            "execution",
            json!({
                "node_did": owner, "execution_id": "execution", "max_turns": 8,
                "stream_batch_ms": 500, "stream_liveness_timeout_secs": 45,
                "deadline_duration_secs": 120
            }),
        ),
        (
            Collection::InferenceProfile,
            "balanced",
            json!({
                "node_did": owner, "profile_id": "balanced", "display_name": "Balanced",
                "backend_id": "local", "model_name": "model-a",
                "context_window": 32768, "max_output_tokens": 4096,
                "reasoning_effort": "max", "sampling_id": "sampling", "execution_id": "execution"
            }),
        ),
        (Collection::Agent, agent_did, agent),
    ];
    let plan = DesiredStateApplyPlan::new(
        documents
            .iter()
            .map(|(collection, _, value)| DesiredStateApplyDocument {
                collection: *collection,
                add: value.clone(),
                update: value.clone(),
            })
            .collect(),
    )
    .unwrap();
    access
        .transact_write("test.canonical_config.apply", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await
        .unwrap();
    for (collection, id, value) in &documents {
        let actual = read(*collection, owner.into(), (*id).into())
            .await
            .unwrap_or_else(|| panic!("{collection:?} {id} roundtrips"));
        let expected = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: *collection,
            add: value.clone(),
            update: value.clone(),
        }])
        .unwrap();
        assert_eq!(
            actual,
            expected.documents()[0].add,
            "{owner} {collection:?}"
        );
        // Logical IDs are scoped by their Node: the same ID under another
        // Node names nothing here.
        let other = "did:key:zOtherNode";
        let other_id = if *collection == Collection::Node {
            other
        } else {
            id
        };
        assert_eq!(
            read(*collection, other.into(), other_id.into()).await,
            None,
            "{collection:?} {id} is scoped to {owner}"
        );
    }
    let principal = ensure_node(&db.node, owner).await.unwrap();
    assert_eq!(
        principal,
        serde_json::from_value(documents[0].2.clone()).unwrap(),
        "identity bootstrap must preserve authored configuration"
    );
    let rows = access
        .execute("{ Node { node_did } Agent { agent_did } InferenceProfile { model_name } }")
        .await
        .unwrap();
    for collection in ["Node", "Agent"] {
        assert_eq!(rows["data"][collection].as_array().unwrap().len(), 1);
    }
    let mut models = rows["data"]["InferenceProfile"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["model_name"].as_str().unwrap())
        .collect::<Vec<_>>();
    models.sort_unstable();
    assert_eq!(models, ["model-a", "test-model"]);
}

#[tokio::test]
async fn session_create_returns_its_physical_receipt() {
    let db = test_db("session-create-receipt").await;
    let actors = RequestActors::new(&db).await;
    let input = json!({
        "session_id": "receipt-session",
        "node_did": actors.node_did(),
        "requester_did": actors.agent_did(),
        "agent_did": actors.agent_did(),
        "created_at": crate::support::agent_requests::canonical_now(),
        "title": null,
        "relationships_pending": false,
        "provenance": {"fork": {"source_session_id": "parent", "at_user_turn": 0}},
    });
    actors.agent_access.transact_write("test.session.create_receipt", |txn| {
        let input = &input;
        Box::pin(async move {
            let response = txn.execute_with_variables(
                "mutation($input:AgentSessionMutationInputArg!){create_AgentSession(input:$input){_docID}}",
                &json!({"input": input}),
            ).await?;
            let response = gents::defra_node::QueryResponse::success(response["data"].clone());
            let created = gents::store::graphql::single_mutation_document(&response, "create_AgentSession")?.expect("create must return a physical document");
            let id = created["_docID"].as_str().expect("physical ID");
            let stored = txn.execute("{ AgentSession { _docID session_id provenance } }").await?;
            assert_eq!(stored["data"]["AgentSession"].as_array().unwrap().len(), 1);
            assert_eq!(stored["data"]["AgentSession"][0]["_docID"], id);
            assert_eq!(stored["data"]["AgentSession"][0]["provenance"]["fork"]["at_user_turn"], 0);
            Ok::<_, anyhow::Error>(())
        })
    }).await.unwrap();
}
