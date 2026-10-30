use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use defra_node::EmbeddedNode;
use gents_protocol::job::DataWrite;

use super::data::DataCaller;
use super::executor::PluginExecutor;
use super::store::InstalledPlugin;
use super::PluginVerdict;
use crate::documents::{PluginToolRef, WriteToolField};
use crate::inference::llm::tool::{
    BoxFuture, ToolDefinition, ToolDispatchResult, ToolDyn, ToolError,
};
use crate::tools::call::lifecycle::delivery::PluginToolWrites;
use gents_loop::live_output::DeferredToolWrites;
use gents_loop::tool_call_lifecycle::runtime::current_tool_runtime_context;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct PluginToolError(String);

fn tool_error(message: impl Into<String>) -> ToolError {
    ToolError::ToolCallError(Box::new(PluginToolError(message.into())))
}

pub struct PluginTool {
    executor: Arc<PluginExecutor>,
    record: InstalledPlugin,
    definition: ToolDefinition,
    root: Option<PathBuf>,
    input_fields: Vec<WriteToolField>,
    tool_calls: bool,
    /// The node a data-declaring plugin's `data_calls` read and whose
    /// terminal commits its writes; absent, the plugin gets no data session.
    data_node: Option<Arc<EmbeddedNode>>,
}

impl PluginTool {
    pub fn resolve(
        executor: Arc<PluginExecutor>,
        plugin: &PluginToolRef,
        root: Option<PathBuf>,
    ) -> Result<Self> {
        let record = executor.resolve(&plugin.plugin, plugin.digest.as_deref())?;
        anyhow::ensure!(
            plugin.job.is_none() || record.declaration.job.0,
            "plugin {} does not declare job dispatch; install a pack whose plugin sets `job: true`",
            plugin.plugin
        );
        let mut description = record
            .instructions
            .clone()
            .unwrap_or_else(|| record.declaration.description.clone());
        if plugin.tool_calls {
            description.push_str(&super::tool_calls::composition_note());
        }
        let definition = ToolDefinition {
            name: plugin.tool_name().to_string(),
            description,
            parameters: bound_schema(&record.declaration.input_schema, &plugin.input_fields)?,
        };
        Ok(Self {
            executor,
            record,
            definition,
            root,
            input_fields: plugin.input_fields.clone(),
            tool_calls: plugin.tool_calls,
            data_node: None,
        })
    }

    /// Offers `node` as the data store of a plugin that declares `data`.
    pub fn with_data_node(mut self, node: Arc<EmbeddedNode>) -> Self {
        self.data_node = Some(node);
        self
    }

    /// The `data_calls` caller for this tool call: the invoking node's DID.
    fn data_caller(&self) -> Option<DataCaller> {
        let node_did = current_tool_runtime_context()?.node_did?;
        DataCaller::new(self.data_node.clone()?, &node_did).ok()
    }

    /// Takes the `writes` a data-declaring plugin returned beside its output
    /// and hands them to the dispatch result, which carries them to the
    /// terminal of this tool call to commit with the tool row. A plugin
    /// without a data grant keeps its output whole.
    fn defer_writes(
        &self,
        output: &mut serde_json::Value,
    ) -> Result<Option<DeferredToolWrites>, ToolError> {
        let Some(grant) = self.record.data_grant() else {
            return Ok(None);
        };
        let Some(writes) = output.as_object_mut().and_then(|o| o.remove("writes")) else {
            return Ok(None);
        };
        let writes: Vec<DataWrite> = serde_json::from_value(writes).map_err(|error| {
            tool_error(format!(
                "plugin writes must be [{{op, collection, doc_id?, fields, on_conflict?}}]: {error}"
            ))
        })?;
        if writes.is_empty() {
            return Ok(None);
        }
        if current_tool_runtime_context()
            .and_then(|context| context.live_output)
            .is_none()
        {
            return Err(tool_error("plugin writes commit with a persisted tool call, and this call has none; call the tool from a session"));
        }
        Ok(Some(DeferredToolWrites::new(PluginToolWrites {
            grant,
            writes,
        })))
    }
}

impl ToolDyn for PluginTool {
    fn name(&self) -> String {
        self.definition.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async { self.definition.clone() })
    }

    fn call<'a>(&'a self, args: String) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { self.call_with_receipt(args).await.result })
    }

    fn call_with_receipt<'a>(&'a self, args: String) -> BoxFuture<'a, ToolDispatchResult> {
        Box::pin(async move {
            let mut input: serde_json::Value =
                match crate::inference::llm::tool::parse_tool_args(&args) {
                    Ok(input) => input,
                    Err(error) => {
                        return ToolDispatchResult {
                            result: Err(error),
                            plugin_receipt: None,
                            deferred_writes: None,
                        };
                    }
                };
            if let Err(error) = fill_inputs(&mut input, &self.input_fields) {
                return ToolDispatchResult {
                    result: Err(tool_error(format!("{error:#}"))),
                    plugin_receipt: Some(super::executor::initial_receipt(&self.record, &input)),
                    deferred_writes: None,
                };
            }
            let (call, receipt) = self
                .executor
                .call_data_bound_with_receipt_for(
                    &self.record,
                    input,
                    self.root.as_deref(),
                    self.tool_calls,
                    self.data_caller(),
                )
                .await;
            let mut deferred_writes = None;
            let result = call
                .map_err(|error| tool_error(format!("{error:#}")))
                .and_then(|mut call| match call.outcome.verdict {
                    PluginVerdict::Success => {
                        deferred_writes = self.defer_writes(&mut call.outcome.output)?;
                        serde_json::to_string(&call.outcome.output).map_err(ToolError::JsonError)
                    }
                    _ => Err(tool_error(format!(
                        "plugin {} did not return a result: {}",
                        call.coordinate, call.outcome.diagnostics
                    ))),
                });
            ToolDispatchResult {
                result,
                plugin_receipt: Some(receipt),
                deferred_writes,
            }
        })
    }
}

fn bound_schema(
    schema: &serde_json::Value,
    fields: &[WriteToolField],
) -> Result<serde_json::Value> {
    let mut schema = schema.clone();
    if fields.is_empty() {
        return Ok(schema);
    }
    let properties = schema["properties"]
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("runtime-filled plugin inputs require an object schema"))?;
    let mut seen = std::collections::BTreeSet::new();
    for field in fields {
        anyhow::ensure!(
            crate::graphql::validate_graphql_name(&field.name).is_ok()
                && field.fill.is_some()
                && !field.required
                && seen.insert(&field.name),
            "plugin input_fields must be distinct runtime-filled fields with required omitted"
        );
        let shape = properties
            .remove(&field.name)
            .ok_or_else(|| anyhow::anyhow!("plugin input schema has no field {:?}", field.name))?;
        anyhow::ensure!(
            shape["type"] == "string",
            "runtime-filled plugin input {:?} must be a String",
            field.name
        );
    }
    if let Some(required) = schema["required"].as_array_mut() {
        required.retain(|name| {
            !fields
                .iter()
                .any(|field| name.as_str() == Some(&field.name))
        });
    }
    Ok(schema)
}

fn fill_inputs(input: &mut serde_json::Value, fields: &[WriteToolField]) -> Result<()> {
    if fields.is_empty() {
        return Ok(());
    }
    let object = input
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("plugin input must be an object"))?;
    let mut resolved = Vec::new();
    for field in fields {
        crate::defra_write::validate_field_input("String", false, true, object.get(&field.name))
            .map_err(|error| anyhow::anyhow!("plugin input {:?}: {error}", field.name))?;
        let fill = field
            .fill
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing plugin input fill"))?;
        resolved.push((
            field.name.clone(),
            serde_json::Value::String(fill.resolve(&field.name)?),
        ));
    }
    object.extend(resolved);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::WriteToolFieldFill;
    use serde_json::json;

    fn fields() -> Vec<WriteToolField> {
        vec![
            WriteToolField {
                name: "path".into(),
                required: false,
                fill: Some(WriteToolFieldFill::SourceField("book_path".into())),
            },
            WriteToolField {
                name: "run_id".into(),
                required: false,
                fill: Some(WriteToolFieldFill::Correlation),
            },
        ]
    }

    #[test]
    fn plugin_bindings_hide_only_declared_string_inputs() {
        let schema = json!({"type":"object","additionalProperties":false,
            "properties":{"path":{"type":"string"},"run_id":{"type":"string"},"page":{"type":"integer"}},
            "required":["path","run_id","page"]});
        let bound = bound_schema(&schema, &fields()).unwrap();
        assert_eq!(bound["required"], json!(["page"]));
        assert_eq!(bound["properties"], json!({"page":{"type":"integer"}}));
        assert_eq!(bound["additionalProperties"], false);
        let mut wrong = schema.clone();
        wrong["properties"]["path"]["type"] = json!("integer");
        assert!(bound_schema(&wrong, &fields()).is_err());
        assert!(bound_schema(&schema, &[fields()[0].clone(), fields()[0].clone()]).is_err());
        assert_eq!(bound_schema(&schema, &[]).unwrap(), schema);
    }

    #[tokio::test]
    async fn plugin_bound_inputs_follow_generated_write_input_admission() {
        gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
            None,tokio_util::sync::CancellationToken::new(),None,None,None,
            Some("book-run".into()),[("book_path".into(),"/books/one".into())].into(),false,async {
                for case in crate::lean_vocab_test::lean_write_input_cases() {
                    if case["filled"] != true { continue; }
                    let value = match case["actual"].as_str() {
                        None => None, Some("text")=>Some(json!("/books/other")),
                        Some("integer")=>Some(json!(1)),Some("number")=>Some(json!(0.5)),
                        Some("boolean")=>Some(json!(false)),Some("array")=>Some(json!([1])),
                        Some("object")=>Some(json!({"path":"other"})),Some("null")=>Some(serde_json::Value::Null),
                        other=>panic!("unknown modeled input {other:?}")};
                    let mut input=json!({"page":3});
                    if let Some(value)=value {input["path"]=value;}
                    let before=input.clone();
                    let result=fill_inputs(&mut input,&fields());
                    assert_eq!(result.is_ok(),case["accepted"].as_bool().unwrap(),"{case}");
                    if result.is_ok() {assert_eq!(input,json!({"page":3,"path":"/books/one","run_id":"book-run"}));}
                    else {assert_eq!(input,before);}
                }
            }).await;
        let mut missing = json!({"page":3});
        assert!(fill_inputs(&mut missing, &fields()).is_err());
        assert_eq!(missing, json!({"page":3}));
    }
}
