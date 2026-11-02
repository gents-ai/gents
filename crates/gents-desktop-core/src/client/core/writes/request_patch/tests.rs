use super::request_patch_cache_reusable;
use crate::client::observe::ObservedStore;
use crate::client::store::{ClientStore, ClientStoreRows};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

fn request_row(state: &str) -> AgentRequestRow {
    AgentRequestRow {
        doc_id: Some("request-doc".to_owned()),
        request_id: "request".to_owned(),
        node_did: Some("did:test:node".to_owned()),
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        lifecycle_state: Some(match state {
            "pending" => RequestLifecycleState::Pending,
            "processing" => RequestLifecycleState::Processing,
            other => panic!("unknown test state {other}"),
        }),
        ..AgentRequestRow::default()
    }
}

fn request_patch(row: AgentRequestRow) -> ClientStore {
    ClientStore::from_rows(ClientStoreRows {
        requests: vec![row],
        ..ClientStoreRows::default()
    })
}

#[test]
fn cached_rows_must_match_physical_rows_at_the_current_fence() {
    let (store, _) = ObservedStore::new(request_patch(request_row("pending")));
    let patch = request_patch(request_row("pending"));
    let captured = store.capture_read();
    assert_eq!(
        store.current_request_patch_read_status(&captured, &patch),
        (true, true)
    );

    store.merge_chat_patch(request_patch(request_row("processing")));
    let captured = store.capture_read();
    assert_eq!(
        store.current_request_patch_read_status(&captured, &patch),
        (true, false),
        "a current fence does not make an old physical request row current"
    );
}

#[test]
fn generated_publication_cases_drive_request_patch_cache_admission() {
    let snapshot: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("Lean contract snapshot");
    let fixture = &snapshot["client_request_patch_publication_cases"];
    for case in fixture["reuse"].as_array().expect("reuse cases") {
        assert_eq!(
            request_patch_cache_reusable(
                case["memoized"].as_bool().expect("memoized state"),
                case["same_signature"]
                    .as_bool()
                    .expect("signature comparison"),
                case["fence_accepted"].as_bool().expect("publication fence"),
                case["rows_current"]
                    .as_bool()
                    .expect("physical row comparison"),
            ),
            case["expected_reuse"].as_bool().expect("Lean result"),
            "{}",
            case["name"].as_str().expect("case name")
        );
    }
    for case in fixture["fresh_read"].as_array().expect("fresh-read cases") {
        let (store, _) = ObservedStore::new(ClientStore::from_rows(ClientStoreRows::default()));
        let captured = store.capture_read();
        let fence_accepted = case["fence_accepted"].as_bool().expect("fence expectation");
        if !fence_accepted {
            store.merge_chat_patch(request_patch(request_row("processing")));
        }
        let published = case["patch_available"]
            .as_bool()
            .expect("fresh patch availability")
            .then(|| {
                store.merge_chat_patch_at_read(&captured, request_patch(request_row("pending")))
            })
            .flatten()
            .is_some();
        let action = match (case["patch_available"].as_bool().unwrap(), published) {
            (false, false) => "no_patch",
            (true, true) => "merge_fresh",
            (true, false) => "reject_candidate",
            (false, true) => panic!("missing patch was published"),
        };
        assert_eq!(
            action,
            case["expected_action"].as_str().expect("Lean action"),
            "{}",
            case["name"].as_str().expect("case name")
        );
    }
}
