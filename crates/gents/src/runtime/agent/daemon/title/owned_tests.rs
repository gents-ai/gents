use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use defra_node::EmbeddedNode;
use futures::{stream, StreamExt};
use gents_protocol::message::ReasoningContent;
use gents_protocol::output::extent::inspect_open_source;
use gents_protocol::output::live::reconstruct_dense_prefix;
use gents_protocol::output::reconstruction::{reconstruct_stream, ObservedSegment};
use gents_protocol::output::{
    OutputOutcome, OutputSource, OutputWriter, PayloadRef, SourceClose, StreamPayload,
};
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};
use tokio::sync::watch;
use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt};
use tracing_subscriber::{Layer, Registry};

use super::{AgentDaemon, AuxiliaryTask};
use crate::inference::backends::provider::BackendProviderKind;
use crate::inference::resolved::{ResolvedAgent, SamplingConfig};
use crate::lean_vocab_test::{
    LeanCanonicalExecutionCase, LeanCanonicalExecutionOperation, LeanCanonicalSource,
    LeanPayloadKind, LeanTerminalSelection,
};
use crate::request::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::request::watcher::{AgentRequest, DefraWatcher};
use crate::runtime::agent::completion_retry::CompletionRetryProfileFields;
use crate::runtime::agent::daemon::AgentDaemonInit;
use crate::runtime::agent::runtime::{
    run_router_with_watcher, RouterInputs, RuntimeAdmissionGate, StartupBarrier,
};
use crate::runtime::snapshot::ActiveRuntimeSnapshot;
use crate::runtime::status::RuntimeStatusHandle;
use crate::session::canonical_rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
use crate::store::config_client::ConfigAccess;
use crate::store::identity::{KeyIdentity, NodeIdentity, RuntimeNode};
use crate::tools::call::hook::{
    BackgroundExecutionRegistry, BackgroundToolRegistry, FailurePolicy,
};
use crate::tools::surface::AgentToolSurfaceConfig;
use crate::turn::prompt::LayeredPromptBuilder;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ReviewUsage {
    usage: rig::completion::Usage,
}

impl rig::completion::GetTokenUsage for ReviewUsage {
    fn token_usage(&self) -> Option<rig::completion::Usage> {
        Some(self.usage)
    }
}

#[derive(Clone)]
struct ReviewProvider {
    reply: Option<String>,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
    pause: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
}

impl CompletionModel for ReviewProvider {
    type Response = ();
    type StreamingResponse = ReviewUsage;
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        unreachable!("review tests construct the provider directly")
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        Err(CompletionError::ProviderError("stream only".into()))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<ReviewUsage>, CompletionError> {
        if let Some((entered, release)) = &self.pause {
            entered.notify_one();
            release.notified().await;
        }
        crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;
        let reply = self.reply.clone().unwrap_or_else(|| {
            let action_id = request.chat_history.iter().find_map(|message| {
                let rig::message::Message::User { content } = message else { return None; };
                content.iter().find_map(|part| {
                    let rig::message::UserContent::Text(text) = part else { return None; };
                    serde_json::from_str::<serde_json::Value>(&text.text).ok()?
                        .get("actionId")?.as_str().map(str::to_owned)
                })
            }).expect("review prompt names its invocation");
            serde_json::json!({"actionId":action_id,"decision":"allow","reason":"Within the requested work."}).to_string()
        });
        self.requests.lock().unwrap().push(request);
        let events = vec![
            RawStreamingChoice::Message(reply),
            RawStreamingChoice::FinalResponse(ReviewUsage {
                usage: rig::completion::Usage {
                    input_tokens: 3,
                    output_tokens: 5,
                    total_tokens: 8,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
            }),
        ];
        Ok(StreamingCompletionResponse::stream(Box::pin(stream::iter(
            events.into_iter().map(Ok),
        ))))
    }
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
    let provider = ReviewProvider {
        reply: Some(reply.clone()),
        requests: Arc::default(),
        pause: None,
    };
    let observed = provider.requests.clone();
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
        &fixture.node,
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
        let requests = observed.lock().unwrap();
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
        crate::goal::session_token_usage(
            &fixture.node,
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
            matches!(row.segment.source, OutputSource::ProviderTurn { scope, .. } if scope.kind == crate::inference::rendered_request::CaptureScopeKind::AccessReview)
        );
    }
    let session = crate::store::graphql::escape_graphql_string(&fixture.parent.session_id);
    let response = ConfigAccess::local(fixture.node.clone()).execute(&format!("{{ AgentSession(filter: {{ session_id: {{ _eq: \"{session}\" }} }}) {{ title }} AgentToolCall {{ _docID }} }}")).await.unwrap();
    assert!(response["data"]["AgentSession"][0]["title"].is_null());
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
    let doc = crate::store::graphql::escape_graphql_string(&fixture.parent.doc_id);
    ConfigAccess::local(fixture.node.clone()).write("test.goal_review_parent", &format!(
        r#"mutation {{ delete_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}) {{ _docID }} }}"#,
    )).await.unwrap();
    let error = crate::goal::session_token_usage(
        &fixture.node,
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
) -> crate::plugin::approval::workspace::ReviewTaskSelection {
    use crate::store::config_client::{DesiredStateApplyDocument, DesiredStateApplyPlan};
    let document = serde_json::json!({
        "node_did": fixture.parent.node_did,
        "task_id": "review",
        "agent_id": fixture.parent.agent_id,
        "prompt_template": prompt,
    });
    let selection = crate::plugin::approval::workspace::ReviewTaskSelection::for_task(
        &serde_json::from_value(document.clone()).unwrap(),
    );
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: crate::Collection::Task,
        add: document.clone(),
        update: document,
    }])
    .unwrap();
    ConfigAccess::transact_local_write(&fixture.node, None, "test.review_policy", |txn| {
        let plan = &plan;
        Box::pin(
            async move { crate::store::config_client::apply_desired_state_plan(txn, plan).await },
        )
    })
    .await
    .unwrap();
    selection
}

#[tokio::test]
async fn workspace_command_uses_the_configured_review_task_and_the_callers_ledger() {
    use crate::plugin::approval::workspace;

    let fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input("unused")),
        Some(10_000),
    )
    .await;
    let selection = install_review_policy(
        &fixture,
        "Allow commands that validate the requested project changes.",
    )
    .await;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let key = workspace::WorkspaceKey::canonical(
        &fixture.parent.node_did,
        fixture.parent.requester_did.as_deref().unwrap(),
        &fixture.parent.agent_id,
        project.path(),
    )
    .unwrap();
    workspace::save_mode(
        home.path(),
        &key,
        0,
        workspace::Mode::AutoReview,
        Some(&selection),
    )
    .unwrap();
    let calls = Arc::default();
    let provider = ReviewProvider {
        reply: None,
        requests: Arc::clone(&calls),
        pause: None,
    };
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let budget = crate::runtime::agent::loop_stream::AggregateTokenBudget::new(10_000);
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let reviewer = fixture
        .task(fixture.admitted(provider), true)
        .workspace_reviewer(
            &fixture.parent,
            Some(budget.clone()),
            tokio_util::sync::CancellationToken::new(),
            shutdown_rx,
            tasks.clone(),
        );
    let context = workspace::for_request(
        Some(home.path()),
        &fixture.parent.node_did,
        fixture.parent.requester_did.as_deref(),
        &fixture.parent.agent_id,
        Some(project.path()),
    )
    .unwrap()
    .unwrap()
    .with_reviewer(reviewer.callback.clone());
    let tool_root = key.path.parent().unwrap().to_owned();
    let tool_context = crate::tools::native::ToolContext::new(tool_root.clone(), false).unwrap();
    let (program, arguments) = if cfg!(windows) {
        (
            "cmd.exe",
            vec!["/D".into(), "/C".into(), "echo".into(), "fixture".into()],
        )
    } else {
        ("/bin/echo", vec!["fixture".into()])
    };
    let output = workspace::scope(
        Some(context),
        crate::tools::native::run_command_for_test(
            &tool_context,
            "bash_unrestricted",
            program,
            &arguments,
            key.path.to_str(),
            Duration::from_secs(5),
            &crate::tools::native::CommandExecutionPolicy::write_capable()
                .with_mode(crate::tools::native::CommandExecutionMode::Unrestricted),
            true,
            2048,
        ),
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
    drain_reviews(&tasks, true).await;
    assert!(reviewer.take_failure().is_none());
    assert_eq!(budget.snapshot().unwrap().used, 8);
    {
        let calls = calls.lock().unwrap();
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
        assert_eq!(input.action["workspace"], serde_json::json!(key.path));
        assert_eq!(
            input.action["subject"]["root"],
            serde_json::json!(tool_root)
        );
    }
    assert_eq!(fixture.session_message_count().await, 0);
    assert!(crate::plugin::approval::pending_requests(home.path())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn changing_the_review_task_while_it_runs_discards_its_allowance() {
    use crate::plugin::approval::workspace;

    let fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input("unused")),
        None,
    )
    .await;
    let selection = install_review_policy(&fixture, "Review the requested project changes.").await;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let key = workspace::WorkspaceKey::canonical(
        &fixture.parent.node_did,
        fixture.parent.requester_did.as_deref().unwrap(),
        &fixture.parent.agent_id,
        project.path(),
    )
    .unwrap();
    workspace::save_mode(
        home.path(),
        &key,
        0,
        workspace::Mode::AutoReview,
        Some(&selection),
    )
    .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let calls = Arc::default();
    let provider = ReviewProvider {
        reply: None,
        requests: Arc::clone(&calls),
        pause: Some((entered.clone(), release.clone())),
    };
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let reviewer = fixture
        .task(fixture.admitted(provider), true)
        .workspace_reviewer(
            &fixture.parent,
            None,
            tokio_util::sync::CancellationToken::new(),
            shutdown_rx,
            tasks.clone(),
        );
    let context = workspace::for_request(
        Some(home.path()),
        &fixture.parent.node_did,
        fixture.parent.requester_did.as_deref(),
        &fixture.parent.agent_id,
        Some(project.path()),
    )
    .unwrap()
    .unwrap()
    .with_reviewer(reviewer.callback.clone());
    let home_path = home.path().to_owned();
    let project_path = key.path.clone();
    let consent = tokio::spawn(workspace::scope(Some(context), async move {
        workspace::authorize_command(
            &["echo".into(), "fixture".into()],
            &project_path,
            &project_path,
            None,
            &crate::tools::native::CommandExecutionPolicy::write_capable(),
        )
        .await
    }));
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .unwrap();
    install_review_policy(&fixture, "Ask before running commands.").await;
    release.notify_one();
    let error = consent.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("not approved"));
    drain_reviews(&tasks, true).await;
    assert!(reviewer.take_failure().is_none());
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert!(crate::plugin::approval::pending_requests(&home_path)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn configuration_edits_cannot_replace_the_selected_review_policy() {
    use crate::plugin::approval::workspace;

    let fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input("unused")),
        None,
    )
    .await;
    let selected = install_review_policy(
        &fixture,
        "Ask before making changes outside the requested project.",
    )
    .await;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let key = workspace::WorkspaceKey::canonical(
        &fixture.parent.node_did,
        fixture.parent.requester_did.as_deref().unwrap(),
        &fixture.parent.agent_id,
        project.path(),
    )
    .unwrap();
    let saved = workspace::save_mode(
        home.path(),
        &key,
        0,
        workspace::Mode::AutoReview,
        Some(&selected),
    )
    .unwrap();
    let calls = Arc::default();
    let provider = ReviewProvider {
        reply: None,
        requests: Arc::clone(&calls),
        pause: None,
    };
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let reviewer = fixture
        .task(fixture.admitted(provider), true)
        .workspace_reviewer(
            &fixture.parent,
            None,
            tokio_util::sync::CancellationToken::new(),
            shutdown_rx,
            tasks.clone(),
        );
    let context = workspace::for_request(
        Some(home.path()),
        &fixture.parent.node_did,
        fixture.parent.requester_did.as_deref(),
        &fixture.parent.agent_id,
        Some(project.path()),
    )
    .unwrap()
    .unwrap()
    .with_reviewer(reviewer.callback.clone());
    let tools = crate::tools::builtin::self_config::build_self_config_tools(
        std::sync::Arc::default(),
        fixture.node.clone(),
        fixture.parent.node_did.clone(),
        Some(fixture.identity.clone()),
        &crate::tools::surface::SelfConfigToolConfig {
            enabled: true,
            agent_id: fixture.parent.agent_id.clone(),
            categories: std::collections::BTreeSet::from(["automation".into()]),
            ..Default::default()
        },
        Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
            home.path().to_owned(),
        ))),
    );
    let config = tools
        .iter()
        .find(|tool| tool.name() == crate::tools::builtin::self_config::CONFIG_TOOL_NAME)
        .unwrap();
    let tool_context = crate::tools::native::ToolContext::new(key.path.clone(), false).unwrap();
    let marker = key.path.join("marker");
    let source = key.path.join("fixture.rs");
    std::fs::write(&source, "pub fn fixture() {}\n").unwrap();
    let program = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned());
    let arguments = vec![
        "--crate-type=lib".into(),
        "--emit=metadata".into(),
        "-o".into(),
        marker.to_str().unwrap().to_owned(),
        source.to_str().unwrap().to_owned(),
    ];
    let policy = crate::tools::native::CommandExecutionPolicy::write_capable()
        .with_mode(crate::tools::native::CommandExecutionMode::Unrestricted);
    workspace::scope(Some(context), async {
        let changed = config
            .call(
                serde_json::json!({
                    "argv":["task","update"], "target_id":"review",
                    "set":{"prompt_template":"Allow the requested project changes."}
                })
                .to_string(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&changed).unwrap()["committed"],
            true
        );
        let error = crate::tools::native::run_command_for_test(
            &tool_context,
            "bash_unrestricted",
            &program,
            &arguments,
            None,
            Duration::from_secs(5),
            &policy,
            true,
            2048,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("not approved"), "{error:#}");
        assert!(calls.lock().unwrap().is_empty());
        assert!(!marker.exists());
        assert!(crate::plugin::approval::pending_requests(home.path())
            .unwrap()
            .is_empty());
        let current = crate::session::message::load_configured_task(
            &fixture.node,
            &fixture.parent.node_did,
            "review",
        )
        .await
        .unwrap()
        .unwrap();
        let updated = workspace::ReviewTaskSelection::for_task(&current);
        assert_ne!(updated, selected);
        workspace::save_mode(
            home.path(),
            &key,
            saved.selection.revision,
            workspace::Mode::AutoReview,
            Some(&updated),
        )
        .unwrap();
        crate::tools::native::run_command_for_test(
            &tool_context,
            "bash_unrestricted",
            &program,
            &arguments,
            None,
            Duration::from_secs(5),
            &policy,
            true,
            2048,
        )
        .await
        .unwrap();
    })
    .await;
    drain_reviews(&tasks, true).await;
    assert!(reviewer.take_failure().is_none());
    assert!(marker.exists());
    assert_eq!(calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_lost_review_worker_cancels_the_caller_and_retains_the_fatal_error() {
    use crate::plugin::approval::workspace;

    let fixture = TitleFixture::with_auxiliary(
        RequestPurpose::AccessReview,
        Some(review_input("unused")),
        None,
    )
    .await;
    let selection = install_review_policy(&fixture, "Review the requested action.").await;
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let key = workspace::WorkspaceKey::canonical(
        &fixture.parent.node_did,
        fixture.parent.requester_did.as_deref().unwrap(),
        &fixture.parent.agent_id,
        project.path(),
    )
    .unwrap();
    workspace::save_mode(
        home.path(),
        &key,
        0,
        workspace::Mode::AutoReview,
        Some(&selection),
    )
    .unwrap();
    let mut provider = TitleProvider::new(Vec::new(), false);
    provider.panic_on_stream = true;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let reviewer = fixture.task(provider, true).workspace_reviewer(
        &fixture.parent,
        None,
        cancellation.clone(),
        shutdown_rx,
        tasks.clone(),
    );
    let context = workspace::for_request(
        Some(home.path()),
        &fixture.parent.node_did,
        fixture.parent.requester_did.as_deref(),
        &fixture.parent.agent_id,
        Some(project.path()),
    )
    .unwrap()
    .unwrap()
    .with_reviewer(reviewer.callback.clone());
    let result = workspace::scope(
        Some(context),
        crate::tools::call::lifecycle::runtime::scope_request_tool_execution(
            None,
            cancellation.clone(),
            workspace::authorize_command(
                &["echo".into(), "fixture".into()],
                &key.path,
                &key.path,
                None,
                &crate::tools::native::CommandExecutionPolicy::write_capable(),
            ),
        ),
    )
    .await;
    assert!(result.is_err());
    assert!(cancellation.is_cancelled());
    assert!(super::super::ShutdownDrainFailure::occurs_in(
        &reviewer.take_failure().unwrap()
    ));
    tasks.close();
    let joined = tokio::time::timeout(Duration::from_secs(10), tasks.join_next())
        .await
        .unwrap()
        .unwrap();
    assert!(joined.unwrap_err().is_panic());
    assert!(tasks.join_next().await.is_none());
    assert!(crate::plugin::approval::pending_requests(home.path())
        .unwrap()
        .is_empty());
    assert_eq!(fixture.session_message_count().await, 0);
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
        let provider = TitleProvider::new(
            vec![
                RawStreamingChoice::Message("not used".into()),
                RawStreamingChoice::FinalResponse(()),
            ],
            false,
        );
        let calls = provider.calls.clone();
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
        assert_eq!(calls.load(Ordering::SeqCst), 0);
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
    let provider = TitleProvider::new(
        vec![
            RawStreamingChoice::Message(
                serde_json::json!({ "actionId": action_id, "decision": "allow", "reason": "Within the requested work." }).to_string(),
            ),
            RawStreamingChoice::FinalResponse(()),
        ],
        false,
    );
    let calls = provider.calls.clone();
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
        &fixture.node,
        &fixture.parent,
    )
    .await
    .unwrap_err();
    assert!(recovered
        .to_string()
        .contains("requires reported provider usage"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
    let calls = provider.calls.clone();
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
    wait_for_first_title_reasoning(&fixture).await;
    before_second.notify_one();
    tokio::time::timeout(Duration::from_secs(10), third_poll.notified())
        .await
        .unwrap();
    assert_eq!(open_title_reasoning(&fixture).await, ["first"]);
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    drain_reviews(&tasks, true).await;
    assert!(budget.snapshot().is_err());
    assert!(!caller_token.is_cancelled());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
    let provider = TitleProvider::new(
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
    let calls = provider.calls.clone();
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
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.session_message_count().await, 0);
    let response = ConfigAccess::local(fixture.node.clone())
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
    let (fields, _) = modeled_title_fields("title_reasoning_audit_complete_no_message");
    let mut provider = TitleProvider::new(provider_events(&fields, true), false);
    provider.fence_generation = Some((
        fixture.node.clone(),
        fixture.title.doc_id.clone(),
        fixture.title.node_did.clone(),
    ));
    let tasks = super::super::auxiliary_tasks::AuxiliaryTasks::default();
    let (_shutdown, shutdown_rx) = watch::channel(false);
    let error = fixture
        .task(provider, true)
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

#[derive(Clone)]
struct TitleProvider {
    events: Arc<Vec<RawStreamingChoice<()>>>,
    stall: bool,
    calls: Arc<AtomicUsize>,
    entered: Arc<tokio::sync::Notify>,
    before_second: Option<Arc<tokio::sync::Notify>>,
    third_poll: Option<Arc<tokio::sync::Notify>>,
    fence_generation: Option<(Arc<EmbeddedNode>, String, String)>,
    error: Option<String>,
    panic_on_stream: bool,
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

impl TitleProvider {
    fn new(events: Vec<RawStreamingChoice<()>>, stall: bool) -> Self {
        Self {
            events: Arc::new(events),
            stall,
            calls: Arc::new(AtomicUsize::new(0)),
            entered: Arc::new(tokio::sync::Notify::new()),
            before_second: None,
            third_poll: None,
            fence_generation: None,
            error: None,
            panic_on_stream: false,
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for TitleProvider {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        unreachable!("title tests construct the provider directly")
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        Err(CompletionError::ProviderError("stream only".into()))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        assert!(!self.panic_on_stream, "scripted review worker panic");
        self.calls.fetch_add(1, Ordering::SeqCst);
        crate::test_support::capture_scripted_provider_request(&request, "scripted").await?;
        if let Some(error) = &self.error {
            return Err(CompletionError::ProviderError(error.clone()));
        }
        if let Some((node, doc_id, node_did)) = &self.fence_generation {
            let doc = crate::store::graphql::escape_graphql_string(doc_id);
            let owner = crate::store::graphql::escape_graphql_string(node_did);
            ConfigAccess::local(node.clone())
                .write(
                    "test.title_fence_generation",
                    &format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }}, node_did: {{ _eq: "{owner}" }} }}, input: {{ execution_generation: "fenced-title-generation" }}) {{ _docID }} }}"#),
                )
                .await
                .expect("fence title generation");
        }
        self.entered.notify_one();
        let items = self.events.iter().cloned().map(Ok::<_, CompletionError>);
        let inner: rig::streaming::StreamingResult<()> = if let (
            Some(before_second),
            Some(third_poll),
        ) =
            (&self.before_second, &self.third_poll)
        {
            let before_second = before_second.clone();
            let third_poll = third_poll.clone();
            let mut events = items.collect::<Vec<_>>().into_iter();
            let first = events.next().expect("first reasoning event");
            let second = events.next().expect("second reasoning event");
            assert!(events.next().is_none());
            Box::pin(
                stream::once(async move { first })
                    .chain(stream::once(async move {
                        before_second.notified().await;
                        second
                    }))
                    .chain(stream::once(async move {
                        third_poll.notify_one();
                        futures::future::pending::<Result<RawStreamingChoice<()>, CompletionError>>(
                        )
                        .await
                    })),
            )
        } else if self.stall {
            Box::pin(stream::iter(items.collect::<Vec<_>>()).chain(stream::pending()))
        } else {
            Box::pin(stream::iter(items.collect::<Vec<_>>()))
        };
        Ok(StreamingCompletionResponse::stream(inner))
    }
}

struct TitleFixture {
    _directory: tempfile::TempDir,
    node: Arc<EmbeddedNode>,
    agent_config: Arc<ResolvedAgent>,
    identity: Arc<dyn NodeIdentity>,
    parent: AgentRequest,
    title: AgentRequest,
}

impl TitleFixture {
    fn admitted<M>(&self, model: M) -> crate::inference::admission::AdmittedCompletionModel<M> {
        let registry = crate::inference::admission::AdmissionRegistry::new(self.node.clone());
        let backend = self.agent_config.backend_id.as_ref().unwrap();
        registry.reconcile(
            1,
            &rapidhash::RapidHashMap::from_iter([(
                backend.clone(),
                crate::inference::admission::BackendAdmissionConfig {
                    backend_id: backend.clone(),
                    max_concurrent: 1,
                    max_queue_depth: 0,
                    enabled: true,
                    probe_status: "healthy".into(),
                    measured_unhealthy: false,
                    config_fingerprint: "scripted".into(),
                },
            )]),
        );
        crate::inference::admission::AdmittedCompletionModel::new(model, registry, "scripted")
    }

    async fn new() -> Self {
        Self::with_auxiliary(RequestPurpose::TitleAudit, None, None).await
    }

    async fn with_auxiliary(
        purpose: RequestPurpose,
        content: Option<String>,
        parent_limit: Option<u64>,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let identity: Arc<dyn NodeIdentity> = Arc::new(
            KeyIdentity::load_or_create(directory.path().join("principal.key"), None).unwrap(),
        );
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(identity.did())
                .data_path(directory.path())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let principal = Arc::new(RuntimeNode {
            node_did: identity.did().to_owned(),
            identity: identity.clone(),
            default_agent_id: "general".into(),
            display_name: None,
            enabled: true,
        });
        let agent_config = Arc::new(ResolvedAgent {
            profile_id: None,
            agent_id: "general".into(),
            node: principal,
            backend_id: Some("general:backend".into()),
            backend_provider_kind: BackendProviderKind::OpenAiCompatible,
            openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
            backend_endpoint: "http://127.0.0.1:1/v1".into(),
            backend_auth: crate::documents::BackendAuth::Unauthenticated,
            model_name: "scripted".into(),
            service_tier: None,
            resolved_reasoning_efforts: None,
            context_window: 8192,
            max_output_tokens: 1024,
            max_turns: 2,
            max_turns_provenance: crate::inference::resolved::MaxTurnsProvenance::Default,
            system_prompt: "system".into(),
            tools: AgentToolSurfaceConfig::meta_only(),
            compaction: None,
            compaction_inference: None,
            max_total_tokens: None,
            stream_batch_ms: 0,
            stream_liveness_timeout: Duration::from_secs(60),
            deadline_duration: Duration::from_secs(120),
            provider_idle_timeout: Duration::from_secs(
                crate::inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
            ),
            completion_retry: CompletionRetryProfileFields::default(),
            sampling: SamplingConfig::default(),
            skills: Vec::new(),
        });
        crate::test_support::install_test_agent(&node, identity.did(), &agent_config.agent_id)
            .await;
        let request_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            request_id,
            identity.did(),
            identity.did(),
            &agent_config.agent_id,
            session_id,
            "explain the request",
            "interactive",
            created_at,
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        crate::sign_agent_request_create(identity.as_ref(), &mut create)
            .await
            .unwrap();
        let response = ConfigAccess::local(node.clone())
            .write("test.title_parent", &create.graphql_mutation().unwrap())
            .await
            .unwrap();
        let parent_doc_id =
            crate::store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        let mut parent =
            crate::request::admission::load_request_for_admission_test(&node, &parent_doc_id)
                .await
                .unwrap();
        let session = gents_protocol::session::AgentSession {
            session_id: parent.session_id.clone(),
            node_did: parent.node_did.clone(),
            requester_did: parent.requester_did.clone(),
            agent_id: parent.agent_id.clone(),
            created_at: parent.created_at.clone(),
            closed_at: None,
            archived_at: None,
            archive_revision: 0,
            stopped_at: None,
            title: None,
            tags: vec![],
            provenance: None,
            observation: None,
        };
        let input =
            gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(session).unwrap())
                .unwrap();
        ConfigAccess::local(node.clone())
            .write(
                "test.title_session",
                &format!("mutation {{ create_AgentSession(input: {input}) {{ _docID }} }}"),
            )
            .await
            .unwrap();
        if purpose == RequestPurpose::AccessReview {
            let mut lifecycle = RequestLifecycle::new_with_execution_binding(
                node.clone(),
                &agent_config.agent_id,
                &parent.node_did,
                parent.clone(),
                agent_config.deadline_duration.as_secs(),
                crate::request::lifecycle::ExecutionOrigin::Interactive,
                agent_config.backend_id.clone().unwrap_or_default(),
            );
            lifecycle.set_configured_max_total_tokens(parent_limit);
            assert_eq!(
                lifecycle.claim_with_identity().await.unwrap(),
                ClaimOutcome::Claimed
            );
            let writer = crate::request::streaming::DefraStreamWriter::new(
                node.clone(),
                &parent.node_did,
                Duration::ZERO,
            );
            lifecycle.begin_owned_execution(&writer).await.unwrap();
            parent = lifecycle.request().clone();
        }
        let title = crate::request::lifecycle::materialize::write_pending_auxiliary_request(
            &node,
            &parent,
            purpose,
            content.unwrap_or_else(|| parent.content.clone()),
            (purpose == RequestPurpose::AccessReview).then(|| {
                (chrono::Utc::now() + chrono::Duration::seconds(60))
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            }),
        )
        .await
        .unwrap();
        Self {
            _directory: directory,
            node,
            agent_config,
            identity,
            parent,
            title,
        }
    }

    fn task<M>(&self, model: M, capture: bool) -> AuxiliaryTask<M>
    where
        M: crate::inference::llm::rig_compat::ProviderModel,
    {
        AuxiliaryTask {
            node: self.node.clone(),
            agent_config: self.agent_config.clone(),
            provider_family: None,
            model: Arc::new(model),
            service_admission: Default::default(),
            verifier: crate::request::admission::AgentRequestAdmissionVerifier::new(
                self.node.clone(),
                self.identity.clone(),
                crate::runtime::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
            capture_factory: capture.then(|| {
                crate::inference::rendered_request::defra_rendered_request_capture_factory(
                    self.node.clone(),
                )
            }),
        }
    }

    async fn terminalize_parent(&self) {
        crate::request::admission::terminalize_pending_request_rejection(
            &self.node,
            &self.parent.doc_id,
            &self.parent.node_did,
            "parent ended before title inference",
            "test.title_parent_terminal",
        )
        .await
        .unwrap();
        let doc = crate::store::graphql::escape_graphql_string(&self.parent.doc_id);
        let response = ConfigAccess::local(self.node.clone())
            .execute(&format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ lifecycle_state }} }}"))
            .await
            .unwrap();
        assert_eq!(
            response["data"]["AgentRequest"][0]["lifecycle_state"],
            "failed"
        );
    }

    async fn interrupt_parent(&self) {
        let mut lifecycle = crate::request::lifecycle::RequestLifecycle::new_with_execution_binding(
            self.node.clone(),
            &self.agent_config.agent_id,
            &self.parent.node_did,
            self.parent.clone(),
            self.agent_config.deadline_duration.as_secs(),
            crate::request::lifecycle::ExecutionOrigin::Interactive,
            self.agent_config.backend_id.clone().unwrap_or_default(),
        );
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            crate::request::lifecycle::ClaimOutcome::Claimed
        );
        crate::request::interrupt::interrupt_request_by_doc_id(
            &self.node,
            &self.parent.doc_id,
            &self.parent.node_did,
            self.parent.requester_did.as_deref(),
        )
        .await
        .unwrap();
        assert_eq!(
            lifecycle
                .terminalize_owned(
                    crate::request::lifecycle::RequestTerminalOutcome::Interrupted,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap(),
            crate::request::lifecycle::TerminalizeResult::Won
        );
        let doc = crate::store::graphql::escape_graphql_string(&self.parent.doc_id);
        let response = ConfigAccess::local(self.node.clone())
            .execute(&format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ lifecycle_state interrupt_requested_at terminal_output }} }}"))
            .await
            .unwrap();
        let row = &response["data"]["AgentRequest"][0];
        assert_eq!(row["lifecycle_state"], "interrupted");
        assert!(row["interrupt_requested_at"].as_str().is_some());
        assert_eq!(
            serde_json::from_value::<gents_protocol::output::TerminalOutput>(
                row["terminal_output"].clone()
            )
            .unwrap(),
            gents_protocol::output::TerminalOutput::NoMessage
        );
    }

    async fn session_message_count(&self) -> usize {
        let session = crate::store::graphql::escape_graphql_string(&self.parent.session_id);
        let response = ConfigAccess::local(self.node.clone())
            .execute(&format!("{{ AgentMessage(filter: {{ session_id: {{ _eq: \"{session}\" }} }}) {{ _docID }} }}"))
            .await
            .unwrap();
        response["data"]["AgentMessage"].as_array().unwrap().len()
    }

    async fn output_rows(&self) -> (Vec<crate::session::canonical_rows::OutputSegmentRow>, usize) {
        let doc = crate::store::graphql::escape_graphql_string(&self.title.doc_id);
        let query = format!("{{ AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: \"{doc}\" }} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} AgentMessage(filter: {{ request_doc_id: {{ _eq: \"{doc}\" }} }}) {{ _docID }} }}");
        let response = ConfigAccess::local(self.node.clone())
            .execute(&query)
            .await
            .unwrap();
        let data = &response["data"];
        let rows = data["AgentOutputSegment"]
            .as_array()
            .unwrap()
            .iter()
            .map(decode_output_segment_row)
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
        let doc = crate::store::graphql::escape_graphql_string(&self.title.doc_id);
        let query = format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ lifecycle_state terminal_output }} }}");
        let response = ConfigAccess::local(self.node.clone())
            .execute(&query)
            .await
            .unwrap();
        let row = &response["data"]["AgentRequest"][0];
        (
            serde_json::from_value(row["lifecycle_state"].clone()).unwrap(),
            serde_json::from_value(row["terminal_output"].clone()).unwrap(),
        )
    }
}

fn modeled_title_fields(name: &str) -> (Vec<(StreamPayload, String, u32, u32)>, OutputOutcome) {
    let case = crate::lean_vocab_test::lean_contract_snapshot().canonical_execution_gate_cases.iter().find(|case| matches!(case, LeanCanonicalExecutionCase::NativeExecution { name: found, .. } | LeanCanonicalExecutionCase::ModelExecution { name: found, .. } if found == name)).expect("Lean title script");
    let (LeanCanonicalExecutionCase::NativeExecution {
        operations,
        expected_observations,
        ..
    }
    | LeanCanonicalExecutionCase::ModelExecution {
        operations,
        expected_observations,
        ..
    }) = case
    else {
        unreachable!()
    };
    assert_eq!(operations.len(), expected_observations.len());
    assert!(
        expected_observations.iter().all(|step| step.accepted),
        "{name}: modeled title path changed"
    );
    let final_state = expected_observations.last().unwrap();
    assert_eq!(
        final_state.terminal_selection,
        Some(LeanTerminalSelection::NoMessage)
    );
    assert!(final_state.messages.is_empty());
    let LeanCanonicalExecutionOperation::AppendOutput { record, .. } = &operations[0] else {
        panic!("{name}: first modeled action changed")
    };
    assert!(matches!(
        &record.coordinate.source,
        LeanCanonicalSource::Auxiliary {
            auxiliary_kind: crate::lean_vocab_test::LeanAuxiliaryKind::Title,
            ..
        }
    ));
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
                LeanPayloadKind::Reasoning => StreamPayload::Reasoning,
                LeanPayloadKind::Signature => StreamPayload::ReasoningSignature,
                LeanPayloadKind::Encrypted => StreamPayload::ReasoningEncrypted,
                LeanPayloadKind::Redacted => StreamPayload::ReasoningRedacted,
                _ => panic!("{name}: title fixture changed payload kind"),
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
    let closure = final_state
        .segments
        .iter()
        .find_map(|record| record.close.as_ref())
        .expect("modeled title closure");
    let crate::lean_vocab_test::LeanCanonicalClosure::Closed { outcome, .. } = closure else {
        panic!("modeled title retracted")
    };
    let outcome = match outcome {
        crate::lean_vocab_test::LeanOutcome::Complete => OutputOutcome::Complete,
        crate::lean_vocab_test::LeanOutcome::Partial => OutputOutcome::Partial,
    };
    (fields, outcome)
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
            content: crate::inference::llm::rig_compat::to_rig_reasoning_part(&content),
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
            crate::inference::rendered_request::CaptureScopeKind::Title
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
    for _ in 0..2_000 {
        let (rows, _) = fixture.output_rows().await;
        let sources = rows
            .iter()
            .filter_map(|row| match &row.segment.source {
                OutputSource::ProviderTurn { scope, attempt, .. }
                    if scope.kind
                        == crate::inference::rendered_request::CaptureScopeKind::Title =>
                {
                    Some((scope.seq, *attempt))
                }
                _ => None,
            })
            .collect::<rapidhash::RapidHashSet<_>>();
        let all_received = sources.len() >= minimum
            && sources.iter().all(|(sequence, attempt)| {
                let source = OutputSource::ProviderTurn {
                    scope: crate::inference::rendered_request::CaptureScope {
                        kind: crate::inference::rendered_request::CaptureScopeKind::Title,
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
                reconstruct_dense_prefix(&observed, &fixture.title.doc_id, &source, &writer, None)
                    .is_ok_and(|prefix| prefix.streams.len() >= fields)
            });
        if all_received {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("title reasoning did not reach the durable output owner");
}

#[tokio::test]
async fn title_reasoning_survives_parent_terminal_under_own_request() {
    let (fields, outcome) = modeled_title_fields("title_reasoning_audit_complete_no_message");
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
        let provider = TitleProvider::new(provider_events(&fields, true), false);
        let calls = provider.calls.clone();
        let (_shutdown, rx) = tokio::sync::watch::channel(false);
        fixture
            .task(provider, true)
            .run(fixture.title.clone(), rx)
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "parent: {parent_state:?}");
        assert_title_audit(&fixture, &fields, outcome, 1, true).await;
        assert_eq!(
            fixture.session_message_count().await,
            public_head_before,
            "parent: {parent_state:?}"
        );
        assert_eq!(
            fixture.terminal_row().await,
            (
                RequestLifecycleState::Completed,
                Some(gents_protocol::output::TerminalOutput::NoMessage)
            ),
            "parent: {parent_state:?}"
        );
    }
}

#[tokio::test]
async fn title_timeout_drains_each_received_reasoning_attempt_as_partial() {
    let (fields, outcome) = modeled_title_fields("title_live_partial_retains_received_reasoning");
    let fixture = TitleFixture::new().await;
    let provider = TitleProvider::new(provider_events(&fields, false), true);
    let calls = provider.calls.clone();
    let entered = provider.entered.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    let task = fixture.task(provider, true);
    let title = fixture.title.clone();
    let running = tokio::spawn(async move { task.run(title, rx).await });
    tokio::time::timeout(Duration::from_secs(15), entered.notified())
        .await
        .expect("first title provider attempt starts");
    wait_for_title_streams(&fixture, 1, fields.len()).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    tokio::time::resume();
    tokio::time::timeout(Duration::from_secs(15), entered.notified())
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
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(outcome, OutputOutcome::Partial);
    assert_title_audit(&fixture, &fields, outcome, 2, false).await;
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Completed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
}

#[tokio::test]
async fn crashed_title_recovers_committed_reasoning_without_publication() {
    let (fields, outcome) = modeled_title_fields("title_expired_unlatched_recovery_no_message");
    let model = crate::lean_vocab_test::lean_contract_snapshot()
        .canonical_execution_gate_cases
        .iter()
        .find(|case| matches!(case, LeanCanonicalExecutionCase::NativeExecution { name, .. } | LeanCanonicalExecutionCase::ModelExecution { name, .. } if name == "title_expired_unlatched_recovery_no_message"))
        .expect("modeled title recovery");
    let (LeanCanonicalExecutionCase::NativeExecution {
        expected_observations,
        ..
    }
    | LeanCanonicalExecutionCase::ModelExecution {
        expected_observations,
        ..
    }) = model
    else {
        unreachable!()
    };
    assert_eq!(
        expected_observations.last().unwrap().request_state,
        "failed"
    );
    assert_eq!(outcome, OutputOutcome::Partial);
    let fixture = TitleFixture::new().await;
    let provider = TitleProvider::new(provider_events(&fields, false), true);
    let entered = provider.entered.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    let task = fixture.task(provider, true);
    let title = fixture.title.clone();
    let running = tokio::spawn(async move { task.run(title, rx).await });
    entered.notified().await;
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
    let physical = crate::store::graphql::escape_graphql_string(&fixture.title.doc_id);
    let owner = crate::store::graphql::escape_graphql_string(fixture.identity.did());
    let response = ConfigAccess::local(fixture.node.clone())
        .execute(&format!(r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }}, node_did: {{ _eq: "{owner}" }} }}, limit: 2) {{ execution_generation lifecycle_state }} }}"#))
        .await
        .expect("read title lease owner");
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .expect("title lease rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["lifecycle_state"], "processing");
    let generation = crate::store::graphql::escape_graphql_string(
        rows[0]["execution_generation"]
            .as_str()
            .expect("title execution generation"),
    );
    let past = crate::store::graphql::escape_graphql_string(
        &(chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339(),
    );
    let expired = ConfigAccess::local(fixture.node.clone())
        .write("test.title_expire_own_lease", &format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{physical}" }}, node_did: {{ _eq: "{owner}" }}, execution_generation: {{ _eq: "{generation}" }}, lifecycle_state: {{ _eq: "processing" }} }}, input: {{ execution_lease_expires_at: "{past}" }}) {{ _docID }} }}"#))
        .await
        .expect("expire exact title generation");
    assert_eq!(
        expired["data"]["update_AgentRequest"][0]["_docID"],
        fixture.title.doc_id
    );

    let first = crate::RequestLifecycle::recover_all(&fixture.node, fixture.identity.did())
        .await
        .expect("recover crashed title");
    assert_eq!(first.requests_recovered, 1);
    assert_title_audit(&fixture, &fields, outcome, 1, false).await;
    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Failed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
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

    let replay = crate::RequestLifecycle::recover_all(&fixture.node, fixture.identity.did())
        .await
        .expect("repeat title recovery");
    assert_eq!(replay.requests_recovered, 0);
    assert_title_audit(&fixture, &fields, outcome, 1, false).await;
    assert_eq!(fixture.output_rows().await.0.len(), after.len());
}

#[tokio::test]
async fn missing_title_capture_fails_before_provider_dispatch() {
    let (fields, _) = modeled_title_fields("title_reasoning_audit_complete_no_message");
    let fixture = TitleFixture::new().await;
    let provider = TitleProvider::new(provider_events(&fields, true), false);
    let calls = provider.calls.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    assert!(fixture
        .task(provider, false)
        .run(fixture.title.clone(), rx)
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
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
    let capture = TitleWarningCapture::default();
    let warnings = capture.0.clone();
    let subscriber = tracing::Dispatch::new(Registry::default().with(capture));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);
    let (fields, outcome) = modeled_title_fields("title_reasoning_audit_complete_no_message");
    for preexisting in [false, true] {
        let mut fixture = TitleFixture::new().await;
        fixture.terminalize_parent().await;
        if !preexisting {
            crate::request::admission::terminalize_pending_request_rejection(
                &fixture.node,
                &fixture.title.doc_id,
                &fixture.title.node_did,
                "retire setup title before live creation",
                "test.title_live_creation_setup",
            )
            .await
            .unwrap();
        }

        let provider = TitleProvider::new(provider_events(&fields, true), false);
        let calls = provider.calls.clone();
        let prompt_builder = LayeredPromptBuilder::for_agent(
            &fixture.agent_config.system_prompt,
            &fixture.agent_config.agent_id,
            &[],
            false,
            &[],
        );
        let preamble = prompt_builder.preamble().to_string();
        let (status_owner, status) = RuntimeStatusHandle::start_with_unbounded_test_clock(
            fixture.node.clone(),
            fixture.identity.did().to_owned(),
        );
        status.initialize_startup("general").await.unwrap();
        status
            .readiness()
            .register_slot("general", 1)
            .await
            .unwrap();
        let (dispatch, receiver) = tokio::sync::mpsc::channel(8);
        let snapshot = Arc::new(ActiveRuntimeSnapshot {
            generation: 1,
            node: None,
            local_did: String::new(),
            default_agent_id: "general".into(),
            agents: Default::default(),
            tool_surfaces: Default::default(),
            backend_admission_configs: Default::default(),
            unavailable_agents: Default::default(),
            active_schedules: Default::default(),
            unavailable_schedules: Default::default(),
            active_event_triggers: Default::default(),
            unavailable_event_triggers: Default::default(),
            active_tasks: Default::default(),
            dispatchers: rapidhash::RapidHashMap::from_iter([("general".into(), dispatch)]),
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
        let (_snapshot_tx, snapshot_rx) = tokio::sync::watch::channel(snapshot);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let mut daemon = AgentDaemon::new(AgentDaemonInit {
            node: fixture.node.clone(),
            agent_config: fixture.agent_config.clone(),
            provider_family: None,
            model: Arc::new(provider),
            preamble,
            loop_tools: Arc::new(Vec::new()),
            prompt_builder,
            hook_failure_policy: FailurePolicy::default(),
            rendered_request_capture_factory: Some(
                crate::inference::rendered_request::defra_rendered_request_capture_factory(
                    fixture.node.clone(),
                ),
            ),
            background_tool_registry: BackgroundToolRegistry::default(),
            background_execution_registry: BackgroundExecutionRegistry::default(),
            startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
            runtime_status: status.clone(),
            slot_generation: 1,
            request_admission: crate::request::admission::AgentRequestAdmissionVerifier::new(
                fixture.node.clone(),
                fixture.identity.clone(),
                crate::runtime::agent::p2p_reconcile::enrollment_authority_channel().1,
            ),
        })
        .unwrap();
        let daemon_rx = Arc::new(tokio::sync::Mutex::new(receiver));
        let daemon_shutdown = shutdown_rx.clone();
        let gate = RuntimeAdmissionGate::closed();
        gate.open().await;
        let router_gate = gate.clone();
        let router_node = fixture.node.clone();
        let router_did = fixture.identity.did().to_owned();
        let router_task = tokio::spawn(async move {
            run_router_with_watcher(RouterInputs {
                node: router_node.clone(),
                node_did: router_did.clone(),
                watcher: DefraWatcher::new(router_node, &router_did).expect("request watcher"),
                active_snapshot_rx: snapshot_rx,
                shutdown: shutdown_rx,
                admission_gate: router_gate,
                runtime_status: status,
                runtime_snapshot_observer: None,
            })
            .await
        });

        if !preexisting {
            daemon.spawn_conversation_title_generation(&fixture.parent);
            let old_doc_id = fixture.title.doc_id.clone();
            let session = crate::store::graphql::escape_graphql_string(&fixture.parent.session_id);
            let purpose =
                crate::store::graphql::escape_graphql_string(RequestPurpose::TitleAudit.as_str());
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                let response = ConfigAccess::local(fixture.node.clone())
                    .execute(&format!("{{ AgentRequest(filter: {{ session_id: {{ _eq: \"{session}\" }}, purpose: {{ _eq: \"{purpose}\" }} }}) {{ _docID }} }}"))
                    .await
                    .unwrap();
                let next = response["data"]["AgentRequest"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(|row| row["_docID"].as_str())
                    .find(|doc_id| *doc_id != old_doc_id);
                if let Some(doc_id) = next {
                    fixture.title = crate::request::admission::load_request_for_admission_test(
                        &fixture.node,
                        doc_id,
                    )
                    .await
                    .unwrap();
                    break;
                }
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "title creator did not persist a request"
                );
                tokio::task::yield_now().await;
            }
        }
        if !preexisting {
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "creator dispatched before daemon"
            );
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

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            if fixture.terminal_row().await.0 == RequestLifecycleState::Completed {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "watcher did not complete title audit"
            );
            tokio::task::yield_now().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "preexisting={preexisting}");
        assert_title_audit(&fixture, &fields, outcome, 1, true).await;
        assert_eq!(
            fixture.terminal_row().await,
            (
                RequestLifecycleState::Completed,
                Some(gents_protocol::output::TerminalOutput::NoMessage)
            )
        );
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

async fn start_owned_title_daemon(
    fixture: &TitleFixture,
    provider: TitleProvider,
) -> (
    tokio::sync::mpsc::Sender<AgentRequest>,
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    crate::runtime::status::RuntimeStatusOwner,
) {
    let prompt_builder = LayeredPromptBuilder::for_agent(
        &fixture.agent_config.system_prompt,
        &fixture.agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let preamble = prompt_builder.preamble().to_string();
    let (status_owner, status) = RuntimeStatusHandle::start_with_unbounded_test_clock(
        fixture.node.clone(),
        fixture.identity.did().to_owned(),
    );
    status.initialize_startup("general").await.unwrap();
    status
        .readiness()
        .register_slot("general", 1)
        .await
        .unwrap();
    let (dispatch, receiver) = tokio::sync::mpsc::channel(8);
    let snapshot = ActiveRuntimeSnapshot {
        generation: 1,
        node: None,
        local_did: String::new(),
        default_agent_id: "general".into(),
        agents: Default::default(),
        tool_surfaces: Default::default(),
        backend_admission_configs: Default::default(),
        unavailable_agents: Default::default(),
        active_schedules: Default::default(),
        unavailable_schedules: Default::default(),
        active_event_triggers: Default::default(),
        unavailable_event_triggers: Default::default(),
        active_tasks: Default::default(),
        dispatchers: rapidhash::RapidHashMap::from_iter([("general".into(), dispatch.clone())]),
        agent_executor_capacities: Default::default(),
        agent_executor_queue_capacities: Default::default(),
    };
    status
        .readiness()
        .publish_snapshot(&snapshot)
        .await
        .unwrap();
    status
        .set_process_state_durable(crate::runtime::agent::ProcessLifecycleState::Ready)
        .await
        .unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mut daemon = AgentDaemon::new(AgentDaemonInit {
        node: fixture.node.clone(),
        agent_config: fixture.agent_config.clone(),
        provider_family: None,
        model: Arc::new(provider),
        preamble,
        loop_tools: Arc::new(Vec::new()),
        prompt_builder,
        hook_failure_policy: FailurePolicy::default(),
        rendered_request_capture_factory: Some(
            crate::inference::rendered_request::defra_rendered_request_capture_factory(
                fixture.node.clone(),
            ),
        ),
        background_tool_registry: BackgroundToolRegistry::default(),
        background_execution_registry: BackgroundExecutionRegistry::default(),
        startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
        runtime_status: status,
        slot_generation: 1,
        request_admission: crate::request::admission::AgentRequestAdmissionVerifier::new(
            fixture.node.clone(),
            fixture.identity.clone(),
            crate::runtime::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    })
    .unwrap();
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
    TitleProvider,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
) {
    let events = ["first", " second"]
        .into_iter()
        .map(|text| RawStreamingChoice::Reasoning {
            id: None,
            content: crate::inference::llm::rig_compat::to_rig_reasoning_part(
                &ReasoningContent::Text {
                    text: text.into(),
                    signature: None,
                },
            ),
        })
        .collect();
    let before_second = Arc::new(tokio::sync::Notify::new());
    let third_poll = Arc::new(tokio::sync::Notify::new());
    let mut provider = TitleProvider::new(events, false);
    provider.before_second = Some(before_second.clone());
    provider.third_poll = Some(third_poll.clone());
    (provider, before_second, third_poll)
}

async fn wait_for_first_title_reasoning(fixture: &TitleFixture) {
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let (rows, _) = fixture.output_rows().await;
            if rows.iter().any(|row| !row.segment.runs.is_empty()) {
                assert_eq!(open_title_reasoning(fixture).await, ["first"]);
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("first title reasoning never became durable");
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
    let calls = provider.calls.clone();
    let (dispatch, shutdown, daemon, status_owner) =
        start_owned_title_daemon(&fixture, provider).await;
    dispatch.send(fixture.title.clone()).await.unwrap();
    wait_for_first_title_reasoning(&fixture).await;
    before_second.notify_one();
    tokio::time::timeout(Duration::from_secs(10), third_poll.notified())
        .await
        .expect("title provider did not process both reasoning chunks");
    assert_eq!(open_title_reasoning(&fixture).await, ["first"]);
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
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
    let calls = provider.calls.clone();
    let task = fixture.task(provider, true);
    let title_request = fixture.title.clone();
    let (_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
    let running = tokio::spawn(async move { task.run(title_request, shutdown_rx).await });
    wait_for_first_title_reasoning(&fixture).await;
    before_second.notify_one();
    tokio::time::timeout(Duration::from_secs(10), third_poll.notified())
        .await
        .expect("title provider did not process both reasoning chunks");
    assert_eq!(open_title_reasoning(&fixture).await, ["first"]);
    assert_eq!(
        fixture.terminal_row().await.0,
        RequestLifecycleState::Processing
    );
    crate::request::interrupt::interrupt_request_by_doc_id(
        &fixture.node,
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
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
    let calls = provider.calls.clone();
    let (dispatch, shutdown, daemon, status_owner) =
        start_owned_title_daemon(&fixture, provider).await;
    dispatch.send(fixture.title.clone()).await.unwrap();
    wait_for_first_title_reasoning(&fixture).await;
    before_second.notify_one();
    tokio::time::timeout(Duration::from_secs(10), third_poll.notified())
        .await
        .expect("title provider did not process both reasoning chunks");
    assert_eq!(open_title_reasoning(&fixture).await, ["first"]);
    let (before, before_messages) = fixture.output_rows().await;
    assert_eq!(before_messages, 0);
    assert!(before.iter().all(|row| row.segment.close.is_none()));
    let doc = crate::store::graphql::escape_graphql_string(&fixture.title.doc_id);
    let owner = crate::store::graphql::escape_graphql_string(&fixture.title.node_did);
    ConfigAccess::local(fixture.node.clone())
        .write(
            "test.title_shutdown_fence_generation",
            &format!(r#"mutation {{ update_AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }}, node_did: {{ _eq: "{owner}" }} }}, input: {{ execution_generation: "fenced-title-generation" }}) {{ _docID }} }}"#),
        )
        .await
        .unwrap();
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
    assert_eq!(calls.load(Ordering::SeqCst), 1);
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
    let capture = TitleWarningCapture::default();
    let warnings = capture.0.clone();
    let subscriber = tracing::Dispatch::new(Registry::default().with(capture));
    let _subscriber_guard = tracing::dispatcher::set_default(&subscriber);
    let (fields, outcome) = modeled_title_fields("title_live_partial_retains_received_reasoning");
    assert_eq!(outcome, OutputOutcome::Partial);
    let fixture = TitleFixture::new().await;
    let provider = TitleProvider::new(reasoning_only_events(&fields), false);
    let calls = provider.calls.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    fixture
        .task(provider, true)
        .run(fixture.title.clone(), rx)
        .await
        .expect("reasoning-only title is an ordinary provider result");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        usize::try_from(super::TITLE_GENERATION_MAX_ATTEMPTS).unwrap()
    );
    assert_title_audit(&fixture, &fields, outcome, 2, false).await;

    let doc = crate::store::graphql::escape_graphql_string(&fixture.title.doc_id);
    let response = ConfigAccess::local(fixture.node.clone())
        .execute(&format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ execution_generation }} }}"))
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

    assert_eq!(
        fixture.terminal_row().await,
        (
            RequestLifecycleState::Completed,
            Some(gents_protocol::output::TerminalOutput::NoMessage)
        )
    );
    let session = crate::store::graphql::escape_graphql_string(&fixture.parent.session_id);
    let response = ConfigAccess::local(fixture.node.clone())
        .execute(&format!(
            "{{ AgentSession(filter: {{ session_id: {{ _eq: \"{session}\" }} }}) {{ title }} }}"
        ))
        .await
        .unwrap();
    let title = &response["data"]["AgentSession"][0]["title"];
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

#[tokio::test]
async fn a_usage_limited_title_falls_back_without_failing_the_turn() {
    let fixture = TitleFixture::new().await;
    let backend = fixture.agent_config.backend_id.clone().unwrap();
    let registry = crate::inference::admission::AdmissionRegistry::new(fixture.node.clone());
    registry.reconcile(
        1,
        &rapidhash::RapidHashMap::from_iter([(
            backend.clone(),
            crate::inference::admission::BackendAdmissionConfig {
                backend_id: backend.clone(),
                max_concurrent: 1,
                max_queue_depth: 1,
                enabled: true,
                probe_status: "healthy".into(),
                measured_unhealthy: false,
                config_fingerprint: backend.clone(),
            },
        )]),
    );
    let resets_at = chrono::Utc::now() + chrono::Duration::hours(2);
    let mut provider = TitleProvider::new(Vec::new(), false);
    provider.error = Some(format!(
        r#"Invalid status code 429 Too Many Requests with message: {{"error":{{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_at":{}}}}}"#,
        resets_at.timestamp()
    ));
    let calls = provider.calls.clone();
    let model = crate::inference::admission::AdmittedCompletionModel::new(
        provider,
        registry,
        format!("{backend}:connection"),
    );
    let parent_state = |node: Arc<EmbeddedNode>, doc: String| async move {
        let doc = crate::store::graphql::escape_graphql_string(&doc);
        ConfigAccess::local(node)
            .execute(&format!("{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc}\" }} }}, limit: 1) {{ lifecycle_state }} }}"))
            .await
            .unwrap()["data"]["AgentRequest"][0]["lifecycle_state"]
            .clone()
    };
    let parent_before = parent_state(fixture.node.clone(), fixture.parent.doc_id.clone()).await;

    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    fixture
        .task(model, true)
        .run(fixture.title.clone(), rx)
        .await
        .expect("a usage-limited title is an ordinary provider failure");

    assert_eq!(
        calls.load(Ordering::SeqCst),
        usize::try_from(super::TITLE_GENERATION_MAX_ATTEMPTS).unwrap()
    );
    let request_id = crate::store::graphql::escape_graphql_string(&fixture.title.request_id);
    let response = ConfigAccess::local(fixture.node.clone())
        .execute(&format!("{{ InferenceCall(filter: {{ request_id: {{ _eq: \"{request_id}\" }} }}) {{ call_kind call_state failure_reason }} }}"))
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
        let Some(gents_loop::provider_limit::ProviderLimit::UsageExhausted(limit)) =
            gents_loop::provider_limit::classify_provider_limit(reason, chrono::Utc::now())
        else {
            panic!("recorded reason is not a usage limit: {reason}");
        };
        assert_eq!(
            limit.resets_at.map(|at| at.timestamp()),
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
    let session = crate::store::graphql::escape_graphql_string(&fixture.parent.session_id);
    let response = ConfigAccess::local(fixture.node.clone())
        .execute(&format!(
            "{{ AgentSession(filter: {{ session_id: {{ _eq: \"{session}\" }} }}) {{ title }} }}"
        ))
        .await
        .unwrap();
    assert_eq!(
        response["data"]["AgentSession"][0]["title"]["text"],
        super::sanitize_generated_title("", &fixture.parent.content)
    );
    assert_eq!(
        parent_state(fixture.node.clone(), fixture.parent.doc_id.clone()).await,
        parent_before
    );
}

#[tokio::test]
async fn fenced_title_output_fails_closed_without_retry() {
    let (fields, _) = modeled_title_fields("title_live_partial_retains_received_reasoning");
    let fixture = TitleFixture::new().await;
    let mut provider = TitleProvider::new(reasoning_only_events(&fields), false);
    provider.fence_generation = Some((
        fixture.node.clone(),
        fixture.title.doc_id.clone(),
        fixture.title.node_did.clone(),
    ));
    let calls = provider.calls.clone();
    let (_shutdown, rx) = tokio::sync::watch::channel(false);
    let error = fixture
        .task(provider, true)
        .run(fixture.title.clone(), rx)
        .await
        .expect_err("a fenced auxiliary writer must fail the title");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "storage failure retried");
    assert!(
        format!("{error:#}").contains("lost its live processing lease"),
        "{error:#}"
    );
    let session = crate::store::graphql::escape_graphql_string(&fixture.parent.session_id);
    let response = ConfigAccess::local(fixture.node.clone())
        .execute(&format!(
            "{{ AgentSession(filter: {{ session_id: {{ _eq: \"{session}\" }} }}) {{ title }} }}"
        ))
        .await
        .unwrap();
    assert!(response["data"]["AgentSession"][0]["title"].is_null());
}

#[tokio::test]
async fn title_output_cap_applies_only_when_reasoning_is_disabled_on_the_wire() {
    use crate::inference::resolved::ReasoningEffort;
    use crate::OpenAiWireApi;
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
            crate::inference::rendered_request::CaptureScopeKind::Title,
        );
        assert_eq!(
            super::title_max_tokens(config.additional_params.as_ref(), configured),
            expected,
            "{kind:?} {wire:?} {effort:?}"
        );
    }

    agent_config.backend_provider_kind = BackendProviderKind::OpenAiCompatible;
    agent_config.openai_wire_api = chat;
    agent_config.sampling.reasoning_effort = Some(ReasoningEffort::None);
    let config = crate::turn::completion_factory::loop_config(
        &agent_config,
        super::title_generation_preamble(),
        0,
        crate::inference::rendered_request::CaptureScopeKind::Title,
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
