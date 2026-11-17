use std::sync::Arc;

use gents_loop::tool::{Tool, ToolDefinition, ToolError};
use gents_loop::tool_call_lifecycle::{
    runtime::scope_request_tool_execution_with_session, ToolOutcome,
};
use gents_loop::tool_effects::{self, ToolEffectDispatcher};
use gents_loop::{HookAction, SessionHook, ToolCallHookAction};
use gents_protocol::message::Message;
use serde_json::{json, Value};

struct Query;
impl Tool for Query {
    const NAME: &'static str = "query";
    type Error = ToolError;
    type Args = Value;
    type Output = Value;
    async fn definition(&self, _: String) -> ToolDefinition {
        ToolDefinition {
            name: "query".into(),
            description: "Reads rows.".into(),
            parameters: json!({"type":"object"}),
        }
    }
    async fn call(&self, _: Value) -> Result<Value, ToolError> {
        assert!(tool_effects::current().is_none());
        assert!(super::tool_calls::Session::for_call(true).is_none());
        Ok(json!({"rows":[1]}))
    }
}

struct Calls;
#[async_trait::async_trait]
impl SessionHook for Calls {
    type Error = std::convert::Infallible;
    async fn admit_tool_effect(
        &self,
        _: &str,
        ordinal: u32,
        _: &str,
        _: &str,
    ) -> Result<String, String> {
        Ok(format!("child-{ordinal}"))
    }

    async fn on_completion_call_with_context(
        &self,
        _prompt: &Message,
        _history: &[Message],
        _context: Option<&Message>,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        _tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
    ) -> ToolCallHookAction {
        ToolCallHookAction::Continue
    }

    async fn on_tool_admission_rejected(
        &self,
        _tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        _outcome: &ToolOutcome,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_tool_result(
        &self,
        _tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        _outcome: &ToolOutcome,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn foreground_live_output_writer(
        &self,
        internal_call_id: &str,
    ) -> gents_loop::live_output::LiveToolOutputWriter {
        gents_loop::live_output::LiveToolOutputRegistry::default()
            .writer_for(internal_call_id.to_string())
            .await
    }

    async fn session_id(&self) -> Option<String> {
        None
    }

    async fn register_stream_tool_call_identity(
        &self,
        _internal_call_id: &str,
        _result_id: &str,
        _call_id: Option<&str>,
    ) {
    }
}

fn dispatcher() -> Arc<dyn ToolEffectDispatcher> {
    gents_loop::loop_stream::tool_effect_dispatcher(
        Arc::new(vec![
            gents_loop::tool_policy::presentation::PresentedTool::wrap(
                Box::new(Query),
                true,
                false,
            ),
        ]),
        Arc::new(Calls),
        "parent".into(),
    )
}

async fn guest(request: Value) -> Value {
    let (home, mut record) = gents_pack::plugin::test_fixtures::installed_plugin(
        &super::tests::executor::host_call_plugin_wat(&request, 1),
        None,
    );
    record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
    gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
    let executor = super::executor::PluginExecutor::new(Some(home.path().to_owned()));
    let result = scope_request_tool_execution_with_session(
        None,
        Default::default(),
        None,
        None,
        None,
        tool_effects::scope(
            Some(dispatcher()),
            executor.call_data_bound_for(&record, json!({}), None, true, None),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        result.outcome.verdict,
        gents_pack::plugin::PluginVerdict::Success,
        "{}",
        result.outcome.diagnostics
    );
    result.outcome.output
}

#[tokio::test]
async fn hidden_tools_remain_discoverable_and_callable_through_a_composing_guest() {
    assert_eq!(
        guest(json!({"describe":null})).await,
        json!({"result":["query"]})
    );
    let definition = guest(json!({"describe":"query"})).await;
    assert_eq!(definition["result"]["description"], "Reads rows.");
    assert_eq!(definition["result"]["parameters"], json!({"type":"object"}));
    assert_eq!(
        guest(json!({"tool_name":"query","arguments":{}})).await,
        json!({"result":{"rows":[1]}})
    );
    assert!(guest(json!({"describe":"ungranted"})).await["error"]
        .as_str()
        .unwrap()
        .contains("not in this request"));
    assert!(
        guest(json!({"tool_name":"ungranted","arguments":{}})).await["error"]
            .as_str()
            .unwrap()
            .contains("not in this request")
    );
    tool_effects::scope(Some(dispatcher()), async {
        assert!(super::tool_calls::Session::for_call(false).is_none());
        tool_effects::scope(None, async {
            assert!(super::tool_calls::Session::for_call(true).is_none());
        })
        .await;
    })
    .await;
}

#[tokio::test]
#[ignore = "set GENTS_SCRIPT_WASM to the script pack's compiled WASM"]
async fn script_discovers_hidden_tools_with_object_keys_and_calls_their_contract() {
    use sha2::Digest;
    let wasm =
        std::fs::read(std::env::var_os("GENTS_SCRIPT_WASM").expect("GENTS_SCRIPT_WASM")).unwrap();
    let bytes = gents_pack::plugin::test_fixtures::build_plugin_afb_bytes(&wasm);
    let (home, mut record) = gents_pack::plugin::test_fixtures::installed_echo();
    let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
    gents_pack::plugin::store::store_bytes(home.path(), &digest, &bytes).unwrap();
    record.digest = format!("sha256:{digest}");
    record.declaration.host_imports = vec![gents_pack::pack::HostImport::HostCall];
    gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
    let executor = super::executor::PluginExecutor::new(Some(home.path().to_owned()));
    let result = scope_request_tool_execution_with_session(None, Default::default(), None, None, None, tool_effects::scope(Some(dispatcher()), executor.call_data_bound_for(&record, json!({"script":"return { names: Object.keys(tools), description: tools.query.description, parameters: tools.query.parameters, result: await tools.query({}) };"}), None, true, None))).await.unwrap();
    assert_eq!(
        result.outcome.verdict,
        gents_pack::plugin::PluginVerdict::Success,
        "{}",
        result.outcome.diagnostics
    );
    assert_eq!(
        result.outcome.output,
        json!({"names":["query"],"description":"Reads rows.","parameters":{"type":"object"},"result":{"rows":[1]}})
    );
}
