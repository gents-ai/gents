use crate::lean_vocab_test::{
    lean_goal_capability_resolution_cases, lean_plugin_asset_mount_cases,
    lean_plugin_calls_route_cases, lean_plugin_data_cases, lean_plugin_project_tier_cases,
    lean_plugin_scratch_cases, lean_tool_policy_cases, LeanPluginAccess,
    LeanPluginDataInsertOutcome, LeanPluginDataInvocation, LeanPluginDataOnConflict,
    LeanPluginDataOperation, LeanPluginDataRefusal, LeanPluginDataStamp, LeanPluginDataWriteFacts,
    LeanPluginMount,
};
use gents::application_write::Observation as WriteFacts;
use gents::plugin::data::{
    self as plugin_data, InsertOutcome, Invocation, OnConflict, Stamp, WriteRefusal,
};
use gents::plugin::mounts::MountAccess;
use gents_protocol::job::AssetMount;
use std::collections::BTreeSet;

#[path = "tool_policy_codec.rs"]
mod tool_policy_codec;

pub(super) fn generated_tool_policy_cases_match_lean_composition() {
    let cases = lean_tool_policy_cases();
    assert_eq!(
        cases.len(),
        35,
        "Lean tool-policy composition matrix drifted"
    );
    for case in cases {
        let actual = tool_policy_codec::compose(&case.agent, &case.ceiling, &case.runtime);
        assert_eq!(
            actual, case.expected,
            "{}: production permission composition",
            case.name
        );
    }
}

pub(super) fn generated_goal_capability_resolution_matches_rust_decoder() {
    let cases = lean_goal_capability_resolution_cases();
    assert_eq!(cases.len(), 4, "goal capability resolution matrix drifted");
    for case in cases {
        let (goal_tools, goal_creation) = gents::tool_surface::resolve_goal_capabilities(
            case.explicit_goal_tools,
            case.explicit_goal_create,
        );
        assert_eq!(
            (goal_tools, goal_creation),
            (case.expected_goal_tools, case.expected_goal_create),
            "Lean capability resolution case {}",
            case.name
        );
    }
}

fn invocation(lean: LeanPluginDataInvocation) -> Invocation {
    match lean {
        LeanPluginDataInvocation::ToolCall => Invocation::ToolCall,
        LeanPluginDataInvocation::JobStep => Invocation::JobStep,
        LeanPluginDataInvocation::Callback => Invocation::Callback,
    }
}

fn stamp(lean: LeanPluginDataStamp) -> Stamp {
    match lean {
        LeanPluginDataStamp::NodeDid => Stamp::NodeDid,
        LeanPluginDataStamp::SessionId => Stamp::SessionId,
        LeanPluginDataStamp::RequestDocId => Stamp::RequestDocId,
        LeanPluginDataStamp::ToolCallDocId => Stamp::ToolCallDocId,
        LeanPluginDataStamp::JobRowDocId => Stamp::JobRowDocId,
        LeanPluginDataStamp::ChainRootDocId => Stamp::ChainRootDocId,
    }
}

fn stamps(lean: &[(String, LeanPluginDataStamp)]) -> Vec<(String, Stamp)> {
    lean.iter()
        .map(|(field, value)| (field.clone(), stamp(*value)))
        .collect()
}

fn refusal(lean: LeanPluginDataRefusal) -> WriteRefusal {
    match lean {
        LeanPluginDataRefusal::StampUnavailable {
            stamp: value,
            invocation: kind,
        } => WriteRefusal::StampUnavailable {
            stamp: stamp(value),
            invocation: invocation(kind),
        },
        LeanPluginDataRefusal::Other => WriteRefusal::Other,
    }
}

/// `ToolPolicy.ApplicationWrite.Observation` as the native write observation
/// that `application_write` already owns; Lean's `blocked` is its `protected`.
fn write_facts(op: LeanPluginDataOperation, w: LeanPluginDataWriteFacts) -> WriteFacts {
    WriteFacts {
        operation: match op {
            LeanPluginDataOperation::Create => "create",
            LeanPluginDataOperation::Update => "update",
            LeanPluginDataOperation::Delete => "delete",
        }
        .to_owned(),
        granted: w.granted,
        application: w.application,
        protected: w.blocked,
        credential: w.credential,
        bounded: w.bounded,
        targets: w.targets as usize,
        limit: w.limit as usize,
        preview: w.preview,
        digest_matches: w.digest_matches,
    }
}

/// Lean `PluginData.applicableStamps`, `admitted`, `refusal`, `queryAdmitted`,
/// `substituteCallerNodeDid` and `insertNote` through the pure core
/// `plugin::data::admit_write_observation` beside the frozen owner
/// `plugin::data::admit_write`, and its siblings, which
/// `application_write::apply_plugin_writes_in_txn` calls inside the output's
/// transaction. Notes are checked here as the read-then-insert decision the
/// transaction applies to the row it read.
#[test]
fn generated_plugin_data_cases_drive_admit_write() {
    let cases = lean_plugin_data_cases();
    assert_eq!(
        cases.writes.len(),
        11,
        "Lean plugin-data write rows drifted"
    );
    assert_eq!(
        cases.queries.len(),
        7,
        "Lean plugin-data query rows drifted"
    );
    assert_eq!(
        cases.filters.len(),
        3,
        "Lean plugin-data filter rows drifted"
    );
    assert_eq!(cases.notes.len(), 8, "Lean plugin-data note rows drifted");

    for (index, row) in cases.writes.iter().enumerate() {
        let applicable =
            plugin_data::applicable_stamps(&stamps(&row.declared_stamps), &row.collection_fields);
        assert_eq!(
            applicable,
            stamps(&row.applicable),
            "write {index}: applicable stamps"
        );
        let observation = plugin_data::WriteObservation {
            in_own_sdl: row.in_own_sdl,
            runtime_or_protected: row.runtime_or_protected,
            actor_is_invoker: row.actor_is_invoker,
            op_granted: row.op_granted,
            stamped_supplied_by_guest: row.stamped_supplied_by_guest,
            invocation: invocation(row.invocation),
            applicable,
            write: write_facts(row.op, row.write),
        };
        let verdict = plugin_data::admit_write_observation(&observation);
        assert_eq!(verdict.is_ok(), row.admitted, "write {index}: admitted");
        assert_eq!(
            verdict.err(),
            row.refusal.map(refusal),
            "write {index}: refusal"
        );
    }

    for (index, row) in cases.queries.iter().enumerate() {
        assert_eq!(
            plugin_data::query_admitted(
                row.limit,
                row.in_own_sdl,
                row.runtime_or_protected,
                row.query_granted,
            ),
            row.expected,
            "query {index}"
        );
    }

    for (index, row) in cases.filters.iter().enumerate() {
        assert_eq!(
            plugin_data::substitute_caller_node_did(&row.filter, &row.host_node_did),
            row.expected,
            "filter {index}"
        );
    }

    for (index, row) in cases.notes.iter().enumerate() {
        let on_conflict = match row.on_conflict {
            LeanPluginDataOnConflict::Ignore => OnConflict::Ignore,
            LeanPluginDataOnConflict::Fail => OnConflict::Fail,
        };
        let outcome = plugin_data::insert_note(row.existing.as_deref(), &row.new, on_conflict);
        let expected = match row.expected {
            LeanPluginDataInsertOutcome::Inserted => InsertOutcome::Inserted,
            LeanPluginDataInsertOutcome::Unchanged => InsertOutcome::Unchanged,
            LeanPluginDataInsertOutcome::AbortStep => InsertOutcome::AbortStep,
        };
        assert_eq!(outcome, expected, "note {index}");
    }
}

fn mount(lean: &LeanPluginMount) -> (AssetMount, bool) {
    (
        AssetMount {
            pack: lean.pack.clone(),
            class: lean.class.clone(),
            digest: lean.digest.clone(),
            path: lean.path.clone(),
            trusted: lean.trusted,
        },
        lean.installed,
    )
}

/// Lean `mountAccess`, `mountCallAdmitted`, `projectPreopen` and
/// `scratchAdmitted` through the pure core `plugin::mounts::resolve_access`
/// beside the frozen file-bound `plugin::mounts::resolve`,
/// `plugin::mounts::project_preopen` and the scratch admission
/// `plugin_job::effects::admit_scratch`.
#[test]
fn generated_plugin_job_resource_cases_drive_mounts_and_scratch() {
    let mounts = lean_plugin_asset_mount_cases();
    assert_eq!(mounts.len(), 12, "Lean asset-mount rows drifted");
    for (index, row) in mounts.iter().enumerate() {
        let resolution = gents::plugin::mounts::resolve_access(
            row.rw_bind,
            &row.mounts.iter().map(mount).collect::<Vec<_>>(),
        );
        let expected = row
            .access
            .iter()
            .map(|access| {
                access.map(|access| match access {
                    LeanPluginAccess::Read => MountAccess::Read,
                    LeanPluginAccess::ReadWrite => {
                        panic!("asset_mount {index}: a mount is never read-write")
                    }
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(resolution.access, expected, "asset_mount {index}: access");
        assert_eq!(resolution.admitted, row.admitted, "asset_mount {index}");
    }

    let tiers = lean_plugin_project_tier_cases();
    assert_eq!(tiers.len(), 16, "Lean project-tier rows drifted");
    for (index, row) in tiers.iter().enumerate() {
        assert_eq!(
            gents::plugin::mounts::project_preopen(&row.cwd, &row.project_roots),
            row.expected,
            "project_tier {index}: {} under {:?}",
            row.cwd,
            row.project_roots
        );
    }

    let scratch = lean_plugin_scratch_cases();
    assert_eq!(scratch.len(), 13, "Lean scratch rows drifted");
    for (index, row) in scratch.iter().enumerate() {
        let allowed = row.cwd_allowed;
        assert_eq!(
            gents::plugin_job::effects::admit_scratch(
                &row.cwd,
                &|_path: &str| allowed,
                &row.tool_call_id,
                &row.name,
                row.size,
                row.count,
            ),
            row.expected,
            "scratch {index}: {}/{}",
            row.tool_call_id,
            row.name
        );
    }
}

/// Lean `PluginCalls.childAdmissible` over `routeOf staticHookOwned jobBound`
/// through `plugin::tool_calls::supported`, given the hook's job-bound tool
/// set. A static hook-owned route is a tool of the hook's own table; a
/// job-bound tool is an operator-chosen name that only the set knows.
#[test]
fn generated_plugin_calls_route_cases_drive_supported() {
    let cases = lean_plugin_calls_route_cases();
    assert_eq!(cases.len(), 16, "Lean child-route rows drifted");
    for (index, row) in cases.iter().enumerate() {
        let name = if row.static_hook_owned {
            gents::goal::GET_GOAL_TOOL_NAME
        } else {
            "workflow_run"
        };
        let job_tools: BTreeSet<String> =
            row.job_bound.then(|| name.to_owned()).into_iter().collect();
        let supported = gents::plugin::tool_calls::supported(name, &job_tools);
        if row.selected && row.admitted {
            assert_eq!(supported, row.expected, "route {index}");
        } else {
            assert!(
                !row.expected,
                "route {index}: only a selected, admitted child runs"
            );
        }
    }
}
