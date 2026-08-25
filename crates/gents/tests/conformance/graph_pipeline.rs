use gents::graph_pipeline::{
    compile_graph, CompilerPolicy, EntryBinding, GraphIntent, GraphLimits, GraphNode,
    PortCardinality, PortRef, PortSpec, StageCapability,
};

use super::lean_contract_snapshot;

const CALLER_DID: &str = "did:key:graph-composer";

fn valid_fixture() -> (GraphIntent, Vec<StageCapability>) {
    let input = PortSpec {
        name: "job".to_owned(),
        collection: "ExperimentJob".to_owned(),
        schema: "ExperimentJob/v1".to_owned(),
        correlation_field: "graph_run_id".to_owned(),
        cardinality: PortCardinality::One,
        required: true,
    };
    let intent = GraphIntent {
        graph_id: "lean-validation-fixture".to_owned(),
        nodes: vec![GraphNode {
            node_id: "worker".to_owned(),
            capability_id: "approved-worker".to_owned(),
            capability_revision: "v1".to_owned(),
        }],
        edges: vec![],
        entries: vec![EntryBinding {
            name: "job".to_owned(),
            collection: input.collection.clone(),
            schema: input.schema.clone(),
            to: PortRef {
                node_id: "worker".to_owned(),
                port: input.name.clone(),
            },
        }],
        limits: GraphLimits {
            max_nodes: 2,
            max_edges: 2,
            max_depth: 2,
            max_fan_out: 2,
        },
    };
    let capability = StageCapability {
        capability_id: "approved-worker".to_owned(),
        revision: "v1".to_owned(),
        task_id: "worker-v1-task".to_owned(),
        input_ports: vec![input],
        output_ports: vec![],
        allowed_callers: vec![CALLER_DID.to_owned()],
    };
    (intent, vec![capability])
}

#[test]
fn generated_validation_cases_fence_whole_graph_compilation_gate() {
    let cases = &lean_contract_snapshot().graph_pipeline_validation_cases;
    assert_eq!(cases.len(), 16, "Lean must emit the full four-bit matrix");

    for test_case in cases {
        let (mut intent, mut capabilities) = valid_fixture();
        if !test_case.types_valid {
            intent.entries[0].schema = "WrongSchema/v1".to_owned();
        }
        if !test_case.topology_valid {
            intent.entries.clear();
        }
        if !test_case.capabilities_authorized {
            capabilities[0].allowed_callers.clear();
        }
        if !test_case.within_bounds {
            intent.limits.max_nodes = 0;
        }

        let accepted = compile_graph(
            &intent,
            &capabilities,
            CALLER_DID,
            &CompilerPolicy::default(),
        )
        .is_ok();
        assert_eq!(accepted, test_case.expected_valid, "{}", test_case.name);
    }
}

#[test]
fn successful_compilation_supplies_stable_publication_identity() {
    let (intent, capabilities) = valid_fixture();
    let first = compile_graph(
        &intent,
        &capabilities,
        CALLER_DID,
        &CompilerPolicy::default(),
    )
    .unwrap();
    let second = compile_graph(
        &intent,
        &capabilities,
        CALLER_DID,
        &CompilerPolicy::default(),
    )
    .unwrap();

    assert_eq!(first, second);
    assert!(first.digest.starts_with("sha256:"));
    assert_eq!(first.nodes[0].task_id, "worker-v1-task");
}
