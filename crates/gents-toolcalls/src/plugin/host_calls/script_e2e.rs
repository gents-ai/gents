use std::sync::Arc;

use gents_loop::tool::{BoxFuture, Tool, ToolDefinition};
use gents_loop::tool_call_lifecycle::ToolOutcome;
use gents_loop::tool_effects::{ToolEffectDispatcher, ToolEffectError};
use gents_pack::plugin::PluginVerdict;
use gents_tools::native::file_tools::read_tools::ReadFileTool;
use gents_tools::native::shared::ToolContext;
use serde_json::{json, Value};
use sha2::Digest;

struct Files(ReadFileTool, std::sync::atomic::AtomicU32);

impl ToolEffectDispatcher for Files {
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async { vec![self.0.definition(String::new()).await] })
    }

    fn call<'a>(
        &'a self,
        parent: &'a gents_loop::tool::ToolCallEnvelope,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async move {
            assert_eq!(name, "read_file");
            assert_eq!(
                ordinal,
                self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
            );
            let args: Value = serde_json::from_str(arguments).unwrap();
            assert_eq!(args["raw_json"], true);
            self.0
                .call(serde_json::from_value(args).unwrap(), parent)
                .await
                .map(ToolOutcome::Completed)
                .map_err(|error| ToolEffectError::Unavailable(error.to_string()))
        })
    }
}

#[tokio::test]
#[ignore = "set GENTS_SCRIPT_WASM to the script pack's compiled WASM"]
async fn script_discovers_batches_and_reads_objects_on_a_scratch_home() {
    let wasm =
        std::fs::read(std::env::var_os("GENTS_SCRIPT_WASM").expect("GENTS_SCRIPT_WASM")).unwrap();
    let bytes = gents_pack::plugin::test_fixtures::build_plugin_afb_bytes(&wasm);
    let (home, mut record) = gents_pack::plugin::test_fixtures::installed_echo();
    let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
    gents_pack::plugin::store::store_bytes(home.path(), &digest, &bytes).unwrap();
    record.digest = format!("sha256:{digest}");
    record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
    gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
    std::fs::write(home.path().join("a.json"), "{\"rows\": [1, 2]}\n").unwrap();
    std::fs::write(home.path().join("b.txt"), "beta\r\ngamma").unwrap();
    let files = Files(
        ReadFileTool::new(
            ToolContext::new(home.path().to_owned(), false).unwrap(),
            32000,
        ),
        Default::default(),
    );
    let executor = crate::plugin::executor::PluginExecutor::new(Some(home.path().to_owned()));
    let files = Arc::new(files);
    let input = json!({"script": r#"
            const names = Object.keys(tools);
            const files = await Promise.all([tools.read_file({path: 'a.json'}), tools.read_file({path: 'b.txt'})]);
            return {names, types: files.map(f => typeof f), rows: JSON.parse(files[0].content).rows, text: files[1].content, ok: files.every(f => f.ok)};
        "#});
    let envelope = gents_loop::tool::ToolCallEnvelope::default()
        .with_effects(Some(files.clone() as Arc<dyn ToolEffectDispatcher>));
    let result = executor
        .call_data_bound_for(
            &record,
            input,
            super::super::executor::ToolInvocation {
                tool_root: None,
                own_folder: None,
                tool_calls: true,
                context: &envelope,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        result.outcome.verdict,
        PluginVerdict::Success,
        "{}",
        result.outcome.diagnostics
    );
    assert_eq!(
        result.outcome.output,
        json!({"names": ["read_file"], "types": ["object", "object"], "rows": [1, 2], "text": "beta\r\ngamma", "ok": true})
    );
    let oversized = executor
        .call_data_bound_for(
            &record,
            json!({"script": r#"
            const path = 'x'.repeat(600000);
            await Promise.all([tools.read_file({path}), tools.read_file({path})]);
        "#}),
            super::super::executor::ToolInvocation {
                tool_root: None,
                own_folder: None,
                tool_calls: true,
                context: &envelope,
            },
            None,
        )
        .await
        .unwrap();
    assert_ne!(oversized.outcome.verdict, PluginVerdict::Success);
    assert!(
        oversized.outcome.diagnostics.contains("TOO_LARGE"),
        "{}",
        oversized.outcome.diagnostics
    );
    assert!(!oversized
        .outcome
        .diagnostics
        .contains("ScriptHostProtocolMismatch"));
    assert_eq!(files.1.load(std::sync::atomic::Ordering::SeqCst), 2);
}
