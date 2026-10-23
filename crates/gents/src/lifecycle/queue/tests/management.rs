use super::*;
use crate::config_client::ConfigAccess;
use crate::lean_vocab_test::LeanQueueManagementOperation;
use rapidhash::{HashMapExt, RapidHashMap};

async fn queue_image(
    db: &TestDb,
    session_id: &str,
    active: Option<&super::folding::ActiveTurn>,
) -> crate::lean_vocab_test::QueueImage {
    crate::lean_vocab_test::native_queue_image(
        &db.node,
        db.node_did(),
        session_id,
        active.map_or(&[], |turn| turn.lifecycle.folded_selection()),
    )
    .await
}

#[tokio::test]
async fn generated_pending_management_cases_bind_to_signed_replacement_owner() {
    let cases = crate::lean_vocab_test::lean_queue_management_cases();
    assert!(!cases.is_empty());
    let mut exercised = Vec::new();
    for (case, variant) in cases.iter().flat_map(|case| [(case, 0), (case, 1)]) {
        let LeanQueueManagementOperation::Replace {
            caller,
            expected,
            offset,
            count,
            replacements,
        } = &case.operation
        else {
            continue;
        };
        let originals = case
            .before
            .folding
            .iter()
            .chain(&case.before.pending)
            .collect::<Vec<_>>();
        let selected =
            &originals[(*offset).min(originals.len())..(offset + count).min(originals.len())];
        if replacements.iter().any(|replacement| {
            originals
                .iter()
                .any(|entry| entry.request_id == replacement.request_id)
        }) || replacements
            .iter()
            .zip(selected)
            .any(|(replacement, original)| replacement.order_key != original.order_key)
        {
            continue;
        }
        if variant == 0 {
            exercised.push(case.name.as_str());
        }
        let db = test_db(&format!("{}-{variant}", case.name)).await;
        let session_id = format!("management-{}", case.name);
        let ttl = |entry: &crate::lean_vocab_test::LeanFoldQueueEntry| match (entry.fresh, variant)
        {
            (true, 0) => None,
            (true, _) => Some("2099-01-01T00:00:00Z"),
            (false, 0) => Some("2000-01-01T00:00:00Z"),
            (false, _) => Some("malformed"),
        };
        let mut bound = RapidHashMap::new();
        let mut names: RapidHashMap<u64, String> = RapidHashMap::new();
        let signed = |id: u64, arrival: usize| {
            gents_protocol::request_admission::AgentRequestCreate::base(
                gents_protocol::request_admission::RequestPurpose::Normal,
                id.to_string(),
                db.node_did(),
                db.node_did(),
                TEST_AGENT_ID,
                &session_id,
                format!("message {id}"),
                "interactive",
                format!("2026-09-01T00:00:{arrival:02}Z"),
                gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                    db.node_did(),
                ),
            )
        };
        if let Some(active) = case.before.active {
            let mut create = signed(active, 0);
            crate::sign_agent_request_create(db.identity.as_ref(), &mut create)
                .await
                .unwrap();
            let response = ConfigAccess::write_local_response(
                &db.node,
                "test.management.active",
                &create.graphql_mutation().unwrap(),
            )
            .await
            .unwrap();
            bound.insert(
                active,
                extract_single_doc_id(&response, "create_AgentRequest").unwrap(),
            );
            names.insert(active, active.to_string());
        }
        for (arrival, entry) in originals.iter().enumerate() {
            let arrival = arrival + 1;
            names.insert(entry.request_id, entry.request_id.to_string());
            let id = if entry.requester_id == Some(2) {
                super::folding::enqueue(&db, &session_id, entry, arrival).await
            } else {
                let mut create = signed(entry.request_id, arrival);
                create.input = super::folding::generated_input(entry);
                create.valid_until = ttl(entry)
                    .filter(|ttl| *ttl != "malformed")
                    .map(str::to_owned);
                crate::sign_agent_request_create(db.identity.as_ref(), &mut create)
                    .await
                    .unwrap();
                let mut mutation = create.graphql_mutation().unwrap();
                if ttl(entry) == Some("malformed") {
                    mutation = mutation.replacen(
                        "create_AgentRequest(input: {",
                        "create_AgentRequest(input: { valid_until: \"malformed\",",
                        1,
                    );
                }
                let response =
                    ConfigAccess::write_local_response(&db.node, "test.management.seed", &mutation)
                        .await
                        .unwrap();
                extract_single_doc_id(&response, "create_AgentRequest").unwrap()
            };
            bound.insert(entry.request_id, id);
        }
        let active = match case.before.active {
            Some(head) => Some(
                super::folding::claim_head(
                    &db,
                    &bound,
                    head,
                    &case
                        .before
                        .folding
                        .iter()
                        .map(|entry| entry.request_id)
                        .collect::<Vec<_>>(),
                )
                .await,
            ),
            None => None,
        };
        assert_eq!(
            queue_image(&db, &session_id, active.as_ref()).await,
            case.before.image(|id| names[&id].clone()),
            "{}: seeded queue",
            case.name
        );
        let edit = PendingQueueEdit {
            expected_request_doc_ids: expected.iter().map(|id| bound[id].clone()).collect(),
            selected_request_doc_ids: selected
                .iter()
                .map(|entry| bound[&entry.request_id].clone())
                .collect(),
            messages: replacements
                .iter()
                .zip(selected)
                .map(|(replacement, original)| PendingMessageEdit {
                    request_doc_id: bound[&original.request_id].clone(),
                    content: format!("message {}", replacement.request_id),
                })
                .collect(),
        };
        let caller_did = match caller {
            Some(1) => db.node_did(),
            Some(2) => "did:test:fold-foreign-requester",
            _ => panic!("unsupported caller in {}", case.name),
        };
        let access = ConfigAccess::local(db.node.clone());
        let state_query = format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID content input admission_signature lifecycle_state valid_until }} }}"#,
            escape_graphql_string(&session_id),
        );
        let snapshot = pending_user_queue(&access, db.node_did(), &session_id, caller_did)
            .await
            .unwrap();
        let mut observed = snapshot
            .entries
            .iter()
            .map(|entry| entry.request_doc_id.clone())
            .collect::<Vec<_>>();
        let mut expected_order = observed.clone();
        if let Some(head) = case.before.active {
            observed.push(bound[&head].clone());
            expected_order.insert(0, bound[&head].clone());
        }
        observed.reverse();
        let projection = pending_user_queue_projection(
            &access,
            db.node_did(),
            &session_id,
            caller_did,
            &observed,
        )
        .await
        .unwrap();
        assert_eq!(
            projection.arrival_order, expected_order,
            "{}: arrival order includes claimed requests",
            case.name
        );
        assert_eq!(
            projection
                .pending
                .entries
                .iter()
                .map(|entry| &entry.request_doc_id)
                .collect::<Vec<_>>(),
            snapshot
                .entries
                .iter()
                .map(|entry| &entry.request_doc_id)
                .collect::<Vec<_>>()
        );
        for original in selected.iter().filter(|entry| !entry.fresh) {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.request_doc_id == bound[&original.request_id])
                .unwrap();
            assert!(
                !entry.editable,
                "{}: stale TTL cannot offer an edit",
                case.name
            );
        }
        let before_rows = access.execute(&state_query).await.unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .clone();
        let result = replace_pending_user_messages(
            &access,
            db.identity.as_ref(),
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(caller_did),
            db.node_did(),
            &session_id,
            caller_did,
            edit,
            &replacements
                .iter()
                .map(|replacement| {
                    super::folding::generated_input(replacement)
                        .queue
                        .unwrap()
                        .delivery
                })
                .collect::<Vec<_>>(),
        )
        .await;
        assert_eq!(
            result.is_ok(),
            case.expected.is_some(),
            "{}: acceptance",
            case.name
        );
        let actual_rows = access.execute(&state_query).await.unwrap()["data"]["AgentRequest"]
            .as_array()
            .unwrap()
            .clone();
        for original in &before_rows {
            let current = actual_rows
                .iter()
                .find(|row| row["_docID"] == original["_docID"])
                .unwrap();
            for field in ["content", "input", "admission_signature", "valid_until"] {
                assert_eq!(
                    current[field], original[field],
                    "{}: signed {field} remains immutable",
                    case.name
                );
            }
        }
        if case.expected.is_none() {
            assert_eq!(
                actual_rows.len(),
                before_rows.len(),
                "{}: rejection creates no requests",
                case.name
            );
            for original in &before_rows {
                assert!(
                    actual_rows.contains(original),
                    "{}: rejection leaves durable state unchanged",
                    case.name
                );
            }
        }
        if let Ok(receipt) = &result {
            for (replacement, id) in replacements.iter().zip(&receipt.request_ids) {
                names.insert(replacement.request_id, id.clone());
            }
        }
        assert_eq!(
            queue_image(&db, &session_id, active.as_ref()).await,
            case.expected
                .as_ref()
                .unwrap_or(&case.before)
                .image(|id| names[&id].clone()),
            "{}: queue state",
            case.name
        );
        if case.expected.is_some() {
            let receipt = result.unwrap();
            assert_eq!(
                receipt.request_ids.len(),
                replacements.len(),
                "{}",
                case.name
            );
            for (replacement, id) in replacements.iter().zip(&receipt.request_doc_ids) {
                assert!(
                    !bound.values().any(|old| old == id),
                    "{}: physical identity is fresh",
                    case.name
                );
                bound.insert(replacement.request_id, id.clone());
            }
            for (replacement, id) in replacements.iter().zip(&receipt.request_doc_ids) {
                let row = crate::request_binding::load_agent_request_by_doc_id(&db.node, id)
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    row.content,
                    format!("message {}", replacement.request_id),
                    "{}",
                    case.name
                );
                let slot = originals
                    .iter()
                    .find(|entry| entry.order_key == replacement.order_key)
                    .unwrap();
                let queue = row.input.queue.unwrap();
                assert_eq!(
                    queue.position.unwrap().slot_request_doc_id,
                    bound[&slot.request_id],
                    "{}: immutable slot",
                    case.name
                );
                assert_eq!(
                    serde_json::to_value(queue.delivery).unwrap(),
                    replacement.delivery,
                    "{}: delivery",
                    case.name
                );
                let persisted = access.execute(&format!(
                    r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ admission_signature valid_until }} }}"#,
                    escape_graphql_string(id),
                )).await.unwrap();
                let predecessor = before_rows
                    .iter()
                    .find(|row| row["_docID"] == bound[&slot.request_id])
                    .unwrap();
                assert_eq!(
                    persisted["data"]["AgentRequest"][0]["valid_until"], predecessor["valid_until"],
                    "{}: signed expiry is retained rather than extended",
                    case.name
                );
                assert!(
                    persisted["data"]["AgentRequest"][0]["admission_signature"]
                        .as_str()
                        .is_some_and(|value| !value.is_empty()),
                    "{}: replacement is signed",
                    case.name
                );
            }
        }
    }
    assert_eq!(
        exercised.len(),
        15,
        "all representable generated management operations"
    );
}
