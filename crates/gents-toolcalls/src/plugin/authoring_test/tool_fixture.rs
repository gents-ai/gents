use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use gents_loop::tool::{BoxFuture, ToolDefinition};
use gents_loop::tool_call_lifecycle::ToolOutcome;
use gents_loop::tool_effects::{ToolEffectDispatcher, ToolEffectError};
use gents_pack::plugin::{BoundDir, PluginBudget, PluginOutcome, PluginRunner};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolFixture {
    definitions: Vec<ToolDefinition>,
    calls: Vec<Call>,
    #[serde(skip)]
    used: AtomicUsize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    tool_name: String,
    arguments: Value,
    result: Value,
}

impl ToolFixture {
    pub async fn run(
        self,
        runner: Arc<PluginRunner>,
        input: Value,
        budget: PluginBudget,
        bound: Option<BoundDir>,
    ) -> Result<PluginOutcome> {
        anyhow::ensure!(
            runner.definition().imports_host_call(),
            "tools fixtures require host_call"
        );
        let fixture = Arc::new(self);
        let parent = gents_loop::tool::ToolCallEnvelope::default()
            .with_effects(Some(fixture.clone() as Arc<dyn ToolEffectDispatcher>));
        let tools = super::super::tool_calls::Session::for_call(true, &parent);
        let outcome =
            super::super::host_calls::run(runner, bound, input, budget, budget.wall_clock, tools)
                .await?;
        anyhow::ensure!(
            fixture.used.load(Ordering::SeqCst) == fixture.calls.len(),
            "tools fixture consumed {} of {} calls",
            fixture.used.load(Ordering::SeqCst),
            fixture.calls.len(),
        );
        Ok(outcome)
    }
}

impl ToolEffectDispatcher for ToolFixture {
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async { self.definitions.clone() })
    }

    fn call<'a>(
        &'a self,
        _parent: &'a gents_loop::tool::ToolCallEnvelope,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async move {
            let index = self.used.fetch_add(1, Ordering::SeqCst);
            let call = self
                .calls
                .get(index)
                .filter(|call| {
                    usize::try_from(ordinal).ok() == Some(index + 1)
                        && call.tool_name == name
                        && serde_json::from_str::<Value>(arguments).ok().as_ref()
                            == Some(&call.arguments)
                })
                .ok_or_else(|| {
                    ToolEffectError::Fatal(format!(
                        "tools fixture call {ordinal}: unexpected {name}({arguments})"
                    ))
                })?;
            Ok(ToolOutcome::Completed(call.result.to_string()))
        })
    }
}
