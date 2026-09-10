use std::collections::BTreeMap;
use std::path::Path;

enum Home {
    Module(&'static str),
    WorkspaceTest(&'static str),
    Boundary(&'static str),
    Gap(&'static str),
}

fn model_homes() -> BTreeMap<&'static str, Home> {
    use Home::*;
    BTreeMap::from([
        ("AgentSession", Gap("Canonical DB projection tests live in request_lifecycle.rs; generated exact-scope selection/refresh adapters still require the migrated session owner.")),
        ("SessionFork", WorkspaceTest("crates/gents/tests/e2e_runtime/fork_invariants.rs")),
        ("ApplyReconcile", Gap("Atomic publication requires the canonical transaction/installer owner; the retired ranked writer and test-local simulator do not implement the contract.")),
        ("Background", Module("conformance/background.rs")),
        ("BackendHealth", Module("conformance/backend_health.rs")),
        ("Callback", Module("conformance/callback_lifecycle.rs")),
        ("Client", Module("conformance/client_runtime.rs")),
        (
            "ClientShell",
            Boundary("projection theorems; desktop rendering is runtime-observed"),
        ),
        ("CodexShim", Module("conformance/codex_shim.rs")),
        ("CommandPolicy", Module("conformance/command_policy.rs")),
        ("Compaction", Module("conformance/streaming_compaction.rs")),
        ("CompletionRetry", Module("conformance/completion_retry.rs")),
        (
            "ConfigDefaults",
            Gap(
                "#1436 Lean contracts: signed-limit decoding fences have no generated \
                 contract output; canonical serde tests cover authoring, not generated refinement",
            ),
        ),
        (
            "ConfigDocuments",
            Gap(
                "#1436 Lean contracts: canonical root names/fields have no generated \
                 contract output; canonical serde tests cover authoring, not generated refinement",
            ),
        ),
        (
            "Configuration",
            Gap(
                "Owner-scoped configuration and discovery cases are exported; real registry/discovery adapters remain the Rust layer obligation, not a test-local resolver",
            ),
        ),
        (
            "CancelPropagation",
            Module("conformance/cancel_propagation.rs"),
        ),
        (
            "CrossMachineComposed",
            Module("conformance/composed_invariants.rs"),
        ),
        ("DurableLineage", Module("conformance/background.rs")),
        ("DescendantGraph", Module("misc/descendant_graph.rs")),
        ("EditMatch", Module("conformance/edit_match.rs")),
        ("EthSubmission", Module("conformance/eth_submission.rs")),
        ("Enrollment", Module("conformance/enrollment.rs")),
        ("EventDelivery", Module("conformance/event_delivery.rs")),
        ("Fleet", Module("conformance/fleet.rs")),
        ("GoalAutomation", Module("conformance/goals.rs")),
        ("Goals", Module("conformance/goals.rs")),
        (
            "GraphPipeline",
            Module("conformance/graph_pipeline.rs"),
        ),
        ("Identity", Module("conformance/identity.rs")),
        ("InferenceCall", Module("conformance/inference_call.rs")),
        ("ManagedExec", Gap("Real process-group/job kill and bounded-drain tests live in src/managed_exec.rs; generated OS/process-tree cases still need those consumers, not fixture-only flags.")),
        ("Mailbox", Module("conformance/mailbox.rs")),
        ("MCPHealth", Module("conformance/mcp_health.rs")),
        (
            "Migration",
            WorkspaceTest("crates/gents-migration/tests/phase_b_steps.rs"),
        ),
        (
            "PairingReconcile",
            Module("conformance/pairing_reconcile.rs"),
        ),
        (
            "Persistence",
            Boundary("fail-open/closed policies are an accepted boundary (Boundaries.lean)"),
        ),
        (
            "P2PBackpressure",
            Boundary(
                "obligation model + operator surface for #630; not a flood-safety fence — queue-admission, retained JoinHandles, and durable pending-DAG recovery require defradb.rs work (boundary.p2p-backpressure.obligation-model)",
            ),
        ),
        ("Process", WorkspaceTest("crates/gents/src/runtime_status/tests.rs")),
        ("PromptAssembly", Module("conformance/prompt_assembly.rs")),
        ("Recovery", Module("conformance/recovery_sweeps.rs")),
        (
            "RenderedCapture",
            Module("conformance/rendered_capture.rs"),
        ),
        ("Request", Module("conformance/request_lifecycle.rs")),
        (
            "RequestExecutionLease",
            WorkspaceTest("crates/gents/src/lean_vocab_test/request_execution_lease_policy.rs"),
        ),
        ("RuntimeReconcile", Module("conformance/client_runtime.rs")),
        ("Scheduling", Module("conformance/scheduling.rs")),
        ("ScopeTemplates", Module("conformance/scope_templates.rs")),
        ("SelfConfig", Module("conformance/self_config.rs")),
        (
            "SessionHydration",
            Module("conformance/session_hydration.rs"),
        ),
        ("SessionRecovery", WorkspaceTest("crates/gents-desktop-core/src/client/mutations/chat/request/tests.rs")),
        (
            "Skills",
            Gap("#460 — implementation slices unshipped; fence lands with them"),
        ),
        (
            "StorageObservation",
            Boundary("daemon-visible classification is an accepted boundary (Boundaries.lean)"),
        ),
        (
            "StreamingResponse",
            Module("conformance/streaming_compaction.rs"),
        ),
        (
            "TaskHooks",
            Gap(
                "#1436 Lean contracts: hook phase/recovery theorems have no generated \
                 contract output; admission/phase/recovery need the shared runtime hook owner",
            ),
        ),
        ("ToolExecution", Module("conformance/tool_execution.rs")),
        ("ToolPolicy", Module("conformance/tool_policy.rs")),
        ("Lsp", Module("conformance/lsp.rs")),
        ("Transcript", Module("conformance/transcript.rs")),
        ("Triggers", Module("conformance/triggers.rs")),
        ("Workspace", Gap("Canonical binding/admission fixtures await the workspace runtime owner; the removed test-local predicate mirror was not implementation coverage.")),
        (
            "ReversePairingHandlers",
            Module("conformance/pairing_reconcile.rs"),
        ),
    ])
}

fn proofs_models(root: &Path) -> Vec<String> {
    let proofs = root.join("crates/gents/proofs/Proofs");
    let mut models = Vec::new();
    for entry in std::fs::read_dir(&proofs).expect("read Proofs/").flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "lean") {
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            if matches!(name.as_str(), "Basic" | "Conformance") {
                continue;
            }
            models.push(name);
        }
    }
    models.sort();
    models
}

#[test]
fn every_lean_model_has_a_declared_conformance_home() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf();

    let homes = model_homes();
    let models = proofs_models(&root);

    let mut undeclared = Vec::new();
    let mut dangling: Vec<&str> = homes.keys().copied().collect();
    let mut gaps = Vec::new();

    for model in &models {
        match homes.get(model.as_str()) {
            None => undeclared.push(model.clone()),
            Some(home) => {
                dangling.retain(|name| name != model);
                match home {
                    Home::Module(path) => {
                        assert!(
                            root.join("crates/gents/tests").join(path).exists(),
                            "{model}: declared conformance module {path} does not exist"
                        );
                    }
                    Home::WorkspaceTest(path) => {
                        assert!(
                            root.join(path).exists(),
                            "{model}: declared workspace conformance test {path} does not exist"
                        );
                    }
                    Home::Boundary(rationale) => {
                        eprintln!("  BOUNDARY {model}: {rationale}");
                    }
                    Home::Gap(issue) => gaps.push(format!("{model}: {issue}")),
                }
            }
        }
    }

    if !gaps.is_empty() {
        eprintln!("declared conformance gaps ({}):", gaps.len());
        for gap in &gaps {
            eprintln!("  GAP {gap}");
        }
    }

    assert!(
        undeclared.is_empty(),
        "Lean models with NO declared conformance home (fence them, declare a \
         boundary, or declare a tracked gap in conformance/structure.rs):\n{}",
        undeclared.join("\n")
    );
    assert!(
        dangling.is_empty(),
        "conformance homes declared for Lean models that no longer exist \
         (remove from conformance/structure.rs):\n{}",
        dangling.join("\n")
    );
}
