use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub enum ConformanceConsumer {
    RustTest {
        id: &'static str,
        package: &'static str,
        source_path: &'static str,
        function: &'static str,
    },
    TypeScriptTest {
        id: &'static str,
        app: &'static str,
        source_path: &'static str,
        suite: &'static str,
        test: &'static str,
    },
}

impl ConformanceConsumer {
    pub fn id(&self) -> &'static str {
        match self {
            Self::RustTest { id, .. } | Self::TypeScriptTest { id, .. } => id,
        }
    }

    fn assert_resolves(self, repo_root: &Path) {
        match self {
            Self::RustTest {
                id,
                package,
                source_path,
                function,
            } => {
                let source = read_source(repo_root, source_path);
                assert_rust_test_function(&source, id, package, source_path, function);
            }
            Self::TypeScriptTest {
                id,
                app,
                source_path,
                suite,
                test,
            } => {
                let source = read_source(repo_root, source_path);
                assert_typescript_test(&source, id, app, source_path, suite, test);
            }
        }
    }
}

/// Registry for `CoverageLedger.lean` consumer ids.
///
/// When a generated Lean contract gets a new Rust or TypeScript consumer, add
/// the test here in the same change as the Lean ledger entry. Rust entries
/// resolve to a real `#[test]` or `#[tokio::test]` function in the named source
/// file; TypeScript entries resolve to a concrete test in the named suite.
pub fn registered_conformance_consumers() -> &'static [ConformanceConsumer] {
    &[
        ConformanceConsumer::RustTest {
            id: "admission::tests::generated_inference_slot_accounting_cases_match_admission_reconstruction_logic",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            function: "generated_inference_slot_accounting_cases_match_admission_reconstruction_logic",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::generated_slot_accounting_fleet_cases_match_admission_runtime_boundary",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            function: "generated_slot_accounting_fleet_cases_match_admission_runtime_boundary",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::rust_inference_call_state_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            function: "rust_inference_call_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::rust_inference_call_terminal_reason_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            function: "rust_inference_call_terminal_reason_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::rust_inference_call_transition_table_matches_lean_contract",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            function: "rust_inference_call_transition_table_matches_lean_contract",
        },
        ConformanceConsumer::TypeScriptTest {
            id: "apps/desktop-tauri/src/lib/chat-shell.test.ts::projectChatShell matches generated Lean ClientShell projection contracts",
            app: "desktop-tauri",
            source_path: "apps/desktop-tauri/src/lib/chat-shell.test.ts",
            suite: "projectChatShell",
            test: "matches generated Lean ClientShell projection contracts",
        },
        ConformanceConsumer::RustTest {
            id: "apply_conformance::generated_apply_reconcile_cases_drive_apply_model_and_production_ordering",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/apply_conformance.rs",
            function: "generated_apply_reconcile_cases_drive_apply_model_and_production_ordering",
        },
        ConformanceConsumer::RustTest {
            id: "backend_registry::tests::generated_backend_health_admission_cases_match_registry_and_admission_policy",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/backend_registry/tests.rs",
            function: "generated_backend_health_admission_cases_match_registry_and_admission_policy",
        },
        ConformanceConsumer::RustTest {
            id: "defra_agent_desktop_tauri::bridge::snapshot::tests::session_state::session_snapshot_projection_consumes_generated_client_shell_contract_cases",
            package: "defra-agent-desktop-tauri",
            source_path: "apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_state.rs",
            function: "session_snapshot_projection_consumes_generated_client_shell_contract_cases",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::generated_persistence_failure_policy_cases_match_hook_decisions",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/hook/tests.rs",
            function: "generated_persistence_failure_policy_cases_match_hook_decisions",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::generated_storage_observation_cases_match_hook_runtime_classification",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/hook/tests.rs",
            function: "generated_storage_observation_cases_match_hook_runtime_classification",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::tests::request_state_machine_contract_is_complete",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/lifecycle.rs",
            function: "request_state_machine_contract_is_complete",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::tests::rust_execution_origin_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/lifecycle.rs",
            function: "rust_execution_origin_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::tests::rust_request_lifecycle_state_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/lifecycle.rs",
            function: "rust_request_lifecycle_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "mcp_pool::tests::tool_retry_disposition_contract_cases_match_mcp_pool_policy",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/mcp_pool/tests.rs",
            function: "tool_retry_disposition_contract_cases_match_mcp_pool_policy",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::runtime_status_generation_updates_match_lean_runtime_reconcile_cases",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/runtime_status/tests.rs",
            function: "runtime_status_generation_updates_match_lean_runtime_reconcile_cases",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::rust_process_state_transitions_match_lean_contract",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/runtime_status/tests.rs",
            function: "rust_process_state_transitions_match_lean_contract",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::rust_process_state_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/runtime_status/tests.rs",
            function: "rust_process_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/runtime_status/tests.rs",
            function: "rust_reconcile_phase_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "state_machine_conformance::generated_session_recovery_cases_drive_db_backed_reissue_contract",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
            function: "generated_session_recovery_cases_drive_db_backed_reissue_contract",
        },
        ConformanceConsumer::RustTest {
            id: "state_machine_conformance::generated_tool_execution_cases_cover_preflight_and_retry_contracts",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
            function: "generated_tool_execution_cases_cover_preflight_and_retry_contracts",
        },
        ConformanceConsumer::RustTest {
            id: "state_machine_conformance::lean_executable_contracts_cover_initial_domains",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
            function: "lean_executable_contracts_cover_initial_domains",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_command_env_cases_match_rust_filtering",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/toolset/tests.rs",
            function: "generated_command_env_cases_match_rust_filtering",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_command_policy_cases_match_rust_validation",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/toolset/tests.rs",
            function: "generated_command_policy_cases_match_rust_validation",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_command_sandbox_cases_match_rust_selection",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/toolset/tests.rs",
            function: "generated_command_sandbox_cases_match_rust_selection",
        },
        ConformanceConsumer::RustTest {
            id: "trigger_engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/trigger_engine/tests.rs",
            function: "trigger_engine_dispatch_matches_lean_generated_contract_cases",
        },
    ]
}

pub fn assert_registered_conformance_consumers_resolve() -> BTreeSet<&'static str> {
    let repo_root = repo_root();
    let mut ids = BTreeSet::new();
    for consumer in registered_conformance_consumers() {
        let id = consumer.id();
        assert!(
            ids.insert(id),
            "duplicate registered conformance consumer id: {id}"
        );
        consumer.assert_resolves(&repo_root);
    }
    ids
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|path| {
            path.join("crates/defra-agent/proofs/lakefile.lean")
                .exists()
        })
        .expect("repository root should contain crates/defra-agent/proofs/lakefile.lean")
        .to_path_buf()
}

fn read_source(repo_root: &Path, source_path: &str) -> String {
    let path = repo_root.join(source_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn assert_rust_test_function(
    source: &str,
    id: &str,
    package: &str,
    source_path: &str,
    function: &str,
) {
    assert!(
        id.ends_with(&format!("::{function}")),
        "registered Rust consumer {id:?} in package {package} must end with ::{function}"
    );

    let needle = format!("fn {function}(");
    let matches = source.match_indices(&needle).collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "registered Rust consumer {id:?} in package {package} must resolve to exactly one `{needle}` in {source_path}; found {}",
        matches.len()
    );

    let declaration_line_start = source[..matches[0].0]
        .rfind('\n')
        .map_or(0, |index| index + 1);
    let attrs = preceding_attribute_block(&source[..declaration_line_start]);
    assert!(
        attrs
            .iter()
            .any(|attr| attr.starts_with("#[test") || attr.starts_with("#[tokio::test")),
        "registered Rust consumer {id:?} in package {package} resolves to {source_path}::{function}, but that function is not marked #[test] or #[tokio::test]"
    );
}

fn assert_typescript_test(
    source: &str,
    id: &str,
    app: &str,
    source_path: &str,
    suite: &str,
    test: &str,
) {
    assert!(
        id.starts_with(source_path),
        "registered TypeScript consumer {id:?} for app {app} must start with source path {source_path}"
    );
    assert!(
        source.contains(&format!("describe(\"{suite}\"")),
        "registered TypeScript consumer {id:?} for app {app} must resolve suite {suite:?} in {source_path}"
    );
    assert!(
        source.contains(&format!("test(\"{test}\"")),
        "registered TypeScript consumer {id:?} for app {app} must resolve test {test:?} in {source_path}"
    );
}

fn preceding_attribute_block(source_before_fn: &str) -> Vec<&str> {
    let mut attrs = Vec::new();
    for line in source_before_fn.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("#[") {
            attrs.push(trimmed);
            continue;
        }
        break;
    }
    attrs
}
