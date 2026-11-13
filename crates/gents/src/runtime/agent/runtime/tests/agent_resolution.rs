use super::super::router::session_selection::{
    session_selection_disposition, SessionSelection, SessionSelectionDisposition,
    UnresolvedSessions,
};
use super::support::*;
use super::*;
use gents_protocol::request_admission::RequestPurpose;
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

    fn request(&self, session: &str, agent: &str) -> AgentRequest {
        let mut request = request(Some(agent), session);
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
    for case in gents_lean_vocab::families::contracts_runtime_reconcile_selection::cases() {
        let input = &case.input;
        let fixture = PrivateSessionFixture::new().await;
        let bound = match input.pre_session_agent {
            Some(agent) if Some(agent) != input.requested_agent => fixture.other.did(),
            _ => fixture.selected.did(),
        };
        fixture.create_session("session-binding", bound, true).await;
        let resolved = resolve_agent_for_request(
            &fixture.db.node,
            &fixture.request("session-binding", fixture.selected.did()),
        )
        .await
        .unwrap()
        .expect("the agent can read the session");
        assert_eq!(resolved.agent_did, fixture.selected.did(), "{}", input.name);
        fixture.db.node.shutdown().await.unwrap();
        case.check(
            gents_lean_vocab::families::contracts_runtime_reconcile_selection::ReconcileSelectionOutput {
                legal: resolved.rejection_reason.is_none(),
            },
        );
    }
}

fn fixture_agent(fixture: &PrivateSessionFixture, id: u64) -> &str {
    match id {
        10 => fixture.selected.did(),
        20 => fixture.other.did(),
        other => panic!("the fixture has no agent {other}"),
    }
}

#[tokio::test]
async fn private_session_selection_is_actor_scoped_and_matches_lean() {
    let fixture = PrivateSessionFixture::new().await;
    let cases =
        gents_lean_vocab::families::contract_cases_runtime_session_selection::cases::<String>();
    for case in &cases {
        let name = case.input.name.as_str();
        let requested = fixture_agent(&fixture, case.input.requested);
        let bound = match case.input.visible {
            Some(id) => fixture_agent(&fixture, id),
            None => fixture.selected.did(),
        };
        fixture
            .create_session(name, bound, case.input.visible.is_some())
            .await;
        let node_view = ConfigAccess::local_as(
            fixture.db.node.clone(),
            fixture.runtime.did().parse().unwrap(),
        )
        .execute(&format!(
            r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(&name)
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
        let result = resolve_agent_for_request(&fixture.db.node, &fixture.request(name, requested))
            .await
            .unwrap();
        let observed = match result {
            None => "unavailable",
            Some(value) if value.rejection_reason.is_some() => "mismatched",
            Some(_) => "matched",
        };
        case.check(observed.to_owned());
    }
    assert!(resolve_agent_for_request(
        &fixture.db.node,
        &fixture.request("missing", fixture.selected.did())
    )
    .await
    .unwrap()
    .is_none());
    fixture.db.node.shutdown().await.unwrap();
}

fn check_selection_case(
    case: &gents_lean_vocab::family::Case<
        gents_lean_vocab::families::runtime_session_selection::SelectionInput,
        gents_lean_vocab::families::runtime_session_selection::SelectionOutput<String, String>,
    >,
    selection: SessionSelection,
) {
    let disposition = session_selection_disposition(
        selection,
        case.input.attempts.try_into().unwrap(),
        case.input.bound.try_into().unwrap(),
    );
    case.check(
        gents_lean_vocab::families::runtime_session_selection::SelectionOutput {
            selection: match selection {
                SessionSelection::Unavailable => "unavailable",
                SessionSelection::Matched => "matched",
                SessionSelection::Mismatched => "mismatched",
            }
            .to_owned(),
            disposition: match disposition {
                SessionSelectionDisposition::Claim => "claim",
                SessionSelectionDisposition::Wait => "wait",
                SessionSelectionDisposition::Reject => "reject",
            }
            .to_owned(),
        },
    );
}

#[test]
fn generated_session_selection_disposition_cases_match_lean() {
    use gents_lean_vocab::families::runtime_session_selection as family;
    for case in family::cases::<String, String>() {
        let selection = match case.input.visible {
            None => SessionSelection::Unavailable,
            Some(visible) if visible == case.input.requested => SessionSelection::Matched,
            Some(_) => SessionSelection::Mismatched,
        };
        check_selection_case(&case, selection);
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

#[tokio::test]
async fn auxiliary_request_resolves_a_client_opened_session() {
    let fixture = PrivateSessionFixture::new().await;
    fixture
        .create_session("aux", fixture.selected.did(), true)
        .await;
    let mut request = fixture.request("aux", fixture.selected.did());
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
    let mut foreign = fixture.request("aux-foreign", fixture.selected.did());
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

/// Lean `RuntimeState.materializedSessionVisible`: a runtime request written by
/// the production pending-request owner carries its session, so the router's
/// Agent-scoped selection finds it instead of leaving the request pending.
#[tokio::test]
async fn materialized_request_session_is_selectable_and_matches_lean() {
    let cases =
        gents_lean_vocab::families::contract_cases_runtime_materialized_session_selection::cases::<
            String,
        >();
    assert!(!cases.is_empty(), "modeled materialized session selection");
    for case in &cases {
        assert!(
            case.input.published_with_request,
            "the pending-request owner always publishes the session: {}",
            case.input.name
        );
        let (db, _user, _access, binding) =
            crate::test_support::root::signed_test_node("materialized-session-selection").await;
        let node_did = db.node_identity.did().to_owned();
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
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
        case.check(observed.to_owned());
        db.node.shutdown().await.unwrap();
    }
}
