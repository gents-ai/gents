use std::path::{Path, PathBuf};
use std::time::Duration;

use gents_documents::TaskHookPhase;
use gents_toolcalls::call::task_hooks::{
    effective_timeout_secs, run_task_hooks, HookAttempt, HookCommandResult, HookPrimaryError,
    ManagedTaskHookExec, TaskAgentResult, TaskHookCancellation, TaskHookExec, TaskHookOutcome,
};

fn agent_result(name: &str) -> TaskAgentResult {
    match name {
        "success" => TaskAgentResult::Success,
        "failure" => TaskAgentResult::Failure,
        "cancelled" => TaskAgentResult::Cancelled,
        "interrupted" => TaskAgentResult::Interrupted,
        "parked" => TaskAgentResult::Parked,
        other => panic!("generated contract emitted an unknown agent result {other:?}"),
    }
}

fn scripted(
    input: &gents_lean_vocab::families::task_hooks_contracts_run_trace::RunInput<
        String,
        String,
        TaskHookPhase,
    >,
    hook_id: &str,
) -> (String, Option<i64>) {
    input
        .script
        .iter()
        .find(|entry| entry.hook_id == hook_id)
        .map(|entry| (entry.result.kind.clone(), entry.result.code))
        .unwrap_or(("exited".to_owned(), Some(0)))
}

fn started_marker(dir: &Path, hook_id: &str) -> PathBuf {
    dir.join(format!("started-{hook_id}"))
}

fn real_command(
    dir: &Path,
    log: &Path,
    hook_id: &str,
    kind: &str,
    code: Option<i64>,
) -> Vec<String> {
    let record = format!(
        "printf 'hook:%s\\n' {hook_id} >> {log}",
        log = log.display()
    );
    let script = match kind {
        "exited" => format!("{record}; exit {}", code.expect("an exit code")),
        "timed_out" => format!("{record}; exec sleep 30"),
        "interrupted" => format!(
            "{record}; : > {marker}; exec sleep 30",
            marker = started_marker(dir, hook_id).display()
        ),
        "launch_failed" => {
            return vec![dir
                .join("missing")
                .join(hook_id)
                .to_str()
                .expect("utf-8 hook path")
                .to_string()]
        }
        other => panic!("unknown scripted command result {other:?}"),
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

fn command_wire(
    result: &HookCommandResult,
) -> gents_lean_vocab::families::task_hooks_contracts_run_trace::CommandWire<String> {
    let (kind, code) = match result {
        HookCommandResult::Exited { code } => ("exited", *code),
        HookCommandResult::LaunchFailed => ("launch_failed", None),
        HookCommandResult::TimedOut => ("timed_out", None),
        HookCommandResult::Interrupted => ("interrupted", None),
    };
    gents_lean_vocab::families::task_hooks_contracts_run_trace::CommandWire {
        kind: kind.to_owned(),
        code,
    }
}

fn attempts(
    actual: &[HookAttempt],
) -> Vec<gents_lean_vocab::families::task_hooks_contracts_run_trace::AttemptWire<String>> {
    actual
        .iter()
        .map(
            |attempt| gents_lean_vocab::families::task_hooks_contracts_run_trace::AttemptWire {
                hook_id: attempt.hook_id.clone(),
                result: command_wire(&attempt.result),
                succeeded: attempt.result.succeeded(),
            },
        )
        .collect()
}

fn outcome_wire(
    outcome: &TaskHookOutcome,
) -> gents_lean_vocab::families::task_hooks_contracts_run_trace::OutcomeWire<String, String> {
    let (kind, error) = match outcome {
        TaskHookOutcome::Success => ("success", None),
        TaskHookOutcome::Failure(HookPrimaryError::Hook(hook_id)) => (
            "failure",
            Some(
                gents_lean_vocab::families::task_hooks_contracts_run_trace::ErrorWire {
                    kind: "hook".to_owned(),
                    hook_id: Some(hook_id.clone()),
                },
            ),
        ),
        TaskHookOutcome::Failure(HookPrimaryError::Agent) => (
            "failure",
            Some(
                gents_lean_vocab::families::task_hooks_contracts_run_trace::ErrorWire {
                    kind: "agent".to_owned(),
                    hook_id: None,
                },
            ),
        ),
        TaskHookOutcome::Interrupted => ("interrupted", None),
        TaskHookOutcome::Parked => ("parked", None),
    };
    gents_lean_vocab::families::task_hooks_contracts_run_trace::OutcomeWire {
        kind: kind.to_owned(),
        error,
    }
}

fn invocation(
    line: &str,
) -> gents_lean_vocab::families::task_hooks_contracts_run_trace::InvocationWire<String> {
    match line.strip_prefix("hook:") {
        Some(hook_id) => {
            gents_lean_vocab::families::task_hooks_contracts_run_trace::InvocationWire {
                kind: "hook".to_owned(),
                hook_id: Some(hook_id.to_owned()),
            }
        }
        None => {
            assert_eq!(line, "work", "unknown invocation log line");
            gents_lean_vocab::families::task_hooks_contracts_run_trace::InvocationWire {
                kind: "work".to_owned(),
                hook_id: None,
            }
        }
    }
}

struct RevokingExec<'a> {
    managed: ManagedTaskHookExec,
    access: gents_store::config_client::ConfigAccess,
    request_doc_id: &'a str,
    revoked: &'a [String],
}

#[async_trait::async_trait]
impl TaskHookExec for RevokingExec<'_> {
    async fn attempt(&self, hook: &gents_documents::TaskHook) -> HookAttempt {
        if self.revoked.contains(&hook.hook_id) {
            let response = self.access.write(
                "test.task_hook_launch_revocation",
                &format!(
                    r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ execution_generation: "revoked-generation" }}) {{ _docID }} }}"#,
                    gents_store::graphql::escape_graphql_string(self.request_doc_id),
                ),
            ).await.unwrap();
            assert_eq!(
                response["data"]["update_AgentRequest"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
        }
        self.managed.attempt(hook).await
    }
}

#[tokio::test]
async fn generated_task_hook_run_cases_drive_real_host_commands() {
    let fixture = super::task_hook_recovery_tests::Fixture::new().await;
    for case in gents_lean_vocab::families::task_hooks_contracts_run_trace::cases::<
        String,
        String,
        String,
        TaskHookPhase,
        String,
        String,
    >() {
        let input = &case.input;
        let dir = tempfile::tempdir().expect("hook case directory");
        let log = dir.path().join("invocations.log");
        let mut hooks = Vec::new();
        let mut held = Vec::new();
        let mut effective_timeout = Vec::new();
        for generated in &input.hooks {
            let mut configured = gents_documents::TaskHook {
                hook_id: generated.hook_id.clone(),
                phase: generated.phase,
                command: generated.command.clone(),
                timeout_secs: generated.timeout_secs,
                skip_exit_codes: generated.skip_exit_codes.clone(),
            };
            let timeout = effective_timeout_secs(&configured);
            effective_timeout.push(timeout);
            let (kind, code) = scripted(input, &generated.hook_id);
            match kind.as_str() {
                "timed_out" => assert!(
                    timeout <= 5,
                    "{}: a timed-out occurrence needs a bounded timeout",
                    input.name
                ),
                "interrupted" => held.push((generated.hook_id.clone(), configured.phase)),
                _ => {}
            }
            configured.command = real_command(dir.path(), &log, &generated.hook_id, &kind, code);
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
                    gents_session::request::lifecycle::RequestExecutionLease::new(
                        lifecycle.execution_generation().unwrap().to_owned(),
                        lifecycle.actor_did().unwrap(),
                    ),
                ),
            access: fixture.agent_access(),
            request_doc_id: &lifecycle.request().doc_id,
            revoked: &input.revoked,
        };
        let agent = agent_result(&input.agent);
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
        .unwrap_or_else(|_| panic!("{}: hook execution did not finish", input.name));
        canceller.abort();

        let observed = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let launched = observed
            .iter()
            .map(|line| invocation(line))
            .collect::<Vec<_>>();
        let phases = [
            &run.before_attempted,
            &run.after_success_attempted,
            &run.after_failure_attempted,
            &run.finally_attempted,
        ];
        let refused_before_launch = phases
            .iter()
            .flat_map(|attempts| attempts.iter())
            .filter(|attempt| {
                attempt.result != HookCommandResult::LaunchFailed
                    && !observed.contains(&format!("hook:{}", attempt.hook_id))
            })
            .map(|attempt| attempt.hook_id.clone())
            .collect::<Vec<_>>();
        let hook = |attempt: &HookAttempt| {
            gents_lean_vocab::families::task_hooks_contracts_run_trace::InvocationWire {
                kind: "hook".to_owned(),
                hook_id: Some(attempt.hook_id.clone()),
            }
        };
        let invocation_trace = run
            .before_attempted
            .iter()
            .map(hook)
            .chain(run.agent_result.iter().map(|_| {
                gents_lean_vocab::families::task_hooks_contracts_run_trace::InvocationWire {
                    kind: "work".to_owned(),
                    hook_id: None,
                }
            }))
            .chain(run.after_success_attempted.iter().map(hook))
            .chain(run.after_failure_attempted.iter().map(hook))
            .chain(run.finally_attempted.iter().map(hook))
            .collect();
        let final_outcome = run.final_outcome();
        let request_state = final_outcome
            .terminal_outcome()
            .map(|outcome| outcome.request_lifecycle_state())
            .unwrap_or(gents_protocol::request_lifecycle::RequestLifecycleState::WaitingForBackend)
            .as_str()
            .to_owned();
        case.check(
            gents_lean_vocab::families::task_hooks_contracts_run_trace::RunOutput {
                effective_timeout_secs: effective_timeout,
                refused_before_launch,
                agent_ran: run.agent_result.is_some(),
                before_attempted: attempts(&run.before_attempted),
                after_success_attempted: attempts(&run.after_success_attempted),
                after_failure_attempted: attempts(&run.after_failure_attempted),
                finally_attempted: attempts(&run.finally_attempted),
                invocation_trace,
                launched_trace: launched,
                cleanup_errors: run.cleanup_errors(),
                outcome: outcome_wire(&run.outcome),
                final_outcome: outcome_wire(&final_outcome),
                request_state,
            },
        );
    }
}
