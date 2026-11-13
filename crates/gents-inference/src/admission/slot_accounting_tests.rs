use super::slot_accounting::{
    reconstructed_running_slot_count, slot_contribution, InferenceCallSlotRow,
};
use crate::test_support::test_db;
use defra_node::Node as EmbeddedNode;
use gents_store::graphql::escape_graphql_string;
use serde::Deserialize;

const NODE_DID: &str = "did:test:test";
const AGENT_NAME: &str = "test";

#[derive(Debug, Deserialize)]
struct PersistedSlotRow {
    call_id: String,
    backend_id: String,
    call_state: String,
}

#[tokio::test]
async fn generated_inference_slot_accounting_cases_drive_db_backed_reconstruction() {
    use gents_lean_vocab::families::inference_slot_accounting_reconstruction as wire;

    let db = test_db(
        "inference-slot-accounting",
        gents_store::node_extension::Extensions::none(),
    )
    .await;

    for (case_index, case) in gents_lean_vocab::families::inference_slot_accounting_reconstruction::cases().into_iter().enumerate() {
        let input = &case.input;
        assert_eq!(
            input.row_backend_ids.len(),
            input.row_states.len(),
            "{input:?} has mismatched row arrays"
        );
        let case_name = format!("slot-case-{case_index}");
        for (index, (backend_id, state)) in input
            .row_backend_ids
            .iter()
            .zip(&input.row_states)
            .enumerate()
        {
            insert_inference_call_row(&db.node, &case_name, index, backend_id, state).await;
        }

        let rows = fetch_persisted_slot_rows_for_case(&db.node, &case_name).await;
        assert_eq!(
            rows.len(),
            input.row_states.len(),
            "{input:?} must read back every seeded InferenceCall row"
        );

        let reconstructed = reconstructed_running_slot_count(
            rows.iter()
                .map(|row| InferenceCallSlotRow::new(&row.backend_id, &row.call_state)),
            &input.backend_id,
        );
        let contribution = match rows.as_slice() {
            [row] => slot_contribution(
                InferenceCallSlotRow::new(&row.backend_id, &row.call_state),
                &input.backend_id,
            ),
            _ => reconstructed,
        };
        let pre_contribution = slot_contribution(
            InferenceCallSlotRow::new(&input.backend_id, &input.pre_state),
            &input.backend_id,
        );
        let post_contribution = slot_contribution(
            InferenceCallSlotRow::new(&input.backend_id, &input.post_state),
            &input.backend_id,
        );
        case.check(wire::ReconstructionOutput {
            contribution: contribution as u64,
            pre_contribution: pre_contribution as u64,
            post_contribution: post_contribution as u64,
            released_slot: pre_contribution == 1 && post_contribution == 0,
            reconstructed_running_count: reconstructed as u64,
            bounded_by_max_concurrent: reconstructed as u64 <= input.max_concurrent,
        });
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
                agent_did: "{AGENT_NAME}",
                node_did: "{NODE_DID}",
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
