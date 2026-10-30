use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::documents::TaskHookPhase;
use crate::lean_vocab_test::{
    lean_task_hook_run_cases, LeanCommandResult, LeanHookAttempt, LeanHookInvocation,
    LeanTaskHookRunCase, LeanTaskOutcome, LeanTaskPrimaryError,
};
use crate::task_hooks::{
    effective_timeout_secs, run_task_hooks, HookAttempt, HookPrimaryError, ManagedTaskHookExec,
    TaskAgentResult, TaskHookCancellation, TaskHookExec, TaskHookOutcome,
};

fn outcome(generated: &LeanTaskOutcome) -> TaskHookOutcome {
    match generated {
        LeanTaskOutcome::Success => TaskHookOutcome::Success,
        LeanTaskOutcome::Failure { error } => TaskHookOutcome::Failure(match error {
            LeanTaskPrimaryError::Hook { hook_id } => HookPrimaryError::Hook(hook_id.clone()),
            LeanTaskPrimaryError::Agent => HookPrimaryError::Agent,
        }),
        LeanTaskOutcome::Interrupted => TaskHookOutcome::Interrupted,
    }
}

fn agent_result(name: &str) -> TaskAgentResult {
    match name {
        "success" => TaskAgentResult::Success,
        "failure" => TaskAgentResult::Failure,
        "cancelled" => TaskAgentResult::Cancelled,
        "interrupted" => TaskAgentResult::Interrupted,
        other => panic!("generated contract emitted an unknown agent result {other:?}"),
    }
}

fn scripted(case: &LeanTaskHookRunCase, hook_id: &str) -> LeanCommandResult {
    case.script
        .iter()
        .find(|entry| entry.hook_id == hook_id)
        .map(|entry| entry.result.clone())
        .unwrap_or(LeanCommandResult::Exited { code: 0 })
}

fn log_line(invocation: &LeanHookInvocation) -> String {
    match invocation {
        LeanHookInvocation::Hook { hook_id } => format!("hook:{hook_id}"),
        LeanHookInvocation::Work => "work".to_string(),
    }
}

fn started_marker(dir: &Path, hook_id: &str) -> PathBuf {
    dir.join(format!("started-{hook_id}"))
}

fn real_command(dir: &Path, log: &Path, hook_id: &str, result: &LeanCommandResult) -> Vec<String> {
    let record = format!(
        "printf 'hook:%s\\n' {hook_id} >> {log}",
        log = log.display()
    );
    let script = match result {
        LeanCommandResult::Exited { code } => format!("{record}; exit {code}"),
        LeanCommandResult::TimedOut => format!("{record}; exec sleep 30"),
        LeanCommandResult::Interrupted => format!(
            "{record}; : > {marker}; exec sleep 30",
            marker = started_marker(dir, hook_id).display()
        ),
        LeanCommandResult::LaunchFailed => {
            return vec![dir
                .join("missing")
                .join(hook_id)
                .to_str()
                .expect("utf-8 hook path")
                .to_string()]
        }
    };
    vec!["sh".to_string(), "-c".to_string(), script]
}

async fn cancel_held_commands(
    dir: PathBuf,
    held: Vec<(String, TaskHookPhase)>,
    cancellation: TaskHookCancellation,
) {
    for (hook_id, phase) in held {
        while !started_marker(&dir, &hook_id).exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        match phase {
            TaskHookPhase::Finally => cancellation.shutdown(),
            _ => cancellation.interrupt(),
        }
    }
}

fn assert_attempts(case: &str, phase: &str, actual: &[HookAttempt], expected: &[LeanHookAttempt]) {
    assert_eq!(
        actual
            .iter()
            .map(|attempt| (attempt.hook_id.as_str(), attempt.result.clone()))
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|attempt| (attempt.hook_id.as_str(), attempt.result.to_native()))
            .collect::<Vec<_>>(),
        "{case}: {phase} attempts disagree with the model's trace"
    );
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(
            actual.result.succeeded(),
            expected.succeeded,
            "{case}: {phase} attempt {:?} disagrees on success",
            expected.hook_id
        );
    }
}

struct RevokingExec<'a> {
    managed: ManagedTaskHookExec,
    node: &'a Arc<defra_node::EmbeddedNode>,
    request_doc_id: &'a str,
    revoked: &'a [String],
}

#[async_trait::async_trait]
impl TaskHookExec for RevokingExec<'_> {
    async fn attempt(&self, hook: &crate::documents::TaskHook) -> HookAttempt {
        if self.revoked.contains(&hook.hook_id) {
            crate::config_client::ConfigAccess::write_local(
                self.node,
                "test.task_hook_launch_revocation",
                &format!(
                    r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "revoked-generation" }}) {{ _docID }} }}"#,
                    crate::graphql::escape_graphql_string(self.request_doc_id),
                ),
            ).await.unwrap();
        }
        self.managed.attempt(hook).await
    }
}

#[tokio::test]
async fn generated_task_hook_run_cases_drive_real_host_commands() {
    let fixture = super::task_hook_recovery::Fixture::new().await;
    let cases = lean_task_hook_run_cases();
    assert!(!cases.is_empty(), "task hook run cases must not be empty");
    for case in cases {
        let dir = tempfile::tempdir().expect("hook case directory");
        let log = dir.path().join("invocations.log");
        let mut hooks = Vec::new();
        let mut held = Vec::new();
        let mut unlaunchable = case.refused_before_launch.clone();
        for generated in &case.hooks {
            let mut configured = generated.to_task_hook();
            assert_eq!(
                effective_timeout_secs(&configured),
                generated.effective_timeout_secs,
                "{}: production timeout resolution disagrees with the model for {:?}",
                case.name,
                generated.hook_id
            );
            let result = scripted(case, &generated.hook_id);
            match result {
                LeanCommandResult::TimedOut => assert!(
                    generated.effective_timeout_secs <= 5,
                    "{}: the model must bound its timed-out occurrence",
                    case.name
                ),
                LeanCommandResult::Interrupted => {
                    held.push((generated.hook_id.clone(), configured.phase))
                }
                LeanCommandResult::LaunchFailed => unlaunchable.push(generated.hook_id.clone()),
                LeanCommandResult::Exited { .. } => {}
            }
            configured.command = real_command(dir.path(), &log, &generated.hook_id, &result);
            hooks.push(configured);
        }

        let cancellation = TaskHookCancellation::default();
        let canceller = tokio::spawn(cancel_held_commands(
            dir.path().to_path_buf(),
            held,
            cancellation.clone(),
        ));
        let lifecycle = fixture.claimed(Duration::from_secs(120)).await;
        let exec = RevokingExec {
            managed: ManagedTaskHookExec::new(dir.path().to_path_buf(), cancellation)
                .with_execution_lease(
                    fixture.node.clone(),
                    lifecycle.request().doc_id.clone(),
                    crate::lifecycle::RequestExecutionLease::new(
                        lifecycle.execution_generation().unwrap().to_owned(),
                    ),
                ),
            node: &fixture.node,
            request_doc_id: &lifecycle.request().doc_id,
            revoked: &case.revoked,
        };
        let agent = agent_result(&case.agent);
        let work_log = log.clone();
        let run = tokio::time::timeout(
            Duration::from_secs(60),
            run_task_hooks(&hooks, &exec, || async move {
                use std::io::Write;
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&work_log)
                    .expect("invocation log");
                writeln!(file, "work").expect("record owned work");
                agent
            }),
        )
        .await
        .unwrap_or_else(|_| panic!("{}: hook execution did not finish", case.name));
        canceller.abort();

        let observed = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let expected = case
            .invocation_trace
            .iter()
            .filter(|invocation| {
                !matches!(invocation, LeanHookInvocation::Hook { hook_id } if unlaunchable.contains(hook_id))
            })
            .map(log_line)
            .collect::<Vec<_>>();
        assert_eq!(
            observed, expected,
            "{}: host commands and the owned work ran in a different sequence than the model's trace",
            case.name
        );
        assert_eq!(
            run.agent_result.is_some(),
            case.expected_agent_ran,
            "{}",
            case.name
        );
        assert_attempts(
            &case.name,
            "before",
            &run.before_attempted,
            &case.before_attempted,
        );
        assert_attempts(
            &case.name,
            "after_success",
            &run.after_success_attempted,
            &case.after_success_attempted,
        );
        assert_attempts(
            &case.name,
            "after_failure",
            &run.after_failure_attempted,
            &case.after_failure_attempted,
        );
        assert_attempts(
            &case.name,
            "finally",
            &run.finally_attempted,
            &case.finally_attempted,
        );
        assert_eq!(run.cleanup_errors(), case.cleanup_errors, "{}", case.name);
        assert_eq!(
            run.outcome,
            outcome(&case.expected_outcome),
            "{}",
            case.name
        );
        let final_outcome = run.final_outcome();
        assert_eq!(
            final_outcome,
            outcome(&case.expected_final_outcome),
            "{}",
            case.name
        );
        assert_eq!(
            final_outcome
                .terminal_outcome()
                .request_lifecycle_state()
                .as_str(),
            case.expected_request_state,
            "{}: the terminal owner would write a different request state",
            case.name
        );
    }
}
