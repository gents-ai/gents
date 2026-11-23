use super::*;
use gents_loop::tool::{BoxFuture, ToolDefinition, ToolError, ToolFootprint};
use std::sync::atomic::{AtomicUsize, Ordering};

struct SuccessfulProbe {
    name: &'static str,
    calls: Arc<AtomicUsize>,
}

impl ToolDyn for SuccessfulProbe {
    fn hidden(&self) -> bool {
        false
    }
    fn name(&self) -> String {
        self.name.into()
    }
    fn emits_command_envelope(&self) -> bool {
        true
    }
    fn footprint(
        &self,
        _args: &str,
        _context: &gents_loop::tool::ToolCallEnvelope,
    ) -> ToolFootprint {
        ToolFootprint::Read
    }
    fn definition<'a>(
        &'a self,
        _prompt: String,
    ) -> BoxFuture<'a, gents_loop::tool::ToolDefinition> {
        let name = self.name.to_owned();
        Box::pin(async move {
            ToolDefinition {
                name,
                description: "Successful repetition fixture".into(),
                parameters: serde_json::json!({"type":"object","properties":{}}),
            }
        })
    }
    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a gents_loop::tool::ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(format!("gents_exec: {{\"ok\":true,\"duration_ms\":{call},\"hint\":\"poll {call}\"}}\nstdout:\nunchanged\nstderr:\n(empty)"))
        })
    }
}

fn call(index: usize, name: &str) -> RawStreamingChoice<()> {
    RawStreamingChoice::ToolCall(RawStreamingToolCall::new(
        format!("success-{index}"),
        name.into(),
        serde_json::json!({}),
    ))
}

#[tokio::test]
async fn repeated_success_stops_after_persisting_the_eighth_result() {
    check(false).await;
}

#[tokio::test]
async fn repeated_success_does_not_dispatch_a_later_read_in_the_same_batch() {
    check(true).await;
}

async fn check(batched_tail: bool) {
    let (db, hook, writer, mut lifecycle) = owned_test_hook().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut turns = (0..7)
        .map(|index| {
            vec![
                call(index, "success_probe"),
                RawStreamingChoice::FinalResponse(()),
            ]
        })
        .collect::<Vec<_>>();
    let mut last = vec![call(7, "success_probe")];
    if batched_tail {
        last.push(call(8, "other_probe"));
    }
    last.push(RawStreamingChoice::FinalResponse(()));
    turns.push(last);
    turns.push(vec![
        call(9, "success_probe"),
        RawStreamingChoice::FinalResponse(()),
    ]);
    let model = scripted(turns);
    let tools = Arc::new(
        ["success_probe", "other_probe"]
            .map(|name| {
                Box::new(SuccessfulProbe {
                    name,
                    calls: Arc::clone(&calls),
                }) as Box<dyn ToolDyn>
            })
            .into_iter()
            .collect(),
    );
    let stream = run_loop_stream(
        model.clone(),
        Some(hook.clone()),
        TaggedMessage::unassociated(Message::user("observe the tool")),
        Vec::new(),
        tools,
        owned_config_for(32, &lifecycle),
    );
    let collected = collect_owned_scripted_stream(
        stream,
        &hook,
        &writer,
        &mut lifecycle,
        gents_loop::provider_input::ProviderInputProfile::OpenAiChatCompletions,
    )
    .await;
    let error = collected
        .error
        .as_deref()
        .expect("the repetition guard stops execution");
    assert!(error.contains(REPEATED_TOOL_SUCCESS_PREFIX), "{error}");
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    assert_eq!(model.requests().len(), 8);
    assert_eq!(collected.tool_results.len(), 8);
    assert!(collected.tool_results.iter().all(
        |result| result.contains("unchanged") && !result.contains(REPEATED_TOOL_SUCCESS_PREFIX)
    ));
    lifecycle
        .terminalize_owned(
            gents_session::request::lifecycle::RequestTerminalOutcome::Failed,
            writer.terminal_output(&lifecycle.request().doc_id).await,
            Some(error),
        )
        .await
        .unwrap();
    let rows = tool_call_rows(&lifecycle.access().unwrap()).await;
    for index in 0..8 {
        let row = rows
            .iter()
            .find(|row| row["tool_call_id"] == format!("success-{index}"))
            .expect("each successful result stays durable");
        assert_eq!(row["lifecycle_state"], "completed", "{row}");
        assert!(row["tool_failure_class"].is_null(), "{row}");
    }
    assert!(!rows.iter().any(|row| row["tool_call_id"] == "success-9"));
    db.node.shutdown().await.unwrap();
}
