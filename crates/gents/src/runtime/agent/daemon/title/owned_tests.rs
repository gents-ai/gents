use std::sync::{Arc, Mutex};
use std::time::Duration;

use defra_node::Node as EmbeddedNode;
use gents_loop::test_support::scripted_provider::{DispatchGate, Reply, ScriptedProvider, Step};
use gents_protocol::message::ReasoningContent;
use gents_protocol::output::extent::inspect_open_source;
use gents_protocol::output::live::reconstruct_dense_prefix;
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::{
    OutputOutcome, OutputSource, OutputWriter, PayloadRef, SourceClose, StreamPayload,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use rig::streaming::RawStreamingChoice;
use tokio::sync::watch;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::{AgentDaemon, AuxiliaryTask};
use crate::runtime::agent::daemon::AgentDaemonInit;
use crate::runtime::agent::runtime::{
    run_router_with_watcher, RouterInputs, RuntimeAdmissionGate, StartupBarrier,
    UNRESOLVED_SESSION_ATTEMPT_BOUND,
};
use crate::runtime::status::{RuntimeStatusHandle, RuntimeStatusOwner};
use crate::turn::prompt::LayeredPromptBuilder;
use gents_automation::triggers::runtime_snapshot::ActiveRuntimeSnapshot;
use gents_inference::backends::provider::BackendProviderKind;
use gents_inference::resolved::ResolvedAgent;
use gents_lean_vocab::canonical_wire as wire;
use gents_protocol::output::rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
use gents_protocol::row::AgentRequest;
use gents_session::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use gents_session::request::watcher::DefraWatcher;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;
use gents_store::test_support::read_when_changed;
use gents_toolcalls::call::hook::background_executions::BackgroundExecutionRegistry;
use gents_toolcalls::call::hook::{BackgroundToolRegistry, FailurePolicy};
use gents_tools::plugin::approval::workspace;
use gents_tools::surface::AgentToolSurfaceConfig;

/// Answers every review with `reply`, or by allowing the invocation its
/// prompt names, and charges three input and five output tokens.
fn review_provider(reply: Option<String>) -> ScriptedProvider {
    ScriptedProvider::responding(move |dispatch| {
        let reply = reply.clone().unwrap_or_else(|| {
            let action_id = dispatch.request.chat_history.iter().find_map(|message| {
                let rig::message::Message::User { content } = message else { return None; };
                content.iter().find_map(|part| {
                    let rig::message::UserContent::Text(text) = part else { return None; };
                    serde_json::from_str::<serde_json::Value>(&text.text).ok()?
                        .get("actionId")?.as_str().map(str::to_owned)
                })
            }).expect("review prompt names its invocation");
            serde_json::json!({"actionId":action_id,"decision":"allow","reason":"Within the requested work."}).to_string()
        });
        Reply::Stream(vec![
            Step::Chunk(RawStreamingChoice::Message(reply)),
            Step::usage_end(rig::completion::Usage {
                input_tokens: 3,
                output_tokens: 5,
                total_tokens: 8,
                cached_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }),
        ])
    })
    .capturing_openai("scripted")
}

fn review_input(action_id: &str) -> String {
    serde_json::to_string(&super::review::ReviewInput {
        action_id: action_id.into(),
        policy: "Allow routine project work. Ask about unrelated changes.".into(),
        action: serde_json::json!({ "command": ["cargo", "test"] }),
        conversation: vec!["Fix the failing project test.".into()],
        omitted_history_entries: 0,
    })
    .unwrap()
}

fn review_execution(
    fixture: &TitleFixture,
    action_id: &str,
    budget: Option<crate::runtime::agent::loop_stream::AggregateTokenBudget>,
) -> super::review::ReviewExecution {
    super::review::ReviewExecution {
        action_id: action_id.into(),
        parent_request_doc_id: fixture.parent.doc_id.clone(),
        aggregate_token_budget: budget,
        cancellation: tokio_util::sync::CancellationToken::new(),
    }
}

async fn drain_reviews(tasks: &super::super::auxiliary_tasks::AuxiliaryTasks, success: bool) {
    tasks.close();
    let result = tokio::time::timeout(Duration::from_secs(10), tasks.join_next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(result.is_ok(), success, "{result:?}");
    assert!(tasks.join_next().await.is_none());
}

struct ReviewWorkspace {
    fixture: TitleFixture,
    selection: workspace::ReviewTaskSelection,
    saved: workspace::Settings,
    home: tempfile::TempDir,
    _project: tempfile::TempDir,
    key: workspace::WorkspaceKey,
    tasks: super::super::auxiliary_tasks::AuxiliaryTasks,
    _shutdown: watch::Sender<bool>,
    reviewer: super::review::WorkspaceReviewer,
    context: workspace::Context,
}

impl ReviewWorkspace {
    async fn setup<M, F>(
        prompt: &str,
        parent_limit: Option<u64>,
        budget: Option<crate::runtime::agent::loop_stream::AggregateTokenBudget>,
        cancellation: tokio_util::sync::CancellationToken,
        model_for: F,
    ) -> Self
    where
        M: gents_inference::llm::rig_compat::ProviderModel,
        F: FnOnce(&TitleFixture) -> M,
    {
        let fixture = TitleFixture::with_auxiliary(
            RequestPurpose::AccessReview,
            Some(review_input("unused")),
            parent_limit,
        )
        .await;
        let selection = install_review_policy(&fixture, prompt).await;
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let key = workspace::WorkspaceKey::canonical(
            &fixture.parent.node_did,
            fixture.parent.requester_did.as_deref().unwrap(),
            &fixture.parent.agent_did,
            project.path(),
        )
        .unwrap();
        let saved = workspace::save_mode(
            home.path(),
            &key,
            0,
            workspace::Mode::AutoReview,
            Some(&selection),
        )
        .unwrap();
        let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let reviewer = fixture.task(model_for(&fixture), true).workspace_reviewer(
            &fixture.parent,
            budget,
            cancellation,
            shutdown_rx,
            tasks.clone(),
        );
        let context = workspace::for_request(
            Some(home.path()),
            &fixture.parent.node_did,
            fixture.parent.requester_did.as_deref(),
            &fixture.parent.agent_did,
            Some(project.path()),
            None,
        )
        .unwrap()
        .unwrap()
        .with_reviewer(reviewer.callback.clone());
        Self {
            fixture,
            selection,
            saved,
            home,
            _project: project,
            key,
            tasks,
            _shutdown: shutdown,
            reviewer,
            context,
        }
    }

    fn approval_call(&self) -> gents_loop::tool::ToolCallEnvelope {
        gents_tools::runtime_context::ToolHostContext {
            approval: Some(self.context.clone()),
            ..Default::default()
        }
        .call()
    }
}

#[tokio::test]
async fn access_review_uses_the_caller_ledger_without_tools_or_chat_publication() {
    let action_id = uuid::Uuid::new_v4().to_string();
    let fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input(&action_id)),
        Some(10_000),
    )
    .await;
    let reply = serde_json::json!({ "actionId": action_id, "decision": "allow", "reason": "The command checks the project." }).to_string();
    let provider = review_provider(Some(reply.clone()));
    let observed = provider.clone();
    let budget = crate::runtime::agent::loop_stream::AggregateTokenBudget::new(10_000);
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let result = fixture
        .task(fixture.admitted(provider), true)
        .run_review(
            fixture.title.clone(),
            review_execution(&fixture, &action_id, Some(budget.clone())),
            shutdown_rx,
            &tasks,
        )
        .await
        .unwrap();
    assert_eq!(result.as_deref(), Some(reply.as_str()));
    drain_reviews(&tasks, true).await;
    assert_eq!(budget.snapshot().unwrap().used, 8);
    let recovered = crate::turn::completion_factory::aggregate_token_budget_for_request(
        &fixture.agent_access(),
        &fixture.parent,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        recovered.snapshot().unwrap().used,
        budget.snapshot().unwrap().used
    );
    {
        let requests = observed.requests();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].tools.is_empty());
        assert_eq!(requests[0].chat_history.len(), 2);
        assert_eq!(
            requests[0]
                .chat_history
                .iter()
                .filter(|message| matches!(message, rig::message::Message::User { .. }))
                .count(),
            1
        );
        assert_eq!(
            requests[0]
                .chat_history
                .iter()
                .filter(|message| matches!(message, rig::message::Message::System { .. }))
                .count(),
            1
        );
    }
    assert_eq!(fixture.session_message_count().await, 0);
    assert_eq!(
        gents_automation::goal::session_token_usage(
            &fixture.node,
            &identity::Did::new(&fixture.parent.agent_did).unwrap(),
            &fixture.parent.node_did,
            &fixture.parent.session_id,
        )
        .await
        .unwrap(),
        8,
    );
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Completed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
    let (output, published) = fixture.output_rows().await;
    assert_eq!(published, 0);
    assert!(!output.is_empty());
    for row in output {
        assert!(
            matches!(row.segment.source, OutputSource::ProviderTurn { scope, .. } if scope.kind == gents_inference::rendered_request::CaptureScopeKind::AccessReview)
        );
    }
    assert!(fixture.session_title().await.is_null());
    let response = fixture
        .agent_access()
        .execute("{ AgentToolCall { _docID } }")
        .await
        .unwrap();
    assert!(response["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn goal_usage_rejects_a_review_without_its_same_session_parent() {
    let fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input("goal-review-scope")),
        Some(10_000),
    )
    .await;
    let doc = gents_store::graphql::escape_graphql_string(&fixture.parent.doc_id);
    fixture
        .agent_access()
        .write(
            "test.goal_review_parent",
            &format!(
                r#"mutation {{ delete_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}) {{ _docID }} }}"#
            ),
        )
        .await
        .unwrap();
    let error = gents_automation::goal::session_token_usage(
        &fixture.node,
        &identity::Did::new(&fixture.parent.agent_did).unwrap(),
        &fixture.parent.node_did,
        &fixture.parent.session_id,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("goal review has no same-session parent"));
}

async fn install_review_policy(
    fixture: &TitleFixture,
    prompt: &str,
) -> workspace::ReviewTaskSelection {
    use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let document = serde_json::json!({
        "node_did": fixture.parent.node_did,
        "task_id": "review",
        "agent_did": fixture.parent.agent_did,
        "prompt_template": prompt,
    });
    let selection = workspace::ReviewTaskSelection::for_task(
        &serde_json::from_value(document.clone()).unwrap(),
    );
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: gents_store::collection::Collection::Task,
        add: document.clone(),
        update: document,
    }])
    .unwrap();
    fixture
        .user_access
        .transact_write("test.review_policy", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    selection
}

/// A User-created Task is editable by its Agent only through the selected
/// self-configuration writer grant: the User's explicit
/// `grant_selected_self_configuration` plus the Task's link to that role.
async fn grant_agent_self_configuration_on_review_task(fixture: &TitleFixture) {
    let node_did = fixture.parent.node_did.clone();
    let agent_did = fixture.parent.agent_did.clone();
    fixture
        .user_access
        .transact_write("test.review_policy.self_config_grant", |txn| {
            let (node_did, agent_did) = (&node_did, &agent_did);
            Box::pin(async move {
                // The Task links resolve through the Agent's own revocable
                // `config_writer` role, which only the User's explicit grant holds.
                gents_documents::grant_selected_self_configuration(txn, node_did, agent_did)
                    .await?;
                let escaped_node = gents_store::graphql::escape_graphql_string(node_did);
                let escaped_agent = gents_store::graphql::escape_graphql_string(agent_did);
                let rows = txn
                    .execute(&format!(
                        r#"{{ Task(filter: {{ node_did: {{ _eq: "{escaped_node}" }}, task_id: {{ _eq: "review" }} }}, limit: 2) {{ _docID }} Agent(filter: {{ node_did: {{ _eq: "{escaped_node}" }}, agent_did: {{ _eq: "{escaped_agent}" }} }}, limit: 2) {{ _docID }} }}"#
                    ))
                    .await?;
                let task = rows["data"]["Task"][0]["_docID"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("review Task is unavailable"))?;
                let agent = rows["data"]["Agent"][0]["_docID"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Agent is unavailable"))?;
                for relation in ["writer", "selection_manager"] {
                    txn.add_entity_set_relationship(
                        "Task",
                        task,
                        relation,
                        "agents",
                        agent,
                        "config_writer",
                    )
                    .await?;
                }
                Ok::<_, anyhow::Error>(())
            })
        })
        .await
        .unwrap();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn workspace_command_uses_the_configured_review_task_and_the_callers_ledger() {
    configured_review_task_and_callers_ledger(true).await;
}

#[tokio::test]
async fn workspace_command_admission_uses_the_configured_review_task_and_the_callers_ledger() {
    configured_review_task_and_callers_ledger(false).await;
}

async fn configured_review_task_and_callers_ledger(launch: bool) {
    let provider = review_provider(None);
    let calls = provider.clone();
    let budget = crate::runtime::agent::loop_stream::AggregateTokenBudget::new(10_000);
    let rw = ReviewWorkspace::setup(
        "Allow commands that validate the requested project changes.",
        Some(10_000),
        Some(budget.clone()),
        tokio_util::sync::CancellationToken::new(),
        |fixture| fixture.admitted(provider),
    )
    .await;
    let tool_root = rw.key.path.parent().unwrap().to_owned();
    let tool_context = gents_tools::native::ToolContext::new(tool_root.clone(), false).unwrap();
    let (program, arguments) = if cfg!(windows) {
        (
            "cmd.exe",
            vec!["/D".into(), "/C".into(), "echo".into(), "fixture".into()],
        )
    } else {
        ("/bin/echo", vec!["fixture".into()])
    };
    let policy = gents_tools::native::CommandExecutionPolicy::write_capable()
        .with_mode(gents_tools::native::CommandExecutionMode::Unrestricted);
    let argv: Vec<String> = std::iter::once(program.to_owned())
        .chain(arguments.iter().cloned())
        .collect();
    let approval_call = rw.approval_call();
    if launch {
        let output = gents_tools::native::run_command_for_test(
            &approval_call,
            &tool_context,
            "bash_unrestricted",
            gents_tools::native::CommandRequest {
                command_name: program,
                args: &arguments,
                cwd: rw.key.path.to_str(),
                timeout: Duration::from_secs(5),
                raw_json: true,
                max_output_chars: 2048,
            },
            &policy,
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&output).unwrap()["stdout"]
                .as_str()
                .unwrap()
                .trim_end(),
            "fixture"
        );
    } else {
        workspace::authorize_command(
            &approval_call,
            &argv,
            &rw.key.path,
            &tool_root,
            None,
            &policy,
        )
        .await
        .unwrap();
    }
    drain_reviews(&rw.tasks, true).await;
    assert!(rw.reviewer.take_failure().is_none());
    assert_eq!(budget.snapshot().unwrap().used, 8);
    {
        let calls = calls.requests();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].tools.is_empty());
        let input = calls[0]
            .chat_history
            .iter()
            .find_map(|message| {
                let rig::message::Message::User { content } = message else {
                    return None;
                };
                content.iter().find_map(|part| {
                    let rig::message::UserContent::Text(text) = part else {
                        return None;
                    };
                    serde_json::from_str::<super::review::ReviewInput>(&text.text).ok()
                })
            })
            .unwrap();
        assert_eq!(input.action["workspace"], serde_json::json!(rw.key.path));
        assert_eq!(
            input.action["subject"]["root"],
            serde_json::json!(tool_root)
        );
    }
    assert_eq!(rw.fixture.session_message_count().await, 0);
    assert!(
        gents_tools::plugin::approval::pending_requests(rw.home.path())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn changing_the_review_task_while_it_runs_discards_its_allowance() {
    let gate = Arc::new(DispatchGate::default());
    let provider = review_provider(None).gated(gate.clone());
    let calls = provider.clone();
    let rw = ReviewWorkspace::setup(
        "Review the requested project changes.",
        None,
        None,
        tokio_util::sync::CancellationToken::new(),
        |fixture| fixture.admitted(provider),
    )
    .await;
    let project_path = rw.key.path.clone();
    let approval_call = rw.approval_call();
    let consent = tokio::spawn(async move {
        workspace::authorize_command(
            &approval_call,
            &["echo".into(), "fixture".into()],
            &project_path,
            &project_path,
            None,
            &gents_tools::native::CommandExecutionPolicy::write_capable(),
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
        .await
        .unwrap();
    install_review_policy(&rw.fixture, "Ask before running commands.").await;
    gate.release.notify_one();
    let error = consent.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("not approved"));
    drain_reviews(&rw.tasks, true).await;
    assert!(rw.reviewer.take_failure().is_none());
    assert_eq!(calls.calls(), 1);
    assert!(
        gents_tools::plugin::approval::pending_requests(rw.home.path())
            .unwrap()
            .is_empty()
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn configuration_edits_cannot_replace_the_selected_review_policy() {
    selected_review_policy_survives_configuration_edits(true).await;
}

#[tokio::test]
async fn command_admission_cannot_replace_the_selected_review_policy() {
    selected_review_policy_survives_configuration_edits(false).await;
}

async fn selected_review_policy_survives_configuration_edits(launch: bool) {
    let provider = review_provider(None);
    let calls = provider.clone();
    let rw = ReviewWorkspace::setup(
        "Ask before making changes outside the requested project.",
        None,
        None,
        tokio_util::sync::CancellationToken::new(),
        |fixture| fixture.admitted(provider),
    )
    .await;
    grant_agent_self_configuration_on_review_task(&rw.fixture).await;
    let tools = gents_tools::builtin::self_config::build_self_config_tools(
        std::sync::Arc::default(),
        rw.fixture.node.clone(),
        rw.fixture.parent.node_did.clone(),
        Some(rw.fixture.agent_config.agent_identity.clone()),
        &gents_tools::surface::SelfConfigToolConfig {
            enabled: true,
            agent_did: rw.fixture.parent.agent_did.clone(),
            categories: std::collections::BTreeSet::from(["automation".into()]),
            ..Default::default()
        },
        Arc::new(gents_toolcalls::plugin::executor::PluginExecutor::new(
            Some(rw.home.path().to_owned()),
        )),
        Arc::new(gents_automation::triggers::task_run::TaskRuns),
    );
    let config = tools
        .iter()
        .find(|tool| tool.name() == gents_protocol::tool_names::CONFIG_TOOL_NAME)
        .unwrap();
    let tool_context = gents_tools::native::ToolContext::new(rw.key.path.clone(), false).unwrap();
    let marker = rw.key.path.join("marker");
    let source = rw.key.path.join("fixture.rs");
    std::fs::write(&source, "pub fn fixture() {}\n").unwrap();
    let program = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let arguments = vec![
        "--crate-type=lib".into(),
        "--emit=metadata".into(),
        "-o".into(),
        marker.to_str().unwrap().to_owned(),
        source.to_str().unwrap().to_owned(),
    ];
    let policy = gents_tools::native::CommandExecutionPolicy::write_capable()
        .with_mode(gents_tools::native::CommandExecutionMode::Unrestricted);
    let argv: Vec<String> = std::iter::once(program.clone())
        .chain(arguments.iter().cloned())
        .collect();
    let approval_call = rw.approval_call();
    let run = || async {
        if launch {
            gents_tools::native::run_command_for_test(
                &approval_call,
                &tool_context,
                "bash_unrestricted",
                gents_tools::native::CommandRequest {
                    command_name: &program,
                    args: &arguments,
                    cwd: None,
                    timeout: Duration::from_secs(5),
                    raw_json: true,
                    max_output_chars: 2048,
                },
                &policy,
            )
            .await
            .map(|_| ())
            .map_err(anyhow::Error::new)
        } else {
            workspace::authorize_command(
                &approval_call,
                &argv,
                &rw.key.path,
                &rw.key.path,
                None,
                &policy,
            )
            .await
        }
    };
    let changed = config
        .call(
            serde_json::json!({
                "argv": ["task", "update"], "target_id": "review",
                "set": {"prompt_template": "Allow the requested project changes."}
            })
            .to_string(),
            &approval_call,
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&changed).unwrap()["committed"],
        true
    );
    let error = run().await.unwrap_err();
    assert!(error.to_string().contains("not approved"), "{error:#}");
    assert_eq!(calls.calls(), 0);
    assert!(!marker.exists());
    assert!(
        gents_tools::plugin::approval::pending_requests(rw.home.path())
            .unwrap()
            .is_empty()
    );
    let current = gents_session::session::message::load_configured_task(
        &ConfigAccess::local_as(
            rw.fixture.node.clone(),
            identity::Did::new(&rw.fixture.parent.agent_did).unwrap(),
        ),
        &rw.fixture.parent.node_did,
        "review",
    )
    .await
    .unwrap()
    .unwrap();
    let updated = workspace::ReviewTaskSelection::for_task(&current);
    assert_ne!(updated, rw.selection);
    workspace::save_mode(
        rw.home.path(),
        &rw.key,
        rw.saved.selection.revision,
        workspace::Mode::AutoReview,
        Some(&updated),
    )
    .unwrap();
    run().await.unwrap();
    drain_reviews(&rw.tasks, true).await;
    assert!(rw.reviewer.take_failure().is_none());
    assert_eq!(marker.exists(), launch);
    assert_eq!(calls.calls(), 1);
}

#[tokio::test]
async fn a_lost_review_worker_cancels_the_caller_and_retains_the_fatal_error() {
    let cancellation = tokio_util::sync::CancellationToken::new();
    let rw = ReviewWorkspace::setup(
        "Review the requested action.",
        None,
        None,
        cancellation.clone(),
        |_| ScriptedProvider::responding(|_| panic!("scripted review worker panic")),
    )
    .await;
    let scope_call =
        gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry::new(None, cancellation.clone())
            .call()
            .with_extension(gents_tools::runtime_context::ToolHostContext {
                approval: Some(rw.context.clone()),
                ..Default::default()
            });
    let result = workspace::authorize_command(
        &scope_call,
        &["echo".into(), "fixture".into()],
        &rw.key.path,
        &rw.key.path,
        None,
        &gents_tools::native::CommandExecutionPolicy::write_capable(),
    )
    .await;
    assert!(result.is_err());
    assert!(cancellation.is_cancelled());
    assert!(super::super::ShutdownDrainFailure::occurs_in(
        &rw.reviewer.take_failure().unwrap()
    ));
    rw.tasks.close();
    let joined = tokio::time::timeout(Duration::from_secs(10), rw.tasks.join_next())
        .await
        .unwrap()
        .unwrap();
    assert!(joined.unwrap_err().is_panic());
    assert!(rw.tasks.join_next().await.is_none());
    assert!(
        gents_tools::plugin::approval::pending_requests(rw.home.path())
            .unwrap()
            .is_empty()
    );
    assert_eq!(rw.fixture.session_message_count().await, 0);
}

#[tokio::test]
async fn access_review_cannot_replace_a_budgeted_callers_ledger_or_invocation() {
    for wrong_invocation in [false, true] {
        let action_id = uuid::Uuid::new_v4().to_string();
        let fixture = TitleFixture::with_auxiliary(
            RequestPurpose::AccessReview,
            Some(review_input(&action_id)),
            Some(10_000),
        )
        .await;
        let provider = title_provider(
            vec![
                RawStreamingChoice::Message("not used".into()),
                RawStreamingChoice::FinalResponse(()),
            ],
            false,
        );
        let calls = provider.clone();
        let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
        let (_shutdown, shutdown_rx) = watch::channel(false);
        let execution = review_execution(
            &fixture,
            if wrong_invocation {
                "another-invocation"
            } else {
                &action_id
            },
            wrong_invocation
                .then(|| crate::runtime::agent::loop_stream::AggregateTokenBudget::new(10_000)),
        );
        let error = fixture
            .task(provider, true)
            .run_review(fixture.title.clone(), execution, shutdown_rx, &tasks)
            .await
            .unwrap_err();
        assert!(error.to_string().contains(if wrong_invocation {
            "another invocation"
        } else {
            "caller's token ledger"
        }));
        drain_reviews(&tasks, false).await;
        assert_eq!(calls.calls(), 0);
        assert_eq!(
            fixture.terminal_row().await,
            (
                RequestLifecycleState::Failed,
                Some(gents_protocol::output::TerminalOutput::NoMessage)
            )
        );
    }
}

#[tokio::test]
async fn access_review_without_reported_usage_invalidates_the_callers_allowance() {
    let action_id = uuid::Uuid::new_v4().to_string();
    let fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input(&action_id)),
        Some(10_000),
    )
    .await;
    let provider = title_provider(
        vec![
            RawStreamingChoice::Message(
                serde_json::json!({ "actionId": action_id, "decision": "allow", "reason": "Within the requested work." }).to_string(),
            ),
            RawStreamingChoice::FinalResponse(()),
        ],
        false,
    );
    let calls = provider.clone();
    let budget = crate::runtime::agent::loop_stream::AggregateTokenBudget::new(10_000);
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let error = fixture
        .task(fixture.admitted(provider), true)
        .run_review(
            fixture.title.clone(),
            review_execution(&fixture, &action_id, Some(budget.clone())),
            shutdown_rx,
            &tasks,
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("aggregate_token_usage_missing"));
    drain_reviews(&tasks, false).await;
    assert!(budget.snapshot().is_err());
    let recovered = crate::turn::completion_factory::aggregate_token_budget_for_request(
        &fixture.agent_access(),
        &fixture.parent,
    )
    .await
    .unwrap_err();
    assert!(recovered
        .to_string()
        .contains("requires reported provider usage"));
    assert_eq!(calls.calls(), 1);
    assert_eq!(fixture.session_message_count().await, 0);
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Failed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
}

#[tokio::test]
async fn dropping_an_access_review_caller_drains_received_output_before_join() {
    let action_id = uuid::Uuid::new_v4().to_string();
    let mut fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input(&action_id)),
        Some(10_000),
    )
    .await;
    Arc::get_mut(&mut fixture.agent_config)
        .unwrap()
        .stream_batch_ms = 5_000;
    let (provider, before_second, third_poll) = buffered_reasoning_title_provider();
    let calls = provider.clone();
    let task = fixture.task(provider, true);
    let request = fixture.title.clone();
    let budget = crate::runtime::agent::loop_stream::AggregateTokenBudget::new(10_000);
    let execution = review_execution(&fixture, &action_id, Some(budget.clone()));
    let caller_token = execution.cancellation.clone();
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let caller = tokio::spawn({
        let tasks = tasks.clone();
        async move {
            task.run_review(request, execution, shutdown_rx, &tasks)
                .await
        }
    });
    advance_to_buffered_second_chunk(&fixture, &before_second, &third_poll).await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    drain_reviews(&tasks, true).await;
    assert!(budget.snapshot().is_err());
    assert!(!caller_token.is_cancelled());
    assert_eq!(calls.calls(), 1);
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Interrupted,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
    assert_eq!(
        open_title_reasoning(&fixture).await.concat(),
        "first second"
    );
    assert_eq!(fixture.session_message_count().await, 0);
}

#[tokio::test]
async fn access_review_never_dispatches_a_requested_tool() {
    let action_id = uuid::Uuid::new_v4().to_string();
    let fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input(&action_id)),
        None,
    )
    .await;
    let provider = title_provider(
        vec![
            RawStreamingChoice::ToolCall(rig::streaming::RawStreamingToolCall::new(
                "call-1".into(),
                "write_file".into(),
                serde_json::json!({ "path": "unexpected", "content": "changed" }),
            )),
            RawStreamingChoice::FinalResponse(()),
        ],
        false,
    );
    let calls = provider.clone();
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    assert!(fixture
        .task(provider, true)
        .run_review(
            fixture.title.clone(),
            review_execution(&fixture, &action_id, None),
            shutdown_rx,
            &tasks
        )
        .await
        .is_err());
    drain_reviews(&tasks, false).await;
    assert_eq!(calls.calls(), 1);
    assert_eq!(fixture.session_message_count().await, 0);
    let response = fixture
        .agent_access()
        .execute("{ AgentToolCall { _docID } }")
        .await
        .unwrap();
    assert!(response["data"]["AgentToolCall"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn access_review_preserves_a_fenced_output_failure_for_the_caller_and_daemon() {
    let action_id = uuid::Uuid::new_v4().to_string();
    let fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input(&action_id)),
        None,
    )
    .await;
    let fields = stimulus_fields();
    let provider = fencing_title_provider(
        provider_events(&fields, true),
        fixture.agent_access(),
        fixture.title.doc_id.clone(),
        fixture.title.node_did.clone(),
    );
    // The claim reads the review's expiry at the instant the fixture wrote it,
    // however long the suite takes to reach this test.
    let written_at = fixture.written_at;
    let mut task = fixture.task(provider, true);
    task.expiry_clock = Arc::new(move || written_at);
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let error = task
        .run_review(
            fixture.title.clone(),
            review_execution(&fixture, &action_id, None),
            shutdown_rx,
            &tasks,
        )
        .await
        .unwrap_err();
    assert!(
        super::super::ShutdownDrainFailure::occurs_in(&error),
        "{error:#}"
    );
    tasks.close();
    let joined = tokio::time::timeout(Duration::from_secs(10), tasks.join_next())
        .await
        .unwrap()
        .unwrap();
    let error = super::super::auxiliary_task_join_result(joined).unwrap_err();
    assert!(
        super::super::ShutdownDrainFailure::occurs_in(&error),
        "{error:#}"
    );
    assert_eq!(
        fixture.terminal_row().await,
        (RequestLifecycleState::Processing, None)
    );
    assert_eq!(fixture.session_message_count().await, 0);
}

#[derive(Default)]
struct TitleWarningCapture(Arc<Mutex<Vec<&'static str>>>);

impl<S> Layer<S> for TitleWarningCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: LayerContext<'_, S>) {
        let metadata = event.metadata();
        if metadata.target() == crate::runtime::agent::daemon::title::TRACING_TARGET
            && *metadata.level() == tracing::Level::WARN
        {
            self.0.lock().unwrap().push(metadata.name());
        }
    }
}

/// Streams `events` on every call, then never answers again when `stall`.
fn title_provider(events: Vec<RawStreamingChoice<()>>, stall: bool) -> ScriptedProvider {
    ScriptedProvider::responding(move |_| {
        if stall {
            Reply::stalled(events.clone())
        } else {
            Reply::turn(events.clone())
        }
    })
    .capturing_openai("scripted")
}

async fn fence_request_generation(access: &ConfigAccess, doc_id: &str, node_did: &str) {
    let doc = gents_store::graphql::escape_graphql_string(doc_id);
    let owner = gents_store::graphql::escape_graphql_string(node_did);
    access
        .write(
            "test.title_fence_generation",
            &format!(
                r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }}, node_did: {{ _eq: "{owner}" }} }}, input: {{ execution_generation: "fenced-title-generation" }}) {{ _docID }} }}"#
            ),
        )
        .await
        .expect("fence title generation");
}

/// [`title_provider`] that first replaces the title request's execution
/// generation, as a competing owner would.
fn fencing_title_provider(
    events: Vec<RawStreamingChoice<()>>,
    access: ConfigAccess,
    doc_id: String,
    node_did: String,
) -> ScriptedProvider {
    ScriptedProvider::new([])
        .respond_async(move |_| {
            let (events, access, doc_id, node_did) = (
                events.clone(),
                access.clone(),
                doc_id.clone(),
                node_did.clone(),
            );
            async move {
                fence_request_generation(&access, &doc_id, &node_did).await;
                Reply::turn(events)
            }
        })
        .capturing_openai("scripted")
}

struct TitleFixture {
    _db: gents_store::test_support::TestStore,
    user_access: ConfigAccess,
    node: Arc<EmbeddedNode>,
    agent_config: Arc<ResolvedAgent>,
    tool_config: Arc<AgentToolSurfaceConfig>,
    identity: Arc<dyn NodeIdentity>,
    parent: AgentRequest,
    title: AgentRequest,
    /// When the auxiliary request was written; an access review expires 60
    /// seconds later.
    written_at: chrono::DateTime<chrono::Utc>,
}

impl TitleFixture {
    fn admitted<M>(&self, model: M) -> gents_inference::admission::AdmittedCompletionModel<M> {
        self.admitted_with(model, 0, "scripted", "scripted")
    }

    fn admitted_with<M>(
        &self,
        model: M,
        max_queue_depth: usize,
        config_fingerprint: &str,
        connection_key: &str,
    ) -> gents_inference::admission::AdmittedCompletionModel<M> {
        let registry = gents_inference::admission::AdmissionRegistry::new(self.node.clone());
        let backend = self.agent_config.backend_id.as_ref().unwrap();
        registry.reconcile(
            1,
            &rapidhash::RapidHashMap::from_iter([(
                backend.clone(),
                gents_inference::admission::BackendAdmissionConfig {
                    backend_id: backend.clone(),
                    max_concurrent: 1,
                    max_queue_depth,
                    enabled: true,
                    probe_status: "healthy".into(),
                    measured_unhealthy: false,
                    config_fingerprint: config_fingerprint.into(),
                },
            )]),
        );
        let mut model = gents_inference::admission::AdmittedCompletionModel::new(
            model,
            registry,
            connection_key,
            self.agent_config.completion_retry.max_rate_limit_wait(),
        );
        model.agent_signer = self.agent_config.agent_identity.managed_agent_signer();
        model
    }

    /// The parent Agent's managed access. Its requests, sessions and output rows
    /// are readable and writable only as that Agent.
    fn agent_access(&self) -> ConfigAccess {
        ConfigAccess::local_managed_agent(
            self.node.clone(),
            identity::Did::new(&self.agent_config.agent_did).unwrap(),
            self.agent_config.agent_identity.as_ref(),
        )
        .unwrap()
    }

    async fn new() -> Self {
        Self::with_auxiliary(RequestPurpose::TitleAudit, None, None).await
    }

    async fn with_auxiliary(
        purpose: RequestPurpose,
        content: Option<String>,
        parent_limit: Option<u64>,
    ) -> Self {
        use gents_store::config_client::NodePermission;
        let signed_home = gents_documents::test_support::SignedHome::enroll(
            crate::support::trial_store("title-owned").await,
            "title-owned",
        )
        .await;
        let binding = gents_inference::test_support::default_agent(&signed_home).await;
        let (db, _user, user_access) = signed_home.into_parts();
        let node = db.node.clone();
        let identity: Arc<dyn NodeIdentity> = db.node_identity.clone();
        let agent_did = binding.agent_did.clone();
        let agent_identity =
            gents_store::identity::resolve_agent_identity(identity.as_ref(), &binding).unwrap();
        let admin =
            ConfigAccess::local_as(node.clone(), identity::Did::new(identity.did()).unwrap());
        let agent_did_id = identity::Did::new(&agent_did).unwrap();
        for permission in [
            NodePermission::CollectionGet,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DocumentDelete,
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
        ] {
            admin
                .grant_node_permission(&agent_did_id, permission)
                .await
                .unwrap();
        }
        let principal = crate::support::fixtures::test_node_for(identity.clone(), &agent_did);
        let mut resolved =
            crate::support::fixtures::test_agent_with_identity(principal, agent_identity);
        resolved.backend_id = Some(format!("{agent_did}:backend"));
        resolved.backend_endpoint = "http://127.0.0.1:1/v1".into();
        resolved.model_name = "scripted".into();
        resolved.context_window = 8192;
        resolved.max_output_tokens = 1024;
        resolved.max_turns = 2;
        resolved.system_prompt = "system".into();
        resolved.stream_batch_ms = 0;
        resolved.stream_liveness_timeout = Duration::from_secs(60);
        resolved.deadline_duration = Duration::from_secs(120);
        let agent_config = Arc::new(resolved);
        let agent_access = ConfigAccess::local_managed_agent(
            node.clone(),
            agent_did_id,
            agent_config.agent_identity.as_ref(),
        )
        .unwrap();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            uuid::Uuid::new_v4().to_string(),
            RequestScope::new(
                identity.did(),
                &agent_config.agent_did,
                &agent_config.agent_did,
                uuid::Uuid::new_v4().to_string(),
            ),
            "explain the request",
            "interactive",
            created_at,
            AgentRequestAdmissionRecord::local_self(&agent_config.agent_did),
        );
        gents_session::request::admission::sign_agent_request_create(
            agent_config.agent_identity.as_ref(),
            &mut create,
        )
        .await
        .unwrap();
        let response = agent_access
            .write("test.title_parent", &create.graphql_mutation().unwrap())
            .await
            .unwrap();
        let parent_doc_id =
            gents_store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        let mut parent = gents_session::request::admission::load_request_for_admission_test_as(
            &node,
            &parent_doc_id,
            &agent_config.agent_did,
        )
        .await
        .unwrap();
        gents_session::test_support::create_agent_owned_session_at(
            &agent_access,
            &parent.node_did,
            &parent.session_id,
            parent.requester_did.as_deref(),
            &parent.created_at,
        )
        .await;
        if purpose == RequestPurpose::AccessReview {
            let mut lifecycle = RequestLifecycle::new_with_execution_binding(
                node.clone(),
                parent.clone(),
                agent_config.deadline_duration.as_secs(),
                gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                agent_config.backend_id.clone().unwrap_or_default(),
            );
            lifecycle
                .bind_managed_agent(agent_config.agent_identity.as_ref())
                .unwrap();
            lifecycle.set_configured_max_total_tokens(parent_limit);
            assert_eq!(
                lifecycle.claim_with_identity().await.unwrap(),
                ClaimOutcome::Claimed
            );
            let writer = gents_session::request::streaming::DefraStreamWriter::new_managed(
                node.clone(),
                agent_config.agent_identity.as_ref(),
                Duration::ZERO,
            )
            .unwrap();
            lifecycle.begin_owned_execution(&writer).await.unwrap();
            parent = lifecycle.request().clone();
        }
        let written_at = chrono::Utc::now();
        let title =
            gents_session::request::lifecycle::materialize::write_pending_auxiliary_request(
                &node,
                &parent,
                purpose,
                content.unwrap_or_else(|| parent.content.clone()),
                (purpose == RequestPurpose::AccessReview).then(|| {
                    (written_at + chrono::Duration::seconds(60))
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }),
                &agent_config.agent_identity.managed_agent_signer().unwrap(),
            )
            .await
            .unwrap();
        Self {
            _db: db,
            user_access,
            node,
            agent_config,
            tool_config: Arc::new(AgentToolSurfaceConfig::meta_only()),
            identity,
            parent,
            title,
            written_at,
        }
    }

    fn task<M>(&self, model: M, capture: bool) -> AuxiliaryTask<M>
    where
        M: gents_inference::llm::rig_compat::ProviderModel,
    {
        AuxiliaryTask {
            node: self.node.clone(),
            agent_config: self.agent_config.clone(),
            provider_family: None,
            model: Arc::new(model),
            service_admission: Default::default(),
            verifier: gents_session::request::admission::AgentRequestAdmissionVerifier::new(
                self.node.clone(),
                self.identity.clone(),
                gents_p2p::p2p_reconcile::enrollment_authority_channel().1,
            ),
            capture_factory: capture.then(|| {
                gents_inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                    self.node.clone(),
                    self.agent_config.agent_identity.as_ref(),
                )
                .unwrap()
            }),
            expiry_clock: Arc::new(chrono::Utc::now),
        }
    }

    async fn start_ready_daemon_parts(
        &self,
        provider: ScriptedProvider,
        include_agents: bool,
    ) -> (
        AgentDaemon<ScriptedProvider>,
        tokio::sync::mpsc::Sender<AgentRequest>,
        tokio::sync::mpsc::Receiver<AgentRequest>,
        Arc<ActiveRuntimeSnapshot>,
        RuntimeStatusHandle,
        RuntimeStatusOwner,
    ) {
        let prompt_builder = LayeredPromptBuilder::for_agent(
            &self.agent_config.system_prompt,
            &self.agent_config.agent_did,
            &[],
            false,
            &[],
        );
        let preamble = prompt_builder.preamble().to_string();
        let (status_owner, status) = RuntimeStatusHandle::start_with_unbounded_test_clock(
            self.node.clone(),
            self.identity.did().to_owned(),
        );
        status
            .initialize_startup(&self.agent_config.agent_did)
            .await
            .unwrap();
        status
            .readiness()
            .register_slot(&self.agent_config.agent_did, 1)
            .await
            .unwrap();
        let (dispatch, receiver) = tokio::sync::mpsc::channel(8);
        let snapshot = Arc::new(ActiveRuntimeSnapshot {
            placement_basis: Default::default(),
            generation: 1,
            node: None,
            local_did: String::new(),
            default_agent_did: self.agent_config.agent_did.clone(),
            tool_configs: if include_agents {
                rapidhash::RapidHashMap::from_iter([(
                    self.agent_config.agent_did.clone(),
                    self.tool_config.clone(),
                )])
            } else {
                Default::default()
            },
            agents: if include_agents {
                rapidhash::RapidHashMap::from_iter([(
                    self.agent_config.agent_did.clone(),
                    self.agent_config.clone(),
                )])
            } else {
                Default::default()
            },
            tool_surfaces: Default::default(),
            backend_admission_configs: Default::default(),
            unavailable_agents: Default::default(),
            active_schedules: Default::default(),
            unavailable_schedules: Default::default(),
            active_event_triggers: Default::default(),
            unavailable_event_triggers: Default::default(),
            active_tasks: Default::default(),
            dispatchers: rapidhash::RapidHashMap::from_iter([(
                self.agent_config.agent_did.clone(),
                gents_automation::triggers::runtime_snapshot::AgentDispatcher::single(
                    dispatch.clone(),
                ),
            )]),
            agent_executor_capacities: Default::default(),
            agent_executor_queue_capacities: Default::default(),
        });
        status
            .readiness()
            .publish_snapshot(snapshot.as_ref())
            .await
            .unwrap();
        status
            .set_process_state_durable(crate::runtime::agent::ProcessLifecycleState::Ready)
            .await
            .unwrap();
        let daemon = AgentDaemon::new(AgentDaemonInit {
            node: self.node.clone(),
            tool_config: self.tool_config.clone(),
            agent_config: self.agent_config.clone(),
            provider_family: None,
            model: Arc::new(provider),
            preamble,
            loop_tools: Arc::new(Vec::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                gents_inference::rendered_request::defra_rendered_request_capture_factory_for_agent(
                    self.node.clone(),
                    self.agent_config.agent_identity.as_ref(),
                )
                .unwrap(),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status: status.clone(),
            slot_generation: 1,
            request_admission: gents_session::request::admission::AgentRequestAdmissionVerifier::new(
                self.node.clone(),
                self.identity.clone(),
                gents_p2p::p2p_reconcile::enrollment_authority_channel().1,
            ),
        })
        .unwrap();
        (daemon, dispatch, receiver, snapshot, status, status_owner)
    }

    async fn terminalize_parent(&self) {
        gents_session::request::admission::terminalize_pending_request_rejection(
            &self.node,
            &self.parent.doc_id,
            &self.parent.node_did,
            &self.parent.agent_did,
            self.agent_config.agent_identity.as_ref(),
            "parent ended before title inference",
            "test.title_parent_terminal",
        )
        .await
        .unwrap();
        let doc = gents_store::graphql::escape_graphql_string(&self.parent.doc_id);
        let response = self
            .agent_access()
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 1) {{ lifecycle_state }} }}"#
            ))
            .await
            .unwrap();
        assert_eq!(
            response["data"]["AgentRequest"][0]["lifecycle_state"],
            "failed"
        );
    }

    async fn interrupt_parent(&self) {
        let mut lifecycle = RequestLifecycle::new_with_execution_binding(
            self.node.clone(),
            self.parent.clone(),
            self.agent_config.deadline_duration.as_secs(),
            gents_session::request::lifecycle::ExecutionOrigin::Interactive,
            self.agent_config.backend_id.clone().unwrap_or_default(),
        );
        lifecycle
            .bind_managed_agent(self.agent_config.agent_identity.as_ref())
            .unwrap();
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            ClaimOutcome::Claimed
        );
        gents_session::request::interrupt::interrupt_request_by_doc_id_with_access(
            &self.agent_access(),
            &self.parent.doc_id,
            &self.parent.node_did,
            self.parent.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(
            lifecycle
                .terminalize_owned(
                    gents_session::request::lifecycle::RequestTerminalOutcome::Interrupted,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap(),
            gents_session::request::lifecycle::TerminalizeResult::Won
        );
        let doc = gents_store::graphql::escape_graphql_string(&self.parent.doc_id);
        let response = self
            .agent_access()
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 1) {{ lifecycle_state terminal_output }} }}"#
            ))
            .await
            .unwrap();
        let row = &response["data"]["AgentRequest"][0];
        assert_eq!(row["lifecycle_state"], "interrupted");
        assert_eq!(
            serde_json::from_value::<gents_protocol::output::TerminalOutput>(
                row["terminal_output"].clone()
            )
            .unwrap(),
            gents_protocol::output::TerminalOutput::NoMessage
        );
    }

    async fn session_title(&self) -> serde_json::Value {
        let session = gents_store::graphql::escape_graphql_string(&self.parent.session_id);
        let response = self
            .agent_access()
            .execute(&format!(
                r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ title }} }}"#
            ))
            .await
            .unwrap();
        response["data"]["AgentSession"][0]["title"].clone()
    }

    async fn session_message_count(&self) -> usize {
        let session = gents_store::graphql::escape_graphql_string(&self.parent.session_id);
        let response = self
            .agent_access()
            .execute(&format!(
                r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ _docID }} }}"#
            ))
            .await
            .unwrap();
        response["data"]["AgentMessage"].as_array().unwrap().len()
    }

    async fn output_rows(&self) -> (Vec<gents_protocol::output::rows::OutputSegmentRow>, usize) {
        let doc = gents_store::graphql::escape_graphql_string(&self.title.doc_id);
        let query = format!(
            r#"{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{doc}" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} AgentMessage(filter: {{ request_doc_id: {{ _eq: "{doc}" }} }}) {{ _docID }} }}"#
        );
        let response = self.agent_access().execute(&query).await.unwrap();
        let data = &response["data"];
        let rows = data["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| decode_output_segment_row(row).map_err(anyhow::Error::from))
            .collect::<anyhow::Result<Vec<_>>>()
            .unwrap();
        (rows, data["AgentMessage"].as_array().unwrap().len())
    }

    async fn terminal_row(
        &self,
    ) -> (
        RequestLifecycleState,
        Option<gents_protocol::output::TerminalOutput>,
    ) {
        let doc = gents_store::graphql::escape_graphql_string(&self.title.doc_id);
        let response = self
            .agent_access()
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 1) {{ lifecycle_state terminal_output }} }}"#
            ))
            .await
            .unwrap();
        let row = &response["data"]["AgentRequest"][0];
        (
            serde_json::from_value(row["lifecycle_state"].clone()).unwrap(),
            serde_json::from_value(row["terminal_output"].clone()).unwrap(),
        )
    }
}

fn title_fields(raw: &gents_lean_vocab::family::Json) -> Vec<(StreamPayload, String, u32, u32)> {
    let record: wire::Segment = wire::decode(&raw.0);
    let flush = record.flush.as_ref().expect("modeled raw flush");
    let mut cursor = 0usize;
    let fields = flush
        .runs
        .iter()
        .map(|run| {
            let end = cursor + usize::try_from(run.bytes).unwrap();
            let text = String::from_utf8(flush.payload[cursor..end].to_vec()).unwrap();
            cursor = end;
            let declaration = run.declaration.as_ref().expect("modeled declaration");
            let kind = match &declaration.kind {
                wire::PayloadKind::Reasoning => StreamPayload::Reasoning,
                wire::PayloadKind::Signature => StreamPayload::ReasoningSignature,
                wire::PayloadKind::Encrypted => StreamPayload::ReasoningEncrypted,
                wire::PayloadKind::Redacted => StreamPayload::ReasoningRedacted,
                other => panic!("title fixture has payload kind {other:?}"),
            };
            (
                kind,
                text,
                u32::try_from(declaration.block).unwrap(),
                u32::try_from(declaration.part).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(cursor, flush.payload.len());
    fields
}

fn stimulus_fields() -> Vec<(StreamPayload, String, u32, u32)> {
    vec![
        (
            StreamPayload::Reasoning,
            "considering a title".to_owned(),
            0,
            0,
        ),
        (
            StreamPayload::ReasoningSignature,
            "signature".to_owned(),
            0,
            1,
        ),
    ]
}

async fn title_outcome(fixture: &TitleFixture) -> Option<OutputOutcome> {
    let (rows, _) = fixture.output_rows().await;
    let mut outcomes = rows.iter().filter_map(|row| match &row.segment.close {
        Some(SourceClose::Closed { outcome, .. }) => Some(*outcome),
        _ => None,
    });
    let first = outcomes.next();
    assert!(
        outcomes.all(|outcome| Some(outcome) == first),
        "title attempts closed with different outcomes"
    );
    first
}

async fn title_observation<T>(fixture: &TitleFixture, outcome: Option<OutputOutcome>) -> T
where
    T: serde::de::DeserializeOwned,
{
    gents_lean_vocab::snapshot().await;
    let (state, terminal) = fixture.terminal_row().await;
    let (_, messages) = fixture.output_rows().await;
    let terminal_selection = terminal.map(|terminal| match terminal {
        gents_protocol::output::TerminalOutput::NoMessage => {
            serde_json::json!({"kind": "no_message"})
        }
        gents_protocol::output::TerminalOutput::Message { message_doc_id } => {
            serde_json::json!({"kind": "message", "native": message_doc_id})
        }
    });
    gents_lean_vocab::family::recode(&serde_json::json!({
        "outcome": outcome.map(|outcome| match outcome {
            OutputOutcome::Complete => "complete",
            OutputOutcome::Partial => "partial",
        }),
        "request_state": state.as_str(),
        "terminal_selection": terminal_selection,
        "messages": messages,
    }))
}

fn provider_events(
    fields: &[(StreamPayload, String, u32, u32)],
    complete: bool,
) -> Vec<RawStreamingChoice<()>> {
    let mut events = Vec::new();
    let mut index = 0;
    while index < fields.len() {
        let content = match &fields[index].0 {
            StreamPayload::Reasoning => {
                let (signature_kind, signature, _, _) = &fields[index + 1];
                assert_eq!(signature_kind, &StreamPayload::ReasoningSignature);
                index += 2;
                ReasoningContent::Text {
                    text: fields[index - 2].1.clone(),
                    signature: Some(signature.clone()),
                }
            }
            StreamPayload::ReasoningEncrypted => {
                let text = fields[index].1.clone();
                index += 1;
                ReasoningContent::Encrypted(text)
            }
            StreamPayload::ReasoningRedacted => {
                let text = fields[index].1.clone();
                index += 1;
                ReasoningContent::Redacted { data: text }
            }
            other => panic!("unsupported modeled title field {other:?}"),
        };
        events.push(RawStreamingChoice::Reasoning {
            id: None,
            content: gents_inference::llm::rig_compat::to_rig_reasoning_part(&content),
        });
    }
    if complete {
        events.push(RawStreamingChoice::Message("generated-title".into()));
        events.push(RawStreamingChoice::FinalResponse(()));
    }
    events
}

fn reasoning_only_events(
    fields: &[(StreamPayload, String, u32, u32)],
) -> Vec<RawStreamingChoice<()>> {
    let mut events = provider_events(fields, false);
    events.push(RawStreamingChoice::FinalResponse(()));
    events
}

async fn assert_title_audit(
    fixture: &TitleFixture,
    expected: &[(StreamPayload, String, u32, u32)],
    outcome: OutputOutcome,
    attempts: usize,
    complete_text: bool,
) {
    let (rows, message_count) = fixture.output_rows().await;
    assert_eq!(
        message_count, 0,
        "title audit published a transcript header"
    );
    let mut sources = rapidhash::RapidHashSet::default();
    for row in &rows {
        let OutputSource::ProviderTurn {
            scope,
            turn_index,
            attempt,
        } = &row.segment.source
        else {
            panic!("title wrote a non-provider source")
        };
        assert_eq!(
            scope.kind,
            gents_inference::rendered_request::CaptureScopeKind::Title
        );
        assert_eq!(*turn_index, 0);
        assert_eq!(row.segment.request_doc_id, fixture.title.doc_id);
        sources.insert((*scope, *turn_index, *attempt));
    }
    assert_eq!(sources.len(), attempts);
    for source in sources {
        let output_source = OutputSource::ProviderTurn {
            scope: source.0,
            turn_index: source.1,
            attempt: source.2,
        };
        let source_rows = rows
            .iter()
            .filter(|row| row.segment.source == output_source)
            .collect::<Vec<_>>();
        let closed = source_rows
            .iter()
            .filter_map(|row| row.segment.close.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(closed.len(), 1, "one exact close per title attempt");
        assert!(
            matches!(closed[0], SourceClose::Closed { outcome: actual, .. } if *actual == outcome)
        );
        let OutputWriter::RequestExecution {
            execution_generation,
        } = &source_rows[0].segment.writer
        else {
            panic!("title audit has no request writer")
        };
        let writer = OutputWriter::RequestExecution {
            execution_generation: execution_generation.clone(),
        };
        let observed = source_rows
            .iter()
            .map(|row| ObservedSegment {
                doc_id: &row.doc_id,
                segment: &row.segment,
            })
            .collect::<Vec<_>>();
        let extent = inspect_open_source(&observed, &fixture.title.doc_id, &output_source, &writer)
            .expect("complete title source extent");
        let SourceClose::Closed {
            segments,
            stream_bytes,
            ..
        } = closed[0]
        else {
            panic!("title source was not closed");
        };
        assert_eq!(
            *segments, extent.segments,
            "title close truncated raw segments"
        );
        assert_eq!(
            *stream_bytes, extent.stream_bytes,
            "title close truncated raw bytes"
        );
        let close_doc_id = source_rows
            .iter()
            .find(|row| row.segment.close.is_some())
            .expect("title close row")
            .doc_id
            .clone();
        let prefix = reconstruct_dense_prefix(
            &observed,
            &fixture.title.doc_id,
            &output_source,
            &writer,
            None,
        )
        .unwrap();
        assert_eq!(prefix.streams, extent.streams);
        for (stream, expected_stream) in extent.streams.iter().enumerate() {
            let sealed = reconstruct_stream(
                &observed,
                &[],
                &[],
                &PayloadRef {
                    close_doc_id: close_doc_id.clone(),
                    stream: u32::try_from(stream).unwrap(),
                },
            )
            .expect("sealed title stream");
            assert_eq!(&sealed, expected_stream);
        }
        assert_eq!(
            prefix.streams.len(),
            expected.len() + usize::from(complete_text)
        );
        for (actual, (kind, text, block, part)) in prefix.streams.iter().zip(expected) {
            assert_eq!(&actual.declaration.payload, kind);
            assert_eq!(&actual.text, text);
            assert_eq!(actual.declaration.block_index, *block);
            assert_eq!(actual.declaration.part_index, *part);
        }
        if complete_text {
            let last = prefix.streams.last().unwrap();
            assert_eq!(last.declaration.payload, StreamPayload::Text);
            assert_eq!(last.text, "generated-title");
        }
    }
}

async fn wait_for_title_streams(fixture: &TitleFixture, minimum: usize, fields: usize) {
    read_when_changed(
        &fixture.node,
        "AgentOutputSegment",
        "title reasoning reaches durable output owner",
        async || {
            let (rows, _) = fixture.output_rows().await;
            let sources = rows
                .iter()
                .filter_map(|row| match &row.segment.source {
                    OutputSource::ProviderTurn { scope, attempt, .. }
                        if scope.kind
                            == gents_inference::rendered_request::CaptureScopeKind::Title =>
                    {
                        Some((scope.seq, *attempt))
                    }
                    _ => None,
                })
                .collect::<rapidhash::RapidHashSet<_>>();
            let all_received = sources.len() >= minimum
                && sources.iter().all(|(sequence, attempt)| {
                    let source = OutputSource::ProviderTurn {
                        scope: gents_inference::rendered_request::CaptureScope {
                            kind: gents_inference::rendered_request::CaptureScopeKind::Title,
                            seq: *sequence,
                        },
                        turn_index: 0,
                        attempt: *attempt,
                    };
                    let scoped = rows
                        .iter()
                        .filter(|row| row.segment.source == source)
                        .collect::<Vec<_>>();
                    let Some(OutputWriter::RequestExecution {
                        execution_generation,
                    }) = scoped.first().map(|row| &row.segment.writer)
                    else {
                        return false;
                    };
                    let writer = OutputWriter::RequestExecution {
                        execution_generation: execution_generation.clone(),
                    };
                    let observed = scoped
                        .iter()
                        .map(|row| ObservedSegment {
                            doc_id: &row.doc_id,
                            segment: &row.segment,
                        })
                        .collect::<Vec<_>>();
                    reconstruct_dense_prefix(
                        &observed,
                        &fixture.title.doc_id,
                        &source,
                        &writer,
                        None,
                    )
                    .is_ok_and(|prefix| prefix.streams.len() >= fields)
                });
            if all_received {
                Ok(())
            } else {
                Err(format!("sources={} rows={}", sources.len(), rows.len()))
            }
        },
    )
    .await;
}

#[tokio::test]
async fn a_title_request_sends_the_profile_temperature() {
    let fixture = TitleFixture::new().await;
    assert_eq!(fixture.agent_config.sampling.temperature, None);
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    let provider = ScriptedProvider::responding(move |dispatch| {
        seen.lock().unwrap().push(dispatch.body.clone());
        Reply::turn(vec![
            RawStreamingChoice::Message("generated-title".into()),
            RawStreamingChoice::FinalResponse(()),
        ])
    })
    .capturing_openai("scripted");
    let (_shutdown, rx) = watch::channel(false);
    fixture
        .task(provider, true)
        .run(fixture.title.clone(), rx)
        .await
        .unwrap();
    let bodies = bodies.lock().unwrap().clone();
    assert_eq!(bodies.len(), 1);
    let body = bodies[0].as_ref().expect("the OpenAI wire body");
    assert!(body.get("temperature").is_none(), "{body}");
}

#[tokio::test]
async fn title_reasoning_survives_parent_terminal_under_own_request() {
    gents_lean_vocab::snapshot().await;
    for case in
        gents_lean_vocab::families::native_execution_contracts_title_parent_terminal::cases()
    {
        let fields = title_fields(&case.input.raw);
        for parent_state in [
            RequestLifecycleState::Failed,
            RequestLifecycleState::Interrupted,
        ] {
            let fixture = TitleFixture::new().await;
            match parent_state {
                RequestLifecycleState::Failed => fixture.terminalize_parent().await,
                RequestLifecycleState::Interrupted => fixture.interrupt_parent().await,
                _ => unreachable!(),
            }
            let public_head_before = fixture.session_message_count().await;
            let provider = title_provider(provider_events(&fields, true), false);
            let calls = provider.clone();
            let (_shutdown, rx) = watch::channel(false);
            fixture
                .task(provider, true)
                .run(fixture.title.clone(), rx)
                .await
                .unwrap();
            assert_eq!(calls.calls(), 1, "parent: {parent_state:?}");
            let outcome = title_outcome(&fixture).await;
            assert_title_audit(&fixture, &fields, outcome.expect("title closed"), 1, true).await;
            assert_eq!(
                fixture.session_message_count().await,
                public_head_before,
                "parent: {parent_state:?}"
            );
            case.check(Some(title_observation(&fixture, outcome).await));
        }
    }
}

#[tokio::test]
async fn title_timeout_drains_each_received_reasoning_attempt_as_partial() {
    gents_lean_vocab::snapshot().await;
    for case in gents_lean_vocab::families::native_execution_contracts_title_timeout::cases() {
        let fields = title_fields(&case.input.raw);
        let fixture = TitleFixture::new().await;
        let provider = title_provider(provider_events(&fields, false), true);
        let calls = provider.clone();
        let (_shutdown, rx) = watch::channel(false);
        let task = fixture.task(provider, true);
        let title = fixture.title.clone();
        let running = tokio::spawn(async move { task.run(title, rx).await });
        tokio::time::timeout(Duration::from_secs(15), calls.wait_for_calls(1))
            .await
            .expect("first title provider attempt starts");
        wait_for_title_streams(&fixture, 1, fields.len()).await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(15), calls.wait_for_calls(2))
            .await
            .expect("second title provider attempt starts");
        wait_for_title_streams(&fixture, 2, fields.len()).await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::time::resume();
        tokio::time::timeout(Duration::from_secs(15), running)
            .await
            .expect("timed out title attempts drain their output")
            .unwrap()
            .unwrap();
        assert_eq!(calls.calls(), 2);
        let outcome = title_outcome(&fixture).await;
        assert_title_audit(&fixture, &fields, outcome.expect("title closed"), 2, false).await;
        case.check(Some(title_observation(&fixture, outcome).await));
    }
}

#[tokio::test]
async fn crashed_title_recovers_committed_reasoning_without_publication() {
    gents_lean_vocab::snapshot().await;
    for case in gents_lean_vocab::families::native_execution_contracts_title_crash_recovery::cases()
    {
        let fields = title_fields(&case.input.raw);
        let fixture = TitleFixture::new().await;
        let provider = title_provider(provider_events(&fields, false), true);
        let entered = provider.clone();
        let (_shutdown, rx) = watch::channel(false);
        let task = fixture.task(provider, true);
        let title = fixture.title.clone();
        let running = tokio::spawn(async move { task.run(title, rx).await });
        entered.wait_for_calls(1).await;
        wait_for_title_streams(&fixture, 1, fields.len()).await;
        let (before, before_messages) = fixture.output_rows().await;
        assert_eq!(before_messages, 0);
        assert!(before.iter().all(|row| row.segment.close.is_none()));
        let before_ids = before
            .iter()
            .map(|row| row.doc_id.clone())
            .collect::<Vec<_>>();

        running.abort();
        assert!(running
            .await
            .expect_err("title task must abort")
            .is_cancelled());
        let (aborted, aborted_messages) = fixture.output_rows().await;
        assert_eq!(aborted_messages, 0);
        assert_eq!(aborted.len(), before.len());
        assert!(aborted.iter().all(|row| row.segment.close.is_none()));
        let physical = gents_store::graphql::escape_graphql_string(&fixture.title.doc_id);
        let owner = gents_store::graphql::escape_graphql_string(fixture.identity.did());
        let response = fixture
            .agent_access()
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }}, node_did: {{ _eq: "{owner}" }} }}, limit: 2) {{ execution_generation lifecycle_state }} }}"#
            ))
            .await
            .expect("read title lease owner");
        let rows = response["data"]["AgentRequest"]
            .as_array()
            .expect("title lease rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["lifecycle_state"], "processing");
        let generation = gents_store::graphql::escape_graphql_string(
            rows[0]["execution_generation"]
                .as_str()
                .expect("title execution generation"),
        );
        let past = gents_store::graphql::escape_graphql_string(
            &(chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
        );
        let expired = fixture
            .agent_access()
            .write(
                "test.title_expire_own_lease",
                &format!(
                    r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }}, node_did: {{ _eq: "{owner}" }}, execution_generation: {{ _eq: "{generation}" }}, lifecycle_state: {{ _eq: "processing" }} }}, input: {{ execution_lease_expires_at: "{past}" }}) {{ _docID }} }}"#
                ),
            )
            .await
            .expect("expire exact title generation");
        assert_eq!(
            expired["data"]["update_AgentRequest"][0]["_docID"],
            fixture.title.doc_id
        );

        let first = RequestLifecycle::recover_all_with_identities(
            &fixture.node,
            fixture.identity.did(),
            std::slice::from_ref(&fixture.agent_config.agent_identity),
        )
        .await
        .expect("recover crashed title");
        assert_eq!(first.requests_recovered, 1);
        let outcome = title_outcome(&fixture).await;
        assert_title_audit(&fixture, &fields, outcome.expect("title closed"), 1, false).await;
        case.check(Some(title_observation(&fixture, outcome).await));
        assert_eq!(fixture.session_message_count().await, 0);
        let (after, _) = fixture.output_rows().await;
        assert_eq!(
            after.len(),
            before_ids.len() + 1,
            "recovery adds one exact closure"
        );
        assert!(before_ids
            .iter()
            .all(|id| after.iter().any(|row| &row.doc_id == id)));

        let replay = RequestLifecycle::recover_all_with_identities(
            &fixture.node,
            fixture.identity.did(),
            std::slice::from_ref(&fixture.agent_config.agent_identity),
        )
        .await
        .expect("repeat title recovery");
        assert_eq!(replay.requests_recovered, 0);
        let replayed = title_outcome(&fixture).await;
        assert_title_audit(&fixture, &fields, replayed.expect("title closed"), 1, false).await;
        case.check(Some(title_observation(&fixture, replayed).await));
        assert_eq!(fixture.output_rows().await.0.len(), after.len());
    }
}

#[tokio::test]
async fn missing_title_capture_fails_before_provider_dispatch() {
    let fields = stimulus_fields();
    let fixture = TitleFixture::new().await;
    let provider = title_provider(provider_events(&fields, true), false);
    let calls = provider.clone();
    let (_shutdown, rx) = watch::channel(false);
    assert!(fixture
        .task(provider, false)
        .run(fixture.title.clone(), rx)
        .await
        .is_err());
    assert_eq!(calls.calls(), 0);
    assert!(fixture.output_rows().await.0.is_empty());
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Failed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
}

#[tokio::test(flavor = "current_thread")]
async fn title_audit_is_dispatched_once_by_watcher_router_and_daemon() {
    gents_lean_vocab::snapshot().await;
    let capture = TitleWarningCapture::default();
    let warnings = capture.0.clone();
    let subscriber = tracing::Dispatch::new(Registry::default().with(capture));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);
    for case in
        gents_lean_vocab::families::native_execution_contracts_title_watcher_dispatch::cases()
    {
        let fields = title_fields(&case.input.raw);
        for preexisting in [false, true] {
            let mut fixture = TitleFixture::new().await;
            fixture.terminalize_parent().await;
            if !preexisting {
                gents_session::request::admission::terminalize_pending_request_rejection(
                    &fixture.node,
                    &fixture.title.doc_id,
                    &fixture.title.node_did,
                    &fixture.title.agent_did,
                    fixture.agent_config.agent_identity.as_ref(),
                    "retire setup title before live creation",
                    "test.title_live_creation_setup",
                )
                .await
                .unwrap();
            }

            let provider = title_provider(provider_events(&fields, true), false);
            let calls = provider.clone();
            let (mut daemon, _dispatch, receiver, snapshot, status, status_owner) =
                fixture.start_ready_daemon_parts(provider, true).await;
            let (_snapshot_tx, snapshot_rx) = watch::channel(snapshot);
            let (shutdown_tx, shutdown_rx) = watch::channel(false);
            let daemon_rx = Arc::new(tokio::sync::Mutex::new(receiver));
            let daemon_shutdown = shutdown_rx.clone();
            let gate = RuntimeAdmissionGate::closed();
            gate.open().await;
            let router_gate = gate.clone();
            let router_node = fixture.node.clone();
            let router_did = fixture.identity.did().to_owned();
            let router_agent = fixture.agent_config.agent_identity.clone();
            let router_task = tokio::spawn(async move {
                run_router_with_watcher(RouterInputs {
                    node: router_node.clone(),
                    node_did: router_did.clone(),
                    watcher: DefraWatcher::for_agents(router_node, &router_did, vec![router_agent])
                        .expect("request watcher"),
                    active_snapshot_rx: snapshot_rx,
                    shutdown: shutdown_rx,
                    admission_gate: router_gate,
                    runtime_status: status,
                    runtime_snapshot_observer: None,
                    unresolved_session_bound: UNRESOLVED_SESSION_ATTEMPT_BOUND,
                })
                .await
            });

            if !preexisting {
                daemon.spawn_conversation_title_generation(&fixture.parent);
                let old_doc_id = fixture.title.doc_id.clone();
                let session =
                    gents_store::graphql::escape_graphql_string(&fixture.parent.session_id);
                let purpose = gents_store::graphql::escape_graphql_string(
                    RequestPurpose::TitleAudit.as_str(),
                );
                let doc_id = read_when_changed(
                    &fixture.node,
                    "AgentRequest",
                    "title creator persists a request",
                    async || {
                        let response = fixture
                            .agent_access()
                            .execute(&format!(
                                r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{session}" }}, purpose: {{ _eq: "{purpose}" }} }}) {{ _docID }} }}"#
                            ))
                            .await
                            .unwrap();
                        response["data"]["AgentRequest"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .filter_map(|row| row["_docID"].as_str())
                            .find(|doc_id| *doc_id != old_doc_id)
                            .map(str::to_owned)
                            .ok_or_else(|| "title request not yet visible".to_owned())
                    },
                )
                .await;
                fixture.title =
                    gents_session::request::admission::load_request_for_admission_test_as(
                        &fixture.node,
                        &doc_id,
                        &fixture.agent_config.agent_did,
                    )
                    .await
                    .unwrap();
                assert_eq!(calls.calls(), 0, "creator dispatched before daemon");
            }
            let daemon_task = tokio::spawn(async move {
                daemon
                    .run(
                        daemon_rx,
                        crate::runtime::agent::worker_capacity::WorkerCapacity::new(1),
                        daemon_shutdown,
                    )
                    .await
            });

            read_when_changed(
                &fixture.node,
                "AgentRequest",
                "watcher completes title audit",
                async || {
                    let (state, _) = fixture.terminal_row().await;
                    if state == RequestLifecycleState::Completed {
                        Ok(())
                    } else {
                        Err(format!("{state:?}"))
                    }
                },
            )
            .await;
            assert_eq!(calls.calls(), 1, "preexisting={preexisting}");
            let outcome = title_outcome(&fixture).await;
            assert_title_audit(&fixture, &fields, outcome.expect("title closed"), 1, true).await;
            case.check(Some(title_observation(&fixture, outcome).await));
            shutdown_tx.send(true).unwrap();
            gate.close().await;
            tokio::time::timeout(Duration::from_secs(5), router_task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(5), daemon_task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            status_owner.close().await.unwrap();
            assert!(
                warnings.lock().unwrap().is_empty(),
                "title owner emitted WARN"
            );
        }
    }
}

async fn start_owned_title_daemon(
    fixture: &TitleFixture,
    provider: ScriptedProvider,
) -> (
    tokio::sync::mpsc::Sender<AgentRequest>,
    watch::Sender<bool>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    RuntimeStatusOwner,
) {
    let (mut daemon, dispatch, receiver, _snapshot, _status, status_owner) =
        fixture.start_ready_daemon_parts(provider, false).await;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        daemon
            .run(
                Arc::new(tokio::sync::Mutex::new(receiver)),
                crate::runtime::agent::worker_capacity::WorkerCapacity::new(1),
                shutdown_rx,
            )
            .await
    });
    (dispatch, shutdown_tx, task, status_owner)
}

fn buffered_reasoning_title_provider() -> (
    ScriptedProvider,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    let events: Vec<RawStreamingChoice<()>> = ["first", " second"]
        .into_iter()
        .map(|text| RawStreamingChoice::Reasoning {
            id: None,
            content: gents_inference::llm::rig_compat::to_rig_reasoning_part(
                &ReasoningContent::Text {
                    text: text.into(),
                    signature: None,
                },
            ),
        })
        .collect();
    let before_second = Arc::new(tokio::sync::Notify::new());
    let third_poll = Arc::new(tokio::sync::Notify::new());
    let [first, second]: [RawStreamingChoice<()>; 2] =
        events.try_into().expect("two reasoning events");
    let (waits, signals) = (before_second.clone(), third_poll.clone());
    let provider = ScriptedProvider::responding(move |_| {
        Reply::Stream(vec![
            Step::Chunk(first.clone()),
            Step::Wait(waits.clone()),
            Step::Chunk(second.clone()),
            Step::Signal(signals.clone()),
            Step::Pending,
        ])
    })
    .capturing_openai("scripted");
    (provider, before_second, third_poll)
}

async fn advance_to_buffered_second_chunk(
    fixture: &TitleFixture,
    before_second: &tokio::sync::Notify,
    third_poll: &tokio::sync::Notify,
) {
    read_when_changed(
        &fixture.node,
        "AgentOutputSegment",
        "first title reasoning never became durable",
        async || {
            let (rows, _) = fixture.output_rows().await;
            if rows.iter().any(|row| !row.segment.runs.is_empty()) {
                assert_eq!(open_title_reasoning(fixture).await, ["first"]);
                Ok(())
            } else {
                Err("no title reasoning runs yet")
            }
        },
    )
    .await;
    before_second.notify_one();
    tokio::time::timeout(Duration::from_secs(10), third_poll.notified())
        .await
        .expect("title provider did not process both reasoning chunks");
    assert_eq!(open_title_reasoning(fixture).await, ["first"]);
}

async fn open_title_reasoning(fixture: &TitleFixture) -> Vec<String> {
    let (rows, message_count) = fixture.output_rows().await;
    assert_eq!(message_count, 0, "title published a public header");
    let source = rows
        .iter()
        .find_map(|row| match &row.segment.source {
            OutputSource::ProviderTurn { scope, .. }
                if scope.kind.request_purpose() == fixture.title.purpose =>
            {
                Some(row.segment.source.clone())
            }
            _ => None,
        })
        .expect("title provider source");
    let writer = rows
        .iter()
        .find(|row| row.segment.source == source)
        .unwrap()
        .segment
        .writer
        .clone();
    let observed = rows
        .iter()
        .filter(|row| row.segment.source == source)
        .map(|row| ObservedSegment {
            doc_id: &row.doc_id,
            segment: &row.segment,
        })
        .collect::<Vec<_>>();
    reconstruct_dense_prefix(&observed, &fixture.title.doc_id, &source, &writer, None)
        .unwrap()
        .streams
        .into_iter()
        .map(|stream| {
            assert_eq!(stream.declaration.payload, StreamPayload::Reasoning);
            stream.text
        })
        .collect()
}

#[tokio::test]
async fn owned_title_shutdown_drains_buffered_reasoning_before_join() {
    let mut fixture = TitleFixture::new().await;
    Arc::get_mut(&mut fixture.agent_config)
        .unwrap()
        .stream_batch_ms = 5_000;
    fixture.terminalize_parent().await;
    let (provider, before_second, third_poll) = buffered_reasoning_title_provider();
    let calls = provider.clone();
    let (dispatch, shutdown, daemon, status_owner) =
        start_owned_title_daemon(&fixture, provider).await;
    dispatch.send(fixture.title.clone()).await.unwrap();
    advance_to_buffered_second_chunk(&fixture, &before_second, &third_poll).await;
    assert_eq!(
        fixture.terminal_row().await.0,
        RequestLifecycleState::Processing
    );
    shutdown.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("title daemon did not join its title task")
        .unwrap()
        .unwrap();
    assert_eq!(calls.calls(), 1);
    assert_title_audit(
        &fixture,
        &[
            (StreamPayload::Reasoning, "first".into(), 0, 0),
            (StreamPayload::Reasoning, " second".into(), 0, 1),
        ],
        OutputOutcome::Partial,
        1,
        false,
    )
    .await;
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Interrupted,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
    status_owner.close().await.unwrap();
}

#[tokio::test]
async fn owned_title_interrupt_drains_buffered_reasoning_before_terminal() {
    let mut fixture = TitleFixture::new().await;
    Arc::get_mut(&mut fixture.agent_config)
        .unwrap()
        .stream_batch_ms = 5_000;
    fixture.terminalize_parent().await;
    let (provider, before_second, third_poll) = buffered_reasoning_title_provider();
    let calls = provider.clone();
    let task = fixture.task(provider, true);
    let title_request = fixture.title.clone();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let running = tokio::spawn(async move { task.run(title_request, shutdown_rx).await });
    advance_to_buffered_second_chunk(&fixture, &before_second, &third_poll).await;
    assert_eq!(
        fixture.terminal_row().await.0,
        RequestLifecycleState::Processing
    );
    gents_session::request::interrupt::interrupt_request_by_doc_id_with_access(
        &fixture.agent_access(),
        &fixture.title.doc_id,
        &fixture.title.node_did,
        fixture.title.requester_did.as_deref(),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("title interrupt did not settle promptly")
        .unwrap()
        .unwrap();
    assert_eq!(calls.calls(), 1);
    assert_title_audit(
        &fixture,
        &[
            (StreamPayload::Reasoning, "first".into(), 0, 0),
            (StreamPayload::Reasoning, " second".into(), 0, 1),
        ],
        OutputOutcome::Partial,
        1,
        false,
    )
    .await;
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Interrupted,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
}

#[tokio::test]
async fn owned_title_shutdown_reports_fenced_partial_without_terminalizing() {
    let mut fixture = TitleFixture::new().await;
    Arc::get_mut(&mut fixture.agent_config)
        .unwrap()
        .stream_batch_ms = 5_000;
    fixture.terminalize_parent().await;
    let (provider, before_second, third_poll) = buffered_reasoning_title_provider();
    let calls = provider.clone();
    let (dispatch, shutdown, daemon, status_owner) =
        start_owned_title_daemon(&fixture, provider).await;
    dispatch.send(fixture.title.clone()).await.unwrap();
    advance_to_buffered_second_chunk(&fixture, &before_second, &third_poll).await;
    let (before, before_messages) = fixture.output_rows().await;
    assert_eq!(before_messages, 0);
    assert!(before.iter().all(|row| row.segment.close.is_none()));
    fence_request_generation(
        &fixture.agent_access(),
        &fixture.title.doc_id,
        &fixture.title.node_did,
    )
    .await;
    shutdown.send(true).unwrap();
    let error = tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .expect("title daemon did not join its title task")
        .unwrap()
        .expect_err("fenced partial must surface through daemon join");
    assert!(
        error.is::<super::super::ShutdownDrainFailure>(),
        "{error:#}"
    );
    assert_eq!(calls.calls(), 1);
    assert_eq!(
        fixture.terminal_row().await,
        (RequestLifecycleState::Processing, None)
    );
    let (after, after_messages) = fixture.output_rows().await;
    assert_eq!(after_messages, 0);
    let mut before = before
        .into_iter()
        .map(|row| (row.doc_id, row.segment))
        .collect::<Vec<_>>();
    let mut after = after
        .into_iter()
        .map(|row| (row.doc_id, row.segment))
        .collect::<Vec<_>>();
    before.sort_by(|left, right| left.0.cmp(&right.0));
    after.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(after, before, "failed drain wrote or closed title output");
    status_owner.close().await.unwrap();
}

#[tokio::test]
async fn reasoning_only_title_retains_each_attempt_and_uses_bounded_fallback() {
    gents_lean_vocab::snapshot().await;
    let capture = TitleWarningCapture::default();
    let warnings = capture.0.clone();
    let subscriber = tracing::Dispatch::new(Registry::default().with(capture));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);
    for case in gents_lean_vocab::families::native_execution_contracts_title_reasoning_only::cases()
    {
        let fields = title_fields(&case.input.raw);
        let fixture = TitleFixture::new().await;
        let provider = title_provider(reasoning_only_events(&fields), false);
        let calls = provider.clone();
        let (_shutdown, rx) = watch::channel(false);
        fixture
            .task(provider, true)
            .run(fixture.title.clone(), rx)
            .await
            .expect("reasoning-only title is an ordinary provider result");
        assert_eq!(
            calls.calls(),
            usize::try_from(super::TITLE_GENERATION_MAX_ATTEMPTS).unwrap()
        );
        let outcome = title_outcome(&fixture).await;
        assert_title_audit(&fixture, &fields, outcome.expect("title closed"), 2, false).await;

        let doc = gents_store::graphql::escape_graphql_string(&fixture.title.doc_id);
        let response = fixture
            .agent_access()
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 1) {{ execution_generation }} }}"#
            ))
            .await
            .unwrap();
        let generation = response["data"]["AgentRequest"][0]["execution_generation"]
            .as_str()
            .expect("title execution generation")
            .to_owned();
        let (rows, _) = fixture.output_rows().await;
        assert!(rows.iter().all(|row| matches!(
            &row.segment.writer,
            OutputWriter::RequestExecution { execution_generation } if *execution_generation == generation
        )));

        case.check(Some(title_observation(&fixture, outcome).await));
        let title = fixture.session_title().await;
        assert_eq!(
            title["text"],
            super::sanitize_generated_title("", &fixture.parent.content),
            "fallback title: {title}"
        );
        assert_eq!(
            warnings.lock().unwrap().len(),
            usize::try_from(super::TITLE_GENERATION_MAX_ATTEMPTS).unwrap(),
            "only the bounded attempt warnings are emitted"
        );
    }
}

#[tokio::test]
async fn a_usage_limited_title_falls_back_without_failing_the_turn() {
    let fixture = TitleFixture::new().await;
    let backend = fixture.agent_config.backend_id.clone().unwrap();
    let resets_at = chrono::Utc::now() + chrono::Duration::hours(2);
    let limit = format!(
        r#"Invalid status code 429 Too Many Requests with message: {{"error":{{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_at":{}}}}}"#,
        resets_at.timestamp()
    );
    let provider = ScriptedProvider::responding(move |_| Reply::provider_error(limit.clone()))
        .capturing_openai("scripted");
    let calls = provider.clone();
    let model = fixture.admitted_with(provider, 1, &backend, &format!("{backend}:connection"));
    let parent_state = |access: ConfigAccess, doc: String| async move {
        let doc = gents_store::graphql::escape_graphql_string(&doc);
        access
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}, limit: 1) {{ lifecycle_state }} }}"#
            ))
            .await
            .unwrap()["data"]["AgentRequest"][0]["lifecycle_state"]
            .clone()
    };
    let parent_before = parent_state(fixture.agent_access(), fixture.parent.doc_id.clone()).await;

    let (_shutdown, rx) = watch::channel(false);
    fixture
        .task(model, true)
        .run(fixture.title.clone(), rx)
        .await
        .expect("a usage-limited title is an ordinary provider failure");

    assert_eq!(
        calls.calls(),
        usize::try_from(super::TITLE_GENERATION_MAX_ATTEMPTS).unwrap()
    );
    let request_id = gents_store::graphql::escape_graphql_string(&fixture.title.request_id);
    let response = fixture
        .agent_access()
        .execute(&format!(
            r#"{{ InferenceCall(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ call_kind call_state failure_reason provider_limit provider_limit_resets_at }} }}"#
        ))
        .await
        .unwrap();
    let rows = response["data"]["InferenceCall"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in rows {
        assert_eq!(row["call_kind"], "oneoff", "{row}");
        assert_eq!(row["call_state"], "failed", "{row}");
        let reason = row["failure_reason"].as_str().unwrap();
        assert!(
            reason.starts_with("provider usage limit reached (resets at "),
            "{reason}"
        );
        assert_eq!(row["provider_limit"], "usage_limited", "{row}");
        let recorded_reset = row["provider_limit_resets_at"]
            .as_str()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok());
        assert_eq!(
            recorded_reset.map(|at| at.timestamp()),
            Some(resets_at.timestamp())
        );
    }

    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Completed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
    assert_eq!(
        fixture.session_title().await["text"],
        super::sanitize_generated_title("", &fixture.parent.content)
    );
    assert_eq!(
        parent_state(fixture.agent_access(), fixture.parent.doc_id.clone()).await,
        parent_before
    );
}

#[tokio::test]
async fn fenced_title_output_fails_closed_without_retry() {
    let fields = stimulus_fields();
    let fixture = TitleFixture::new().await;
    let provider = fencing_title_provider(
        reasoning_only_events(&fields),
        fixture.agent_access(),
        fixture.title.doc_id.clone(),
        fixture.title.node_did.clone(),
    );
    let calls = provider.clone();
    let (_shutdown, rx) = watch::channel(false);
    let error = fixture
        .task(provider, true)
        .run(fixture.title.clone(), rx)
        .await
        .expect_err("a fenced auxiliary writer must fail the title");
    assert_eq!(calls.calls(), 1, "storage failure retried");
    assert!(
        format!("{error:#}").contains("lost its live processing lease"),
        "{error:#}"
    );
    assert!(fixture.session_title().await.is_null());
}

#[tokio::test]
async fn title_output_cap_applies_only_when_reasoning_is_disabled_on_the_wire() {
    use gents_documents::inference_defaults::ReasoningEffort;
    use gents_loop::openai_wire::OpenAiWireApi;
    let fixture = TitleFixture::new().await;
    let configured = Some(1024);
    let capped = Some(super::TITLE_VISIBLE_MAX_TOKENS);
    let mut agent_config = (*fixture.agent_config).clone();
    let chat = OpenAiWireApi::ChatCompletions;
    let responses = OpenAiWireApi::Responses;
    for (kind, wire, effort, expected) in [
        (
            BackendProviderKind::OpenAiCompatible,
            chat,
            None,
            configured,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            chat,
            Some(ReasoningEffort::Low),
            configured,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            chat,
            Some(ReasoningEffort::None),
            capped,
        ),
        (
            BackendProviderKind::Vllm,
            chat,
            Some(ReasoningEffort::Low),
            configured,
        ),
        (
            BackendProviderKind::Vllm,
            chat,
            Some(ReasoningEffort::None),
            capped,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            responses,
            None,
            configured,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            responses,
            Some(ReasoningEffort::High),
            configured,
        ),
        (
            BackendProviderKind::OpenAiCompatible,
            responses,
            Some(ReasoningEffort::None),
            capped,
        ),
        (
            BackendProviderKind::OpenRouter,
            chat,
            Some(ReasoningEffort::None),
            capped,
        ),
        (
            BackendProviderKind::ChatGptCodex,
            responses,
            None,
            configured,
        ),
        (
            BackendProviderKind::ChatGptCodex,
            responses,
            Some(ReasoningEffort::None),
            capped,
        ),
        (
            BackendProviderKind::XaiGrokOAuth,
            chat,
            Some(ReasoningEffort::None),
            configured,
        ),
        (
            BackendProviderKind::ClaudeCliSubscription,
            chat,
            Some(ReasoningEffort::None),
            configured,
        ),
    ] {
        agent_config.backend_provider_kind = kind;
        agent_config.openai_wire_api = wire;
        agent_config.sampling.reasoning_effort = effort;
        let config = crate::turn::completion_factory::loop_config(
            &agent_config,
            super::title_generation_preamble(),
            0,
            gents_inference::rendered_request::CaptureScopeKind::Title,
        );
        assert_eq!(
            super::title_max_tokens(config.additional_params.as_ref(), configured),
            expected,
            "{kind:?} {wire:?} {effort:?}"
        );
    }

    agent_config.backend_provider_kind = BackendProviderKind::Vllm;
    agent_config.openai_wire_api = chat;
    agent_config.sampling.reasoning_effort = Some(ReasoningEffort::None);
    let config = crate::turn::completion_factory::loop_config(
        &agent_config,
        super::title_generation_preamble(),
        0,
        gents_inference::rendered_request::CaptureScopeKind::Title,
    );
    let overridden = crate::turn::completion_factory::merge_optional_params(
        config.additional_params,
        Some(serde_json::json!({ "chat_template_kwargs": { "enable_thinking": true } })),
    );
    assert_eq!(
        super::title_max_tokens(overridden.as_ref(), configured),
        configured
    );
}
