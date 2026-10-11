//! An installed plugin offered to the model as a tool.
//!
//! The model sees the plugin's own name, description and input schema, and
//! supplies only the arguments. Which artifact runs, and with what authority,
//! is the installed record's, resolved when the tool surface is built.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use super::executor::PluginExecutor;
use super::store::InstalledPlugin;
use super::PluginVerdict;
use crate::document_config::{PluginToolRef, WriteToolField};
use crate::llm::tool::{BoxFuture, ToolDefinition, ToolDispatchResult, ToolDyn, ToolError};

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
    /// The root the operator gave the agent's file tools: the working folder
    /// of a session that has no workspace folder of its own.
    root: Option<PathBuf>,
    input_fields: Vec<WriteToolField>,
}

impl PluginTool {
    /// The tool `plugin` names, refused when it is not installed or no
    /// longer the pinned artifact.
    pub fn resolve(
        executor: Arc<PluginExecutor>,
        plugin: &PluginToolRef,
        root: Option<PathBuf>,
    ) -> Result<Self> {
        let record = executor.resolve(&plugin.plugin, plugin.digest.as_deref())?;
        let definition = ToolDefinition {
            name: plugin.tool_name().to_string(),
            description: record
                .instructions
                .clone()
                .unwrap_or_else(|| record.declaration.description.clone()),
            parameters: bound_schema(&record.declaration.input_schema, &plugin.input_fields)?,
        };
        Ok(Self {
            executor,
            record,
            definition,
            root,
            input_fields: plugin.input_fields.clone(),
        })
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
            let mut input: serde_json::Value = match crate::llm::tool::parse_tool_args(&args) {
                Ok(input) => input,
                Err(error) => {
                    return ToolDispatchResult {
                        result: Err(error),
                        plugin_receipt: None,
                    }
                }
            };
            if let Err(error) = fill_inputs(&mut input, &self.input_fields) {
                return ToolDispatchResult {
                    result: Err(tool_error(format!("{error:#}"))),
                    plugin_receipt: Some(super::executor::initial_receipt(&self.record, &input)),
                };
            }
            let (call, receipt) = self
                .executor
                .call_data_bound_with_receipt(&self.record, input, self.root.as_deref())
                .await;
            let result =
                call.map_err(|error| tool_error(format!("{error:#}")))
                    .and_then(|call| match call.outcome.verdict {
                        PluginVerdict::Success => serde_json::to_string(&call.outcome.output)
                            .map_err(ToolError::JsonError),
                        _ => Err(tool_error(format!(
                            "plugin {} did not return a result: {}",
                            call.coordinate, call.outcome.diagnostics
                        ))),
                    });
            ToolDispatchResult {
                result,
                plugin_receipt: Some(receipt),
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
    use crate::document_config::WriteToolFieldFill;
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
