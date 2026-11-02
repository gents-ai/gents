use super::super::admit::{admit_step, StepCall};
use super::*;
use crate::tools::call::lifecycle::{AwaitMode, ToolRowKind};
use gents_protocol::job::{DynamicMode, JobCeiling, JobPolicy, JobState};

fn ceiling() -> JobCeiling {
    JobCeiling {
        targets: vec!["a".to_owned()],
        max_live: 4,
        max_spawns: 10,
        max_messages_per_slot: 2,
        result_bytes: 1024,
        max_wall_secs: 60,
    }
}

fn facts() -> HostFacts {
    HostFacts {
        engine_digest: "engine".to_owned(),
        plugin_ref: "ns/plugin".to_owned(),
        cwd: "/work".to_owned(),
        args_digest: "args".to_owned(),
        chain_root: "row".to_owned(),
        origin: JobOrigin::Dynamic,
        adopted_at: 1,
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: ceiling(),
    }
}

fn context() -> StepContext {
    StepContext {
        live: ceiling(),
        available: true,
        caller_targets: EndpointScope::All,
        job_targets: EndpointScope::All,
        policy: JobPolicy {
            dynamic: DynamicMode::Run,
            dynamic_targets: vec!["a".to_owned()],
            ..Default::default()
        },
        engine_digest: "engine".to_owned(),
        engine_provenance_operator: true,
        max_hop: Some(8),
        target_hops: vec![(("node".to_owned(), "a".to_owned()), 0)],
        target_ids: vec![("a".to_owned(), ("node".to_owned(), "a".to_owned()))],
        live_background: 0,
        started_by_job: false,
        cwd_allowed: Vec::new(),
        within: Vec::new(),
        script_digest: None,
        adoption_facts: facts(),
        caller_scope: ProcessControlScope {
            request_id: "request".to_owned(),
            session_id: "session".to_owned(),
            node_did: "node".to_owned(),
            requester_did: None,
        },
        control_targets: Vec::new(),
        resume_targets: Vec::new(),
        resume_facts: facts(),
    }
}

fn step0_row() -> StepRow {
    StepRow {
        root: "row".to_owned(),
        row_doc_id: "row".to_owned(),
        caller_session: "session".to_owned(),
        caller_cwd: String::new(),
        handle: String::new(),
        caller_hop: 0,
        call: StepCall {
            argv: vec!["run".to_owned()],
            args: None,
            script: None,
            agent_budget: None,
        },
        root_call: None,
        resume: None,
        pin: None,
        children: Vec::new(),
        spent: 0,
        observed_terminal: 0,
        consecutive_failures: 0,
        state: JobState::Running,
    }
}

fn view() -> ToolCallView {
    ToolCallView {
        state: ToolCallState::Running,
        kind: ToolRowKind::Native,
        await_mode: AwaitMode::Foreground,
    }
}

const PIN: &str =
    r#""pin": {"script_id": "s", "source_digest": "sha256:s", "agent_budget": 2, "handle": "h"}"#;

fn output(json: &str) -> JobStepOutput {
    serde_json::from_str(json).unwrap()
}

fn pinned_effects() -> JobStepOutput {
    output(&format!(
        r#"{{"effects": {{{PIN}, "spawn": [{{"slot": "s", "attempt": 0, "target": "a", "prompt": "p", "title": "t"}}]}}}}"#
    ))
}

#[test]
fn adopt_flips_a_pinned_effects_step_to_a_background_plugin_job_with_no_delivery() {
    let out = pinned_effects();
    let adopted = adopt(&context(), &view(), &step0_row(), &out).unwrap();
    assert_eq!(adopted.call.await_mode, AwaitMode::Background);
    assert_eq!(adopted.call.kind, ToolRowKind::PluginJob);
    assert_eq!(adopted.call.state, ToolCallState::Running);
    assert_eq!(adopted.delivery, None);
    assert_eq!(
        adopted.row,
        admit_step(&context(), &step0_row(), &out).unwrap()
    );
}

#[test]
fn adopt_terminalizes_a_pinned_step0_result_inline() {
    for json in [
        format!(r#"{{"result": "done", {PIN}}}"#),
        format!(
            r#"{{"suspend": {{"kind": "pause", "reason": "budget_exceeded", "message": "m"}}, {PIN}}}"#
        ),
    ] {
        let adopted = adopt(&context(), &view(), &step0_row(), &output(&json)).unwrap();
        assert_eq!(adopted.call.state, ToolCallState::Completed, "{json}");
        assert_eq!(adopted.call.await_mode, AwaitMode::Background, "{json}");
        assert_eq!(adopted.call.kind, ToolRowKind::PluginJob, "{json}");
        assert_eq!(adopted.delivery, Some(Delivery::Inline), "{json}");
    }
}

#[test]
fn pinless_or_refused_step0_leaves_the_call() {
    let adopted = adopt(
        &context(),
        &view(),
        &step0_row(),
        &output(r#"{"result": "done"}"#),
    )
    .unwrap();
    assert_eq!(adopted.call, view());
    assert_eq!(adopted.delivery, None);

    let full = StepContext {
        live_background: 8,
        ..context()
    };
    let refusal = adopt(&full, &view(), &step0_row(), &pinned_effects()).unwrap_err();
    assert_eq!(refusal.code(), Some("background_tool_budget_exceeded"));
}

#[test]
fn receipt_decisions_commit_once_then_conflict() {
    let adopted = adopt(&context(), &view(), &step0_row(), &pinned_effects()).unwrap();
    let (terminal, late) = adopted.receipt_decisions();
    assert_eq!(
        (terminal, late),
        (ReceiptDecision::Commit, ReceiptDecision::Conflict)
    );
    assert_eq!(
        (terminal.spelling(), late.spelling()),
        ("commit", "conflict")
    );
}

#[test]
fn unique_handle_keeps_a_free_handle_and_suffixes_the_least_free_n() {
    assert_eq!(unique_handle(&[], "h"), "h");
    assert_eq!(unique_handle(&["x".to_owned()], "h"), "h");
    assert_eq!(unique_handle(&["h".to_owned()], "h"), "h-2");
    assert_eq!(
        unique_handle(&["h".to_owned(), "h-2".to_owned()], "h"),
        "h-3"
    );
}

#[test]
fn split_address_splits_at_the_last_colon() {
    assert_eq!(
        split_address("job-session:a:b:call1"),
        Some(("job-session:a:b".to_owned(), "call1".to_owned()))
    );
    assert_eq!(split_address("nocolon"), None);
}
