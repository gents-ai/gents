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
    gents_toolcalls::call::lifecycle::ToolCallLifecycle,
    gents_loop::live_output::LiveToolOutputRegistry,
    gents_loop::live_output::LiveToolOutputWriter,
) {
    let (node, _path, tool) =
        gents_toolcalls::call::lifecycle::admission_fixture::published_spawn_parent(name).await;
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
    tool: &gents_toolcalls::call::lifecycle::ToolCallLifecycle,
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
    tool: &gents_toolcalls::call::lifecycle::ToolCallLifecycle,
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

#[tokio::test]
async fn request_deadline_presents_output_committed_before_the_cut() {
    let (node, mut tool, _registry, writer) = committed_run("cli-request-deadline").await;
    let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(CliTool::new(config(30)))];
    let args = serde_json::json!({"argv": ["-c",
        "printf 'partial stdout'; printf 'partial stderr' >&2; sleep 30"]})
    .to_string();
    let started = std::time::Instant::now();
    let outcome =
        gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_session(
            Some(chrono::Utc::now() + chrono::Duration::seconds(2)),
            tokio_util::sync::CancellationToken::new(),
            None,
            None,
            None,
            gents_loop::loop_stream::dispatch_tool(&tools, "sh", args, Some(writer), None),
        )
        .await;
    assert!(
        matches!(
            outcome,
            gents_loop::tool_call_lifecycle::ToolOutcome::TimedOut { .. }
        ),
        "{outcome:?}"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(20));

    let raw = committed_output(&node, &tool).await;
    assert!(
        raw.contains("partial stdout") && raw.contains("partial stderr"),
        "captured output must be committed before the deadline: {raw:?}"
    );
    assert!(tool.timeout().await.unwrap());
    let presented = presented(&node, &tool).await;
    assert!(presented.starts_with(&raw), "{presented:?}");
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
            1,
            "printf '%17000s' ' '; printf 'last out'; printf 'last err' >&2; sleep 30",
        ),
    ] {
        let (node, mut tool, registry, writer) = committed_run(name).await;
        let cli = CliTool::new(config(timeout_secs));
        let args = serde_json::json!({"argv": ["-c", script]}).to_string();
        let outcome = ToolOutcome::from_dispatch(
            "sh",
            gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_session(
                None,
                tokio_util::sync::CancellationToken::new(),
                None,
                Some(writer),
                None,
                cli.call(args),
            )
            .await,
        );
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
                assert!(text.starts_with("timed out after 1s\n"), "{text}");
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
