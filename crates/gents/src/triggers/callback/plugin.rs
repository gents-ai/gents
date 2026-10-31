//! A callback whose handler is an installed plugin.
//!
//! The plugin's single JSON result becomes output documents whose correlation
//! is copied from the source by the host, never authored by the plugin. The
//! outputs, the invocation's success and its `CallbackResult` commit in one
//! transaction, so a result is visible only once every output is written; an
//! invocation found already executing on recovery fails and is never run twice.
//!
//! A stage binds the path its source names with no working folder and nobody
//! to ask, so the path is reachable only inside a folder the operator allowed
//! (`gents plugin dirs add`, or a pack scenario's `allowed_folders`), at the
//! access that call asks for.

use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;
use gents_protocol::job::DataWrite;
use serde_json::{Map, Value};

use super::documents::{
    create_callback_result_mutation, update_invocation_mutation, CallbackInvocationDoc,
    CallbackResultDoc,
};
use super::run::{deny, encode_journal, persist_journal};
use super::{LIFECYCLE_FAILED, LIFECYCLE_RUNNING, LIFECYCLE_SUCCEEDED};
use crate::documents::{PortCardinality, PortSpec};
use crate::plugin::data::{DataCaller, PluginData, StampContext};
use crate::plugin::executor::PluginExecutor;
use crate::plugin::PluginVerdict;
use crate::tools::workspace::journal::advance;
use crate::tools::workspace::{ActionJournalEntry, ActionJournalState};

/// A plugin writes only a pack's own collections: never the runtime's
/// documents and never a protected collection.
pub(crate) fn writable_output_collection(collection: &str) -> Result<()> {
    crate::store::graphql::validate_collection_identifier(collection)?;
    anyhow::ensure!(
        !gents_protocol::schemas::ALL_COLLECTION_NAMES.contains(&collection),
        "a plugin cannot write {collection}, which belongs to the runtime"
    );
    crate::documents::reject_protected_collection_name(collection)
}

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
        crate::store::graphql::validate_graphql_name(field)?;
    }
    let mut names = std::collections::BTreeSet::new();
    for output in outputs {
        writable_output_collection(&output.collection)?;
        crate::store::graphql::validate_graphql_name(&output.correlation_field)?;
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

/// The documents `output` writes, as `(collection, document)` pairs; several
/// outputs take an object keyed by output name, and each document gets
/// `correlation` in its correlation field, which the plugin may not set.
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

fn take_writes(output: &mut Value) -> Result<Vec<DataWrite>, String> {
    let Some(writes) = output.as_object_mut().and_then(|o| o.remove("writes")) else {
        return Ok(Vec::new());
    };
    serde_json::from_value(writes).map_err(|error| {
        format!(
            "plugin writes must be [{{op, collection, doc_id?, fields, on_conflict?}}]: {error}"
        )
    })
}

pub(super) async fn execute(
    node: &std::sync::Arc<EmbeddedNode>,
    invocation: &mut CallbackInvocationDoc,
    handler: PluginHandler<'_>,
    source: &Value,
    plugins: &PluginExecutor,
    journal: Vec<ActionJournalEntry>,
    data_node: Option<&Arc<EmbeddedNode>>,
) -> Result<()> {
    if let Some(journal) = crate::tools::workspace::recover_running(&journal) {
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
    let input = crate::triggers::callback::documents::strip_secret_fields(source.clone());
    let bound = match plugins
        .bind_input(
            &record,
            &input,
            &crate::plugin::executor::BindContext::headless(None),
        )
        .await
    {
        Ok(bound) => bound,
        Err(reason) => return deny(node, invocation, &reason).await,
    };
    let mut journal = journal;
    advance(&mut journal, 0, ActionJournalState::Validated);
    advance(&mut journal, 0, ActionJournalState::Executing);
    persist_journal(node, invocation, &journal, LIFECYCLE_RUNNING, None).await?;

    // The invocation carries the correlation it was caused by; a grouped
    // delivery's input is an array, so the source itself cannot say it.
    let correlation = invocation.caused_by_correlation.clone().or_else(|| {
        handler
            .correlation_field
            .and_then(|field| source.get(field))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let grant = record.data_grant();
    let caller = grant
        .as_ref()
        .and(data_node)
        .and_then(|node| DataCaller::new(node.clone(), &invocation.owner_node_did).ok());
    let called = plugins.call_bound_data(&record, input, bound, caller).await;
    let failure = match called {
        Err(error) => Some(format!("{error:#}")),
        Ok(call) if call.outcome.verdict != PluginVerdict::Success => Some(format!(
            "plugin {} did not return a result: {}",
            call.coordinate, call.outcome.diagnostics
        )),
        Ok(mut call) => {
            let writes = match grant {
                Some(_) => take_writes(&mut call.outcome.output),
                None => Ok(Vec::new()),
            };
            match writes.and_then(|writes| {
                output_documents(
                    &call.outcome.output,
                    handler.outputs,
                    correlation.as_deref(),
                )
                .map(|documents| (writes, documents))
            }) {
                Err(reason) => Some(format!("plugin {}: {reason}", call.coordinate)),
                Ok((writes, documents)) => {
                    let writes = grant.filter(|_| !writes.is_empty()).map(|g| (g, writes));
                    let mut written = journal.clone();
                    advance(&mut written, 0, ActionJournalState::EffectObserved);
                    advance(&mut written, 0, ActionJournalState::ResultDocsWritten);
                    match commit_success(node, invocation, &written, documents, writes, correlation)
                        .await
                    {
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

async fn commit_success(
    node: &std::sync::Arc<EmbeddedNode>,
    invocation: &mut CallbackInvocationDoc,
    journal: &[ActionJournalEntry],
    documents: Vec<(String, Value)>,
    writes: Option<(PluginData, Vec<DataWrite>)>,
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
        owner_node_did: invocation.owner_node_did.clone(),
        workspace_id: None,
        work_unit_id: None,
        caused_by_correlation: correlation,
        created_at: None,
    });
    let creates = documents
        .iter()
        .map(|(collection, document)| {
            crate::store::graphql::validate_collection_identifier(collection)?;
            let input = gents_protocol::graphql::graphql_input_literal(document)?;
            Ok(format!(
                "mutation {{ create_{collection}(input: {input}) {{ _docID }} }}"
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let invocation_id = invocation.invocation_id.clone();
    let stamps = StampContext::Callback {
        node_did: invocation.owner_node_did.clone(),
    };
    crate::store::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "callback.plugin_result",
        |txn| {
            let creates = &creates;
            let update = &update;
            let result = &result;
            let invocation_id = &invocation_id;
            let writes = &writes;
            let stamps = &stamps;
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
                if let Some((grant, writes)) = writes {
                    crate::plugin::application_write::apply_plugin_writes_in_txn(
                        txn,
                        grant,
                        stamps,
                        writes.clone(),
                    )
                    .await?;
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

    async fn run_echo_binding(digest: &str) -> (std::sync::Arc<EmbeddedNode>, tempfile::TempDir) {
        run_binding(digest, None, false, "").await
    }

    async fn run_binding(
        digest: &str,
        allowed: Option<&std::path::Path>,
        bind: bool,
        job_path: &str,
    ) -> (std::sync::Arc<EmbeddedNode>, tempfile::TempDir) {
        let binding = bind.then(|| crate::pack::PluginDirBinding {
            input_field: "path".into(),
            original_field: Some("origin".into()),
            description: "a directory".into(),
            access: Default::default(),
            write_fields: Vec::new(),
        });
        run_plugin_binding(
            crate::plugin::tests::executor::installed_echo(),
            digest,
            binding,
            allowed.map(|path| (path, crate::pack::BindAccess::Read)),
            job_path,
        )
        .await
    }

    async fn run_plugin_binding(
        (home, mut record): (tempfile::TempDir, crate::plugin::store::InstalledPlugin),
        digest: &str,
        binding: Option<crate::pack::PluginDirBinding>,
        allowed: Option<(&std::path::Path, crate::pack::BindAccess)>,
        job_path: &str,
    ) -> (std::sync::Arc<EmbeddedNode>, tempfile::TempDir) {
        if binding.is_some() {
            record.declaration.input_schema = serde_json::json!({
                "type": "object",
                "properties": {
                    "job_run": {"type": "string"},
                    "text": {"type": "string"},
                    "path": {"type": "string"},
                    "origin": {"type": "string"},
                },
            });
            record.declaration.bind_dir = binding;
            record.declaration.validate().unwrap();
            crate::plugin::store::write_record(home.path(), &record).unwrap();
        }
        if let Some((allowed, access)) = allowed {
            crate::plugin::allowed::add(home.path(), allowed, access).unwrap();
        }
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(node.as_ref()).await.unwrap();
        node.add_collection(
            "type Job { job_run: String text: String path: String origin: String }
             type Echoed { job_run: String text: String path: String origin: String run_ref: String data_calls: Boolean }
             type PluginNote { body: String }
             type PluginOwned { body: String owner: String }
             type PluginJobbed { body: String job_row_doc_id: String }",
        )
        .await
        .unwrap();
        let owner = "did:key:zPluginOwner";
        let setup = format!(
            r#"mutation {{
                create_Callback(input: {{callback_id:"cb-echo",node_did:"{owner}",enabled:true,
                    handler:{{kind:"plugin",plugin:"team/plugin",digest:"{digest}",correlation_field:"job_run",
                        outputs:[{{name:"echoed",collection:"Echoed",schema:"Echoed/v1",correlation_field:"run_ref",cardinality:"one",required:true}}]}}}}) {{_docID}}
                create_EventSource(input: {{event_source_id:"jobs",node_did:"{owner}",source_collection:"Job",event_kind:"created"}}) {{_docID}}
                create_CallbackBinding(input: {{binding_id:"bind-echo",node_did:"{owner}",event_source_id:"jobs",callback_id:"cb-echo",input_fields:["job_run","text","path","origin"],enabled:true}}) {{_docID}}
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
            .execute(&format!(
                r#"mutation {{ create_Job(input: {{job_run: "run-7", text: "hello", path: {}}}) {{ _docID }} }}"#,
                serde_json::to_string(job_path).unwrap()
            ))
            .await;
        assert!(!created.has_errors(), "{:?}", created.errors);
        engine.deliver_arrivals(Some("Job")).await;
        (node, home)
    }

    async fn rows(node: &EmbeddedNode, query: &str, collection: &str) -> Vec<Value> {
        crate::store::graphql::rows::<Value>(&node.execute(query).await, collection).unwrap()
    }

    #[tokio::test]
    async fn a_created_document_runs_the_plugin_and_its_output_starts_the_next_stage() {
        let (_, record) = crate::plugin::tests::executor::installed_echo();
        let (node, _home) = run_echo_binding(&record.digest).await;

        let echoed = rows(&node, "{ Echoed { job_run text path run_ref } }", "Echoed").await;
        assert_eq!(
            echoed,
            vec![json!({"job_run": "run-7", "text": "hello", "path": "", "run_ref": "run-7"})]
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

    async fn only_invocation(node: &EmbeddedNode) -> Value {
        let mut invocations = rows(
            node,
            "{ CallbackInvocation { lifecycle_state error } }",
            "CallbackInvocation",
        )
        .await;
        assert_eq!(invocations.len(), 1);
        invocations.remove(0)
    }

    /// A plugin that, while it is offered `service`, returns `canned` (an
    /// `Echoed` document and its `writes`) instead of echoing its input, and
    /// declares `data` over the three plugin collections of the test node.
    fn data_plugin(
        service: &str,
        canned: &Value,
    ) -> (tempfile::TempDir, crate::plugin::store::InstalledPlugin) {
        use gents_protocol::job::{PluginDataDeclaration, StampedField};
        let (home, mut record) = crate::plugin::tests::executor::installed_plugin(
            &crate::plugin::tests::executor::asking_plugin_wat(
                service,
                "data_results",
                canned,
                true,
            ),
            None,
        );
        record.declaration.data = Some(PluginDataDeclaration {
            collections: vec![
                "PluginNote".into(),
                "PluginOwned".into(),
                "PluginJobbed".into(),
            ],
            ops: vec!["query".into(), "insert".into()],
            stamped: [
                ("owner".to_owned(), StampedField::NodeDid),
                ("job_row_doc_id".to_owned(), StampedField::JobRowDocId),
            ]
            .into(),
        });
        record.data_collection_fields = [
            ("PluginNote", vec!["body"]),
            ("PluginOwned", vec!["body", "owner"]),
            ("PluginJobbed", vec!["body", "job_row_doc_id"]),
        ]
        .into_iter()
        .map(|(name, fields)| {
            (
                name.to_owned(),
                fields.into_iter().map(str::to_owned).collect(),
            )
        })
        .collect();
        crate::plugin::store::write_record(home.path(), &record).unwrap();
        (home, record)
    }

    fn echoed_with(text: &str, writes: Value) -> Value {
        json!({"job_run": "run-7", "text": text, "writes": writes})
    }

    fn insert(collection: &str) -> Value {
        json!({"op": "insert", "collection": collection, "fields": {"body": "x"}})
    }

    async fn run_data_plugin(
        service: &str,
        canned: Value,
    ) -> (std::sync::Arc<EmbeddedNode>, tempfile::TempDir) {
        let installed = data_plugin(service, &canned);
        let digest = installed.1.digest.clone();
        run_plugin_binding(installed, &digest, None, None, "").await
    }

    #[tokio::test]
    async fn callback_write_commits_atomically_with_its_callback_result() {
        let (node, _home) = run_data_plugin(
            "data_calls",
            echoed_with(
                "offered",
                json!([insert("PluginNote"), insert("PluginOwned")]),
            ),
        )
        .await;
        assert_eq!(
            only_invocation(&node).await["lifecycle_state"],
            LIFECYCLE_SUCCEEDED
        );
        let echoed = rows(&node, "{ Echoed { text } }", "Echoed").await;
        assert_eq!(echoed, vec![json!({"text": "offered"})]);
        assert_eq!(
            rows(&node, "{ PluginNote { body } }", "PluginNote").await,
            vec![json!({"body": "x"})]
        );
        assert_eq!(
            rows(&node, "{ PluginOwned { body owner } }", "PluginOwned").await,
            vec![json!({"body": "x", "owner": "did:key:zPluginOwner"})]
        );
        assert_eq!(
            rows(&node, "{ CallbackResult { result_id } }", "CallbackResult")
                .await
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn callback_job_only_stamp_refuses_and_commits_nothing() {
        let (node, _home) = run_data_plugin(
            "data_calls",
            echoed_with(
                "offered",
                json!([insert("PluginNote"), insert("PluginJobbed")]),
            ),
        )
        .await;
        let invocation = only_invocation(&node).await;
        assert_eq!(
            invocation["lifecycle_state"],
            super::super::LIFECYCLE_FAILED
        );
        assert!(invocation["error"]
            .as_str()
            .is_some_and(|error| error.contains("has no value in a callback call")));
        assert!(rows(&node, "{ PluginNote { body } }", "PluginNote")
            .await
            .is_empty());
        assert!(rows(&node, "{ PluginJobbed { body } }", "PluginJobbed")
            .await
            .is_empty());
        assert!(rows(&node, "{ Echoed { text } }", "Echoed")
            .await
            .is_empty());
        assert!(
            rows(&node, "{ CallbackResult { result_id } }", "CallbackResult")
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn callback_is_offered_data_calls_but_never_job_calls() {
        let canned = echoed_with("offered", json!([]));
        let (node, _home) = run_data_plugin("data_calls", canned.clone()).await;
        let echoed = rows(&node, "{ Echoed { text } }", "Echoed").await;
        assert_eq!(
            echoed,
            vec![json!({"text": "offered"})],
            "data_calls offered"
        );

        let (node, _home) = run_data_plugin("job_calls", canned).await;
        let echoed = rows(&node, "{ Echoed { text } }", "Echoed").await;
        assert_eq!(
            echoed,
            vec![json!({"text": "hello"})],
            "job_calls never offered"
        );
    }

    #[tokio::test]
    async fn a_bound_plugin_node_reads_the_one_file_its_source_names() {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("a.txt"), "hello").unwrap();
        let digest = crate::plugin::tests::executor::installed_echo().1.digest;
        let file = docs.join("a.txt");
        let (node, _home) = run_binding(&digest, Some(&docs), true, file.to_str().unwrap()).await;
        assert_eq!(
            only_invocation(&node).await["lifecycle_state"],
            LIFECYCLE_SUCCEEDED
        );
        let echoed = rows(&node, "{ Echoed { path origin } }", "Echoed").await;
        assert_eq!(
            echoed[0]["origin"],
            file.canonicalize().unwrap().to_str().unwrap(),
            "the next stage is told the real path, not the link"
        );
        let seen = echoed[0]["path"].as_str().unwrap();
        assert!(seen.ends_with("/a.txt"), "{seen}");
        assert_ne!(
            seen,
            file.canonicalize().unwrap().to_str().unwrap(),
            "the plugin sees the file alone, through a private folder"
        );
    }

    #[tokio::test]
    async fn a_bound_plugin_node_refuses_a_path_outside_the_allowed_folders_and_never_asks() {
        let root = tempfile::tempdir().unwrap();
        let docs = root.path().join("docs");
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let digest = crate::plugin::tests::executor::installed_echo().1.digest;
        let (node, _home) =
            run_binding(&digest, Some(&docs), true, outside.to_str().unwrap()).await;
        let invocation = only_invocation(&node).await;
        assert_eq!(
            invocation["lifecycle_state"],
            super::super::LIFECYCLE_DENIED
        );
        assert!(invocation["error"]
            .as_str()
            .unwrap_or_default()
            .contains("gents plugin dirs add"));
        assert!(rows(&node, "{ Echoed { run_ref } }", "Echoed")
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn a_plugin_stage_writes_only_where_the_operator_allowed_writing() {
        let writer = || {
            crate::plugin::tests::executor::installed_plugin(
                &crate::plugin::tests::create_file_wat("out.json"),
                None,
            )
        };
        let binding = || crate::pack::PluginDirBinding {
            input_field: "path".into(),
            original_field: None,
            description: "the folder results are written to".into(),
            access: crate::pack::BindAccess::ReadWrite,
            write_fields: vec!["text".into()],
        };
        for (access, writes) in [
            (crate::pack::BindAccess::Read, false),
            (crate::pack::BindAccess::ReadWrite, true),
        ] {
            let out = tempfile::tempdir().unwrap();
            let installed = writer();
            let digest = installed.1.digest.clone();
            let (node, _home) = run_plugin_binding(
                installed,
                &digest,
                Some(binding()),
                Some((out.path(), access)),
                out.path().to_str().unwrap(),
            )
            .await;
            let invocation = only_invocation(&node).await;
            assert_eq!(out.path().join("out.json").exists(), writes, "{access:?}");
            if writes {
                assert_eq!(invocation["lifecycle_state"], LIFECYCLE_SUCCEEDED);
            } else {
                assert_eq!(
                    invocation["lifecycle_state"],
                    super::super::LIFECYCLE_DENIED
                );
                let error = invocation["error"].as_str().unwrap_or_default();
                assert!(
                    error.contains(r#"sets "text""#) && error.contains("--access read_write"),
                    "{error}"
                );
            }
        }
    }
}
