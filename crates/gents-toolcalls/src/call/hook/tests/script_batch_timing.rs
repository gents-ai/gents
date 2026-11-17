use super::*;
use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDyn, ToolError, ToolFootprint};
use gents_loop::SessionHook;
use serde_json::Value;
use std::time::{Duration, Instant};

struct SleepingRead;
impl ToolDyn for SleepingRead {
    fn name(&self) -> String {
        "query".into()
    }
    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "query".into(),
                description: "A read-only one-second timing probe.".into(),
                parameters: json!({"type":"object"}),
            }
        })
    }
    fn footprint(&self, _: &str) -> ToolFootprint {
        ToolFootprint::Read
    }
    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let input: Value = serde_json::from_str(&args).unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
            Ok(input.to_string())
        })
    }
}

#[tokio::test]
#[ignore = "timing: set GENTS_SCRIPT_WASM to the compiled script WASM; inspect script effect phase tracing"]
async fn scratch_persisted_script_batch_timing() {
    use sha2::Digest;
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::INFO)
        .try_init();
    let home = tempfile::tempdir().unwrap();
    let db = gents_inference::test_support::test_db(
        "script-batch-phase-timing",
        gents_session::test_support::no_lifecycle_plugins(),
    )
    .await;
    let hook = signed_hook(&db.node, FailurePolicy::FailClosed).await;
    hook.on_completion_call(&user_text_message("time four reads"), &[])
        .await;
    let session = hook.session_id().await.unwrap();
    bind_interruptible_request(
        &hook,
        "script-timing-request",
        &session,
        Utc::now() + chrono::Duration::minutes(5),
    )
    .await;
    let deadline = {
        let fixtures = hook_execution_fixtures().lock().await;
        fixtures[&hook_execution_fixture_key(&hook, "script-timing-request")]
            .lifecycle
            .claimed_deadline_at()
            .unwrap()
    };
    hook.set_request_deadline_at(Some(deadline)).await;
    let hook = Arc::new(hook);
    let wasm =
        std::fs::read(std::env::var_os("GENTS_SCRIPT_WASM").expect("GENTS_SCRIPT_WASM")).unwrap();
    let bytes = gents_pack::plugin::test_fixtures::build_plugin_afb_bytes(&wasm);
    let (_, mut record) = gents_pack::plugin::test_fixtures::installed_echo();
    let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
    gents_pack::plugin::store::store_bytes(home.path(), &digest, &bytes).unwrap();
    record.digest = format!("sha256:{digest}");
    record.declaration.limits = Some(gents_pack::pack::PluginLimits {
        wall_clock_secs: Some(60),
        ..Default::default()
    });
    record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
    gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
    let executor = crate::plugin::executor::PluginExecutor::new(Some(home.path().to_owned()));
    for (label, script) in [
        (
            "serial",
            "const out=[]; for(let id=1;id<=4;id++) out.push(await tools.query({id})); return out;",
        ),
        (
            "parallel",
            "return await Promise.all([1,2,3,4].map(id => tools.query({id})));",
        ),
    ] {
        // Distinct parents keep effect ordinals unique between samples.
        let parent = format!("script-{label}");
        accept_hook_tool_call(&hook, &parent, "plugin", "{}", None).await;
        assert!(matches!(
            hook.on_tool_call("plugin", None, &parent, "{}").await,
            ToolCallHookAction::Continue
        ));
        let effects = gents_loop::loop_stream::tool_effect_dispatcher(
            Arc::new(vec![Box::new(SleepingRead)]),
            hook.clone(),
            parent.clone(),
        );
        let start = Instant::now();
        let result = gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution(
            Some(deadline),
            Default::default(),
            gents_loop::tool_effects::scope(
                Some(effects),
                executor.call_data_bound_for(&record, json!({"script":script}), None, true, None),
            ),
        )
        .await
        .unwrap();
        tracing::info!(sample = label, total_us = start.elapsed().as_micros() as u64, home = %home.path().display(), "scratch script total; reserve/before/body/result phases logged by production dispatcher");
        assert_eq!(
            result.outcome.verdict,
            gents_pack::plugin::PluginVerdict::Success,
            "{}",
            result.outcome.diagnostics
        );
        assert_eq!(
            result.outcome.output,
            json!([{"id":1},{"id":2},{"id":3},{"id":4}])
        );
    }
    db.node.shutdown().await.unwrap();
}
