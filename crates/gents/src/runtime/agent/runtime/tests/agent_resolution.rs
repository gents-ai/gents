use super::support::*;
use super::*;
use gents_store::config_client::{ConfigAccess, NodePermission};
use gents_store::identity::{GrantSet, NodeIdentity};

struct PrivateSessionFixture {
    db: crate::support::TestDb,
    runtime: Arc<dyn NodeIdentity>,
    user: gents_store::identity::KeyIdentity,
    selected: gents_store::identity::KeyIdentity,
    other: gents_store::identity::KeyIdentity,
}

impl PrivateSessionFixture {
    async fn new() -> Self {
        let db = crate::support::test_db("private-session-selection").await;
        let runtime = db.node_identity.clone();
        let user = test_identity("selection-user");
        let selected = test_identity("selection-agent");
        let other = test_identity("selection-other-agent");
        let admin = ConfigAccess::local_as(db.node.clone(), runtime.did().parse().unwrap());
        GrantSet::OperatorUser
            .grant(&admin, &identity::Did::new(user.did()).unwrap())
            .await
            .unwrap();
        for did in [selected.did(), other.did()] {
            for permission in [
                NodePermission::DocumentRead,
                NodePermission::DocumentUpdate,
                NodePermission::DacRelationAdd,
            ] {
                admin
                    .grant_node_permission(&identity::Did::new(did).unwrap(), permission)
                    .await
                    .unwrap();
            }
        }
        Self {
            db,
            runtime,
            user,
            selected,
            other,
        }
    }

    async fn create_session(&self, session: &str, bound: &str, selected_can_read: bool) {
        let access = self.db.client_access(&self.user);
        create_user_agent_session(
            &access,
            self.runtime.did(),
            Some(self.user.did()),
            session,
            bound,
            selected_can_read.then(|| self.selected.did()),
        )
        .await;
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
        crate::test_support::root::signed_test_node("agent-resolution-blank").await;
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
        let case = gents_lean_vocab::lean_runtime_reconcile_case(name);
        let fixture = PrivateSessionFixture::new().await;
        let bound = if case.pre_session_agent.is_some() && !case.legal {
            fixture.other.did()
        } else {
            fixture.selected.did()
        };
        fixture.create_session("session-binding", bound, true).await;
        let resolved =
            resolve_agent_for_request(&fixture.db.node, &fixture.request("session-binding"))
                .await
                .unwrap()
                .expect("the agent can read the session");
        assert_eq!(resolved.agent_did, fixture.selected.did(), "{name}");
        assert_eq!(resolved.rejection_reason.is_none(), case.legal, "{name}");
        fixture.db.node.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn private_session_selection_is_actor_scoped_and_matches_lean() {
    let fixture = PrivateSessionFixture::new().await;
    let cases = &gents_lean_vocab::lean_contract_snapshot().runtime_session_selection_cases;
    assert_eq!(cases.len(), 4);
    for case in cases {
        let name = case.name.as_str();
        if name == "materialized" {
            continue;
        }
        let bound = if name == "mismatched" {
            fixture.other.did()
        } else {
            fixture.selected.did()
        };
        fixture
            .create_session(name, bound, name != "invisible")
            .await;
        let node_view = ConfigAccess::local_as(
            fixture.db.node.clone(),
            fixture.runtime.did().parse().unwrap(),
        )
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
        let result = resolve_agent_for_request(&fixture.db.node, &fixture.request(name))
            .await
            .unwrap();
        let observed = match result {
            None => "unavailable",
            Some(value) if value.rejection_reason.is_some() => "mismatched",
            Some(_) => "matched",
        };
        assert_eq!(observed, case.expected.as_str(), "{name}");
    }
    assert!(
        resolve_agent_for_request(&fixture.db.node, &fixture.request("missing"))
            .await
            .unwrap()
            .is_none()
    );
    fixture.db.node.shutdown().await.unwrap();
}

/// Lean `RuntimeState.materializedSessionVisible`: a runtime request written by
/// the production pending-request owner carries its session, so the router's
/// Agent-scoped selection finds it instead of leaving the request pending.
#[tokio::test]
async fn materialized_request_session_is_selectable_and_matches_lean() {
    let case = gents_lean_vocab::lean_contract_snapshot()
        .runtime_session_selection_cases
        .iter()
        .find(|case| case.name == "materialized")
        .cloned()
        .expect("modeled materialized session selection");
    let (db, _user, _access, binding) =
        crate::test_support::root::signed_test_node("materialized-session-selection").await;
    let node_did = db.node_identity.did().to_owned();
    let agent =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding).unwrap();
    let enqueued =
        gents_session::request::lifecycle::materialize::write_pending_agent_request_with_identity(
            &db.node,
            identity::Did::new(agent.did()).unwrap(),
            &node_did,
            agent.did(),
            "materialized selection probe",
            gents_session::request::lifecycle::ExecutionOrigin::Interactive,
            gents_session::request::lifecycle::TriggerLineage {
                trigger_id: None,
                trigger_kind: Some("manual".into()),
                source_doc_id: None,
                correlation: None,
                trigger_context: None,
            },
            Some("materialized probe"),
            None,
            None,
            None,
            None,
            None,
            Some(agent.as_ref()),
        )
        .await
        .unwrap();
    let mut routed = request(Some(agent.did()), &enqueued.session_id);
    routed.node_did = node_did;
    routed.requester_did = Some(agent.did().to_owned());
    let observed = match resolve_agent_for_request(&db.node, &routed).await.unwrap() {
        None => "unavailable",
        Some(value) if value.rejection_reason.is_some() => "mismatched",
        Some(_) => "matched",
    };
    assert_eq!(observed, case.expected.as_str());
    db.node.shutdown().await.unwrap();
}
