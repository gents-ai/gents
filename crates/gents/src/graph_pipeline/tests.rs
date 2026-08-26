use std::collections::BTreeMap;

use super::*;

fn one_port(name: &str, schema: &str, required: bool) -> PortSpec {
    PortSpec {
        name: name.to_owned(),
        collection: schema.split('/').next().unwrap_or(schema).to_owned(),
        schema: schema.to_owned(),
        correlation_field: "graph_run_id".to_owned(),
        cardinality: PortCardinality::One,
        required,
    }
}

fn many_port(name: &str, schema: &str, required: bool) -> PortSpec {
    PortSpec {
        name: name.to_owned(),
        collection: schema.split('/').next().unwrap_or(schema).to_owned(),
        schema: schema.to_owned(),
        correlation_field: "graph_run_id".to_owned(),
        cardinality: PortCardinality::Many,
        required,
    }
}

fn capability(
    id: &str,
    behavior: &str,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
) -> StageCapability {
    StageCapability {
        capability_id: id.to_owned(),
        revision: "v1".to_owned(),
        task_id: behavior.to_owned(),
        input_ports: inputs,
        output_ports: outputs,
        allowed_callers: vec!["did:key:composer".to_owned()],
    }
}

fn catalog() -> Vec<StageCapability> {
    vec![
        capability(
            "extract",
            "extract-behavior",
            vec![one_port("job", "ExperimentJob/v1", true)],
            vec![one_port("finding", "ExperimentFinding/v1", false)],
        ),
        capability(
            "review",
            "review-behavior",
            vec![one_port("finding", "ExperimentFinding/v1", true)],
            vec![],
        ),
    ]
}

fn linear_intent() -> GraphIntent {
    GraphIntent {
        graph_id: "review-pipeline".to_owned(),
        nodes: vec![
            GraphNode {
                node_id: "extract".to_owned(),
                capability_id: "extract".to_owned(),
                capability_revision: "v1".to_owned(),
            },
            GraphNode {
                node_id: "review".to_owned(),
                capability_id: "review".to_owned(),
                capability_revision: "v1".to_owned(),
            },
        ],
        edges: vec![GraphEdge {
            from: PortRef {
                node_id: "extract".to_owned(),
                port: "finding".to_owned(),
            },
            to: PortRef {
                node_id: "review".to_owned(),
                port: "finding".to_owned(),
            },
            delivery: DeliveryMode::PerDocument,
            concurrency: DeliveryConcurrency::Parallel,
            predicate: None,
        }],
        entries: vec![EntryBinding {
            name: "job".to_owned(),
            collection: "ExperimentJob".to_owned(),
            schema: "ExperimentJob/v1".to_owned(),
            input_contract: None,
            to: PortRef {
                node_id: "extract".to_owned(),
                port: "job".to_owned(),
            },
        }],
        results: vec![ResultContract {
            name: "findings".to_owned(),
            from: PortRef {
                node_id: "extract".to_owned(),
                port: "finding".to_owned(),
            },
            cardinality: ResultCardinality::AtMost { count: 8 },
            terminal: true,
        }],
        limits: GraphLimits {
            max_nodes: 8,
            max_edges: 16,
            max_depth: 8,
            max_fan_out: 4,
            max_total_invocations: 32,
            max_runtime_secs: 7_200,
        },
    }
}

fn compile(
    intent: &GraphIntent,
    capabilities: &[StageCapability],
) -> Result<GraphPlan, GraphCompileError> {
    compile_graph(
        intent,
        capabilities,
        "did:key:composer",
        &CompilerPolicy::default(),
    )
}

fn has_code(error: &GraphCompileError, code: DiagnosticCode) -> bool {
    error
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code)
}

#[test]
fn compiles_typed_linear_graph_and_resolves_existing_tasks() {
    let plan = compile(&linear_intent(), &catalog()).expect("valid plan");

    assert_eq!(plan.compiler_version, COMPILER_VERSION);
    assert!(plan.digest.starts_with("sha256:"));
    assert_eq!(plan.nodes[0].task_id, "extract-behavior");
    assert_eq!(plan.nodes[1].task_id, "review-behavior");
    assert_eq!(plan.edges[0].source_collection, "ExperimentFinding");
    assert_eq!(plan.edges[0].correlation_field, "graph_run_id");
    assert_eq!(plan.entries[0].correlation_field, "graph_run_id");
}

#[test]
fn rejects_collection_and_correlation_mismatches() {
    let mut capabilities = catalog();
    capabilities[1].input_ports[0].collection = "OtherFinding".to_owned();
    capabilities[1].input_ports[0].correlation_field = "other_run_id".to_owned();

    let error = compile(&linear_intent(), &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::SchemaMismatch));
    assert!(has_code(&error, DiagnosticCode::CorrelationMismatch));

    capabilities[1].input_ports[0].correlation_field = "not-valid!".to_owned();
    let error = compile(&linear_intent(), &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::InvalidCorrelationField));

    capabilities[1].input_ports[0].collection = "not-valid!".to_owned();
    let error = compile(&linear_intent(), &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::InvalidCollection));
}

#[test]
fn rejects_unsafe_predicates_and_ambiguous_output_collections() {
    let mut intent = linear_intent();
    intent.edges[0].predicate = Some("x: { _eq: 1 } }) { Task { task_id } } #".to_owned());
    let error = compile(&intent, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::InvalidPredicate));

    let mut capabilities = catalog();
    capabilities[1]
        .output_ports
        .push(one_port("duplicate", "ExperimentFinding/v1", false));
    let error = compile(&linear_intent(), &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::DuplicateOutputCollection));
}

#[test]
fn canonical_plan_and_digest_ignore_proposal_order() {
    let first = compile(&linear_intent(), &catalog()).unwrap();
    let mut reordered = linear_intent();
    reordered.nodes.reverse();
    reordered.entries.reverse();
    reordered.edges.reverse();
    let second = compile(&reordered, &catalog()).unwrap();

    assert_eq!(first, second);
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&second).unwrap()
    );
}

#[test]
fn plan_digest_verification_rejects_semantic_tampering() {
    let mut plan = compile(&linear_intent(), &catalog()).unwrap();
    assert!(verify_graph_plan_digest(&plan));

    plan.nodes[0].task_id = "attacker-selected-task".to_owned();
    assert!(!verify_graph_plan_digest(&plan));
}

#[test]
fn empty_allowlist_denies_capability() {
    let mut capabilities = catalog();
    capabilities[0].allowed_callers.clear();

    let error = compile(&linear_intent(), &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::UnauthorizedCapability));
}

#[test]
fn unknown_capability_and_unapproved_revision_are_distinct() {
    let mut intent = linear_intent();
    intent.nodes[0].capability_id = "missing".to_owned();
    let unknown = compile(&intent, &catalog()).unwrap_err();
    assert!(has_code(&unknown, DiagnosticCode::UnknownCapability));

    intent.nodes[0].capability_id = "extract".to_owned();
    intent.nodes[0].capability_revision = "v2".to_owned();
    let revision = compile(&intent, &catalog()).unwrap_err();
    assert!(has_code(
        &revision,
        DiagnosticCode::CapabilityRevisionMismatch
    ));
}

#[test]
fn rejects_unknown_ports_and_schema_mismatches() {
    let mut intent = linear_intent();
    intent.edges[0].from.port = "missing".to_owned();
    intent.entries[0].schema = "Wrong/v1".to_owned();

    let error = compile(&intent, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::UnknownPort));
    assert!(has_code(&error, DiagnosticCode::SchemaMismatch));
}

#[test]
fn per_group_requires_one_to_many_and_a_bounded_group() {
    let mut capabilities = catalog();
    capabilities[1].input_ports[0] = many_port("finding", "ExperimentFinding/v1", true);
    let mut intent = linear_intent();
    intent.edges[0].delivery = DeliveryMode::PerGroup {
        expected: GroupCount::Static { count: 3 },
        timeout_secs: None,
    };
    assert!(compile(&intent, &capabilities).is_ok());

    intent.edges[0].delivery = DeliveryMode::PerGroup {
        expected: GroupCount::Static { count: 1 },
        timeout_secs: None,
    };
    let error = compile(&intent, &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::InvalidGroupSize));
}

#[test]
fn per_group_accepts_a_bounded_source_field_and_validates_timeout() {
    let mut capabilities = catalog();
    capabilities[1].input_ports[0] = many_port("finding", "ExperimentFinding/v1", true);
    let mut intent = linear_intent();
    intent.edges[0].delivery = DeliveryMode::PerGroup {
        expected: GroupCount::SourceField {
            field: "expected_total".to_owned(),
        },
        timeout_secs: Some(60),
    };
    intent.edges[0].concurrency = DeliveryConcurrency::Serial;

    let plan = compile(&intent, &capabilities).expect("source-field group is valid");
    assert_eq!(plan.edges[0].delivery, intent.edges[0].delivery);
    assert_eq!(plan.edges[0].concurrency, DeliveryConcurrency::Serial);

    if let DeliveryMode::PerGroup {
        expected: GroupCount::SourceField { field },
        timeout_secs,
    } = &mut intent.edges[0].delivery
    {
        *field = "not-valid!".to_owned();
        *timeout_secs = Some(0);
    }
    let error = compile(&intent, &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::InvalidGroupCountField));
    assert!(has_code(&error, DiagnosticCode::InvalidGroupTimeout));
}

#[test]
fn result_contracts_are_typed_canonical_and_digest_bound() {
    let mut intent = linear_intent();
    intent.results = vec![ResultContract {
        name: "findings".to_owned(),
        from: PortRef {
            node_id: "extract".to_owned(),
            port: "finding".to_owned(),
        },
        cardinality: ResultCardinality::AtMost { count: 8 },
        terminal: true,
    }];

    let mut plan = compile(&intent, &catalog()).expect("valid result contract");
    assert_eq!(plan.results[0].collection, "ExperimentFinding");
    assert!(plan.results[0].terminal);
    assert!(verify_graph_plan_digest(&plan));
    plan.results[0].terminal = false;
    assert!(!verify_graph_plan_digest(&plan));

    intent.results.push(intent.results[0].clone());
    intent.results[1].cardinality = ResultCardinality::Exactly { count: 0 };
    let error = compile(&intent, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::DuplicateResult));
    assert!(has_code(&error, DiagnosticCode::InvalidResultCardinality));
}

#[test]
fn compilation_requires_a_terminal_result_contract() {
    let mut intent = linear_intent();
    intent.results.clear();
    let error = compile(&intent, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::MissingTerminalResult));

    intent.results = vec![ResultContract {
        name: "observations".to_owned(),
        from: PortRef {
            node_id: "extract".to_owned(),
            port: "finding".to_owned(),
        },
        cardinality: ResultCardinality::AtMost { count: 8 },
        terminal: false,
    }];
    let error = compile(&intent, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::MissingTerminalResult));
}

fn package_plan(artifacts: Vec<PlannedPackageArtifact>) -> PackagePlan {
    PackagePlan {
        name: "code-review".to_owned(),
        version: "1.0.0".to_owned(),
        package_digest: format!("sha256:{}", "1".repeat(64)),
        bundled_provenance: BundledProvenance {
            binary_version: "0.12.0".to_owned(),
            build_commit: "test".to_owned(),
        },
        roles: BTreeMap::new(),
        workspace_authority: BTreeMap::new(),
        predecessor_revision_digest: None,
        artifacts,
        required_schema_digests: vec![],
    }
}

#[test]
fn package_plan_order_is_canonical_and_configuration_changes_revision_identity() {
    let artifacts = vec![
        PlannedPackageArtifact {
            logical_id: "review".to_owned(),
            physical_id: "pkg-review".to_owned(),
            kind: PackageArtifactKind::Task,
            content_digest: format!("sha256:{}", "b".repeat(64)),
        },
        PlannedPackageArtifact {
            logical_id: "prepare".to_owned(),
            physical_id: "pkg-prepare".to_owned(),
            kind: PackageArtifactKind::Behavior,
            content_digest: format!("sha256:{}", "a".repeat(64)),
        },
    ];
    let first = bind_package_plan(
        compile(&linear_intent(), &catalog()).unwrap(),
        package_plan(artifacts.clone()),
    );
    let second = bind_package_plan(
        compile(&linear_intent(), &catalog()).unwrap(),
        package_plan(artifacts.into_iter().rev().collect()),
    );
    assert_eq!(first, second);

    let mut configured = package_plan(first.package.clone().unwrap().artifacts);
    configured.roles.insert(
        "reviewer".to_owned(),
        PackageRoleBinding {
            principal_did: "did:key:reviewer".to_owned(),
            deployment_id: "local".to_owned(),
            backend_id: Some("backend".to_owned()),
            profile_id: Some("profile".to_owned()),
            model_name: Some("model".to_owned()),
        },
    );
    let configured = bind_package_plan(compile(&linear_intent(), &catalog()).unwrap(), configured);
    assert_ne!(first.digest, configured.digest);
    assert!(verify_graph_plan_digest(&configured));
}

#[test]
fn required_input_must_have_exactly_one_binding() {
    let mut missing = linear_intent();
    missing.entries.clear();
    let error = compile(&missing, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::MissingInputBinding));

    let mut duplicate = linear_intent();
    duplicate.entries.push(EntryBinding {
        name: "job-again".to_owned(),
        ..duplicate.entries[0].clone()
    });
    let error = compile(&duplicate, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::MultipleInputBindings));
}

#[test]
fn rejects_cycles_and_unreachable_nodes() {
    let mut capabilities = catalog();
    capabilities[0]
        .input_ports
        .push(one_port("review", "Review/v1", false));
    capabilities[1]
        .output_ports
        .push(one_port("review", "Review/v1", false));
    let mut cyclic = linear_intent();
    cyclic.edges.push(GraphEdge {
        from: PortRef {
            node_id: "review".to_owned(),
            port: "review".to_owned(),
        },
        to: PortRef {
            node_id: "extract".to_owned(),
            port: "review".to_owned(),
        },
        delivery: DeliveryMode::PerDocument,
        concurrency: DeliveryConcurrency::Parallel,
        predicate: None,
    });
    let error = compile(&cyclic, &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::Cycle));

    let mut unreachable = linear_intent();
    unreachable.edges.clear();
    let error = compile(&unreachable, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::UnreachableNode));
}

#[test]
fn rejects_requested_and_actual_resource_limit_violations() {
    let mut requested = linear_intent();
    requested.limits.max_nodes = CompilerPolicy::default().max_nodes + 1;
    let error = compile(&requested, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::PlatformLimitExceeded));

    requested = linear_intent();
    requested.limits.max_runtime_secs = 0;
    let error = compile(&requested, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::PlatformLimitExceeded));

    requested.limits.max_runtime_secs = CompilerPolicy::default().max_runtime_secs + 1;
    let error = compile(&requested, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::PlatformLimitExceeded));

    let mut actual = linear_intent();
    actual.limits.max_nodes = 1;
    let error = compile(&actual, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::NodeLimitExceeded));
}

#[test]
fn rejects_depth_and_fan_out_over_intent_limits() {
    let mut depth = linear_intent();
    depth.limits.max_depth = 1;
    let error = compile(&depth, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::DepthLimitExceeded));

    let mut fan_out = linear_intent();
    fan_out.limits.max_fan_out = 0;
    let error = compile(&fan_out, &catalog()).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::FanOutLimitExceeded));
}

#[test]
fn duplicate_nodes_entries_and_capability_ports_fail_closed() {
    let mut intent = linear_intent();
    intent.nodes.push(intent.nodes[0].clone());
    intent.entries.push(intent.entries[0].clone());
    let mut capabilities = catalog();
    let duplicate_port = capabilities[0].output_ports[0].clone();
    capabilities[0].output_ports.push(duplicate_port);

    let error = compile(&intent, &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::DuplicateNode));
    assert!(has_code(&error, DiagnosticCode::DuplicateEntry));
    assert!(has_code(&error, DiagnosticCode::DuplicatePort));
}

#[test]
fn duplicate_capability_revisions_fail_closed() {
    let mut capabilities = catalog();
    capabilities.push(capabilities[0].clone());

    let error = compile(&linear_intent(), &capabilities).unwrap_err();
    assert!(has_code(&error, DiagnosticCode::DuplicateCapability));
}

#[test]
fn diagnostics_are_stably_sorted() {
    let mut intent = linear_intent();
    intent.graph_id.clear();
    intent.entries.clear();
    let error = compile(&intent, &catalog()).unwrap_err();
    let keys: Vec<_> = error
        .diagnostics
        .iter()
        .map(|diagnostic| (&diagnostic.path, &diagnostic.code, &diagnostic.message))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

#[test]
fn intent_schema_rejects_unknown_fields() {
    let raw = serde_json::json!({
        "graph_id": "g",
        "nodes": [],
        "edges": [],
        "entries": [],
        "limits": {
            "max_nodes": 1,
            "max_edges": 1,
            "max_depth": 1,
            "max_fan_out": 1
        },
        "write_directly_to_defradb": true
    });
    assert!(serde_json::from_value::<GraphIntent>(raw).is_err());
}
