//! Non-interactive pack runs: `gents demo run <pack>`.
//!
//! A pack is a self-contained desired-state root (its own `schemas/` plus the
//! config documents) with an `experiment.json` describing how to drive it.
//!
//! Two orderings are load-bearing and neither is visible from the outside: the
//! pack applies *after* the runtime is ready, so its backend is unprobed for up
//! to one probe interval while the server already reports `serving`; and a seed
//! written before the event source observes its collection is dropped in
//! silence, because triggers are created/first-seen only.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use super::fleet::{spawn_server_with_args, wait_http, wait_runtime_ready};
use super::util::{path_arg, run_cli_json};
use crate::cli::args::DemoRunArgs;
use crate::desired_state::interpolate::interpolate;
use crate::graphql_access::post_graphql;
use gents::graphql::{escape_graphql_string, validate_collection_identifier};

#[derive(Debug, Deserialize)]
struct PackManifest {
    name: String,
    #[serde(default)]
    description: String,
    init: PackInit,
    seed: PackSeed,
    #[serde(default)]
    default_prompt: String,
    expect: PackExpect,
    #[serde(default = "default_timeout")]
    await_timeout_secs: u64,
}

fn default_timeout() -> u64 {
    240
}

#[derive(Debug, Deserialize)]
struct PackInit {
    inference_url: String,
    model_name: String,
    #[serde(default)]
    backend_preset: Option<String>,
    #[serde(default)]
    openai_wire_api: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackSeed {
    collection: String,
    job_id_field: String,
    prompt_field: String,
    #[serde(default)]
    fields: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct PackExpect {
    trigger_ids: Vec<String>,
    #[serde(default)]
    collection_counts: BTreeMap<String, u64>,
    #[serde(default)]
    projections: Vec<String>,
    #[serde(default)]
    signed_provenance: bool,
    #[serde(default)]
    required_tool_call_trigger_ids: Vec<String>,
    #[serde(default)]
    source_edges: Vec<SourceEdgeExpectation>,
    #[serde(default)]
    fan_in: Option<FanInExpectation>,
}

#[derive(Debug, Deserialize)]
struct FanInExpectation {
    member_collection: String,
    result_collection: String,
    report_collection: String,
    correlation_field: String,
    expected_count_field: String,
    consumer_trigger_id: String,
}

#[derive(Debug, Clone)]
struct FanInEvidence {
    correlation: String,
    expected_count: usize,
    member_count: usize,
    result_count: usize,
    consumer_request_id: String,
    report_count: usize,
}

#[derive(Debug, Deserialize)]
struct SourceEdgeExpectation {
    producer_trigger_id: String,
    producer_tool_name: String,
    consumer_trigger_id: String,
    source_collection: String,
}

/// Resolve a pack by path, or by name under `demo/`.
fn resolve_pack(target: &str) -> Result<PathBuf> {
    let direct = PathBuf::from(target);
    if direct.join("experiment.json").is_file() {
        return Ok(direct);
    }
    let under_demo = PathBuf::from("demo").join(target);
    if under_demo.join("experiment.json").is_file() {
        return Ok(under_demo);
    }
    bail!(
        "no pack at {} or {} (a pack is a directory containing experiment.json)",
        direct.display(),
        under_demo.display()
    )
}

fn load_manifest(pack: &Path) -> Result<PackManifest> {
    let path = pack.join("experiment.json");
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let expanded = interpolate(&raw).map_err(|missing| {
        anyhow::anyhow!(
            "{} references unset environment variable(s): {}",
            path.display(),
            missing.join(", ")
        )
    })?;
    let manifest =
        serde_json::from_str(&expanded).with_context(|| format!("parsing {}", path.display()))?;
    validate_manifest(&manifest).with_context(|| format!("validating {}", path.display()))?;
    Ok(manifest)
}

fn validate_manifest(manifest: &PackManifest) -> Result<()> {
    if !manifest.expect.source_edges.is_empty() && !manifest.expect.signed_provenance {
        bail!("expect.source_edges requires expect.signed_provenance=true");
    }
    Ok(())
}

pub(crate) async fn list(root: &Path) -> Result<()> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let entries =
        std::fs::read_dir(root).with_context(|| format!("reading pack root {}", root.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.join("experiment.json").is_file() {
            continue;
        }
        match load_manifest(&path) {
            Ok(manifest) => rows.push((manifest.name, manifest.description)),
            Err(error) => rows.push((
                path.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into(),
                format!("(unreadable: {error})"),
            )),
        }
    }
    rows.sort();
    if rows.is_empty() {
        println!("no packs under {}", root.display());
        return Ok(());
    }
    for (name, description) in rows {
        println!("{name:<20} {description}");
    }
    Ok(())
}

/// Wait for the trigger engine to announce it is watching `collection`.
///
/// This is the real go-signal, and it is strictly later than "serving": the
/// event source only starts observing once the behaviors behind the triggers
/// are runnable, which needs the pack's backend probed. Seeding earlier is
/// silently dropped rather than rejected.
async fn wait_for_event_source(log: &Path, collection: &str, deadline: Duration) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < deadline {
        if let Ok(text) = std::fs::read_to_string(log) {
            if text
                .lines()
                .any(|line| observes_collection(&strip_ansi(line), collection))
            {
                return Ok(());
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    bail!(
        "timed out after {}s waiting for the event source to observe {collection}. \
         The pack's backend may still be unprobed — check {} for \
         'behavior unavailable after runtime reconcile'.",
        deadline.as_secs(),
        log.display()
    )
}

/// The runtime writes coloured tracing output even to a file, which splits
/// `source_collection=Name` with escape sequences. Strip them before matching.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        // CSI: ESC '[' params… final-byte. The '[' introducer is itself inside
        // the final-byte range, so it must be consumed before scanning.
        if chars.peek() == Some(&'[') {
            chars.next();
        }
        for next in chars.by_ref() {
            if ('@'..='~').contains(&next) {
                break;
            }
        }
    }
    out
}

/// Match the observe line for exactly `collection`, so `ExperimentJob` does
/// not satisfy a wait on a collection whose name it prefixes.
fn observes_collection(line: &str, collection: &str) -> bool {
    if !line.contains("event source now observing") {
        return false;
    }
    let needle = format!("source_collection={collection}");
    let Some(idx) = line.find(&needle) else {
        return false;
    };
    line[idx + needle.len()..]
        .chars()
        .next()
        .is_none_or(|next| !next.is_alphanumeric() && next != '_')
}

fn seed_mutation(seed: &PackSeed, job_id: &str, prompt: &str) -> String {
    let mut fields = vec![
        format!(
            "{}: \"{}\"",
            seed.job_id_field,
            escape_graphql_string(job_id)
        ),
        format!(
            "{}: \"{}\"",
            seed.prompt_field,
            escape_graphql_string(prompt)
        ),
    ];
    for (key, value) in &seed.fields {
        fields.push(format!("{key}: \"{}\"", escape_graphql_string(value)));
    }
    format!(
        "mutation {{ create_{}(input: {{ {} }}) {{ _docID }} }}",
        seed.collection,
        fields.join(", ")
    )
}

#[derive(Debug, Clone)]
struct StageResult {
    trigger_id: String,
    request_id: String,
    lifecycle_state: String,
    caused_by_source_doc_id: Option<String>,
}

#[derive(Debug, Clone)]
struct StageProvenance {
    request_id: String,
    request_doc_id: String,
    rendered_request_count: usize,
    request_commit_cids: Vec<String>,
    request_fact_counts: BTreeMap<String, usize>,
    signer_identity: String,
}

#[derive(Debug, Clone)]
struct SourceEdgeEvidence {
    producer_trigger_id: String,
    producer_request_id: String,
    producer_request_doc_id: String,
    producer_tool_name: String,
    producer_tool_call_doc_id: String,
    source_collection: String,
    source_doc_id: String,
    source_commit_cids: Vec<String>,
    consumer_trigger_id: String,
    consumer_request_id: String,
    consumer_request_doc_id: String,
}

async fn graphql_rows(graphql: &str, field: &str, query: &str) -> Result<Vec<Value>> {
    let response = post_graphql(graphql, query).await?;
    if let Some(errors) = response.get("errors").and_then(Value::as_array) {
        if !errors.is_empty() {
            bail!("GraphQL {field} query failed: {errors:?}");
        }
    }
    response
        .pointer(&format!("/data/{field}"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("GraphQL {field} query returned no row array"))
}

async fn composite_commits(graphql: &str, doc_id: &str) -> Result<Vec<Value>> {
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

fn require_signed_commits(
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
    graphql: &str,
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

async fn verify_stage_provenance(
    graphql: &str,
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

    let mut request_fact_counts = BTreeMap::new();
    for (collection, required, extra_fields) in [
        ("AgentResponse", true, "status content reasoning"),
        ("AgentMessage", true, "role content reasoning"),
        ("InferenceCall", true, "call_state"),
        (
            "AgentToolCall",
            require_tool_call,
            "status tool_name result",
        ),
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
            "AgentResponse"
                if !rows.iter().any(|row| {
                    matches!(
                        row.get("status").and_then(Value::as_str),
                        Some("complete" | "completed")
                    )
                }) =>
            {
                bail!(
                    "completed request {} has no terminal AgentResponse",
                    stage.request_id
                );
            }
            "AgentMessage"
                if !rows.iter().any(|row| {
                    row.get("role").and_then(Value::as_str) == Some("assistant")
                        && ["content", "reasoning"].iter().any(|field| {
                            row.get(*field)
                                .and_then(Value::as_str)
                                .is_some_and(|value| !value.trim().is_empty())
                        })
                }) =>
            {
                bail!(
                    "completed request {} has no materialized assistant AgentMessage",
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

async fn verify_source_edges(
    graphql: &str,
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

        let tool_query = format!(
            r#"{{ AgentToolCall(filter: {{
                request_doc_id: {{ _eq: "{}" }},
                tool_name: {{ _eq: "{}" }}
            }}) {{
                _docID
                request_doc_id
                tool_name
                result
            }} }}"#,
            escape_graphql_string(&producer_provenance.request_doc_id),
            escape_graphql_string(&expected.producer_tool_name),
        );
        let tool_rows = graphql_rows(graphql, "AgentToolCall", &tool_query).await?;
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

async fn render_projection_artifacts(
    bin: &Path,
    graphql: &str,
    run_dir: &Path,
    stages: &[StageResult],
    projections: &[String],
) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let artifact_dir = run_dir.join("projections");
    std::fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("creating projection directory {}", artifact_dir.display()))?;
    let mut artifacts = BTreeMap::new();
    for stage in stages {
        let timeline_args = vec![
            "trace".to_string(),
            "timeline".to_string(),
            "--graphql".to_string(),
            graphql.to_string(),
            "--request-id".to_string(),
            stage.request_id.clone(),
        ];
        let timeline = run_cli_json(bin, &timeline_args)
            .await
            .with_context(|| format!("projecting timeline for request {}", stage.request_id))?;
        let timeline_path = artifact_dir.join(format!("{}-timeline.json", stage.request_id));
        std::fs::write(&timeline_path, serde_json::to_vec_pretty(&timeline)?)
            .with_context(|| format!("writing {}", timeline_path.display()))?;

        let mut request_artifacts =
            BTreeMap::from([("timeline".to_string(), path_arg(&timeline_path))]);
        for projection in projections {
            let project_args = vec![
                "trace".to_string(),
                "project".to_string(),
                "--graphql".to_string(),
                graphql.to_string(),
                "--request-id".to_string(),
                stage.request_id.clone(),
                "--projection".to_string(),
                projection.clone(),
            ];
            let projected = run_cli_json(bin, &project_args).await.with_context(|| {
                format!(
                    "rendering {projection} projection for request {}",
                    stage.request_id
                )
            })?;
            let projection_path = artifact_dir.join(format!(
                "{}-{}.json",
                stage.request_id,
                projection.replace('_', "-")
            ));
            std::fs::write(&projection_path, serde_json::to_vec_pretty(&projected)?)
                .with_context(|| format!("writing {}", projection_path.display()))?;
            request_artifacts.insert(projection.clone(), path_arg(&projection_path));
        }
        artifacts.insert(stage.request_id.clone(), request_artifacts);
    }
    Ok(artifacts)
}

async fn await_stages(
    graphql: &str,
    trigger_ids: &[String],
    deadline: Duration,
) -> Result<Vec<StageResult>> {
    let started = Instant::now();
    loop {
        let mut done: Vec<StageResult> = Vec::new();
        for trigger_id in trigger_ids {
            let query = format!(
                r#"{{ AgentRequest(filter: {{ caused_by_trigger_id: {{ _eq: "{}" }} }}) {{ request_id lifecycle_state caused_by_source_doc_id }} }}"#,
                escape_graphql_string(trigger_id)
            );
            let Ok(resp) = post_graphql(graphql, &query).await else {
                continue;
            };
            let Some(rows) = resp.pointer("/data/AgentRequest").and_then(Value::as_array) else {
                continue;
            };
            for row in rows {
                let state = row
                    .get("lifecycle_state")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if matches!(state, "completed" | "failed" | "cancelled") {
                    done.push(StageResult {
                        trigger_id: trigger_id.clone(),
                        request_id: row
                            .get("request_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        lifecycle_state: state.to_string(),
                        caused_by_source_doc_id: row
                            .get("caused_by_source_doc_id")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    });
                    break;
                }
            }
        }
        if done.len() == trigger_ids.len() {
            return Ok(done);
        }
        // A trigger that fired and failed to materialize will never retry:
        // created/first-seen means the source document is already marked seen.
        // Surface its own last_error instead of waiting out the deadline.
        for trigger_id in trigger_ids {
            if done.iter().any(|s| &s.trigger_id == trigger_id) {
                continue;
            }
            if let Some(error) = trigger_error(graphql, trigger_id).await {
                bail!("trigger {trigger_id} fired but did not materialize: {error}");
            }
        }
        if started.elapsed() >= deadline {
            let seen: Vec<&str> = done.iter().map(|s| s.trigger_id.as_str()).collect();
            bail!(
                "timed out after {}s: reached a terminal state for [{}], expected [{}]",
                deadline.as_secs(),
                seen.join(", "),
                trigger_ids.join(", ")
            );
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// The trigger's own `last_error`, when it recorded a failed fire.
async fn trigger_error(graphql: &str, trigger_id: &str) -> Option<String> {
    let query = format!(
        r#"{{ EventTrigger(filter: {{ trigger_id: {{ _eq: "{}" }} }}) {{ last_status last_error }} }}"#,
        escape_graphql_string(trigger_id)
    );
    let resp = post_graphql(graphql, &query).await.ok()?;
    let row = resp.pointer("/data/EventTrigger/0")?;
    if row.get("last_status").and_then(Value::as_str) != Some("error") {
        return None;
    }
    Some(
        row.get("last_error")
            .and_then(Value::as_str)
            .unwrap_or("(no last_error recorded)")
            .to_string(),
    )
}

async fn verify_fan_in(
    graphql: &str,
    expected: &FanInExpectation,
    correlation: &str,
    agent_did: &str,
) -> Result<Option<FanInEvidence>> {
    for collection in [
        &expected.member_collection,
        &expected.result_collection,
        &expected.report_collection,
    ] {
        validate_collection_identifier(collection)?;
    }
    for field in [&expected.correlation_field, &expected.expected_count_field] {
        gents::graphql::validate_graphql_name(field)?;
    }

    let escaped_correlation = escape_graphql_string(correlation);
    let load_members = |collection: &str| {
        format!(
            r#"{{ {collection}(filter: {{ {correlation_field}: {{ _eq: "{escaped_correlation}" }} }}) {{ _docID {correlation_field} {expected_count_field} }} }}"#,
            correlation_field = expected.correlation_field,
            expected_count_field = expected.expected_count_field,
        )
    };
    let member_rows = graphql_rows(
        graphql,
        &expected.member_collection,
        &load_members(&expected.member_collection),
    )
    .await?;
    if member_rows.is_empty() {
        bail!("fan-in produced no {} rows", expected.member_collection);
    }
    let expected_count = member_rows.len();
    for row in &member_rows {
        let count = row
            .get(&expected.expected_count_field)
            .and_then(|value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .or_else(|| value.as_str()?.parse::<usize>().ok())
            })
            .context("fan-in member has no valid expected count")?;
        if count != expected_count {
            bail!(
                "fan-in {} rows disagree with closed-set count: row says {}, actual {}",
                expected.member_collection,
                count,
                expected_count
            );
        }
    }

    let result_rows = graphql_rows(
        graphql,
        &expected.result_collection,
        &load_members(&expected.result_collection),
    )
    .await?;
    if result_rows.len() != expected_count {
        bail!(
            "fan-in expected {} correlated {} rows, found {}",
            expected_count,
            expected.result_collection,
            result_rows.len()
        );
    }
    for row in &result_rows {
        let count = row
            .get(&expected.expected_count_field)
            .and_then(|value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .or_else(|| value.as_str()?.parse::<usize>().ok())
            })
            .context("fan-in result has no valid expected count")?;
        if count != expected_count {
            bail!("fan-in result cardinality snapshot drifted");
        }
    }

    let request_query = format!(
        r#"{{ AgentRequest(filter: {{
            agent_did: {{ _eq: "{}" }},
            caused_by_trigger_id: {{ _eq: "{}" }},
            caused_by_trigger_kind: {{ _eq: "event" }},
            caused_by_correlation: {{ _eq: "{}" }}
        }}) {{ request_id }} }}"#,
        escape_graphql_string(agent_did),
        escape_graphql_string(&expected.consumer_trigger_id),
        escaped_correlation,
    );
    let request_rows = graphql_rows(graphql, "AgentRequest", &request_query).await?;
    if request_rows.len() != 1 {
        bail!(
            "fan-in consumer {} expected exactly one correlated AgentRequest, found {}",
            expected.consumer_trigger_id,
            request_rows.len()
        );
    }
    let consumer_request_id = request_rows[0]
        .get("request_id")
        .and_then(Value::as_str)
        .context("fan-in consumer request has no request_id")?
        .to_string();

    let report_query = format!(
        r#"{{ {collection}(filter: {{ {field}: {{ _eq: "{escaped_correlation}" }} }}) {{ _docID }} }}"#,
        collection = expected.report_collection,
        field = expected.correlation_field,
    );
    let report_rows = graphql_rows(graphql, &expected.report_collection, &report_query).await?;
    if report_rows.len() != 1 {
        bail!(
            "fan-in expected exactly one correlated {}, found {}",
            expected.report_collection,
            report_rows.len()
        );
    }

    Ok(Some(FanInEvidence {
        correlation: correlation.to_string(),
        expected_count,
        member_count: member_rows.len(),
        result_count: result_rows.len(),
        consumer_request_id,
        report_count: report_rows.len(),
    }))
}

async fn count_rows(graphql: &str, collection: &str) -> u64 {
    let query = format!("{{ {collection} {{ _docID }} }}");
    post_graphql(graphql, &query)
        .await
        .ok()
        .and_then(|resp| {
            resp.pointer(&format!("/data/{collection}"))
                .and_then(Value::as_array)
                .map(|rows| rows.len() as u64)
        })
        .unwrap_or(0)
}

async fn token_totals(graphql: &str) -> (u64, u64) {
    let query = "{ InferenceCall { prompt_tokens completion_tokens } }";
    let Ok(resp) = post_graphql(graphql, query).await else {
        return (0, 0);
    };
    let Some(rows) = resp
        .pointer("/data/InferenceCall")
        .and_then(Value::as_array)
    else {
        return (0, 0);
    };
    rows.iter().fold((0, 0), |(p, c), row| {
        (
            p + row
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            c + row
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        )
    })
}

/// Timestamped so `runs/` sorts chronologically and two runs never collide.
fn default_job_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("exp-{secs}")
}

pub(crate) async fn run(args: DemoRunArgs) -> Result<()> {
    let bin = std::env::current_exe().context("resolving the gents binary path")?;
    let pack = resolve_pack(&args.pack)?;
    let manifest = load_manifest(&pack)?;
    let job_id = args.job_id.clone().unwrap_or_else(default_job_id);
    let prompt = args
        .prompt
        .clone()
        .unwrap_or_else(|| manifest.default_prompt.clone());
    if prompt.trim().is_empty() {
        bail!("no prompt: pass --prompt or give the pack a default_prompt");
    }

    // Everything a run produces lands under <pack>/runs/<job_id>/ — home, log,
    // and artifacts together, so a failed run is debuggable from one place.
    let run_dir = pack.join("runs").join(&job_id);
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("creating run directory {}", run_dir.display()))?;

    // A fresh home per run by default. Triggers are first-seen, so a reused
    // home can silently skip a stage whose source rows already existed.
    let owned_home = args.home.is_none();
    let home = args.home.clone().unwrap_or_else(|| run_dir.join("home"));
    if owned_home && home.exists() {
        std::fs::remove_dir_all(&home).ok();
    }
    std::fs::create_dir_all(&home)
        .with_context(|| format!("creating pack home {}", home.display()))?;

    println!("pack     {} ({})", manifest.name, pack.display());
    println!("job_id   {job_id}");
    println!("run dir  {}", run_dir.display());
    println!("endpoint {}", manifest.init.inference_url);
    println!("model    {}", manifest.init.model_name);

    let mut init_args: Vec<String> = vec![
        "init".into(),
        "--home".into(),
        path_arg(&home),
        "--dangerously-overwrite".into(),
        "--inference-url".into(),
        manifest.init.inference_url.clone(),
        "--model-name".into(),
        manifest.init.model_name.clone(),
        "--tool-package".into(),
        "minimal".into(),
    ];
    if let Some(preset) = manifest.init.backend_preset.as_deref() {
        init_args.push("--backend-preset".into());
        init_args.push(preset.into());
    }
    if let Some(wire) = manifest.init.openai_wire_api.as_deref() {
        init_args.push("--openai-wire-api".into());
        init_args.push(wire.into());
    }
    let init = run_cli_json(&bin, &init_args).await?;
    let agent_did = init
        .get("agent_did")
        .and_then(Value::as_str)
        .context("init did not return agent_did")?
        .to_string();

    let port = args.http_port;
    let graphql = format!("http://127.0.0.1:{port}/api/v0/graphql");
    let log = run_dir.join("server.log");
    let started = Instant::now();

    let mut server = spawn_server_with_pack(&bin, &home, port, &log, &pack)?;
    let outcome = async {
        wait_http(&format!("http://127.0.0.1:{port}/healthz"), &mut server).await?;
        wait_runtime_ready(&graphql, &agent_did, &mut server).await?;
        println!(
            "runtime  ready; waiting for the event source to observe {}…",
            manifest.seed.collection
        );
        wait_for_event_source(
            &log,
            &manifest.seed.collection,
            Duration::from_secs(manifest.await_timeout_secs),
        )
        .await?;

        let mutation = seed_mutation(&manifest.seed, &job_id, &prompt);
        post_graphql(&graphql, &mutation)
            .await
            .context("seeding the pack")?;
        println!("seeded   1 {} document", manifest.seed.collection);

        let stages = await_stages(
            &graphql,
            &manifest.expect.trigger_ids,
            Duration::from_secs(manifest.await_timeout_secs),
        )
        .await?;

        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for collection in manifest.expect.collection_counts.keys() {
            counts.insert(collection.clone(), count_rows(&graphql, collection).await);
        }
        let signer_identity = gents::identity::commit_signer_identity_for_did(&agent_did)?;
        let provenance = if manifest.expect.signed_provenance {
            let mut evidence = Vec::with_capacity(stages.len());
            for stage in &stages {
                evidence.push(
                    verify_stage_provenance(
                        &graphql,
                        stage,
                        &signer_identity,
                        manifest
                            .expect
                            .required_tool_call_trigger_ids
                            .contains(&stage.trigger_id),
                    )
                    .await
                    .with_context(|| {
                        format!("verifying signed provenance for {}", stage.request_id)
                    })?,
                );
            }
            evidence
        } else {
            Vec::new()
        };
        let source_edges = verify_source_edges(
            &graphql,
            &manifest.expect.source_edges,
            &stages,
            &provenance,
            &signer_identity,
        )
        .await
        .context("verifying durable source edges")?;
        let fan_in = match manifest.expect.fan_in.as_ref() {
            Some(expected) => verify_fan_in(&graphql, expected, &job_id, &agent_did).await?,
            None => None,
        };
        let projection_artifacts = render_projection_artifacts(
            &bin,
            &graphql,
            &run_dir,
            &stages,
            &manifest.expect.projections,
        )
        .await?;
        let (prompt_tokens, completion_tokens) = token_totals(&graphql).await;
        Ok::<_, anyhow::Error>((
            stages,
            counts,
            provenance,
            source_edges,
            fan_in,
            projection_artifacts,
            prompt_tokens,
            completion_tokens,
        ))
    }
    .await;

    let _ = server.start_kill();

    let (
        stages,
        counts,
        provenance,
        source_edges,
        fan_in,
        projection_artifacts,
        prompt_tokens,
        completion_tokens,
    ) = match outcome {
        Ok(values) => values,
        Err(error) => {
            eprintln!("\nrun failed: {error:#}");
            eprintln!("server log: {}", log.display());
            return Err(error);
        }
    };

    let elapsed = started.elapsed();
    let mut failures: Vec<String> = Vec::new();
    for stage in &stages {
        if stage.lifecycle_state != "completed" {
            failures.push(format!(
                "{} ended {}",
                stage.trigger_id, stage.lifecycle_state
            ));
        }
    }
    for (collection, expected) in &manifest.expect.collection_counts {
        let actual = counts.get(collection).copied().unwrap_or(0);
        if actual < *expected {
            failures.push(format!(
                "{collection}: expected at least {expected}, found {actual}"
            ));
        }
    }

    let meta = json!({
        "pack": manifest.name,
        "job_id": job_id,
        "agent_did": agent_did,
        "endpoint": manifest.init.inference_url,
        "model": manifest.init.model_name,
        "elapsed_secs": elapsed.as_secs(),
        "prompt": prompt,
        "stages": stages.iter().map(|s| json!({
            "trigger_id": s.trigger_id,
            "request_id": s.request_id,
            "lifecycle_state": s.lifecycle_state,
            "caused_by_source_doc_id": s.caused_by_source_doc_id,
        })).collect::<Vec<_>>(),
        "collection_counts": counts,
        "provenance": provenance.iter().map(|evidence| json!({
            "request_id": evidence.request_id,
            "request_doc_id": evidence.request_doc_id,
            "rendered_request_count": evidence.rendered_request_count,
            "request_commit_cids": evidence.request_commit_cids,
            "request_fact_counts": evidence.request_fact_counts,
            "signer_identity": evidence.signer_identity,
        })).collect::<Vec<_>>(),
        "source_edges": source_edges.iter().map(|edge| json!({
            "producer_trigger_id": edge.producer_trigger_id,
            "producer_request_id": edge.producer_request_id,
            "producer_request_doc_id": edge.producer_request_doc_id,
            "producer_tool_name": edge.producer_tool_name,
            "producer_tool_call_doc_id": edge.producer_tool_call_doc_id,
            "source_collection": edge.source_collection,
            "source_doc_id": edge.source_doc_id,
            "source_commit_cids": edge.source_commit_cids,
            "consumer_trigger_id": edge.consumer_trigger_id,
            "consumer_request_id": edge.consumer_request_id,
            "consumer_request_doc_id": edge.consumer_request_doc_id,
        })).collect::<Vec<_>>(),
        "fan_in": fan_in.as_ref().map(|evidence| json!({
            "correlation": evidence.correlation,
            "expected_count": evidence.expected_count,
            "member_count": evidence.member_count,
            "result_count": evidence.result_count,
            "consumer_request_id": evidence.consumer_request_id,
            "report_count": evidence.report_count,
        })),
        "projection_artifacts": projection_artifacts,
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "ok": failures.is_empty(),
        "failures": failures,
    });
    let meta_path = run_dir.join("meta.json");
    if let Ok(text) = serde_json::to_string_pretty(&meta) {
        let _ = std::fs::write(&meta_path, text);
    }

    println!();
    for stage in &stages {
        println!(
            "  {:<12} {:<10} {}",
            stage.trigger_id, stage.lifecycle_state, stage.request_id
        );
    }
    for (collection, actual) in &counts {
        println!("  {collection:<12} {actual} document(s)");
    }
    println!(
        "  tokens       {prompt_tokens} prompt + {completion_tokens} completion in {}s",
        elapsed.as_secs()
    );
    println!("  artifacts    {}", meta_path.display());
    if owned_home && !args.keep_home {
        std::fs::remove_dir_all(&home).ok();
    } else {
        println!("  home         {}", home.display());
    }

    if failures.is_empty() {
        println!("\nok");
        Ok(())
    } else {
        bail!(
            "pack run did not meet expectations: {}",
            failures.join("; ")
        )
    }
}

fn spawn_server_with_pack(
    bin: &Path,
    home: &Path,
    port: u16,
    log: &Path,
    pack: &Path,
) -> Result<tokio::process::Child> {
    let root = path_arg(pack);
    spawn_server_with_args(bin, home, port, log, &["--apply-root", &root])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: tracing colours its file output, so the raw bytes are
    /// `source_collection` ESC `=` ESC `ExperimentJob`. Matching the plain
    /// substring silently never fired and the runner timed out with the
    /// observe line sitting in the log.
    const COLOURED: &str = "\u{1b}[32m INFO\u{1b}[0m \u{1b}[2mgents::trigger_engine::event_source\u{1b}[0m\u{1b}[2m:\u{1b}[0m event source now observing source collection \u{1b}[3msource_collection\u{1b}[0m\u{1b}[2m=\u{1b}[0mExperimentJob \u{1b}[3mgeneration\u{1b}[0m\u{1b}[2m=\u{1b}[0m3";

    #[test]
    fn matches_the_observe_line_through_ansi_colouring() {
        assert!(observes_collection(&strip_ansi(COLOURED), "ExperimentJob"));
    }

    #[test]
    fn does_not_match_a_different_collection() {
        assert!(!observes_collection(
            &strip_ansi(COLOURED),
            "ExperimentFinding"
        ));
    }

    #[test]
    fn does_not_match_a_name_that_merely_prefixes_another() {
        let line = "event source now observing source collection source_collection=ExperimentJobArchive generation=3";
        assert!(!observes_collection(line, "ExperimentJob"));
        assert!(observes_collection(line, "ExperimentJobArchive"));
    }

    #[test]
    fn ignores_unrelated_lines() {
        assert!(!observes_collection(
            "gents behavior started behavior_id=exp-stage1",
            "ExperimentJob"
        ));
    }

    #[test]
    fn signed_fact_gate_requires_every_composite_commit_to_have_the_node_signer() {
        let signer = "did:key:zNode";
        let signed = json!({
            "cid": "bafy-signed",
            "signature": { "identity": signer, "type": "ES256K" }
        });
        let unsigned = json!({ "cid": "bafy-unsigned", "signature": null });

        assert!(require_signed_commits("AgentMessage", "doc-1", &[signed.clone()], signer).is_ok());
        assert!(require_signed_commits("AgentMessage", "doc-1", &[], signer).is_err());
        let error = require_signed_commits("AgentMessage", "doc-1", &[signed, unsigned], signer)
            .unwrap_err()
            .to_string();
        assert!(error.contains("bafy-unsigned"), "{error}");
    }

    #[test]
    fn source_edge_expectations_require_signed_provenance() {
        let manifest = PackManifest {
            name: "invalid-source-edge".to_string(),
            description: String::new(),
            init: PackInit {
                inference_url: "http://127.0.0.1:8080".to_string(),
                model_name: "test".to_string(),
                backend_preset: None,
                openai_wire_api: None,
            },
            seed: PackSeed {
                collection: "Source".to_string(),
                job_id_field: "job_id".to_string(),
                prompt_field: "prompt".to_string(),
                fields: BTreeMap::new(),
            },
            default_prompt: String::new(),
            expect: PackExpect {
                trigger_ids: Vec::new(),
                collection_counts: BTreeMap::new(),
                projections: Vec::new(),
                signed_provenance: false,
                required_tool_call_trigger_ids: Vec::new(),
                source_edges: vec![SourceEdgeExpectation {
                    producer_trigger_id: "producer".to_string(),
                    producer_tool_name: "create_Source".to_string(),
                    consumer_trigger_id: "consumer".to_string(),
                    source_collection: "Source".to_string(),
                }],
                fan_in: None,
            },
            await_timeout_secs: 1,
        };

        let error = validate_manifest(&manifest).expect_err("unsigned source edges must fail");
        assert!(error
            .to_string()
            .contains("source_edges requires expect.signed_provenance=true"));
    }

    #[test]
    fn created_doc_reference_requires_the_exact_collection_and_doc_token() {
        assert_eq!(
            created_doc_reference("created ExperimentFinding bae-source", "ExperimentFinding"),
            Some("bae-source")
        );
        assert_eq!(
            created_doc_reference("created OtherFinding bae-source", "ExperimentFinding"),
            None
        );
        assert_eq!(
            created_doc_reference(
                "created ExperimentFinding bae-source trailing",
                "ExperimentFinding"
            ),
            None
        );
        assert_eq!(
            created_doc_reference(
                "prefix created ExperimentFinding bae-source",
                "ExperimentFinding"
            ),
            None
        );
    }
}
