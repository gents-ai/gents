use super::support::*;
use super::*;
use crate::store::config_client::{ConfigAccess, NodePermission};
use crate::store::identity::NodeIdentity;

struct PrivateSessionFixture {
    node: Arc<defra_node::Node>,
    runtime: crate::store::identity::KeyIdentity,
    user: crate::store::identity::KeyIdentity,
    selected: crate::store::identity::KeyIdentity,
    other: crate::store::identity::KeyIdentity,
}

impl PrivateSessionFixture {
    async fn new() -> Self {
        let runtime = test_identity("selection-runtime");
        let user = test_identity("selection-user");
        let selected = test_identity("selection-agent");
        let other = test_identity("selection-other-agent");
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_enabled()
                .build()
                .await
                .unwrap(),
        );
        ensure_runtime_schemas(&node).await.unwrap();
        let admin = ConfigAccess::local_as(node.clone(), runtime.did().parse().unwrap());
        for did in [user.did(), selected.did(), other.did()] {
            for permission in [
                NodePermission::DocumentRead,
                NodePermission::DocumentUpdate,
                NodePermission::DacRelationAdd,
            ] {
                admin
                    .grant_node_permission(&did.parse().unwrap(), permission)
                    .await
                    .unwrap();
            }
        }
        Self {
            node,
            runtime,
            user,
            selected,
            other,
        }
    }

    async fn create_session(&self, session: &str, bound: &str, selected_can_read: bool) {
        let owner = ConfigAccess::local_as(self.node.clone(), self.user.did().parse().unwrap());
        let created = owner.write("test.router.private_session", &format!(
            r#"mutation {{ create_AgentSession(input: {{ node_did: "{}", session_id: "{}", requester_did: "{}", agent_did: "{}", created_at: "2026-11-04T00:00:00Z" }}) {{ _docID }} }}"#,
            escape_graphql_string(self.runtime.did()), escape_graphql_string(session),
            escape_graphql_string(self.user.did()), escape_graphql_string(bound),
        )).await.unwrap();
        let doc = crate::store::graphql::created_doc_id(&created, "AgentSession").unwrap();
        if selected_can_read {
            owner
                .add_actor_relationship(
                    "AgentSession",
                    &doc,
                    "working_agent",
                    &self.selected.did().parse().unwrap(),
                )
                .await
                .unwrap();
        }
    }

    fn request(&self, session: &str) -> AgentRequest {
        let mut request = request(Some(self.selected.did()), session);
        request.node_did = self.runtime.did().to_owned();
        request.requester_did = Some(self.user.did().to_owned());
        request
    }
}

#[tokio::test]
async fn resolve_agent_rejects_missing_or_blank_explicit_selection() {
    let (db, _user, _access, _binding) =
        crate::test_support::signed_test_node("agent-resolution-blank").await;
    let node = db.node.clone();
    for selected in [None, Some(""), Some("   ")] {
        assert!(
            resolve_agent_for_request(&node, &request(selected, "session"))
                .await
                .is_err()
        );
    }
    node.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_agent_resolution_matches_lean_binding_cases() {
    for name in [
        "default-A-explicit-B-binds-B",
        "existing-A-requested-B-rejected",
        "existing-A-requested-A-accepted",
    ] {
        let case = crate::lean_vocab_test::lean_runtime_reconcile_case(name);
        let fixture = PrivateSessionFixture::new().await;
        let bound = if case.pre_session_agent.is_some() && !case.legal {
            fixture.other.did()
        } else {
            fixture.selected.did()
        };
        fixture.create_session("session-binding", bound, true).await;
        let resolved =
            resolve_agent_for_request(&fixture.node, &fixture.request("session-binding"))
                .await
                .unwrap()
                .expect("the agent can read the session");
        assert_eq!(resolved.agent_did, fixture.selected.did(), "{name}");
        assert_eq!(resolved.rejection_reason.is_none(), case.legal, "{name}");
        fixture.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn private_session_selection_is_actor_scoped_and_matches_lean() {
    let fixture = PrivateSessionFixture::new().await;
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().runtime_session_selection_cases;
    assert_eq!(cases.len(), 3);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let bound = if name == "mismatched" {
            fixture.other.did()
        } else {
            fixture.selected.did()
        };
        fixture
            .create_session(name, bound, name != "invisible")
            .await;
        let node_view =
            ConfigAccess::local_as(fixture.node.clone(), fixture.runtime.did().parse().unwrap())
                .execute(&format!(
                    r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                    escape_graphql_string(name)
                ))
                .await
                .unwrap();
        assert!(
            node_view["data"]["AgentSession"]
                .as_array()
                .unwrap()
                .is_empty(),
            "runtime must not read private session"
        );
        let result = resolve_agent_for_request(&fixture.node, &fixture.request(name))
            .await
            .unwrap();
        let observed = match result {
            None => "unavailable",
            Some(value) if value.rejection_reason.is_some() => "mismatched",
            Some(_) => "matched",
        };
        assert_eq!(observed, case["expected"].as_str().unwrap(), "{name}");
    }
    assert!(
        resolve_agent_for_request(&fixture.node, &fixture.request("missing"))
            .await
            .unwrap()
            .is_none()
    );
    fixture.node.shutdown().await.unwrap();
}
