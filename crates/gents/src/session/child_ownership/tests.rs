use super::*;
use crate::store::identity::{KeyIdentity, NodeIdentity};
use serde_json::json;

#[tokio::test]
async fn publication_matches_atomic_session_child_model() {
    for case in &crate::lean_vocab_test::lean_contract_snapshot().session_child_cases {
        let name = case["name"].as_str().unwrap();
        let home = tempfile::tempdir().unwrap();
        let runtime = KeyIdentity::load_or_create(home.path().join("node"), None).unwrap();
        let binding = crate::store::identity::provision_agent_identity(&runtime).unwrap();
        let agent = crate::store::identity::resolve_agent_identity(&runtime, &binding).unwrap();
        let reader = KeyIdentity::load_or_create(home.path().join("reader"), None).unwrap();
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let actor = identity::Did::new(agent.did()).unwrap();
        let access = ConfigAccess::local_managed_agent(node.clone(), actor.clone(), agent.as_ref()).unwrap();
        let reader_access =
            ConfigAccess::local_as(node.clone(), identity::Did::new(reader.did()).unwrap());
        let session = access.write("test.child.session", &format!(
            r#"mutation {{ create_AgentSession(input: {{node_did: "{}", session_id: "child-test", agent_did: "{}", created_at: "2026-11-04T00:00:00Z"}}) {{_docID}} }}"#, runtime.did(), agent.did())).await.unwrap();
        let session_doc = crate::store::graphql::created_doc_id(&session, "AgentSession").unwrap();
        access
            .add_actor_relationship(
                "AgentSession",
                &session_doc,
                "reader",
                &identity::Did::new(reader.did()).unwrap(),
            )
            .await
            .unwrap();
        let creator = if name == "wrong_owner" {
            reader.did()
        } else {
            agent.did()
        };
        let runtime_did = runtime.did().to_owned();
        let agent_did = agent.did().to_owned();
        let message = access.transact_write("test.child.create", |txn| {
            let session_doc = &session_doc;
            let runtime_did = &runtime_did;
            let agent_did = &agent_did;
            Box::pin(async move {
                let response = txn.execute_with_variables(
                    "mutation($input: AgentMessageMutationInputArg!) { create_AgentMessage(input: $input) { _docID } }",
                    &json!({"input": {"node_did": runtime_did, "session_id": "child-test", "session_doc_id": session_doc,
                        "agent_did": agent_did, "creator_did": creator, "parent_linked": case["parent_linked"],
                        "message_key": "child-test", "sequence": 1, "role": "assistant", "created_at": "2026-11-04T00:00:00Z"}}),
                ).await?;
                Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(&response, "AgentMessage")?)
            })
        }).await.unwrap();
        if name == "grant_failed" {
            let result = reader_access
                .transact_write("test.child.denied_grant", |txn| {
                    let message = &message;
                    let session_doc = &session_doc;
                    Box::pin(async move {
                        txn.add_resource_relationship(
                            "AgentMessage",
                            message,
                            "parent",
                            "sessions",
                            session_doc,
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await;
            assert!(result.is_err());
        } else if name == "complete" {
            let reached = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let paused = ConfigApplyTxn::with_successful_mutation_pause_at(
                1,
                reached.clone(),
                release.clone(),
                publish_agent_session_child_relationships_with_access(
                    &access,
                    runtime.did(),
                    &actor,
                    "child-test",
                ),
            );
            tokio::pin!(paused);
            tokio::select! {
                _ = reached.notified() => {},
                _ = &mut paused => panic!("foreground publication did not reach its marker"),
            }
            recover_agent_session_child_relationships_with_access(&access, runtime.did(), &actor)
                .await
                .unwrap();
            release.notify_one();
            let (result, hit) = paused.await;
            assert!(hit);
            result.unwrap();
        } else {
            let future = publish_agent_session_child_relationships_with_access(
                &access,
                runtime.did(),
                &actor,
                "child-test",
            );
            if name == "mark_failed" {
                let (result, count) =
                    ConfigApplyTxn::with_successful_mutation_failure_at(Some(1), future).await;
                assert!(result.is_err());
                assert_eq!(count, 1);
            } else {
                future.await.unwrap();
            }
        }
        let query = format!(
            r#"{{ AgentMessage(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID parent_linked}} }}"#,
            escape_graphql_string(&message)
        );
        let actual = access.execute(&query).await.unwrap();
        assert_eq!(
            actual["data"]["AgentMessage"][0]["parent_linked"], case["expected_linked"],
            "{name}"
        );
        let visible = reader_access.execute(&query).await.unwrap()["data"]["AgentMessage"]
            .as_array()
            .unwrap()
            .len()
            == 1;
        assert_eq!(
            visible,
            case["expected_granted"].as_bool().unwrap(),
            "{name}"
        );
        node.shutdown().await.unwrap();
    }
}
