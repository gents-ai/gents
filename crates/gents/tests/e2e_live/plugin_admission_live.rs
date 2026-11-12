use std::path::PathBuf;
use std::time::Duration;

use gents::DocumentRuntimeOptions;
use gents_documents::{BashMode, BashTools, HostTools, Tools};
use gents_protocol::job::{JobChildState, JobControlKind, JobControlRequest, JobState, JobView};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_toolcalls::background::job::view::{load_job_view, JobKey};
use gents_toolcalls::call::control::{
    cancel_background_tool_call, CancelBackgroundToolCallOutcome,
};
use gents_toolcalls::call::hook::BackgroundExecutionRegistry;
use gents_tools::surface::ToolCeiling;
use serde_json::{json, Value};

use super::plugin_goal_live::{select_profiles, submit_with_cwd};
use super::plugin_job_support::{
    effects, guest_pin, install_job_fixture, job_settings, result, spawn, JobActors, PlanEntry,
    CHILD, PARENT,
};
use crate::support::interrupt::BootedAgent;
use crate::support::live_inference::{
    boot_live_agent_with_options, live_target, wait_for_request_terminal, wait_for_rows, LiveUser,
};
use crate::support::snapshots::SnapshotReader;
use crate::support::{test_db, TestDb};

struct LiveJobs {
    db: TestDb,
    actors: JobActors,
    live: LiveUser,
    home: tempfile::TempDir,
    cwd: PathBuf,
    runtime: BootedAgent,
    release: std::fs::File,
}

impl LiveJobs {
    async fn boot(name: &str) -> Self {
        let target = live_target().await;
        let db = test_db(name).await;
        let home = tokio::task::spawn_blocking(tempfile::tempdir)
            .await
            .unwrap()
            .unwrap();
        let root = tokio::fs::canonicalize(home.path()).await.unwrap();
        let cwd = root.join("work");
        tokio::fs::create_dir(&cwd).await.unwrap();
        let fifo = root.join("release");
        let release = tokio::task::spawn_blocking(move || {
            use std::os::unix::fs::OpenOptionsExt;
            assert!(std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success());
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(fifo)
                .unwrap()
        })
        .await
        .unwrap();
        let actors = install_job_fixture(&db, home.path(), job_settings()).await;
        let live = LiveUser::install(&db, &target).await;
        select_profiles(
            &live,
            &target,
            &[&actors.fixture.binding.agent_did, &actors.child_did],
        )
        .await;
        for (agent, id, child) in [
            (&actors.fixture.binding.agent_did, PARENT, false),
            (&actors.child_did, CHILD, true),
        ] {
            let mut tools: Tools = serde_json::from_value(
                live.access
                    .transact_readonly("test.live_job.tools", |txn| {
                        let node = &live.node_did;
                        let tools_id = format!("{id}:tools");
                        Box::pin(async move {
                            Ok::<_, anyhow::Error>(
                                gents_documents::read_desired_state_record_in_txn(
                                    txn,
                                    Collection::Tools,
                                    node,
                                    &tools_id,
                                )
                                .await?
                                .unwrap()
                                .1,
                            )
                        })
                    })
                    .await
                    .unwrap(),
            )
            .unwrap();
            tools.host = Some(HostTools {
                root: Some(root.to_str().unwrap().into()),
                bash: child.then(|| BashTools {
                    mode: BashMode::ReadOnly,
                    read_only_commands: Some(vec!["sh".into()]),
                    ..Default::default()
                }),
                ..Default::default()
            });
            crate::support::fixtures::configure_agent_tools_as(
                &live.access, &live.node_did, agent,
                Some("Follow the current user's exact tool instructions. Never repeat a call after a receipt or a refusal. Acknowledge completion briefly without tools.".into()),
                tools, Vec::new(),
            ).await;
        }
        gents_tools::surface::set_workspace_root_with_readers(
            &live.access,
            &live.node_did,
            root.to_str().unwrap(),
            None,
            true,
            &[
                actors.fixture.binding.agent_did.clone(),
                actors.child_did.clone(),
            ],
        )
        .await
        .unwrap();
        let (runtime, _) = boot_live_agent_with_options(
            &db,
            db.node_identity.clone(),
            DocumentRuntimeOptions {
                plugin_home: Some(home.path().to_owned()),
                tool_ceiling: ToolCeiling::readonly_at(&root),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        Self {
            db,
            actors,
            live,
            home,
            cwd,
            runtime,
            release,
        }
    }

    fn held_prompt(&self) -> String {
        let args = json!({"command":"sh", "args":["-c", "IFS= read -r line < \"$1\"", "job-child", self.home.path().join("release")]});
        format!(
            "Call bash exactly once with {args}. Wait for its result, then acknowledge briefly."
        )
    }

    async fn invoke(&self, name: &str, plan: Vec<PlanEntry>) -> Value {
        self.invoke_in_session(name, name, plan).await
    }

    async fn invoke_in_session(&self, session: &str, name: &str, plan: Vec<PlanEntry>) -> Value {
        let plan = tokio::task::spawn_blocking(move || {
            plan.iter().map(PlanEntry::to_json).collect::<Vec<_>>()
        })
        .await
        .unwrap();
        let arguments = json!({"args":{"plan":plan}});
        submit_with_cwd(&self.live.access, &self.actors.fixture, &self.live.node_did, session, name,
            format!("Call job_fixture exactly once with these exact arguments: {arguments}. Acknowledge the receipt or refusal and end the turn. Do not retry, inspect, or start another job."), &self.cwd).await;
        assert_eq!(
            wait_for_request_terminal(&self.live.access, name, Duration::from_secs(120)).await,
            "completed"
        );
        let rows = self.live.access.execute(&format!(
            r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "job_fixture"}}}}) {{_docID tool_call_id lifecycle_state}} }}"#,
            escape_graphql_string(name),
        )).await.unwrap();
        let rows = rows["data"]["AgentToolCall"].as_array().unwrap();
        rows.first()
            .expect("the requested job produced a durable receipt or refusal")
            .clone()
    }

    async fn job(&self, row: &Value, until: impl Fn(&JobView) -> bool) -> JobView {
        let mut changes = self.live.access.subscribe_changes("AgentToolCall").await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            let view = load_job_view(
                &self.live.access,
                None,
                JobKey::RowDocId(row["_docID"].as_str().unwrap().into()),
            )
            .await
            .unwrap();
            if until(&view) {
                return view;
            }
            tokio::time::timeout_at(deadline, changes.recv())
                .await
                .expect("job did not reach its required state");
        }
    }

    async fn child_rows(&self, row: &Value) -> Vec<Value> {
        self.live.access.execute(&format!(
            r#"{{ AgentRequest(filter: {{caused_by_parent_tool_call_doc_id: {{_eq: "{}"}}}}) {{_docID request_id lifecycle_state failure_reason interrupt_requested_at}} }}"#,
            escape_graphql_string(row["_docID"].as_str().unwrap()),
        )).await.unwrap()["data"]["AgentRequest"].as_array().unwrap().clone()
    }

    async fn wait_held(&self, row: &Value, count: usize) {
        self.job(row, |view| view.children.len() == count).await;
        let children = self.child_rows(row).await;
        assert_eq!(children.len(), count);
        for child in children {
            let query = format!(
                r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{}"}}, tool_name: {{_eq: "bash"}}}}) {{lifecycle_state}} }}"#,
                escape_graphql_string(child["request_id"].as_str().unwrap())
            );
            wait_for_rows(
                &self.live.access,
                "AgentToolCall",
                &query,
                Duration::from_secs(120),
                |data| {
                    data["AgentToolCall"].as_array().is_some_and(|rows| {
                        rows.iter().any(|row| row["lifecycle_state"] == "running")
                    })
                },
            )
            .await;
        }
    }

    async fn refusal(&self, row: &Value) {
        assert_eq!(row["lifecycle_state"], "failed");
        assert!(self.child_rows(row).await.is_empty());
    }
}

fn entry(when: Value, output: gents_protocol::job::JobStepOutput) -> PlanEntry {
    PlanEntry {
        when,
        output,
        control: None,
    }
}

async fn holding_plan(handle: &str, spawns: Vec<gents_protocol::job::JobSpawn>) -> Vec<PlanEntry> {
    let handle = handle.to_owned();
    let pin = tokio::task::spawn_blocking(move || guest_pin(&handle))
        .await
        .unwrap();
    let mut entries = vec![entry(json!({"step":0}), effects(Some(pin), spawns.clone()))];
    entries.extend(spawns.iter().map(|child| {
        entry(
            json!({"children":{&child.slot:"live"}}),
            effects(None, Vec::new()),
        )
    }));
    entries.push(entry(json!({}), result(json!({"done":true}))));
    entries
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spawn_cwd_with_dotdot_symlink_or_outside_caller_cwd_fails_closed_with_no_child() {
    let h = LiveJobs::boot("live-job-cwd-policy").await;
    let outside = h.home.path().join("outside");
    tokio::fs::create_dir(&outside).await.unwrap();
    let link = h.cwd.join("link");
    let target = outside.clone();
    tokio::task::spawn_blocking(move || std::os::unix::fs::symlink(target, link))
        .await
        .unwrap()
        .unwrap();
    for (name, cwd) in [
        ("dotdot", "../outside".to_owned()),
        ("symlink", "link".to_owned()),
        ("absolute", outside.to_str().unwrap().to_owned()),
    ] {
        let mut child = spawn("a", "Acknowledge briefly.");
        child.cwd = Some(cwd);
        let row = h
            .invoke(
                &format!("live-job-cwd-{name}"),
                holding_plan(name, vec![child]).await,
            )
            .await;
        h.refusal(&row).await;
    }
    h.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn job_plan_addressed_to_a_peer_agent_is_refused_before_any_child() {
    let h = LiveJobs::boot("live-job-peer-policy").await;
    let peer = test_db("live-job-peer-target").await;
    h.live
        .apply(
            "test.live_job.peer_target",
            vec![(
                Collection::AgentTarget,
                serde_json::to_value(crate::support::fixtures::agent_target(
                    &h.live.node_did,
                    CHILD,
                    peer.node_identity.did(),
                    &h.actors.child_did,
                ))
                .unwrap(),
            )],
        )
        .await;
    let row = h
        .invoke(
            "live-job-peer-refusal",
            holding_plan("peer", vec![spawn("a", "Acknowledge briefly.")]).await,
        )
        .await;
    h.refusal(&row).await;
    h.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_cwd_outside_the_operator_root_is_refused_by_the_child_admission() {
    let h = LiveJobs::boot("live-job-moved-root").await;
    let pin = tokio::task::spawn_blocking(|| guest_pin("root"))
        .await
        .unwrap();
    let plan = vec![
        entry(
            json!({"step":0}),
            effects(Some(pin), vec![spawn("a", &h.held_prompt())]),
        ),
        entry(
            json!({"children":{"b":"failed"}}),
            result(json!({"refused":true})),
        ),
        entry(json!({"children":{"b":"live"}}), effects(None, Vec::new())),
        entry(
            json!({"children":{"a":"completed"}}),
            effects(None, vec![spawn("b", "Acknowledge briefly.")]),
        ),
        entry(json!({"children":{"a":"live"}}), effects(None, Vec::new())),
    ];
    let row = h.invoke("live-job-root-move", plan).await;
    h.wait_held(&row, 1).await;
    let outside = tokio::task::spawn_blocking(tempfile::tempdir)
        .await
        .unwrap()
        .unwrap();
    tokio::fs::rename(&h.cwd, h.home.path().join("work-before"))
        .await
        .unwrap();
    let target = outside.path().to_owned();
    let link = h.cwd.clone();
    tokio::task::spawn_blocking(move || std::os::unix::fs::symlink(target, link))
        .await
        .unwrap()
        .unwrap();
    let mut release = h.release.try_clone().unwrap();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        release.write_all(b"release\n").unwrap();
    })
    .await
    .unwrap();
    h.finish_refused_child(row).await;
}

impl LiveJobs {
    async fn finish_refused_child(self, row: Value) {
        let view = self
            .job(&row, |view| {
                view.state == JobState::Completed
                    && view
                        .children
                        .iter()
                        .any(|child| child.slot == "b" && child.state == JobChildState::Failed)
            })
            .await;
        assert_eq!(view.state, JobState::Completed);
        let children = self.child_rows(&row).await;
        let failed = children
            .iter()
            .find(|child| child["lifecycle_state"] == "failed")
            .expect("escaped child was refused");
        assert!(failed["failure_reason"]
            .as_str()
            .unwrap()
            .contains("escapes operator tool root"));
        self.runtime.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operator_cancel_and_non_job_stop_each_end_the_row_cancelled() {
    for operator in [true, false] {
        let name = if operator {
            "live-job-operator-cancel"
        } else {
            "live-job-plugin-stop"
        };
        let h = LiveJobs::boot(name).await;
        let row = h
            .invoke(
                name,
                holding_plan(
                    name,
                    vec![spawn("a", &h.held_prompt()), spawn("b", &h.held_prompt())],
                )
                .await,
            )
            .await;
        h.wait_held(&row, 2).await;
        if operator {
            let access = ConfigAccess::local_managed_agent(
                h.db.node.clone(),
                identity::Did::new(h.actors.fixture.agent.did()).unwrap(),
                h.actors.fixture.agent.as_ref(),
            )
            .unwrap();
            let outcome = cancel_background_tool_call(
                &access,
                &BackgroundExecutionRegistry::default(),
                &gents_automation::goal::session_message::SessionGoals,
                name,
                row["tool_call_id"].as_str().unwrap(),
            )
            .await
            .unwrap();
            assert!(matches!(
                outcome,
                CancelBackgroundToolCallOutcome::Cancelled { .. }
            ));
        } else {
            let mut stop = entry(json!({"step":0}), result(json!({"stopped":true})));
            stop.control = Some(JobControlRequest {
                row_doc_id: row["_docID"].as_str().unwrap().into(),
                kind: JobControlKind::Stop,
            });
            h.invoke_in_session(name, "live-job-stop-request", vec![stop])
                .await;
        }
        let view = h.job(&row, |view| view.state == JobState::Cancelled).await;
        for child in view.children {
            assert_eq!(
                wait_for_request_terminal(
                    &h.live.access,
                    &child.request_id,
                    Duration::from_secs(120)
                )
                .await,
                "interrupted"
            );
        }
        let query = format!(
            r#"{{ AgentMessage(filter: {{session_id: {{_eq: "{}"}}}}) {{publication}} }}"#,
            escape_graphql_string(name)
        );
        wait_for_rows(
            &h.live.access,
            "AgentMessage",
            &query,
            Duration::from_secs(120),
            |data| {
                data["AgentMessage"].as_array().is_some_and(|rows| {
                    rows.iter().any(|message| {
                        message["publication"]["kind"] == "tool_delivery"
                            && message["publication"]["tool_call_doc_id"] == row["_docID"]
                    })
                })
            },
        )
        .await;
        h.runtime.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn nine_concurrent_adoptions_on_one_request_leave_exactly_eight() {
    let h = LiveJobs::boot("live-job-adoption-cap").await;
    let mut plans = Vec::new();
    for index in 0..9 {
        plans.push(
            holding_plan(
                &format!("live-cap-{index}"),
                vec![spawn("child", &h.held_prompt())],
            )
            .await,
        );
    }
    let arguments = tokio::task::spawn_blocking(move || {
        plans
            .into_iter()
            .map(|plan| {
                json!({"args":{"plan":plan.iter().map(PlanEntry::to_json).collect::<Vec<_>>()}})
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap();
    const REQUEST: &str = "live-job-adoption-cap";
    submit_with_cwd(
        &h.live.access,
        &h.actors.fixture,
        &h.live.node_did,
        REQUEST,
        REQUEST,
        format!(
            "Call job_fixture nine times in parallel in your next response, once for each of these nine distinct argument objects: {}. Emit all nine calls together. Keep each object's plan intact and do not combine them. After all results arrive, acknowledge and end your turn. Do not repeat any call, including a refused call, and do not stop any job.",
            serde_json::to_string(&arguments).unwrap(),
        ),
        &h.cwd,
    )
    .await;
    assert_eq!(
        wait_for_request_terminal(&h.live.access, REQUEST, Duration::from_secs(120)).await,
        "completed"
    );
    let rows = h.live.access.execute(&format!(
        r#"{{ AgentToolCall(filter: {{request_id: {{_eq: "{REQUEST}"}}, tool_name: {{_eq: "job_fixture"}}}}) {{_docID tool_call_id lifecycle_state}} }}"#,
    )).await.unwrap();
    let rows = rows["data"]["AgentToolCall"].as_array().unwrap();
    assert_eq!(rows.len(), 9, "all nine jobs were submitted: {rows:?}");
    let running = rows
        .iter()
        .filter(|row| row["lifecycle_state"] == "running")
        .collect::<Vec<_>>();
    let refused = rows
        .iter()
        .filter(|row| row["lifecycle_state"] == "failed")
        .collect::<Vec<_>>();
    assert_eq!(running.len(), 8, "the parent retained its eight-job budget");
    assert_eq!(refused.len(), 1, "the ninth adoption was refused");
    for row in running {
        h.wait_held(row, 1).await;
        let view = h.job(row, |view| view.state == JobState::Running).await;
        assert_eq!(view.children.len(), 1);
        assert_eq!(view.children[0].state, JobChildState::Live);
    }
    h.refusal(refused[0]).await;
    let result = gents_session::tool_call::query::result::load_tool_call_result(
        &h.live.access,
        refused[0]["_docID"].as_str().unwrap(),
        &h.live.node_did,
        REQUEST,
        Some(h.actors.fixture.user.did()),
    )
    .await
    .unwrap();
    let refusal: Value = serde_json::from_str(
        &gents_session::tool_call::query::result::render_tool_result(&result).unwrap(),
    )
    .unwrap();
    assert_eq!(refusal["code"], "background_tool_budget_exceeded");
    h.runtime.shutdown().await;
}
