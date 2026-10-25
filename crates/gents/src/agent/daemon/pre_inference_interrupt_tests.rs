use gents_protocol::error_chain::ErrorChain;
use tracing::instrument::WithSubscriber;

type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Clone, Default)]
pub(in crate::agent::daemon) struct InterruptionCapture(Arc<std::sync::Mutex<Vec<String>>>);

impl InterruptionCapture {
    pub(in crate::agent::daemon) fn subscriber(&self) -> impl tracing::Subscriber + Send + Sync {
        use tracing_subscriber::layer::SubscriberExt;
        tracing_subscriber::Registry::default().with(self.clone())
    }

    pub(in crate::agent::daemon) fn assert_source(&self, source: &str) {
        assert_eq!(*self.0.lock().unwrap(), [source]);
    }
}

impl<S> tracing_subscriber::Layer<S> for InterruptionCapture
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        struct SourceVisitor<'a>(&'a mut Vec<String>);
        impl tracing::field::Visit for SourceVisitor<'_> {
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "cancellation_source" {
                    self.0.push(value.to_owned());
                }
            }

            fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
        }
        if event.metadata().target() == "gents::agent::daemon" {
            event.record(&mut SourceVisitor(&mut self.0.lock().unwrap()));
        }
    }
}

#[derive(Clone)]
struct PreInferenceBlockingTool {
    entered: Arc<tokio::sync::Notify>,
}

impl crate::llm::tool::ToolDyn for PreInferenceBlockingTool {
    fn name(&self) -> String {
        "pre_inference_block".into()
    }

    fn definition<'a>(
        &'a self,
        _: String,
    ) -> crate::llm::tool::BoxFuture<'a, crate::llm::tool::ToolDefinition> {
        Box::pin(async move {
            self.entered.notify_one();
            std::future::pending().await
        })
    }

    fn call<'a>(
        &'a self,
        _: String,
    ) -> crate::llm::tool::BoxFuture<'a, Result<String, crate::llm::tool::ToolError>> {
        Box::pin(async { unreachable!("the pre-inference window never dispatches a tool") })
    }
}

#[derive(Clone)]
struct ProviderCallCountingModel(Arc<AtomicUsize>);

#[allow(refining_impl_trait)]
impl CompletionModel for ProviderCallCountingModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &(), _: impl Into<String>) -> Self {
        Self(Arc::new(AtomicUsize::new(0)))
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(CompletionError::ProviderError("streaming only".into()))
    }

    async fn stream(
        &self,
        _: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(StreamingCompletionResponse::stream(Box::pin(stream::iter(
            vec![Ok(RawStreamingChoice::FinalResponse(()))],
        ))))
    }
}

fn pre_inference_agent() -> Arc<ResolvedAgent> {
    let agent_config = test_agent();
    let mut agent_config = ResolvedAgent::clone(agent_config.as_ref());
    agent_config.stream_liveness_timeout = Duration::from_secs(600);
    agent_config.deadline_duration = Duration::from_secs(1_200);
    Arc::new(agent_config)
}

async fn seed_titled_session(
    node: &Arc<defra_node::EmbeddedNode>,
    request: &AgentRequest,
    agent_config: &ResolvedAgent,
) -> TestResult {
    let title = gents_protocol::session::SessionTitle {
        text: "pre-inference interrupt".into(),
        source: gents_protocol::session::SessionTitleSource::Task,
    };
    let created = crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "session.create",
        move |txn| {
            let title = title.clone();
            Box::pin(async move {
                crate::session::ensure_session_in_txn(
                    txn,
                    &request.session_id,
                    &request.node_did,
                    &agent_config.agent_id,
                    request.requester_did.as_deref(),
                    Some(title),
                    None,
                    &request.created_at,
                )
                .await
            })
        },
    )
    .await?;
    if !created {
        return Err("seeded session already existed without this title".into());
    }
    Ok(())
}

async fn request_terminal_row(
    node: &defra_node::EmbeddedNode,
    request_doc_id: &str,
) -> TestResult<serde_json::Value> {
    let doc_id = crate::graphql::escape_graphql_string(request_doc_id);
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                lifecycle_state failure_reason terminalized_at interrupt_requested_at stopped_before_running
            }} }}"#
        ),
        "pre_inference_interrupt_request_row",
    )
    .await?;
    let row = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .ok_or("AgentRequest read selected no row for the claimed request")?;
    Ok(row)
}

#[tokio::test]
async fn interrupt_while_preparing_provider_input_terminalizes_through_the_owner() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let agent_config = pre_inference_agent();
    let node_did = agent_config.node_did().to_owned();
    let identity = agent_config.node_identity().clone();
    let prompt = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let mut daemon = AgentDaemon::new(AgentDaemonInit {
        node: node.clone(),
        agent_config: agent_config.clone(),
        provider_family: None,
        model: Arc::new(ProviderCallCountingModel(provider_calls.clone())),
        preamble: prompt.preamble().to_owned(),
        loop_tools: Arc::new(vec![Box::new(PreInferenceBlockingTool {
            entered: entered.clone(),
        }) as Box<dyn crate::llm::tool::ToolDyn>]),
        prompt_builder: prompt,
        hook_failure_policy: FailurePolicy::default(),
        rendered_request_capture_factory: None,
        background_tool_registry: BackgroundToolRegistry::default(),
        background_execution_registry: BackgroundExecutionRegistry::default(),
        startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
        runtime_status: crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            node_did.clone(),
        ),
        slot_generation: 1,
        request_admission: crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    })
    .unwrap();
    let request = create_routed_request(&node, &agent_config, &node_did).await;
    let request_doc_id = request.doc_id.clone();
    let requester_did = request.requester_did.clone();
    seed_titled_session(&node, &request, &agent_config)
        .await
        .expect("seed a titled session through the session writer");

    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let capture = InterruptionCapture::default();
    let process = daemon
        .process_request(request, shutdown_rx)
        .with_subscriber(capture.subscriber());
    tokio::pin!(process);

    let settled_in = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::select! {
            _ = entered.notified() => {},
            _ = &mut process => panic!("daemon exited before reaching the pre-inference window"),
        }
        let row = request_terminal_row(node.as_ref(), &request_doc_id)
            .await
            .unwrap();
        assert_eq!(row["lifecycle_state"], "processing");

        crate::interrupt::interrupt_request_by_doc_id(
            &node,
            &request_doc_id,
            &node_did,
            requester_did.as_deref(),
        )
        .await
        .expect("latch interrupt");
        let latched_at = std::time::Instant::now();

        (&mut process).await.expect("request owner completes");
        latched_at.elapsed()
    })
    .await
    .expect("owner must terminalize the interrupted claim without waiting for recovery");

    assert!(
        settled_in < Duration::from_secs(10),
        "owner terminalization took {settled_in:?}, which no longer excludes the \
         recovery path waiting out the {}s default execution lease",
        crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
    );

    let row = request_terminal_row(node.as_ref(), &request_doc_id)
        .await
        .expect("read the terminalized request row");
    assert_eq!(row["lifecycle_state"], "interrupted");
    assert_eq!(row["stopped_before_running"], false);
    assert!(
        !row["terminalized_at"].is_null(),
        "owner terminalization must stamp terminalized_at: {row}"
    );
    assert_eq!(
        row["failure_reason"], "interrupted",
        "interruption uses the ordinary owner terminal reason: {row}"
    );
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        0,
        "no provider call may run in the pre-inference window"
    );
    capture.assert_source("preparation");
    node.shutdown().await;
}

#[tokio::test]
async fn interrupt_latched_at_the_claim_terminalizes_before_execution_begins() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let agent_config = pre_inference_agent();
    let node_did = agent_config.node_did().to_owned();
    let identity = agent_config.node_identity().clone();
    let prompt = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let mut daemon = AgentDaemon::new(AgentDaemonInit {
        node: node.clone(),
        agent_config: agent_config.clone(),
        provider_family: None,
        model: Arc::new(ProviderCallCountingModel(provider_calls.clone())),
        preamble: prompt.preamble().to_owned(),
        loop_tools: Arc::new(Vec::<Box<dyn crate::llm::tool::ToolDyn>>::new()),
        prompt_builder: prompt,
        hook_failure_policy: FailurePolicy::default(),
        rendered_request_capture_factory: None,
        background_tool_registry: BackgroundToolRegistry::default(),
        background_execution_registry: BackgroundExecutionRegistry::default(),
        startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
        runtime_status: crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            node_did.clone(),
        ),
        slot_generation: 1,
        request_admission: crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    })
    .unwrap();
    let request = create_routed_request(&node, &agent_config, &node_did).await;
    let request_doc_id = request.doc_id.clone();
    let requester_did = request.requester_did.clone();
    seed_titled_session(&node, &request, &agent_config)
        .await
        .expect("seed a titled session through the session writer");

    let stream_writer = crate::streaming::DefraStreamWriter::new(
        node.clone(),
        agent_config.node_did(),
        Duration::from_millis(agent_config.stream_batch_ms),
    );
    let origin =
        crate::lifecycle::ExecutionOrigin::from_persisted(request.execution_origin.as_deref())
            .expect("a created request carries a canonical execution origin");
    let mut lifecycle = crate::lifecycle::RequestLifecycle::new_with_execution_binding(
        node.clone(),
        &agent_config.agent_id,
        agent_config.node_did(),
        request.clone(),
        agent_config.deadline_duration.as_secs(),
        origin,
        agent_config.backend_id.clone().unwrap_or_default(),
    );
    lifecycle.set_execution_lease_duration(agent_config.stream_liveness_timeout);
    lifecycle.set_configured_max_total_tokens(agent_config.max_total_tokens);
    assert_eq!(
        lifecycle
            .claim_with_identity()
            .await
            .expect("claim the fresh request"),
        crate::lifecycle::ClaimOutcome::Claimed,
        "this runtime must win the claim to own the pre-execution window"
    );
    let claimed = request_terminal_row(node.as_ref(), &request_doc_id)
        .await
        .expect("read the claimed request row");
    assert_eq!(
        claimed["lifecycle_state"], "claimed",
        "the window under test runs before execution begins: {claimed}"
    );

    let (interrupt_tx, mut interrupt_rx) =
        tokio::sync::watch::channel::<Option<crate::interrupt::InterruptIntent>>(None);
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let _observer = tokio_util::task::AbortOnDropHandle::new(
        crate::interrupt::spawn_request_interrupt_observer(
            node.clone(),
            request_doc_id.clone(),
            interrupt_tx,
            shutdown_rx.clone(),
        ),
    );
    crate::interrupt::interrupt_request_by_doc_id(
        &node,
        &request_doc_id,
        &node_did,
        requester_did.as_deref(),
    )
    .await
    .expect("latch interrupt");
    tokio::time::timeout(Duration::from_secs(20), async {
        interrupt_rx
            .wait_for(|intent| intent.is_some())
            .await
            .map(|_| ())
    })
    .await
    .expect("the claim's observer must surface the durable latch")
    .expect("the observer channel stays open while the claim is held");

    let latched = request_terminal_row(node.as_ref(), &request_doc_id)
        .await
        .expect("read the latched request row");
    assert!(
        !latched["interrupt_requested_at"].is_null(),
        "the window decides on the durable latch: {latched}"
    );

    let outcome = daemon
        .observe_owned_work(
            &mut lifecycle,
            &stream_writer,
            &request,
            shutdown_rx,
            interrupt_rx,
        )
        .await;
    assert_eq!(
        outcome.observation(),
        crate::task_hooks::TaskAgentResult::Cancelled
    );
    let crate::agent::daemon::OwnedWorkOutcome::Interrupted(
        crate::agent::daemon::RequestInterruptionSource::Claimed,
    ) = outcome
    else {
        panic!("an observable latch must interrupt the claimed request");
    };

    let undecided = request_terminal_row(node.as_ref(), &request_doc_id)
        .await
        .expect("read the pre-terminal request row");
    assert_eq!(
        undecided["lifecycle_state"], "claimed",
        "the admission window must decide before execution begins: {undecided}"
    );

    let capture = InterruptionCapture::default();
    daemon
        .finalize_interrupt_before_work(&mut lifecycle, &stream_writer, &request)
        .with_subscriber(capture.subscriber())
        .await
        .expect("owner terminalizes the request");
    capture.assert_source("claimed");

    let terminal = request_terminal_row(node.as_ref(), &request_doc_id)
        .await
        .expect("read the terminalized request row");
    assert_eq!(terminal["stopped_before_running"], true);
    assert_eq!(
        terminal["lifecycle_state"], "interrupted",
        "the owner terminalizes the claimed request rather than leaving it for recovery: {terminal}"
    );
    assert!(
        !terminal["terminalized_at"].is_null(),
        "owner terminalization must stamp terminalized_at: {terminal}"
    );
    assert_eq!(
        terminal["failure_reason"], "interrupted",
        "interruption uses the ordinary owner terminal reason: {terminal}"
    );
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        0,
        "no provider call may run before execution begins"
    );
    node.shutdown().await;
}

#[tokio::test]
async fn interrupt_while_restoring_the_provider_checkpoint_terminalizes_through_the_owner() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let agent_config = pre_inference_agent();
    let node_did = agent_config.node_did().to_owned();
    let identity = agent_config.node_identity().clone();
    let prompt = LayeredPromptBuilder::for_agent(
        &agent_config.system_prompt,
        &agent_config.agent_id,
        &[],
        false,
        &[],
    );
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let mut daemon = AgentDaemon::new(AgentDaemonInit {
        node: node.clone(),
        agent_config: agent_config.clone(),
        provider_family: None,
        model: Arc::new(ProviderCallCountingModel(provider_calls.clone())),
        preamble: prompt.preamble().to_owned(),
        loop_tools: Arc::new(Vec::<Box<dyn crate::llm::tool::ToolDyn>>::new()),
        prompt_builder: prompt,
        hook_failure_policy: FailurePolicy::default(),
        rendered_request_capture_factory: None,
        background_tool_registry: BackgroundToolRegistry::default(),
        background_execution_registry: BackgroundExecutionRegistry::default(),
        startup_barrier: Arc::new(StartupBarrier::ready_for_test()),
        runtime_status: crate::runtime_status::RuntimeStatusHandle::new(
            node.clone(),
            node_did.clone(),
        ),
        slot_generation: 1,
        request_admission: crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    })
    .unwrap();
    let request = create_routed_request(&node, &agent_config, &node_did).await;
    let request_doc_id = request.doc_id.clone();
    let requester_did = request.requester_did.clone();
    seed_titled_session(&node, &request, &agent_config)
        .await
        .expect("seed a titled session through the session writer");

    let reached = Arc::new(tokio::sync::Notify::new());
    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let capture = InterruptionCapture::default();
    let process = crate::config_client::ConfigApplyTxn::with_read_pause(
        "reduction_index: ASC",
        reached.clone(),
        daemon.process_request(request, shutdown_rx),
    )
    .with_subscriber(capture.subscriber());
    tokio::pin!(process);

    let settled_in = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::select! {
            _ = reached.notified() => {},
            _ = &mut process => panic!("daemon exited before restoring its provider checkpoint"),
        }
        let row = request_terminal_row(node.as_ref(), &request_doc_id)
            .await
            .unwrap();
        assert_eq!(row["lifecycle_state"], "processing");

        crate::interrupt::interrupt_request_by_doc_id(
            &node,
            &request_doc_id,
            &node_did,
            requester_did.as_deref(),
        )
        .await
        .expect("latch interrupt");
        let latched_at = std::time::Instant::now();

        (&mut process).await.expect("request owner completes");
        latched_at.elapsed()
    })
    .await
    .expect("owner must terminalize without waiting for the parked checkpoint read");
    assert!(
        settled_in < Duration::from_secs(10),
        "owner terminalization took {settled_in:?}"
    );

    let row = request_terminal_row(node.as_ref(), &request_doc_id)
        .await
        .expect("read the terminalized request row");
    assert_eq!(row["lifecycle_state"], "interrupted", "{row}");
    assert_eq!(row["failure_reason"], "interrupted", "{row}");
    assert_eq!(row["stopped_before_running"], false, "{row}");
    assert!(!row["terminalized_at"].is_null(), "{row}");
    assert_eq!(provider_calls.load(Ordering::SeqCst), 0);
    capture.assert_source("mid_flight");
    node.shutdown().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_while_hashing_a_sealed_workspace_kills_git_and_keeps_its_index() {
    use crate::managed_exec::ownership::{ProcessIdentity, ProcessObservation};

    fn git(dir: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    let repo = tempfile::tempdir().unwrap();
    let probe = tempfile::tempdir().unwrap();
    let dir = repo.path();
    git(dir, &["init", "-q"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "test"]);
    std::fs::write(dir.join("tracked.txt"), "sealed\n").unwrap();
    git(dir, &["add", "tracked.txt"]);
    git(dir, &["commit", "-q", "-m", "base"]);

    let marker = probe.path().join("filter.pid");
    let filter = probe.path().join("park-filter");
    std::fs::write(
        &filter,
        format!(
            "#!/bin/sh\necho $$ > '{}.tmp' && mv '{0}.tmp' '{0}'\nexec sleep 30\n",
            marker.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&filter, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(
        dir,
        &["config", "filter.park.clean", filter.to_str().unwrap()],
    );
    std::fs::write(dir.join(".git/info/attributes"), "* filter=park\n").unwrap();
    let index_before = std::fs::read(dir.join(".git/index")).unwrap();

    let (interrupt_tx, mut interrupt_rx) =
        tokio::sync::watch::channel::<Option<crate::interrupt::InterruptIntent>>(None);
    let dest = dir.to_path_buf();
    let hashing = tokio::spawn(async move {
        super::super::request::prepare_unless_interrupted(
            &mut interrupt_rx,
            crate::workspace::adapter::working_tree_hash(&dest),
        )
        .await
        .map(|hashed| hashed.map_err(|error| ErrorChain(&error).to_string()))
    });

    let filter_pid = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(pid) = std::fs::read_to_string(&marker)
                .ok()
                .and_then(|pid| pid.trim().parse::<i32>().ok())
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("git add must reach the parking clean filter");

    let filter = ProcessIdentity::of_live(filter_pid).expect("the parked clean filter is running");
    interrupt_tx
        .send(Some(crate::interrupt::InterruptIntent {
            at: chrono::Utc::now(),
        }))
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), hashing)
        .await
        .expect("an interrupt during hashing must not wait for Git")
        .expect("hashing task joins");
    assert!(
        outcome.is_none(),
        "the interrupt wins over hashing: {outcome:?}"
    );

    assert_ne!(
        filter.await_leader_exit(Duration::from_secs(10)).await,
        ProcessObservation::Running,
        "abandoned hashing must kill Git's process group"
    );
    assert_eq!(
        std::fs::read(dir.join(".git/index")).unwrap(),
        index_before,
        "hashing stages into a temporary index only"
    );
    assert!(!dir.join(".git/index.lock").exists());
}
