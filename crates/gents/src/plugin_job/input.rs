//! Derived job children. Spawn requests are attributed with
//! `decode_job_retry_key`; message requests, the other chain-caused requests,
//! join their spawn by session-id equality. Every request of a spawn's session counts
//! toward its usage, but only the spawn and message requests are its requests.

use rapidhash::{HashMapExt, RapidHashMap, RapidHashSet};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents_protocol::job::{
    JobCaller, JobCeilingView, JobChildState, JobChildView, JobPin, JobProgress, JobRef,
    JobRootCall, JobRowView, JobStepInput, JobUsage, JobVerifiedScript, PluginJobSettings,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;
use serde_json::Value;

use crate::background_tools::caused_request_terminals_in_txn;
use crate::config_client::{ConfigAccess, ConfigApplyTxn, ConfigRead};
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};
use crate::plugin::job_calls::job_state;
use crate::plugin::mounts::resolve as resolve_mounts;
use crate::plugin::store::{parse_coordinate, read_record};
use crate::plugin_job::admit::{meet, StepChild};
use crate::plugin_job::ancestry::started_by_job;
use crate::plugin_job::control::resume_echo;
use crate::plugin_job::driver::JobRuntime;
use crate::plugin_job::effects::scratch_name;
use crate::plugin_job::keys::decode_job_retry_key;
use crate::plugin_job::origin::{classify, script_index, Origin};
use crate::plugin_job::records::{read_pin, read_progress, read_step_state};
use crate::plugin_job::settings::effective_ceiling;
use crate::plugin_job::JobBinding;
use crate::provider_usage::{sum_charged_from_persisted_parts, sum_persisted_usage_columns};
use crate::request_admission::load_request_context;
use crate::request_binding::load_agent_request_by_doc_id;
use crate::tool_call_lifecycle::{load_tool_call_arguments, CausedRequestTerminal, ToolCallState};
use crate::watcher::AgentRequest;
use crate::workspace::request_workspace_cwd;

#[cfg(test)]
tokio::task_local! {
    pub(super) static CHILD_QUERIES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

const FIELDS: &str = "_docID request_id node_did agent_id session_id retry_key lifecycle_state input created_at terminalized_at";

#[derive(Deserialize)]
struct Stored {
    #[serde(default, rename = "_docID")]
    doc_id: Option<String>,
    request_id: String,
    #[serde(default)]
    node_did: Option<String>,
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    retry_key: Option<String>,
    #[serde(default)]
    lifecycle_state: Option<RequestLifecycleState>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    terminalized_at: Option<String>,
}

struct Attempt {
    slot: String,
    attempt: u32,
    spawn: Stored,
    messages: Vec<Stored>,
    usage_only: Vec<String>,
}

pub async fn derive_children(
    access: &ConfigAccess,
    chain_rows: &[String],
    result_bytes: u64,
) -> Result<Vec<(StepChild, JobChildView)>> {
    access
        .transact_readonly("plugin_job.derive_children", |txn| {
            Box::pin(derive_children_in_txn(txn, chain_rows, result_bytes))
        })
        .await
}

/// [`derive_children`], read through `txn`.
pub async fn derive_children_in_txn(
    txn: &ConfigApplyTxn<'_>,
    chain_rows: &[String],
    result_bytes: u64,
) -> Result<Vec<(StepChild, JobChildView)>> {
    if chain_rows.is_empty() {
        return Ok(Vec::new());
    }
    let list = graphql_string_list_literal(chain_rows.iter().map(String::as_str));
    let document = format!(
        "{{ AgentRequest(filter: {{ caused_by_parent_tool_call_doc_id: {{ _in: {list} }} }}) {{ {FIELDS} }} }}"
    );
    let rows: Vec<Stored> = read(txn, &document, "AgentRequest").await?;
    let caused: RapidHashSet<String> = rows.iter().map(|row| row.request_id.clone()).collect();
    let mut attempts: BTreeMap<(String, u32), Attempt> = BTreeMap::new();
    let mut sessions: RapidHashMap<String, (String, u32)> = RapidHashMap::new();
    for row in rows {
        let key = row.retry_key.clone().unwrap_or_default();
        let Some((slot, attempt)) = decode_job_retry_key(&key) else {
            continue;
        };
        let session = row
            .session_id
            .clone()
            .context("spawn request omitted session_id")?;
        if sessions.insert(session, (slot.clone(), attempt)).is_some() {
            anyhow::bail!("two spawn requests share a session");
        }
        if attempts
            .insert(
                (slot.clone(), attempt),
                Attempt {
                    slot,
                    attempt,
                    spawn: row,
                    messages: Vec::new(),
                    usage_only: Vec::new(),
                },
            )
            .is_some()
        {
            anyhow::bail!("duplicate slot attempt");
        }
    }
    if attempts.is_empty() {
        return Ok(Vec::new());
    }
    let list = graphql_string_list_literal(sessions.keys().map(String::as_str));
    let document =
        format!("{{ AgentRequest(filter: {{ session_id: {{ _in: {list} }} }}) {{ {FIELDS} }} }}");
    for row in read::<Stored>(txn, &document, "AgentRequest").await? {
        let Some((slot, attempt)) = row.session_id.as_deref().and_then(|id| sessions.get(id))
        else {
            continue;
        };
        let attempt = attempts
            .get_mut(&(slot.clone(), *attempt))
            .context("spawn session missing its attempt")?;
        if row.request_id == attempt.spawn.request_id {
            continue;
        }
        if caused.contains(&row.request_id) {
            attempt.messages.push(row);
        } else {
            attempt.usage_only.push(row.request_id);
        }
    }
    let mut request_ids = Vec::new();
    let mut latest_docs = Vec::new();
    for attempt in attempts.values() {
        request_ids.push(attempt.spawn.request_id.clone());
        request_ids.extend(attempt.messages.iter().map(|row| row.request_id.clone()));
        request_ids.extend(attempt.usage_only.iter().cloned());
        latest_docs.push(
            latest(attempt)?
                .doc_id
                .clone()
                .context("child request omitted _docID")?,
        );
    }
    let usage = usage_by_request(txn, &request_ids).await?;
    let terminals = caused_request_terminals_in_txn(txn, &latest_docs).await?;
    attempts
        .into_values()
        .map(|attempt| pair(attempt, &usage, &terminals, result_bytes))
        .collect()
}

fn latest(attempt: &Attempt) -> Result<&Stored> {
    std::iter::once(&attempt.spawn)
        .chain(&attempt.messages)
        .max_by(|left, right| {
            (left.created_at.as_deref(), left.request_id.as_str())
                .cmp(&(right.created_at.as_deref(), right.request_id.as_str()))
        })
        .context("attempt has no spawn request")
}

/// Prompt and completion token counts of each request's inference calls.
type UsageByRequest = RapidHashMap<String, Vec<(Option<i64>, Option<i64>)>>;

fn pair(
    attempt: Attempt,
    usage: &UsageByRequest,
    terminals: &RapidHashMap<String, CausedRequestTerminal>,
    result_bytes: u64,
) -> Result<(StepChild, JobChildView)> {
    let latest_id = latest(&attempt)?.request_id.clone();
    let mut rows = vec![attempt.spawn];
    let message_count = u32::try_from(attempt.messages.len()).unwrap_or(u32::MAX);
    rows.extend(attempt.messages);
    let spawn = &rows[0];
    let latest = rows
        .iter()
        .find(|row| row.request_id == latest_id)
        .context("attempt lost its latest request")?;
    let terminal = latest.doc_id.as_ref().and_then(|doc| terminals.get(doc));
    let state = child_state(terminal);
    let parts = rows
        .iter()
        .map(|row| &row.request_id)
        .chain(&attempt.usage_only)
        .flat_map(|id| usage.get(id).into_iter().flatten().copied());
    let (input, output, _) = sum_persisted_usage_columns(
        parts
            .clone()
            .map(|(prompt, completion)| (prompt, completion, None)),
    );
    let total = sum_charged_from_persisted_parts(parts)?;
    let len = match terminal {
        Some(CausedRequestTerminal::Completed { output }) => output.len() as u64,
        _ => 0,
    };
    Ok((
        StepChild {
            retry_key: spawn.retry_key.clone().unwrap_or_default(),
            slot: attempt.slot,
            attempt: attempt.attempt,
            target: (
                spawn
                    .node_did
                    .clone()
                    .context("spawn request omitted node_did")?,
                spawn
                    .agent_id
                    .clone()
                    .context("spawn request omitted agent_id")?,
            ),
            state,
            messages: message_count,
        },
        JobChildView {
            attempt: attempt.attempt,
            session_id: spawn.session_id.clone().unwrap_or_default(),
            request_id: latest.request_id.clone(),
            title: title_of(spawn),
            state,
            output_bytes: len.min(result_bytes),
            truncated: len > result_bytes,
            usage: JobUsage {
                input: input.unwrap_or_default(),
                output: output.unwrap_or_default(),
                total,
            },
            duration_ms: duration_ms(&rows, &latest_id, state),
        },
    ))
}

fn child_state(terminal: Option<&CausedRequestTerminal>) -> JobChildState {
    match terminal {
        None => JobChildState::Live,
        Some(CausedRequestTerminal::Completed { .. }) => JobChildState::Completed,
        Some(CausedRequestTerminal::Failed { .. }) => JobChildState::Failed,
        Some(CausedRequestTerminal::Interrupted) => JobChildState::Interrupted,
        Some(CausedRequestTerminal::Dead | CausedRequestTerminal::Superseded) => {
            JobChildState::Other
        }
    }
}

fn title_of(row: &Stored) -> String {
    row.input
        .as_ref()
        .and_then(|input| input.pointer("/initial_title/text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn duration_ms(rows: &[Stored], latest_id: &str, latest_state: JobChildState) -> u64 {
    let start = rows
        .iter()
        .filter_map(|row| stamp(row.created_at.as_deref()))
        .min();
    let end = rows
        .iter()
        .filter(|row| {
            if row.request_id == latest_id {
                latest_state != JobChildState::Live
            } else {
                row.lifecycle_state
                    .is_some_and(RequestLifecycleState::is_terminal)
            }
        })
        .filter_map(|row| stamp(row.terminalized_at.as_deref()))
        .max();
    match (start, end) {
        (Some(start), Some(end)) if end >= start => u64::try_from(end - start).unwrap_or(0),
        _ => 0,
    }
}

fn stamp(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.timestamp_millis())
}

async fn usage_by_request<R>(reader: &R, request_ids: &[String]) -> Result<UsageByRequest>
where
    R: ConfigRead,
{
    let mut grouped: UsageByRequest = RapidHashMap::new();
    if request_ids.is_empty() {
        return Ok(grouped);
    }
    let list = graphql_string_list_literal(request_ids.iter().map(String::as_str));
    let document = format!(
        "{{ InferenceCall(filter: {{ request_id: {{ _in: {list} }} }}) {{ request_id prompt_tokens completion_tokens }} }}"
    );
    #[derive(Deserialize)]
    struct UsageRow {
        #[serde(default)]
        request_id: Option<String>,
        #[serde(default)]
        prompt_tokens: Option<i64>,
        #[serde(default)]
        completion_tokens: Option<i64>,
    }
    for row in read::<UsageRow>(reader, &document, "InferenceCall").await? {
        let Some(request_id) = row.request_id else {
            continue;
        };
        grouped
            .entry(request_id)
            .or_default()
            .push((row.prompt_tokens, row.completion_tokens));
    }
    Ok(grouped)
}

async fn read<T>(reader: &impl ConfigRead, document: &str, collection: &str) -> Result<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    #[cfg(test)]
    let _ = CHILD_QUERIES.try_with(|queries| {
        queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    });
    let response = reader.execute_read(document).await?;
    let rows = response
        .pointer(&format!("/data/{collection}"))
        .cloned()
        .with_context(|| format!("{collection} child query omitted rows"))?;
    serde_json::from_value(rows).with_context(|| format!("decoding {collection} child rows"))
}

/// Which step of a job an input is built for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepKind<'a> {
    /// The foreground call before adoption: pin and origin null, cwd from
    /// the request, and the plugin and its policy from the hook's binding.
    Step0 { binding: &'a JobBinding },
    /// A pinned row's step: cwd from `job_pin.cwd`, plugin from `job_pin.plugin_ref`.
    Later,
}

#[derive(Deserialize)]
struct CallRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    session_id: String,
    node_did: String,
    #[serde(default)]
    requester_did: Option<String>,
    request_doc_id: String,
    tool_call_id: String,
    tool_name: String,
}

/// The arguments the model's accepted call carried; C9 echoes them verbatim.
#[derive(Deserialize)]
pub(super) struct CallArgs {
    #[serde(default)]
    pub(super) argv: Vec<String>,
    #[serde(default)]
    pub(super) args: Option<serde_json::Map<String, Value>>,
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default)]
    pub(super) script: Option<String>,
    #[serde(default)]
    pub(super) agent_budget: Option<u32>,
}

async fn read_call(access: &ConfigAccess, row_doc_id: &str) -> Result<(CallRow, CallArgs)> {
    let document = format!(
        r#"{{ AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ _docID session_id node_did requester_did request_doc_id tool_call_id tool_name }} }}"#,
        escape_graphql_string(row_doc_id)
    );
    let row = read::<CallRow>(access, &document, "AgentToolCall")
        .await?
        .into_iter()
        .next()
        .with_context(|| format!("job row {row_doc_id} does not exist"))?;
    let text = load_tool_call_arguments(
        access,
        &row.doc_id,
        &row.node_did,
        &row.session_id,
        row.requester_did.as_deref(),
    )
    .await?;
    let args = serde_json::from_str(&text)
        .with_context(|| format!("decoding the accepted arguments of job row {row_doc_id}"))?;
    Ok((row, args))
}

/// The job settings the caller's agent holds now for `coordinate`; `None`
/// when its Tools document names no `job` block for it.
pub(super) async fn live_job_settings(
    runtime: &JobRuntime,
    caller: &AgentRequest,
    coordinate: &str,
) -> Result<Option<PluginJobSettings>> {
    let (_, tools, _) = load_request_context(&runtime.node, &caller.node_did, &caller.agent_id)
        .await
        .context("reading the caller's tools")?;
    Ok(tools
        .and_then(|tools| tools.integrations)
        .and_then(|integrations| integrations.plugins)
        .into_iter()
        .flatten()
        .find(|plugin| plugin.plugin == coordinate)
        .and_then(|plugin| plugin.job))
}

/// `row_doc_id` and the rows its pin resumes, oldest first.
pub(super) async fn chain_of<R>(
    reader: &R,
    row_doc_id: &str,
    pin: Option<&JobPin>,
) -> Result<Vec<String>>
where
    R: ConfigRead,
{
    let mut chain = vec![row_doc_id.to_owned()];
    let mut next = pin.and_then(|pin| pin.resume_of.clone());
    while let Some(prior) = next.take().filter(|prior| !chain.contains(prior)) {
        next = read_pin(reader, &prior)
            .await?
            .and_then(|pin| pin.resume_of);
        chain.push(prior);
    }
    chain.reverse();
    Ok(chain)
}

fn scratch_files(cwd: Option<&Path>, tool_call_id: &str) -> BTreeMap<String, String> {
    let Some(cwd) = cwd.filter(|_| scratch_name(tool_call_id)) else {
        return BTreeMap::new();
    };
    let dir = cwd.join(".gents/workflows/runs").join(tool_call_id);
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            Some((name, entry.path().to_str()?.to_owned()))
        })
        .collect()
}

/// The caller session's rows of this plugin that carry a decodable `job_pin`.
async fn session_jobs(
    access: &ConfigAccess,
    session_id: &str,
    tool_name: &str,
) -> Result<Vec<JobRowView>> {
    #[derive(Deserialize)]
    struct Stored {
        #[serde(rename = "_docID")]
        doc_id: String,
        tool_call_id: String,
        #[serde(default)]
        job_pin: Option<Value>,
        #[serde(default)]
        job_progress: Option<Value>,
        #[serde(default)]
        lifecycle_state: Option<String>,
    }
    let document = format!(
        r#"{{ AgentToolCall(filter: {{ session_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "{}" }} }}) {{ _docID tool_call_id job_pin job_progress lifecycle_state }} }}"#,
        escape_graphql_string(session_id),
        escape_graphql_string(tool_name)
    );
    let mut jobs: Vec<JobRowView> = read::<Stored>(access, &document, "AgentToolCall")
        .await?
        .into_iter()
        .filter_map(|row| {
            let pin: JobPin = serde_json::from_value(row.job_pin?).ok()?;
            let progress = row
                .job_progress
                .and_then(|progress| serde_json::from_value::<JobProgress>(progress).ok());
            let lifecycle = row
                .lifecycle_state
                .as_deref()
                .and_then(ToolCallState::from_persisted)
                .unwrap_or(ToolCallState::Running);
            let state = job_state(lifecycle, progress.as_ref());
            Some(JobRowView {
                row_doc_id: row.doc_id,
                tool_call_id: row.tool_call_id,
                handle: pin.handle,
                state,
            })
        })
        .collect();
    jobs.sort_by(|left, right| left.row_doc_id.cmp(&right.row_doc_id));
    Ok(jobs)
}

/// The input of one job step, read from the row, its caller request, the
/// plugin record, the caller's live job settings and the derived children,
/// with the progress revision it was built from.
pub async fn build_step_input(
    access: &ConfigAccess,
    runtime: &JobRuntime,
    row_doc_id: &str,
    step: StepKind<'_>,
) -> Result<(JobStepInput, u64)> {
    let (row, call) = read_call(access, row_doc_id).await?;
    let caller = load_agent_request_by_doc_id(&runtime.node, &row.request_doc_id)
        .await?
        .with_context(|| format!("the caller request of job row {row_doc_id} does not exist"))?;
    let (pin, coordinate, settings) = match step {
        StepKind::Step0 { binding } => (
            None,
            binding.plugin.plugin.clone(),
            binding.settings.clone(),
        ),
        StepKind::Later => {
            let pin = read_pin(access, row_doc_id)
                .await?
                .with_context(|| format!("job row {row_doc_id} has no job_pin"))?;
            let settings = live_job_settings(runtime, &caller, &pin.plugin_ref).await?;
            (Some(pin.clone()), pin.plugin_ref, settings)
        }
    };
    let cwd = match &pin {
        Some(pin) => pin.cwd.clone(),
        None => request_workspace_cwd(&caller)
            .or_else(|| runtime.tool_root.clone())
            .map(|cwd| cwd.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    let cwd_path = (!cwd.is_empty()).then(|| PathBuf::from(&cwd));
    let policy = settings.as_ref().map(|settings| &settings.policy);
    let (namespace, name) = parse_coordinate(&coordinate)?;
    let plugin = read_record(&runtime.home, namespace, name)?;
    let mounts = resolve_mounts(&runtime.home, &plugin, policy, cwd_path.as_deref())?;
    let verified_script = match &pin {
        Some(pin) => match classify(
            &script_index(&runtime.home, &plugin, policy, cwd_path.as_deref())?,
            pin,
        ) {
            Origin::Installed {
                verified_script,
                digest,
            } => Some(JobVerifiedScript {
                source: String::from_utf8(verified_script)
                    .context("the verified script is not UTF-8")?,
                digest,
            }),
            Origin::Dynamic => None,
        },
        None => None,
    };
    let progress = read_progress(access, row_doc_id).await?;
    let revision = progress.as_ref().map_or(0, |progress| progress.revision);
    let live = effective_ceiling(settings.as_ref());
    let effective = pin
        .as_ref()
        .map_or_else(|| live.clone(), |pin| meet(&live, &pin.ceiling_at_pin));
    let spawn_cap = pin.as_ref().map_or(effective.max_spawns, |pin| {
        effective.max_spawns.min(pin.agent_budget)
    });
    let dynamic = policy.cloned().unwrap_or_default();
    let ceiling = JobCeilingView {
        targets: effective.targets,
        max_live: effective.max_live,
        remaining_spawns: spawn_cap
            .saturating_sub(progress.as_ref().map_or(0, |progress| progress.spawned)),
        max_messages_per_slot: effective.max_messages_per_slot,
        result_bytes: effective.result_bytes,
        dynamic: dynamic.dynamic,
        dynamic_targets: dynamic.dynamic_targets,
    };
    let chain = chain_of(access, row_doc_id, pin.as_ref()).await?;
    let mut children: BTreeMap<String, JobChildView> = BTreeMap::new();
    for (child, view) in derive_children(access, &chain, ceiling.result_bytes).await? {
        if children
            .get(&child.slot)
            .is_none_or(|kept| kept.attempt <= view.attempt)
        {
            children.insert(child.slot, view);
        }
    }
    let resume = read_step_state(access, row_doc_id)
        .await?
        .and_then(|state| state.resume);
    let root_call = match (&pin, &resume) {
        (Some(pin), Some(_)) => {
            let (_, root) = read_call(access, &pin.chain_root_doc_id).await?;
            Some(JobRootCall {
                argv: root.argv,
                args: root.args,
                path: root.path,
                agent_budget: root.agent_budget,
            })
        }
        _ => None,
    };
    let control = resume.as_ref().map(resume_echo);
    let input = JobStepInput {
        protocol: 1,
        job: JobRef {
            row_doc_id: row_doc_id.to_owned(),
            chain_root_doc_id: pin.as_ref().map_or_else(
                || row_doc_id.to_owned(),
                |pin| pin.chain_root_doc_id.clone(),
            ),
            chain,
            handle: pin
                .as_ref()
                .map(|pin| pin.handle.clone())
                .unwrap_or_default(),
        },
        caller: JobCaller {
            started_by_job: started_by_job(access, &row.session_id, caller.request_hop).await?,
            session_id: row.session_id.clone(),
            request_id: caller.request_id.clone(),
            request_doc_id: row.request_doc_id.clone(),
            tool_call_id: row.tool_call_id.clone(),
            node_did: caller.node_did.clone(),
            agent_id: caller.agent_id.clone(),
            request_hop: caller.request_hop,
            correlation: caller.caused_by_correlation.clone(),
            cwd,
        },
        argv: call.argv,
        args: call.args,
        path: call.path,
        script: call.script,
        agent_budget: call.agent_budget,
        root_call,
        origin: pin.as_ref().map(|pin| pin.origin),
        pin,
        verified_script,
        mounts,
        ceiling,
        children,
        control,
        scratch: scratch_files(cwd_path.as_deref(), &row.tool_call_id),
        jobs: session_jobs(access, &row.session_id, &row.tool_name).await?,
    };
    Ok((input, revision))
}

#[cfg(test)]
pub(crate) mod tests {
    use gents_protocol::error_chain::ErrorChain;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use defra_node::EmbeddedNode;
    use gents_protocol::job::JobChildState;
    use serde_json::{json, Value};

    use super::{derive_children, CHILD_QUERIES};
    use crate::config_client::ConfigAccess;
    use crate::plugin_job::keys::{job_message_key, job_session_id, job_spawn_key};

    pub(crate) async fn open() -> Arc<EmbeddedNode> {
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(node_did())
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();
        node
    }

    pub(crate) async fn insert(
        node: &std::sync::Arc<EmbeddedNode>,
        collection: &str,
        value: Value,
    ) -> String {
        ConfigAccess::transact_local_write(node, None, "test.plugin_job_children", |txn| {
            let value = &value;
            Box::pin(async move {
                let query = format!(
                    "mutation($input: {collection}MutationInputArg!) {{create_{collection}(input:$input){{_docID}}}}"
                );
                let response = txn
                    .execute_with_variables(&query, &json!({ "input": value }))
                    .await?;
                Ok(gents_protocol::graphql::extract_mutation_doc_id(&response, collection)?)
            })
        })
        .await
        .unwrap()
    }

    /// Overwrites the stored `lifecycle_state` of the job row `row_doc_id`.
    pub(crate) async fn set_lifecycle(
        node: &std::sync::Arc<EmbeddedNode>,
        row_doc_id: &str,
        state: &str,
    ) {
        ConfigAccess::transact_local_write(node, None, "test.plugin_job_lifecycle", |txn| {
            Box::pin(async move {
                txn.execute(&format!(
                    r#"mutation {{ update_AgentToolCall(docID: "{}", input: {{lifecycle_state: "{}"}}) {{ _docID }} }}"#,
                    crate::graphql::escape_graphql_string(row_doc_id),
                    crate::graphql::escape_graphql_string(state)
                ))
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "test seed helper takes the seeded row's fields"
    )]
    pub(crate) async fn request(
        node: &std::sync::Arc<EmbeddedNode>,
        chain: &str,
        request_id: &str,
        session_id: &str,
        retry_key: &str,
        agent_id: &str,
        state: &str,
        title: Option<&str>,
        created_at: &str,
        terminalized_at: Option<&str>,
    ) {
        let mut value = json!({
            "request_id": request_id,
            "purpose": "normal",
            "node_did": "did-owner",
            "requester_did": null,
            "session_id": session_id,
            "agent_id": agent_id,
            "lifecycle_state": state,
            "retry_key": retry_key,
            "created_at": created_at,
            "caused_by_parent_tool_call_doc_id": chain,
            "caused_by_parent_tool_call_id": "call",
            "caused_by_parent_request_id": "parent",
            "caused_by_parent_request_doc_id": "parent-doc",
        });
        if let Some(title) = title {
            value["input"] = json!({"initial_title": {"text": title, "source": "task"}});
        }
        if let Some(at) = terminalized_at {
            value["terminalized_at"] = json!(at);
        }
        insert(node, "AgentRequest", value).await;
    }

    async fn usage(
        node: &std::sync::Arc<EmbeddedNode>,
        call_id: &str,
        request_id: &str,
        prompt: i64,
        completion: i64,
    ) {
        insert(
            node,
            "InferenceCall",
            json!({
                "call_id": call_id,
                "request_id": request_id,
                "node_did": "did-owner",
                "call_state": "completed",
                "prompt_tokens": prompt,
                "completion_tokens": completion,
            }),
        )
        .await;
    }

    async fn counted(
        access: &ConfigAccess,
        chain: &[String],
        cap: u64,
    ) -> (
        anyhow::Result<
            Vec<(
                crate::plugin_job::admit::StepChild,
                gents_protocol::job::JobChildView,
            )>,
        >,
        usize,
    ) {
        let queries = Arc::new(AtomicUsize::new(0));
        let output = CHILD_QUERIES
            .scope(queries.clone(), derive_children(access, chain, cap))
            .await;
        (output, queries.load(Ordering::Relaxed))
    }

    #[tokio::test]
    async fn children_are_attributed_by_retry_key_across_two_slots_with_a_retry_attempt() {
        let node = open().await;
        let chain = "chain-row";
        request(
            &node,
            chain,
            "a0",
            &job_session_id("root", "slot-a", 0),
            &job_spawn_key("root", "slot-a", 0),
            "agent-a",
            "processing",
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            None,
        )
        .await;
        request(
            &node,
            chain,
            "a1",
            &job_session_id("root", "slot-a", 1),
            &job_spawn_key("root", "slot-a", 1),
            "agent-a",
            "processing",
            Some("Alpha retry"),
            "2026-01-01T00:01:00Z",
            None,
        )
        .await;
        request(
            &node,
            "other-row",
            "b0",
            &job_session_id("root", "slot-b", 0),
            &job_spawn_key("root", "slot-b", 0),
            "agent-b",
            "processing",
            Some("Beta"),
            "2026-01-01T00:02:00Z",
            None,
        )
        .await;
        let access = ConfigAccess::local(node.clone());
        let children = derive_children(&access, &[chain.into(), "other-row".into()], 64)
            .await
            .unwrap();
        let keys: Vec<_> = children
            .iter()
            .map(|(child, _)| (child.slot.clone(), child.attempt, child.retry_key.clone()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("slot-a".into(), 0, job_spawn_key("root", "slot-a", 0)),
                ("slot-a".into(), 1, job_spawn_key("root", "slot-a", 1)),
                ("slot-b".into(), 0, job_spawn_key("root", "slot-b", 0)),
            ]
        );
        assert_eq!(children[0].0.target, ("did-owner".into(), "agent-a".into()));
        assert_eq!(children[2].0.target, ("did-owner".into(), "agent-b".into()));
        assert_eq!(children[0].1.title, "Alpha");
        assert_eq!(children[1].1.request_id, "a1");
        node.shutdown().await;
    }

    #[tokio::test]
    async fn message_requests_join_their_slot_by_session_and_never_open_a_slot() {
        let node = open().await;
        let chain = "chain-row";
        let session = job_session_id("root", "slot-a", 0);
        request(
            &node,
            chain,
            "spawn",
            &session,
            &job_spawn_key("root", "slot-a", 0),
            "agent-a",
            "processing",
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            None,
        )
        .await;
        request(
            &node,
            chain,
            "message",
            &session,
            &job_message_key("root", "slot-a", 0, 1),
            "agent-message",
            "processing",
            None,
            "2026-01-01T00:05:00Z",
            None,
        )
        .await;
        request(
            &node,
            chain,
            "stray",
            "other-session",
            "not-a-spawn-key",
            "agent-x",
            "processing",
            Some("Stray"),
            "2026-01-01T00:06:00Z",
            None,
        )
        .await;
        usage(&node, "spawn-call", "spawn", 1, 1).await;
        usage(&node, "message-call", "message", 2, 3).await;
        usage(&node, "stray-call", "stray", 100, 100).await;
        let access = ConfigAccess::local(node.clone());
        let children = derive_children(&access, &[chain.into()], 64).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].0.messages, 1);
        assert_eq!(children[0].0.target.1, "agent-a");
        assert_eq!(children[0].1.request_id, "message");
        assert_eq!(children[0].1.usage.input, 3);
        assert_eq!(children[0].1.usage.output, 4);
        assert_eq!(children[0].1.usage.total, 7);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn failed_slot_reports_state_failed() {
        let node = open().await;
        let chain = "chain-row";
        request(
            &node,
            chain,
            "spawn",
            &job_session_id("root", "slot-a", 0),
            &job_spawn_key("root", "slot-a", 0),
            "agent-a",
            "failed",
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            Some("2026-01-01T00:00:02Z"),
        )
        .await;
        let access = ConfigAccess::local(node.clone());
        let children = derive_children(&access, &[chain.into()], 64).await.unwrap();
        assert_eq!(children.len(), 1);
        assert_eq!(
            children[0].0.state,
            gents_protocol::job::JobChildState::Failed
        );
        assert_eq!(children[0].1.state, children[0].0.state);
        assert_eq!(children[0].1.duration_ms, 2_000);
        assert_eq!(children[0].1.output_bytes, 0);
        assert!(!children[0].1.truncated);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn child_usage_sums_input_output_and_total_over_the_slot_session() {
        let node = open().await;
        let chain = "chain-row";
        let session = job_session_id("root", "slot-a", 0);
        request(
            &node,
            chain,
            "spawn",
            &session,
            &job_spawn_key("root", "slot-a", 0),
            "agent-a",
            "processing",
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            None,
        )
        .await;
        request(
            &node,
            chain,
            "message",
            &session,
            &job_message_key("root", "slot-a", 0, 1),
            "agent-a",
            "processing",
            None,
            "2026-01-01T00:01:00Z",
            None,
        )
        .await;
        request(
            &node,
            chain,
            "other",
            &job_session_id("root", "slot-b", 0),
            &job_spawn_key("root", "slot-b", 0),
            "agent-b",
            "processing",
            Some("Beta"),
            "2026-01-01T00:00:00Z",
            None,
        )
        .await;
        usage(&node, "c1", "spawn", 10, 4).await;
        usage(&node, "c2", "message", 3, 1).await;
        usage(&node, "c3", "other", 7, 8).await;
        let access = ConfigAccess::local(node.clone());
        let children = derive_children(&access, &[chain.into()], 64).await.unwrap();
        let alpha = children
            .iter()
            .find(|(child, _)| child.slot == "slot-a")
            .unwrap();
        let beta = children
            .iter()
            .find(|(child, _)| child.slot == "slot-b")
            .unwrap();
        assert_eq!(alpha.1.usage.input, 13);
        assert_eq!(alpha.1.usage.output, 5);
        assert_eq!(alpha.1.usage.total, 18);
        assert_eq!(beta.1.usage.input, 7);
        assert_eq!(beta.1.usage.output, 8);
        assert_eq!(beta.1.usage.total, 15);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn child_derivation_query_count_is_independent_of_child_count() {
        let node = open().await;
        let one = "one-row";
        let many = "many-row";
        request(
            &node,
            one,
            "only",
            &job_session_id("root", "only", 0),
            &job_spawn_key("root", "only", 0),
            "agent",
            "processing",
            None,
            "2026-01-01T00:00:00Z",
            None,
        )
        .await;
        for attempt in 0..3 {
            request(
                &node,
                many,
                &format!("m{attempt}"),
                &job_session_id("root", "many", attempt),
                &job_spawn_key("root", "many", attempt),
                "agent",
                "processing",
                None,
                "2026-01-01T00:00:00Z",
                None,
            )
            .await;
        }
        let access = ConfigAccess::local(node.clone());
        let (single, single_queries) = counted(&access, &[one.into()], 64).await;
        let (triple, triple_queries) = counted(&access, &[many.into()], 64).await;
        assert_eq!(single.unwrap().len(), 1);
        assert_eq!(triple.unwrap().len(), 3);
        assert_eq!(single_queries, triple_queries);
        assert!(single_queries > 0);
        node.shutdown().await;
    }

    pub(crate) async fn completed_request(
        node: &Arc<EmbeddedNode>,
        chain: &str,
        request_id: &str,
        session_id: &str,
        retry_key: &str,
        text: &str,
    ) {
        let node_did = "did:test:test";
        insert(
            node,
            "AgentRequest",
            json!({
                "request_id": request_id,
                "purpose": "normal",
                "node_did": node_did,
                "agent_id": "general",
                "session_id": session_id,
                "retry_parent_request": "",
                "retry_root_request": request_id,
                "superseded_by_request": "",
                "content": "fixture",
                "lifecycle_state": "pending",
                "backend_id": "",
                "execution_origin": "interactive",
                "failure_reason": "",
                "created_at": "2026-01-01T00:00:00Z",
                "retry_count": 0,
                "max_retries": 3,
                "request_hop": 1,
                "retry_key": retry_key,
                "caused_by_parent_tool_call_doc_id": chain,
                "caused_by_parent_tool_call_id": "call",
                "caused_by_parent_request_id": "parent",
                "caused_by_parent_request_doc_id": "parent-doc",
            }),
        )
        .await;
        let row = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{request_id}" }} }}) {{ {} }} }}"#,
                crate::watcher::AGENT_REQUEST_FIELDS,
            ))
            .await;
        let row: gents_protocol::row::AgentRequestRow =
            crate::graphql::first_row(&row, "AgentRequest")
                .unwrap()
                .unwrap();
        let mut lifecycle = crate::lifecycle::RequestLifecycle::new_with_node_did(
            node.clone(),
            "general",
            node_did,
            row.try_into().unwrap(),
            60,
        );
        assert_eq!(
            lifecycle.claim().await.unwrap(),
            crate::lifecycle::ClaimOutcome::Claimed
        );
        let writer = crate::streaming::DefraStreamWriter::new(
            node.clone(),
            node_did,
            std::time::Duration::from_millis(1),
        );
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        writer
            .start_provider_attempt(
                &lifecycle.request().doc_id,
                0,
                0,
                "inference.1".parse().unwrap(),
            )
            .await;
        let published = writer
            .publish_native_turn(
                &lifecycle,
                0,
                0,
                &gents_protocol::message::Message::assistant(text),
            )
            .await
            .unwrap();
        lifecycle
            .terminalize_owned(
                crate::lifecycle::RequestTerminalOutcome::Completed,
                gents_protocol::output::TerminalOutput::Message {
                    message_doc_id: published.message_doc_id,
                },
                None,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn completed_child_reports_its_rendered_terminal_and_truncates_at_the_cap() {
        let node = open().await;
        let chain = "chain-row";
        completed_request(
            &node,
            chain,
            "done",
            &job_session_id("root", "slot-a", 0),
            &job_spawn_key("root", "slot-a", 0),
            "twelve bytes",
        )
        .await;
        let access = ConfigAccess::local(node.clone());
        let whole = derive_children(&access, &[chain.into()], 64).await.unwrap();
        assert_eq!(whole[0].0.state, JobChildState::Completed);
        assert_eq!(whole[0].1.state, JobChildState::Completed);
        assert_eq!(whole[0].1.output_bytes, 12);
        assert!(!whole[0].1.truncated);
        let capped = derive_children(&access, &[chain.into()], 5).await.unwrap();
        assert_eq!(capped[0].1.output_bytes, 5);
        assert!(capped[0].1.truncated);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn interrupted_child_and_unselected_completion_take_their_state_from_the_terminal_owner()
    {
        let node = open().await;
        let chain = "chain-row";
        request(
            &node,
            chain,
            "stopped",
            &job_session_id("root", "slot-a", 0),
            &job_spawn_key("root", "slot-a", 0),
            "agent-a",
            "interrupted",
            None,
            "2026-01-01T00:00:00Z",
            Some("2026-01-01T00:00:01Z"),
        )
        .await;
        request(
            &node,
            chain,
            "pending-selection",
            &job_session_id("root", "slot-b", 0),
            &job_spawn_key("root", "slot-b", 0),
            "agent-b",
            "completed",
            None,
            "2026-01-01T00:00:00Z",
            Some("2026-01-01T00:00:01Z"),
        )
        .await;
        let access = ConfigAccess::local(node.clone());
        let children = derive_children(&access, &[chain.into()], 64).await.unwrap();
        assert_eq!(children[0].0.state, JobChildState::Interrupted);
        assert_eq!(children[1].0.state, JobChildState::Live);
        node.shutdown().await;
    }

    #[tokio::test]
    async fn session_requests_no_chain_row_caused_count_only_toward_usage() {
        let node = open().await;
        let chain = "chain-row";
        let session = job_session_id("root", "slot-a", 0);
        request(
            &node,
            chain,
            "spawn",
            &session,
            &job_spawn_key("root", "slot-a", 0),
            "agent-a",
            "processing",
            Some("Alpha"),
            "2026-01-01T00:00:00Z",
            None,
        )
        .await;
        request(
            &node,
            "elsewhere",
            "title-audit",
            &session,
            "title-audit-key",
            "agent-a",
            "completed",
            None,
            "2026-01-01T00:03:00Z",
            Some("2026-01-01T00:03:01Z"),
        )
        .await;
        usage(&node, "c1", "spawn", 1, 1).await;
        usage(&node, "c2", "title-audit", 5, 6).await;
        let access = ConfigAccess::local(node.clone());
        let children = derive_children(&access, &[chain.into()], 64).await.unwrap();
        assert_eq!(children[0].1.usage.input, 6);
        assert_eq!(children[0].1.usage.output, 7);
        assert_eq!(children[0].1.usage.total, 13);
        assert_eq!(children[0].1.request_id, "spawn");
        assert_eq!(children[0].0.messages, 0);
        assert_eq!(children[0].1.state, JobChildState::Live);
        assert_eq!(children[0].1.duration_ms, 0);
        node.shutdown().await;
    }

    use std::collections::BTreeMap;
    use std::path::Path;

    use gents_protocol::job::{
        AssetMount, DynamicMode, JobBudget, JobCeilingView, JobControlEcho, JobControlEchoKind,
        JobOrigin, JobPin, JobPolicy, JobProgress, JobResume, JobResumeEngine, JobRootCall,
        JobRowView, JobState, JobStepInput, JobStepOutput, JobUsage, JobVerifiedScript,
        PluginDataDeclaration, PluginJobSettings, StampedField,
    };
    use gents_protocol::plugin::{PluginExecutionReceipt, PluginExecutionVerdict};

    use super::{build_step_input, StepKind};
    use crate::document_config::Tools;
    use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
    use crate::pack::{write_home_install, HomePackInstall, InstalledVia, PackKind};
    use crate::plugin::executor::PluginExecutor;
    use crate::plugin::store::InstalledPlugin;
    use crate::plugin_job::driver::JobRuntime;
    use crate::plugin_job::records::{
        write_pin_in_txn, write_progress_in_txn, write_step_state_in_txn, HostProgressFacts,
    };
    use crate::plugin_job::JobBinding;
    use crate::tool_call_lifecycle::admission_fixture::{
        fixture_job_pin, publish_accepted_on_claimed_request,
    };
    use crate::tool_call_lifecycle::AwaitMode;

    pub(crate) fn node_did() -> &'static str {
        use crate::NodeIdentity as _;

        static IDENTITY: std::sync::OnceLock<crate::KeyIdentity> = std::sync::OnceLock::new();
        IDENTITY
            .get_or_init(|| {
                let directory = tempfile::tempdir().unwrap();
                crate::KeyIdentity::load_or_create(directory.path().join("node.key"), None).unwrap()
            })
            .did()
    }
    const SCRIPT: &[u8] = b"fn step(input) { input }";

    fn sha(bytes: &[u8]) -> String {
        use sha2::Digest;
        format!("sha256:{:x}", sha2::Sha256::digest(bytes))
    }

    fn plugin() -> InstalledPlugin {
        InstalledPlugin {
            namespace: "acme".into(),
            name: "engine".into(),
            version: "1.0.0".into(),
            digest: format!("sha256:{}", "e".repeat(64)),
            language: "rust".into(),
            declaration: crate::pack::PackPlugin {
                name: "engine".into(),
                asset_mounts: vec![gents_protocol::job::AssetMountDeclaration {
                    class: "workflows".into(),
                }],
                ..Default::default()
            },
            granted: None,
            instructions: None,
            owner_pack_coordinate: Some("acme/engine".into()),
            owner_pack_digest: None,
            model_binding: None,
            data_unique_keys: Default::default(),
            data_collection_fields: Default::default(),
        }
    }

    pub(crate) fn live_settings(policy: JobPolicy) -> PluginJobSettings {
        PluginJobSettings {
            agent_targets: vec!["a".into(), "b".into()],
            max_live: 3,
            max_spawns: 10,
            max_messages_per_slot: 2,
            result_bytes: 2048,
            max_wall_secs: 600,
            policy,
        }
    }

    fn trusting(packs: &[&str]) -> JobPolicy {
        JobPolicy {
            trusted_packs: packs.iter().map(|pack| (*pack).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn binding(settings: Option<PluginJobSettings>) -> JobBinding {
        JobBinding {
            tool_name: "engine".into(),
            plugin: serde_json::from_value(json!({"plugin": "acme/engine"})).unwrap(),
            settings,
        }
    }

    fn evidence() -> PluginExecutionReceipt {
        PluginExecutionReceipt {
            coordinate: "acme/engine".into(),
            artifact_digest: "sha256:engine".into(),
            input_digest: "sha256:input".into(),
            output_digest: Some("sha256:output".into()),
            authority: None,
            limits: None,
            verdict: PluginExecutionVerdict::Success,
        }
    }

    pub(crate) fn resume_block(row: &str) -> JobResume {
        JobResume {
            row_doc_id: row.to_owned(),
            engine: None,
            answer: None,
            budget: None,
        }
    }

    /// Records `coordinate` as an operator-installed documents pack whose
    /// `workflows/` class holds `script`.
    fn install(home: &Path, coordinate: &str, hex: &str, script: &[u8]) {
        let assets = format!("packs/.materialized/{coordinate}/{hex}");
        let dir = home.join(&assets).join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("flow.rhai"), script).unwrap();
        write_home_install(
            home,
            &HomePackInstall {
                coordinate: coordinate.into(),
                version: "1.0.0".into(),
                digest: format!("sha256:{hex}"),
                kind: PackKind::Documents,
                assets,
                dependency_assets: vec![],
                plugins: vec![],
                ux: Vec::new(),
                installed_at: "2026-01-01T00:00:00Z".into(),
                installed_via: Some(InstalledVia::Operator),
            },
        )
        .unwrap();
    }

    /// An embedded node holding the caller's Agent, context and Tools (with
    /// `settings` as the live job block of `acme/engine`), a claimed caller
    /// request, and an installed `acme/engine` under a temp home.
    pub(crate) struct Job {
        pub(crate) node: Arc<EmbeddedNode>,
        pub(crate) access: ConfigAccess,
        pub(crate) runtime: JobRuntime,
        home: tempfile::TempDir,
        cwd: tempfile::TempDir,
        request: RequestLifecycle,
        turn: usize,
        call_ids: BTreeMap<String, String>,
    }

    pub(crate) async fn new_job(
        settings: Option<&PluginJobSettings>,
        request_cwd: Option<&str>,
    ) -> Job {
        let node = open().await;
        let home = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        crate::plugin::store::write_record(home.path(), &plugin()).unwrap();
        let tools: Tools = serde_json::from_value(json!({
            "tools_id": "tools",
            "node_did": node_did(),
            "integrations": {"plugins": [{"plugin": "acme/engine", "job": settings}]},
        }))
        .unwrap();
        let access = ConfigAccess::local(node.clone());
        crate::config_client::write_tools_document(&access, &tools)
            .await
            .unwrap();
        insert(
            &node,
            "AgentContext",
            json!({"context_id": "context", "node_did": node_did(), "tools_id": "tools"}),
        )
        .await;
        insert(
            &node,
            "Agent",
            json!({"agent_id": "general", "node_did": node_did(), "context_id": "context",
                   "inference_profile_id": "inference", "enabled": true}),
        )
        .await;
        let now = chrono::Utc::now().to_rfc3339();
        insert(
            &node,
            "AgentSession",
            json!({"session_id": "session-1", "node_did": node_did(), "requester_did": null,
                   "agent_id": "general", "created_at": now}),
        )
        .await;
        let mut value = json!({
            "request_id": "request-1", "purpose": "normal", "node_did": node_did(),
            "agent_id": "general", "session_id": "session-1",
            "retry_parent_request": "", "retry_root_request": "request-1",
            "superseded_by_request": "", "content": "spawn", "lifecycle_state": "pending",
            "backend_id": "", "execution_origin": "interactive", "failure_reason": "",
            "created_at": now, "retry_count": 0, "max_retries": 3, "request_hop": 0,
        });
        if let Some(cwd) = request_cwd {
            value["input"] = json!({ "cwd": cwd });
        }
        insert(&node, "AgentRequest", value).await;
        let response = node
            .execute(&format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "request-1" }} }}) {{ {} }} }}"#,
                crate::watcher::AGENT_REQUEST_FIELDS
            ))
            .await;
        let row: gents_protocol::row::AgentRequestRow =
            crate::graphql::first_row(&response, "AgentRequest")
                .unwrap()
                .unwrap();
        let mut request = RequestLifecycle::new_with_node_did(
            node.clone(),
            "general",
            node_did(),
            row.try_into().unwrap(),
            60,
        );
        assert_eq!(request.claim().await.unwrap(), ClaimOutcome::Claimed);
        let runtime = JobRuntime {
            node: node.clone(),
            plugins: Arc::new(PluginExecutor::new(Some(home.path().to_path_buf()))),
            home: home.path().to_path_buf(),
            tool_root: Some(cwd.path().to_path_buf()),
        };
        Job {
            node,
            access,
            runtime,
            home,
            cwd,
            request,
            turn: 0,
            call_ids: BTreeMap::new(),
        }
    }

    impl Job {
        /// Publishes an accepted `tool` call on the caller request and returns
        /// its row doc id; `call_ids` keeps the row's stored tool call id.
        pub(crate) async fn row(&mut self, tool: &str, call_id: &str, arguments: Value) -> String {
            let call = publish_accepted_on_claimed_request(
                self.node.clone(),
                &mut self.request,
                node_did(),
                self.turn,
                tool,
                call_id,
                arguments,
                AwaitMode::Foreground,
                false,
            )
            .await
            .unwrap();
            self.turn += 1;
            let doc_id = call.doc_id().unwrap().to_owned();
            self.call_ids
                .insert(doc_id.clone(), call.tool_call_id().to_owned());
            doc_id
        }

        pub(crate) fn pin(&self, root: &str, resume_of: Option<&str>) -> JobPin {
            JobPin {
                plugin_ref: "acme/engine".into(),
                cwd: self.cwd.path().display().to_string(),
                chain_root_doc_id: root.to_owned(),
                resume_of: resume_of.map(str::to_owned),
                handle: format!("handle-{root}"),
                ..fixture_job_pin()
            }
        }

        pub(crate) async fn write_pin(&self, row: &str, pin: &JobPin) {
            ConfigAccess::transact_local_write(&self.node, None, "test.input.pin", |txn| {
                Box::pin(async move { write_pin_in_txn(txn, row, pin).await })
            })
            .await
            .unwrap();
        }

        pub(crate) async fn write_progress(&self, row: &str, state: JobState, spawned: u32) {
            ConfigAccess::transact_local_write(&self.node, None, "test.input.progress", |txn| {
                Box::pin(async move {
                    let guest = JobProgress {
                        title: "audit".into(),
                        state,
                        pause_reason: None,
                        phases: Vec::new(),
                        active: Vec::new(),
                        spawned: 0,
                        budget: JobBudget {
                            total: 0,
                            spent: 0,
                            remaining: 0,
                        },
                        last_event: "scanning".into(),
                        last_event_at: None,
                        question: None,
                        observed_terminal_children: 0,
                        revision: 0,
                    };
                    let host = HostProgressFacts {
                        spawned,
                        budget: JobBudget {
                            total: 4,
                            spent: spawned,
                            remaining: 4 - spawned,
                        },
                        observed_terminal_children: 0,
                        state,
                        pause_reason: None,
                    };
                    write_progress_in_txn(txn, row, 0, guest, host).await
                })
            })
            .await
            .unwrap();
        }

        pub(crate) async fn write_step_state(&self, row: &str, resume: Option<&JobResume>) {
            ConfigAccess::transact_local_write(&self.node, None, "test.input.step_state", |txn| {
                Box::pin(
                    async move { write_step_state_in_txn(txn, row, &evidence(), resume).await },
                )
            })
            .await
            .unwrap();
        }

        async fn later(&self, row: &str) -> anyhow::Result<(JobStepInput, u64)> {
            build_step_input(&self.access, &self.runtime, row, StepKind::Later).await
        }
    }

    fn run_args() -> Value {
        json!({
            "argv": ["run", "--fast"],
            "args": {"n": 1, "nested": {"a": [1, 2]}},
            "path": "/work/flow",
            "script": "fn main() {}",
            "agent_budget": 7,
        })
    }

    #[tokio::test]
    async fn every_c9_argument_is_echoed_on_every_step() {
        let live = live_settings(JobPolicy::default());
        let mut job = new_job(Some(&live), None).await;
        let call = run_args();
        let row = job.row("workflow", "call-1", call.clone()).await;
        let check = |input: &JobStepInput| {
            assert_eq!(input.argv, ["run", "--fast"]);
            assert_eq!(serde_json::to_value(&input.args).unwrap(), call["args"]);
            assert_eq!(input.path.as_deref(), Some("/work/flow"));
            assert_eq!(input.script.as_deref(), Some("fn main() {}"));
            assert_eq!(input.agent_budget, Some(7));
        };
        let binding = binding(Some(live.clone()));
        let (step0, revision) = build_step_input(
            &job.access,
            &job.runtime,
            &row,
            StepKind::Step0 { binding: &binding },
        )
        .await
        .unwrap();
        check(&step0);
        assert_eq!(revision, 0);
        job.write_pin(&row, &job.pin(&row, None)).await;
        let (later, _) = job.later(&row).await.unwrap();
        check(&later);
        let bare = job
            .row("workflow", "call-2", json!({"argv": ["list"]}))
            .await;
        job.write_pin(&bare, &job.pin(&bare, None)).await;
        let (input, _) = job.later(&bare).await.unwrap();
        assert_eq!(input.argv, ["list"]);
        assert_eq!(input.args, None);
        assert_eq!(input.path, None);
        assert_eq!(input.script, None);
        assert_eq!(input.agent_budget, None);
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn root_call_is_present_only_on_validated_resume_rows() {
        let live = live_settings(JobPolicy::default());
        let mut job = new_job(Some(&live), None).await;
        let root = job.row("workflow", "call-1", run_args()).await;
        let resumed = job
            .row("workflow", "call-2", json!({"argv": ["resume", "h"]}))
            .await;
        job.write_pin(&root, &job.pin(&root, None)).await;
        job.write_pin(&resumed, &job.pin(&root, Some(&root))).await;
        let (plain, _) = job.later(&resumed).await.unwrap();
        assert_eq!(plain.root_call, None);

        job.write_step_state(&root, None).await;
        let (root_input, _) = job.later(&root).await.unwrap();
        assert_eq!(root_input.root_call, None);
        assert_eq!(root_input.job.chain, std::slice::from_ref(&root));

        job.write_step_state(&resumed, Some(&resume_block(&root)))
            .await;
        let (input, _) = job.later(&resumed).await.unwrap();
        let call = run_args();
        assert_eq!(
            input.root_call,
            Some(JobRootCall {
                argv: vec!["run".into(), "--fast".into()],
                args: call["args"].as_object().cloned(),
                path: Some("/work/flow".into()),
                agent_budget: Some(7),
            })
        );
        assert_eq!(input.argv, ["resume", "h"]);
        assert_eq!(input.job.chain, [root.clone(), resumed.clone()]);
        assert_eq!(input.job.chain_root_doc_id, root);
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn ceiling_carries_live_intersect_pinned_limits_and_dynamic_targets() {
        let live = live_settings(JobPolicy {
            dynamic: DynamicMode::Run,
            dynamic_targets: vec!["a".into()],
            ..Default::default()
        });
        let mut job = new_job(Some(&live), None).await;
        let row = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        let mut pin = job.pin(&row, None);
        pin.agent_budget = 4;
        pin.ceiling_at_pin = gents_protocol::job::JobCeiling {
            targets: vec!["b".into(), "c".into()],
            max_live: 5,
            max_spawns: 6,
            max_messages_per_slot: 4,
            result_bytes: 4096,
            max_wall_secs: 600,
        };
        job.write_pin(&row, &pin).await;
        job.write_progress(&row, JobState::Running, 2).await;
        let (input, revision) = job.later(&row).await.unwrap();
        assert_eq!(revision, 1);
        assert_eq!(
            input.ceiling,
            JobCeilingView {
                targets: vec!["b".into()],
                max_live: 3,
                remaining_spawns: 2,
                max_messages_per_slot: 2,
                result_bytes: 2048,
                dynamic: DynamicMode::Run,
                dynamic_targets: vec!["a".into()],
            }
        );
        assert_eq!(input.pin, Some(pin));
        assert_eq!(input.origin, Some(JobOrigin::Installed));
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn children_keep_each_slots_highest_attempt() {
        let live = live_settings(JobPolicy::default());
        let mut job = new_job(Some(&live), None).await;
        let row = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        job.write_pin(&row, &job.pin(&row, None)).await;
        for (slot, attempt, request_id) in [
            ("slot-a", 1, "a1"),
            ("slot-a", 0, "a0"),
            ("slot-b", 0, "b0"),
        ] {
            request(
                &job.node,
                &row,
                request_id,
                &job_session_id("root", slot, attempt),
                &job_spawn_key("root", slot, attempt),
                "agent-a",
                "processing",
                Some(request_id),
                "2026-01-01T00:00:00Z",
                None,
            )
            .await;
        }
        let (input, _) = job.later(&row).await.unwrap();
        let kept: Vec<_> = input
            .children
            .iter()
            .map(|(slot, view)| (slot.as_str(), view.attempt, view.request_id.as_str()))
            .collect();
        assert_eq!(kept, [("slot-a", 1, "a1"), ("slot-b", 0, "b0")]);
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn jobs_state_follows_the_row_lifecycle_first() {
        let live = live_settings(JobPolicy::default());
        let mut job = new_job(Some(&live), None).await;
        let mut rows = Vec::new();
        for (call, lifecycle, progress) in [
            ("call-1", Some("completed"), Some(JobState::Paused)),
            ("call-2", Some("completed"), Some(JobState::AwaitingUser)),
            ("call-3", Some("failed"), Some(JobState::Running)),
        ] {
            let row = job.row("workflow", call, json!({"argv": ["run"]})).await;
            job.write_pin(&row, &job.pin(&row, None)).await;
            if let Some(state) = progress {
                job.write_progress(&row, state, 0).await;
            }
            if let Some(lifecycle) = lifecycle {
                set_lifecycle(&job.node, &row, lifecycle).await;
            }
            rows.push(row);
        }
        let unpinned = job.row("workflow", "call-4", json!({"argv": ["x"]})).await;
        let other_tool = job.row("other", "call-5", json!({"argv": ["x"]})).await;
        job.write_pin(&other_tool, &job.pin(&other_tool, None))
            .await;
        let live_row = job
            .row("workflow", "call-6", json!({"argv": ["run"]}))
            .await;
        job.write_pin(&live_row, &job.pin(&live_row, None)).await;
        rows.push(live_row.clone());
        let (input, _) = job.later(&live_row).await.unwrap();
        let mut expected: Vec<JobRowView> = rows
            .iter()
            .zip([
                JobState::Paused,
                JobState::AwaitingUser,
                JobState::Failed,
                JobState::Running,
            ])
            .map(|(row, state)| JobRowView {
                row_doc_id: row.clone(),
                tool_call_id: job.call_ids[row].clone(),
                handle: format!("handle-{row}"),
                state,
            })
            .collect();
        expected.sort_by(|a, b| a.row_doc_id.cmp(&b.row_doc_id));
        assert_eq!(input.jobs, expected);
        assert!(input.jobs.iter().all(|view| view.row_doc_id != unpinned));
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn host_overwritten_fields_replace_guest_shaped_values() {
        let live = live_settings(JobPolicy::default());
        let mut job = new_job(Some(&live), None).await;
        let guest = json!({
            "argv": ["run"],
            "args": {"pin": {"bogus": 1}, "ceiling": {"max_live": 99}},
            "pin": {"bogus": 1},
            "origin": "installed",
            "caller": {"cwd": "/evil", "node_did": "did:evil"},
            "ceiling": {"max_live": 99},
            "mounts": [{"pack": "evil"}],
            "verified_script": {"source": "evil", "digest": "evil"},
            "root_call": {"argv": ["evil"]},
            "control": {"kind": "answer"},
            "scratch": {"x": "/evil"},
            "jobs": [],
        });
        let row = job.row("workflow", "call-1", guest.clone()).await;
        let pin = job.pin(&row, None);
        job.write_pin(&row, &pin).await;
        let (input, _) = job.later(&row).await.unwrap();
        assert_eq!(serde_json::to_value(&input.args).unwrap(), guest["args"]);
        assert_eq!(input.pin, Some(pin.clone()));
        assert_eq!(input.origin, Some(pin.origin));
        assert_eq!(input.caller.cwd, pin.cwd);
        assert_eq!(input.caller.node_did, node_did());
        assert_eq!(input.ceiling.max_live, 2);
        assert!(input.mounts.is_empty());
        assert_eq!(input.verified_script, None);
        assert_eq!(input.root_call, None);
        assert_eq!(input.control, None);
        assert!(input.scratch.is_empty());
        assert_eq!(input.jobs.len(), 1);
        job.node.shutdown().await;
    }

    fn json_keys(value: &Value, keys: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    keys.push(key.clone());
                    json_keys(child, keys);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| json_keys(item, keys)),
            _ => {}
        }
    }

    #[test]
    fn job_contract_keys_have_no_secret_substring() {
        let pin = fixture_job_pin();
        let child = gents_protocol::job::JobChildView {
            attempt: 1,
            session_id: "s".into(),
            request_id: "r".into(),
            title: "t".into(),
            state: JobChildState::Live,
            output_bytes: 1,
            truncated: false,
            usage: JobUsage {
                input: 1,
                output: 2,
                total: 3,
            },
            duration_ms: 4,
        };
        let progress = JobProgress {
            title: "audit".into(),
            state: JobState::Running,
            pause_reason: None,
            phases: Vec::new(),
            active: vec![gents_protocol::job::JobActiveChild {
                slot: "slot-a".into(),
                title: "t".into(),
                session_id: "s".into(),
            }],
            spawned: 1,
            budget: JobBudget {
                total: 4,
                spent: 1,
                remaining: 3,
            },
            last_event: "scanning".into(),
            last_event_at: Some("2026-01-01T00:00:00Z".into()),
            question: Some("q".into()),
            observed_terminal_children: 0,
            revision: 3,
        };
        let input = JobStepInput {
            protocol: 1,
            job: gents_protocol::job::JobRef {
                row_doc_id: "row".into(),
                chain_root_doc_id: "root".into(),
                chain: vec!["root".into(), "row".into()],
                handle: "h".into(),
            },
            caller: gents_protocol::job::JobCaller {
                session_id: "s".into(),
                request_id: "r".into(),
                request_doc_id: "rd".into(),
                tool_call_id: "c".into(),
                node_did: "did".into(),
                agent_id: "a".into(),
                request_hop: 1,
                correlation: Some("corr".into()),
                cwd: "/work".into(),
                started_by_job: false,
            },
            argv: vec!["run".into()],
            args: json!({"n": 1}).as_object().cloned(),
            path: Some("/p".into()),
            script: Some("fn main() {}".into()),
            agent_budget: Some(3),
            root_call: Some(JobRootCall {
                argv: vec!["run".into()],
                args: json!({"n": 1}).as_object().cloned(),
                path: Some("/p".into()),
                agent_budget: Some(3),
            }),
            pin: Some(pin),
            origin: Some(JobOrigin::Installed),
            verified_script: Some(JobVerifiedScript {
                source: "fn main() {}".into(),
                digest: "sha256:x".into(),
            }),
            mounts: vec![AssetMount {
                pack: "acme/op".into(),
                class: "workflows".into(),
                digest: "sha256:y".into(),
                path: "/m".into(),
                trusted: true,
            }],
            ceiling: JobCeilingView {
                targets: vec!["a".into()],
                max_live: 1,
                remaining_spawns: 1,
                max_messages_per_slot: 1,
                result_bytes: 1024,
                dynamic: DynamicMode::Check,
                dynamic_targets: vec!["a".into()],
            },
            children: [("slot-a".to_owned(), child)].into(),
            control: Some(JobControlEcho {
                kind: JobControlEchoKind::Answer,
                payload: Some(JobResume {
                    row_doc_id: "row".into(),
                    engine: Some(JobResumeEngine::Current),
                    answer: Some("yes".into()),
                    budget: Some(2),
                }),
            }),
            scratch: [("notes.md".to_owned(), "/work/notes.md".to_owned())].into(),
            jobs: vec![JobRowView {
                row_doc_id: "row".into(),
                tool_call_id: "c".into(),
                handle: "h".into(),
                state: JobState::Running,
            }],
        };
        let output = JobStepOutput::Result {
            result: json!({"done": true}),
            writes: Vec::new(),
            scratch: vec![gents_protocol::job::JobScratch {
                name: "notes.md".into(),
                content: "x".into(),
            }],
            progress: Some(progress.clone()),
            control: Vec::new(),
            pin: None,
            resume: None,
        };
        let data = PluginDataDeclaration {
            collections: vec!["notes".into()],
            ops: vec!["insert".into()],
            stamped: [
                ("owner".to_owned(), StampedField::NodeDid),
                ("job_row".to_owned(), StampedField::JobRowDocId),
            ]
            .into(),
        };
        let mut keys = Vec::new();
        for value in [
            serde_json::to_value(&input).unwrap(),
            serde_json::to_value(&output).unwrap(),
            serde_json::to_value(&progress).unwrap(),
            serde_json::to_value(&data).unwrap(),
        ] {
            json_keys(&value, &mut keys);
        }
        assert!(keys.len() > 60);
        let secret: Vec<&String> = keys
            .iter()
            .filter(|key| crate::toolset::is_secret_env_name(key))
            .collect();
        assert!(secret.is_empty(), "{secret:?}");
        let value = serde_json::to_value(&input).unwrap();
        assert_eq!(crate::callback::strip_secret_fields(value.clone()), value);
    }

    #[tokio::test]
    async fn later_step_mounts_and_verified_script_come_from_the_pinned_plugin_record() {
        let live = live_settings(trusting(&["acme/op"]));
        let mut job = new_job(Some(&live), None).await;
        install(job.home.path(), "acme/op", "22", SCRIPT);
        let row = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        let mut pin = job.pin(&row, None);
        pin.source_digest = sha(SCRIPT);
        job.write_pin(&row, &pin).await;
        let (input, _) = job.later(&row).await.unwrap();
        let expected = crate::plugin::mounts::resolve(
            job.home.path(),
            &plugin(),
            Some(&live.policy),
            Some(Path::new(&pin.cwd)),
        )
        .unwrap();
        assert_eq!(expected.len(), 1);
        assert!(expected[0].trusted);
        assert_eq!(input.mounts, expected);
        assert_eq!(
            input.verified_script,
            Some(JobVerifiedScript {
                source: String::from_utf8(SCRIPT.to_vec()).unwrap(),
                digest: pin.source_digest.clone(),
            })
        );

        let stranger = job
            .row("workflow", "call-2", json!({"argv": ["run"]}))
            .await;
        let mut other = job.pin(&stranger, None);
        other.source_digest = sha(b"a script no mount holds");
        job.write_pin(&stranger, &other).await;
        let (input, _) = job.later(&stranger).await.unwrap();
        assert_eq!(input.verified_script, None);
        assert_eq!(input.mounts, expected);

        let missing = job
            .row("workflow", "call-3", json!({"argv": ["run"]}))
            .await;
        let mut absent = job.pin(&missing, None);
        absent.plugin_ref = "acme/missing".into();
        job.write_pin(&missing, &absent).await;
        let error = job.later(&missing).await.unwrap_err();
        assert!(
            format!("{}", ErrorChain(error.as_ref())).contains("acme/missing"),
            "{}",
            ErrorChain(error.as_ref())
        );
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn step0_takes_the_plugin_from_the_binding_and_cwd_from_the_tool_root() {
        let bound = live_settings(trusting(&["acme/op"]));
        let mut job = new_job(None, None).await;
        install(job.home.path(), "acme/op", "22", SCRIPT);
        let row = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        let binding = binding(Some(bound.clone()));
        let step0 = |runtime: JobRuntime| {
            let (access, binding, row) = (job.access.clone(), binding.clone(), row.clone());
            async move {
                build_step_input(
                    &access,
                    &runtime,
                    &row,
                    StepKind::Step0 { binding: &binding },
                )
                .await
                .unwrap()
                .0
            }
        };

        let none = JobRuntime {
            tool_root: None,
            ..job.runtime.clone()
        };
        let input = step0(none).await;
        assert_eq!(input.caller.cwd, "");
        assert!(input.scratch.is_empty());

        let notes = job
            .cwd
            .path()
            .join(".gents/workflows/runs")
            .join(&job.call_ids[&row])
            .join("notes.md");
        std::fs::create_dir_all(notes.parent().unwrap()).unwrap();
        std::fs::write(&notes, "x").unwrap();
        let input = step0(job.runtime.clone()).await;
        let tool_root = job.cwd.path().display().to_string();
        assert_eq!(input.caller.cwd, tool_root);
        assert_eq!(
            input.scratch,
            [("notes.md".to_owned(), notes.display().to_string())].into()
        );
        let record = crate::plugin::store::read_record(job.home.path(), "acme", "engine").unwrap();
        let expected = crate::plugin::mounts::resolve(
            job.home.path(),
            &record,
            Some(&bound.policy),
            Some(job.cwd.path()),
        )
        .unwrap();
        assert_eq!(expected.len(), 1);
        assert_eq!(input.mounts, expected);
        assert_eq!(input.pin, None);
        assert_eq!(input.origin, None);
        assert_eq!(input.verified_script, None);
        assert_eq!(input.job.chain, std::slice::from_ref(&row));
        assert_eq!(input.job.chain_root_doc_id, row);
        job.node.shutdown().await;

        let request_cwd = tempfile::tempdir().unwrap();
        let named = request_cwd.path().display().to_string();
        let mut job = new_job(None, Some(&named)).await;
        let row = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        let (input, _) = build_step_input(
            &job.access,
            &job.runtime,
            &row,
            StepKind::Step0 { binding: &binding },
        )
        .await
        .unwrap();
        assert_eq!(input.caller.cwd, named);
        assert!(input.scratch.is_empty());
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn scratch_listing_ignores_a_tool_call_id_that_is_not_one_path_component() {
        let cwd = tempfile::tempdir().unwrap();
        let runs = cwd.path().join(".gents/workflows/runs");
        std::fs::create_dir_all(&runs).unwrap();
        std::fs::write(runs.join("leak.md"), "x").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("leak.md"), "x").unwrap();
        for id in ["..", ".", "", "a/b", outside.path().to_str().unwrap()] {
            assert!(
                super::scratch_files(Some(cwd.path()), id).is_empty(),
                "{id}"
            );
        }
        std::fs::create_dir_all(runs.join("call-1")).unwrap();
        std::fs::write(runs.join("call-1/notes.md"), "x").unwrap();
        assert_eq!(super::scratch_files(Some(cwd.path()), "call-1").len(), 1);
    }

    #[tokio::test]
    async fn control_echoes_the_job_step_state_resume_block() {
        let live = live_settings(JobPolicy::default());
        let mut job = new_job(Some(&live), None).await;
        let mut cases = Vec::new();
        for (call, resume, kind) in [
            (
                "call-1",
                Some(JobResume {
                    answer: Some("yes".into()),
                    engine: Some(JobResumeEngine::Current),
                    ..resume_block("prior")
                }),
                Some(JobControlEchoKind::Answer),
            ),
            (
                "call-2",
                Some(JobResume {
                    engine: Some(JobResumeEngine::Current),
                    ..resume_block("prior")
                }),
                Some(JobControlEchoKind::EngineCurrent),
            ),
            (
                "call-3",
                Some(JobResume {
                    budget: Some(3),
                    ..resume_block("prior")
                }),
                Some(JobControlEchoKind::Resume),
            ),
            ("call-4", None, None),
        ] {
            let row = job.row("workflow", call, json!({"argv": ["run"]})).await;
            job.write_pin(&row, &job.pin(&row, None)).await;
            if resume.is_some() {
                job.write_step_state(&row, resume.as_ref()).await;
            }
            cases.push((row, resume, kind));
        }
        for (row, resume, kind) in cases {
            let (input, _) = job.later(&row).await.unwrap();
            assert_eq!(
                input.control,
                kind.map(|kind| JobControlEcho {
                    kind,
                    payload: resume.clone(),
                })
            );
        }
        job.node.shutdown().await;
    }

    #[tokio::test]
    async fn step_input_echoes_the_persisted_resume_block() {
        let live = live_settings(JobPolicy::default());
        let mut job = new_job(Some(&live), None).await;
        let resumed = job
            .row("workflow", "call-1", json!({"argv": ["run"]}))
            .await;
        let plain = job
            .row("workflow", "call-2", json!({"argv": ["run"]}))
            .await;
        let block = JobResume {
            answer: Some("yes".into()),
            ..resume_block("prior")
        };
        for row in [&resumed, &plain] {
            job.write_pin(row, &job.pin(row, None)).await;
        }
        job.write_step_state(&resumed, Some(&block)).await;
        let (input, _) = job.later(&resumed).await.unwrap();
        assert_eq!(
            input.control,
            Some(crate::plugin_job::control::resume_echo(&block))
        );
        let (input, _) = job.later(&plain).await.unwrap();
        assert_eq!(input.control, None);
        job.node.shutdown().await;
    }
}
