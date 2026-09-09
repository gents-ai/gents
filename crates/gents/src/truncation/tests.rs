use serde_json::json;

use super::spill::extract_mutation_doc_id;

// The truncate/truncate_text tests moved to gents-loop with the pure logic
// they exercise (crates/gents-loop/src/truncation.rs); only the DefraDB spill
// helper stays native here.

#[test]
fn extract_mutation_doc_id_accepts_create_and_add_shapes() {
    let create_data = json!({
        "create_AgentToolResult": { "_docID": "doc-create" }
    });
    assert_eq!(
        extract_mutation_doc_id(&create_data, "AgentToolResult"),
        Some("doc-create")
    );

    let add_data = json!({
        "add_AgentToolResult": [{ "_docID": "doc-add" }]
    });
    assert_eq!(
        extract_mutation_doc_id(&add_data, "AgentToolResult"),
        Some("doc-add")
    );
}
