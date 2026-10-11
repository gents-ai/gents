use super::*;
use crate::config_client::ConfigAccess;
use crate::lean_vocab_test::LeanQueueManagementOperation;
use std::collections::HashMap;

#[tokio::test]
async fn generated_pending_management_cases_bind_to_signed_replacement_owner() {
    let cases = crate::lean_vocab_test::lean_queue_management_cases();
    assert!(!cases.is_empty());
    let mut exercised = Vec::new();
    // Each model freshness value has two native TTL encodings; both must bind.
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
        // The native API generates fresh identities and derives positions itself;
        // forged identity/slot cases belong to admission, not this API's inputs.
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
        let mut bound = HashMap::new();
        for (arrival, entry) in originals.iter().enumerate() {
            let id = if entry.requester_id == Some(2) {
                super::folding::enqueue(&db, &session_id, entry, arrival).await
            } else {
                let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
                    gents_protocol::request_admission::RequestPurpose::Normal,
                    entry.request_id.to_string(),
                    db.node_did(),
                    db.node_did(),
                    TEST_AGENT_ID,
                    &session_id,
                    format!("message {}", entry.request_id),
                    "interactive",
                    format!("2026-09-01T00:00:{arrival:02}Z"),
                    gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                        db.node_did(),
                    ),
                );
                create.input = super::folding::generated_input(entry);
                create.valid_until = ttl(entry)
                    .filter(|ttl| *ttl != "malformed")
                    .map(str::to_owned);
                crate::sign_agent_request_create(db.identity.as_ref(), &mut create)
                    .await
                    .unwrap();
                let mut mutation = create.graphql_mutation().unwrap();
                if ttl(entry) == Some("malformed") {
                    // Malformed TTLs cannot be canonically signed. Insert an adversarial
                    // signature-mismatched row directly; never mutate immutable fields.
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
        let access = ConfigAccess::Local(db.node.clone());
        let state_query = format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID content input admission_signature lifecycle_state valid_until }} }}"#,
            escape_graphql_string(&session_id),
        );
        let snapshot = pending_user_queue(&access, db.node_did(), &session_id, caller_did)
            .await
            .unwrap();
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
        if let Some(after) = &case.expected {
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
            let snapshot = pending_user_queue(&access, db.node_did(), &session_id, caller_did)
                .await
                .unwrap();
            let expected_ids = after
                .folding
                .iter()
                .chain(&after.pending)
                .map(|entry| bound[&entry.request_id].clone())
                .collect::<Vec<_>>();
            assert_eq!(
                snapshot
                    .entries
                    .iter()
                    .map(|entry| entry.request_doc_id.clone())
                    .collect::<Vec<_>>(),
                expected_ids,
                "{}: logical queue order",
                case.name
            );
            for original in &originals {
                let row = actual_rows
                    .iter()
                    .find(|row| row["_docID"] == bound[&original.request_id])
                    .unwrap();
                assert_eq!(
                    row["lifecycle_state"] != "pending",
                    after.terminal.contains(&original.request_id),
                    "{}: terminal requests",
                    case.name
                );
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
        9,
        "all representable generated management operations"
    );
}
