//! A callback whose handler is an installed plugin.
//!
//! The plugin receives the projected source document on stdin. Its single JSON
//! result becomes the handler's output documents, with the correlation copied
//! from the source by the host, never authored by the plugin. The output
//! documents, the invocation's success and its `CallbackResult` commit in one
//! transaction, so a result is visible only once every output is written. An
//! invocation found already executing on recovery fails with its reason and is
//! never run twice.

use anyhow::Result;
use defra_node::EmbeddedNode;
use serde_json::{Map, Value};

use super::documents::{
    create_callback_result_mutation, update_invocation_mutation, CallbackInvocationDoc,
    CallbackResultDoc,
};
use super::run::{deny, encode_journal, persist_journal};
use super::{LIFECYCLE_FAILED, LIFECYCLE_RUNNING, LIFECYCLE_SUCCEEDED};
use crate::graph_pipeline::{PortCardinality, PortSpec};
use crate::plugin::executor::PluginExecutor;
use crate::plugin::PluginVerdict;
use crate::workspace::journal::advance;
use crate::workspace::{ActionJournalEntry, ActionJournalState};

/// A plugin writes only a pack's own collections: never the runtime's
/// documents (requests, triggers, callbacks and the rest) and never a
/// protected collection.
fn writable_output_collection(collection: &str) -> Result<()> {
    crate::graphql::validate_collection_identifier(collection)?;
    anyhow::ensure!(
        !gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&collection),
        "a plugin cannot write {collection}, which belongs to the runtime"
    );
    crate::document_config::reject_protected_collection_name(collection)
}

/// Refuses a plugin handler that could never run or write its outputs.
pub fn validate_handler(
    plugin: &str,
    digest: &str,
    correlation_field: Option<&str>,
    outputs: &[PortSpec],
) -> Result<()> {
    crate::plugin::store::parse_coordinate(plugin)?;
    let hex = digest.strip_prefix("sha256:").unwrap_or_default();
    anyhow::ensure!(
        hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit()),
        "callback plugin {plugin} pins {digest:?}, which is not sha256:<64 hex>"
    );
    if let Some(field) = correlation_field {
        crate::graphql::validate_graphql_name(field)?;
    }
    let mut names = std::collections::BTreeSet::new();
    for output in outputs {
        writable_output_collection(&output.collection)?;
        crate::graphql::validate_graphql_name(&output.correlation_field)?;
        anyhow::ensure!(
            names.insert(output.name.as_str()),
            "callback plugin {plugin} declares output {:?} twice",
            output.name
        );
        anyhow::ensure!(
            correlation_field.is_some(),
            "callback plugin {plugin} writes {} with a correlation field but reads none from its source",
            output.collection
        );
    }
    Ok(())
}

/// The documents `output` writes, as `(collection, document)` pairs.
///
/// One output takes the plugin's value itself (an object, or an array of
/// objects for `many`); several outputs take an object keyed by output name.
/// Each document gets `correlation` in its output's correlation field, which
/// the plugin may not set itself.
pub(crate) fn output_documents(
    output: &Value,
    outputs: &[PortSpec],
    correlation: Option<&str>,
) -> Result<Vec<(String, Value)>, String> {
    let per_port: Vec<(&PortSpec, Option<&Value>)> = match outputs {
        [] => return Ok(Vec::new()),
        [only] => vec![(only, Some(output))],
        many => {
            let object = output.as_object().ok_or_else(|| {
                "the plugin must return an object keyed by output name".to_owned()
            })?;
            if let Some(unknown) = object
                .keys()
                .find(|key| !many.iter().any(|port| &port.name == *key))
            {
                return Err(format!("the plugin returned unknown output {unknown:?}"));
            }
            many.iter()
                .map(|port| (port, object.get(&port.name)))
                .collect()
        }
    };
    let mut documents = Vec::new();
    for (port, value) in per_port {
        writable_output_collection(&port.collection).map_err(|error| format!("{error:#}"))?;
        let rows: Vec<&Value> = match (&port.cardinality, value) {
            (_, None | Some(Value::Null)) => Vec::new(),
            (PortCardinality::One, Some(value)) => vec![value],
            (PortCardinality::Many, Some(Value::Array(items))) => items.iter().collect(),
            (PortCardinality::Many, Some(_)) => {
                return Err(format!(
                    "output {:?} takes an array of documents",
                    port.name
                ))
            }
        };
        if port.required && rows.is_empty() {
            return Err(format!(
                "the plugin did not return required output {:?}",
                port.name
            ));
        }
        for row in rows {
            let mut document: Map<String, Value> = row
                .as_object()
                .cloned()
                .ok_or_else(|| format!("output {:?} takes objects", port.name))?;
            let correlation = correlation.ok_or_else(|| {
                "the source document carries no correlation for the outputs".to_owned()
            })?;
            if document
                .get(&port.correlation_field)
                .is_some_and(|value| value.as_str() != Some(correlation))
            {
                return Err(format!(
                    "the plugin changed {:?}, which the runtime writes",
                    port.correlation_field
                ));
            }
            document.insert(
                port.correlation_field.clone(),
                Value::String(correlation.to_owned()),
            );
            documents.push((port.collection.clone(), Value::Object(document)));
        }
    }
    Ok(documents)
}

pub(super) struct PluginHandler<'a> {
    pub plugin: &'a str,
    pub digest: &'a str,
    pub correlation_field: Option<&'a str>,
    pub outputs: &'a [PortSpec],
}

/// Runs a claimed, running invocation whose callback is a plugin.
pub(super) async fn execute(
    node: &EmbeddedNode,
    invocation: &mut CallbackInvocationDoc,
    handler: PluginHandler<'_>,
    source: &Value,
    plugins: &PluginExecutor,
    journal: Vec<ActionJournalEntry>,
) -> Result<()> {
    if !journal.is_empty() {
        return persist_journal(
            node,
            invocation,
            &journal,
            LIFECYCLE_FAILED,
            Some("the plugin call was interrupted before its results were written; it is not run again"),
        )
        .await;
    }
    let record = match plugins.resolve(handler.plugin, Some(handler.digest)) {
        Ok(record) => record,
        Err(error) => return deny(node, invocation, &format!("{error:#}")).await,
    };
    let mut journal = journal;
    advance(&mut journal, 0, ActionJournalState::Validated);
    advance(&mut journal, 0, ActionJournalState::Executing);
    persist_journal(node, invocation, &journal, LIFECYCLE_RUNNING, None).await?;

    let input = crate::callback::documents::strip_secret_fields(source.clone());
    // The invocation carries the correlation it was caused by; a grouped
    // delivery's input is an array, so the source itself cannot say.
    let correlation = invocation.caused_by_correlation.clone().or_else(|| {
        handler
            .correlation_field
            .and_then(|field| source.get(field))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let failure = match plugins.call(&record, input).await {
        Err(error) => Some(format!("{error:#}")),
        Ok(call) if call.outcome.verdict != PluginVerdict::Success => Some(format!(
            "plugin {} did not return a result: {}",
            call.coordinate, call.outcome.diagnostics
        )),
        Ok(call) => {
            match output_documents(
                &call.outcome.output,
                handler.outputs,
                correlation.as_deref(),
            ) {
                Err(reason) => Some(format!("plugin {}: {reason}", call.coordinate)),
                Ok(documents) => {
                    let mut written = journal.clone();
                    advance(&mut written, 0, ActionJournalState::EffectObserved);
                    advance(&mut written, 0, ActionJournalState::ResultDocsWritten);
                    match commit_success(node, invocation, &written, documents, correlation).await {
                        Ok(()) => return Ok(()),
                        Err(error) => {
                            Some(format!("writing the plugin's results failed: {error:#}"))
                        }
                    }
                }
            }
        }
    };
    let reason = failure.unwrap_or_default();
    tracing::warn!(
        invocation_id = %invocation.invocation_id,
        callback_id = %invocation.callback_id,
        %reason,
        "callback plugin invocation failed"
    );
    persist_journal(node, invocation, &journal, LIFECYCLE_FAILED, Some(&reason)).await
}

/// Writes the outputs, marks the invocation succeeded and records its result,
/// all or nothing.
async fn commit_success(
    node: &EmbeddedNode,
    invocation: &mut CallbackInvocationDoc,
    journal: &[ActionJournalEntry],
    documents: Vec<(String, Value)>,
    correlation: Option<String>,
) -> Result<()> {
    let mut succeeded = invocation.clone();
    succeeded.action_journal = Some(encode_journal(journal));
    succeeded.lifecycle_state = LIFECYCLE_SUCCEEDED.to_owned();
    succeeded.error = None;
    let update = update_invocation_mutation(&succeeded, Some(LIFECYCLE_RUNNING));
    let result = create_callback_result_mutation(&CallbackResultDoc {
        result_id: format!("res-{}", invocation.invocation_id),
        invocation_id: invocation.invocation_id.clone(),
        binding_id: Some(invocation.origin.binding_id().to_owned()),
        owner_agent_did: invocation.owner_agent_did.clone(),
        workspace_id: None,
        work_unit_id: None,
        caused_by_correlation: correlation,
        created_at: None,
    });
    let creates = documents
        .iter()
        .map(|(collection, document)| {
            crate::graphql::validate_collection_identifier(collection)?;
            let input = gents_protocol::graphql::graphql_input_literal(document)?;
            Ok(format!(
                "mutation {{ create_{collection}(input: {input}) {{ _docID }} }}"
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let invocation_id = invocation.invocation_id.clone();
    crate::config_client::ConfigAccess::transact_local(
        node,
        None,
        "callback.plugin_result",
        |txn| {
            let creates = &creates;
            let update = &update;
            let result = &result;
            let invocation_id = &invocation_id;
            Box::pin(async move {
                let updated = txn.execute(update).await?;
                anyhow::ensure!(
                    updated
                        .pointer("/data/update_CallbackInvocation")
                        .and_then(Value::as_array)
                        .is_some_and(|rows| !rows.is_empty()),
                    "CallbackInvocation {invocation_id} is no longer running"
                );
                for create in creates {
                    txn.execute(create).await?;
                }
                txn.execute(result).await?;
                Ok(())
            })
        },
    )
    .await?;
    *invocation = succeeded;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn port(name: &str, cardinality: PortCardinality, required: bool) -> PortSpec {
        PortSpec {
            name: name.into(),
            collection: format!("Out{name}"),
            schema: format!("Out{name}/v1"),
            correlation_field: "run_id".into(),
            cardinality,
            required,
        }
    }

    #[test]
    fn one_output_takes_the_value_and_gets_the_correlation() {
        let docs = output_documents(
            &json!({"score": 3}),
            &[port("a", PortCardinality::One, true)],
            Some("run-1"),
        )
        .unwrap();
        assert_eq!(
            docs,
            vec![("Outa".into(), json!({"score": 3, "run_id": "run-1"}))]
        );
    }

    #[test]
    fn many_takes_an_array_and_several_outputs_take_an_object() {
        let docs = output_documents(
            &json!({"a": [{"x": 1}, {"x": 2}], "b": {"y": 1}}),
            &[
                port("a", PortCardinality::Many, true),
                port("b", PortCardinality::One, false),
            ],
            Some("r"),
        )
        .unwrap();
        assert_eq!(docs.len(), 3);
        assert!(output_documents(
            &json!({"x": 1}),
            &[port("a", PortCardinality::Many, true)],
            Some("r")
        )
        .is_err());
    }

    #[test]
    fn the_plugin_cannot_author_runtime_facts_or_skip_required_outputs() {
        let one = [port("a", PortCardinality::One, true)];
        assert!(output_documents(&json!({"run_id": "forged"}), &one, Some("r")).is_err());
        assert!(output_documents(&json!({"run_id": "r"}), &one, Some("r")).is_ok());
        assert!(output_documents(&Value::Null, &one, Some("r")).is_err());
        assert!(output_documents(&json!({"x": 1}), &one, None).is_err());
        assert!(output_documents(&json!("text"), &one, Some("r")).is_err());
        let two = [
            port("a", PortCardinality::One, false),
            port("b", PortCardinality::One, false),
        ];
        assert!(output_documents(&json!({"c": {}}), &two, Some("r")).is_err());
        assert!(output_documents(&json!({}), &two, Some("r"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_plugin_cannot_write_the_runtimes_own_collections() {
        let mut protected = port("a", PortCardinality::One, true);
        protected.collection = "AgentRequest".into();
        let outputs = [protected];
        let digest = format!("sha256:{}", "a".repeat(64));
        assert!(validate_handler("team/lint", &digest, Some("run_id"), &outputs).is_err());
        assert!(output_documents(&json!({"x": 1}), &outputs, Some("r")).is_err());
        let mut eval = port("a", PortCardinality::One, true);
        eval.collection = gents_protocol::schemas::EVAL_RUN_NAME.into();
        assert!(output_documents(&json!({"x": 1}), &[eval], Some("r")).is_err());
    }

    #[test]
    fn a_handler_that_could_never_run_is_refused() {
        let digest = format!("sha256:{}", "a".repeat(64));
        let outputs = [port("a", PortCardinality::One, true)];
        validate_handler("team/lint", &digest, Some("run_id"), &outputs).unwrap();
        assert!(validate_handler("lint", &digest, Some("run_id"), &outputs).is_err());
        assert!(validate_handler("team/lint", "sha256:x", Some("run_id"), &outputs).is_err());
        assert!(validate_handler("team/lint", &digest, None, &outputs).is_err());
        let twice = [outputs[0].clone(), outputs[0].clone()];
        assert!(validate_handler("team/lint", &digest, Some("run_id"), &twice).is_err());
    }

    /// A node with a `Job` source, an `Echoed` output, and a binding that runs
    /// the echo plugin on every created Job; returns the created Job's id.
    async fn run_echo_binding(digest: &str) -> (std::sync::Arc<EmbeddedNode>, tempfile::TempDir) {
        let (home, _) = crate::plugin::tests::executor::installed_echo();
        let node = std::sync::Arc::new(EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        node.add_schema(
            "type Job { job_run: String text: String }
             type Echoed { job_run: String text: String run_ref: String }",
        )
        .await
        .unwrap();
        let owner = "did:key:zPluginOwner";
        let setup = format!(
            r#"mutation {{
                create_Callback(input: {{callback_id:"cb-echo",agent_did:"{owner}",enabled:true,
                    handler:{{kind:"plugin",plugin:"team/plugin",digest:"{digest}",correlation_field:"job_run",
                        outputs:[{{name:"echoed",collection:"Echoed",schema:"Echoed/v1",correlation_field:"run_ref",cardinality:"one",required:true}}]}}}}) {{_docID}}
                create_EventSource(input: {{event_source_id:"jobs",agent_did:"{owner}",source_collection:"Job",event_kind:"created"}}) {{_docID}}
                create_CallbackBinding(input: {{binding_id:"bind-echo",agent_did:"{owner}",event_source_id:"jobs",callback_id:"cb-echo",input_fields:["job_run","text"],enabled:true}}) {{_docID}}
            }}"#
        );
        let response = node.execute(&setup).await;
        assert!(!response.has_errors(), "{:?}", response.errors);

        let mut engine = super::super::CallbackEngine::new(
            node.clone(),
            owner.into(),
            None,
            tokio_util::sync::CancellationToken::new(),
        );
        engine.plugins = std::sync::Arc::new(PluginExecutor::new(Some(home.path().to_owned())));
        engine.reconcile_bindings().await;
        let created = node
            .execute(
                r#"mutation { create_Job(input: {job_run: "run-7", text: "hello"}) { _docID } }"#,
            )
            .await;
        let doc_id = crate::graphql::single_mutation_document(&created, "create_Job")
            .unwrap()
            .and_then(|row| row.get("_docID"))
            .and_then(Value::as_str)
            .unwrap()
            .to_owned();
        engine.handle_created_doc("Job", &doc_id).await;
        (node, home)
    }

    async fn rows(node: &EmbeddedNode, query: &str, collection: &str) -> Vec<Value> {
        crate::graphql::rows::<Value>(&node.execute(query).await, collection).unwrap()
    }

    #[tokio::test]
    async fn a_created_document_runs_the_plugin_and_its_output_starts_the_next_stage() {
        let (_, record) = crate::plugin::tests::executor::installed_echo();
        let (node, _home) = run_echo_binding(&record.digest).await;

        let echoed = rows(&node, "{ Echoed { job_run text run_ref } }", "Echoed").await;
        assert_eq!(
            echoed,
            vec![json!({"job_run": "run-7", "text": "hello", "run_ref": "run-7"})]
        );
        let invocations = rows(
            &node,
            "{ CallbackInvocation { invocation_id lifecycle_state action_journal } }",
            "CallbackInvocation",
        )
        .await;
        assert_eq!(invocations.len(), 1);
        assert_eq!(invocations[0]["lifecycle_state"], LIFECYCLE_SUCCEEDED);
        let results = rows(
            &node,
            "{ CallbackResult { invocation_id binding_id caused_by_correlation } }",
            "CallbackResult",
        )
        .await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["invocation_id"], invocations[0]["invocation_id"]);
        assert_eq!(results[0]["binding_id"], "bind-echo");
        assert_eq!(results[0]["caused_by_correlation"], "run-7");
    }

    #[tokio::test]
    async fn another_artifact_under_the_same_name_is_denied_and_writes_nothing() {
        let (node, _home) = run_echo_binding(&format!("sha256:{}", "0".repeat(64))).await;
        assert!(rows(&node, "{ Echoed { run_ref } }", "Echoed")
            .await
            .is_empty());
        let invocations = rows(
            &node,
            "{ CallbackInvocation { lifecycle_state error } }",
            "CallbackInvocation",
        )
        .await;
        assert_eq!(
            invocations[0]["lifecycle_state"],
            super::super::LIFECYCLE_DENIED
        );
        assert!(invocations[0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("not the pinned")));
        assert!(
            rows(&node, "{ CallbackResult { result_id } }", "CallbackResult")
                .await
                .is_empty()
        );
    }
}
