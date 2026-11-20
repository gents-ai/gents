use gents_loop::tool::ToolDyn;
use gents_tools::native::cli_tool::*;
use gents_tools::native::CliToolConfig;
use rapidhash::RapidHashMap;

fn config(timeout_secs: u64) -> CliToolConfig {
    CliToolConfig {
        name: "sh".into(),
        binary_path: "/bin/sh".into(),
        description: String::new(),
        allowed_argv_prefixes: vec![],
        env_vars: RapidHashMap::from_iter([("GENTS_T".to_string(), "1".to_string())]),
        working_dir: None,
        timeout_secs,
        max_output_chars: gents_tools::native::DEFAULT_MAX_COMMAND_CHARS,
    }
}

async fn committed_run(
    name: &str,
) -> (
    std::sync::Arc<defra_node::Node>,
    crate::call::lifecycle::ToolCallLifecycle,
    gents_loop::live_output::LiveToolOutputRegistry,
    gents_loop::live_output::LiveToolOutputWriter,
) {
    let (node, _path, tool) =
        crate::call::lifecycle::admission_fixture::published_spawn_parent(name).await;
    let binding = tool
        .tool_output_binding()
        .expect("running tool output binding");
    let registry = gents_loop::live_output::LiveToolOutputRegistry::default();
    let writer = registry
        .canonical_writer_for(
            tool.doc_id().unwrap().to_owned(),
            std::sync::Arc::new(binding),
        )
        .await;
    (node, tool, registry, writer)
}

async fn presented(
    _node: &std::sync::Arc<defra_node::Node>,
    tool: &crate::call::lifecycle::ToolCallLifecycle,
) -> String {
    gents_session::tool_call::query::result::load_tool_call_presentation(
        &tool.access().unwrap(),
        tool.doc_id().unwrap(),
        tool.node_did(),
        tool.session_id(),
        tool.requester_did(),
    )
    .await
    .unwrap()
    .result
    .expect("terminal delivery presentation")
}

async fn committed_output(
    _node: &std::sync::Arc<defra_node::Node>,
    tool: &crate::call::lifecycle::ToolCallLifecycle,
) -> String {
    gents_session::session::tool_output_rows::observe_canonical_tool_output_with_access(
        &tool.access().unwrap(),
        tool.doc_id().unwrap(),
        tool.request_doc_id().unwrap(),
        tool.session_id(),
        tool.node_did(),
        tool.requester_did(),
    )
    .await
    .unwrap()
    .into_text()
}

/// The deadline is the property here, so the test drives the cut itself
/// (TLA+ `WallClockBound` `"driven"`): it waits until the child's output is
/// committed, then advances the runtime's paused clock past the request
/// deadline, which the real clock never reaches. A wall-clock deadline instead
/// raced the child's writes and cut a loaded host's child before it printed.
#[tokio::test]
async fn request_deadline_presents_output_committed_before_the_cut() {
    let (node, mut tool, _registry, writer) = committed_run("cli-request-deadline").await;
    let unreached = gents_store::test_support::UNREACHED_DEADLINE;
    // The tool's own timeout outlives the advance, so only the request
    // deadline is due when the clock moves.
    let tools: Vec<Box<dyn ToolDyn>> =
        vec![Box::new(CliTool::new(config(2 * unreached.as_secs())))];
    let args = serde_json::json!({"argv": ["-c",
        "printf 'partial stdout'; printf 'partial stderr' >&2; sleep 30"]})
    .to_string();
    let envelope = gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry::new(
        Some(gents_store::test_support::unreached_deadline_at()),
        tokio_util::sync::CancellationToken::new(),
    )
    .call();
    let dispatch =
        gents_loop::loop_stream::dispatch_tool(&tools, "sh", args, Some(writer), None, &envelope);
    tokio::pin!(dispatch);
    let raw = tokio::select! {
        outcome = &mut dispatch => panic!("the child ended before the cut: {outcome:?}"),
        raw = gents_store::test_support::read_when_changed(
            &node,
            &["AgentOutputSegment"],
            "the child's output is committed before the cut",
            async || {
                let raw = committed_output(&node, &tool).await;
                (raw.contains("partial stdout") && raw.contains("partial stderr")).then_some(raw)
            },
        ) => raw,
    };
    tokio::time::pause();
    tokio::time::advance(unreached + std::time::Duration::from_secs(1)).await;
    tokio::time::resume();
    let outcome = dispatch.await;
    assert!(
        matches!(
            outcome,
            gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { .. }
        ),
        "{outcome:?}"
    );
    let committed = committed_output(&node, &tool).await;
    assert!(
        committed.starts_with(&raw),
        "the cut keeps the output committed before it: {committed:?}"
    );
    assert!(tool.timeout().await.unwrap());
    let presented = presented(&node, &tool).await;
    assert!(presented.starts_with(&committed), "{presented:?}");
    assert!(
        presented.contains("\ntool call deadline exceeded at "),
        "{presented:?}"
    );
}

#[tokio::test]
async fn committed_output_reconstructs_completed_and_stopped_results() {
    use gents_loop::tool_call_lifecycle::ToolOutcome;

    for (name, timeout_secs, script) in [
        (
            "cli-completed",
            30,
            "printf 'out \"q\"\\n'; printf 'err\\t' >&2; exit 4",
        ),
        (
            "cli-invalid-utf8",
            30,
            "printf 'ok\\303'; printf '\\377\\303\\251' >&2; exit 4",
        ),
        (
            "cli-crlf-head",
            30,
            "i=0; while [ $i -lt 3000 ]; do printf 'line %d\\r\\n' $i; i=$((i+1)); done; exit 4",
        ),
        (
            "cli-stopped",
            // Past any run: the owning request's cancellation stops the child.
            gents_store::test_support::UNREACHED_DEADLINE.as_secs(),
            "printf '%17000s' ' '; printf 'last out'; printf 'last err' >&2; sleep 30",
        ),
    ] {
        let (node, mut tool, registry, writer) = committed_run(name).await;
        let cli = CliTool::new(config(timeout_secs));
        let args = serde_json::json!({"argv": ["-c", script]}).to_string();
        let stop = tokio_util::sync::CancellationToken::new();
        let envelope = gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry {
            live_output: Some(writer),
            ..gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry::new(None, stop.clone())
        }
        .call();
        let call = cli.call(args, &envelope);
        let dispatched = if name == "cli-stopped" {
            // The stop is driven, not raced (TLA+ `WallClockBound`
            // `"driven"`): the owning request cancels once the child's last
            // writes are committed. A 1 s CLI timeout raced those writes; a
            // paused clock cannot drive that timeout, because the stop's own
            // reap and drain timers would then fire at once.
            tokio::pin!(call);
            tokio::select! {
                result = &mut call => panic!("{name}: the child ended before its stop: {result:?}"),
                () = gents_store::test_support::read_when_changed(
                    &node,
                    &["AgentOutputSegment"],
                    "the stopped child's output is committed",
                    async || {
                        let raw = committed_output(&node, &tool).await;
                        (raw.contains("last out") && raw.contains("last err")).then_some(())
                    },
                ) => {}
            }
            stop.cancel();
            call.await
        } else {
            call.await
        };
        let outcome = ToolOutcome::from_dispatch("sh", dispatched);
        let text = outcome.model_facing_text().to_owned();
        let presentation = registry
            .take_prepared_presentation(tool.doc_id().unwrap(), &text)
            .await
            .unwrap()
            .expect("CLI tool prepares a presentation over committed output");
        match outcome {
            ToolOutcome::Completed(_) => {
                assert!(text.contains("exit_code: 4"), "{text}");
                if name == "cli-crlf-head" {
                    assert!(text.contains(r"\r\n\n[Showing lines 1-"), "CRLF head kept");
                }
                tool.complete_with_presentation(&text, Some(presentation))
                    .await
                    .unwrap();
            }
            ToolOutcome::Failed { class, .. } => {
                assert!(
                    text.starts_with("command cancelled by the owning request\n"),
                    "{text}"
                );
                assert!(
                    text.contains("[Showing last 16000 of 17008 bytes]"),
                    "{text}"
                );
                tool.fail_with_presentation(&text, class, Some(presentation))
                    .await
                    .unwrap();
            }
            other => panic!("{name}: unexpected outcome {other:?}"),
        }
        assert_eq!(presented(&node, &tool).await, text, "{name}");
    }
}
