use super::super::router::session_selection::{
    session_selection_disposition, SessionSelection, SessionSelectionDisposition,
    UnresolvedSessions,
};
use super::support::*;
use super::*;
use crate::store::config_client::{ConfigAccess, NodePermission};
use crate::store::identity::{GrantSet, NodeIdentity};
use gents_protocol::request_admission::RequestPurpose;

struct PrivateSessionFixture {
    db: crate::support::TestDb,
    runtime: Arc<dyn NodeIdentity>,
    user: crate::store::identity::KeyIdentity,
    selected: crate::store::identity::KeyIdentity,
    other: crate::store::identity::KeyIdentity,
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
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().runtime_session_selection_cases;
    assert_eq!(cases.len(), 5);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let expected = case["expected"].as_str().unwrap();
        let bound = if expected == "mismatched" {
            fixture.other.did()
        } else {
            fixture.selected.did()
        };
        fixture
            .create_session(name, bound, expected != "unavailable")
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
        assert_eq!(observed, expected, "{name}");
    }
    assert!(
        resolve_agent_for_request(&fixture.db.node, &fixture.request("missing"))
            .await
            .unwrap()
            .is_none()
    );
    fixture.db.node.shutdown().await.unwrap();
}

#[test]
fn generated_session_selection_disposition_cases_match_lean() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().runtime_session_selection_cases;
    assert_eq!(cases.len(), 5);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let selection = match case["expected"].as_str().unwrap() {
            "unavailable" => SessionSelection::Unavailable,
            "matched" => SessionSelection::Matched,
            "mismatched" => SessionSelection::Mismatched,
            other => panic!("unmodeled session selection {other}"),
        };
        let observed = match session_selection_disposition(
            selection,
            case["attempts"].as_u64().unwrap() as u32,
            case["bound"].as_u64().unwrap() as u32,
        ) {
            SessionSelectionDisposition::Claim => "claim",
            SessionSelectionDisposition::Wait => "wait",
            SessionSelectionDisposition::Reject => "reject",
        };
        assert_eq!(
            observed,
            case["expected_disposition"].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn unresolved_session_attempts_count_consecutive_observations_per_request() {
    let mut unresolved = UnresolvedSessions::default();
    assert_eq!(unresolved.record("a"), 1);
    assert_eq!(unresolved.record("b"), 1);
    assert_eq!(unresolved.record("a"), 2);
    unresolved.forget("a");
    assert_eq!(unresolved.record("a"), 1);
    assert_eq!(unresolved.record("b"), 2);
}

/// An auxiliary request carries the selected Agent as its requester, so the
/// router must select its session by node and session id alone; the session's
/// own requester is the client that opened it.
#[tokio::test]
async fn auxiliary_request_resolves_a_client_opened_session() {
    let fixture = PrivateSessionFixture::new().await;
    fixture
        .create_session("aux", fixture.selected.did(), true)
        .await;
    let mut request = fixture.request("aux");
    request.purpose = RequestPurpose::TitleAudit;
    request.requester_did = Some(fixture.selected.did().to_owned());
    let resolved = resolve_agent_for_request(&fixture.db.node, &request)
        .await
        .unwrap()
        .expect("an auxiliary request resolves its own session");
    assert_eq!(resolved.agent_did, fixture.selected.did());
    assert_eq!(resolved.rejection_reason, None);

    fixture
        .create_session("aux-foreign", fixture.other.did(), true)
        .await;
    let mut foreign = fixture.request("aux-foreign");
    foreign.purpose = RequestPurpose::TitleAudit;
    foreign.requester_did = Some(fixture.selected.did().to_owned());
    let resolved = resolve_agent_for_request(&fixture.db.node, &foreign)
        .await
        .unwrap()
        .expect("a foreign session is visible to the auxiliary request");
    assert!(
        resolved.rejection_reason.is_some(),
        "the session pin still rejects an auxiliary request for another agent"
    );
    fixture.db.node.shutdown().await.unwrap();
}
