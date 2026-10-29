use gents::config_client::{ConfigAccess, GraphqlEndpoint};
use std::collections::BTreeMap;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::manifest::SourceEdgeExpectation;
use super::results::graphql_rows;
use super::stages::StageResult;
use super::tool_calls::canonical_stage_tool_rows;
use gents::graphql::{escape_graphql_string, validate_collection_identifier};
use gents_protocol::client_protocol::RequestLifecycleState;
use gents_protocol::output::TerminalOutput;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::transcript::present_message;

#[derive(Debug, Clone)]
pub(super) struct StageProvenance {
    pub(super) request_id: String,
    pub(super) request_doc_id: String,
    pub(super) rendered_request_count: usize,
    pub(super) request_commit_cids: Vec<String>,
    pub(super) request_fact_counts: BTreeMap<String, usize>,
    pub(super) terminal_output: String,
    pub(super) signer_identity: String,
}

#[derive(Debug, Clone)]
pub(super) struct SourceEdgeEvidence {
    pub(super) producer_trigger_id: String,
    pub(super) producer_request_id: String,
    pub(super) producer_request_doc_id: String,
    pub(super) producer_tool_name: String,
    pub(super) producer_tool_call_doc_id: String,
    pub(super) source_collection: String,
    pub(super) source_doc_id: String,
    pub(super) source_commit_cids: Vec<String>,
    pub(super) consumer_trigger_id: String,
    pub(super) consumer_request_id: String,
    pub(super) consumer_request_doc_id: String,
}

async fn composite_commits(graphql: &GraphqlEndpoint, doc_id: &str) -> Result<Vec<Value>> {
    let query = format!(
        r#"query {{
            _commits(docID: "{}") {{
                cid
                fieldName
                signature {{ identity type }}
            }}
        }}"#,
        escape_graphql_string(doc_id),
    );
    Ok(graphql_rows(graphql, "_commits", &query)
        .await?
        .into_iter()
        .filter(|commit| commit.get("fieldName").and_then(Value::as_str) == Some("_C"))
        .collect())
}

fn commit_has_signer(commit: &Value, signer_identity: &str) -> bool {
    commit
        .pointer("/signature/identity")
        .and_then(Value::as_str)
        == Some(signer_identity)
}

pub(super) fn require_signed_commits(
    collection: &str,
    doc_id: &str,
    commits: &[Value],
    signer_identity: &str,
) -> Result<()> {
    if commits.is_empty() {
        bail!("{collection} {doc_id} has no composite commits");
    }
    if let Some(unsigned) = commits
        .iter()
        .find(|commit| !commit_has_signer(commit, signer_identity))
    {
        bail!(
            "{collection} {doc_id} commit {} was not signed by the node identity",
            unsigned
                .get("cid")
                .and_then(Value::as_str)
                .unwrap_or("(unknown)")
        );
    }
    Ok(())
}

async fn verify_request_fact_collection(
    graphql: &GraphqlEndpoint,
    stage: &StageResult,
    request_doc_id: &str,
    signer_identity: &str,
    collection: &str,
    required: bool,
    extra_fields: &str,
) -> Result<Vec<Value>> {
    let query = format!(
        r#"{{ {collection}(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{
            _docID
            request_doc_id
            {extra_fields}
        }} }}"#,
        escape_graphql_string(request_doc_id),
    );
    let rows = graphql_rows(graphql, collection, &query).await?;
    if required && rows.is_empty() {
        bail!(
            "completed request {} has no durable {collection} facts",
            stage.request_id
        );
    }

    for row in &rows {
        let doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .with_context(|| format!("{collection} provenance query returned no _docID"))?;
        if row.get("request_doc_id").and_then(Value::as_str) != Some(request_doc_id) {
            bail!("{collection} {doc_id} does not point to AgentRequest {request_doc_id}");
        }
        let commits = composite_commits(graphql, doc_id).await?;
        require_signed_commits(collection, doc_id, &commits, signer_identity)?;
    }
    Ok(rows)
}

pub(super) async fn verify_stage_provenance(
    graphql: &GraphqlEndpoint,
    stage: &StageResult,
    signer_identity: &str,
    require_tool_call: bool,
) -> Result<StageProvenance> {
    let request_query = format!(
        r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
        escape_graphql_string(&stage.request_id),
    );
    let request_rows = graphql_rows(graphql, "AgentRequest", &request_query).await?;
    if request_rows.len() != 1 {
        bail!(
            "request {} resolved to {} AgentRequest documents; provenance requires exactly one",
            stage.request_id,
            request_rows.len()
        );
    }
    let request_doc_id = request_rows[0]
        .get("_docID")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .context("AgentRequest provenance query returned no _docID")?
        .to_string();
    let request_commits = composite_commits(graphql, &request_doc_id).await?;
    require_signed_commits(
        "AgentRequest",
        &request_doc_id,
        &request_commits,
        signer_identity,
    )?;

    let rendered_query = format!(
        r#"{{ RenderedRequest(filter: {{ request_doc_id: {{ _eq: "{}" }} }}) {{
            _docID
            request_doc_id
            request_commit_cid
        }} }}"#,
        escape_graphql_string(&request_doc_id),
    );
    let rendered_rows = graphql_rows(graphql, "RenderedRequest", &rendered_query).await?;
    if rendered_rows.is_empty() {
        bail!(
            "request {} completed without a durable RenderedRequest",
            stage.request_id
        );
    }

    let mut request_commit_cids = Vec::with_capacity(rendered_rows.len());
    for row in &rendered_rows {
        let rendered_doc_id = row
            .get("_docID")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("RenderedRequest provenance query returned no _docID")?;
        if row.get("request_doc_id").and_then(Value::as_str) != Some(&request_doc_id) {
            bail!(
                "RenderedRequest {rendered_doc_id} does not point to AgentRequest {request_doc_id}"
            );
        }
        let request_commit_cid = row
            .get("request_commit_cid")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .with_context(|| {
                format!("RenderedRequest {rendered_doc_id} has no exact request commit CID")
            })?;
        let Some(request_commit) = request_commits
            .iter()
            .find(|commit| commit.get("cid").and_then(Value::as_str) == Some(request_commit_cid))
        else {
            bail!(
                "RenderedRequest {rendered_doc_id} pins unknown AgentRequest commit {request_commit_cid}"
            );
        };
        if !commit_has_signer(request_commit, signer_identity) {
            bail!("AgentRequest commit {request_commit_cid} was not signed by the node identity");
        }
        let rendered_commits = composite_commits(graphql, rendered_doc_id).await?;
        require_signed_commits(
            "RenderedRequest",
            rendered_doc_id,
            &rendered_commits,
            signer_identity,
        )?;
        request_commit_cids.push(request_commit_cid.to_string());
    }
    request_commit_cids.sort();
    request_commit_cids.dedup();

    let terminal_query = format!(
        r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{
            _docID
            request_id
            node_did
            requester_did
            session_id
            lifecycle_state
            failure_reason
            terminal_output
        }} }}"#,
        escape_graphql_string(&request_doc_id),
    );
    let terminal_rows = graphql_rows(graphql, "AgentRequest", &terminal_query).await?;
    anyhow::ensure!(
        terminal_rows.len() == 1,
        "request {} resolved to {} terminal AgentRequest documents; expected exactly one",
        stage.request_id,
        terminal_rows.len()
    );
    let request: AgentRequestRow = serde_json::from_value(terminal_rows[0].clone())
        .context("decoding canonical terminal AgentRequest")?;
    if !request.is_terminal() {
        bail!(
            "request {} did not reach a terminal lifecycle state",
            stage.request_id
        );
    }
    let selection = request.terminal_output.as_ref().with_context(|| {
        format!(
            "terminal request {} is missing AgentRequest.terminal_output; \
             a terminal row without the selection is incomplete, not NoMessage",
            stage.request_id
        )
    })?;
    let terminal_output = match selection {
        TerminalOutput::Message { message_doc_id } => {
            let access = ConfigAccess::Graphql(graphql.clone());
            let observation = gents::session::observe_request_output(&access, &request)
                .await
                .context("resolving the selected canonical terminal output")?;
            let gents::session::CanonicalRequestOutput::TerminalMessage {
                message: native, ..
            } = observation
            else {
                bail!(
                    "selected canonical terminal output for request {} is not reconstructable: {observation:?}",
                    stage.request_id
                );
            };
            let presentation = present_message(&native);
            anyhow::ensure!(
                presentation.has_visible_body() || presentation.has_tool_calls,
                "selected canonical terminal output for request {} reconstructed an empty assistant body",
                stage.request_id
            );
            format!("message:{message_doc_id}")
        }
        TerminalOutput::NoMessage => {
            if stage.lifecycle_state == RequestLifecycleState::Completed {
                bail!(
                    "completed request {} selected TerminalOutput::NoMessage but the \
                     stage assertion requires durable signed output; NoMessage is \
                     preserved as an explicit absence, not satisfied here",
                    stage.request_id
                );
            }
            "no_message".to_string()
        }
    };

    let mut request_fact_counts = BTreeMap::new();
    for (collection, required, extra_fields) in [
        ("AgentMessage", true, "role"),
        ("AgentOutputSegment", true, ""),
        ("InferenceCall", true, "call_state"),
        ("AgentToolCall", require_tool_call, "tool_name"),
        ("CompactionEntry", false, "summary"),
    ] {
        let rows = verify_request_fact_collection(
            graphql,
            stage,
            &request_doc_id,
            signer_identity,
            collection,
            required,
            extra_fields,
        )
        .await?;
        match collection {
            "AgentMessage" if terminal_output == "no_message" => {}
            "AgentMessage"
                if !rows.iter().any(|row| {
                    row.get("role").and_then(Value::as_str) == Some("assistant")
                        && row
                            .get("_docID")
                            .and_then(Value::as_str)
                            .is_some_and(|doc_id| {
                                terminal_output
                                    .strip_prefix("message:")
                                    .is_some_and(|selected| selected == doc_id)
                            })
                }) =>
            {
                bail!(
                    "completed request {} has no canonical assistant AgentMessage header \
                     matching its selected terminal output",
                    stage.request_id
                );
            }
            _ => {}
        }
        request_fact_counts.insert(collection.to_string(), rows.len());
    }

    Ok(StageProvenance {
        request_id: stage.request_id.clone(),
        request_doc_id,
        rendered_request_count: rendered_rows.len(),
        request_commit_cids,
        request_fact_counts,
        terminal_output,
        signer_identity: signer_identity.to_string(),
    })
}

fn created_doc_reference<'a>(result: &'a str, collection: &str) -> Option<&'a str> {
    let mut parts = result.split_whitespace();
    if parts.next() != Some("created") || parts.next() != Some(collection) {
        return None;
    }
    let doc_id = parts.next().filter(|value| !value.is_empty())?;
    parts.next().is_none().then_some(doc_id)
}

fn stage_for_trigger<'a>(stages: &'a [StageResult], trigger_id: &str) -> Result<&'a StageResult> {
    let mut matching = stages.iter().filter(|stage| stage.trigger_id == trigger_id);
    let stage = matching
        .next()
        .with_context(|| format!("source edge trigger {trigger_id} produced no stage"))?;
    if matching.next().is_some() {
        bail!("source edge trigger {trigger_id} produced multiple stages");
    }
    Ok(stage)
}

fn provenance_for_stage<'a>(
    provenance: &'a [StageProvenance],
    stage: &StageResult,
) -> Result<&'a StageProvenance> {
    provenance
        .iter()
        .find(|evidence| evidence.request_id == stage.request_id)
        .with_context(|| {
            format!(
                "source edge stage {} has no signed request provenance",
                stage.request_id
            )
        })
}

pub(super) async fn verify_source_edges(
    graphql: &GraphqlEndpoint,
    expected_edges: &[SourceEdgeExpectation],
    stages: &[StageResult],
    provenance: &[StageProvenance],
    signer_identity: &str,
) -> Result<Vec<SourceEdgeEvidence>> {
    let mut evidence = Vec::with_capacity(expected_edges.len());
    for expected in expected_edges {
        validate_collection_identifier(&expected.source_collection).with_context(|| {
            format!(
                "source edge {} -> {} has invalid source collection",
                expected.producer_trigger_id, expected.consumer_trigger_id
            )
        })?;
        let producer = stage_for_trigger(stages, &expected.producer_trigger_id)?;
        let consumer = stage_for_trigger(stages, &expected.consumer_trigger_id)?;
        let producer_provenance = provenance_for_stage(provenance, producer)?;
        let consumer_provenance = provenance_for_stage(provenance, consumer)?;
        let source_doc_id = consumer
            .caused_by_source_doc_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .with_context(|| {
                format!(
                    "consumer request {} records no caused_by_source_doc_id",
                    consumer.request_id
                )
            })?;

        let tool_rows = canonical_stage_tool_rows(graphql, &producer.request_id).await?;
        let matching_tool_rows = tool_rows
            .iter()
            .filter(|row| {
                row.get("request_doc_id").and_then(Value::as_str)
                    == Some(producer_provenance.request_doc_id.as_str())
                    && row.get("tool_name").and_then(Value::as_str)
                        == Some(expected.producer_tool_name.as_str())
                    && row
                        .get("result")
                        .and_then(Value::as_str)
                        .and_then(|result| {
                            created_doc_reference(result, &expected.source_collection)
                        })
                        == Some(source_doc_id)
            })
            .collect::<Vec<_>>();
        if matching_tool_rows.len() != 1 {
            bail!(
                "producer request {} has {} {} results referencing {} {}",
                producer.request_id,
                matching_tool_rows.len(),
                expected.producer_tool_name,
                expected.source_collection,
                source_doc_id
            );
        }
        let tool_row = matching_tool_rows[0];
        let tool_call_doc_id = tool_row
            .get("_docID")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("source-edge AgentToolCall returned no _docID")?;
        if tool_row.get("request_doc_id").and_then(Value::as_str)
            != Some(producer_provenance.request_doc_id.as_str())
        {
            bail!(
                "AgentToolCall {tool_call_doc_id} does not point to producer AgentRequest {}",
                producer_provenance.request_doc_id
            );
        }
        let tool_commits = composite_commits(graphql, tool_call_doc_id).await?;
        require_signed_commits(
            "AgentToolCall",
            tool_call_doc_id,
            &tool_commits,
            signer_identity,
        )?;

        let source_query = format!(
            r#"{{ {}(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
            expected.source_collection,
            escape_graphql_string(source_doc_id),
        );
        let source_rows = graphql_rows(graphql, &expected.source_collection, &source_query).await?;
        if source_rows.len() != 1
            || source_rows[0].get("_docID").and_then(Value::as_str) != Some(source_doc_id)
        {
            bail!(
                "consumer request {} points to {} {}, which resolved to {} documents",
                consumer.request_id,
                expected.source_collection,
                source_doc_id,
                source_rows.len()
            );
        }
        let source_commits = composite_commits(graphql, source_doc_id).await?;
        require_signed_commits(
            &expected.source_collection,
            source_doc_id,
            &source_commits,
            signer_identity,
        )?;
        let mut source_commit_cids = source_commits
            .iter()
            .filter_map(|commit| commit.get("cid").and_then(Value::as_str))
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        source_commit_cids.sort();
        source_commit_cids.dedup();

        evidence.push(SourceEdgeEvidence {
            producer_trigger_id: producer.trigger_id.clone(),
            producer_request_id: producer.request_id.clone(),
            producer_request_doc_id: producer_provenance.request_doc_id.clone(),
            producer_tool_name: expected.producer_tool_name.clone(),
            producer_tool_call_doc_id: tool_call_doc_id.to_string(),
            source_collection: expected.source_collection.clone(),
            source_doc_id: source_doc_id.to_string(),
            source_commit_cids,
            consumer_trigger_id: consumer.trigger_id.clone(),
            consumer_request_id: consumer.request_id.clone(),
            consumer_request_doc_id: consumer_provenance.request_doc_id.clone(),
        });
    }
    Ok(evidence)
}
