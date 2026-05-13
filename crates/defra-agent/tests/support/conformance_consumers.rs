use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub enum ConformanceConsumer {
    RustTest {
        id: &'static str,
        package: &'static str,
        source_path: &'static str,
        module_path: &'static str,
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

    fn assert_resolves(&self, repo_root: &Path, sources: &mut BTreeMap<&'static str, String>) {
        match self {
            Self::RustTest {
                id,
                package,
                source_path,
                module_path,
                function,
            } => {
                let source = cached_source(repo_root, sources, source_path);
                assert_rust_test_function(source, id, package, source_path, module_path, function);
            }
            Self::TypeScriptTest {
                id,
                app,
                source_path,
                suite,
                test,
            } => {
                let source = cached_source(repo_root, sources, source_path);
                assert_typescript_test(source, id, app, source_path, suite, test);
            }
        }
    }
}

/// Registry for `CoverageLedger.lean` consumer ids.
///
/// When a generated Lean contract gets a new Rust or TypeScript consumer, add
/// the test here in the same change as the Lean ledger entry. Rust entries
/// resolve to a real `#[test]` or `#[tokio::test]` function at the named module
/// path in the named source file; TypeScript entries resolve to a concrete test
/// in the named suite.
pub fn registered_conformance_consumers() -> &'static [ConformanceConsumer] {
    &[
        ConformanceConsumer::RustTest {
            id: "admission::tests::generated_inference_slot_accounting_cases_match_admission_reconstruction_logic",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            module_path: "admission::tests",
            function: "generated_inference_slot_accounting_cases_match_admission_reconstruction_logic",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::generated_slot_accounting_fleet_cases_match_admission_runtime_boundary",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            module_path: "admission::tests",
            function: "generated_slot_accounting_fleet_cases_match_admission_runtime_boundary",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::rust_inference_call_state_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            module_path: "admission::tests",
            function: "rust_inference_call_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::rust_inference_call_terminal_reason_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            module_path: "admission::tests",
            function: "rust_inference_call_terminal_reason_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::rust_inference_call_transition_table_matches_lean_contract",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/admission/tests.rs",
            module_path: "admission::tests",
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
            module_path: "apply_conformance",
            function: "generated_apply_reconcile_cases_drive_apply_model_and_production_ordering",
        },
        ConformanceConsumer::RustTest {
            id: "backend_registry::tests::generated_backend_health_admission_cases_match_registry_and_admission_policy",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/backend_registry/tests.rs",
            module_path: "backend_registry::tests",
            function: "generated_backend_health_admission_cases_match_registry_and_admission_policy",
        },
        ConformanceConsumer::RustTest {
            id: "defra_agent_desktop_tauri::bridge::snapshot::tests::session_state::session_snapshot_projection_consumes_generated_client_shell_contract_cases",
            package: "defra-agent-desktop-tauri",
            source_path: "apps/desktop-tauri/src-tauri/src/bridge/snapshot/tests/session_state.rs",
            module_path: "defra_agent_desktop_tauri::bridge::snapshot::tests::session_state",
            function: "session_snapshot_projection_consumes_generated_client_shell_contract_cases",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::generated_persistence_failure_policy_cases_match_hook_decisions",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/hook/tests.rs",
            module_path: "hook::tests",
            function: "generated_persistence_failure_policy_cases_match_hook_decisions",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::generated_storage_observation_cases_match_hook_runtime_classification",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/hook/tests.rs",
            module_path: "hook::tests",
            function: "generated_storage_observation_cases_match_hook_runtime_classification",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::tests::request_state_machine_contract_is_complete",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/lifecycle.rs",
            module_path: "lifecycle::tests",
            function: "request_state_machine_contract_is_complete",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::tests::rust_execution_origin_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/lifecycle.rs",
            module_path: "lifecycle::tests",
            function: "rust_execution_origin_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::tests::rust_request_lifecycle_state_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/lifecycle.rs",
            module_path: "lifecycle::tests",
            function: "rust_request_lifecycle_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "live_overlay_conformance::live_overlay_cases_match_lean_table",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/live_overlay_conformance.rs",
            module_path: "live_overlay_conformance",
            function: "live_overlay_cases_match_lean_table",
        },
        ConformanceConsumer::RustTest {
            id: "mcp_pool::tests::tool_retry_disposition_contract_cases_match_mcp_pool_policy",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/mcp_pool/tests.rs",
            module_path: "mcp_pool::tests",
            function: "tool_retry_disposition_contract_cases_match_mcp_pool_policy",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::runtime_status_generation_updates_match_lean_runtime_reconcile_cases",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "runtime_status_generation_updates_match_lean_runtime_reconcile_cases",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::generated_process_transition_cases_match_runtime_status_policy",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "generated_process_transition_cases_match_runtime_status_policy",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::rust_process_state_transitions_match_lean_contract",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "rust_process_state_transitions_match_lean_contract",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::rust_process_state_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "rust_process_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "rust_reconcile_phase_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "state_machine_conformance::generated_session_recovery_cases_drive_db_backed_reissue_contract",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
            module_path: "state_machine_conformance",
            function: "generated_session_recovery_cases_drive_db_backed_reissue_contract",
        },
        ConformanceConsumer::RustTest {
            id: "state_machine_conformance::generated_request_transition_cases_cover_lifecycle_policy",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
            module_path: "state_machine_conformance",
            function: "generated_request_transition_cases_cover_lifecycle_policy",
        },
        ConformanceConsumer::RustTest {
            id: "state_machine_conformance::generated_queue_deadline_cases_pin_r4a_contract_rows",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
            module_path: "state_machine_conformance",
            function: "generated_queue_deadline_cases_pin_r4a_contract_rows",
        },
        ConformanceConsumer::RustTest {
            id: "state_machine_conformance::generated_recovery_sweep_cases_pin_startup_recovery_contract",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
            module_path: "state_machine_conformance",
            function: "generated_recovery_sweep_cases_pin_startup_recovery_contract",
        },
        ConformanceConsumer::RustTest {
            id: "state_machine_conformance::generated_tool_execution_cases_cover_preflight_and_retry_contracts",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
            module_path: "state_machine_conformance",
            function: "generated_tool_execution_cases_cover_preflight_and_retry_contracts",
        },
        ConformanceConsumer::RustTest {
            id: "state_machine_conformance::lean_executable_contracts_cover_initial_domains",
            package: "defra-agent",
            source_path: "crates/defra-agent/tests/state_machine_conformance.rs",
            module_path: "state_machine_conformance",
            function: "lean_executable_contracts_cover_initial_domains",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_command_env_cases_match_rust_filtering",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/toolset/tests.rs",
            module_path: "toolset::tests",
            function: "generated_command_env_cases_match_rust_filtering",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_command_policy_cases_match_rust_validation",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/toolset/tests.rs",
            module_path: "toolset::tests",
            function: "generated_command_policy_cases_match_rust_validation",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_command_sandbox_cases_match_rust_selection",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/toolset/tests.rs",
            module_path: "toolset::tests",
            function: "generated_command_sandbox_cases_match_rust_selection",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_native_filesystem_boundary_cases_match_preemptible_boundary_contract",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/toolset/tests.rs",
            module_path: "toolset::tests",
            function: "generated_native_filesystem_boundary_cases_match_preemptible_boundary_contract",
        },
        ConformanceConsumer::RustTest {
            id: "trigger_engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/trigger_engine/tests.rs",
            module_path: "trigger_engine::tests",
            function: "trigger_engine_dispatch_matches_lean_generated_contract_cases",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::tests::rust_tool_call_state_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/tool_call_lifecycle.rs",
            module_path: "tool_call_lifecycle::tests",
            function: "rust_tool_call_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::tests::rust_failure_class_vocabulary_matches_lean_model",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/tool_call_lifecycle.rs",
            module_path: "tool_call_lifecycle::tests",
            function: "rust_failure_class_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::tests::tool_call_state_machine_contract_is_complete",
            package: "defra-agent",
            source_path: "crates/defra-agent/src/tool_call_lifecycle.rs",
            module_path: "tool_call_lifecycle::tests",
            function: "tool_call_state_machine_contract_is_complete",
        },
    ]
}

pub fn assert_registered_conformance_consumers_resolve() -> BTreeSet<&'static str> {
    let repo_root = repo_root();
    let mut sources = BTreeMap::new();
    let mut ids = BTreeSet::new();
    for consumer in registered_conformance_consumers() {
        let id = consumer.id();
        assert!(
            ids.insert(id),
            "duplicate registered conformance consumer id: {id}"
        );
        consumer.assert_resolves(&repo_root, &mut sources);
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

fn cached_source<'a>(
    repo_root: &Path,
    sources: &'a mut BTreeMap<&'static str, String>,
    source_path: &'static str,
) -> &'a str {
    if !sources.contains_key(source_path) {
        sources.insert(source_path, read_source(repo_root, source_path));
    }
    sources
        .get(source_path)
        .expect("source should be cached after insertion")
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
    module_path: &str,
    function: &str,
) {
    let expected_id = format!("{module_path}::{function}");
    assert!(
        id == expected_id,
        "registered Rust consumer {id:?} in package {package} must equal {expected_id:?}"
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

    let suite_call = find_ts_call(source, &["describe"], suite, 0, source.len()).unwrap_or_else(|| {
        panic!(
            "registered TypeScript consumer {id:?} for app {app} must resolve suite {suite:?} in {source_path}"
        )
    });
    let suite_open = source[suite_call.literal_end..]
        .find('{')
        .map(|offset| suite_call.literal_end + offset)
        .unwrap_or_else(|| {
            panic!(
                "registered TypeScript consumer {id:?} for app {app} resolved suite {suite:?} in {source_path}, but the suite callback body was not found"
            )
        });
    let suite_close = matching_brace(source, suite_open).unwrap_or_else(|| {
        panic!(
            "registered TypeScript consumer {id:?} for app {app} resolved suite {suite:?} in {source_path}, but the suite callback body was not balanced"
        )
    });
    assert!(
        find_ts_call(source, &["test", "it"], test, suite_open, suite_close).is_some(),
        "registered TypeScript consumer {id:?} for app {app} must resolve test {test:?} inside suite {suite:?} in {source_path}"
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

#[derive(Debug, Clone, Copy)]
struct TsCall {
    literal_end: usize,
}

fn find_ts_call(
    source: &str,
    callees: &[&str],
    first_arg: &str,
    start: usize,
    end: usize,
) -> Option<TsCall> {
    let mut offset = start;
    while offset < end {
        let remaining = &source[offset..end];
        let next = callees
            .iter()
            .filter_map(|callee| {
                remaining
                    .find(callee)
                    .map(|index| (offset + index, *callee))
            })
            .min_by_key(|(index, _)| *index)?;
        let call_start = next.0;
        let callee = next.1;
        offset = call_start + callee.len();
        if !is_identifier_boundary(source, call_start, callee.len()) {
            continue;
        }

        let Some(open_paren) = skip_ws(source, offset, end) else {
            continue;
        };
        if source.as_bytes().get(open_paren) != Some(&b'(') {
            continue;
        }
        let Some(arg_start) = skip_ws(source, open_paren + 1, end) else {
            continue;
        };
        let Some(literal_end) = quoted_literal_matches(source, arg_start, end, first_arg) else {
            continue;
        };
        return Some(TsCall { literal_end });
    }
    None
}

fn is_identifier_boundary(source: &str, start: usize, len: usize) -> bool {
    let before = start
        .checked_sub(1)
        .and_then(|index| source.as_bytes().get(index))
        .copied();
    let after = source.as_bytes().get(start + len).copied();
    !before.is_some_and(is_identifier_byte) && !after.is_some_and(is_identifier_byte)
}

fn is_identifier_byte(byte: u8) -> bool {
    byte == b'_' || byte == b'$' || byte.is_ascii_alphanumeric()
}

fn skip_ws(source: &str, start: usize, end: usize) -> Option<usize> {
    (start..end).find(|index| {
        source
            .as_bytes()
            .get(*index)
            .is_some_and(|byte| !byte.is_ascii_whitespace())
    })
}

fn quoted_literal_matches(source: &str, start: usize, end: usize, expected: &str) -> Option<usize> {
    let quote = *source.as_bytes().get(start)?;
    if !matches!(quote, b'\'' | b'"' | b'`') {
        return None;
    }

    let mut index = start + 1;
    let literal_start = index;
    while index < end {
        match source.as_bytes()[index] {
            b'\\' => index += 2,
            byte if byte == quote => {
                if &source[literal_start..index] == expected {
                    return Some(index + 1);
                }
                return None;
            }
            _ => index += 1,
        }
    }
    None
}

fn matching_brace(source: &str, open: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(open) != Some(&b'{') {
        return None;
    }

    let mut depth = 1usize;
    let mut index = open + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => index = skip_quoted(source, index)?,
            b'/' if bytes.get(index + 1) == Some(&b'/') => index = skip_line_comment(bytes, index),
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index)?
            }
            b'{' => {
                depth += 1;
                index += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    None
}

fn skip_quoted(source: &str, start: usize) -> Option<usize> {
    let quote = source.as_bytes()[start];
    let mut index = start + 1;
    while index < source.len() {
        match source.as_bytes()[index] {
            b'\\' => index += 2,
            byte if byte == quote => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

fn skip_line_comment(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(|byte| *byte == b'\n')
        .map_or(bytes.len(), |offset| start + offset + 1)
}

fn skip_block_comment(bytes: &[u8], start: usize) -> Option<usize> {
    bytes[start + 2..]
        .windows(2)
        .position(|window| window == b"*/")
        .map(|offset| start + offset + 4)
}
