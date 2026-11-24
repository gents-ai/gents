use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use gents_pack::pack::core_packs::CorePack;
use gents_pack::pack::AGENT_SLOT_REFERENCE_PREFIX;
use gents_protocol::tool_names::{
    AGENT_TOOL_NAMES, META_TOOL_NAMES, NATIVE_TOOL_NAMES, SELF_CONFIG_TOOL_NAMES,
};
use serde_json::Value;

use super::registry::{definition, fixtures, repo, suites, Suite};
use super::rules::{findings, Rule, Subject};

const README: &str = "crates/gents/tests/fixtures/configurator_evals/README.md";
const CELL_AGENT: &str = "engineer";
const ENGINEER_CONTEXT_ID: &str = "engineer-context";
const ENGINEER_TOOLS_ID: &str = "engineer-tools";

#[rustfmt::skip]
const ENGINEER_PIN: &[(&str, usize)] = &[];
#[rustfmt::skip]
const LONG_PROMPT: &[(&str, usize)] = &[
    ("engineer_automation_graphs/cases/bay_thread_per_key.json", 1),
    ("engineer_automation_graphs/cases/cold_packing_chain.json", 1),
    ("engineer_automation_graphs/cases/declared_quorum.json", 1),
    ("engineer_automation_graphs/cases/desk_ticket_routing.json", 1),
    ("engineer_automation_graphs/cases/dock_alert_triage.json", 1),
    ("engineer_automation_graphs/cases/inspection_band.json", 1),
    ("engineer_automation_graphs/cases/linen_proof_preview.json", 1),
    ("engineer_automation_graphs/cases/paired_crate_scans.json", 1),
    ("engineer_automation_graphs/cases/proof_slip_run.json", 1),
    ("engineer_automation_graphs/cases/redline_follow_up.json", 1),
    ("engineer_automation_graphs/cases/scale_card_repair.json", 2),
    ("engineer_automation_graphs/cases/yard_receipts.json", 1),
    ("engineer_goals/cases/blocked_manifest.json", 1),
    ("engineer_goals/cases/crate_budget_raise.json", 1),
    ("engineer_goals/cases/narrow_report_objective.json", 1),
    ("engineer_goals/cases/pallet_totals_goal.json", 1),
    ("engineer_goals/cases/points_partial_blocked.json", 1),
    ("engineer_goals/cases/queue_one_per_turn.json", 1),
    ("engineer_goals/cases/shelf_budget_raise.json", 1),
    ("engineer_host_files/cases/beacon_live_minutes.json", 1),
    ("engineer_host_files/cases/berth_overlap_pairs.json", 1),
    ("engineer_host_files/cases/change_request_apply.json", 1),
    ("engineer_host_files/cases/dispatch_queue_merge.json", 1),
    ("engineer_host_files/cases/heron_current_decisions.json", 1),
    ("engineer_host_files/cases/pickup_shortfall.json", 1),
    ("engineer_host_files/cases/retry_callers_audit.json", 1),
    ("engineer_mailbox/cases/blocked_order_escalation.json", 1),
    ("engineer_mailbox/cases/boiler_new_breach.json", 2),
    ("engineer_mailbox/cases/clinic_seating_shortage.json", 1),
    ("engineer_mailbox/cases/freezer_breach_sustained.json", 2),
    ("engineer_mailbox/cases/kiln_flag_and_question.json", 1),
    ("engineer_mailbox/cases/tank_two_low_readings.json", 1),
    ("engineer_pack_authoring/cases/calibration_copies_audit.json", 1),
    ("engineer_pack_authoring/cases/kiln_runbook_missing_skill.json", 1),
    ("engineer_pack_authoring/cases/parts_desk_app_pack.json", 1),
    ("engineer_pack_authoring/cases/stockroom_register_pack.json", 1),
    ("engineer_pack_authoring/cases/tide_notes_version_bump.json", 1),
    ("engineer_pages/cases/add_chart_keep_css.json", 1),
    ("engineer_pages/cases/clear_lint_finding.json", 1),
    ("engineer_pages/cases/fix_stale_calibrations.json", 1),
    ("engineer_pages/cases/harbor_arrivals_page.json", 1),
    ("engineer_pages/cases/refund_aging.json", 1),
    ("engineer_pages/cases/rename_read_tool.json", 1),
    ("engineer_pages/cases/restore_my_holdings.json", 1),
    ("engineer_pages/cases/rig_flow_scatter.json", 1),
    ("engineer_pages/cases/ticket_board_page.json", 1),
    ("engineer_script/cases/cold_bin_notices.json", 1),
    ("engineer_script/cases/dock_totals_file.json", 1),
    ("engineer_script/cases/freight_pagination.json", 1),
    ("engineer_script/cases/lab_lock_audit.json", 1),
    ("engineer_script/cases/manifest_overlap.json", 1),
    ("engineer_script/cases/pickup_reconcile.json", 1),
    ("engineer_script/cases/relay_precedence.json", 1),
    ("engineer_script/cases/sensor_recovery.json", 1),
    ("engineer_script/cases/ticket_bulk_close.json", 1),
    ("engineer_script/cases/wet_lots_audit.json", 1),
    ("engineer_skills/cases/plan_before_build.json", 1),
    ("engineer_skills/cases/plugin_tool_missing.json", 1),
    ("engineer_skills/cases/trigger_never_fires.json", 1),
    ("engineer_workflows/cases/calibration_review.json", 1),
    ("engineer_workflows/cases/late_desk_escalation.json", 1),
    ("engineer_workflows/cases/reefer_tally_fan_in.json", 1),
    ("engineer_workflows/cases/west_loading_reconcile.json", 1),
    ("ladder/l1_inference/cases/val_stream_settings.json", 1),
    ("ladder/l2_agent/cases/analyst_grants_file_tools.json", 1),
    ("ladder/l2_agent/cases/clerk_automation_seed_fire.json", 1),
    ("ladder/l2_agent/cases/cursor_grants_list_only.json", 1),
    ("ladder/l2_agent/cases/restore_desk_instructions.json", 2),
    ("ladder/l2_agent/cases/scout_grants_two_file_tools.json", 1),
    ("ladder/l3_datastore/cases/held_ds_surface_on_new_agent.json", 1),
    ("ladder/l3_datastore/cases/train_ds_board_desk_reader.json", 1),
    ("ladder/l3_datastore/cases/train_ds_parcel_scan_surface.json", 1),
    ("ladder/l3_datastore/cases/train_ds_schema_surface_select_call.json", 1),
    ("ladder/l3_datastore/cases/val_ds_automation_seed_to_output.json", 1),
    ("ladder/l3_datastore/cases/val_ds_baseline.json", 1),
    ("ladder/l3_datastore/cases/val_ds_create_only_surface.json", 1),
    ("ladder/l3_datastore/cases/val_ds_finish_partial_setup.json", 1),
    ("ladder/l3_datastore/cases/val_ds_row_persists_across_stages.json", 1),
    ("ladder/l3_datastore/cases/val_ds_two_collection_surface.json", 1),
    ("ladder/l4_automation/cases/completion_record_write_contract.json", 1),
    ("ladder/l4_automation/cases/default_filter_missing_field.json", 1),
    ("ladder/l4_automation/cases/parallel_base.json", 1),
    ("ladder/l4_automation/cases/parallel_two_inputs.json", 1),
    ("ladder/l4_automation/cases/queued_serial_two_inputs.json", 1),
    ("ladder/l4_automation/cases/serial_session_combo.json", 1),
    ("ladder/l4_automation/cases/session_template_routing.json", 1),
    ("ladder/l4_automation/cases/train_parallel_baseline.json", 1),
    ("ladder/l4_automation/cases/val_stop_fern_receipt_outcomes.json", 1),
    ("ladder/l5_agents_tools/cases/heldout_mixed_valid_and_ghost.json", 1),
];
#[rustfmt::skip]
const PROMPT_NAMES_TOOL: &[(&str, usize)] = &[
    ("engineer_automation_graphs/cases/linen_proof_preview.json", 1),
    ("engineer_context/cases/rotated_log_last_event.json", 1),
    ("engineer_pages/cases/add_chart_keep_css.json", 1),
    ("engineer_pages/cases/clear_lint_finding.json", 1),
    ("engineer_pages/cases/fix_stale_calibrations.json", 1),
    ("engineer_pages/cases/rename_read_tool.json", 1),
    ("engineer_pages/cases/restore_my_holdings.json", 1),
    ("engineer_script/cases/freight_pagination.json", 1),
    ("engineer_script/cases/wet_lots_audit.json", 1),
];
#[rustfmt::skip]
const ROUTE: &[(&str, usize)] = &[
    ("engineer_automation_graphs/cases/bay_thread_per_key.json", 5),
    ("engineer_automation_graphs/cases/cold_packing_chain.json", 5),
    ("engineer_automation_graphs/cases/declared_quorum.json", 5),
    ("engineer_automation_graphs/cases/desk_ticket_routing.json", 5),
    ("engineer_automation_graphs/cases/dock_alert_triage.json", 5),
    ("engineer_automation_graphs/cases/inspection_band.json", 5),
    ("engineer_automation_graphs/cases/linen_proof_preview.json", 10),
    ("engineer_automation_graphs/cases/paired_crate_scans.json", 5),
    ("engineer_automation_graphs/cases/proof_slip_run.json", 5),
    ("engineer_automation_graphs/cases/redline_follow_up.json", 5),
    ("engineer_automation_graphs/cases/scale_card_repair.json", 10),
    ("engineer_automation_graphs/cases/yard_receipts.json", 5),
    ("engineer_context/cases/budget_headroom.json", 5),
    ("engineer_context/cases/cleared_handover_recovery.json", 4),
    ("engineer_context/cases/cleared_manifest_recovery.json", 4),
    ("engineer_context/cases/manual_final_step.json", 4),
    ("engineer_context/cases/rotated_log_last_event.json", 4),
    ("engineer_context/cases/route_fare_total.json", 4),
    ("engineer_context/cases/slimmed_rotation_recovery.json", 4),
    ("engineer_context/cases/trimmed_outputs_report.json", 9),
    ("engineer_goals/cases/blocked_manifest.json", 4),
    ("engineer_goals/cases/crate_budget_raise.json", 3),
    ("engineer_goals/cases/greenhouse_water_goal.json", 3),
    ("engineer_goals/cases/narrow_report_objective.json", 3),
    ("engineer_goals/cases/no_goal_check.json", 8),
    ("engineer_goals/cases/no_goal_needed.json", 7),
    ("engineer_goals/cases/pallet_totals_goal.json", 3),
    ("engineer_goals/cases/points_partial_blocked.json", 4),
    ("engineer_goals/cases/queue_one_per_turn.json", 3),
    ("engineer_goals/cases/shelf_budget_raise.json", 3),
    ("engineer_goals/cases/tags_already_owned.json", 3),
    ("engineer_host_files/cases/beacon_live_minutes.json", 2),
    ("engineer_host_files/cases/berth_overlap_pairs.json", 2),
    ("engineer_host_files/cases/change_request_apply.json", 2),
    ("engineer_host_files/cases/chiller_alarm_edit.json", 2),
    ("engineer_host_files/cases/crew_lists_split.json", 2),
    ("engineer_host_files/cases/dispatch_queue_merge.json", 2),
    ("engineer_host_files/cases/heron_current_decisions.json", 2),
    ("engineer_host_files/cases/pickup_shortfall.json", 4),
    ("engineer_host_files/cases/product_rename_prose.json", 2),
    ("engineer_host_files/cases/pump_error_tally.json", 2),
    ("engineer_host_files/cases/retry_callers_audit.json", 2),
    ("engineer_host_files/cases/station_renumber.json", 2),
    ("engineer_mailbox/cases/binder_next_shift.json", 6),
    ("engineer_mailbox/cases/blocked_order_escalation.json", 3),
    ("engineer_mailbox/cases/boiler_new_breach.json", 12),
    ("engineer_mailbox/cases/clinic_seating_shortage.json", 6),
    ("engineer_mailbox/cases/cooler_boundary_silent.json", 6),
    ("engineer_mailbox/cases/dock_slot_question.json", 6),
    ("engineer_mailbox/cases/door_interlock_reminder.json", 12),
    ("engineer_mailbox/cases/ferry_crew_swap_question.json", 6),
    ("engineer_mailbox/cases/freezer_breach_sustained.json", 11),
    ("engineer_mailbox/cases/invoice_chat_only.json", 6),
    ("engineer_mailbox/cases/kiln_flag_and_question.json", 6),
    ("engineer_mailbox/cases/tank_two_low_readings.json", 23),
    ("engineer_pack_authoring/cases/berth_watch_preview.json", 5),
    ("engineer_pack_authoring/cases/calibration_copies_audit.json", 5),
    ("engineer_pack_authoring/cases/courier_zones_check_clean.json", 3),
    ("engineer_pack_authoring/cases/escalation_surface_export.json", 3),
    ("engineer_pack_authoring/cases/handbook_manifest_repair.json", 3),
    ("engineer_pack_authoring/cases/kiln_runbook_missing_skill.json", 3),
    ("engineer_pack_authoring/cases/pallet_pack_remove.json", 6),
    ("engineer_pack_authoring/cases/parts_desk_app_pack.json", 3),
    ("engineer_pack_authoring/cases/stockroom_register_pack.json", 3),
    ("engineer_pack_authoring/cases/tide_notes_version_bump.json", 6),
    ("engineer_pages/cases/add_chart_keep_css.json", 2),
    ("engineer_pages/cases/clear_lint_finding.json", 2),
    ("engineer_pages/cases/dock_totals_bar.json", 1),
    ("engineer_pages/cases/fix_stale_calibrations.json", 2),
    ("engineer_pages/cases/harbor_arrivals_page.json", 1),
    ("engineer_pages/cases/refund_aging.json", 1),
    ("engineer_pages/cases/rename_read_tool.json", 2),
    ("engineer_pages/cases/restore_my_holdings.json", 2),
    ("engineer_pages/cases/rig_flow_scatter.json", 1),
    ("engineer_pages/cases/ticket_board_page.json", 1),
    ("engineer_pages/cases/tide_line.json", 1),
    ("engineer_pages/cases/unbilled_parcels.json", 1),
    ("engineer_script/cases/cold_bin_notices.json", 7),
    ("engineer_script/cases/dock_totals_file.json", 5),
    ("engineer_script/cases/freight_pagination.json", 7),
    ("engineer_script/cases/lab_lock_audit.json", 6),
    ("engineer_script/cases/manifest_overlap.json", 6),
    ("engineer_script/cases/pickup_reconcile.json", 6),
    ("engineer_script/cases/relay_precedence.json", 7),
    ("engineer_script/cases/retired_bins.json", 7),
    ("engineer_script/cases/sensor_recovery.json", 6),
    ("engineer_script/cases/ticket_bulk_close.json", 3),
    ("engineer_script/cases/wet_lots_audit.json", 7),
    ("engineer_skills/cases/archive_retention_records.json", 6),
    ("engineer_skills/cases/courier_eta_no_skill.json", 6),
    ("engineer_skills/cases/courier_ticket_collection.json", 4),
    ("engineer_skills/cases/courier_window_tickets.json", 6),
    ("engineer_skills/cases/ferry_reimbursement_claims.json", 6),
    ("engineer_skills/cases/lab_holding_periods.json", 6),
    ("engineer_skills/cases/plan_before_build.json", 3),
    ("engineer_skills/cases/plugin_tool_missing.json", 6),
    ("engineer_skills/cases/shift_minutes_no_skill.json", 6),
    ("engineer_skills/cases/trigger_never_fires.json", 6),
    ("engineer_skills/cases/urgent_count_once.json", 6),
    ("engineer_workflows/cases/calibration_review.json", 2),
    ("engineer_workflows/cases/freeze_check.json", 2),
    ("engineer_workflows/cases/gate_count_recall.json", 4),
    ("engineer_workflows/cases/harbor_read.json", 2),
    ("engineer_workflows/cases/harbor_sum.json", 2),
    ("engineer_workflows/cases/late_desk_escalation.json", 2),
    ("engineer_workflows/cases/mail_count.json", 2),
    ("engineer_workflows/cases/pier_sums.json", 2),
    ("engineer_workflows/cases/reefer_tally_fan_in.json", 2),
    ("engineer_workflows/cases/west_loading_reconcile.json", 2),
    ("ladder/l1_inference/cases/held_nightly_automation_reviewer.json", 4),
    ("ladder/l1_inference/cases/train_basic_exec_profile_bind.json", 4),
    ("ladder/l1_inference/cases/val_minimal_limits.json", 4),
    ("ladder/l1_inference/cases/val_moderate_limits_reuse_backend.json", 4),
    ("ladder/l1_inference/cases/val_new_agent_reviewer.json", 4),
    ("ladder/l1_inference/cases/val_reasoning_effort_high.json", 4),
    ("ladder/l1_inference/cases/val_stream_settings.json", 4),
    ("ladder/l1_inference/cases/val_trap_preserve_default_profile.json", 4),
    ("ladder/l2_agent/cases/restore_desk_instructions.json", 5),
    ("ladder/l3_datastore/cases/held_ds_surface_on_new_agent.json", 4),
    ("ladder/l3_datastore/cases/train_ds_board_desk_reader.json", 4),
    ("ladder/l3_datastore/cases/train_ds_parcel_scan_surface.json", 4),
    ("ladder/l3_datastore/cases/train_ds_schema_surface_select_call.json", 4),
    ("ladder/l3_datastore/cases/val_ds_automation_seed_to_output.json", 2),
    ("ladder/l3_datastore/cases/val_ds_baseline.json", 4),
    ("ladder/l3_datastore/cases/val_ds_create_only_surface.json", 4),
    ("ladder/l3_datastore/cases/val_ds_finish_partial_setup.json", 6),
    ("ladder/l3_datastore/cases/val_ds_row_persists_across_stages.json", 6),
    ("ladder/l3_datastore/cases/val_ds_two_collection_surface.json", 6),
    ("ladder/l4_automation/cases/completion_record_write_contract.json", 2),
    ("ladder/l4_automation/cases/default_filter_missing_field.json", 2),
    ("ladder/l4_automation/cases/emit_outcome_false.json", 2),
    ("ladder/l4_automation/cases/held_interval_kiln_watch_pulse.json", 2),
    ("ladder/l4_automation/cases/held_pause_sundial_weekday_checklist.json", 4),
    ("ladder/l4_automation/cases/parallel_base.json", 2),
    ("ladder/l4_automation/cases/parallel_two_inputs.json", 2),
    ("ladder/l4_automation/cases/queued_serial_two_inputs.json", 2),
    ("ladder/l4_automation/cases/serial_session_combo.json", 2),
    ("ladder/l4_automation/cases/session_template_routing.json", 2),
    ("ladder/l4_automation/cases/train_daily_cron_digest.json", 2),
    ("ladder/l4_automation/cases/train_interval_heartbeat_fires.json", 2),
    ("ladder/l4_automation/cases/train_parallel_baseline.json", 2),
    ("ladder/l4_automation/cases/train_pause_nightly_freight_route.json", 4),
    ("ladder/l4_automation/cases/train_resume_tide_gauge_task.json", 4),
    ("ladder/l4_automation/cases/train_suspend_kiln_shift_task.json", 4),
    ("ladder/l4_automation/cases/val_retime_luthier_schedule_in_place.json", 4),
    ("ladder/l4_automation/cases/val_stop_fern_receipt_outcomes.json", 4),
    ("ladder/l5_agents_tools/cases/heldout_mixed_valid_and_ghost.json", 4),
    ("ladder/l5_agents_tools/cases/heldout_night_proof_desk_notice.json", 11),
    ("ladder/l5_agents_tools/cases/train_minimal_self_target.json", 6),
    ("ladder/l5_agents_tools/cases/train_notify_west_dock_thread.json", 11),
    ("ladder/l5_agents_tools/cases/train_repair_intake_acknowledgement.json", 8),
    ("ladder/l5_agents_tools/cases/train_stop_one_crate_load.json", 9),
    ("ladder/l5_agents_tools/cases/val_caller_agent_and_delegate.json", 6),
    ("ladder/l5_agents_tools/cases/val_ghost_agent_refused.json", 2),
    ("ladder/l5_agents_tools/cases/val_linen_replenishment_request.json", 11),
    ("ladder/l5_agents_tools/cases/val_repoint_target_in_place.json", 6),
    ("ladder/l5_agents_tools/cases/val_self_target_existing_agent.json", 4),
    ("ladder/l5_agents_tools/cases/val_stop_duplicate_invoice_batch.json", 9),
    ("ladder/l5_agents_tools/cases/val_two_targets_both_selected.json", 4),
    ("ladder/l5_agents_tools/cases/val_verify_then_delegate_no_dupes.json", 5),
];
#[rustfmt::skip]
const UNEXERCISED: &[(&str, usize)] = &[
    ("engineer_automation_graphs/cases/proof_slip_run.json", 1),
    ("engineer_pack_authoring/cases/courier_zones_check_clean.json", 1),
    ("engineer_pack_authoring/cases/handbook_manifest_repair.json", 1),
    ("engineer_pack_authoring/cases/kiln_runbook_missing_skill.json", 1),
    ("engineer_pack_authoring/cases/parts_desk_app_pack.json", 1),
    ("engineer_pack_authoring/cases/stockroom_register_pack.json", 1),
    ("engineer_pack_authoring/cases/tide_notes_version_bump.json", 1),
    ("engineer_pages/cases/harbor_arrivals_page.json", 1),
    ("engineer_pages/cases/ticket_board_page.json", 1),
    ("engineer_skills/cases/courier_ticket_collection.json", 1),
    ("ladder/l2_agent/cases/analyst_grants_file_tools.json", 1),
    ("ladder/l2_agent/cases/archivist_full_files_no_bash.json", 1),
    ("ladder/l2_agent/cases/cursor_grants_list_only.json", 1),
    ("ladder/l2_agent/cases/ledger_prompt_edit_in_place.json", 1),
    ("ladder/l2_agent/cases/porter_fix_in_place_disable_bash.json", 1),
    ("ladder/l2_agent/cases/restore_desk_instructions.json", 1),
    ("ladder/l2_agent/cases/scout_grants_two_file_tools.json", 1),
    ("ladder/l4_automation/cases/held_interval_kiln_watch_pulse.json", 1),
    ("ladder/l4_automation/cases/train_daily_cron_digest.json", 1),
    ("ladder/l4_automation/cases/train_interval_heartbeat_fires.json", 1),
    ("ladder/l4_automation/cases/val_retime_luthier_schedule_in_place.json", 1),
];

const OPEN: [(Rule, &[(&str, usize)]); 5] = [
    (Rule::EngineerPin, ENGINEER_PIN),
    (Rule::LongPrompt, LONG_PROMPT),
    (Rule::PromptNamesTool, PROMPT_NAMES_TOOL),
    (Rule::Route, ROUTE),
    (Rule::Unexercised, UNEXERCISED),
];

#[test]
fn cases_break_the_eval_rules_only_as_open_lists_and_open_only_shrinks() {
    let found = scan();
    let mut open = BTreeMap::new();
    for (rule, cases) in OPEN {
        for (case, count) in cases {
            assert!(
                open.insert((rule, (*case).to_owned()), *count).is_none(),
                "OPEN lists {case} twice under rule {}",
                rule.number()
            );
        }
    }
    let added: Vec<String> = found
        .iter()
        .filter(|(key, details)| details.len() > open.get(*key).copied().unwrap_or(0))
        .flat_map(|((rule, case), details)| {
            details
                .iter()
                .map(move |detail| format!("  {case}: {detail} (rule {})", rule.number()))
        })
        .collect();
    assert!(
        added.is_empty(),
        "these cases break the rules in {README} (\"Rules for eval cases\") more often than OPEN in {} allows; change them to keep the rules. A route the prompt itself asks for takes a route_reason on its tool_calls_expected check.\n{}",
        file!(),
        added.join("\n")
    );
    let shrunk: BTreeMap<(Rule, String), usize> = open
        .iter()
        .map(|(key, count)| {
            let now = found.get(key).map_or(0, BTreeSet::len);
            (key.clone(), now.min(*count))
        })
        .filter(|(_, count)| *count > 0)
        .collect();
    assert!(
        shrunk == open,
        "cases break the rules in {README} less often than OPEN says; it only shrinks, so replace its five lists in {} with:\n\n{}",
        file!(),
        render(&shrunk)
    );
}

fn scan() -> BTreeMap<(Rule, String), BTreeSet<String>> {
    let suites = suites();
    let tools = known_tools(&suites);
    let mut found: BTreeMap<(Rule, String), BTreeSet<String>> = BTreeMap::new();
    for suite in &suites {
        let subject = subject(tools.clone());
        let paths = case_paths(&suite.dir);
        for case in definition(&suite.dir).cases {
            let path = &paths[&case.case_id];
            for (rule, detail) in findings(&case, &subject) {
                found
                    .entry((rule, path.clone()))
                    .or_default()
                    .insert(detail);
            }
        }
    }
    found
}

fn subject(tools: BTreeSet<String>) -> Subject {
    Subject {
        engineer: format!("{AGENT_SLOT_REFERENCE_PREFIX}{CELL_AGENT}"),
        tools_id: ENGINEER_TOOLS_ID.to_owned(),
        context_id: ENGINEER_CONTEXT_ID.to_owned(),
        tools,
    }
}

fn known_tools(suites: &[Suite]) -> BTreeSet<String> {
    let mut tools: BTreeSet<String> = NATIVE_TOOL_NAMES
        .iter()
        .chain(&AGENT_TOOL_NAMES)
        .chain(&META_TOOL_NAMES)
        .chain(&SELF_CONFIG_TOOL_NAMES)
        .map(|name| (*name).to_owned())
        .collect();
    for pack in CorePack::ALL {
        let (_, name) = pack.coordinate().split_once('/').unwrap();
        let manifest = json(&repo().join("core-packs").join(name).join("manifest.json"));
        tools.extend(
            array(&manifest["plugins"])
                .filter_map(|plugin| plugin["name"].as_str().map(str::to_owned)),
        );
    }
    for suite in suites {
        for case in definition(&suite.dir).cases {
            for check in case
                .stages
                .iter()
                .flat_map(|stage| &stage.checks)
                .filter(|check| check.check == "tool_calls_expected")
            {
                let params = &check.params;
                tools.extend(
                    array(&params["required"])
                        .chain(array(&params["forbidden"]))
                        .filter_map(|tool| tool.as_str().map(str::to_owned)),
                );
                tools.extend(
                    params["allowed_argv"]
                        .as_object()
                        .into_iter()
                        .flat_map(|argv| argv.keys().cloned()),
                );
            }
        }
    }
    tools
}

fn case_paths(dir: &Path) -> BTreeMap<String, String> {
    let config = json(&dir.join("pack_config.json"));
    array(&config["eval_definitions"][0]["cases"])
        .map(|file| {
            let file = dir.join(file.as_str().unwrap().trim_start_matches("./"));
            let case_id = json(&file)["case_id"].as_str().unwrap().to_owned();
            let relative = file.strip_prefix(fixtures()).unwrap();
            (case_id, relative.to_string_lossy().replace('\\', "/"))
        })
        .collect()
}

fn render(open: &BTreeMap<(Rule, String), usize>) -> String {
    OPEN.iter()
        .map(|(rule, _)| {
            let name = const_name(*rule);
            let lines: Vec<String> = open
                .iter()
                .filter(|((listed, _), _)| listed == rule)
                .map(|((_, case), count)| format!("    ({case:?}, {count}),"))
                .collect();
            if lines.is_empty() {
                format!("#[rustfmt::skip]\nconst {name}: &[(&str, usize)] = &[];")
            } else {
                format!(
                    "#[rustfmt::skip]\nconst {name}: &[(&str, usize)] = &[\n{}\n];",
                    lines.join("\n")
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn const_name(rule: Rule) -> String {
    let mut name = String::new();
    for (index, c) in format!("{rule:?}").chars().enumerate() {
        if c.is_uppercase() && index > 0 {
            name.push('_');
        }
        name.push(c.to_ascii_uppercase());
    }
    name
}

fn json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap())
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn array(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}
