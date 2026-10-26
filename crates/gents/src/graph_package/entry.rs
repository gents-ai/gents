use std::path::{Path, PathBuf};
use std::process::Command;

use gents_protocol::graphql::{graphql_input_literal, GraphqlError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config_client::{ConfigAccess, ConfigClientError, TransactionFailure};
use crate::graph_pipeline::{
    admit_operator_input, EntryInputOrigin, GraphPipelineError, GraphPlan, HostInput, PlannedEntry,
};
use crate::plugin::executor::PluginExecutor;

use super::catalog::GraphPackageError;

const PREPARE_DOCUMENTS_BATCH_LIMIT: usize = 32;
const PREPARE_DOCUMENTS_BATCH_BYTES: usize = 1024 * 1024;

type Result<T, E = EntryError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum EntryError {
    #[error(transparent)]
    GraphPackage(#[from] GraphPackageError),
    #[error(transparent)]
    PluginRun(#[from] crate::plugin::executor::PluginRunError),
    #[error(transparent)]
    GraphPipeline(#[from] GraphPipelineError),
    #[error(transparent)]
    ConfigClient(#[from] ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] GraphqlError),
    #[error("canonicalizing repository {}", path.display())]
    CanonicalizeRepository {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} is not a Git work tree", path.display())]
    NotWorkTree { path: PathBuf },
    #[error(transparent)]
    Overlay(#[from] crate::workspace::overlay::OverlayError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Quickstart(#[from] crate::workspace::QuickstartError),
    #[error("entry {entry_name:?} read_only_workspace requires an earlier git_diff")]
    ReadOnlyWorkspaceNeedsDiff { entry_name: String },
    #[error("persisting prepared entry documents")]
    PersistDocuments(#[source] ConfigClientError),
    #[error(
        "prepare document batch silently dropped alias {key:?} ({position} of {batch_len} in this batch); the backend must reject a partial batched mutation, not accept it silently"
    )]
    PrepareBatchDropped {
        key: String,
        position: usize,
        batch_len: usize,
    },
    #[error("entry {entry_name:?} prepare plugin is not pinned")]
    PreparePluginNotPinned { entry_name: String },
    #[error(transparent)]
    PreparePluginFailed(Box<PreparePluginFailure>),
    #[error("the {coordinate} plugin returned a malformed prepare result")]
    PrepareOutputMalformed {
        coordinate: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("the {coordinate} plugin's prepared entry input must be a JSON object")]
    PrepareInputNotObject { coordinate: String },
    #[error(
        "the {coordinate} plugin wrote to {collection:?}, which entry {entry_name:?} does not declare in prepare.writes"
    )]
    PrepareUndeclaredWrite {
        coordinate: String,
        collection: String,
        entry_name: String,
    },
    #[error("the {coordinate} plugin's document fields must be a JSON object")]
    PrepareFieldsNotObject { coordinate: String },
}

impl TransactionFailure for EntryError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::ConfigClient(error) | Self::PersistDocuments(error) => Some(error),
            Self::GraphPipeline(error) => error.config_client_error(),
            _ => None,
        }
    }
}

fn git_output_bytes(repo: &Path, arguments: &[&str]) -> Result<Vec<u8>, GraphPackageError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(arguments)
        .output()
        .map_err(|source| GraphPackageError::GitSpawn {
            repo: repo.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(GraphPackageError::GitFailed {
            arguments: arguments.join(" "),
            repo: repo.display().to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output.stdout)
}

fn git_output(repo: &Path, arguments: &[&str]) -> Result<String, GraphPackageError> {
    Ok(String::from_utf8(git_output_bytes(repo, arguments)?)?
        .trim()
        .to_owned())
}

fn git_output_exact(repo: &Path, arguments: &[&str]) -> Result<String, GraphPackageError> {
    String::from_utf8(git_output_bytes(repo, arguments)?).map_err(GraphPackageError::GitDiffNotUtf8)
}

fn resolve_repository(
    repo: &Path,
    base: &str,
    head: &str,
    process_root: Option<&Path>,
) -> Result<(PathBuf, String, String)> {
    let canonical =
        std::fs::canonicalize(repo).map_err(|source| EntryError::CanonicalizeRepository {
            path: repo.to_path_buf(),
            source,
        })?;
    crate::workspace::require_under_ceiling(&canonical, process_root)?;
    if git_output(&canonical, &["rev-parse", "--is-inside-work-tree"])? != "true" {
        return Err(EntryError::NotWorkTree {
            path: canonical.clone(),
        });
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

struct GitDiffFacts {
    repository: PathBuf,
    base_sha: String,
    head_sha: String,
    name_status: String,
    stat: String,
    patch: String,
}

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

pub struct EntryRunRequest<'a> {
    pub plan: &'a GraphPlan,
    pub entry: Option<&'a str>,
    pub input: Value,
    pub host_root: Option<&'a Path>,
    pub plugins: &'a PluginExecutor,
}

#[derive(Debug)]
pub struct PreparedEntryRun {
    pub entry_name: String,
    pub input: Value,
    pub origin: EntryInputOrigin,
    pub documents: usize,
}

pub(crate) fn select_entry<'a>(
    plan: &'a GraphPlan,
    requested: Option<&str>,
) -> Result<&'a PlannedEntry, GraphPackageError> {
    match requested {
        Some(name) => plan
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| GraphPackageError::EntryNotNamed {
                graph_id: plan.graph_id.clone(),
                name: name.to_owned(),
            }),
        None => match plan.entries.as_slice() {
            [entry] => Ok(entry),
            [] => Err(GraphPackageError::NoEntries {
                graph_id: plan.graph_id.clone(),
            }),
            entries => Err(GraphPackageError::AmbiguousEntries {
                graph_id: plan.graph_id.clone(),
                count: entries.len(),
                names: entries
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparePluginDocument {
    pub(crate) collection: String,
    pub(crate) fields: Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PreparePluginOutput {
    pub(crate) input: Value,
    pub(crate) documents: Vec<PreparePluginDocument>,
}

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

fn read_only_workspace_host_json(workspace_id: &str, owner_node_did: &str) -> Value {
    json!({
        "workspace_id": workspace_id,
        "owner_node_did": owner_node_did,
        "authority": "readOnly",
    })
}

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
) -> Result<&'a str, GraphPackageError> {
    admitted.get(field).and_then(Value::as_str).ok_or_else(|| {
        GraphPackageError::RequiredStringField {
            entry_name: entry_name.to_owned(),
            field: field.to_owned(),
        }
    })
}

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
                let facts =
                    git_diff
                        .as_ref()
                        .ok_or_else(|| EntryError::ReadOnlyWorkspaceNeedsDiff {
                            entry_name: entry_name.to_owned(),
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
                        &workspace.workspace.owner_node_did,
                    ),
                );
            }
        }
    }
    Ok(host)
}

fn batch_alias_doc_id<'a>(response: &'a Value, alias: &str) -> Option<&'a str> {
    let value = response.get("data")?.get(alias)?;
    value
        .get("_docID")
        .and_then(Value::as_str)
        .or_else(|| value.as_array()?.first()?.get("_docID")?.as_str())
}

async fn persist_prepared_documents(
    access: &ConfigAccess,
    documents: &[PreparePluginDocument],
) -> Result<()> {
    if documents.is_empty() {
        return Ok(());
    }
    let documents = documents.to_vec();
    access
        .transact_write("graph.prepare_entry_documents", move |txn| {
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
                        .map_err(EntryError::PersistDocuments)?;
                    for alias in 0..batch_len {
                        let key = format!("d{alias}");
                        if !batch_alias_doc_id(&response, &key).is_some_and(|id| !id.is_empty()) {
                            return Err(EntryError::PrepareBatchDropped {
                                key,
                                position: alias + 1,
                                batch_len,
                            });
                        }
                    }
                    start = end;
                }
                Ok(())
            })
        })
        .await
}

#[derive(Debug)]
pub struct PreparePluginFailure {
    pub coordinate: String,
    pub verdict: crate::plugin::PluginVerdict,
    pub diagnostics: String,
    /// The declared limit the verdict names, when the plugin declares one.
    pub declared_limit: Option<u32>,
    pub diff_bytes: Option<usize>,
}

impl PreparePluginFailure {
    fn new(
        coordinate: &str,
        plugin: &crate::pack::PackPlugin,
        verdict: crate::plugin::PluginVerdict,
        diagnostics: &str,
        diff_bytes: Option<usize>,
    ) -> Self {
        use crate::plugin::PluginVerdict;
        let limits = plugin.limits.as_ref();
        let declared_limit = match verdict {
            PluginVerdict::BadOutput => limits.and_then(|limits| limits.max_output_mib),
            PluginVerdict::OutOfMemory => limits.and_then(|limits| limits.memory_mib),
            PluginVerdict::Timeout => limits.and_then(|limits| limits.wall_clock_secs),
            _ => None,
        };
        Self {
            coordinate: coordinate.to_owned(),
            verdict,
            diagnostics: diagnostics.to_owned(),
            declared_limit,
            diff_bytes,
        }
    }
}

impl std::fmt::Display for PreparePluginFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::plugin::PluginVerdict;
        let coordinate = &self.coordinate;
        if let (Some(bytes), Some(limit)) = (self.diff_bytes, self.declared_limit) {
            let bound = match self.verdict {
                PluginVerdict::BadOutput => Some(format!("its declared {limit} MiB output bound")),
                PluginVerdict::OutOfMemory => {
                    Some(format!("its declared {limit} MiB memory bound"))
                }
                PluginVerdict::Timeout => Some(format!("its declared {limit}s time bound")),
                _ => None,
            };
            if let Some(bound) = bound {
                return write!(
                    f,
                    "the {coordinate} plugin exceeded {bound} for a {bytes}-byte diff; narrow base..head"
                );
            }
        }
        let what = match self.verdict {
            PluginVerdict::Success => "succeeded",
            PluginVerdict::Refused => "declined to run",
            PluginVerdict::OutOfFuel => "ran out of its execution budget",
            PluginVerdict::OutOfMemory => "exceeded its memory bound",
            PluginVerdict::Timeout => "exceeded its time bound",
            PluginVerdict::BadOutput => "did not return a valid result",
            PluginVerdict::Failed => "exited with a failure",
        };
        write!(f, "the {coordinate} plugin {what}: {}", self.diagnostics)
    }
}

impl std::error::Error for PreparePluginFailure {}

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
        .ok_or_else(|| EntryError::PreparePluginNotPinned {
            entry_name: entry.name.clone(),
        })?;
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
        return Err(EntryError::PreparePluginFailed(Box::new(
            PreparePluginFailure::new(
                &call.coordinate,
                &record.declaration,
                call.outcome.verdict,
                &call.outcome.diagnostics,
                diff_bytes,
            ),
        )));
    }
    let output: PreparePluginOutput =
        serde_json::from_value(call.outcome.output).map_err(|source| {
            EntryError::PrepareOutputMalformed {
                coordinate: call.coordinate.clone(),
                source,
            }
        })?;
    if !output.input.is_object() {
        return Err(EntryError::PrepareInputNotObject {
            coordinate: call.coordinate.clone(),
        });
    }
    for document in &output.documents {
        if !prepare
            .writes
            .iter()
            .any(|write| write == &document.collection)
        {
            return Err(EntryError::PrepareUndeclaredWrite {
                coordinate: call.coordinate.clone(),
                collection: document.collection.clone(),
                entry_name: entry.name.clone(),
            });
        }
        if !document.fields.is_object() {
            return Err(EntryError::PrepareFieldsNotObject {
                coordinate: call.coordinate.clone(),
            });
        }
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
