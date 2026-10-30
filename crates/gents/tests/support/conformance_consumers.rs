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

pub fn registered_conformance_consumers() -> &'static [ConformanceConsumer] {
    &[
        ConformanceConsumer::RustTest {
            id: "session_message::agent_tools_tests::explicit_steer_checks_the_physical_head_at_publication",
            package: "gents",
            source_path: "crates/gents/src/session/message/agent_tools_tests.rs",
            module_path: "session_message::agent_tools_tests",
            function: "explicit_steer_checks_the_physical_head_at_publication",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::intake::tests::boundary_selection_matches_lean",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/intake.rs",
            module_path: "lifecycle::queue::intake::tests",
            function: "boundary_selection_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "session_message::agent_tools_tests::steer_reachability_matches_lean",
            package: "gents",
            source_path: "crates/gents/src/session/message/agent_tools_tests.rs",
            module_path: "session_message::agent_tools_tests",
            function: "steer_reachability_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "session_message::delivery_conformance::agent_delivery_matches_lean",
            package: "gents",
            source_path: "crates/gents/src/session/message/delivery_conformance.rs",
            module_path: "session_message::delivery_conformance",
            function: "agent_delivery_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "session_message::agent_tools_tests::interrupt_delivery_latches_only_the_prepublication_head",
            package: "gents",
            source_path: "crates/gents/src/session/message/agent_tools_tests.rs",
            module_path: "session_message::agent_tools_tests",
            function: "interrupt_delivery_latches_only_the_prepublication_head",
        },
        ConformanceConsumer::RustTest {
            id: "background_completion::session_message::turn_assignment_tests::folded_session_message_waits_for_exact_head_and_replays_once",
            package: "gents",
            source_path: "crates/gents/src/background_completion/session_message.rs",
            module_path: "background_completion::session_message::turn_assignment_tests",
            function: "folded_session_message_waits_for_exact_head_and_replays_once",
        },
        ConformanceConsumer::RustTest {
            id: "background_completion::session_message::folded_outcome_conformance::folded_outcome_matches_lean",
            package: "gents",
            source_path: "crates/gents/src/background_completion/session_message.rs",
            module_path: "background_completion::session_message::folded_outcome_conformance",
            function: "folded_outcome_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "config_client::txn::owner_tests::tracked_transaction_finality_distinguishes_rejection_from_lost_receipt",
            package: "gents",
            source_path: "crates/gents/src/config_client/txn_owner_tests.rs",
            module_path: "config_client::txn::owner_tests",
            function: "tracked_transaction_finality_distinguishes_rejection_from_lost_receipt",
        },
        ConformanceConsumer::RustTest {
            id: "inference::backends::registry::tests::generated_catalog_refreshes_preserve_payload_commits_and_latest_freshness",
            package: "gents",
            source_path: "crates/gents/src/inference/backends/registry/tests.rs",
            module_path: "inference::backends::registry::tests",
            function: "generated_catalog_refreshes_preserve_payload_commits_and_latest_freshness",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::session_hydration_reconcile::selection::tests::generated_terminal_selections_preserve_superseded_session_hydration",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/selection/tests.rs",
            module_path: "agent::p2p_reconcile::session_hydration_reconcile::selection::tests",
            function: "generated_terminal_selections_preserve_superseded_session_hydration",
        },
        ConformanceConsumer::RustTest {
            id: "gents_loop::claude_messages_body::output_tests::checkpoint_wire_claude_output_matches_executed_model",
            package: "gents-loop",
            source_path: "crates/gents-loop/src/claude_messages_output_tests.rs",
            module_path: "gents_loop::claude_messages_body::output_tests",
            function: "checkpoint_wire_claude_output_matches_executed_model",
        },
        ConformanceConsumer::RustTest {
            id: "gents_loop::compaction::summary::tests::checkpoint_wire_fallback_matches_executed_model",
            package: "gents-loop",
            source_path: "crates/gents-loop/src/compaction_summary_tests.rs",
            module_path: "gents_loop::compaction::summary::tests",
            function: "checkpoint_wire_fallback_matches_executed_model",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::shared::command::network_enforcement_disclosure_tests::generated_command_isolation_matches_native_policy",
            package: "gents",
            source_path: "crates/gents/src/toolset/shared/command_network_disclosure_tests.rs",
            module_path: "toolset::shared::command::network_enforcement_disclosure_tests",
            function: "generated_command_isolation_matches_native_policy",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::shared::command::command_credentials::tests::generated_bounded_sandboxes_deny_credentials_and_default_network",
            package: "gents",
            source_path: "crates/gents/src/toolset/shared/command_credentials_tests.rs",
            module_path: "toolset::shared::command::command_credentials::tests",
            function: "generated_bounded_sandboxes_deny_credentials_and_default_network",
        },
        ConformanceConsumer::RustTest {
            id: "provider_input::tool_call_identity_tests::modeled_tool_identity_projection_is_idempotent_and_preserves_other_fields",
            package: "gents-loop",
            source_path: "crates/gents-loop/src/provider_input/tool_call_identity_tests.rs",
            module_path: "provider_input::tool_call_identity_tests",
            function: "modeled_tool_identity_projection_is_idempotent_and_preserves_other_fields",
        },
        ConformanceConsumer::RustTest {
            id: "mailbox_question::question_policy_follows_the_executable_model",
            package: "gents-protocol",
            source_path: "crates/gents-protocol/tests/mailbox_question.rs",
            module_path: "mailbox_question",
            function: "question_policy_follows_the_executable_model",
        },
        ConformanceConsumer::RustTest {
            id: "session::preview::tests::terminal_reply_projection_follows_the_model",
            package: "gents",
            source_path: "crates/gents/src/session/preview.rs",
            module_path: "session::preview::tests",
            function: "terminal_reply_projection_follows_the_model",
        },
        ConformanceConsumer::RustTest {
            id: "commands::init::inference_seed_tests::generated_initial_inference_keeps_unselected_models_and_limits_unpinned",
            package: "gents-cli",
            source_path: "crates/gents-cli/src/commands/init/inference_seed_tests.rs",
            module_path: "commands::init::inference_seed_tests",
            function: "generated_initial_inference_keeps_unselected_models_and_limits_unpinned",
        },
        ConformanceConsumer::RustTest {
            id: "client::mutations::chat::request::receipt::goal_delivery::tests::generated_goal_delivery_histories_bind_native_signed_preparations",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/mutations/chat/request/receipt/goal_delivery/tests.rs",
            module_path: "client::mutations::chat::request::receipt::goal_delivery::tests",
            function: "generated_goal_delivery_histories_bind_native_signed_preparations",
        },
        ConformanceConsumer::RustTest {
            id: "plugin_job::control::tests::kill_reads_children_that_committed_after_the_recovery_snapshot",
            package: "gents",
            source_path: "crates/gents/src/plugin_job/control.rs",
            module_path: "plugin_job::control::tests",
            function: "kill_reads_children_that_committed_after_the_recovery_snapshot",
        },
        ConformanceConsumer::RustTest {
            id: "plugin_job::operator_control_conformance::generated_operator_controls_stay_on_the_owning_runtime",
            package: "gents",
            source_path: "crates/gents/src/plugin_job/operator_control_conformance.rs",
            module_path: "plugin_job::operator_control_conformance",
            function: "generated_operator_controls_stay_on_the_owning_runtime",
        },
        ConformanceConsumer::RustTest {
            id: "tool_control::tests::operator_process_control_matches_executable_model",
            package: "gents",
            source_path: "crates/gents/src/tool_control.rs",
            module_path: "tool_control::tests",
            function: "operator_process_control_matches_executable_model",
        },
        ConformanceConsumer::RustTest {
            id: "tool_control::tests::generated_stop_actions_select_the_durable_transition",
            package: "gents",
            source_path: "crates/gents/src/tool_control.rs",
            module_path: "tool_control::tests",
            function: "generated_stop_actions_select_the_durable_transition",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::generated_background_process_identity_cases_cancel_only_the_physical_invocation",
            package: "gents",
            source_path: "crates/gents/src/hook/tests.rs",
            module_path: "hook::tests",
            function: "generated_background_process_identity_cases_cancel_only_the_physical_invocation",
        },
        ConformanceConsumer::RustTest {
            id: "hook::persistence::background_control::observation::tests::generated_observation_cases_preserve_deadlines_and_closed_feed_recovery",
            package: "gents",
            source_path: "crates/gents/src/hook/persistence/background_control/observation_tests.rs",
            module_path: "hook::persistence::background_control::observation::tests",
            function: "generated_observation_cases_preserve_deadlines_and_closed_feed_recovery",
        },
        ConformanceConsumer::RustTest {
            id: "plugin_job::admit::tests::generated_wire_targets_preserve_endpoint_and_permission_identity",
            package: "gents",
            source_path: "crates/gents/src/plugin_job/admit.rs",
            module_path: "plugin_job::admit::tests",
            function: "generated_wire_targets_preserve_endpoint_and_permission_identity",
        },
        ConformanceConsumer::RustTest {
            id: "goal::request_assignment::scope_tests::generated_ancestry_scope_cases_use_the_lean_composition",
            package: "gents",
            source_path: "crates/gents/src/goal/request_assignment/scope_tests.rs",
            module_path: "goal::request_assignment::scope_tests",
            function: "generated_ancestry_scope_cases_use_the_lean_composition",
        },
        ConformanceConsumer::RustTest {
            id: "gents_protocol::database_error::tests::generated_storage_failures_preserve_recovery_evidence",
            package: "gents-protocol",
            source_path: "crates/gents-protocol/src/database_error.rs",
            module_path: "gents_protocol::database_error::tests",
            function: "generated_storage_failures_preserve_recovery_evidence",
        },
        ConformanceConsumer::RustTest {
            id: "client_installation::tests::generated_installation_traces_preserve_draining_and_replacement_guards",
            package: "gents-protocol",
            source_path: "crates/gents-protocol/src/client_installation.rs",
            module_path: "client_installation::tests",
            function: "generated_installation_traces_preserve_draining_and_replacement_guards",
        },
        ConformanceConsumer::RustTest {
            id: "client_installation::compatibility_tests::generated_installation_compatibility_matches_lean",
            package: "gents-protocol",
            source_path: "crates/gents-protocol/src/client_installation.rs",
            module_path: "client_installation::compatibility_tests",
            function: "generated_installation_compatibility_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "serve_lifecycle::replacement::tests::generated_replacement_traces_match_authenticated_admission_and_release_guards",
            package: "gents-protocol",
            source_path: "crates/gents-protocol/src/serve_lifecycle/replacement.rs",
            module_path: "serve_lifecycle::replacement::tests",
            function: "generated_replacement_traces_match_authenticated_admission_and_release_guards",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::session_hydration_reconcile::inventory::tests::generated_request_coverage_uses_only_selected_physical_scope",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/inventory/tests.rs",
            module_path: "agent::p2p_reconcile::session_hydration_reconcile::inventory::tests",
            function: "generated_request_coverage_uses_only_selected_physical_scope",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::session_hydration_reconcile::inventory_order::tests::generated_orders_use_canonical_publication_observations",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/inventory_order/tests.rs",
            module_path: "agent::p2p_reconcile::session_hydration_reconcile::inventory_order::tests",
            function: "generated_orders_use_canonical_publication_observations",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::session_hydration_reconcile::inventory::tests::generated_fork_inventory_retains_only_selected_native_origins",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/inventory/tests.rs",
            module_path: "agent::p2p_reconcile::session_hydration_reconcile::inventory::tests",
            function: "generated_fork_inventory_retains_only_selected_native_origins",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::file_image_hydration::tests::inventory::generated_inventory_images_keep_reader_root_snapshot_and_source_distinct",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/file_image_hydration/tests/inventory.rs",
            module_path: "agent::p2p_reconcile::file_image_hydration::tests::inventory",
            function: "generated_inventory_images_keep_reader_root_snapshot_and_source_distinct",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::session_hydration_reconcile::inventory::tests::readers::shared_reader_metadata_preserves_canonical_authorship_and_image_privacy",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/inventory/tests/readers.rs",
            module_path: "agent::p2p_reconcile::session_hydration_reconcile::inventory::tests::readers",
            function: "shared_reader_metadata_preserves_canonical_authorship_and_image_privacy",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::session_hydration_reconcile::inventory::tests::generated_projection_cases_use_native_owners_and_scopes",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/inventory/tests.rs",
            module_path: "agent::p2p_reconcile::session_hydration_reconcile::inventory::tests",
            function: "generated_projection_cases_use_native_owners_and_scopes",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_refusals_verify_exact_demand",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_refusals_verify_exact_demand",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_reply_journals_preserve_immutable_outcomes",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_reply_journals_preserve_immutable_outcomes",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_operator_inventory_requires_the_native_owner",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_operator_inventory_requires_the_native_owner",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_client_scope_requires_a_unique_retained_admission",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_client_scope_requires_a_unique_retained_admission",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_client_progress_survives_reload_and_rollback",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_client_progress_survives_reload_and_rollback",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_preparation_retries_keep_the_original_snapshot",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_preparation_retries_keep_the_original_snapshot",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_completion_requires_confirmed_authorized_delivery",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_completion_requires_confirmed_authorized_delivery",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_page_grants_bind_only_the_exact_metadata_document",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_page_grants_bind_only_the_exact_metadata_document",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_demands_preserve_attempt_and_cursor_identity",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_demands_preserve_attempt_and_cursor_identity",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_issuance_withholds_revoked_authority",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_issuance_withholds_revoked_authority",
        },
        ConformanceConsumer::RustTest {
            id: "file_inventory::tests::generated_receivers_follow_signed_inventory_prefixes",
            package: "gents",
            source_path: "crates/gents/src/file_inventory/tests.rs",
            module_path: "file_inventory::tests",
            function: "generated_receivers_follow_signed_inventory_prefixes",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::session_hydration_reconcile::tests::generated_hydration_routes_require_current_enrollment_materialization",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/admission_tests.rs",
            module_path: "agent::p2p_reconcile::session_hydration_reconcile::tests",
            function: "generated_hydration_routes_require_current_enrollment_materialization",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::enrollment_reconcile::tests::generated_client_observation_admission_matches_lean",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/enrollment_reconcile.rs",
            module_path: "agent::p2p_reconcile::enrollment_reconcile::tests",
            function: "generated_client_observation_admission_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::title_audit_usage::tests::generated_budget_recovery_cases_bind_signed_catalog_usage",
            package: "gents",
            source_path: "crates/gents/src/toolset/title_audit_usage.rs",
            module_path: "toolset::title_audit_usage::tests",
            function: "generated_budget_recovery_cases_bind_signed_catalog_usage",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::approval::command_grant_contract_tests::generated_command_answers_preserve_scope_and_one_time_delivery",
            package: "gents",
            source_path: "crates/gents/src/plugin/approval/command_grant_contract_tests.rs",
            module_path: "plugin::approval::command_grant_contract_tests",
            function: "generated_command_answers_preserve_scope_and_one_time_delivery",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::approval::command_grant_contract_tests::generated_command_revocation_preserves_the_selected_mode",
            package: "gents",
            source_path: "crates/gents/src/plugin/approval/command_grant_contract_tests.rs",
            module_path: "plugin::approval::command_grant_contract_tests",
            function: "generated_command_revocation_preserves_the_selected_mode",
        },
        ConformanceConsumer::RustTest {
            id: "tool_policy::workspace_permissions::tests::generated_command_grant_lookups",
            package: "gents-loop",
            source_path: "crates/gents-loop/src/tool_policy/workspace_permissions.rs",
            module_path: "tool_policy::workspace_permissions::tests",
            function: "generated_command_grant_lookups",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::approval::workspace::tests::generated_workspace_permission_writes_preserve_selected_policy_versions",
            package: "gents",
            source_path: "crates/gents/src/plugin/approval/workspace.rs",
            module_path: "plugin::approval::workspace::tests",
            function: "generated_workspace_permission_writes_preserve_selected_policy_versions",
        },
        ConformanceConsumer::RustTest {
            id: "agent::daemon::title::review::task::tests::generated_task_policies_bind_the_configured_task_fields",
            package: "gents",
            source_path: "crates/gents/src/agent/daemon/title/review/task.rs",
            module_path: "agent::daemon::title::review::task::tests",
            function: "generated_task_policies_bind_the_configured_task_fields",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::approval::workspace::command::tests::generated_workspace_command_roots_preserve_the_tool_ceiling",
            package: "gents",
            source_path: "crates/gents/src/plugin/approval/workspace/command.rs",
            module_path: "plugin::approval::workspace::command::tests",
            function: "generated_workspace_command_roots_preserve_the_tool_ceiling",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::allowed::tests::generated_operator_path_cases_protect_names_and_file_aliases",
            package: "gents",
            source_path: "crates/gents/src/plugin/allowed_tests.rs",
            module_path: "plugin::allowed::tests",
            function: "generated_operator_path_cases_protect_names_and_file_aliases",
        },
        ConformanceConsumer::RustTest {
            id: "session::title_text::tests::generated_title_text_cases_bind_native_presentation",
            package: "gents",
            source_path: "crates/gents/src/session/title_text.rs",
            module_path: "session::title_text::tests",
            function: "generated_title_text_cases_bind_native_presentation",
        },
        ConformanceConsumer::RustTest {
            id: "client::mutations::manage::principal::editor::tests::config_editor_matches_executable_lean_contract",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/mutations/manage/principal/editor.rs",
            module_path: "client::mutations::manage::principal::editor::tests",
            function: "config_editor_matches_executable_lean_contract",
        },
        ConformanceConsumer::RustTest {
            id: "client::core::observation_scope::tests::generated_attachment_traces_keep_every_frontend_observable",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/core/observation_scope.rs",
            module_path: "client::core::observation_scope::tests",
            function: "generated_attachment_traces_keep_every_frontend_observable",
        },
        ConformanceConsumer::RustTest {
            id: "client::core::inference_observation::tests::generated_inference_observation_traces",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/core/inference_observation.rs",
            module_path: "client::core::inference_observation::tests",
            function: "generated_inference_observation_traces",
        },
        ConformanceConsumer::RustTest {
            id: "bridge::product_telemetry::tests::generated_telemetry_traces_match_native_consent_and_http",
            package: "gents-desktop-tauri",
            source_path: "apps/gents-desktop/src-tauri/src/bridge/product_telemetry.rs",
            module_path: "bridge::product_telemetry::tests",
            function: "generated_telemetry_traces_match_native_consent_and_http",
        },
        ConformanceConsumer::RustTest {
            id: "client::observe::projection_store::reload_tests::generated_notice_delivery_never_regresses_watchers",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/observe/projection_store/reload_tests.rs",
            module_path: "client::observe::projection_store::reload_tests",
            function: "generated_notice_delivery_never_regresses_watchers",
        },
        ConformanceConsumer::RustTest {
            id: "client::query::request_closure::tests::generated_request_closures_use_exact_database_dependencies",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/query/request_closure/tests.rs",
            module_path: "client::query::request_closure::tests",
            function: "generated_request_closures_use_exact_database_dependencies",
        },
        ConformanceConsumer::RustTest {
            id: "client::core::writes::pending_queue::tests::generated_accepted_receipt_survives_projection_refresh_failure",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/core/writes/pending_queue/tests.rs",
            module_path: "client::core::writes::pending_queue::tests",
            function: "generated_accepted_receipt_survives_projection_refresh_failure",
        },
        ConformanceConsumer::RustTest {
            id: "client::store::merges::overlay_tests::generated_request_identity_cases_preserve_physical_twins_and_fold_owners",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/store/merges.rs",
            module_path: "client::store::merges::overlay_tests",
            function: "generated_request_identity_cases_preserve_physical_twins_and_fold_owners",
        },
        ConformanceConsumer::RustTest {
            id: "client::core::writes::request_patch::tests::generated_publication_cases_drive_request_patch_cache_admission",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/core/writes/request_patch/tests.rs",
            module_path: "client::core::writes::request_patch::tests",
            function: "generated_publication_cases_drive_request_patch_cache_admission",
        },
        ConformanceConsumer::RustTest {
            id: "e2e_runtime::session_input_edit::generated_remote_edits_replicate_signed_effect_and_receipt",
            package: "gents",
            source_path: "crates/gents/tests/e2e_runtime/session_input_edit.rs",
            module_path: "e2e_runtime::session_input_edit",
            function: "generated_remote_edits_replicate_signed_effect_and_receipt",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::tests::management::generated_pending_management_cases_bind_to_signed_replacement_owner",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/tests/management.rs",
            module_path: "lifecycle::queue::tests::management",
            function: "generated_pending_management_cases_bind_to_signed_replacement_owner",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::tests::folding::expired_or_malformed_steering_is_a_modelled_publication_barrier",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/tests/folding.rs",
            module_path: "lifecycle::queue::tests::folding",
            function: "expired_or_malformed_steering_is_a_modelled_publication_barrier",
        },
        ConformanceConsumer::RustTest {
            id: "gents_loop::end_to_end_test::modeled_unsafe_stream_boundaries_never_intake_or_finish_before_provider_publication",
            package: "gents-loop",
            source_path: "crates/gents-loop/src/end_to_end_test.rs",
            module_path: "gents_loop::end_to_end_test",
            function: "modeled_unsafe_stream_boundaries_never_intake_or_finish_before_provider_publication",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::session_input_edit::tests::generated_commands_bind_to_atomic_queue_owner",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/session_input_edit/tests.rs",
            module_path: "agent::p2p_reconcile::session_input_edit::tests",
            function: "generated_commands_bind_to_atomic_queue_owner",
        },
        ConformanceConsumer::RustTest {
            id: "lean_vocab_test::support::session_documents::tests::generated_pending_timeline_cases_preserve_request_identities_and_order",
            package: "gents",
            source_path: "crates/gents/src/lean_vocab_test/session_documents.rs",
            module_path: "lean_vocab_test::support::session_documents::tests",
            function: "generated_pending_timeline_cases_preserve_request_identities_and_order",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::tests::durable_contract::generated_fire_transactions_are_atomic_and_owner_scoped",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/tests/durable_contract.rs",
            module_path: "triggers::engine::tests::durable_contract",
            function: "generated_fire_transactions_are_atomic_and_owner_scoped",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::tests::durable_contract::durable_delivery_predicates_match_executable_lean_owners",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/tests/durable_contract.rs",
            module_path: "triggers::engine::tests::durable_contract",
            function: "durable_delivery_predicates_match_executable_lean_owners",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::tests::durable_contract::generated_latest_delivery_admission_preserves_rejected_and_replayed_work",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/tests/durable_contract.rs",
            module_path: "triggers::engine::tests::durable_contract",
            function: "generated_latest_delivery_admission_preserves_rejected_and_replayed_work",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::tests::durable_contract::generated_arrival_checkpoints_preserve_committed_delivery_across_crashes",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/tests/durable_contract.rs",
            module_path: "triggers::engine::tests::durable_contract",
            function: "generated_arrival_checkpoints_preserve_committed_delivery_across_crashes",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::tests::durable_contract::generated_callback_arrival_checkpoints_use_invocation_receipts",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/tests/durable_contract.rs",
            module_path: "triggers::engine::tests::durable_contract",
            function: "generated_callback_arrival_checkpoints_use_invocation_receipts",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::tests::durable_contract::generated_terminal_outcome_action_traces_use_native_owners",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/tests/durable_contract.rs",
            module_path: "triggers::engine::tests::durable_contract",
            function: "generated_terminal_outcome_action_traces_use_native_owners",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_protected_replay_compaction_cases_bind_native_split_and_checkpoint",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_protected_replay_compaction_cases_bind_native_split_and_checkpoint",
        },
        ConformanceConsumer::RustTest {
            id: "agent::stream_processor::tests::generated_reasoning_delta_cases_coalesce_native_parts",
            package: "gents",
            source_path: "crates/gents/src/agent/stream_processor_tests.rs",
            module_path: "agent::stream_processor::tests",
            function: "generated_reasoning_delta_cases_coalesce_native_parts",
        },
        ConformanceConsumer::RustTest {
            id: "native_output_reconstruction::generated_reasoning_audit_cases_drive_exact_native_prefix",
            package: "gents-lean-contract",
            source_path: "crates/gents-lean-contract/tests/native_output_reconstruction.rs",
            module_path: "native_output_reconstruction",
            function: "generated_reasoning_audit_cases_drive_exact_native_prefix",
        },
        ConformanceConsumer::RustTest {
            id: "native_output_reconstruction::generated_auxiliary_cases_drive_native_audit_and_public_projection",
            package: "gents-lean-contract",
            source_path: "crates/gents-lean-contract/tests/native_output_reconstruction.rs",
            module_path: "native_output_reconstruction",
            function: "generated_auxiliary_cases_drive_native_audit_and_public_projection",
        },
        ConformanceConsumer::RustTest {
            id: "native_output_reconstruction::generated_reasoning_signature_cases_drive_native_header_validation",
            package: "gents-lean-contract",
            source_path: "crates/gents-lean-contract/tests/native_output_reconstruction.rs",
            module_path: "native_output_reconstruction",
            function: "generated_reasoning_signature_cases_drive_native_header_validation",
        },
        ConformanceConsumer::RustTest {
            id: "inference::clients::claude_messages::tests::generated_claude_wire_start_cases_drive_native_parser",
            package: "gents",
            source_path: "crates/gents/src/inference/clients/claude_messages/tests.rs",
            module_path: "inference::clients::claude_messages::tests",
            function: "generated_claude_wire_start_cases_drive_native_parser",
        },
        ConformanceConsumer::RustTest {
            id: "tool_policy::access_review::tests::generated_access_review_cases_drive_native_reply_and_routing",
            package: "gents-loop",
            source_path: "crates/gents-loop/src/tool_policy/access_review.rs",
            module_path: "tool_policy::access_review::tests",
            function: "generated_access_review_cases_drive_native_reply_and_routing",
        },
        ConformanceConsumer::RustTest {
            id: "session::tests::editing_a_canonical_user_message_forks_its_prefix_and_preserves_the_source",
            package: "gents",
            source_path: "crates/gents/src/session/tests.rs",
            module_path: "session::tests",
            function: "editing_a_canonical_user_message_forks_its_prefix_and_preserves_the_source",
        },
        ConformanceConsumer::RustTest {
            id: "session::fork::tests::generated_edit_anchors_select_exact_canonical_user_messages",
            package: "gents",
            source_path: "crates/gents/src/session/fork.rs",
            module_path: "session::fork::tests",
            function: "generated_edit_anchors_select_exact_canonical_user_messages",
        },
        ConformanceConsumer::RustTest {
            id: "session::fork::tests::generated_edit_fork_bindings_control_destination_reuse",
            package: "gents",
            source_path: "crates/gents/src/session/fork.rs",
            module_path: "session::fork::tests",
            function: "generated_edit_fork_bindings_control_destination_reuse",
        },
        ConformanceConsumer::RustTest {
            id: "completion_factory::tests::generated_service_tier_cases_drive_loop_config",
            package: "gents",
            source_path: "crates/gents/src/completion_factory/tests.rs",
            module_path: "completion_factory::tests",
            function: "generated_service_tier_cases_drive_loop_config",
        },
        ConformanceConsumer::RustTest {
            id: "tool_policy::workspace_permissions::tests::generated_workspace_permission_cases",
            package: "gents-loop",
            source_path: "crates/gents-loop/src/tool_policy/workspace_permissions.rs",
            module_path: "tool_policy::workspace_permissions::tests",
            function: "generated_workspace_permission_cases",
        },
        ConformanceConsumer::RustTest {
            id: "tool_policy::workspace_permissions::tests::generated_workspace_permission_write_cases",
            package: "gents-loop",
            source_path: "crates/gents-loop/src/tool_policy/workspace_permissions.rs",
            module_path: "tool_policy::workspace_permissions::tests",
            function: "generated_workspace_permission_write_cases",
        },
        ConformanceConsumer::RustTest {
            id: "completion_factory::tests::generated_responses_storage_cases_drive_loop_config",
            package: "gents",
            source_path: "crates/gents/src/completion_factory/tests.rs",
            module_path: "completion_factory::tests",
            function: "generated_responses_storage_cases_drive_loop_config",
        },
        ConformanceConsumer::RustTest {
            id: "completion_factory::tests::generated_responses_effort_cases_drive_loop_config",
            package: "gents",
            source_path: "crates/gents/src/completion_factory/tests.rs",
            module_path: "completion_factory::tests",
            function: "generated_responses_effort_cases_drive_loop_config",
        },
        ConformanceConsumer::RustTest {
            id: "streaming::auxiliary_tests::generated_auxiliary_cases_drive_non_claude_sink_audit_without_publication",
            package: "gents",
            source_path: "crates/gents/src/request/streaming/auxiliary_tests.rs",
            module_path: "streaming::auxiliary_tests",
            function: "generated_auxiliary_cases_drive_non_claude_sink_audit_without_publication",
        },
        ConformanceConsumer::RustTest {
            id: "streaming::auxiliary_tests::generated_auxiliary_cases_drive_owned_begin_close_and_publication_guards",
            package: "gents",
            source_path: "crates/gents/src/request/streaming/auxiliary_tests.rs",
            module_path: "streaming::auxiliary_tests",
            function: "generated_auxiliary_cases_drive_owned_begin_close_and_publication_guards",
        },
        ConformanceConsumer::RustTest {
            id: "gents_protocol::transcript::tests::generated_reasoning_visibility_matches_persisted_presentation",
            package: "gents-protocol",
            source_path: "crates/gents-protocol/src/transcript.rs",
            module_path: "gents_protocol::transcript::tests",
            function: "generated_reasoning_visibility_matches_persisted_presentation",
        },
        ConformanceConsumer::RustTest {
            id: "gents_loop::stream_processor::tests::generated_reasoning_visibility_matches_live_preview",
            package: "gents-loop",
            source_path: "crates/gents-loop/src/stream_processor.rs",
            module_path: "gents_loop::stream_processor::tests",
            function: "generated_reasoning_visibility_matches_live_preview",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::delivery::spawned_background_tests::generated_terminal_diagnostic_replay_cases_bind_native_replay",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/delivery/spawned_background_tests.rs",
            module_path: "tool_call_lifecycle::delivery::spawned_background_tests",
            function: "generated_terminal_diagnostic_replay_cases_bind_native_replay",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::delivery::spawned_background_tests::generated_terminal_diagnostic_cases_bind_native_presentation",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/delivery/spawned_background_tests.rs",
            module_path: "tool_call_lifecycle::delivery::spawned_background_tests",
            function: "generated_terminal_diagnostic_cases_bind_native_presentation",
        },
        ConformanceConsumer::RustTest {
            id: "lean_vocab_test::support::canonical_presentation::native_adapter::generated_payload_presentation_cases_use_native_reconstruction",
            package: "gents",
            source_path: "crates/gents/src/lean_vocab_test/canonical_presentation/native_adapter.rs",
            module_path: "lean_vocab_test::support::canonical_presentation::native_adapter",
            function: "generated_payload_presentation_cases_use_native_reconstruction",
        },
        ConformanceConsumer::RustTest {
            id: "canonical_execution_native_adapter::every_generated_native_execution_script_runs_to_completion",
            package: "gents",
            source_path: "crates/gents/src/lean_vocab_test/canonical_execution/native_adapter.rs",
            module_path: "canonical_execution_native_adapter",
            function: "every_generated_native_execution_script_runs_to_completion",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::root_admission::generated_root_admission_cases_drive_production_root_policy",
            package: "gents",
            source_path: "crates/gents/tests/conformance/root_admission.rs",
            module_path: "conformance::root_admission",
            function: "generated_root_admission_cases_drive_production_root_policy",
        },
        ConformanceConsumer::RustTest {
            id: "inference::clients::chatgpt_codex::tests::routing_affinity_matches_lean_owner",
            package: "gents",
            source_path: "crates/gents/src/inference/clients/chatgpt_codex/tests.rs",
            module_path: "inference::clients::chatgpt_codex::tests",
            function: "routing_affinity_matches_lean_owner",
        },
        ConformanceConsumer::RustTest {
            id: "agent::daemon::wake_input::tests::current_wake_input_matches_lean_owner",
            package: "gents",
            source_path: "crates/gents/src/agent/daemon/wake_input.rs",
            module_path: "agent::daemon::wake_input::tests",
            function: "current_wake_input_matches_lean_owner",
        },
        ConformanceConsumer::RustTest {
            id: "session::output::tests::current_input_selection_matches_lean_owner",
            package: "gents",
            source_path: "crates/gents/src/session/output/tests.rs",
            module_path: "session::output::tests",
            function: "current_input_selection_matches_lean_owner",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::runtime::tests::invocation_correlation_matches_lean",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/runtime.rs",
            module_path: "tool_call_lifecycle::runtime::tests",
            function: "invocation_correlation_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "defra_query::field_page::tests::field_pages_match_lean",
            package: "gents",
            source_path: "crates/gents/src/defra_query/field_page.rs",
            module_path: "defra_query::field_page::tests",
            function: "field_pages_match_lean",
        },
        ConformanceConsumer::RustTest {
            id: "defra_write::tests::native_input_admission_matches_lean",
            package: "gents",
            source_path: "crates/gents/src/defra_write/tests.rs",
            module_path: "defra_write::tests",
            function: "native_input_admission_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "application_write::tests::admission_matches_executable_lean_owner",
            package: "gents",
            source_path: "crates/gents/src/plugin/application_write/tests.rs",
            module_path: "application_write::tests",
            function: "admission_matches_executable_lean_owner",
        },
        ConformanceConsumer::RustTest {
            id: "config_client::desired_state::tests::guarded_publication_matches_lean_publish_if_cases",
            package: "gents",
            source_path: "crates/gents/src/config_client/desired_state/tests.rs",
            module_path: "config_client::desired_state::tests",
            function: "guarded_publication_matches_lean_publish_if_cases",
        },
        ConformanceConsumer::RustTest {
            id: "config_client::desired_state::tests::generated_default_replacements_preserve_runtime_startup_selection",
            package: "gents",
            source_path: "crates/gents/src/config_client/desired_state/tests.rs",
            module_path: "config_client::desired_state::tests",
            function: "generated_default_replacements_preserve_runtime_startup_selection",
        },
        ConformanceConsumer::RustTest {
            id: "agent::runtime::tests::agent_resolution::explicit_agent_resolution_matches_lean_binding_cases",
            package: "gents",
            source_path: "crates/gents/src/agent/runtime/tests/agent_resolution.rs",
            module_path: "agent::runtime::tests::agent_resolution",
            function: "explicit_agent_resolution_matches_lean_binding_cases",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::request_input::lean_request_inputs_decode_without_losing_explicit_issuance_facts",
            package: "gents",
            source_path: "crates/gents/tests/conformance/request_input.rs",
            module_path: "conformance::request_input",
            function: "lean_request_inputs_decode_without_losing_explicit_issuance_facts",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::request_input::runtime_continuation_inputs_match_the_lean_owner",
            package: "gents",
            source_path: "crates/gents/tests/conformance/request_input.rs",
            module_path: "conformance::request_input",
            function: "runtime_continuation_inputs_match_the_lean_owner",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::tests::event_group_eligibility_matches_lean_generated_contract_cases",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/tests/mod.rs",
            module_path: "triggers::engine::tests",
            function: "event_group_eligibility_matches_lean_generated_contract_cases",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::enrollment::generated_enrollment_durable_cases_drive_current_projection",
            package: "gents",
            source_path: "crates/gents/tests/conformance/enrollment.rs",
            module_path: "conformance::enrollment",
            function: "generated_enrollment_durable_cases_drive_current_projection",
        },
        ConformanceConsumer::RustTest {
            id: "client::core::enrollment::tests::generated_pending_request_satisfaction_matches_lean",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/core/enrollment/tests.rs",
            module_path: "client::core::enrollment::tests",
            function: "generated_pending_request_satisfaction_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::live_overlay::pending_user_turn_cases_match_lean_table",
            package: "gents",
            source_path: "crates/gents/tests/conformance/live_overlay.rs",
            module_path: "conformance::live_overlay",
            function: "pending_user_turn_cases_match_lean_table",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::tests::steering::generated_pending_steering_terminals_retain_signed_admission_without_output",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/tests/steering.rs",
            module_path: "lifecycle::queue::tests::steering",
            function: "generated_pending_steering_terminals_retain_signed_admission_without_output",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::tests::steering::generated_owned_prepublication_terminals_retain_signed_admission_without_output",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/tests/steering.rs",
            module_path: "lifecycle::queue::tests::steering",
            function: "generated_owned_prepublication_terminals_retain_signed_admission_without_output",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_authored_input_is_durable_before_provider_stream_entry",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/request_assembly.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_authored_input_is_durable_before_provider_stream_entry",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_aggregate_token_budget_cases_drive_the_owned_loop_ledger",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/budgeting.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_aggregate_token_budget_cases_drive_the_owned_loop_ledger",
        },
        ConformanceConsumer::TypeScriptTest {
            id: "packages/gents-desktop-chat/src/chat-shell.test.ts::requestProgressPresentation matches every generated Lean request lifecycle projection",
            app: "gents-desktop-chat",
            source_path: "packages/gents-desktop-chat/src/chat-shell.test.ts",
            suite: "requestProgressPresentation",
            test: "matches every generated Lean request lifecycle projection",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::client_runtime::generated_agent_readiness_cases_drive_the_production_projector",
            package: "gents",
            source_path: "crates/gents/tests/conformance/client_runtime.rs",
            module_path: "conformance::client_runtime",
            function: "generated_agent_readiness_cases_drive_the_production_projector",
        },
        ConformanceConsumer::RustTest {
            id: "node_readiness_publisher::tests::generated_readiness_publication_traces_write_only_semantic_changes",
            package: "gents",
            source_path: "crates/gents/src/node_readiness_publisher/tests.rs",
            module_path: "node_readiness_publisher::tests",
            function: "generated_readiness_publication_traces_write_only_semantic_changes",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_slot_accounting_cases_pin_inference_and_fleet_contracts",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_slot_accounting_cases_pin_inference_and_fleet_contracts",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::rust_eval_outcome_vocabulary_and_projection_match_lean",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "rust_eval_outcome_vocabulary_and_projection_match_lean",
        },
        ConformanceConsumer::RustTest {
            id: "optimization::policy::tests::gates_costs_and_decisions_match_lean",
            package: "gents",
            source_path: "crates/gents/src/eval/optimization/policy.rs",
            module_path: "optimization::policy::tests",
            function: "gates_costs_and_decisions_match_lean",
        },
        ConformanceConsumer::RustTest {
            id: "optimization::subject::tests::task_seed_coverage_matches_lean_for_canonical_pack_and_definition_inputs",
            package: "gents",
            source_path: "crates/gents/src/eval/optimization/subject.rs",
            module_path: "optimization::subject::tests",
            function: "task_seed_coverage_matches_lean_for_canonical_pack_and_definition_inputs",
        },
        ConformanceConsumer::RustTest {
            id: "optimization::driver::matrix::an_unexercised_task_target_is_refused_before_any_job_or_trial_is_created",
            package: "gents",
            source_path: "crates/gents/src/eval/optimization/driver/matrix.rs",
            module_path: "optimization::driver::matrix",
            function: "an_unexercised_task_target_is_refused_before_any_job_or_trial_is_created",
        },
        ConformanceConsumer::RustTest {
            id: "caused_sessions::tests::generated_plugin_job_lineage_cases_drive_caused_session_readers",
            package: "gents-cli",
            source_path: "crates/gents-cli/src/caused_sessions.rs",
            module_path: "caused_sessions::tests",
            function: "generated_plugin_job_lineage_cases_drive_caused_session_readers",
        },
        ConformanceConsumer::RustTest {
            id: "optimization::gate::seeds::tests::exact_seed_rendering_matches_lean",
            package: "gents",
            source_path: "crates/gents/src/eval/optimization/gate/seeds.rs",
            module_path: "optimization::gate::seeds::tests",
            function: "exact_seed_rendering_matches_lean",
        },
        ConformanceConsumer::RustTest {
            id: "optimization::driver::matrix::exact_seed_failures_reject_candidates_before_validation",
            package: "gents",
            source_path: "crates/gents/src/eval/optimization/driver/matrix.rs",
            module_path: "optimization::driver::matrix",
            function: "exact_seed_failures_reject_candidates_before_validation",
        },
        ConformanceConsumer::RustTest {
            id: "commands::codex_shim::turn_projection::tests::generated_reasoning_projection_cases_drive_turn_projection_notifications",
            package: "gents-cli",
            source_path: "crates/gents-cli/src/commands/codex_shim/turn_projection.rs",
            module_path: "commands::codex_shim::turn_projection::tests",
            function: "generated_reasoning_projection_cases_drive_turn_projection_notifications",
        },
        ConformanceConsumer::RustTest {
            id: "commands::codex_shim::turn_projection::tests::generated_tool_metadata_cases_drive_the_tool_item_owner",
            package: "gents-cli",
            source_path: "crates/gents-cli/src/commands/codex_shim/turn_projection.rs",
            module_path: "commands::codex_shim::turn_projection::tests",
            function: "generated_tool_metadata_cases_drive_the_tool_item_owner",
        },
        ConformanceConsumer::RustTest {
            id: "commands::codex_shim::thread_projection::conformance::generated_thread_status_cases_drive_thread_projection",
            package: "gents-cli",
            source_path: "crates/gents-cli/src/commands/codex_shim/thread_projection/conformance.rs",
            module_path: "commands::codex_shim::thread_projection::conformance",
            function: "generated_thread_status_cases_drive_thread_projection",
        },
        ConformanceConsumer::RustTest {
            id: "commands::codex_shim::thread_projection::conformance::generated_agent_selection_cases_drive_scoped_model_binding",
            package: "gents-cli",
            source_path: "crates/gents-cli/src/commands/codex_shim/thread_projection/conformance.rs",
            module_path: "commands::codex_shim::thread_projection::conformance",
            function: "generated_agent_selection_cases_drive_scoped_model_binding",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_core::client::mutations::chat::request::tests::generated_session_recovery_cases_drive_desktop_retry_request",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/mutations/chat/request/tests.rs",
            module_path: "gents_desktop_core::client::mutations::chat::request::tests",
            function: "generated_session_recovery_cases_drive_desktop_retry_request",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_artifact_mode_meet_cases_drive_command_effect_intersection",
            package: "gents",
            source_path: "crates/gents/src/toolset/tests.rs",
            module_path: "toolset::tests",
            function: "generated_artifact_mode_meet_cases_drive_command_effect_intersection",
        },
        ConformanceConsumer::RustTest {
            id: "workspace::overlay::overlay_tests::generated_artifact_admission_cases_drive_live_binding_and_launch_policy",
            package: "gents",
            source_path: "crates/gents/src/workspace/overlay_tests.rs",
            module_path: "workspace::overlay::overlay_tests",
            function: "generated_artifact_admission_cases_drive_live_binding_and_launch_policy",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_artifact_spawn_cases_drive_live_foreground_and_background_launches",
            package: "gents",
            source_path: "crates/gents/src/toolset/tests.rs",
            module_path: "toolset::tests",
            function: "generated_artifact_spawn_cases_drive_live_foreground_and_background_launches",
        },
        ConformanceConsumer::RustTest {
            id: "workspace::tests::operator_base_freeze::generated_operator_base_freeze_cases_drive_real_git_executor",
            package: "gents",
            source_path: "crates/gents/src/workspace/tests/operator_base_freeze.rs",
            module_path: "workspace::tests::operator_base_freeze",
            function: "generated_operator_base_freeze_cases_drive_real_git_executor",
        },
        ConformanceConsumer::RustTest {
            id: "workspace::tests::generated_workspace_path_capability_cases_drive_real_git_executor",
            package: "gents",
            source_path: "crates/gents/src/workspace/tests.rs",
            module_path: "workspace::tests",
            function: "generated_workspace_path_capability_cases_drive_real_git_executor",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::embedded_impl::tests::explicit_file_image_push_keeps_unselected_images_and_collection_interest_local",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/embedded_impl/tests.rs",
            module_path: "agent::p2p_reconcile::embedded_impl::tests",
            function: "explicit_file_image_push_keeps_unselected_images_and_collection_interest_local",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::file_review_access::tests::file_image_read_grant_matches_signed_receipt_contract",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/file_review_access/tests.rs",
            module_path: "tool_call_lifecycle::file_review_access::tests",
            function: "file_image_read_grant_matches_signed_receipt_contract",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::file_image_hydration::tests::generated_cases_drive_exact_tool_selection",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/file_image_hydration/tests.rs",
            module_path: "agent::p2p_reconcile::file_image_hydration::tests",
            function: "generated_cases_drive_exact_tool_selection",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::file_image_hydration::tests::generated_routes_follow_executable_scope_filters",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/file_image_hydration/tests.rs",
            module_path: "agent::p2p_reconcile::file_image_hydration::tests",
            function: "generated_routes_follow_executable_scope_filters",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::file_image_hydration::tests::generated_completion_cases_gate_delivery_and_receipts",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/file_image_hydration/tests.rs",
            module_path: "agent::p2p_reconcile::file_image_hydration::tests",
            function: "generated_completion_cases_gate_delivery_and_receipts",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::file_image_hydration::tests::generated_attempt_cases_preserve_terminal_outcomes_and_prevent_redelivery",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/file_image_hydration/tests.rs",
            module_path: "agent::p2p_reconcile::file_image_hydration::tests",
            function: "generated_attempt_cases_preserve_terminal_outcomes_and_prevent_redelivery",
        },
        ConformanceConsumer::RustTest {
            id: "client::schema::tests::generated_file_review_rules_exclude_whole_collection_delivery",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/schema.rs",
            module_path: "client::schema::tests",
            function: "generated_file_review_rules_exclude_whole_collection_delivery",
        },
        ConformanceConsumer::RustTest {
            id: "mailbox::notification_tests::generated_notification_cases_drive_durable_writes",
            package: "gents",
            source_path: "crates/gents/src/session/mailbox/notification_tests.rs",
            module_path: "mailbox::notification_tests",
            function: "generated_notification_cases_drive_durable_writes",
        },
        ConformanceConsumer::RustTest {
            id: "mailbox::reply_tests::generated_reply_cases_drive_claim_validation",
            package: "gents",
            source_path: "crates/gents/src/session/mailbox/reply_tests.rs",
            module_path: "mailbox::reply_tests",
            function: "generated_reply_cases_drive_claim_validation",
        },
        ConformanceConsumer::RustTest {
            id: "mailbox::reply_tests::modeled_handoff_positive_sequence_maps_to_native_owners",
            package: "gents",
            source_path: "crates/gents/src/session/mailbox/reply_tests.rs",
            module_path: "mailbox::reply_tests",
            function: "modeled_handoff_positive_sequence_maps_to_native_owners",
        },
        ConformanceConsumer::RustTest {
            id: "defra_write::tests::generated_count_carriers_bind_admission_parser_and_storage",
            package: "gents",
            source_path: "crates/gents/src/defra_write/tests.rs",
            module_path: "defra_write::tests",
            function: "generated_count_carriers_bind_admission_parser_and_storage",
        },
        ConformanceConsumer::RustTest {
            id: "agent::output_obligation::logical_tests::generated_logical_output_obligation_cases_drive_signed_requests_and_durable_writes",
            package: "gents",
            source_path: "crates/gents/src/agent/output_obligation/logical_tests.rs",
            module_path: "agent::output_obligation::logical_tests",
            function: "generated_logical_output_obligation_cases_drive_signed_requests_and_durable_writes",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::tests::generated_plugin_resource_cases_bind_budget_and_consent",
            package: "gents",
            source_path: "crates/gents/src/plugin/tests.rs",
            module_path: "plugin::tests",
            function: "generated_plugin_resource_cases_bind_budget_and_consent",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::http_calls::tests::generated_plugin_network_cases_drive_admission",
            package: "gents",
            source_path: "crates/gents/src/plugin/http_calls_tests.rs",
            module_path: "plugin::http_calls::tests",
            function: "generated_plugin_network_cases_drive_admission",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::tests::generated_plugin_call_access_cases_drive_bind_admission",
            package: "gents",
            source_path: "crates/gents/src/plugin/tests.rs",
            module_path: "plugin::tests",
            function: "generated_plugin_call_access_cases_drive_bind_admission",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::tests::executor::generated_plugin_invocation_retry_revalidates_current_installation",
            package: "gents",
            source_path: "crates/gents/src/plugin/tests/executor.rs",
            module_path: "plugin::tests::executor",
            function: "generated_plugin_invocation_retry_revalidates_current_installation",
        },
        ConformanceConsumer::RustTest {
            id: "pack::publication_contract_tests::generated_local_publication_survives_notification_outcomes",
            package: "gents",
            source_path: "crates/gents/src/pack/publication_contract_tests.rs",
            module_path: "pack::publication_contract_tests",
            function: "generated_local_publication_survives_notification_outcomes",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::plugin_receipt::tests::generated_receipt_commit_cases",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/plugin_receipt.rs",
            module_path: "tool_call_lifecycle::plugin_receipt::tests",
            function: "generated_receipt_commit_cases",
        },
        ConformanceConsumer::RustTest {
            id: "pack::tests::generated_plugin_model_slots_require_optional_agent_free_declarations",
            package: "gents",
            source_path: "crates/gents/src/pack/tests.rs",
            module_path: "pack::tests",
            function: "generated_plugin_model_slots_require_optional_agent_free_declarations",
        },
        ConformanceConsumer::RustTest {
            id: "tool_surface::timeouts::tests::generated_tool_timeout_cases_bind_native_resolution",
            package: "gents",
            source_path: "crates/gents/src/tool_surface/timeouts.rs",
            module_path: "tool_surface::timeouts::tests",
            function: "generated_tool_timeout_cases_bind_native_resolution",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::plugin_effect::tests::generated_plugin_effect_admission_matches_execution_owner",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/plugin_effect.rs",
            module_path: "tool_call_lifecycle::plugin_effect::tests",
            function: "generated_plugin_effect_admission_matches_execution_owner",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::query::result::tests::generated_plugin_child_terminal_presentation_matches_execution_owner",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/query/result.rs",
            module_path: "tool_call_lifecycle::query::result::tests",
            function: "generated_plugin_child_terminal_presentation_matches_execution_owner",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::terminal_tools::tests::generated_plugin_child_request_accounting_matches_execution_owner",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/terminal_tools.rs",
            module_path: "lifecycle::terminal_tools::tests",
            function: "generated_plugin_child_request_accounting_matches_execution_owner",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::tool_calls::tests::generated_tool_call_bounds_and_grants",
            package: "gents",
            source_path: "crates/gents/src/plugin/tool_calls.rs",
            module_path: "plugin::tool_calls::tests",
            function: "generated_tool_call_bounds_and_grants",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::claude_messages_redacted_and_signed_tool_round_trip_through_owned_loop",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/claude.rs",
            module_path: "agent::loop_stream::tests",
            function: "claude_messages_redacted_and_signed_tool_round_trip_through_owned_loop",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_schema_argument_repair_cases_drive_owned_loop",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/schema_argument_repair.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_schema_argument_repair_cases_drive_owned_loop",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_child_admission_cases_drive_real_dispatcher",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/tool_execution.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_child_admission_cases_drive_real_dispatcher",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_invalid_tool_progress_cases_drive_owned_loop",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/invalid_tool_progress.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_invalid_tool_progress_cases_drive_owned_loop",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_repeated_tool_failure_cases_drive_owned_loop",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/repeated_tool_failure.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_repeated_tool_failure_cases_drive_owned_loop",
        },
        ConformanceConsumer::RustTest {
            id: "workspace::tests::path_alias_contract::generated_workspace_path_alias_cases_drive_real_git_delta",
            package: "gents",
            source_path: "crates/gents/src/workspace/tests/path_alias_contract.rs",
            module_path: "workspace::tests::path_alias_contract",
            function: "generated_workspace_path_alias_cases_drive_real_git_delta",
        },
        ConformanceConsumer::RustTest {
            id: "goal::first_request_binding::contract_tests::generated_first_request_bindings_commit_exact_physical_roots",
            package: "gents",
            source_path: "crates/gents/src/goal/first_request_binding/contract_tests.rs",
            module_path: "goal::first_request_binding::contract_tests",
            function: "generated_first_request_bindings_commit_exact_physical_roots",
        },
        ConformanceConsumer::RustTest {
            id: "goal::first_request_binding::contract_tests::generated_continuation_assignments_preserve_physical_bindings",
            package: "gents",
            source_path: "crates/gents/src/goal/first_request_binding/contract_tests.rs",
            module_path: "goal::first_request_binding::contract_tests",
            function: "generated_continuation_assignments_preserve_physical_bindings",
        },
        ConformanceConsumer::RustTest {
            id: "goal::request_assignment::history_tests::generated_assignment_histories_drive_goal_version_selection",
            package: "gents",
            source_path: "crates/gents/src/goal/request_assignment/history_tests.rs",
            module_path: "goal::request_assignment::history_tests",
            function: "generated_assignment_histories_drive_goal_version_selection",
        },
        ConformanceConsumer::RustTest {
            id: "session_message::goal_input_tests::generated_session_messages_capture_target_goal_before_signing",
            package: "gents",
            source_path: "crates/gents/src/session/message/goal_input_tests.rs",
            module_path: "session_message::goal_input_tests",
            function: "generated_session_messages_capture_target_goal_before_signing",
        },
        ConformanceConsumer::RustTest {
            id: "plugin_job::effects::goal_input_tests::generated_deliveries_capture_destination_goal_in_the_admission_transaction",
            package: "gents",
            source_path: "crates/gents/src/plugin_job/effects/goal_input_tests.rs",
            module_path: "plugin_job::effects::goal_input_tests",
            function: "generated_deliveries_capture_destination_goal_in_the_admission_transaction",
        },
        ConformanceConsumer::RustTest {
            id: "client::core::goal::tests::generated_goal_input_bindings_wait_for_verified_source_state",
            package: "gents-desktop-core",
            source_path: "crates/gents-desktop-core/src/client/core/goal.rs",
            module_path: "client::core::goal::tests",
            function: "generated_goal_input_bindings_wait_for_verified_source_state",
        },
        ConformanceConsumer::RustTest {
            id: "request_input::tests::generated_fresh_session_resubmissions_drop_invocation_lineage",
            package: "gents-protocol",
            source_path: "crates/gents-protocol/src/request_input/tests.rs",
            module_path: "request_input::tests",
            function: "generated_fresh_session_resubmissions_drop_invocation_lineage",
        },
        ConformanceConsumer::RustTest {
            id: "goal::request_assignment::contract_tests::generated_goal_request_assignments_drive_signed_binding",
            package: "gents",
            source_path: "crates/gents/src/goal/request_assignment/contract_tests.rs",
            module_path: "goal::request_assignment::contract_tests",
            function: "generated_goal_request_assignments_drive_signed_binding",
        },
        ConformanceConsumer::RustTest {
            id: "agent::daemon::wake_input::scope_tests::generated_goal_wake_receipts_scope_claims_and_exact_acknowledgement",
            package: "gents",
            source_path: "crates/gents/src/agent/daemon/wake_input_scope_tests.rs",
            module_path: "agent::daemon::wake_input::scope_tests",
            function: "generated_goal_wake_receipts_scope_claims_and_exact_acknowledgement",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::tests::goal_assignment_wakes::generated_goal_wake_scopes_preserve_atomic_notification_bindings",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/tests/goal_assignment_wakes.rs",
            module_path: "lifecycle::queue::tests::goal_assignment_wakes",
            function: "generated_goal_wake_scopes_preserve_atomic_notification_bindings",
        },
        ConformanceConsumer::RustTest {
            id: "goal::request_head::tests::generated_goal_wake_ancestry_keeps_assignment_without_widening_invocations",
            package: "gents",
            source_path: "crates/gents/src/goal/request_head_tests.rs",
            module_path: "goal::request_head::tests",
            function: "generated_goal_wake_ancestry_keeps_assignment_without_widening_invocations",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::goal_context::generated_goal_tool_context_uses_the_signed_callers_assignment",
            package: "gents",
            source_path: "crates/gents/src/hook/tests/goal_context.rs",
            module_path: "hook::tests::goal_context",
            function: "generated_goal_tool_context_uses_the_signed_callers_assignment",
        },
        ConformanceConsumer::RustTest {
            id: "goal::request_head::tests::generated_goal_ancestry_cases_keep_one_signed_goal",
            package: "gents",
            source_path: "crates/gents/src/goal/request_head_tests.rs",
            module_path: "goal::request_head::tests",
            function: "generated_goal_ancestry_cases_keep_one_signed_goal",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::folding::assignment_tests::generated_goal_assignments_bound_fold_and_steering",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/folding/assignment_tests.rs",
            module_path: "lifecycle::queue::folding::assignment_tests",
            function: "generated_goal_assignments_bound_fold_and_steering",
        },
        ConformanceConsumer::RustTest {
            id: "goal::request_head::tests::generated_goal_request_head_cases_drive_signed_row_selector",
            package: "gents",
            source_path: "crates/gents/src/goal/request_head_tests.rs",
            module_path: "goal::request_head::tests",
            function: "generated_goal_request_head_cases_drive_signed_row_selector",
        },
        ConformanceConsumer::RustTest {
            id: "goal::claimed_publication::contract_tests::generated_goal_claimed_publication_cases_drive_real_transactions",
            package: "gents",
            source_path: "crates/gents/src/goal/claimed_publication/contract_tests.rs",
            module_path: "goal::claimed_publication::contract_tests",
            function: "generated_goal_claimed_publication_cases_drive_real_transactions",
        },
        ConformanceConsumer::RustTest {
            id: "goal::client_control::contract_tests::generated_goal_client_controls_drive_signed_commands",
            package: "gents",
            source_path: "crates/gents/src/goal/client_control/contract_tests.rs",
            module_path: "goal::client_control::contract_tests",
            function: "generated_goal_client_controls_drive_signed_commands",
        },
        ConformanceConsumer::RustTest {
            id: "goal::client_control::start_tests::generated_goal_starts_commit_the_session_goal_and_first_request_together",
            package: "gents",
            source_path: "crates/gents/src/goal/client_control/start_tests.rs",
            module_path: "goal::client_control::start_tests",
            function: "generated_goal_starts_commit_the_session_goal_and_first_request_together",
        },
        ConformanceConsumer::RustTest {
            id: "goal::client_control::receipt_tests::generated_goal_receipt_traces_preserve_results_and_reject_erased_stale_intents",
            package: "gents",
            source_path: "crates/gents/src/goal/client_control/receipt_tests.rs",
            module_path: "goal::client_control::receipt_tests",
            function: "generated_goal_receipt_traces_preserve_results_and_reject_erased_stale_intents",
        },
        ConformanceConsumer::RustTest {
            id: "goal::observation::contract_tests::generated_goal_observations_drive_signature_and_scope_validation",
            package: "gents",
            source_path: "crates/gents/src/goal/observation/contract_tests.rs",
            module_path: "goal::observation::contract_tests",
            function: "generated_goal_observations_drive_signature_and_scope_validation",
        },
        ConformanceConsumer::RustTest {
            id: "goal::observation::contract_tests::generated_goal_rescans_recover_lost_notifications",
            package: "gents",
            source_path: "crates/gents/src/goal/observation/contract_tests.rs",
            module_path: "goal::observation::contract_tests",
            function: "generated_goal_rescans_recover_lost_notifications",
        },
        ConformanceConsumer::RustTest {
            id: "plugin::approval::contract_tests::generated_approval_traces_drive_native_queue_and_grants",
            package: "gents",
            source_path: "crates/gents/src/plugin/approval_contract_tests.rs",
            module_path: "plugin::approval::contract_tests",
            function: "generated_approval_traces_drive_native_queue_and_grants",
        },
        ConformanceConsumer::RustTest {
            id: "goal::edit::contract_tests::generated_goal_edit_cases_drive_real_transactions",
            package: "gents",
            source_path: "crates/gents/src/goal/edit/contract_tests.rs",
            module_path: "goal::edit::contract_tests",
            function: "generated_goal_edit_cases_drive_real_transactions",
        },
        ConformanceConsumer::RustTest {
            id: "goal::edit::contract_tests::generated_goal_objective_whitespace_matches_native_character_classification",
            package: "gents",
            source_path: "crates/gents/src/goal/edit/contract_tests.rs",
            module_path: "goal::edit::contract_tests",
            function: "generated_goal_objective_whitespace_matches_native_character_classification",
        },
        ConformanceConsumer::RustTest {
            id: "goal::operator_resume::contract_tests::generated_goal_operator_resume_cases_drive_real_transactions",
            package: "gents",
            source_path: "crates/gents/src/goal/operator_resume/contract_tests.rs",
            module_path: "goal::operator_resume::contract_tests",
            function: "generated_goal_operator_resume_cases_drive_real_transactions",
        },
        ConformanceConsumer::RustTest {
            id: "goal::operator_resume::tests::generated_resume_on_cases_drive_account_preflight_and_receipt_recovery",
            package: "gents",
            source_path: "crates/gents/src/goal/operator_resume/tests.rs",
            module_path: "goal::operator_resume::tests",
            function: "generated_resume_on_cases_drive_account_preflight_and_receipt_recovery",
        },
        ConformanceConsumer::RustTest {
            id: "goal::operator_resume::contract_tests::generated_goal_config_reactivation_cases_drive_transactional_setter",
            package: "gents",
            source_path: "crates/gents/src/goal/operator_resume/contract_tests.rs",
            module_path: "goal::operator_resume::contract_tests",
            function: "generated_goal_config_reactivation_cases_drive_transactional_setter",
        },
        ConformanceConsumer::RustTest {
            id: "goal::reset_resume::contract_tests::generated_goal_reset_resume_cases_drive_real_transactions",
            package: "gents",
            source_path: "crates/gents/src/goal/reset_resume/contract_tests.rs",
            module_path: "goal::reset_resume::contract_tests",
            function: "generated_goal_reset_resume_cases_drive_real_transactions",
        },
        ConformanceConsumer::RustTest {
            id: "cli_goal::goal_resume_request_reuses_signed_predecessor_and_returns_same_child",
            package: "gents-cli",
            source_path: "crates/gents-cli/tests/suites/cli_goal.rs",
            module_path: "cli_goal",
            function: "goal_resume_request_reuses_signed_predecessor_and_returns_same_child",
        },
        ConformanceConsumer::RustTest {
            id: "admission::registry::contract_tests::generated_inference_registry_cases_drive_real_permits",
            package: "gents",
            source_path: "crates/gents/src/inference/admission/registry_contract_tests.rs",
            module_path: "admission::registry::contract_tests",
            function: "generated_inference_registry_cases_drive_real_permits",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::execution_lease::tests::generated_renewal_invalidates_real_publication_point_read_transaction",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/execution_lease/tests.rs",
            module_path: "lifecycle::execution_lease::tests",
            function: "generated_renewal_invalidates_real_publication_point_read_transaction",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::execution_lease::tests::generated_recovery_invalidates_real_admitted_renewal_transaction",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/execution_lease/tests.rs",
            module_path: "lifecycle::execution_lease::tests",
            function: "generated_recovery_invalidates_real_admitted_renewal_transaction",
        },
        ConformanceConsumer::RustTest {
            id: "lean_vocab_test::request_execution_lease_policy::generated_provider_eof_cases_fence_production_policy",
            package: "gents",
            source_path: "crates/gents/src/lean_vocab_test/request_execution_lease_policy.rs",
            module_path: "lean_vocab_test::request_execution_lease_policy",
            function: "generated_provider_eof_cases_fence_production_policy",
        },
        ConformanceConsumer::RustTest {
            id: "lean_vocab_test::task_hooks_policy::generated_task_hook_admission_cases_fence_production_validation",
            package: "gents",
            source_path: "crates/gents/src/lean_vocab_test/task_hooks_policy.rs",
            module_path: "lean_vocab_test::task_hooks_policy",
            function: "generated_task_hook_admission_cases_fence_production_validation",
        },
        ConformanceConsumer::RustTest {
            id: "lean_vocab_test::task_hooks_policy::generated_task_hook_cases_fence_the_modeled_phase_vocabulary",
            package: "gents",
            source_path: "crates/gents/src/lean_vocab_test/task_hooks_policy.rs",
            module_path: "lean_vocab_test::task_hooks_policy",
            function: "generated_task_hook_cases_fence_the_modeled_phase_vocabulary",
        },
        ConformanceConsumer::RustTest {
            id: "lean_vocab_test::task_hook_executor::generated_task_hook_run_cases_drive_real_host_commands",
            package: "gents",
            source_path: "crates/gents/src/lean_vocab_test/task_hook_executor.rs",
            module_path: "lean_vocab_test::task_hook_executor",
            function: "generated_task_hook_run_cases_drive_real_host_commands",
        },
        ConformanceConsumer::RustTest {
            id: "lean_vocab_test::task_hook_recovery::generated_task_hook_recovery_cases_drive_startup_recovery_and_host_records",
            package: "gents",
            source_path: "crates/gents/src/lean_vocab_test/task_hook_recovery.rs",
            module_path: "lean_vocab_test::task_hook_recovery",
            function: "generated_task_hook_recovery_cases_drive_startup_recovery_and_host_records",
        },
        ConformanceConsumer::RustTest {
            id: "lean_vocab_test::task_hook_recovery::generated_task_hook_sweep_cases_drive_the_record_sweep",
            package: "gents",
            source_path: "crates/gents/src/lean_vocab_test/task_hook_recovery.rs",
            module_path: "lean_vocab_test::task_hook_recovery",
            function: "generated_task_hook_sweep_cases_drive_the_record_sweep",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::mailbox::rust_mailbox_vocabularies_and_machine_match_lean_contract",
            package: "gents",
            source_path: "crates/gents/tests/conformance/mailbox.rs",
            module_path: "conformance::mailbox",
            function: "rust_mailbox_vocabularies_and_machine_match_lean_contract",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::goals::rust_goal_status_vocabulary_and_machine_match_lean_contract",
            package: "gents",
            source_path: "crates/gents/tests/conformance/goals.rs",
            module_path: "conformance::goals",
            function: "rust_goal_status_vocabulary_and_machine_match_lean_contract",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::goals::generated_goal_decision_cases_fence_runtime_controller",
            package: "gents",
            source_path: "crates/gents/tests/conformance/goals.rs",
            module_path: "conformance::goals",
            function: "generated_goal_decision_cases_fence_runtime_controller",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::goals::generated_goal_readiness_gate_cases_fence_retry_accounting",
            package: "gents",
            source_path: "crates/gents/tests/conformance/goals.rs",
            module_path: "conformance::goals",
            function: "generated_goal_readiness_gate_cases_fence_retry_accounting",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::goal_source::claimed_readiness_tests::generated_claimed_readiness_cases_drive_goal_source",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/goal_source/claimed_readiness_tests.rs",
            module_path: "triggers::engine::goal_source::claimed_readiness_tests",
            function: "generated_claimed_readiness_cases_drive_goal_source",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::goals::generated_goal_transition_cases_fence_runtime_state_machine",
            package: "gents",
            source_path: "crates/gents/tests/conformance/goals.rs",
            module_path: "conformance::goals",
            function: "generated_goal_transition_cases_fence_runtime_state_machine",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::goals::generated_goal_create_cases_fence_authority_and_idempotency",
            package: "gents",
            source_path: "crates/gents/tests/conformance/goals.rs",
            module_path: "conformance::goals",
            function: "generated_goal_create_cases_fence_authority_and_idempotency",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::goals::generated_task_goal_cases_fence_declaration_and_identity",
            package: "gents",
            source_path: "crates/gents/tests/conformance/goals.rs",
            module_path: "conformance::goals",
            function: "generated_task_goal_cases_fence_declaration_and_identity",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::goals::generated_task_goal_recovery_cases_fence_request_witness",
            package: "gents",
            source_path: "crates/gents/tests/conformance/goals.rs",
            module_path: "conformance::goals",
            function: "generated_task_goal_recovery_cases_fence_request_witness",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::goals::generated_goal_submission_cases_fence_atomic_visibility",
            package: "gents",
            source_path: "crates/gents/tests/conformance/goals.rs",
            module_path: "conformance::goals",
            function: "generated_goal_submission_cases_fence_atomic_visibility",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::goals::generated_goal_continuation_materialization_cases_fence_restart_idempotency",
            package: "gents",
            source_path: "crates/gents/tests/conformance/goals.rs",
            module_path: "conformance::goals",
            function: "generated_goal_continuation_materialization_cases_fence_restart_idempotency",
        },
        ConformanceConsumer::RustTest {
            id: "goal_continuation_live::durable_goal_continues_with_real_inference_until_model_completes",
            package: "gents",
            source_path: "crates/gents/tests/e2e_live/goal_continuation_live.rs",
            module_path: "goal_continuation_live",
            function: "durable_goal_continues_with_real_inference_until_model_completes",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::generated_slot_accounting_fleet_cases_match_admission_runtime_boundary",
            package: "gents",
            source_path: "crates/gents/src/inference/admission/tests.rs",
            module_path: "admission::tests",
            function: "generated_slot_accounting_fleet_cases_match_admission_runtime_boundary",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_inference_slot_accounting_cases_drive_db_backed_reconstruction",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_inference_slot_accounting_cases_drive_db_backed_reconstruction",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::rust_inference_call_state_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/inference/admission/tests.rs",
            module_path: "admission::tests",
            function: "rust_inference_call_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::rust_inference_call_terminal_reason_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/inference/admission/tests.rs",
            module_path: "admission::tests",
            function: "rust_inference_call_terminal_reason_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "admission::tests::rust_inference_call_transition_table_matches_lean_contract",
            package: "gents",
            source_path: "crates/gents/src/inference/admission/tests.rs",
            module_path: "admission::tests",
            function: "rust_inference_call_transition_table_matches_lean_contract",
        },
        ConformanceConsumer::TypeScriptTest {
            id: "packages/gents-desktop-chat/src/chat-shell.test.ts::projectChatShell matches generated Lean ClientShell projection contracts",
            app: "gents-desktop-chat",
            source_path: "packages/gents-desktop-chat/src/chat-shell.test.ts",
            suite: "projectChatShell",
            test: "matches generated Lean ClientShell projection contracts",
        },
        ConformanceConsumer::RustTest {
            id: "agent::p2p_reconcile::session_hydration_closure::tests::generated_modeled_closure_input_selects_exact_native_manifest",
            package: "gents",
            source_path: "crates/gents/src/agent/p2p_reconcile/session_hydration_closure.rs",
            module_path: "agent::p2p_reconcile::session_hydration_closure::tests",
            function: "generated_modeled_closure_input_selects_exact_native_manifest",
        },
        ConformanceConsumer::RustTest {
            id: "rendered_request::encoding::tests::generated_storage_cases_drive_the_lossless_codec",
            package: "gents",
            source_path: "crates/gents/src/inference/rendered_request/encoding.rs",
            module_path: "rendered_request::encoding::tests",
            function: "generated_storage_cases_drive_the_lossless_codec",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::session_hydration::generated_session_hydration_apply_cases_match_terminal_delivery_core",
            package: "gents",
            source_path: "crates/gents/tests/conformance/session_hydration.rs",
            module_path: "conformance::session_hydration",
            function: "generated_session_hydration_apply_cases_match_terminal_delivery_core",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::session_hydration::generated_session_hydration_start_cases_match_receiver_start",
            package: "gents",
            source_path: "crates/gents/tests/conformance/session_hydration.rs",
            module_path: "conformance::session_hydration",
            function: "generated_session_hydration_start_cases_match_receiver_start",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::session_hydration::generated_session_hydration_admission_cases_match_receiver_owner",
            package: "gents",
            source_path: "crates/gents/tests/conformance/session_hydration.rs",
            module_path: "conformance::session_hydration",
            function: "generated_session_hydration_admission_cases_match_receiver_owner",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::enrollment::generated_enrollment_cases_match_production_transition_core",
            package: "gents",
            source_path: "crates/gents/tests/conformance/enrollment.rs",
            module_path: "conformance::enrollment",
            function: "generated_enrollment_cases_match_production_transition_core",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::enrollment::generated_enrollment_encoding_vectors_match_wire_codec",
            package: "gents",
            source_path: "crates/gents/tests/conformance/enrollment.rs",
            module_path: "conformance::enrollment",
            function: "generated_enrollment_encoding_vectors_match_wire_codec",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::enrollment::generated_enrollment_digest_vectors_match_wire_codec",
            package: "gents",
            source_path: "crates/gents/tests/conformance/enrollment.rs",
            module_path: "conformance::enrollment",
            function: "generated_enrollment_digest_vectors_match_wire_codec",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::enrollment::generated_agent_request_admission_cases_match_shared_projector",
            package: "gents",
            source_path: "crates/gents/tests/conformance/enrollment.rs",
            module_path: "conformance::enrollment",
            function: "generated_agent_request_admission_cases_match_shared_projector",
        },
        ConformanceConsumer::RustTest {
            id: "session_message::agent_tools_tests::session_target_authority_matches_lean_owner",
            package: "gents",
            source_path: "crates/gents/src/session/message/agent_tools_tests.rs",
            module_path: "session_message::agent_tools_tests",
            function: "session_target_authority_matches_lean_owner",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::enrollment::generated_causal_hop_cases_match_native_materializer",
            package: "gents",
            source_path: "crates/gents/tests/conformance/enrollment.rs",
            module_path: "conformance::enrollment",
            function: "generated_causal_hop_cases_match_native_materializer",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_tool_policy_cases_match_lean_composition",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_tool_policy_cases_match_lean_composition",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_goal_capability_resolution_matches_rust_decoder",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_goal_capability_resolution_matches_rust_decoder",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_lsp_action_cases_match_rust_authorization",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_lsp_action_cases_match_rust_authorization",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::self_config_field_tables_match_lean_contract",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "self_config_field_tables_match_lean_contract",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_self_config_cases_fence_patch_merge",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_self_config_cases_fence_patch_merge",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::self_config::generated_agent_decision_cases_drive_production_agent_admission",
            package: "gents",
            source_path: "crates/gents/tests/conformance/self_config.rs",
            module_path: "conformance::self_config",
            function: "generated_agent_decision_cases_drive_production_agent_admission",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::self_config::generated_agent_materialization_cases_drive_production_agent_materialization",
            package: "gents",
            source_path: "crates/gents/tests/conformance/self_config.rs",
            module_path: "conformance::self_config",
            function: "generated_agent_materialization_cases_drive_production_agent_materialization",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::self_config::generated_sibling_tools_cases_drive_production_tools_selection",
            package: "gents",
            source_path: "crates/gents/tests/conformance/self_config.rs",
            module_path: "conformance::self_config",
            function: "generated_sibling_tools_cases_drive_production_tools_selection",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::self_config::generated_self_config_selection_cases_drive_production_tools_selection",
            package: "gents",
            source_path: "crates/gents/tests/conformance/self_config.rs",
            module_path: "conformance::self_config",
            function: "generated_self_config_selection_cases_drive_production_tools_selection",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::self_config::generated_self_config_job_guard_cases_drive_native_guard",
            package: "gents",
            source_path: "crates/gents/tests/conformance/self_config.rs",
            module_path: "conformance::self_config",
            function: "generated_self_config_job_guard_cases_drive_native_guard",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::plugin_job::generated_plugin_job_admission_cases_drive_admit_step",
            package: "gents",
            source_path: "crates/gents/tests/conformance/plugin_job.rs",
            module_path: "conformance::plugin_job",
            function: "generated_plugin_job_admission_cases_drive_admit_step",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::plugin_job::generated_plugin_job_adoption_cases_drive_adopt",
            package: "gents",
            source_path: "crates/gents/tests/conformance/plugin_job.rs",
            module_path: "conformance::plugin_job",
            function: "generated_plugin_job_adoption_cases_drive_adopt",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::plugin_job::generated_plugin_job_control_cases_drive_control_owners",
            package: "gents",
            source_path: "crates/gents/tests/conformance/plugin_job.rs",
            module_path: "conformance::plugin_job",
            function: "generated_plugin_job_control_cases_drive_control_owners",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::plugin_job::generated_plugin_job_key_cases_match_protocol_encoders",
            package: "gents",
            source_path: "crates/gents/tests/conformance/plugin_job.rs",
            module_path: "conformance::plugin_job",
            function: "generated_plugin_job_key_cases_match_protocol_encoders",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::plugin_job::generated_plugin_job_authority_cases_drive_origin_and_provenance",
            package: "gents",
            source_path: "crates/gents/tests/conformance/plugin_job.rs",
            module_path: "conformance::plugin_job",
            function: "generated_plugin_job_authority_cases_drive_origin_and_provenance",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::plugin_job::generated_plugin_job_wire_cases_round_trip_protocol_types",
            package: "gents",
            source_path: "crates/gents/tests/conformance/plugin_job.rs",
            module_path: "conformance::plugin_job",
            function: "generated_plugin_job_wire_cases_round_trip_protocol_types",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::plugin_job::generated_plugin_job_lineage_cases_decode_strictly",
            package: "gents",
            source_path: "crates/gents/tests/conformance/plugin_job.rs",
            module_path: "conformance::plugin_job",
            function: "generated_plugin_job_lineage_cases_decode_strictly",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::plugin_job::plugin_job_protocol_field_names_are_secret_free",
            package: "gents",
            source_path: "crates/gents/tests/conformance/plugin_job.rs",
            module_path: "conformance::plugin_job",
            function: "plugin_job_protocol_field_names_are_secret_free",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::session_control::generated_session_control_cases_drive_native_interrupt_stop_and_resume",
            package: "gents",
            source_path: "crates/gents/tests/conformance/session_control.rs",
            module_path: "conformance::session_control",
            function: "generated_session_control_cases_drive_native_interrupt_stop_and_resume",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::recovery_sweeps::generated_plugin_job_recovery_cases_drive_redrive_owner",
            package: "gents",
            source_path: "crates/gents/tests/conformance/recovery_sweeps.rs",
            module_path: "conformance::recovery_sweeps",
            function: "generated_plugin_job_recovery_cases_drive_redrive_owner",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::background::generated_plugin_job_restart_and_foreground_rows_match_owners",
            package: "gents",
            source_path: "crates/gents/tests/conformance/background.rs",
            module_path: "conformance::background",
            function: "generated_plugin_job_restart_and_foreground_rows_match_owners",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::tool_policy::generated_plugin_data_cases_drive_admit_write",
            package: "gents",
            source_path: "crates/gents/tests/conformance/tool_policy.rs",
            module_path: "conformance::tool_policy",
            function: "generated_plugin_data_cases_drive_admit_write",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::tool_policy::generated_plugin_job_resource_cases_drive_scratch",
            package: "gents",
            source_path: "crates/gents/tests/conformance/tool_policy.rs",
            module_path: "conformance::tool_policy",
            function: "generated_plugin_job_resource_cases_drive_scratch",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::tool_policy::generated_plugin_host_import_cases_drive_the_grant",
            package: "gents",
            source_path: "crates/gents/tests/conformance/tool_policy.rs",
            module_path: "conformance::tool_policy",
            function: "generated_plugin_host_import_cases_drive_the_grant",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::tool_policy::generated_plugin_calls_route_cases_drive_supported",
            package: "gents",
            source_path: "crates/gents/tests/conformance/tool_policy.rs",
            module_path: "conformance::tool_policy",
            function: "generated_plugin_calls_route_cases_drive_supported",
        },
        ConformanceConsumer::RustTest {
            id: "cli_mcp_probe::mcp_probe_json_reports_health_snapshot_for_registry_service",
            package: "gents-cli",
            source_path: "crates/gents-cli/tests/suites/cli_mcp_probe.rs",
            module_path: "cli_mcp_probe",
            function: "mcp_probe_json_reports_health_snapshot_for_registry_service",
        },
        ConformanceConsumer::RustTest {
            id: "cli_config_task_run::config_task_run_matches_lean_manual_dispatch_contract",
            package: "gents-cli",
            source_path: "crates/gents-cli/tests/suites/cli_config_task_run.rs",
            module_path: "cli_config_task_run",
            function: "config_task_run_matches_lean_manual_dispatch_contract",
        },
        ConformanceConsumer::RustTest {
            id: "inference::backends::registry::tests::generated_backend_health_admission_cases_match_registry_and_admission_policy",
            package: "gents",
            source_path: "crates/gents/src/inference/backends/registry/tests.rs",
            module_path: "inference::backends::registry::tests",
            function: "generated_backend_health_admission_cases_match_registry_and_admission_policy",
        },
        ConformanceConsumer::RustTest {
            id: "inference::backends::registry::tests::display_state_matches_every_lean_backend_health_admission_case",
            package: "gents",
            source_path: "crates/gents/src/inference/backends/registry/tests.rs",
            module_path: "inference::backends::registry::tests",
            function: "display_state_matches_every_lean_backend_health_admission_case",
        },
        ConformanceConsumer::RustTest {
            id: "inference::backends::health::tests::generated_backend_health_cases_match_prober_transitions",
            package: "gents",
            source_path: "crates/gents/src/inference/backends/health/tests.rs",
            module_path: "inference::backends::health::tests",
            function: "generated_backend_health_cases_match_prober_transitions",
        },
        ConformanceConsumer::RustTest {
            id: "inference::backends::health::tests::generated_backend_probe_schedule_cases_match_runtime_selection",
            package: "gents",
            source_path: "crates/gents/src/inference/backends/health/tests.rs",
            module_path: "inference::backends::health::tests",
            function: "generated_backend_probe_schedule_cases_match_runtime_selection",
        },
        ConformanceConsumer::RustTest {
            id: "http::prometheus::tests::backend_probe_status_metric_reflects_measured_health",
            package: "gents-cli",
            source_path: "crates/gents-cli/src/http/prometheus.rs",
            module_path: "http::prometheus::tests",
            function: "backend_probe_status_metric_reflects_measured_health",
        },
        ConformanceConsumer::RustTest {
            id: "cli_server::server_exposes_fleet_slot_snapshot_endpoint",
            package: "gents-cli",
            source_path: "crates/gents-cli/tests/cli_server.rs",
            module_path: "cli_server",
            function: "server_exposes_fleet_slot_snapshot_endpoint",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_projection_consumes_generated_client_shell_contract_cases",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/session_state.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::session_state",
            function: "session_snapshot_projection_consumes_generated_client_shell_contract_cases",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::session::live_delta::tests::generated_live_cursor_contract",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/session/live_delta/tests.rs",
            module_path: "gents_desktop_bridge::snapshot::session::live_delta::tests",
            function: "generated_live_cursor_contract",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_binds_generated_session_turn_cases",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/session_state.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::session_state",
            function: "session_snapshot_binds_generated_session_turn_cases",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::session_state::session_queue_order_matches_native_arrival_model_after_claim",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/session_state.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::session_state",
            function: "session_queue_order_matches_native_arrival_model_after_claim",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_binds_request_lifecycle_operator_ui_cases",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/session_state.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::session_state",
            function: "session_snapshot_binds_request_lifecycle_operator_ui_cases",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_transcript_rendering_consumes_generated_transcript_cases",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/session_state.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::session_state",
            function: "session_snapshot_transcript_rendering_consumes_generated_transcript_cases",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::session_timeline::structured_command_policy_denial_projects_to_rendered_tool",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/session_timeline.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::session_timeline",
            function: "structured_command_policy_denial_projects_to_rendered_tool",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::runtime::task_recent_runs_view_consumes_generated_trigger_dispatch_lineage_contract_cases",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/runtime.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::runtime",
            function: "task_recent_runs_view_consumes_generated_trigger_dispatch_lineage_contract_cases",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::mcp_health::mcp_health_view_preserves_every_generated_lean_mcp_health_case_transition",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/mcp_health.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::mcp_health",
            function: "mcp_health_view_preserves_every_generated_lean_mcp_health_case_transition",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::operations_snapshot::tests::project_filters_to_background_await_mode_only",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/operations_snapshot/tests.rs",
            module_path: "gents_desktop_bridge::snapshot::operations_snapshot::tests",
            function: "project_filters_to_background_await_mode_only",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::provenance::tests::generated_plugin_job_lineage_cases_drive_desktop_opening_request",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/provenance.rs",
            module_path: "gents_desktop_bridge::provenance::tests",
            function: "generated_plugin_job_lineage_cases_drive_desktop_opening_request",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::background_admission::spawn_process_rejects_target_policy_before_spawned_admission",
            package: "gents",
            source_path: "crates/gents/src/hook/tests/background_admission.rs",
            module_path: "hook::tests::background_admission",
            function: "spawn_process_rejects_target_policy_before_spawned_admission",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::dispatch_receipt_scripts_bind_call_hook_under_both_persistence_policies",
            package: "gents",
            source_path: "crates/gents/src/hook/tests.rs",
            module_path: "hook::tests",
            function: "dispatch_receipt_scripts_bind_call_hook_under_both_persistence_policies",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::control_tool_completion_failure_never_reauthorizes_dispatch",
            package: "gents",
            source_path: "crates/gents/src/hook/tests.rs",
            module_path: "hook::tests",
            function: "control_tool_completion_failure_never_reauthorizes_dispatch",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::dispatch_receipt_loss_gates_real_hook_loop_invocation",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/tool_execution.rs",
            module_path: "agent::loop_stream::tests",
            function: "dispatch_receipt_loss_gates_real_hook_loop_invocation",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::policy_rejection_settles_pending_call_without_dispatch_election",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/tool_execution.rs",
            module_path: "agent::loop_stream::tests",
            function: "policy_rejection_settles_pending_call_without_dispatch_election",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::generated_interrupt_dispositions_drive_in_flight_interrupt",
            package: "gents",
            source_path: "crates/gents/src/hook/tests.rs",
            module_path: "hook::tests",
            function: "generated_interrupt_dispositions_drive_in_flight_interrupt",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::generated_persistence_failure_policy_cases_match_hook_decisions",
            package: "gents",
            source_path: "crates/gents/src/hook/tests.rs",
            module_path: "hook::tests",
            function: "generated_persistence_failure_policy_cases_match_hook_decisions",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::generated_storage_observation_cases_match_hook_runtime_classification",
            package: "gents",
            source_path: "crates/gents/src/hook/tests.rs",
            module_path: "hook::tests",
            function: "generated_storage_observation_cases_match_hook_runtime_classification",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::identity::resolved_identity_permission_cases_drive_defra_acp",
            package: "gents",
            source_path: "crates/gents/tests/conformance/identity.rs",
            module_path: "conformance::identity",
            function: "resolved_identity_permission_cases_drive_defra_acp",
        },
        ConformanceConsumer::RustTest {
            id: "http::identity_decide::tests::identity_decide_endpoint_matches_resolved_lean_permission_cases",
            package: "gents-cli",
            source_path: "crates/gents-cli/src/http/identity_decide.rs",
            module_path: "http::identity_decide::tests",
            function: "identity_decide_endpoint_matches_resolved_lean_permission_cases",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::tests::request_state_machine_contract_is_complete",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle.rs",
            module_path: "lifecycle::tests",
            function: "request_state_machine_contract_is_complete",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::tests::rust_execution_origin_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle.rs",
            module_path: "lifecycle::tests",
            function: "rust_execution_origin_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::tests::rust_request_lifecycle_state_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle.rs",
            module_path: "lifecycle::tests",
            function: "rust_request_lifecycle_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::session_timeline::session_snapshot_consumes_generated_live_overlay_cases",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/session_timeline.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::session_timeline",
            function: "session_snapshot_consumes_generated_live_overlay_cases",
        },
        ConformanceConsumer::RustTest {
            id: "mcp_pool::tests::list_tools_transport_failure_retries_generated_safe_read_case",
            package: "gents",
            source_path: "crates/gents/src/mcp_pool/tests.rs",
            module_path: "mcp_pool::tests",
            function: "list_tools_transport_failure_retries_generated_safe_read_case",
        },
        ConformanceConsumer::RustTest {
            id: "mcp_pool::tests::call_tool_transport_failure_obeys_generated_no_retry_cases_without_idempotency_metadata",
            package: "gents",
            source_path: "crates/gents/src/mcp_pool/tests.rs",
            module_path: "mcp_pool::tests",
            function: "call_tool_transport_failure_obeys_generated_no_retry_cases_without_idempotency_metadata",
        },
        ConformanceConsumer::RustTest {
            id: "managed_exec::tests::rust_managed_exec_state_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/managed_exec/tests.rs",
            module_path: "managed_exec::tests",
            function: "rust_managed_exec_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "managed_exec::tests::rust_process_stop_outcome_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/managed_exec/tests.rs",
            module_path: "managed_exec::tests",
            function: "rust_process_stop_outcome_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "managed_exec::tests::rust_cancel_process_reply_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/managed_exec/tests.rs",
            module_path: "managed_exec::tests",
            function: "rust_cancel_process_reply_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "managed_exec::tests::generated_process_stop_cases_drive_host_owner",
            package: "gents",
            source_path: "crates/gents/src/managed_exec/tests.rs",
            module_path: "managed_exec::tests",
            function: "generated_process_stop_cases_drive_host_owner",
        },
        ConformanceConsumer::RustTest {
            id: "managed_exec::tests::managed_exec_state_machine_contract_is_complete",
            package: "gents",
            source_path: "crates/gents/src/managed_exec/tests.rs",
            module_path: "managed_exec::tests",
            function: "managed_exec_state_machine_contract_is_complete",
        },
        ConformanceConsumer::RustTest {
            id: "pack::installation::tests::generated_plugin_reference_cases_validate_prepared_pack_resources",
            package: "gents",
            source_path: "crates/gents/src/pack/installation/tests.rs",
            module_path: "pack::installation::tests",
            function: "generated_plugin_reference_cases_validate_prepared_pack_resources",
        },
        ConformanceConsumer::RustTest {
            id: "agent::runtime::tests::startup_recovery::generated_snapshot_demotions_preserve_healthy_agents",
            package: "gents",
            source_path: "crates/gents/src/agent/runtime/tests/startup_recovery.rs",
            module_path: "agent::runtime::tests::startup_recovery",
            function: "generated_snapshot_demotions_preserve_healthy_agents",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::runtime_status_generation_updates_match_lean_runtime_reconcile_cases",
            package: "gents",
            source_path: "crates/gents/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "runtime_status_generation_updates_match_lean_runtime_reconcile_cases",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::generated_process_transition_cases_match_runtime_status_policy",
            package: "gents",
            source_path: "crates/gents/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "generated_process_transition_cases_match_runtime_status_policy",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::rust_process_state_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "rust_process_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::rust_reconcile_phase_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "rust_reconcile_phase_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "runtime_status::tests::runtime_reconcile_state_machine_contract_is_complete",
            package: "gents",
            source_path: "crates/gents/src/runtime_status/tests.rs",
            module_path: "runtime_status::tests",
            function: "runtime_reconcile_state_machine_contract_is_complete",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_request_transition_cases_cover_lifecycle_policy",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_request_transition_cases_cover_lifecycle_policy",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_queue_deadline_cases_pin_r4a_contract_rows",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_queue_deadline_cases_pin_r4a_contract_rows",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_recovery_sweep_cases_drive_startup_recovery_contract",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_recovery_sweep_cases_drive_startup_recovery_contract",
        },
        ConformanceConsumer::RustTest {
            id: "agent::daemon::inference::tests::generated_retry_selection_cases_bind_to_the_daemon",
            package: "gents",
            source_path: "crates/gents/src/agent/daemon/fold_checkpoint_reclaim_tests.rs",
            module_path: "agent::daemon::inference::tests",
            function: "generated_retry_selection_cases_bind_to_the_daemon",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::tests::folding::generated_fold_publication_scripts_bind_to_native_owners",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/tests/folding.rs",
            module_path: "lifecycle::queue::tests::folding",
            function: "generated_fold_publication_scripts_bind_to_native_owners",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::tests::folding::generated_handover_fold_claims_bind_to_native_claims",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/tests/folding.rs",
            module_path: "lifecycle::queue::tests::folding",
            function: "generated_handover_fold_claims_bind_to_native_claims",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::folded_prompts_publish_and_send_in_queue_order",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/request_assembly.rs",
            module_path: "agent::loop_stream::tests",
            function: "folded_prompts_publish_and_send_in_queue_order",
        },
        ConformanceConsumer::RustTest {
            id: "lifecycle::queue::tests::folding::generated_fold_queue_cases_bind_to_native_claims",
            package: "gents",
            source_path: "crates/gents/src/request/lifecycle/queue/tests/folding.rs",
            module_path: "lifecycle::queue::tests::folding",
            function: "generated_fold_queue_cases_bind_to_native_claims",
        },
        ConformanceConsumer::RustTest {
            id: "interrupt::queue_tests::generated_interrupt_queue_events_bind_to_native_rows",
            package: "gents",
            source_path: "crates/gents/src/request/interrupt/queue_tests.rs",
            module_path: "interrupt::queue_tests",
            function: "generated_interrupt_queue_events_bind_to_native_rows",
        },
        ConformanceConsumer::RustTest {
            id: "interrupt::queue_tests::generated_http_overlap_cases_preserve_cutoff",
            package: "gents",
            source_path: "crates/gents/src/request/interrupt/queue_tests.rs",
            module_path: "interrupt::queue_tests",
            function: "generated_http_overlap_cases_preserve_cutoff",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::recovery_conformance::generated_native_restart_dispositions_use_canonical_admission_owner",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/recovery_conformance.rs",
            module_path: "tool_call_lifecycle::recovery_conformance",
            function: "generated_native_restart_dispositions_use_canonical_admission_owner",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::recovery_conformance::generated_session_message_restart_dispositions_use_canonical_admission_owner",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/recovery_conformance.rs",
            module_path: "tool_call_lifecycle::recovery_conformance",
            function: "generated_session_message_restart_dispositions_use_canonical_admission_owner",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::recovery_closeout_conformance::generated_native_missing_parent_restart_cases_defer",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/recovery_closeout_conformance.rs",
            module_path: "tool_call_lifecycle::recovery_closeout_conformance",
            function: "generated_native_missing_parent_restart_cases_defer",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::recovery_conformance::generated_native_recovery_cases_use_canonical_admission_owner",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/recovery_conformance.rs",
            module_path: "tool_call_lifecycle::recovery_conformance",
            function: "generated_native_recovery_cases_use_canonical_admission_owner",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::recovery_closeout_conformance::generated_session_message_recovery_cases_use_accepted_call",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/recovery_closeout_conformance.rs",
            module_path: "tool_call_lifecycle::recovery_closeout_conformance",
            function: "generated_session_message_recovery_cases_use_accepted_call",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::recovery_closeout_conformance::generated_orphan_background_recovery_cases_use_accepted_native_call",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/recovery_closeout_conformance.rs",
            module_path: "tool_call_lifecycle::recovery_closeout_conformance",
            function: "generated_orphan_background_recovery_cases_use_accepted_native_call",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::recovery_closeout_conformance::generated_missing_parent_deferred_cases_keep_accepted_row_running",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/recovery_closeout_conformance.rs",
            module_path: "tool_call_lifecycle::recovery_closeout_conformance",
            function: "generated_missing_parent_deferred_cases_keep_accepted_row_running",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::recovery_closeout_conformance::generated_background_completion_recovery_uses_accepted_native_call",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/recovery_closeout_conformance.rs",
            module_path: "tool_call_lifecycle::recovery_closeout_conformance",
            function: "generated_background_completion_recovery_uses_accepted_native_call",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::completion_owner_conformance::generated_r6_notification_precedes_continuation_claim",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/completion_owner_conformance.rs",
            module_path: "tool_call_lifecycle::completion_owner_conformance",
            function: "generated_r6_notification_precedes_continuation_claim",
        },
        ConformanceConsumer::RustTest {
            id: "background_tools::tests::generated_tool_output_paging_cases_match_slice_function",
            package: "gents",
            source_path: "crates/gents/src/background_tools.rs",
            module_path: "background_tools::tests",
            function: "generated_tool_output_paging_cases_match_slice_function",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::background_hook_conformance::generated_absent_requester_process_control_uses_accepted_hook_calls",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/background_hook_conformance.rs",
            module_path: "tool_call_lifecycle::background_hook_conformance",
            function: "generated_absent_requester_process_control_uses_accepted_hook_calls",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::completion_owner_conformance::generated_r6_completion_owner_cases_use_accepted_native_output",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/completion_owner_conformance.rs",
            module_path: "tool_call_lifecycle::completion_owner_conformance",
            function: "generated_r6_completion_owner_cases_use_accepted_native_output",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::composed_conformance::generated_composed_invariants_use_canonical_admission_owner",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle/composed_conformance.rs",
            module_path: "tool_call_lifecycle::composed_conformance",
            function: "generated_composed_invariants_use_canonical_admission_owner",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::background_budget::generated_background_budget_uses_accepted_dispatch",
            package: "gents",
            source_path: "crates/gents/src/hook/tests/background_budget.rs",
            module_path: "hook::tests::background_budget",
            function: "generated_background_budget_uses_accepted_dispatch",
        },
        ConformanceConsumer::RustTest {
            id: "hook::tests::process_control_scope::generated_wait_observer_interrupt_preserves_background_process",
            package: "gents",
            source_path: "crates/gents/src/hook/tests/process_control_scope.rs",
            module_path: "hook::tests::process_control_scope",
            function: "generated_wait_observer_interrupt_preserves_background_process",
        },
        ConformanceConsumer::RustTest {
            id: "e2e_background::r6_background_tools::wait_tool_caller_interrupt_returns_without_cancelling_background_row",
            package: "gents",
            source_path: "crates/gents/tests/e2e_background/r6_background_tools.rs",
            module_path: "e2e_background::r6_background_tools",
            function: "wait_tool_caller_interrupt_returns_without_cancelling_background_row",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_codex_shim_projection_cases_pin_adapter_mapping",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_codex_shim_projection_cases_pin_adapter_mapping",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_codex_shim_binding_cases_pin_runnable_gated_binding",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_codex_shim_binding_cases_pin_runnable_gated_binding",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::generated_startup_readiness_cases_pin_bounded_barrier_release",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "generated_startup_readiness_cases_pin_bounded_barrier_release",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_sanitize_cases_drive_the_production_sanitizer",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_sanitize_cases_drive_the_production_sanitizer",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_budget_cases_drive_dynamic_output_compaction_trigger",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/budgeting.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_budget_cases_drive_dynamic_output_compaction_trigger",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_turn_budget_cases_drive_every_completion_dispatch",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/budgeting.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_turn_budget_cases_drive_every_completion_dispatch",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_compaction_trigger_cases_drive_the_production_trigger",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/budgeting.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_compaction_trigger_cases_drive_the_production_trigger",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_retention_cases_drive_production_compaction_target",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/budgeting.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_retention_cases_drive_production_compaction_target",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_claude_map_cases_drive_the_messages_parser",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_claude_map_cases_drive_the_messages_parser",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_claude_body_cases_drive_the_body_builder",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_claude_body_cases_drive_the_body_builder",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_claude_stream_cases_drive_the_messages_parser",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_claude_stream_cases_drive_the_messages_parser",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_claude_thinking_stream_cases_drive_native_sse_parser",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_claude_thinking_stream_cases_drive_native_sse_parser",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_claude_initial_thinking_text_seals_in_native_accumulator",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_claude_initial_thinking_text_seals_in_native_accumulator",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_claude_replay_cases_drive_native_messages_body",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_claude_replay_cases_drive_native_messages_body",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_reasoning_suffix_cases_bind_native_selection",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_reasoning_suffix_cases_bind_native_selection",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_replay_shape_cases_bind_source_index_projection",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_replay_shape_cases_bind_source_index_projection",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_replay_prefix_cases_bind_acceptance_checks",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_replay_prefix_cases_bind_acceptance_checks",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_claude_checkpoint_cases_bind_selected_assistant_projection",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_claude_checkpoint_cases_bind_selected_assistant_projection",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_assistant_order_cases_bind_selected_provider_order",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_assistant_order_cases_bind_selected_provider_order",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::prompt_assembly::generated_mode_sanitize_cases_bind_composed_provider_view",
            package: "gents",
            source_path: "crates/gents/tests/conformance/prompt_assembly.rs",
            module_path: "conformance::prompt_assembly",
            function: "generated_mode_sanitize_cases_bind_composed_provider_view",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_rendered_capture_cases_hold_against_the_real_defra_sink",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/capture.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_rendered_capture_cases_hold_against_the_real_defra_sink",
        },
        ConformanceConsumer::RustTest {
            id: "completion_factory::tests::rehydrates_aggregate_budget_from_durable_inference_calls",
            package: "gents",
            source_path: "crates/gents/src/completion_factory/tests.rs",
            module_path: "completion_factory::tests",
            function: "rehydrates_aggregate_budget_from_durable_inference_calls",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_rendered_capture_cases_fence_persist_before_send",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/capture.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_rendered_capture_cases_fence_persist_before_send",
        },
        ConformanceConsumer::RustTest {
            id: "provider_context_reduction::durable_reduction_conformance::generated_durable_reduction_cases_pin_storage_and_capture_citations",
            package: "gents",
            source_path: "crates/gents/tests/conformance/durable_reduction.rs",
            module_path: "provider_context_reduction::durable_reduction_conformance",
            function: "generated_durable_reduction_cases_pin_storage_and_capture_citations",
        },
        ConformanceConsumer::RustTest {
            id: "compaction::tests::generated_rolling_cases_drive_the_production_commit_precondition",
            package: "gents",
            source_path: "crates/gents/src/inference/compaction/tests.rs",
            module_path: "compaction::tests",
            function: "generated_rolling_cases_drive_the_production_commit_precondition",
        },
        ConformanceConsumer::RustTest {
            id: "compaction::tests::generated_reduction_engine_cases_drive_shared_decision_outcome",
            package: "gents",
            source_path: "crates/gents/src/inference/compaction/tests.rs",
            module_path: "compaction::tests",
            function: "generated_reduction_engine_cases_drive_shared_decision_outcome",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::rendered_capture::generated_rendered_capture_key_cases_pin_the_capture_key_tuple",
            package: "gents",
            source_path: "crates/gents/tests/conformance/rendered_capture.rs",
            module_path: "conformance::rendered_capture",
            function: "generated_rendered_capture_key_cases_pin_the_capture_key_tuple",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::rendered_capture::generated_capture_scope_cases_pin_the_shared_parser_and_order",
            package: "gents",
            source_path: "crates/gents/tests/conformance/rendered_capture.rs",
            module_path: "conformance::rendered_capture",
            function: "generated_capture_scope_cases_pin_the_shared_parser_and_order",
        },
        ConformanceConsumer::RustTest {
            id: "cli_trace_export::trace_capture_fetches_metadata_with_field_commit_cid",
            package: "gents-cli",
            source_path: "crates/gents-cli/tests/suites/cli_trace_export.rs",
            module_path: "cli_trace_export",
            function: "trace_capture_fetches_metadata_with_field_commit_cid",
        },
        ConformanceConsumer::TypeScriptTest {
            id: "apps/gents-desktop/tests/request-trace.test.tsx::request trace panel renders the reconstructed event stream",
            app: "gents-desktop",
            source_path: "apps/gents-desktop/tests/request-trace.test.tsx",
            suite: "request trace panel",
            test: "renders the reconstructed event stream",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_layer_cases_pin_the_assembled_request_order",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/request_assembly.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_layer_cases_pin_the_assembled_request_order",
        },
        ConformanceConsumer::RustTest {
            id: "agent::loop_stream::tests::generated_repair_cases_drive_tool_argument_repair",
            package: "gents",
            source_path: "crates/gents/src/agent/loop_stream/tests/request_assembly.rs",
            module_path: "agent::loop_stream::tests",
            function: "generated_repair_cases_drive_tool_argument_repair",
        },
        ConformanceConsumer::RustTest {
            id: "meta_tools::call::tests::generated_tool_preflight_cases_match_health_and_schema_gates",
            package: "gents",
            source_path: "crates/gents/src/meta_tools/call.rs",
            module_path: "meta_tools::call::tests",
            function: "generated_tool_preflight_cases_match_health_and_schema_gates",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::lean_executable_contracts_cover_initial_domains",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "lean_executable_contracts_cover_initial_domains",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::pairing_reconcile_shutdown_boundary_preempts_in_flight_sweep",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "pairing_reconcile_shutdown_boundary_preempts_in_flight_sweep",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::pairing_reconcile_top_level_sweep_failure_is_nonterminal_and_retried",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "pairing_reconcile_top_level_sweep_failure_is_nonterminal_and_retried",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::pairing_reconcile_sweep_does_not_head_of_line_block_ready_peer",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "pairing_reconcile_sweep_does_not_head_of_line_block_ready_peer",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::tool_call::lean_emits_await_mode_vocabulary",
            package: "gents",
            source_path: "crates/gents/tests/conformance/tool_call.rs",
            module_path: "conformance::tool_call",
            function: "lean_emits_await_mode_vocabulary",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::pairing_reconcile::generated_pairing_reconcile_cases_drive_production_projector",
            package: "gents",
            source_path: "crates/gents/tests/conformance/pairing_reconcile.rs",
            module_path: "conformance::pairing_reconcile",
            function: "generated_pairing_reconcile_cases_drive_production_projector",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::event_delivery_transition_cases_match_contract",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "event_delivery_transition_cases_match_contract",
        },
        ConformanceConsumer::RustTest {
            id: "watcher::tests::generated_release_cases_preserve_native_session_order",
            package: "gents",
            source_path: "crates/gents/src/request/watcher/tests.rs",
            module_path: "watcher::tests",
            function: "generated_release_cases_preserve_native_session_order",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::tests::event_source::generated_sibling_delivery_case_preserves_pending_correlation",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/tests/event_source.rs",
            module_path: "triggers::engine::tests::event_source",
            function: "generated_sibling_delivery_case_preserves_pending_correlation",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::event_delivery_source_instances_match_runtime",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "event_delivery_source_instances_match_runtime",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::event_delivery_convergence_traces_match_runtime_or_deviation",
            package: "gents",
            source_path: "crates/gents/tests/conformance.rs",
            module_path: "conformance",
            function: "event_delivery_convergence_traces_match_runtime_or_deviation",
        },
        ConformanceConsumer::RustTest {
            id: "prompt::tests::mcp_discovery_guidance_matches_presented_operations_model",
            package: "gents",
            source_path: "crates/gents/src/prompt/tests.rs",
            module_path: "prompt::tests",
            function: "mcp_discovery_guidance_matches_presented_operations_model",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_command_env_cases_match_rust_filtering",
            package: "gents",
            source_path: "crates/gents/src/toolset/tests.rs",
            module_path: "toolset::tests",
            function: "generated_command_env_cases_match_rust_filtering",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_command_policy_cases_match_rust_validation",
            package: "gents",
            source_path: "crates/gents/src/toolset/tests.rs",
            module_path: "toolset::tests",
            function: "generated_command_policy_cases_match_rust_validation",
        },
        ConformanceConsumer::RustTest {
            id: "toolset::tests::generated_command_sandbox_cases_match_rust_selection",
            package: "gents",
            source_path: "crates/gents/src/toolset/tests.rs",
            module_path: "toolset::tests",
            function: "generated_command_sandbox_cases_match_rust_selection",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::engine::tests::trigger_engine_dispatch_matches_lean_generated_contract_cases",
            package: "gents",
            source_path: "crates/gents/src/triggers/engine/tests/mod.rs",
            module_path: "triggers::engine::tests",
            function: "trigger_engine_dispatch_matches_lean_generated_contract_cases",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::tests::rust_tool_call_state_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle.rs",
            module_path: "tool_call_lifecycle::tests",
            function: "rust_tool_call_state_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::tests::rust_cancel_cause_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle.rs",
            module_path: "tool_call_lifecycle::tests",
            function: "rust_cancel_cause_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::tests::rust_failure_class_vocabulary_matches_lean_model",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle.rs",
            module_path: "tool_call_lifecycle::tests",
            function: "rust_failure_class_vocabulary_matches_lean_model",
        },
        ConformanceConsumer::RustTest {
            id: "tool_call_lifecycle::tests::tool_call_state_machine_contract_is_complete",
            package: "gents",
            source_path: "crates/gents/src/tool_call_lifecycle.rs",
            module_path: "tool_call_lifecycle::tests",
            function: "tool_call_state_machine_contract_is_complete",
        },
        ConformanceConsumer::RustTest {
            id: "health_checker::tests::generated_mcp_health_cases_match_health_checker_transitions",
            package: "gents",
            source_path: "crates/gents/src/health_checker.rs",
            module_path: "health_checker::tests",
            function: "generated_mcp_health_cases_match_health_checker_transitions",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::tests::operations_interrupt::interrupt_request_reaches_only_the_named_request",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/tests/operations_interrupt.rs",
            module_path: "gents_desktop_bridge::tests::operations_interrupt",
            function: "interrupt_request_reaches_only_the_named_request",
        },
        ConformanceConsumer::RustTest {
            id: "gents_desktop_bridge::snapshot::tests::session_state::session_snapshot_derives_cancel_causes_from_request_and_tool",
            package: "gents-desktop-bridge",
            source_path: "crates/gents-desktop-bridge/src/snapshot/tests/session_state.rs",
            module_path: "gents_desktop_bridge::snapshot::tests::session_state",
            function: "session_snapshot_derives_cancel_causes_from_request_and_tool",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::callback_lifecycle::generated_callback_journals_match_runtime_owner",
            package: "gents",
            source_path: "crates/gents/tests/conformance/callback_lifecycle.rs",
            module_path: "conformance::callback_lifecycle",
            function: "generated_callback_journals_match_runtime_owner",
        },
        ConformanceConsumer::RustTest {
            id: "conformance::callback_lifecycle::generated_retry_decisions_match_runtime_owner",
            package: "gents",
            source_path: "crates/gents/tests/conformance/callback_lifecycle.rs",
            module_path: "conformance::callback_lifecycle",
            function: "generated_retry_decisions_match_runtime_owner",
        },
        ConformanceConsumer::RustTest {
            id: "triggers::callback::tests::generated_recovery_and_denial_match_runtime_owners",
            package: "gents",
            source_path: "crates/gents/src/triggers/callback/tests.rs",
            module_path: "triggers::callback::tests",
            function: "generated_recovery_and_denial_match_runtime_owners",
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
        .find(|path| path.join("crates/gents/proofs/lakefile.lean").exists())
        .expect("repository root should contain crates/gents/proofs/lakefile.lean")
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
