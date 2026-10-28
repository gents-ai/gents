use super::super::session_hydration::{
    AppliedPairingRoute, SessionOwner, VerifiedActiveMembership,
};
use super::*;

mod inventory;

#[test]
fn generated_cases_drive_exact_tool_selection() {
    #[derive(Deserialize)]
    struct Session {
        scope: FileImageScope,
        native_session: u64,
    }
    #[derive(Deserialize)]
    struct Catalog {
        routes: Vec<Route>,
        network: String,
        memberships: Vec<Membership>,
        sessions: Vec<Session>,
    }
    #[derive(Deserialize)]
    struct Route {
        peer_id: String,
        requester_did: String,
        node_did: String,
    }
    #[derive(Deserialize)]
    struct Membership {
        network_id: String,
        member_did: String,
    }
    #[derive(Deserialize)]
    struct Case {
        name: String,
        native_session: u64,
        demand: FileImageDemand,
        catalog: Catalog,
        tools: Vec<FileImageTool>,
        images: Vec<FileImageObservation>,
        expected: FileImageSelection,
    }
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases: Vec<Case> =
        serde_json::from_value(snapshot["file_image_hydration_cases"].clone()).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        assert_eq!(
            case.native_session, 0,
            "file images use abstract enrollment admission"
        );
        let catalog = HydrationCatalog {
            applied_pairing_routes: case
                .catalog
                .routes
                .into_iter()
                .map(|route| AppliedPairingRoute {
                    peer_id: route.peer_id,
                    requester_did: route.requester_did,
                    node_did: route.node_did,
                })
                .collect(),
            selected_network_id: case.catalog.network,
            verified_active_memberships: case
                .catalog
                .memberships
                .into_iter()
                .map(|membership| VerifiedActiveMembership {
                    network_id: membership.network_id,
                    member_did: membership.member_did,
                })
                .collect(),
            sessions: case
                .catalog
                .sessions
                .into_iter()
                .map(|session| {
                    assert_eq!(session.native_session, 0);
                    SessionOwner {
                        session_doc_id: None,
                        session_id: session.scope.session_id,
                        requester_did: session.scope.requester_did,
                        node_did: session.scope.node_did,
                    }
                })
                .collect(),
            ..HydrationCatalog::default()
        };
        assert_eq!(
            select_file_images(&case.demand, &catalog, &case.tools, &case.images),
            case.expected,
            "{}",
            case.name
        );
    }
}

#[test]
fn generated_routes_follow_executable_scope_filters() {
    use super::super::policy::{
        client_route_collections, resolve_template_filters, PairingDirection,
    };
    use super::super::templates::{filter_conditions, resolve_template, CLIENT_TEMPLATE};
    use serde_json::{json, Value};

    #[derive(Deserialize)]
    struct Clause {
        field: String,
        operator: String,
        value: String,
    }
    #[derive(Deserialize)]
    struct Predicate {
        collection: String,
        clauses: Vec<Clause>,
    }
    #[derive(Deserialize)]
    struct Case {
        direction: String,
        requester: String,
        owner: String,
        demand_collection: String,
        image_collection: String,
        expected: Predicate,
        images_granted: bool,
        image_filter_installed: bool,
    }
    fn sorted_conjunction(mut value: Value) -> Value {
        if let Some(clauses) = value.get_mut("_and").and_then(Value::as_array_mut) {
            clauses.sort_by_key(Value::to_string);
        }
        value
    }
    let snapshot: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases: Vec<Case> =
        serde_json::from_value(snapshot["file_image_hydration_route_cases"].clone()).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let direction = PairingDirection::parse(&case.direction).unwrap();
        let template = resolve_template(CLIENT_TEMPLATE).unwrap();
        let actual = resolve_template_filters(template, direction, &case.requester, &case.owner);
        assert_eq!(case.expected.collection, case.demand_collection);
        let mut clauses: Vec<Value> = case
            .expected
            .clauses
            .into_iter()
            .map(|clause| json!({(clause.field): {(clause.operator): clause.value}}))
            .collect();
        let expected = if clauses.len() == 1 {
            clauses.remove(0)
        } else {
            json!({"_and": clauses})
        };
        let predicate = actual.get(&case.demand_collection).unwrap();
        assert_eq!(
            sorted_conjunction(Value::Object(filter_conditions(predicate).unwrap())),
            sorted_conjunction(expected),
            "{}",
            case.direction
        );
        assert_eq!(
            client_route_collections(direction).contains(&case.image_collection.as_str()),
            case.images_granted
        );
        assert_eq!(
            actual.contains_key(&case.image_collection),
            case.image_filter_installed
        );
    }
}

#[test]
fn generated_completion_cases_gate_delivery_and_receipts() {
    use super::super::session_hydration::{HydrationDeliveryResult, HydrationTerminalWriteResult};
    #[derive(Deserialize)]
    struct Case {
        selection: FileImageSelection,
        before_delivery: bool,
        after_delivery: bool,
        delivery: String,
        write: String,
        target: Option<String>,
        expected: Option<FileImageSelection>,
    }
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases: Vec<Case> =
        serde_json::from_value(snapshot["file_image_hydration_completion_cases"].clone()).unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let delivery = match case.delivery.as_str() {
            "confirmed" => HydrationDeliveryResult::Confirmed,
            "indeterminate" => HydrationDeliveryResult::Indeterminate,
            other => panic!("unexpected delivery {other}"),
        };
        let write = match case.write.as_str() {
            "committed" => HydrationTerminalWriteResult::Committed,
            "failed" => HydrationTerminalWriteResult::Failed,
            "notAttempted" => HydrationTerminalWriteResult::NotAttempted,
            other => panic!("unexpected receipt write {other}"),
        };
        assert_eq!(
            image_delivery_target(&case.selection, case.before_delivery),
            case.target.as_deref()
        );
        assert_eq!(
            completed_image_selection(
                &case.selection,
                case.before_delivery,
                case.after_delivery,
                delivery,
                write
            ),
            case.expected
        );
    }
}

#[test]
fn generated_attempt_cases_preserve_terminal_outcomes_and_prevent_redelivery() {
    use super::super::session_hydration::HydrationTerminalWriteResult;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Case {
        prior: Option<FileImageSelection>,
        candidate: FileImageSelection,
        authorized: bool,
        write: String,
        target: Option<String>,
        expected: Option<FileImageSelection>,
    }
    let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases: Vec<Case> =
        serde_json::from_value(snapshot["file_image_attempt_cases"].clone()).unwrap();
    assert_eq!(cases.len(), 72);
    for case in cases {
        let write = match case.write.as_str() {
            "committed" => HydrationTerminalWriteResult::Committed,
            "failed" => HydrationTerminalWriteResult::Failed,
            "notAttempted" => HydrationTerminalWriteResult::NotAttempted,
            other => panic!("unexpected receipt write {other}"),
        };
        assert_eq!(
            image_attempt_delivery_target(case.prior.as_ref(), &case.candidate, case.authorized),
            case.target.as_deref(),
        );
        assert_eq!(
            record_image_outcome(case.prior.as_ref(), &case.candidate, write),
            case.expected,
        );
    }
}
