//! Node-bound adapters for a graph entry's contract.
//!
//! [`prepare_entry_run`] is the generic path: it admits operator input
//! against an entry's `input_schema` and, when the entry declares `prepare`,
//! collects the declared host facts and hands them plus the admitted input
//! to the pack's own plugin, persisting whatever documents it returns.
//!
//! Every adapter here prepares host evidence and durable workspace/config
//! rows for the same [`ConfigAccess`] and principal that own the graph.
//! Model-facing tools and the CLI share this code; neither discovers another
//! Gents home, endpoint, credential, or binary.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use gents_protocol::graphql::graphql_input_literal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config_client::ConfigAccess;
use crate::graph_pipeline::{
    admit_operator_input, EntryInputOrigin, GraphPlan, HostInput, PlannedEntry,
};
use crate::plugin::executor::PluginExecutor;

/// A prepare plugin's returned documents are persisted in batches of at most
/// this many, so one prepare step never renders an unbounded mutation.
const PREPARE_DOCUMENTS_BATCH_LIMIT: usize = 32;
/// ...or this many bytes of rendered mutation text, whichever comes first.
const PREPARE_DOCUMENTS_BATCH_BYTES: usize = 1024 * 1024;

fn git_output_bytes(repo: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .output()
        .with_context(|| format!("running git in {}", repo.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed in {}: {}",
            arguments.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn git_output(repo: &Path, arguments: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git_output_bytes(repo, arguments)?)?
        .trim()
        .to_owned())
}

fn git_output_exact(repo: &Path, arguments: &[&str]) -> Result<String> {
    String::from_utf8(git_output_bytes(repo, arguments)?)
        .context("Git emitted non-UTF-8 diff output")
}

fn resolve_repository(
    repo: &Path,
    base: &str,
    head: &str,
    process_root: Option<&Path>,
) -> Result<(PathBuf, String, String)> {
    let canonical = std::fs::canonicalize(repo)
        .with_context(|| format!("canonicalizing repository {}", repo.display()))?;
    crate::workspace::require_under_ceiling(&canonical, process_root)?;
    if git_output(&canonical, &["rev-parse", "--is-inside-work-tree"])? != "true" {
        anyhow::bail!("{} is not a Git work tree", canonical.display());
    }
    // Git discovers enclosing repositories. A permitted subdirectory must not
    // grant review access to the rest of a repository outside the ceiling.
    let repository_root =
        std::fs::canonicalize(git_output(&canonical, &["rev-parse", "--show-toplevel"])?)?;
    crate::workspace::require_under_ceiling(&repository_root, process_root)?;
    let base_sha = git_output(
        &canonical,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base}^{{commit}}"),
        ],
    )?;
    let head_sha = git_output(
        &canonical,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{head}^{{commit}}"),
        ],
    )?;
    Ok((canonical, base_sha, head_sha))
}

/// Host facts collected for one `git_diff` step: the resolved repository,
/// SHAs, and the exact `git` output shapes a pack plugin builds its evidence
/// from.
struct GitDiffFacts {
    repository: PathBuf,
    base_sha: String,
    head_sha: String,
    name_status: String,
    stat: String,
    patch: String,
}

/// Runs the declared `git diff` at the entry's declared context lines and
/// rename threshold.
fn collect_git_diff(
    repository: &Path,
    base: &str,
    head: &str,
    unified_context_lines: u32,
    rename_similarity_percent: u8,
    host_root: Option<&Path>,
) -> Result<GitDiffFacts> {
    let (repository, base_sha, head_sha) = resolve_repository(repository, base, head, host_root)?;
    let name_status = git_output(
        &repository,
        &["diff", "--name-status", &base_sha, &head_sha, "--"],
    )?;
    let stat = git_output(&repository, &["diff", "--stat", &base_sha, &head_sha, "--"])?;
    let patch = git_output_exact(
        &repository,
        &[
            "-c",
            "core.quotepath=true",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--binary",
            &format!("--find-renames={rename_similarity_percent}%"),
            &format!("--unified={unified_context_lines}"),
            &base_sha,
            &head_sha,
            "--",
        ],
    )?;
    Ok(GitDiffFacts {
        repository,
        base_sha,
        head_sha,
        name_status,
        stat,
        patch,
    })
}

/// What starts a graph run through the generic entry contract: the plan and
/// (optionally) which of its entries, the operator's raw input, the host
/// ceiling a `git_diff`/`read_only_workspace` step may not escape, and the
/// executor that calls the pack's own prepare plugin.
pub struct EntryRunRequest<'a> {
    pub plan: &'a GraphPlan,
    pub entry: Option<&'a str>,
    pub input: Value,
    pub host_root: Option<&'a Path>,
    pub plugins: &'a PluginExecutor,
}

/// The entry input a run is about to start with, and how it got there.
#[derive(Debug)]
pub struct PreparedEntryRun {
    pub entry_name: String,
    pub input: Value,
    pub origin: EntryInputOrigin,
    /// Documents the prepare plugin's evidence was persisted as; `0` when
    /// the entry has no `prepare` step.
    pub documents: usize,
}

/// Selects the entry a run starts from, the one seam every caller (the CLI,
/// `self_config`'s `RunGraphTool`, and this module's own prepare path) goes
/// through, so a ceiling or authority decision made from the selection can
/// never disagree with which entry actually runs.
pub(crate) fn select_entry<'a>(
    plan: &'a GraphPlan,
    requested: Option<&str>,
) -> Result<&'a PlannedEntry> {
    match requested {
        Some(name) => plan
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .with_context(|| format!("graph {:?} has no entry named {name:?}", plan.graph_id)),
        None => match plan.entries.as_slice() {
            [entry] => Ok(entry),
            [] => anyhow::bail!("graph {:?} declares no entry", plan.graph_id),
            entries => anyhow::bail!(
                "graph {:?} has {} entries; name the entry to start (one of: {})",
                plan.graph_id,
                entries.len(),
                entries
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        },
    }
}

/// One document a prepare plugin asked to create, as its stdout names it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparePluginDocument {
    pub(crate) collection: String,
    pub(crate) fields: Value,
}

/// A prepare plugin's whole stdout contract (D4 step 6).
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparePluginOutput {
    pub(crate) input: Value,
    pub(crate) documents: Vec<PreparePluginDocument>,
}

/// The `git_diff` host fact exactly as a prepare plugin's stdin carries it.
fn git_diff_host_json(facts: &GitDiffFacts) -> Value {
    json!({
        "repository": facts.repository.to_string_lossy(),
        "base_sha": facts.base_sha,
        "head_sha": facts.head_sha,
        "name_status": facts.name_status,
        "stat": facts.stat,
        "patch": facts.patch,
    })
}

/// The `read_only_workspace` host fact exactly as a prepare plugin's stdin
/// carries it.
fn read_only_workspace_host_json(workspace_id: &str, owner_agent_did: &str) -> Value {
    json!({
        "workspace_id": workspace_id,
        "owner_agent_did": owner_agent_did,
        "authority": "readOnly",
    })
}

/// A prepare plugin's whole stdin envelope: the admitted operator input, a
/// fresh nonce, and the host facts collected for it.
fn prepare_stdin(input: Value, nonce: &str, host: Value) -> Value {
    json!({
        "input": input,
        "nonce": nonce,
        "host": host,
    })
}

fn required_string_field<'a>(
    admitted: &'a Map<String, Value>,
    entry_name: &str,
    field: &str,
) -> Result<&'a str> {
    admitted
        .get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("entry {entry_name:?} input is missing string field {field:?}"))
}

/// Runs an entry's declared `prepare.host` steps in order, folding each
/// one's facts into the `host` object the plugin's stdin carries.
async fn collect_host_facts(
    access: &ConfigAccess,
    principal_did: &str,
    entry_name: &str,
    admitted: &Map<String, Value>,
    host_steps: &[HostInput],
    host_root: Option<&Path>,
) -> Result<Map<String, Value>> {
    let mut host = Map::new();
    let mut git_diff: Option<GitDiffFacts> = None;
    for step in host_steps {
        match step {
            HostInput::GitDiff {
                repository_field,
                base_field,
                head_field,
                unified_context_lines,
                rename_similarity_percent,
            } => {
                let repository = required_string_field(admitted, entry_name, repository_field)?;
                let base = required_string_field(admitted, entry_name, base_field)?;
                let head = required_string_field(admitted, entry_name, head_field)?;
                let facts = collect_git_diff(
                    Path::new(repository),
                    base,
                    head,
                    *unified_context_lines,
                    *rename_similarity_percent,
                    host_root,
                )?;
                host.insert("git_diff".to_owned(), git_diff_host_json(&facts));
                git_diff = Some(facts);
            }
            HostInput::ReadOnlyWorkspace => {
                let facts = git_diff.as_ref().with_context(|| {
                    format!("entry {entry_name:?} read_only_workspace requires an earlier git_diff")
                })?;
                let workspace = crate::workspace::provision_read_only_workspace(
                    access,
                    &facts.repository,
                    &facts.head_sha,
                    principal_did,
                )
                .await?;
                host.insert(
                    "workspace".to_owned(),
                    read_only_workspace_host_json(
                        &workspace.workspace.workspace_id,
                        &workspace.workspace.owner_agent_did,
                    ),
                );
            }
        }
    }
    Ok(host)
}

/// The `_docID` a batched mutation's aliased `create_X` field carries for
/// one alias, whether the backend answers a create with a bare object or
/// (DefraDB's own shape) a single-element list.
fn batch_alias_doc_id<'a>(response: &'a Value, alias: &str) -> Option<&'a str> {
    let value = response.get("data")?.get(alias)?;
    value
        .get("_docID")
        .and_then(Value::as_str)
        .or_else(|| value.as_array()?.first()?.get("_docID")?.as_str())
}

/// Persists a prepare plugin's returned documents, batched so one prepare
/// step never renders one unbounded mutation: at most
/// [`PREPARE_DOCUMENTS_BATCH_LIMIT`] documents or
/// [`PREPARE_DOCUMENTS_BATCH_BYTES`] of rendered mutation text per batch, all
/// inside one transaction, in the plugin's own returned order.
async fn persist_prepared_documents(
    access: &ConfigAccess,
    documents: &[PreparePluginDocument],
) -> Result<()> {
    if documents.is_empty() {
        return Ok(());
    }
    let documents = documents.to_vec();
    access
        .transact("graph.prepare_entry_documents", move |txn| {
            let documents = documents.clone();
            Box::pin(async move {
                let mut start = 0;
                while start < documents.len() {
                    let mut mutation = String::from("mutation {");
                    let mut end = start;
                    while end < documents.len() && end - start < PREPARE_DOCUMENTS_BATCH_LIMIT {
                        let document = &documents[end];
                        let alias = end - start;
                        let literal = graphql_input_literal(&document.fields)?;
                        let piece = format!(
                            " d{alias}: create_{}(input: {literal}) {{ _docID }}",
                            document.collection
                        );
                        if alias > 0 && mutation.len() + piece.len() > PREPARE_DOCUMENTS_BATCH_BYTES
                        {
                            break;
                        }
                        mutation.push_str(&piece);
                        end += 1;
                    }
                    mutation.push_str(" }");
                    let batch_len = end - start;
                    let response = txn
                        .execute(&mutation)
                        .await
                        .context("persisting prepared entry documents")?;
                    for alias in 0..batch_len {
                        let key = format!("d{alias}");
                        anyhow::ensure!(
                            batch_alias_doc_id(&response, &key).is_some_and(|id| !id.is_empty()),
                            "prepare document batch silently dropped alias {key:?} ({} of {batch_len} in this batch); the backend must reject a partial batched mutation, not accept it silently",
                            alias + 1
                        );
                    }
                    start = end;
                }
                Ok(())
            })
        })
        .await
}

/// One sentence a caller can act on for a prepare plugin's non-success
/// verdict, in product vocabulary rather than the verdict's own Debug name.
/// When a `git_diff` step ran, `BadOutput`, `OutOfMemory` and `Timeout` name
/// the declared limit the diff tripped and its byte size, and suggest
/// narrowing `base..head`, since that is the one lever an operator has.
fn describe_prepare_plugin_failure(
    coordinate: &str,
    plugin: &crate::pack::PackPlugin,
    verdict: crate::plugin::PluginVerdict,
    diagnostics: &str,
    diff_bytes: Option<usize>,
) -> String {
    use crate::plugin::PluginVerdict;
    let limits = plugin.limits.as_ref();
    if let Some(bytes) = diff_bytes {
        let bound = match verdict {
            PluginVerdict::BadOutput => limits
                .and_then(|limits| limits.max_output_mib)
                .map(|mib| format!("its declared {mib} MiB output bound")),
            PluginVerdict::OutOfMemory => limits
                .and_then(|limits| limits.memory_mib)
                .map(|mib| format!("its declared {mib} MiB memory bound")),
            PluginVerdict::Timeout => limits
                .and_then(|limits| limits.wall_clock_secs)
                .map(|secs| format!("its declared {secs}s time bound")),
            _ => None,
        };
        if let Some(bound) = bound {
            return format!(
                "the {coordinate} plugin exceeded {bound} for a {bytes}-byte diff; narrow base..head"
            );
        }
    }
    let what = match verdict {
        PluginVerdict::Success => {
            unreachable!("describe_prepare_plugin_failure is only called on a non-success verdict")
        }
        PluginVerdict::Refused => "declined to run",
        PluginVerdict::OutOfFuel => "ran out of its execution budget",
        PluginVerdict::OutOfMemory => "exceeded its memory bound",
        PluginVerdict::Timeout => "exceeded its time bound",
        PluginVerdict::BadOutput => "did not return a valid result",
        PluginVerdict::Failed => "exited with a failure",
    };
    format!("the {coordinate} plugin {what}: {diagnostics}")
}

/// Selects an entry, admits its operator input, and, when the entry
/// declares `prepare`, runs its host steps and pack plugin to shape the
/// entry's real input and persist its evidence documents.
pub async fn prepare_entry_run(
    access: &ConfigAccess,
    principal_did: &str,
    request: EntryRunRequest<'_>,
) -> Result<PreparedEntryRun> {
    let entry = select_entry(request.plan, request.entry)?;
    let admitted = admit_operator_input(entry, request.input)?;
    let Some(prepare) = entry.prepare.as_ref() else {
        return Ok(PreparedEntryRun {
            entry_name: entry.name.clone(),
            input: Value::Object(admitted),
            origin: EntryInputOrigin::Operator,
            documents: 0,
        });
    };
    let host = collect_host_facts(
        access,
        principal_did,
        &entry.name,
        &admitted,
        &prepare.host,
        request.host_root,
    )
    .await?;
    let digest = prepare
        .digest
        .as_deref()
        .with_context(|| format!("entry {:?} prepare plugin is not pinned", entry.name))?;
    let record = request.plugins.resolve(&prepare.plugin, Some(digest))?;
    let diff_bytes = host
        .get("git_diff")
        .and_then(|git_diff| git_diff.get("patch"))
        .and_then(Value::as_str)
        .map(str::len);
    let nonce = uuid::Uuid::new_v4().to_string();
    let stdin = prepare_stdin(Value::Object(admitted), &nonce, Value::Object(host));
    let call = request.plugins.call(&record, stdin).await?;
    if call.outcome.verdict != crate::plugin::PluginVerdict::Success {
        anyhow::bail!(describe_prepare_plugin_failure(
            &call.coordinate,
            &record.declaration,
            call.outcome.verdict,
            &call.outcome.diagnostics,
            diff_bytes,
        ));
    }
    let output: PreparePluginOutput =
        serde_json::from_value(call.outcome.output).with_context(|| {
            format!(
                "the {} plugin returned a malformed prepare result",
                call.coordinate
            )
        })?;
    anyhow::ensure!(
        output.input.is_object(),
        "the {} plugin's prepared entry input must be a JSON object",
        call.coordinate
    );
    for document in &output.documents {
        anyhow::ensure!(
            prepare
                .writes
                .iter()
                .any(|write| write == &document.collection),
            "the {} plugin wrote to {:?}, which entry {:?} does not declare in prepare.writes",
            call.coordinate,
            document.collection,
            entry.name
        );
        anyhow::ensure!(
            document.fields.is_object(),
            "the {} plugin's document fields must be a JSON object",
            call.coordinate
        );
    }
    persist_prepared_documents(access, &output.documents).await?;
    Ok(PreparedEntryRun {
        entry_name: entry.name.clone(),
        input: output.input,
        origin: EntryInputOrigin::Prepared,
        documents: output.documents.len(),
    })
}

#[cfg(test)]
mod tests;
