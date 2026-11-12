use super::{record_request_patch_publication, request_patch_cache_reusable};
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
    let store = crate::client::observe::ObservedStores::new()
        .insert_for_test("did:test:node", request_patch(request_row("pending")));
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
fn generated_reuse_cases_drive_request_patch_cache_admission() {
    gents_lean_vocab::families::request_patch_publication_cases_reuse::replay(|case| {
        gents_lean_vocab::families::request_patch_publication_cases_reuse::ReuseOutput {
            reuse: request_patch_cache_reusable(
                case.memoized,
                case.same_signature,
                case.fence_accepted,
                case.rows_current,
            ),
        }
    });
}

#[test]
fn generated_fresh_read_cases_drive_request_patch_publication() {
    gents_lean_vocab::families::request_patch_publication_cases_fresh_read::replay::<String>(|case| {
        let store = crate::client::observe::ObservedStores::new().insert_for_test(
            "did:test:node",
            ClientStore::from_rows(ClientStoreRows::default()),
        );
        let captured = store.capture_read();
        if !case.fence_accepted {
            store.merge_chat_patch(request_patch(request_row("processing")));
        }
        let published = case
            .patch_available
            .then(|| {
                store.merge_chat_patch_at_read(&captured, request_patch(request_row("pending")))
            })
            .flatten()
            .is_some();
        let action = match (case.patch_available, published) {
            (false, false) => "no_patch",
            (true, true) => "merge_fresh",
            (true, false) => "reject_candidate",
            (false, true) => panic!("missing patch was published"),
        };
        gents_lean_vocab::families::request_patch_publication_cases_fresh_read::FreshReadOutput {
            action: action.to_owned(),
        }
    });
}

#[test]
fn generated_publication_cases_memoize_only_accepted_patches() {
    gents_lean_vocab::families::request_patch_publication_cases_publications::replay::<String>(
        |case| {
            let key = "source\0did:test:node\0session\0<null>\0request".to_owned();
            let signature = (1, 1, 1);
            let mut signatures = rapidhash::RapidHashMap::default();
            if case.before == "memoized" {
                record_request_patch_publication(&mut signatures, key.clone(), signature);
            }
            let store = crate::client::observe::ObservedStores::new().insert_for_test(
                "did:test:node",
                ClientStore::from_rows(ClientStoreRows::default()),
            );
            let captured = store.capture_read();
            if !case.accepted {
                store.merge_chat_patch(request_patch(request_row("processing")));
            }
            if store
                .merge_chat_patch_at_read(&captured, request_patch(request_row("pending")))
                .is_some()
            {
                record_request_patch_publication(&mut signatures, key.clone(), signature);
            }
            gents_lean_vocab::families::request_patch_publication_cases_publications::PublicationOutput {
                after: if signatures.get(&key) == Some(&signature) {
                    "memoized"
                } else {
                    "empty"
                }
                .to_owned(),
            }
        },
    );
}
