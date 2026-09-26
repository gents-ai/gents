// Included in inference.rs's test module to reuse its real daemon harness.

/// Parks the request inside provider-input estimation. `build_request` awaits
/// every tool definition, so this future holds the daemon in the window between
/// the claim and the first provider call for as long as a test needs.
#[derive(Clone)]
struct PreInferenceBlockingTool {
    entered: Arc<std::sync::atomic::AtomicBool>,
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
            self.entered.store(true, Ordering::SeqCst);
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

fn pre_inference_behavior() -> Arc<ResolvedBehavior> {
    let behavior = test_behavior();
    let mut behavior = ResolvedBehavior::clone(behavior.as_ref());
    // The owner may only finalize under a live lease. A lease long enough to
    // outlast the whole test keeps expiry out of the assertion: terminalization
    // here is the owner's, never a lapsed generation's.
    behavior.stream_liveness_timeout = Duration::from_secs(600);
    behavior.deadline_duration = Duration::from_secs(1_200);
    Arc::new(behavior)
}

async fn seed_titled_session(
    node: &defra_node::EmbeddedNode,
    request: &AgentRequest,
    behavior: &ResolvedBehavior,
) {
    // A session that already has a title keeps generated-title work off the
    // provider, so a zero provider-call count is a fact about this test rather
    // than a race with the title task.
    let session = gents_protocol::session::AgentSession {
        session_id: request.session_id.clone(),
        agent_did: request.agent_did.clone(),
        requester_did: request.requester_did.clone(),
        behavior_id: behavior.behavior_id.clone(),
        created_at: request.created_at.clone(),
        closed_at: None,
        title: Some(gents_protocol::session::SessionTitle {
            text: "pre-inference interrupt".into(),
            source: gents_protocol::session::SessionTitleSource::Task,
        }),
        tags: vec![],
        provenance: None,
        observation: None,
    };
    let input =
        gents_protocol::graphql::graphql_input_literal(&serde_json::to_value(session).unwrap())
            .unwrap();
    let seeded = node
        .execute(&format!(
            "mutation {{ create_AgentSession(input: {input}) {{_docID}} }}"
        ))
        .await;
    assert!(!seeded.has_errors(), "{:?}", seeded.errors);
}

async fn request_terminal_row(
    node: &defra_node::EmbeddedNode,
    request_doc_id: &str,
) -> serde_json::Value {
    let doc_id = crate::graphql::escape_graphql_string(request_doc_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{doc_id}" }} }}, limit: 1) {{
                lifecycle_state failure_reason terminalized_at interrupt_requested_at
            }} }}"#
        ))
        .await;
    response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|rows| rows.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("request row")
}

/// An interrupt latched while a claimed request is still preparing its provider
/// input must be terminalized by the owned completion loop, not left for the
/// recovery sweep.
///
/// Nothing in this test can terminalize the request except the owner: no
/// recovery sweep runs in-process, the lease outlives the assertion window, and
/// the sweep's own terminal write stamps `execution lease expired` instead of
/// the reason asserted here.
#[tokio::test]
async fn interrupt_while_preparing_provider_input_terminalizes_through_the_owner() {
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let behavior = pre_inference_behavior();
    let agent_did = behavior.agent_did().to_owned();
    let identity = behavior.principal_identity().clone();
    let prompt = LayeredPromptBuilder::for_behavior(
        &behavior.system_prompt,
        &behavior.behavior_id,
        &[],
        false,
        &[],
    );
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let mut daemon = BehaviorDaemon::new(
        node.clone(),
        behavior.clone(),
        Arc::new(ProviderCallCountingModel(provider_calls.clone())),
        prompt.preamble().to_owned(),
        Arc::new(vec![Box::new(PreInferenceBlockingTool {
            entered: entered.clone(),
        }) as Box<dyn crate::llm::tool::ToolDyn>]),
        prompt,
        FailurePolicy::default(),
        None,
        BackgroundToolRegistry::default(),
        BackgroundExecutionRegistry::default(),
        Arc::new(StartupBarrier::ready_for_test()),
        crate::runtime_status::RuntimeStatusHandle::new(node.clone(), agent_did.clone()),
        1,
        crate::request_admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            identity,
            crate::agent::p2p_reconcile::enrollment_authority_channel().1,
        ),
    )
    .unwrap();
    let request = create_routed_request(&node, &behavior, &agent_did).await;
    let request_doc_id = request.doc_id.clone();
    let requester_did = request.requester_did.clone();
    seed_titled_session(node.as_ref(), &request, &behavior).await;

    let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let process = daemon.process_request(request, shutdown_rx);
    tokio::pin!(process);

    let settled_in = tokio::time::timeout(Duration::from_secs(20), async {
        let reach_pre_inference_window = async {
            loop {
                let row = request_terminal_row(node.as_ref(), &request_doc_id).await;
                if entered.load(Ordering::SeqCst) && row["lifecycle_state"] == "claimed" {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::pin!(reach_pre_inference_window);
        tokio::select! {
            _ = &mut reach_pre_inference_window => {}
            _ = &mut process => panic!("daemon exited before reaching the pre-inference window"),
        }

        crate::interrupt::interrupt_request_by_doc_id(
            node.as_ref(),
            &request_doc_id,
            &agent_did,
            requester_did.as_deref(),
        )
        .await
        .expect("latch interrupt");
        let latched_at = std::time::Instant::now();

        (&mut process).await;
        latched_at.elapsed()
    })
    .await
    .expect("owner must terminalize the interrupted claim without waiting for recovery");

    // Recovery cannot terminalize a claim before its execution lease expires, so
    // a settle time far under the default lease is the owner's write and nothing
    // else's — the sweep would also stamp a different failure_reason.
    assert!(
        settled_in < Duration::from_secs(10),
        "owner terminalization took {settled_in:?}, which no longer excludes the \
         recovery path waiting out the {}s default execution lease",
        crate::config::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
    );

    let row = request_terminal_row(node.as_ref(), &request_doc_id).await;
    assert_eq!(row["lifecycle_state"], "interrupted");
    assert!(
        !row["terminalized_at"].is_null(),
        "owner terminalization must stamp terminalized_at: {row}"
    );
    assert_eq!(
        row["failure_reason"],
        gents_protocol::request_lifecycle::interrupt_terminal_reason::BEFORE_ANY_PROVIDER_CALL,
        "runtime must record that the interrupt caught the request before any provider call: {row}"
    );
    assert_eq!(
        provider_calls.load(Ordering::SeqCst),
        0,
        "no provider call may run in the pre-inference window"
    );
}
