# Agent Readability Refactor Plan

Companion to the desktop readability refactor (`docs/issues/2026-04-16-desktop-readability-refactor-plan.md`). Covers `crates/defra-agent` and `crates/defra-agent-cli`. The `crates/defra-agent-protocol` crate is already healthy and is out of scope.

## Goal

Reduce file size, sharpen module boundaries, extract inline tests, and cut comment noise across the agent library and CLI without changing behavior.

## Rules

- No new feature work in oversized catch-all files.
- Prefer extraction along existing state and workflow boundaries; do not invent new abstractions during a readability pass.
- Aim for `250-400` lines for most files and treat `500` as a hard warning threshold. Apply this bar to both crates.
- When a strict numeric cap would force artificial splits that hide cohesion, apply the **soft-cap fallback**: drop the numeric cap for that file and split by subcommand or feature area instead. The fallback must be justified in the PR description.
- `mod.rs` files become dispatch / re-export shells, not implementation dumps.
- Inline `#[cfg(test)] mod tests { ... }` blocks are not acceptable at end of phase. Every module's tests live in a sibling `<module>/tests.rs` or a `<module>/tests/` directory.
- Shared test helpers (mock HTTP servers, fixtures, waits) live in a single per-crate `tests/support/` layer, not duplicated across files.
- One file (or tightly-coupled file + test pair) per PR.

### Lean-spec boundary

This effort is readability-only. State-machine and lifecycle semantics do not change. Any phase touching `admission/`, `lifecycle/`, `agent/reconcile*`, `agent/runtime*`, `scheduler/`, `session/`, `hook/persistence.rs`, or `streaming/` must pass `cargo test -p defra-agent`, `tests/state_machine_conformance.rs`, and `tests/lifecycle_regression.rs` with no changes to expectations. A readability PR that fails conformance is a bug, not a spec update. Real behavior changes start in Lean first per `CLAUDE.md`.

### Comment cleanup policy

Each phase trims the files it touches:

- Delete comments that restate the code.
- Delete change-log / history comments ("added for X", "fix for issue Y", "used to do Z").
- Delete section banner comments (`// === setup ===`) unless the file is one we have deliberately kept long.
- Rewrite `///` doc comments on public items to be terse and current; drop fluff paragraphs.
- Keep only comments that explain a non-obvious *why* -- hidden invariants, subtle ordering, workarounds with issue links.

### Working agreement

- Work lives in an adjacent worktree (`../defra-agent-agent-readability/` or similar branch-named sibling dir) so it does not collide with the in-flight desktop refactor.
- Desktop refactor and agent refactor run in parallel; the file sets do not overlap, so PRs land independently.
- Spec and design artefacts are committed to `main` first; implementation happens on the worktree branch.

## Active Order

Agent library first, CLI second. The library is backed by the Lean spec + conformance tests, giving the strongest safety net for establishing refactor patterns and voice for comments. Apply the patterns to the CLI second.

1. `defra-agent/src/admission/mod.rs` (A1)
2. `defra-agent/src/agent/runtime.rs` + `runtime/tests.rs` (A2)
3. `defra-agent/src/document_config.rs` (A3)
4. `defra-agent/src/tool_surface.rs` (A4)
5. `defra-agent/src/scheduler/tests.rs` (A5)
6. `defra-agent/src/agent/tests.rs` (A6)
7. `defra-agent/tests/lifecycle_regression.rs` + `tests/backend_auth.rs` (A7)
8. `defra-agent/src/agent/document_view.rs` (A8)
9. `defra-agent` borderline sweep + test-support polish (A9)
10. `defra-agent-cli/src/main.rs` HTTP / Prometheus extraction (B1)
11. `defra-agent-cli/src/main.rs` document-writer extraction (B2)
12. `defra-agent-cli/src/main.rs` CLI type + command scaffold (B3)
13. `defra-agent-cli/src/main.rs` `init` + `reset` + `serve` handlers (B4)
14. `defra-agent-cli/src/main.rs` `config` subcommand family (B5)
15. `defra-agent-cli/src/main.rs` `p2p` subcommand family (B6)
16. `defra-agent-cli/src/main.rs` remaining handlers (B7)
17. `defra-agent-cli/src/main.rs` finalization (B8)
18. `defra-agent-cli/src/desired_state.rs` (B9)
19. `defra-agent-cli/src/tui.rs` (B10)
20. `defra-agent-cli/tests/cli_e2e.rs` (B11)

## Status

### Phase A1: `src/admission/mod.rs` (1456 lines)

Split into a `mod.rs` shell plus one file per responsibility, keeping the existing `stream_guard.rs`:

- `src/admission/config.rs` -- `BackendAdmissionConfig`, `backend_admission_configs_from_backends`
- `src/admission/client.rs` -- `AdmittedCompletionClient`, `AdmittedCompletionModel`, `CallKind`, `AdmissionCallContext`, `scope_request`, `scope_call`, `current_context`
- `src/admission/registry.rs` -- `AdmissionRegistry`, `AdmissionRegistryInner`, `RegistryState`, `PendingControllerConfig`
- `src/admission/controller.rs` -- `BackendAdmissionController`, `QueuedCallGuard`, `PendingCallMetadata`, `InferenceCallRecord`
- `src/admission/permit.rs` -- `AdmissionPermit`, `PermitTerminal`, `Drop` and `StreamGuardLifecycle` impls, `spawn_persistence`, `completion_persistence_error`
- `src/admission/persistence.rs` -- `persist_call_queued`, `persist_call_started`, `persist_existing_call_running`, `persist_terminal_call`, `persist_existing_call_terminal`, `add_call_mutation`, `upsert_call_running_mutation`, `upsert_call_terminal_mutation`, `optional_graphql_string`, `usage_fields`, `extract_inference_call_doc_id`
- `src/admission/tests/` -- unit tests split by sibling module

Status: not started

### Phase A2: `src/agent/runtime.rs` + `runtime/tests.rs` (1119 + 1386 lines)

Split together; the tests already follow the module seams.

- `src/agent/runtime/mod.rs` -- shell only (re-exports, no impls)
- `src/agent/runtime/context.rs` -- `RuntimeContext` with its ~355-line impl block, `StartupBarrier`, `BehaviorResolution`, `BackgroundTaskResult`
- `src/agent/runtime/router.rs` -- `run_router`, `run_router_with_watcher`, `wait_for_next_request_with_latest_snapshot`, `run_router_generation_observer`, `resolve_behavior_for_request`, `fail_routed_request`, `normalize_optional_string`, `format_pending_visibility_error`
- `src/agent/runtime/control_watcher.rs` -- `run_control_watcher`
- `src/agent/runtime/startup.rs` -- `validate_startup_snapshot`, `resolve_startup_snapshot`, `resolve_backend_admission_configs`, `resolve_tool_surfaces`, `resolve_document_snapshot_with_tools`, `log_recovery`, `is_degraded_startup_unavailable_reason`
- `src/agent/runtime/tests/support.rs` -- `test_node`, `test_identity`, `MockModelEndpoint`, HTTP helpers, `bind_default_behavior_backend*`, `create_agent_request*`, wait utilities
- `src/agent/runtime/tests/behavior_resolution.rs` -- 3 `resolve_behavior_*` tests
- `src/agent/runtime/tests/router.rs` -- 2 `router_*` tests
- `src/agent/runtime/tests/control_watcher.rs` -- 3 `control_watcher_*` tests
- `src/agent/runtime/tests/startup_recovery.rs` -- 5 `run_agent_*` tests

Status: not started

### Phase A3: `src/document_config.rs` (1100 lines)

Split by document type, plus a serde/graphql helper layer.

- `src/document_config/mod.rs` -- shell + `PrincipalBootstrap`, `default_behavior_id_for_agent`, `ensure_agent_principal`
- `src/document_config/principal.rs` -- `AgentPrincipal`, `load_agent_principal`, `load_agent_principal_record`, `load_agent_principal_by_doc_id`, `upsert_agent_principal`
- `src/document_config/behavior.rs` -- `AgentBehavior`, `load_agent_behavior`, `load_agent_behavior_record`, `load_agent_behavior_by_doc_id`, `list_agent_behaviors`, `list_agent_behavior_records`, `upsert_agent_behavior`, `create_default_behavior`
- `src/document_config/inference_profile.rs` -- `InferenceProfile`, `load_inference_profile`, `load_inference_profile_record`, `load_inference_profile_by_doc_id`, `list_inference_profile_records`, `upsert_inference_profile`
- `src/document_config/tool_selection.rs` -- `ToolSelectionDocument`, `load_tool_selection`, `load_tool_selection_record`, `load_tool_selection_by_doc_id`, `list_tool_selection_records`, `list_all_tool_selection_records`, `upsert_tool_selection`
- `src/document_config/serde.rs` -- `deserialize_optional_string_vec`, `first_row_with_doc_id`, `rows_with_doc_id`, `default_display_name_for_did`, `normalize_optional_string`
- `src/document_config/graphql_fields.rs` -- `graphql_string_field`, `graphql_nullable_string_field`, `graphql_optional_int_field`, `graphql_optional_float_field`, `graphql_optional_bool_field`, `graphql_string_list_field`, `graphql_bool`
- `src/document_config/tests/`

Status: not started

### Phase A4: `src/tool_surface.rs` (947 lines)

- `src/tool_surface/mod.rs` -- shell + `ToolSurface`, `cli_tool`
- `src/tool_surface/modes.rs` -- `FileToolMode`, `BashMode`, `ToolCeiling`
- `src/tool_surface/selection.rs` -- `ToolSelection`, `CustomToolFactory`
- `src/tool_surface/behavior_config.rs` -- `BehaviorToolConfig`
- `src/tool_surface/runtime_context.rs` -- `ToolRuntimeContext`
- `src/tool_surface/build.rs` -- `build_host_tools`, `downgrade_file_tools`, `downgrade_bash`, `resolve_effective_tool_root`, `resolve_configured_tool_root`, `resolve_path_with_canonical_prefix`, `dedupe_strings`, `has_registered_mcp_services`
- `src/tool_surface/tests/`

Status: not started

### Phase A5: `src/scheduler/tests.rs` (855 lines)

- `src/scheduler/tests/support.rs` -- `test_admission_registry`, HTTP helpers, `insert_backend*`, `insert_due_task`, `query_task_row`, `delete_task`
- `src/scheduler/tests/unit.rs` -- 11 sync tests on parse / from-value / is_due / disabled / timeout / missing-collection
- `src/scheduler/tests/execution.rs` -- 4 async integration-style tests (`scheduled_execution_*`, `stale_runtime_bookkeeping_is_skipped_after_task_delete`, `scheduler_tick_shutdown_is_prompt_while_task_waits_for_backend_capacity`)

Status: not started

### Phase A6: `src/agent/tests.rs` (609 lines)

- `src/agent/tests/support.rs` -- `test_node`, `test_identity`, `insert_inference_profile`, `insert_backend*`, `update_default_behavior`
- `src/agent/tests/document_loading.rs` -- 4 `from_default_behavior_documents_*` tests
- `src/agent/tests/builder.rs` -- 2 `builder_*` tests
- `src/agent/tests/supervision.rs` -- 1 `supervision_*` test

Status: not started

### Phase A7: `tests/lifecycle_regression.rs` + `tests/backend_auth.rs` (608 + 580 lines)

Top-level integration tests compile as separate crates, so split into multiple files sharing the existing `tests/support/` layer (currently 277 lines).

Shared support expansion:

- `tests/support/http_mock.rs` -- mock HTTP server pattern (currently duplicated in `agent/runtime/tests.rs`, `scheduler/tests.rs`, `backend_auth.rs`): `read_http_request`, `find_subslice`, `write_http_response`, `HttpRequestData`, `MockModelEndpoint`-style helpers
- `tests/support/fixtures.rs` -- `test_identity`, `test_behavior`, backend / behavior insert helpers
- `tests/support/waits.rs` -- `wait_for_runtime_process_state`, `wait_for_request_state`, `wait_for_chat_request_count`, `wait_for_inference_call_state`

Lifecycle regression split:

- `tests/lifecycle_claim.rs` -- `pending_request_hydrates_sampling_fields_and_metadata`, 3 `claim_*` tests
- `tests/lifecycle_recovery.rs` -- 4 `recover_all_*` tests
- `tests/lifecycle_terminal.rs` -- `complete_does_not_overwrite_conversation_for_newer_request`, `advance_increments_progress_seq`

Backend auth split:

- `tests/backend_auth_config.rs` -- 3 sync `behavior_config_*` tests
- `tests/backend_auth_startup.rs` -- `run_agent_uses_backend_specific_api_key_env_var_for_startup_probe`, `openrouter_oneshot_uses_provider_request_preferences`
- `tests/backend_auth_live.rs` -- `live_openrouter_oneshot_succeeds` (unchanged gating; isolated file)

Status: not started

### Phase A8: `src/agent/document_view.rs` (600 lines)

Over the 500 warning threshold; split along existing seams.

- `src/agent/document_view/mod.rs` -- shell + `DocumentRecord`, `DocumentRuntimeView`, `ControlUpdateOutcome`, view impls that belong with the types
- `src/agent/document_view/load.rs` -- `load_document_runtime_view`, `hydrate_referenced_tool_selections`, `find_tool_selection_by_scan`
- `src/agent/document_view/apply.rs` -- `apply_control_update`
- `src/agent/document_view/snapshot.rs` -- `resolve_document_runtime_snapshot_from_view`, `collect_unresolved_behavior_references`, `behavior_references_ready`, `non_empty`
- `src/agent/document_view/tests.rs` -- already at 219 lines; keep single-file unless it grows

Status: not started

### Phase A9: borderline sweep + test-support polish (`defra-agent`)

Targeted review of files 400-534 lines. Split only when a natural seam exists; otherwise document cohesion and apply the soft-cap fallback. Candidate files:

- `src/agent/reconcile.rs` (534)
- `src/lifecycle/transition.rs` (503)
- `src/agent/builder.rs` (471)
- `src/toolset/file_tools.rs` (463)
- `src/session/tests.rs` (459)
- `src/agent/reconcile/tests.rs` (444)
- `src/streaming.rs` (438)
- `src/compaction/tests.rs` (418)
- `src/toolset/tests.rs` (402)

Also:

- Verify every `mod.rs` in the crate is a shell only.
- Delete any remaining inline `#[cfg(test)] mod tests` blocks.
- Final pass on public `///` doc comments per the comment cleanup policy.

Status: not started

### Phase B1: `defra-agent-cli/src/main.rs` HTTP / Prometheus extraction (~445 lines)

Zero subcommand touching; pure self-contained move.

- `src/http/mod.rs` -- shell + `RuntimeHttpState`
- `src/http/router.rs` -- `runtime_contract_router`, `metrics_handler`, `version_handler`, `healthz_handler`
- `src/http/version.rs` -- `VersionResponse`, `BuildMetadata`, `version_response`, `NodeIdentityResponse`, `P2pShareableAddressResponse`
- `src/http/healthz.rs` -- `render_healthz_payload`
- `src/http/prometheus.rs` -- `render_prometheus_metrics`, `load_metrics_query_data`, `push_metric_prelude`, `push_metric_sample`, `format_metric_labels`, `escape_prometheus_label`, `rfc3339_timestamp_seconds`, `MetricsQueryData`, `MetricsRuntimeRow`, `MetricsBackendRow`

Status: not started

### Phase B2: `defra-agent-cli/src/main.rs` document-writer extraction (~520 lines)

- `src/config_writes/mod.rs` -- shell + `ConfigAccess`, `ExistingDocumentRef`
- `src/config_writes/inference_backend.rs` -- `InferenceBackendUpsertDocument`, `write_inference_backend_document`
- `src/config_writes/agent_behavior.rs` -- `write_agent_behavior_document`
- `src/config_writes/tool_selection.rs` -- `write_tool_selection_document`, `tool_selection_fields`
- `src/config_writes/scheduled_task.rs` -- `write_scheduled_task_document`, `create_scheduled_task_document`, `select_matching_scheduled_task_row`, `query_scheduled_task_rows`, `scheduled_task_row_matches_expected`
- `src/config_writes/common.rs` -- `query_documents_by_unique_value`, `select_existing_document`, `extract_mutation_doc_id`

Status: not started

### Phase B3: CLI type + command scaffold

Pure type-move PR; no logic change.

- `src/cli/mod.rs` + `src/cli/args.rs` -- all clap structs and enums: `Cli`, `Command`, `InitArgs`, `ResetArgs`, `ServeArgs`, `ChatArgs`, `TuiArgs`, `ShowCommand`, `StatusArgs`, `DiagnoseArgs`, `RuntimeShowArgs`, `ConfigCommand`, `BackendCommand`, `BehaviorCommand`, `ToolSelectionCommand`, `BehaviorUpsertArgs`, `ToolSelectionUpsertArgs`, `InferenceProfileCommand`, `ScheduledTaskCommand`, `InferenceProfileUpsertArgs`, `ScheduledTaskSetArgs`, `BackendUpsertArgs`, `BackendDiscoverModelsArgs`, `ConfigExportArgs`, `ConfigImportArgs`, `ConfigValidateArgs`, `ConfigDiffArgs`, `ConfigApplyArgs`, `P2pCommand`, `P2pAccessArgs`, `P2pConnectArgs`, `P2pCollectionsCommand`, `P2pReplicatorsCommand`, `P2pDocumentsCommand`, `P2pCollectionsMutateArgs`, `P2pSyncBranchableArgs`, `P2pSyncVersionsArgs`, `P2pReplicatorAddArgs`, `P2pReplicatorRemoveArgs`, `P2pDocumentsMutateArgs`, `P2pDocumentsSyncArgs`, `RequestCommand`, `RequestSubmitArgs`, `RequestShowArgs`, `ResponseCommand`, `ResponseShowArgs`, `ResponseWaitArgs`, value-enums (`ChatOutputFormat`, `P2pTransportArg`, `P2pRelayModeArg`, `P2pDiscoveryArg`, `BackendPresetArg`, `ToolCeilingArg`, `P2pCollectionProfileArg`) and their impls.
- `src/commands/mod.rs` -- empty dispatcher shell.
- `src/shared.rs` -- cross-command response DTOs: `ResolvedBackendConfig`, `DiscoveredBackendTarget`, `InitSummary`, `StoredInitConfig`, `StoredRuntimeState`, `P2pPeerRow`, `P2pCollectionSubscriptionRow`, `P2pReplicatorRow`, `P2pReplicatorOutputRow`, `P2pReplicatorRequest`, `P2pReplicatorDeleteRequest`, `P2pSyncDocumentsRequest`, `P2pSyncBranchableRequest`, `P2pSyncVersionsRequest`, `ConfigExportBundle`, `ConfigApplyCounts`, `ConfigApplyReport`.

Status: not started

### Phase B4: `init` + `reset` + `serve` handlers

- `src/commands/init.rs` -- `init`, `initialize_runtime_home`, `standard_tool_selection`, `standard_system_prompt`, `init_next_steps`, `is_probably_ollama_endpoint`, `resolve_init_backend_config`, `resolve_default_tool_root`
- `src/commands/reset.rs` -- `reset`
- `src/commands/serve.rs` -- `serve`, `default_p2p_transport`, `default_p2p_secret_key_path`, `CliReadyObserver`

Status: not started

### Phase B5: `config` subcommand family

- `src/commands/config/mod.rs` -- `ConfigCommand` dispatch
- `src/commands/config/backend.rs` -- `backend_set`, `backend_discover_models`, `resolve_backend_discovery_target`, `load_backend_row`, `resolve_backend_upsert_config`, `resolve_backend_config_with_preset`, `resolve_backend_endpoint`, `resolve_backend_provider_kind`, `resolve_backend_api_key_env_var`, `resolve_required_env_api_key`
- `src/commands/config/behavior.rs` -- `behavior_set`
- `src/commands/config/tools.rs` -- `tool_selection_set`
- `src/commands/config/profile.rs` -- `inference_profile_set`
- `src/commands/config/task.rs` -- `scheduled_task_set`
- `src/commands/config/export.rs` -- `config_export`, `build_config_export_bundle`, `build_desired_state_live_bundle`, `live_manifest_from_bundle`, `sort_document_rows`, `normalize_tool_service_registry_export_rows`, `collect_string_field_values`, `graphql_string_list_literal`
- `src/commands/config/import.rs` -- `config_import`, `read_config_import_bundle`, `validate_config_import_bundle`, `migrate_config_import_bundle`, `apply_import_collection`, `sanitize_import_document`
- `src/commands/config/validate.rs` -- `config_validate`, `load_desired_manifest_or_bail`
- `src/commands/config/diff.rs` -- `config_diff`, `diff_has_pending_apply`
- `src/commands/config/apply.rs` -- `config_apply`, `config_apply_counts_changed`, `select_apply_collection_docs`, `select_apply_principal_docs`, `apply_desired_state_changes`, `graphql_input_literal`

Status: not started

### Phase B6: `p2p` subcommand family

- `src/commands/p2p/mod.rs` -- `P2pCommand` dispatch + `peer_id_from_public_addr`
- `src/commands/p2p/collections.rs` -- collection subscribe / unsubscribe and profile handling
- `src/commands/p2p/replicators.rs` -- add / remove / list replicators
- `src/commands/p2p/documents.rs` -- mutate + sync document commands
- `src/commands/p2p/access.rs` -- access commands
- `src/commands/p2p/connect.rs` -- `connect` + sync (branchable, versions)
- `src/commands/p2p/output.rs` -- `P2pReplicatorOutputRow` and any shared output formatting

Status: not started

### Phase B7: remaining handlers (`chat`, `status`, `diagnose`, `show`, `request`, `response`)

- `src/commands/chat.rs` -- `chat`
- `src/commands/status.rs` -- `status`, `load_runtime_status_output`, `load_live_unavailable_behaviors`, `collect_unavailable_behaviors_from_bundle`, `string_field`, `bool_field`, `expand_nonempty_values`, `load_collection_name_by_id`, `collection_version_string_field`
- `src/commands/diagnose.rs` -- `diagnose`, `diagnose_schema_presence`, `diagnose_tool_ceiling`, `diagnose_backends`, `diagnose_backend`
- `src/commands/show.rs` -- `show_runtime` and `ShowCommand` dispatch
- `src/commands/request.rs` -- `request_submit`, `request_show`
- `src/commands/response.rs` -- `response_show`, `response_wait`

Status: not started

### Phase B8: `main.rs` finalization

Target ~200-300 lines: constants, logging setup, the top-level `main()` dispatcher calling into `commands::*`, and remaining shared helpers.

- Move shared GraphQL helpers -- `resolve_config_access`, `graphql_endpoint_available`, `load_runtime_row`, `graphql_rows`, `graphql_rows_or_empty_if_collection_missing`, `is_collection_missing_error`, `post_graphql`, `normalize_optional_string` -- into `src/graphql_access.rs` if necessary to hit the line target.
- Final `//` comment sweep across newly split files per the comment cleanup policy.

Status: not started

### Phase B9: `src/desired_state.rs` (1519 lines)

- `src/desired_state/mod.rs` -- shell + type defs (`DesiredAgentPrincipal`, `DesiredAgentBehavior`, `DesiredToolSelection`, `DesiredInferenceBackend`, `DesiredInferenceProfile`, `DesiredToolServiceRegistry`, `DesiredScheduledTask`, `DesiredStateManifest`, `DesiredStateCollectionDiff`, `DesiredStateDiffCounts`, `DesiredStateDiffCollections`, `DesiredStateDiffCollectionsCounts`, `DesiredStateDiffReport`, `DesiredStateCounts`, `DesiredStateValidationReport`)
- `src/desired_state/load.rs` -- `validate_manifest_root`, `load_manifest_root`, `load_required_json`, `load_optional_json`, `load_optional_json_collection`, `load_json_file`, `load_json_collection`
- `src/desired_state/validate.rs` -- `validate_manifest`, `non_empty`, `normalize_tool_service_string`, `normalize_tool_service_mcp_path`, `optional_string_from_value`, `optional_i64_from_value`
- `src/desired_state/convert.rs` -- `manifest_from_export_bundle`, `export_bundle_from_manifest`, `tool_service_registry_from_live_value`, `normalize_tool_service_registry_storage_fields`, `desired_from_value`
- `src/desired_state/diff.rs` -- `diff_manifests`, `diff_single`, `diff_collection`
- `src/desired_state/normalize.rs` -- `normalize_manifest`, `strip_deprecated_inference_backend_fields`, `default_max_queue_depth`
- `src/desired_state/tests/`

Status: not started

### Phase B10: `src/tui.rs` (761 lines)

- `src/tui/mod.rs` -- shell + `run`, `App`, `FocusPane`, `TerminalGuard`
- `src/tui/snapshot.rs` -- `Snapshot`, `MessageRow`, `ToolRow`, `ResponseRow`, `RuntimeRow`, `load_snapshot`, `parse_rows`
- `src/tui/render.rs` -- `draw_ui`, `pane_block`, `scroll_offset`, `render_transcript`, `render_tools`, `render_reasoning_history`, `render_runtime`, `decode_message`, `render_transcript_message_body`, `extract_message_reasoning`, `transcript_label`, `truncate_tail`, `truncate_middle`, `format_tool_args_preview`
- `src/tui/tests.rs` stays

Status: not started

### Phase B11: `tests/cli_e2e.rs` (6788 lines)

Top-level integration-test files compile as separate crates, so split into many files sharing a support dir.

Shared support expansion (existing `crates/defra-agent-cli/tests/support/` pattern if present, else create):

- `cli_bin`, `desktop_bin`, `run_desktop_init_json`, `allocate_port`, `graphql_url`, `run_init_json`, `spawn_server`, `spawn_server_with_ready_json`, `spawn_server_with_env`, `read_runtime_state_json`, `assert_runtime_init_state`, `wait_for_port`, `spawn_cli`, `run_cli_json`, `run_cli_text`, `run_cli_failure_stderr`, `run_cli_failure_stdout_json`, `write_json_file`, `read_json_file`, `write_manifest_root_from_export`, `project_array_fields`, `project_object_fields`, `wait_for_request`, `insert_terminal_response`, `graphql_query`, `first_graphql_row`, `wait_for_runtime_ready`, `doc_id_for_selection`, `read_captured_log`

Per-subcommand test files (each its own top-level integration test crate):

- `tests/cli_init.rs` -- ~10 init tests (including `init_accepts_tool_root_for_readonly_defaults`)
- `tests/cli_help.rs` -- `top_level_help_shows_quickstart_workflow`, `status_without_runtime_suggests_init_and_server`
- `tests/cli_config_validate.rs` -- 3 validate tests
- `tests/cli_config_diff.rs` -- 2 diff tests
- `tests/cli_config_apply.rs` -- apply scenarios (running runtime, fresh local, fresh graphql, end-to-end reconcile)
- `tests/cli_config_export_import.rs` -- round-trip tests (2)
- `tests/cli_config_backend.rs` -- backend upsert + discover tests
- `tests/cli_config_tools.rs` -- tool selection tests (defaults + file root)
- `tests/cli_config_tasks.rs` -- `scheduled_task_set_*` tests
- `tests/cli_chat.rs` -- 4 chat tests
- `tests/cli_request.rs` -- request submit (waits, content/output files)
- `tests/cli_server.rs` -- server startup + metrics + degraded-mode + iroh p2p startup tests
- `tests/cli_status.rs` -- status-reads-local + status-includes-p2p
- `tests/cli_diagnose.rs` -- diagnose tests
- `tests/cli_reconciliation.rs` -- `reconciled_runtime_sends_generation_two_tools_and_completes_tool_loop`
- `tests/cli_p2p.rs` -- `p2p_connects_two_local_servers_via_operator_commands`
- `tests/cli_live.rs` -- live-network tests (`standard_onboarding_live_demo_runs_real_conversation_with_filesystem_tools`, `cli_flow_runs_real_tool_loop_against_live_endpoint`)

If any resulting file lands above 500 lines, apply the soft-cap fallback and document in the PR.

Status: not started
