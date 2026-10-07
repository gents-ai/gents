use gents::graph_pipeline::{
    compile_graph, CompilerPolicy, DeliveryConcurrency, DiagnosticCode, EntryBinding, GraphEdge,
    GraphIntent, GraphLimits, GraphNode, PortCardinality, PortRef, PortSpec, ResultCardinality,
    ResultContract, StageCapability,
};

use super::lean_contract_snapshot;
use crate::lean_vocab_test::{LeanGraphBoundsFault, LeanGraphTopologyFault};

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
    let output = PortSpec {
        name: "result".to_owned(),
        collection: "ExperimentResult".to_owned(),
        schema: "ExperimentResult/v1".to_owned(),
        correlation_field: "graph_run_id".to_owned(),
        cardinality: PortCardinality::One,
        required: false,
    };
    let intent = GraphIntent {
        agent_did: CALLER_DID.to_owned(),
        graph_id: "lean-validation-fixture".to_owned(),
        nodes: vec![GraphNode {
            session: None,
            node_id: "worker".to_owned(),
            capability_id: "approved-worker".to_owned(),
            capability_revision: "v1".to_owned(),
        }],
        edges: vec![],
        entries: vec![EntryBinding {
            name: "job".to_owned(),
            collection: input.collection.clone(),
            schema: input.schema.clone(),
            input_contract: None,
            input_schema: None,
            prepare: None,
            to: PortRef {
                node_id: "worker".to_owned(),
                port: input.name.clone(),
            },
        }],
        results: vec![ResultContract {
            name: "result".to_owned(),
            from: PortRef {
                node_id: "worker".to_owned(),
                port: output.name.clone(),
            },
            cardinality: ResultCardinality::Exactly { count: 1 },
            terminal: true,
        }],
        limits: GraphLimits {
            max_nodes: 2,
            max_edges: 2,
            max_depth: 2,
            max_fan_out: 2,
            max_total_invocations: 2,
            max_runtime_secs: 60,
        },
        tags: Vec::new(),
    };
    let capability = StageCapability {
        agent_did: CALLER_DID.to_owned(),
        capability_id: "approved-worker".to_owned(),
        revision: "v1".to_owned(),
        target: gents::graph_pipeline::StageTarget::Task {
            task_id: "worker-v1-task".to_owned(),
        },
        input_ports: vec![input],
        output_ports: vec![output],
        allowed_callers: vec![CALLER_DID.to_owned()],
        workspace_authority: None,
        tags: Vec::new(),
    };
    (intent, vec![capability])
}

/// Give the worker a second, optional input on its own output's collection and
/// wire its output into it: the smallest graph whose only compile fault is
/// `Cycle` (one node, one self-edge, every port and binding otherwise valid).
/// The two-node cycle is covered by
/// `graph_pipeline::tests::rejects_cycles_and_unreachable_nodes`.
fn make_cyclic(intent: &mut GraphIntent, capabilities: &mut [StageCapability]) {
    capabilities[0].input_ports.push(PortSpec {
        name: "feedback".to_owned(),
        collection: "ExperimentResult".to_owned(),
        schema: "ExperimentResult/v1".to_owned(),
        correlation_field: "graph_run_id".to_owned(),
        cardinality: PortCardinality::One,
        required: false,
    });
    intent.edges.push(GraphEdge {
        from: PortRef {
            node_id: "worker".to_owned(),
            port: "result".to_owned(),
        },
        to: PortRef {
            node_id: "worker".to_owned(),
            port: "feedback".to_owned(),
        },
        delivery: None,
        concurrency: DeliveryConcurrency::Parallel,
        predicate: None,
    });
}

#[test]
fn generated_validation_cases_fence_whole_graph_compilation_gate() {
    let cases = &lean_contract_snapshot().graph_pipeline_validation_cases;
    assert_eq!(
        cases.len(),
        48,
        "Lean must emit the full topology and bounds fault matrix"
    );

    // Single-fault cases pin the concrete diagnostic channel so rejection must
    // come from the declared gate, not an unrelated compiler check. Multi-fault
    // cases legitimately emit several codes at once.
    for test_case in cases {
        let (mut intent, mut capabilities) = valid_fixture();
        let mut expected_codes = Vec::new();
        if !test_case.types_valid {
            intent.entries[0].schema = "WrongSchema/v1".to_owned();
            expected_codes.push(DiagnosticCode::SchemaMismatch);
        }
        match test_case.topology_fault {
            LeanGraphTopologyFault::Valid => {}
            LeanGraphTopologyFault::MissingInputBinding => {
                intent.entries.clear();
                expected_codes.push(DiagnosticCode::MissingInputBinding);
            }
            LeanGraphTopologyFault::Cycle => {
                make_cyclic(&mut intent, &mut capabilities);
                expected_codes.push(DiagnosticCode::Cycle);
            }
        }
        if !test_case.capabilities_authorized {
            capabilities[0].allowed_callers.clear();
            expected_codes.push(DiagnosticCode::UnauthorizedCapability);
        }
        match test_case.bounds_fault {
            LeanGraphBoundsFault::Within => {}
            LeanGraphBoundsFault::NodeLimit => {
                intent.limits.max_nodes = 0;
                expected_codes.push(DiagnosticCode::NodeLimitExceeded);
            }
        }
        if !test_case.terminal_result_declared {
            intent.results.clear();
            expected_codes.push(DiagnosticCode::MissingTerminalResult);
        }

        let compiled = compile_graph(
            &intent,
            &capabilities,
            CALLER_DID,
            &CompilerPolicy::default(),
        );
        assert_eq!(
            compiled.is_ok(),
            test_case.expected_valid,
            "{}",
            test_case.name
        );
        if let Err(error) = &compiled {
            let saw_cycle = error
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::Cycle);
            assert_eq!(
                saw_cycle,
                test_case.topology_fault == LeanGraphTopologyFault::Cycle,
                "{}: Cycle must be reported exactly for the cycle fault; observed {:?}",
                test_case.name,
                error.diagnostics
            );
        }
        if let [expected_code] = expected_codes.as_slice() {
            let error = compiled.unwrap_err();
            assert!(
                error
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == *expected_code),
                "{} must reject through {expected_code:?}; observed {:?}",
                test_case.name,
                error.diagnostics
            );
        }
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
    assert_eq!(first.nodes[0].target.task_id(), Some("worker-v1-task"));
}

#[test]
fn generated_revision_gate_cases_fence_publication_and_start_readiness() {
    let cases = &lean_contract_snapshot().graph_pipeline_revision_gate_cases;
    assert_eq!(
        cases.len(),
        32,
        "Lean must emit the complete revision gate matrix"
    );

    for test_case in cases {
        let decision = gents::graph_pipeline::revision_gate_decision(
            &test_case.status,
            test_case.artifacts_complete,
            test_case.activation_precondition_met,
            test_case.pointer_matches,
        );
        assert_eq!(
            decision.may_activate, test_case.expected_activate,
            "{} activate",
            test_case.name
        );
        assert_eq!(
            decision.may_start, test_case.expected_start,
            "{} start",
            test_case.name
        );
    }
}

#[test]
fn generated_run_terminal_cases_fence_completion_cas() {
    let cases = &lean_contract_snapshot().graph_pipeline_run_terminal_cases;
    assert_eq!(
        cases.len(),
        64,
        "Lean must emit the complete terminal matrix"
    );

    for test_case in cases {
        let decision = gents::graph_pipeline::graph_run_terminal_decision(
            &test_case.status,
            test_case.cancellation_requested,
            test_case.result_contract_satisfied,
            test_case.active_work_terminal,
            test_case.failure_proven,
        );
        assert_eq!(
            decision.may_succeed, test_case.expected_succeed,
            "{} succeed",
            test_case.name
        );
        assert_eq!(
            decision.may_fail, test_case.expected_fail,
            "{} fail",
            test_case.name
        );
        assert_eq!(
            decision.may_cancel, test_case.expected_cancel,
            "{} cancel",
            test_case.name
        );
    }
}
