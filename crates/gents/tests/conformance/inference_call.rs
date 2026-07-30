//! InferenceCall conformance home: drives the generated slot-accounting cases
//! against REAL persisted `InferenceCall` rows. The scheduler's fleet slot
//! state is a derived view over these rows (Boundaries.lean:
//! `boundary.inference-slots.running-row-derived`), so the integration witness
//! seeds each case's rows in DefraDB, reads them back, and reconstructs the
//! running slot count exactly as admission does — pinning the S7 capacity
//! bound (`reconstructed ≤ max_concurrent`) over the persisted projection.
//! The pure transition/vocabulary checks remain in `admission::tests`.

use super::*;

#[derive(Debug, Deserialize)]
struct PersistedSlotRow {
    call_id: String,
    backend_id: String,
    call_state: String,
}

pub(super) async fn generated_inference_slot_accounting_cases_drive_db_backed_reconstruction() {
    let cases = lean_inference_slot_accounting_cases();
    assert_eq!(
        cases.len(),
        11,
        "Lean should emit the finite InferenceCall slot-accounting cases"
    );

    let db = test_db("inference-slot-accounting").await;

    for case in cases {
        assert_eq!(
            case.row_backend_ids.len(),
            case.row_states.len(),
            "Lean case {} emitted mismatched row arrays",
            case.name
        );

        for (index, (backend_id, state)) in case
            .row_backend_ids
            .iter()
            .zip(&case.row_states)
            .enumerate()
        {
            insert_inference_call_row(&db.node, &case.name, index, backend_id, state).await;
        }

        let rows = fetch_persisted_slot_rows_for_case(&db.node, &case.name).await;
        assert_eq!(
            rows.len(),
            case.row_states.len(),
            "case {} must read back every seeded InferenceCall row",
            case.name
        );

        if let [row] = rows.as_slice() {
            assert_eq!(
                slot_contribution(
                    InferenceCallSlotRow::new(&row.backend_id, &row.call_state),
                    &case.backend_id,
                ),
                case.expected_contribution,
                "case {} drifted from admission slot contribution over the persisted row",
                case.name
            );
        }

        let reconstructed = reconstructed_running_slot_count(
            rows.iter()
                .map(|row| InferenceCallSlotRow::new(&row.backend_id, &row.call_state)),
            &case.backend_id,
        );
        assert_eq!(
            reconstructed, case.reconstructed_running_count,
            "case {} drifted from admission reconstruction over persisted rows",
            case.name
        );
        assert_eq!(
            case.bounded_by_max_concurrent,
            reconstructed <= case.max_concurrent,
            "case {} drifted from the max_concurrent capacity bound",
            case.name
        );
    }
}

fn case_call_id(case_name: &str, index: usize) -> String {
    format!("{case_name}::call-{index}")
}

async fn insert_inference_call_row(
    node: &EmbeddedNode,
    case_name: &str,
    index: usize,
    backend_id: &str,
    call_state: &str,
) {
    let call_id = case_call_id(case_name, index);
    let request_id = format!("{case_name}::request-{index}");
    let now = chrono::Utc::now().to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            add_InferenceCall(input: {{
                call_id: "{call_id}",
                runtime_instance_id: "runtime-slot-conformance",
                request_id: "{request_id}",
                call_seq: 1,
                backend_id: "{backend_id}",
                behavior_id: "{AGENT_NAME}",
                agent_did: "{AGENT_DID}",
                call_kind: "inference",
                attempt: 1,
                call_state: "{call_state}",
                queued_at: "{now}",
                started_at: "{now}",
                priority: 0,
                queue_depth_at_enqueue: 0,
                controller_generation: 0,
                backend_config_fingerprint: "test"
            }}) {{ _docID }}
        }}"#,
        call_id = escape_graphql_string(&call_id),
        request_id = escape_graphql_string(&request_id),
        backend_id = escape_graphql_string(backend_id),
        call_state = escape_graphql_string(call_state),
        now = escape_graphql_string(&now),
    );
    let resp = node.execute(&mutation).await;
    assert!(
        !resp.has_errors(),
        "insert InferenceCall slot row failed: {:?}",
        resp.errors
    );
}

async fn fetch_persisted_slot_rows_for_case(
    node: &EmbeddedNode,
    case_name: &str,
) -> Vec<PersistedSlotRow> {
    let query = r#"{
        InferenceCall {
            call_id
            backend_id
            call_state
        }
    }"#;
    let response = node.execute(query).await;
    assert!(
        !response.has_errors(),
        "query InferenceCall slot rows failed: {:?}",
        response.errors
    );
    let prefix = format!("{case_name}::call-");
    response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
        .and_then(|value| serde_json::from_value::<Vec<PersistedSlotRow>>(value.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.call_id.starts_with(&prefix))
        .collect()
}
