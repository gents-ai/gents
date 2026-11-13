use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};

use defra_node::Node as EmbeddedNode;
use gents_protocol::job::DataWrite;

use super::data::DataCaller;
use super::executor::PluginExecutor;
use crate::call::lifecycle::delivery::PluginToolWrites;
use gents_documents::{PluginToolRef, WriteToolField};
use gents_loop::live_output::DeferredToolWrites;
use gents_loop::tool::{BoxFuture, ToolDefinition, ToolDispatchResult, ToolDyn, ToolError};
use gents_loop::tool_call_lifecycle::runtime::current_tool_runtime_context;
use gents_pack::plugin::store::InstalledPlugin;
use gents_pack::plugin::PluginVerdict;

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
    data_caller: Option<DataCaller>,
    model_actor: Option<identity::Did>,
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
            data_caller: None,
            model_actor: None,
        })
    }

    pub fn with_agent_data(
        mut self,
        node: Arc<EmbeddedNode>,
        node_did: &str,
        agent_did: Option<&str>,
    ) -> Result<Self> {
        if let Some(agent_did) = agent_did {
            if self.record.model_binding.is_some() {
                anyhow::ensure!(
                    agent_did != node_did && node.node_identity_did() != Some(agent_did),
                    "plugin model requires a distinct selected Agent"
                );
            }
            self.model_actor = Some(identity::Did::new(agent_did)?);
        }
        if self.record.data_grant().is_some() {
            let agent_did = agent_did.ok_or_else(|| {
                anyhow::anyhow!("plugin data requires the selected agent identity")
            })?;
            self.data_caller = Some(DataCaller::for_agent(node, node_did, agent_did)?);
        }
        Ok(self)
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
            actor: self
                .data_caller
                .as_ref()
                .and_then(DataCaller::actor)
                .cloned()
                .ok_or_else(|| tool_error("plugin writes require the selected agent identity"))?,
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
            let input: serde_json::Value = match gents_loop::tool::parse_tool_args(&args) {
                Ok(input) => input,
                Err(error) => {
                    return ToolDispatchResult {
                        result: Err(error),
                        plugin_receipt: None,
                        deferred_writes: None,
                    };
                }
            };
            let invoked = self.invoke(input).await;
            ToolDispatchResult {
                result: invoked.result.and_then(|(output, _)| {
                    serde_json::to_string(&output).map_err(ToolError::JsonError)
                }),
                plugin_receipt: Some(invoked.receipt),
                deferred_writes: invoked.deferred_writes,
            }
        })
    }
}

/// One invocation of the tool: the plugin's output and binding note, the
/// receipt of the attempt, and the writes its terminal commits.
struct Invoked {
    result: Result<(serde_json::Value, Option<String>), ToolError>,
    receipt: gents_protocol::plugin::PluginExecutionReceipt,
    deferred_writes: Option<DeferredToolWrites>,
}

impl PluginTool {
    async fn invoke(&self, mut input: serde_json::Value) -> Invoked {
        if let Err(error) = fill_inputs(&mut input, &self.input_fields) {
            return Invoked {
                result: Err(tool_error(format!("{error:#}"))),
                receipt: super::executor::initial_receipt(&self.record, &input),
                deferred_writes: None,
            };
        }
        let (call, receipt) = self
            .executor
            .call_data_bound_with_receipt_as(
                &self.record,
                input,
                self.root.as_deref(),
                self.tool_calls,
                self.data_caller.clone(),
                self.model_actor.clone(),
            )
            .await;
        let mut deferred_writes = None;
        let result = call
            .map_err(|error| tool_error(format!("{error:#}")))
            .and_then(|mut call| match call.outcome.verdict {
                PluginVerdict::Success => {
                    deferred_writes = self.defer_writes(&mut call.outcome.output)?;
                    Ok((call.outcome.output, call.binding_note))
                }
                _ => Err(tool_error(format!(
                    "plugin {} did not return a result: {}",
                    call.coordinate, call.outcome.diagnostics
                ))),
            });
        Invoked {
            result,
            receipt,
            deferred_writes,
        }
    }

    /// The tool `config plugin run` calls: `coordinate` as the calling
    /// agent's Tools select it, or unselected (no `tool_calls`) when they do
    /// not. Trigger-filled inputs are the caller's to supply here.
    pub(crate) fn for_run(
        executor: Arc<PluginExecutor>,
        coordinate: &str,
        selected: &[PluginToolRef],
    ) -> Result<Self> {
        let selection = selected.iter().find(|plugin| plugin.plugin == coordinate);
        Self::resolve(
            executor,
            &PluginToolRef {
                plugin: coordinate.to_owned(),
                digest: None,
                tool_calls: selection.is_some_and(|plugin| plugin.tool_calls),
                input_fields: Vec::new(),
                job: None,
            },
            None,
        )
    }

    /// One call through the rounds a tool call drives, inside the tool call
    /// that asked for it: the plugin's `writes` commit with that call's row.
    /// An output that is only a request the host did not serve is an error
    /// naming the service and the call that serves it, never the plugin's
    /// result; a result that carries a request-shaped field is returned with
    /// a note saying that field was not served.
    pub(crate) async fn run_once(
        &self,
        input: serde_json::Value,
    ) -> Result<(serde_json::Value, Option<String>, Option<String>)> {
        let invoked = self.invoke(input).await;
        let (output, binding_note) = invoked.result.map_err(|error| match error {
            ToolError::ToolCallError(error) => anyhow::anyhow!("{error}"),
            other => anyhow::anyhow!("{other}"),
        })?;
        let carried = match super::rounds::unserved_request(&output) {
            Some(super::rounds::Unserved::Request(service)) => {
                anyhow::bail!("{}", self.unserved(service).0)
            }
            Some(super::rounds::Unserved::Carried(service)) => {
                let (_, why, next) = self.unserved(service);
                Some(format!("output is the plugin's result. Its {service} field is shaped like a request and was not served ({why}); if the plugin meant it as a request: {next}"))
            }
            None => None,
        };
        if let Some(writes) = invoked.deferred_writes {
            current_tool_runtime_context()
                .and_then(|context| context.live_output)
                .context("plugin writes commit with a persisted tool call")?
                .record_deferred_writes(writes)
                .await
                .map_err(|error| anyhow::anyhow!("plugin writes rejected: {error}"))?;
        }
        Ok((output, carried, binding_note))
    }

    /// The refusal for an unserved `service` request, with its reason and
    /// next call on their own.
    fn unserved(&self, service: &str) -> (String, String, String) {
        let coordinate = format!("{}/{}", self.record.namespace, self.record.name);
        let name = &self.definition.name;
        let (why, next) = match service {
            "data_calls" => (
                "the plugin declares no data".to_owned(),
                format!("declare data.collections and data.ops in its source, then rebuild it with [\"plugin\",\"build\",\"{name}\"]"),
            ),
            "tool_calls" => (
                format!("your Tools do not select {coordinate} with tool_calls: true"),
                format!("read [\"tools\",\"get\"], add {{\"plugin\":\"{coordinate}\",\"tool_calls\":true}} to integrations.plugins with [\"tools\",\"update\"], then run it again in a fresh request"),
            ),
            "job_calls" => (
                "plugin run serves no job: a job's steps run from its own tool call".to_owned(),
                format!("select {coordinate} with `job` in integrations.plugins through [\"tools\",\"update\"], then call the tool `{name}` in a fresh request"),
            ),
            "model_calls" => (
                "the plugin has no usable model binding".to_owned(),
                format!("read [\"plugin\",\"get\",\"{coordinate}\"] and reinstall its pack with options.inference-slot"),
            ),
            _ => (
                "the plugin's granted authority allows no network host".to_owned(),
                format!("read [\"plugin\",\"get\",\"{coordinate}\"] and reinstall its pack with the network authority granted"),
            ),
        };
        (
            format!("plugin {coordinate} asked for {service} and was not re-invoked: {why}. Its request was not served and is not a result. Next: {next}"),
            why,
            next,
        )
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
            gents_store::graphql::validate_graphql_name(&field.name).is_ok()
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
        gents_tools::builtin::defra_write::validate_field_input(
            "String",
            false,
            true,
            object.get(&field.name),
        )
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
    use gents_documents::WriteToolFieldFill;
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
            None,
            None,tokio_util::sync::CancellationToken::new(),None,None,None,
            Some("book-run".into()),[("book_path".into(),"/books/one".into())].into(),false,async {
                for case in gents_lean_vocab::families::contracts_write_input_plugin_fill::cases::<String>() {
                    let value = match case.input.actual.as_deref() {
                        None => None, Some("text")=>Some(json!("/books/other")),
                        Some("integer")=>Some(json!(1)),Some("number")=>Some(json!(0.5)),
                        Some("boolean")=>Some(json!(false)),Some("array")=>Some(json!([1])),
                        Some("object")=>Some(json!({"path":"other"})),Some("null")=>Some(serde_json::Value::Null),
                        other=>panic!("unknown modeled input {other:?}")};
                    let mut input=json!({"page":3});
                    if let Some(value)=value {input["path"]=value;}
                    let before=input.clone();
                    let result=fill_inputs(&mut input,&fields());
                    if result.is_ok() {assert_eq!(input,json!({"page":3,"path":"/books/one","run_id":"book-run"}));}
                    else {assert_eq!(input,before);}
                    case.check(result.is_ok());
                }
            }).await;
        let mut missing = json!({"page":3});
        assert!(fill_inputs(&mut missing, &fields()).is_err());
        assert_eq!(missing, json!({"page":3}));
    }
}
