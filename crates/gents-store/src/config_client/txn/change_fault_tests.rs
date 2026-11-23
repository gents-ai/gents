use std::sync::Arc;

use serde_json::json;

use super::{RelationshipChange, RelationshipChangeKind, RelationshipMatch};
use crate::config_client::{ConfigAccess, ConfigApplyTxn, ConfigClientError};
use crate::graphql::escape_graphql_string;
use crate::identity::{KeyIdentity, NodeIdentity};

struct World {
    _home: tempfile::TempDir,
    node_did: String,
    agent_did: String,
    agent: ConfigAccess,
    reader: ConfigAccess,
    reader_did: identity::Did,
    own_session: String,
    foreign_session: String,
}

async fn session(access: &ConfigAccess, node_did: &str, agent_did: &str, id: &str) -> String {
    let created = access
        .write(
            "test.change_fault.session",
            &format!(
                r#"mutation {{ create_AgentSession(input: {{node_did: "{}", session_id: "{}", agent_did: "{}", created_at: "2026-11-04T00:00:00Z"}}) {{_docID}} }}"#,
                escape_graphql_string(node_did),
                escape_graphql_string(id),
                escape_graphql_string(agent_did)
            ),
        )
        .await
        .unwrap();
    crate::graphql::created_doc_id(&created, "AgentSession").unwrap()
}

async fn world() -> World {
    let home = tempfile::tempdir().unwrap();
    let runtime = KeyIdentity::load_or_create(home.path().join("node"), None).unwrap();
    let node = Arc::new(
        defra_node::Node::builder()
            .with_node_identity_did(runtime.did())
            .build()
            .await
            .unwrap(),
    );
    crate::schema::ensure_runtime_schemas(&node).await.unwrap();
    let managed = |access_node: &Arc<defra_node::Node>| {
        let binding = crate::identity::provision_agent_identity(&runtime).unwrap();
        let agent = crate::identity::resolve_agent_identity(&runtime, &binding).unwrap();
        let did = agent.did().to_owned();
        let access = ConfigAccess::local_managed_agent(
            access_node.clone(),
            identity::Did::new(&did).unwrap(),
            agent.as_ref(),
        )
        .unwrap();
        (did, access)
    };
    let (agent_did, agent) = managed(&node);
    let (other_did, other) = managed(&node);
    let reader_key = KeyIdentity::load_or_create(home.path().join("reader"), None).unwrap();
    let reader_did = identity::Did::new(reader_key.did()).unwrap();
    let reader = ConfigAccess::local_as(node.clone(), reader_did.clone());
    let own_session = session(&agent, runtime.did(), &agent_did, "own").await;
    let foreign_session = session(&other, runtime.did(), &other_did, "foreign").await;
    World {
        node_did: runtime.did().to_owned(),
        _home: home,
        agent_did,
        agent,
        reader,
        reader_did,
        own_session,
        foreign_session,
    }
}

impl World {
    async fn publish(&self, key: &str, session_doc: &str) -> anyhow::Result<()> {
        self.agent
            .transact_write("test.change_fault.publish", |txn| {
                Box::pin(async move {
                    txn.execute_with_variables(
                        "mutation($input: AgentMessageMutationInputArg!) { create_AgentMessage(input: $input) { _docID } }",
                        &json!({"input": {"node_did": self.node_did, "session_id": "own",
                            "session_doc_id": self.own_session, "agent_did": self.agent_did,
                            "creator_did": self.agent_did, "parent_linked": false, "message_key": key,
                            "sequence": 1, "role": "assistant", "created_at": "2026-11-04T00:00:00Z"}}),
                    )
                    .await?;
                    txn.add_actor_relationship("AgentSession", session_doc, "reader", &self.reader_did)
                        .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
    }

    async fn message(&self, key: &str) -> Option<(String, bool)> {
        let rows = self
            .agent
            .execute(&format!(
                r#"{{ AgentMessage(filter: {{message_key: {{_eq: "{}"}}}}) {{_docID parent_linked}} }}"#,
                escape_graphql_string(key)
            ))
            .await
            .unwrap();
        let row = rows["data"]["AgentMessage"].as_array()?.first()?.clone();
        Some((
            row["_docID"].as_str()?.to_owned(),
            row["parent_linked"].as_bool()?,
        ))
    }

    async fn reader_sees_own_session(&self) -> bool {
        let rows = self
            .reader
            .execute(&format!(
                r#"{{ AgentSession(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID}} }}"#,
                escape_graphql_string(&self.own_session)
            ))
            .await
            .unwrap();
        rows["data"]["AgentSession"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    }

    async fn mark(
        &self,
        access: &ConfigAccess,
        doc: &str,
    ) -> Result<serde_json::Value, ConfigClientError> {
        access
            .write(
                "test.change_fault.mark",
                &format!(
                    r#"mutation {{ update_AgentMessage(docID: "{}", input: {{parent_linked: true}}) {{_docID}} }}"#,
                    escape_graphql_string(doc)
                ),
            )
            .await
    }
}

fn holds(error: &anyhow::Error, matches: impl Fn(&ConfigClientError) -> bool) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<ConfigClientError>())
        .any(matches)
}

#[tokio::test]
async fn an_injected_relationship_failure_aborts_like_a_native_refusal() {
    let world = world().await;

    let refused = world
        .publish("refused", &world.foreign_session)
        .await
        .expect_err("native ACP refuses a relationship on another Agent's session");
    assert!(
        holds(&refused, |error| matches!(
            error,
            ConfigClientError::RelationshipNotManaged(_)
        )),
        "{refused:#}"
    );
    assert_eq!(world.message("refused").await, None);

    let (injected, trace) = ConfigApplyTxn::with_relationship_change_failure(
        Some(RelationshipMatch::first("AgentSession", "reader")),
        world.publish("injected", &world.own_session),
    )
    .await;
    let injected = injected.expect_err("the selected relationship change fails");
    assert!(
        holds(&injected, |error| matches!(
            error,
            ConfigClientError::InjectedRelationshipFailure { .. }
        )),
        "{injected:#}"
    );
    assert_eq!(
        trace,
        [RelationshipChange {
            operation: Some("test.change_fault.publish"),
            collection: "AgentSession".into(),
            relation: "reader".into(),
            kind: RelationshipChangeKind::Add,
        }]
    );
    assert_eq!(world.message("injected").await, None);
    assert!(!world.reader_sees_own_session().await);

    world
        .publish("committed", &world.own_session)
        .await
        .unwrap();
    assert!(world.message("committed").await.is_some());
    assert!(world.reader_sees_own_session().await);
}

#[tokio::test]
async fn an_injected_auto_commit_failure_leaves_the_document_like_a_native_refusal() {
    let world = world().await;
    world.publish("marked", &world.own_session).await.unwrap();
    let (doc, linked) = world.message("marked").await.unwrap();
    assert!(!linked);

    let _ = world.mark(&world.reader, &doc).await;
    assert_eq!(world.message("marked").await, Some((doc.clone(), false)));

    let (injected, fired) = ConfigApplyTxn::with_auto_commit_failure(
        "test.change_fault.mark",
        world.mark(&world.agent, &doc),
    )
    .await;
    assert!(fired);
    assert!(matches!(
        injected,
        Err(ConfigClientError::InjectedAutoCommitFailure {
            operation: "test.change_fault.mark"
        })
    ));
    assert_eq!(world.message("marked").await, Some((doc.clone(), false)));

    world.mark(&world.agent, &doc).await.unwrap();
    assert_eq!(world.message("marked").await, Some((doc, true)));
}
