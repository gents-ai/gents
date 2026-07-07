//! Structure fence: every Lean model has a declared conformance home.
//!
//! The conformance tree mirrors `proofs/Proofs/` so coverage gaps are
//! STRUCTURAL: adding a Lean model without declaring where its Rust fence
//! lives fails this test. A declaration is one of:
//!
//! - a module in this binary (`conformance/<file>.rs`) — one model per
//!   module, except where two models are genuinely exercised by one harness
//!   (the sharing is then documented inline at the table entry);
//! - `Boundary` — the model is intentionally documented-only (see
//!   `Proofs/Conformance/Boundaries.lean`);
//! - `Gap` — known-missing, with the tracking issue. Gaps are allowed but
//!   loud: they are printed on every run.
//!
//! When you add a model to `Proofs/`, this test fails until you either fence
//! it or declare the gap. That is the point.

use std::collections::BTreeMap;
use std::path::Path;

enum Home {
    /// Fenced by a module of this binary (path relative to tests/).
    Module(&'static str),
    /// Intentionally documented-only (Boundaries.lean).
    Boundary(&'static str),
    /// Known gap, tracked by an issue.
    Gap(&'static str),
}

/// Lean model directory/barrel name → its conformance home.
fn model_homes() -> BTreeMap<&'static str, Home> {
    use Home::*;
    BTreeMap::from([
        ("ApplyReconcile", Module("conformance/apply_reconcile.rs")),
        ("Background", Module("conformance/background.rs")),
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
            "CancelPropagation",
            Module("conformance/cancel_propagation.rs"),
        ),
        (
            "CrossMachineComposed",
            Module("conformance/composed_invariants.rs"),
        ),
        ("EventDelivery", Module("conformance/event_delivery.rs")),
        ("Fleet", Module("conformance/fleet.rs")),
        ("Identity", Module("conformance/identity.rs")),
        ("InferenceCall", Module("conformance/inference_call.rs")),
        ("ManagedExec", Module("conformance/managed_exec.rs")),
        ("MCPHealth", Module("conformance/mcp_health.rs")),
        // PairingReconcile and ReversePairingHandlers deliberately share one
        // home: both models are exercised by the same two-node scenario
        // harness (tests/support/pairing_conformance/), where the handlers
        // are the reconcile loop's transition functions.
        (
            "PairingReconcile",
            Module("conformance/pairing_reconcile.rs"),
        ),
        // Discovery derivation + signed-invite guard. peer_registry_discovery.rs
        // fences the derivation/ownership properties AND the membership half of
        // the join gate (`signedByMember`/`isMember`) via the real
        // `decide_join_admission` engine fn. The signature half (`sigValid`) is
        // fenced separately by defra-agent-protocol::pairing_token verify/tamper
        // tests; identity-binding of the admitted entry is intentionally out of
        // scope (trusted-fleet TOFU — see Transition.join docstring).
        //
        // The §9 network-membership layer (NetworkMembership.lean: admin-signed
        // Membership + member-signed Endpoint → materialization, with the five
        // §9 obligations) lives under this same barrel. peer_registry_discovery.rs
        // fences the executable derivation/reconciliation seam by calling the
        // real `derive_network_desired` and `reconcile_network_tick` functions.
        (
            "PeerRegistryDiscovery",
            Module("conformance/peer_registry_discovery.rs"),
        ),
        (
            "Persistence",
            Boundary("fail-open/closed policies are an accepted boundary (Boundaries.lean)"),
        ),
        ("Process", Module("conformance/process.rs")),
        ("PromptAssembly", Module("conformance/prompt_assembly.rs")),
        ("Recovery", Module("conformance/recovery_sweeps.rs")),
        ("Request", Module("conformance/request_lifecycle.rs")),
        ("RuntimeReconcile", Module("conformance/client_runtime.rs")),
        ("Scheduling", Module("conformance/scheduling.rs")),
        // Scope-template resolution model (deterministic + catalog-total
        // resolveTemplate, pure scopeFilter). Fenced by scope_templates.rs,
        // which calls the real resolution fns; the template-driven reconcile +
        // filter-aware replicator identity is additionally exercised by
        // pairing_reconcile.rs and the p2p_reconcile engine/diff unit tests.
        ("ScopeTemplates", Module("conformance/scope_templates.rs")),
        ("SessionRecovery", Module("conformance/session_recovery.rs")),
        (
            "Skills",
            Gap("#460 — implementation slices unshipped; fence lands with them"),
        ),
        (
            "StorageObservation",
            Boundary("daemon-visible classification is an accepted boundary (Boundaries.lean)"),
        ),
        // Idle-deadline precondition is additionally a registered boundary
        // (boundary.streaming-response.idle-timeout-deadline); the timeout
        // became configurable in #450.
        (
            "StreamingResponse",
            Module("conformance/streaming_compaction.rs"),
        ),
        ("ToolExecution", Module("conformance/tool_execution.rs")),
        ("ToolPolicy", Module("conformance/tool_policy.rs")),
        ("Transcript", Module("conformance/transcript.rs")),
        ("Triggers", Module("conformance/triggers.rs")),
        ("Workflow", Module("workflow_conformance.rs")),
        (
            "ReversePairingHandlers",
            Module("conformance/pairing_reconcile.rs"),
        ),
    ])
}

fn proofs_models(root: &Path) -> Vec<String> {
    let proofs = root.join("crates/defra-agent/proofs/Proofs");
    let mut models = Vec::new();
    for entry in std::fs::read_dir(&proofs).expect("read Proofs/").flatten() {
        let path = entry.path();
        // A model is a top-level barrel: <Name>.lean (dirs are its submodules).
        if path.extension().is_some_and(|ext| ext == "lean") {
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            // Infrastructure, not models.
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
                            root.join("crates/defra-agent/tests").join(path).exists(),
                            "{model}: declared conformance module {path} does not exist"
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

    // Declared gaps are allowed but loud.
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
