//! Pre-commit scratch files and slot interrupts. `admit_scratch` is Lean
//! `scratchAdmitted`; `write_scratch` admits each file through it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::job::{JobEffects, JobScratch, JobSpawn};
use gents_protocol::request_admission::AgentRequestCreate;
use sha2::{Digest, Sha256};

use crate::lifecycle::{
    next_request_hop, RequestHopCause, SessionMessageCause, SessionMessageTarget,
};
use crate::plugin_job::admit::{StepChild, StepContext, StepRow};
use crate::plugin_job::keys::{job_message_key, job_session_id, job_spawn_key};
use crate::session_origin::SessionScope;

const SCRATCH_MAX_BYTES: u64 = 1_048_576;
const SCRATCH_MAX_FILES: u32 = 32;

pub fn admit_scratch(
    cwd: &str,
    cwd_allowed: &dyn Fn(&str) -> bool,
    tool_call_id: &str,
    name: &str,
    size: u64,
    count: u32,
) -> Option<String> {
    if !scratch_name(name)
        || !scratch_name(tool_call_id)
        || size > SCRATCH_MAX_BYTES
        || count > SCRATCH_MAX_FILES
    {
        return None;
    }
    let path = format!("{cwd}/.gents/workflows/runs/{tool_call_id}/{name}");
    cwd_allowed(&path).then_some(path)
}

pub(super) fn scratch_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

pub fn write_scratch(cwd: &Path, tool_call_id: &str, files: &[JobScratch]) -> Result<()> {
    let cwd_str = cwd.to_str().context("cwd is not utf-8")?;
    for file in files {
        let dir = cwd.join(".gents/workflows/runs").join(tool_call_id);
        let count = distinct_count(&dir, &file.name);
        let size = u64::try_from(file.content.len()).unwrap_or(u64::MAX);
        let path = admit_scratch(
            cwd_str,
            &|path| stays_under(cwd, path),
            tool_call_id,
            &file.name,
            size,
            count,
        )
        .context("scratch file refused")?;
        let path = PathBuf::from(&path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, file.content.as_bytes())?;
    }
    Ok(())
}

fn distinct_count(dir: &Path, name: &str) -> u32 {
    let mut names = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                if let Some(file_name) = entry.file_name().to_str() {
                    names.insert(file_name.to_owned());
                }
            }
        }
    }
    names.insert(name.to_owned());
    u32::try_from(names.len()).unwrap_or(u32::MAX)
}

fn stays_under(cwd: &Path, path: &str) -> bool {
    let Ok(mut resolved) = cwd.canonicalize() else {
        return false;
    };
    let Ok(relative) = Path::new(path).strip_prefix(cwd) else {
        return false;
    };
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                resolved.push(part);
                match std::fs::symlink_metadata(&resolved) {
                    Ok(meta) if meta.file_type().is_symlink() => return false,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return false,
                }
            }
            _ => return false,
        }
    }
    true
}

pub async fn interrupt_slots(
    node: &Arc<EmbeddedNode>,
    caller: &SessionScope,
    slots: &[SessionScope],
) -> Result<()> {
    for target in slots {
        if let Some(reason) =
            crate::session_message::interrupt_refusal(node, caller, target).await?
        {
            anyhow::bail!(reason);
        }
        crate::session_message::interrupt_session(node, target).await?;
    }
    Ok(())
}

pub(crate) struct JobContext<'a> {
    pub(crate) ctx: &'a StepContext,
    pub(crate) row: &'a StepRow,
    pub(crate) cause: &'a SessionMessageCause,
}

pub(crate) async fn stage_effects_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    job: &JobContext<'_>,
    fresh: &[&JobSpawn],
    effects: &JobEffects,
) -> Result<()> {
    stage_recorded_effects_in_txn(txn, job, fresh, effects)
        .await
        .map(|_| ())
}

/// `stage_effects_in_txn` returning the staged requests. A failed step commit
/// may still have landed: the caller keeps these for `recover_step_commit`.
pub(crate) async fn stage_recorded_effects_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    job: &JobContext<'_>,
    fresh: &[&JobSpawn],
    effects: &JobEffects,
) -> Result<Vec<AgentRequestCreate>> {
    let mut staged = Vec::new();
    for spawn in fresh {
        staged.push(stage_spawn(txn, job, spawn).await?);
    }
    let mut issued = BTreeMap::<(String, u32), u32>::new();
    for message in &effects.message {
        let (slot, attempt, target, admitted) = {
            let child =
                latest(job.row, &message.slot).context("no admitted attempt for message slot")?;
            (
                child.slot.clone(),
                child.attempt,
                child.target.clone(),
                child.messages,
            )
        };
        let seen = issued.entry((slot.clone(), attempt)).or_insert(admitted);
        let k = 1 + *seen;
        *seen += 1;
        let session_id = job_session_id(&job.row.root, &slot, attempt);
        let hop = message_hop(txn, job, &target.0, &session_id).await?;
        staged.push(
            stage_request(
                txn,
                job.cause,
                &target.0,
                &target.1,
                session_id,
                &message.text,
                None,
                job_message_key(&job.row.root, &slot, attempt, k),
                hop,
                None,
                true,
            )
            .await?,
        );
    }
    Ok(staged)
}

/// Staging commits atomically with the step, so after a commit error the step
/// landed only if every staged retry key holds its staged fingerprint; a step
/// that staged nothing has no witness and keeps the commit error.
pub(crate) async fn recover_step_commit(
    node: &std::sync::Arc<EmbeddedNode>,
    identity: Option<identity::Did>,
    commit_error: anyhow::Error,
    staged: &[AgentRequestCreate],
) -> Result<()> {
    let recovery = crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        identity,
        "plugin_job.recover_step_commit",
        move |txn| {
            Box::pin(async move {
                for create in staged {
                    let retry_key = create.retry_key.as_deref().context("staged request lacks a retry_key")?;
                    match one_request(txn, retry_key).await? {
                        None => return Ok(false),
                        Some(existing) if same_fingerprint(&existing, create) => {}
                        Some(_) => bail!("job staging integrity error: retry key {retry_key} does not match the staged request"),
                    }
                }
                Ok(!staged.is_empty())
            })
        },
    )
    .await;
    match recovery {
        Ok(true) => Ok(()),
        Ok(false) => Err(commit_error),
        Err(recovery_error) => Err(anyhow::anyhow!(
            "job step recovery failed after ambiguous commit: {recovery_error:#}; original commit error: {commit_error:#}"
        )),
    }
}

fn latest<'a>(row: &'a StepRow, slot: &str) -> Option<&'a StepChild> {
    row.children
        .iter()
        .filter(|child| {
            child.slot == slot
                && child.retry_key == job_spawn_key(&row.root, &child.slot, child.attempt)
        })
        .max_by_key(|child| child.attempt)
}

async fn stage_spawn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    job: &JobContext<'_>,
    spawn: &JobSpawn,
) -> Result<AgentRequestCreate> {
    let (node_did, agent_id) = super::admit::resolve_target(job.ctx, &spawn.target)
        .cloned()
        .context("spawn target is not a known AgentTarget id")?;
    let own = job
        .ctx
        .target_hops
        .iter()
        .find(|(pair, _)| pair.0 == node_did && pair.1 == agent_id)
        .map_or(0, |(_, hop)| *hop);
    let hop = next_request_hop(
        RequestHopCause::CrossSession {
            cause_hop: job.row.caller_hop,
        },
        own,
    );
    let base = job
        .row
        .pin
        .as_ref()
        .map_or(&job.row.caller_cwd, |pin| &pin.cwd);
    // Admission resolved a relative spawn cwd against the pinned cwd; the child
    // gets that path, not one its own admission would resolve against the
    // server process's cwd.
    let cwd = Some(match &spawn.cwd {
        Some(dir) => Path::new(base).join(dir).to_string_lossy().into_owned(),
        None => base.clone(),
    });
    stage_request(
        txn,
        job.cause,
        &node_did,
        &agent_id,
        job_session_id(&job.row.root, &spawn.slot, spawn.attempt),
        &spawn.prompt,
        Some(spawn.title.as_str()),
        job_spawn_key(&job.row.root, &spawn.slot, spawn.attempt),
        hop,
        cwd,
        false,
    )
    .await
}

async fn message_hop(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    job: &JobContext<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<u32> {
    let own = crate::session::load_session_current_hop_in_txn(txn, node_did, session_id).await?;
    Ok(next_request_hop(
        RequestHopCause::CrossSession {
            cause_hop: job.row.caller_hop,
        },
        own,
    ))
}

#[expect(
    clippy::too_many_arguments,
    reason = "fields of the staged AgentRequest"
)]
async fn stage_request(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    cause: &SessionMessageCause,
    node_did: &str,
    agent_id: &str,
    session_id: String,
    content: &str,
    title: Option<&str>,
    retry_key: String,
    hop: u32,
    cwd: Option<String>,
    existing_destination: bool,
) -> Result<AgentRequestCreate> {
    let existing = one_request(txn, &retry_key).await?;
    let mut input = gents_protocol::request_input::RequestInput {
        cwd,
        ..Default::default()
    };
    if let Some(existing) = &existing {
        input.goal_assignment = existing
            .input
            .as_ref()
            .and_then(|input| input.goal_assignment.clone());
    } else if existing_destination && node_did == cause.caller_node_did {
        input = crate::goal::prepare_goal_input_in_txn(
            txn,
            node_did,
            &session_id,
            agent_id,
            Some(&cause.caller_node_did),
            input,
        )
        .await?;
    } else if existing_destination {
        let verifier = crate::identity::RegisteredIdentity::from_registered_did(
            cause.caller_node_did.clone(),
            None,
        )?;
        let state = crate::goal::load_session_goal_observation_state_in_txn(
            txn,
            &verifier,
            node_did,
            &session_id,
            Some(&cause.caller_node_did),
        )
        .await?;
        input = crate::goal::bind_goal_input(input, state)?;
    }
    let request_id = uuid::Uuid::new_v4().to_string();
    let create = crate::lifecycle::materialize::build_session_message_request_with_input(
        cause,
        &SessionMessageTarget {
            node_did: node_did.to_owned(),
            agent_id: agent_id.to_owned(),
            session_id: session_id.clone(),
        },
        content,
        title,
        &request_id,
        Some(retry_key.clone()),
        hop,
        input,
    )
    .await?;
    if let Some(existing) = existing {
        if same_fingerprint(&existing, &create) {
            return Ok(create);
        }
        bail!("job staging integrity error: retry key {retry_key} does not match the admitted request");
    }
    let plan = crate::session_message::Plan::request(session_id, hop, create.clone());
    crate::session_message::stage_planned_write_in_txn(txn, cause, &plan).await?;
    Ok(create)
}

type StoredRequest = gents_protocol::row::AgentRequestRow;

async fn one_request(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    retry_key: &str,
) -> Result<Option<StoredRequest>> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}) {{
                {}
            }} }}"#,
            crate::graphql::escape_graphql_string(retry_key),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    anyhow::ensure!(
        rows.len() <= 1,
        "job staging integrity error: retry key {retry_key} is not unique"
    );
    let stored: Option<StoredRequest> = rows
        .into_iter()
        .next()
        .map(serde_json::from_value)
        .transpose()
        .context("decoding job request fingerprint")?;
    if let Some(stored) = &stored {
        crate::request_admission::verify_request_receipt_signature(stored)?;
    }
    Ok(stored)
}

fn same_fingerprint(existing: &StoredRequest, create: &AgentRequestCreate) -> bool {
    existing.node_did.as_deref() == Some(create.node_did.as_str())
        && existing.agent_id.as_deref() == Some(create.agent_id.as_str())
        && existing.session_id.as_deref() == Some(create.session_id.as_str())
        && existing.content.as_deref().map(body_digest) == Some(body_digest(&create.content))
        && existing.request_hop == Some(i64::from(create.request_hop))
        && existing
            .input
            .as_ref()
            .and_then(|input| input.goal_assignment.as_ref())
            == create.input.goal_assignment.as_ref()
}

/// The sender header carries its send time, so a re-staged message compares
/// only what follows it.
fn body_digest(content: &str) -> String {
    let body = content
        .strip_prefix("[message ")
        .and_then(|rest| rest.split_once("]\n\n"))
        .map_or(content, |(_, body)| body);
    format!("{:x}", Sha256::digest(body.as_bytes()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use defra_node::EmbeddedNode;
    use serde_json::json;

    use super::{admit_scratch, interrupt_slots, write_scratch};
    use crate::config_client::ConfigAccess;
    use crate::session_origin::SessionScope;
    use gents_protocol::job::JobScratch;

    fn file(name: &str, content: &str) -> JobScratch {
        JobScratch {
            name: name.to_owned(),
            content: content.to_owned(),
        }
    }

    fn read(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap()
    }

    #[test]
    fn scratch_file_lands_under_runs_tool_call_id_before_commit() {
        let cwd = tempfile::tempdir().unwrap();
        write_scratch(cwd.path(), "call-1", &[file("notes.md", "hello")]).unwrap();
        let path = cwd.path().join(".gents/workflows/runs/call-1/notes.md");
        assert_eq!(read(&path), b"hello");
    }

    #[test]
    fn scratch_traversal_and_symlink_escape_are_refused() {
        let cwd = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        assert!(write_scratch(cwd.path(), "call-1", &[file("../escape.md", "no")]).is_err());
        assert!(!cwd.path().join("escape.md").exists());
        assert!(!outside.path().join("escape.md").exists());
        std::os::unix::fs::symlink(outside.path(), cwd.path().join(".gents")).unwrap();
        assert!(write_scratch(cwd.path(), "call-1", &[file("notes.md", "no")]).is_err());
        assert!(!outside
            .path()
            .join("workflows/runs/call-1/notes.md")
            .exists());
    }

    #[test]
    fn scratch_link_component_that_does_not_resolve_is_refused() {
        let cwd = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let missing = outside.path().join("missing");
        std::os::unix::fs::symlink(&missing, cwd.path().join(".gents")).unwrap();
        assert!(write_scratch(cwd.path(), "call-1", &[file("notes.md", "no")]).is_err());
        assert!(!missing.exists());
        std::fs::remove_file(cwd.path().join(".gents")).unwrap();
        std::fs::create_dir_all(cwd.path().join(".gents/workflows")).unwrap();
        std::os::unix::fs::symlink(&missing, cwd.path().join(".gents/workflows/runs")).unwrap();
        assert!(write_scratch(cwd.path(), "call-1", &[file("notes.md", "no")]).is_err());
        assert!(!missing.exists());
        std::fs::remove_file(cwd.path().join(".gents/workflows/runs")).unwrap();
        std::fs::create_dir_all(cwd.path().join(".gents/workflows/runs/call-1")).unwrap();
        std::os::unix::fs::symlink(
            &missing,
            cwd.path().join(".gents/workflows/runs/call-1/notes.md"),
        )
        .unwrap();
        assert!(write_scratch(cwd.path(), "call-1", &[file("notes.md", "no")]).is_err());
        assert!(!missing.exists());
    }

    #[test]
    fn scratch_33rd_file_is_refused() {
        let cwd = tempfile::tempdir().unwrap();
        for index in 0..32 {
            write_scratch(cwd.path(), "call-1", &[file(&format!("n{index}.txt"), "x")]).unwrap();
        }
        assert!(write_scratch(cwd.path(), "call-1", &[file("n32.txt", "x")]).is_err());
        assert!(!cwd
            .path()
            .join(".gents/workflows/runs/call-1/n32.txt")
            .exists());
    }

    #[test]
    fn scratch_file_over_1_mib_is_refused() {
        let cwd = tempfile::tempdir().unwrap();
        let big = "a".repeat(1_048_577);
        assert!(write_scratch(cwd.path(), "call-1", &[file("big.bin", &big)]).is_err());
        assert!(!cwd
            .path()
            .join(".gents/workflows/runs/call-1/big.bin")
            .exists());
    }

    #[test]
    fn scratch_rewrite_is_idempotent() {
        let cwd = tempfile::tempdir().unwrap();
        let notes = file("notes.md", "same");
        write_scratch(cwd.path(), "call-1", std::slice::from_ref(&notes)).unwrap();
        write_scratch(cwd.path(), "call-1", &[notes]).unwrap();
        let path = cwd.path().join(".gents/workflows/runs/call-1/notes.md");
        assert_eq!(read(&path), b"same");
    }

    #[test]
    fn resumed_row_writes_under_its_own_tool_call_id_and_earlier_paths_stay() {
        let cwd = tempfile::tempdir().unwrap();
        write_scratch(cwd.path(), "call-1", &[file("notes.md", "first")]).unwrap();
        write_scratch(cwd.path(), "call-2", &[file("notes.md", "second")]).unwrap();
        assert_eq!(
            read(&cwd.path().join(".gents/workflows/runs/call-1/notes.md")),
            b"first"
        );
        assert_eq!(
            read(&cwd.path().join(".gents/workflows/runs/call-2/notes.md")),
            b"second"
        );
    }

    async fn insert(
        node: &std::sync::Arc<EmbeddedNode>,
        collection: &str,
        value: serde_json::Value,
    ) -> String {
        ConfigAccess::transact_local_write(node, None, "test.plugin_job_effects", |txn| {
            let value = &value;
            Box::pin(async move {
                let query = format!(
                    "mutation($input: {collection}MutationInputArg!) {{create_{collection}(input:$input){{_docID}}}}"
                );
                let response = txn
                    .execute_with_variables(&query, &json!({ "input": value }))
                    .await?;
                gents_protocol::graphql::extract_mutation_doc_id(&response, collection)
            })
        })
        .await
        .unwrap()
    }

    async fn stamp(node: &std::sync::Arc<EmbeddedNode>, doc_id: &str) -> Option<String> {
        let escaped = crate::graphql::escape_graphql_string(doc_id);
        let response = node
            .execute(&format!(
                "{{AgentRequest(filter:{{_docID:{{_eq:\"{escaped}\"}}}}){{interrupt_requested_at}}}}"
            ))
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        response.data.unwrap()["AgentRequest"][0]["interrupt_requested_at"]
            .as_str()
            .map(str::to_owned)
    }

    #[tokio::test]
    async fn repeated_slot_interrupt_is_a_no_op() {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let parent = insert(&node, "AgentRequest", json!({"request_id":"parent","purpose":"normal","node_did":"owner","requester_did":null,"session_id":"caller","agent_id":"configured","lifecycle_state":"processing"})).await;
        insert(&node, "AgentSession", json!({"session_id":"child","node_did":"owner","requester_did":null,"agent_id":"configured","created_at":"2026-01-01T00:00:00Z","provenance":{"parent_request_doc_id":parent}})).await;
        let child = insert(&node, "AgentRequest", json!({"request_id":"child","purpose":"normal","node_did":"owner","requester_did":null,"session_id":"child","agent_id":"configured","lifecycle_state":"processing"})).await;
        let caller = SessionScope {
            node_did: "owner".into(),
            session_id: "caller".into(),
            requester_did: None,
        };
        let target = SessionScope {
            node_did: "owner".into(),
            session_id: "child".into(),
            requester_did: None,
        };
        assert!(stamp(&node, &child).await.is_none());
        interrupt_slots(&node, &caller, std::slice::from_ref(&target))
            .await
            .unwrap();
        let latched = stamp(&node, &child).await;
        assert!(latched.is_some());
        interrupt_slots(&node, &caller, &[target]).await.unwrap();
        assert_eq!(stamp(&node, &child).await, latched);
        node.shutdown().await;
    }

    #[test]
    fn admit_scratch_is_lean_scratch_admitted() {
        let allow = |_path: &str| true;
        let path = "/work/repo/.gents/workflows/runs/call-1/notes.md";
        assert_eq!(
            admit_scratch("/work/repo", &allow, "call-1", "notes.md", 0, 1),
            Some(path.into())
        );
        assert_eq!(
            admit_scratch("/work/repo", &allow, "call-1", "notes.md", 1_048_576, 32),
            Some(path.into())
        );
        for (tool_call_id, name, size, count) in [
            ("", "notes.md", 0, 1),
            (".", "notes.md", 0, 1),
            ("..", "notes.md", 0, 1),
            ("a/b", "notes.md", 0, 1),
            ("call-1", "", 0, 1),
            ("call-1", ".", 0, 1),
            ("call-1", "..", 0, 1),
            ("call-1", "a/b", 0, 1),
            ("call-1", "notes.md", 1_048_577, 1),
            ("call-1", "notes.md", 0, 33),
        ] {
            assert_eq!(
                admit_scratch("/work/repo", &allow, tool_call_id, name, size, count),
                None
            );
        }
        let refuse = |_path: &str| false;
        assert_eq!(
            admit_scratch("/work/repo", &refuse, "call-1", "notes.md", 0, 1),
            None
        );
    }

    use crate::background_tools::ProcessControlScope;
    use crate::lifecycle::SessionMessageCause;
    use crate::plugin_job::admit::{HostFacts, StepCall, StepChild, StepContext, StepRow};
    use crate::plugin_job::keys::{job_message_key, job_spawn_key};
    use crate::tool_call_lifecycle::admission_fixture::{
        published_admission, PublishedAdmissionOptions,
    };
    use crate::tool_surface::EndpointScope;
    use gents_protocol::job::{
        DynamicMode, JobCeiling, JobChildState, JobEffects, JobMessage, JobOrigin, JobPin,
        JobPolicy, JobSpawn, JobState,
    };

    struct Harness {
        node: Arc<EmbeddedNode>,
        path: std::path::PathBuf,
        cause: SessionMessageCause,
        ctx: StepContext,
        row: StepRow,
    }

    async fn harness(name: &str) -> Harness {
        let admission = published_admission(PublishedAdmissionOptions {
            name: name.to_owned(),
            real_identity: true,
            ..Default::default()
        })
        .await
        .unwrap();
        let tool = &admission.tool;
        let request_doc_id = tool.request_doc_id().unwrap().to_owned();
        let caller =
            crate::request_binding::load_agent_request_by_doc_id(&admission.node, &request_doc_id)
                .await
                .unwrap()
                .unwrap();
        let cause = SessionMessageCause {
            caller_node_did: admission.node_did.clone(),
            caller_agent_id: "caller-agent".to_owned(),
            caller_session_id: "caller-session".to_owned(),
            caller_requester_did: None,
            caller_request_id: caller.request_id,
            caller_request_doc_id: request_doc_id,
            caller_hop: 0,
            tool_call_id: tool.tool_call_id().to_owned(),
            tool_call_doc_id: tool.doc_id().unwrap().to_owned(),
            correlation: None,
        };
        let ctx = context(&admission.node_did);
        let row = running_row();
        Harness {
            node: admission.node.clone(),
            path: admission.path.clone(),
            cause,
            ctx,
            row,
        }
    }

    impl Harness {
        async fn close(self) {
            self.node.shutdown().await;
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }

    fn ceiling() -> JobCeiling {
        JobCeiling {
            targets: vec!["general".to_owned()],
            max_live: 4,
            max_spawns: 10,
            max_messages_per_slot: 8,
            result_bytes: 1024,
            max_wall_secs: 60,
        }
    }

    fn facts() -> HostFacts {
        HostFacts {
            engine_digest: "engine".to_owned(),
            plugin_ref: "ns/plugin".to_owned(),
            cwd: "/work".to_owned(),
            args_digest: "args".to_owned(),
            chain_root: "root".to_owned(),
            origin: JobOrigin::Installed,
            adopted_at: 1,
            resume_of: None,
            replayed_from: None,
            ceiling_at_pin: ceiling(),
        }
    }

    fn pin() -> JobPin {
        JobPin {
            script_id: "script".to_owned(),
            source_digest: "source".to_owned(),
            origin: JobOrigin::Installed,
            engine_digest: "engine".to_owned(),
            plugin_ref: "ns/plugin".to_owned(),
            agent_budget: 10,
            handle: "handle".to_owned(),
            cwd: "/work".to_owned(),
            adopted_at: 1,
            args_digest: "args".to_owned(),
            chain_root_doc_id: "root".to_owned(),
            resume_of: None,
            replayed_from: None,
            ceiling_at_pin: ceiling(),
        }
    }

    fn context(node_did: &str) -> StepContext {
        StepContext {
            live: ceiling(),
            available: true,
            caller_targets: EndpointScope::All,
            job_targets: EndpointScope::All,
            policy: JobPolicy {
                dynamic: DynamicMode::Run,
                dynamic_targets: vec!["general".to_owned()],
                trusted_packs: Vec::new(),
            },
            engine_digest: "engine".to_owned(),
            engine_provenance_operator: true,
            max_hop: Some(8),
            target_hops: vec![((node_did.to_owned(), "general".to_owned()), 0)],
            target_ids: vec![(
                "general".to_owned(),
                (node_did.to_owned(), "general".to_owned()),
            )],
            live_background: 0,
            started_by_job: false,
            cwd_allowed: Vec::new(),
            within: Vec::new(),
            adoption_facts: facts(),
            caller_scope: ProcessControlScope {
                request_id: "request".to_owned(),
                session_id: "session".to_owned(),
                node_did: node_did.to_owned(),
                requester_did: None,
            },
            control_targets: Vec::new(),
            resume_targets: Vec::new(),
            resume_facts: facts(),
        }
    }

    fn running_row() -> StepRow {
        StepRow {
            root: "root".to_owned(),
            row_doc_id: "row".to_owned(),
            caller_session: "session".to_owned(),
            caller_cwd: "/caller".to_owned(),
            handle: "handle".to_owned(),
            caller_hop: 0,
            call: StepCall {
                argv: vec!["run".to_owned()],
                args: None,
                script: None,
                agent_budget: None,
            },
            root_call: None,
            resume: None,
            pin: Some(pin()),
            children: Vec::new(),
            spent: 0,
            observed_terminal: 0,
            consecutive_failures: 0,
            state: JobState::Running,
        }
    }

    fn spawn(slot: &str, attempt: u32, prompt: &str, title: &str, cwd: Option<&str>) -> JobSpawn {
        JobSpawn {
            slot: slot.to_owned(),
            attempt,
            target: "general".to_owned(),
            prompt: prompt.to_owned(),
            title: title.to_owned(),
            cwd: cwd.map(str::to_owned),
        }
    }

    fn job_effects(spawn: Vec<JobSpawn>, message: Vec<JobMessage>) -> JobEffects {
        JobEffects {
            state: None,
            pin: None,
            resume: None,
            spawn,
            message,
            interrupt: Vec::new(),
            progress: None,
            writes: Vec::new(),
            scratch: Vec::new(),
        }
    }

    async fn stage(
        harness: &Harness,
        fresh: &[JobSpawn],
        effects: &JobEffects,
        row: &StepRow,
    ) -> anyhow::Result<()> {
        let fresh_refs: Vec<&JobSpawn> = fresh.iter().collect();
        let job = super::JobContext {
            ctx: &harness.ctx,
            row,
            cause: &harness.cause,
        };
        let job = &job;
        let fresh_refs = &fresh_refs;
        ConfigAccess::transact_local_write(
            &harness.node,
            None,
            "plugin_job.stage_effects",
            move |txn| {
                Box::pin(
                    async move { super::stage_effects_in_txn(txn, job, fresh_refs, effects).await },
                )
            },
        )
        .await
    }

    async fn by_key(node: &std::sync::Arc<EmbeddedNode>, key: &str) -> Vec<serde_json::Value> {
        let query = format!(
            r#"{{ AgentRequest(filter: {{ retry_key: {{ _eq: "{}" }} }}) {{
                _docID content request_hop created_at
                caused_by_parent_request_id caused_by_parent_request_doc_id
                caused_by_parent_tool_call_id caused_by_parent_tool_call_doc_id
                input
            }} }}"#,
            crate::graphql::escape_graphql_string(key)
        );
        let response =
            crate::graphql::graphql_with_transaction_retry(node, &query, "job staging read")
                .await
                .unwrap();
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default()
    }

    fn input_of(row: &serde_json::Value) -> serde_json::Value {
        match &row["input"] {
            serde_json::Value::String(text) => serde_json::from_str(text).unwrap(),
            other => other.clone(),
        }
    }

    #[tokio::test]
    async fn spawn_creates_one_request_with_the_job_row_as_calling_edge() {
        let harness = harness("job-stage-edge").await;
        let kept = spawn("kept", 0, "do this", "Kept", None);
        let skipped = spawn("old", 0, "skip", "Old", None);
        let effects = job_effects(vec![kept.clone(), skipped.clone()], Vec::new());
        stage(&harness, &[kept], &effects, &harness.row)
            .await
            .unwrap();
        let rows = by_key(&harness.node, &job_spawn_key("root", "kept", 0)).await;
        assert_eq!(rows.len(), 1);
        assert!(by_key(&harness.node, &job_spawn_key("root", "old", 0))
            .await
            .is_empty());
        let row = &rows[0];
        assert_eq!(
            row["caused_by_parent_request_id"],
            harness.cause.caller_request_id
        );
        assert_eq!(
            row["caused_by_parent_request_doc_id"],
            harness.cause.caller_request_doc_id
        );
        assert_eq!(
            row["caused_by_parent_tool_call_id"],
            harness.cause.tool_call_id
        );
        assert_eq!(
            row["caused_by_parent_tool_call_doc_id"],
            harness.cause.tool_call_doc_id
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn a_later_same_key_spawn_that_admission_dropped_is_not_staged() {
        let harness = harness("job-stage-same-key").await;
        let first = spawn("kept", 0, "first", "Kept", None);
        let later = spawn("kept", 0, "later", "Kept", None);
        let effects = job_effects(vec![first.clone(), later], Vec::new());
        stage(&harness, &[first], &effects, &harness.row)
            .await
            .unwrap();
        let rows = by_key(&harness.node, &job_spawn_key("root", "kept", 0)).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["content"],
            format!(
                "{}\n\nfirst",
                crate::lifecycle::materialize::session_message_sender_header(
                    &harness.cause,
                    rows[0]["created_at"].as_str().unwrap()
                )
            )
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn ambiguous_step_commit_recovers_only_the_same_staged_requests() {
        let harness = harness("job-stage-recover").await;
        let kept = spawn("kept", 0, "first", "Kept", None);
        let effects = job_effects(vec![kept.clone()], Vec::new());
        let fresh = [&kept];
        let job = super::JobContext {
            ctx: &harness.ctx,
            row: &harness.row,
            cause: &harness.cause,
        };
        let (job, fresh, effects) = (&job, &fresh, &effects);
        let staged = ConfigAccess::transact_local_write(
            &harness.node,
            None,
            "plugin_job.stage_effects",
            move |txn| {
                Box::pin(async move {
                    super::stage_recorded_effects_in_txn(txn, job, fresh, effects).await
                })
            },
        )
        .await
        .unwrap();
        let lost = || anyhow::anyhow!("commit acknowledgement lost");
        super::recover_step_commit(&harness.node, None, lost(), &staged)
            .await
            .unwrap();
        let mut absent = staged.clone();
        absent[0].retry_key = Some(job_spawn_key("root", "never", 0));
        let error = super::recover_step_commit(&harness.node, None, lost(), &absent)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "commit acknowledgement lost");
        let error = super::recover_step_commit(&harness.node, None, lost(), &[])
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "commit acknowledgement lost");
        let mut changed = staged.clone();
        changed[0].content = "second".to_owned();
        let error = super::recover_step_commit(&harness.node, None, lost(), &changed)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("integrity"), "{error:#}");
        harness.close().await;
    }

    #[tokio::test]
    async fn spawn_replay_with_the_same_fingerprint_is_a_no_op() {
        let harness = harness("job-stage-replay").await;
        let kept = spawn("kept", 0, "same", "Kept", None);
        let effects = job_effects(vec![kept.clone()], Vec::new());
        stage(
            &harness,
            std::slice::from_ref(&kept),
            &effects,
            &harness.row,
        )
        .await
        .unwrap();
        let first = by_key(&harness.node, &job_spawn_key("root", "kept", 0)).await;
        stage(&harness, &[kept], &effects, &harness.row)
            .await
            .unwrap();
        let second = by_key(&harness.node, &job_spawn_key("root", "kept", 0)).await;
        assert_eq!(second.len(), 1);
        assert_eq!(second[0]["_docID"], first[0]["_docID"]);
        harness.close().await;
    }

    #[tokio::test]
    async fn spawn_fingerprint_mismatch_is_an_integrity_error() {
        let harness = harness("job-stage-mismatch").await;
        let kept = spawn("kept", 0, "first", "Kept", None);
        let effects = job_effects(vec![kept.clone()], Vec::new());
        stage(
            &harness,
            std::slice::from_ref(&kept),
            &effects,
            &harness.row,
        )
        .await
        .unwrap();
        let changed = spawn("kept", 0, "second", "Kept", None);
        let again = job_effects(vec![changed.clone()], Vec::new());
        let error = stage(&harness, &[changed], &again, &harness.row)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("integrity"), "{error:#}");
        let rows = by_key(&harness.node, &job_spawn_key("root", "kept", 0)).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["content"],
            format!(
                "{}\n\nfirst",
                crate::lifecycle::materialize::session_message_sender_header(
                    &harness.cause,
                    rows[0]["created_at"].as_str().unwrap()
                )
            )
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn message_k_is_one_plus_the_admitted_messages_of_the_slot_attempt() {
        let harness = harness("job-stage-message").await;
        let mut row = harness.row.clone();
        row.children.push(StepChild {
            retry_key: job_spawn_key("root", "slot", 0),
            slot: "slot".to_owned(),
            attempt: 0,
            target: (harness.cause.caller_node_did.clone(), "general".to_owned()),
            state: JobChildState::Live,
            messages: 2,
        });
        let effects = job_effects(
            Vec::new(),
            vec![JobMessage {
                slot: "slot".to_owned(),
                text: "third".to_owned(),
            }],
        );
        stage(&harness, &[], &effects, &row).await.unwrap();
        let key = job_message_key("root", "slot", 0, 3);
        let rows = by_key(&harness.node, &key).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["content"],
            format!(
                "{}\n\nthird",
                crate::lifecycle::materialize::session_message_sender_header(
                    &harness.cause,
                    rows[0]["created_at"].as_str().unwrap()
                )
            )
        );
        assert!(
            by_key(&harness.node, &job_message_key("root", "slot", 0, 1))
                .await
                .is_empty()
        );
        harness.close().await;
    }

    #[tokio::test]
    async fn child_carries_the_spawn_title_guest_prompt_and_admitted_cwd() {
        let harness = harness("job-stage-child").await;
        let kept = spawn("kept", 0, "guest prompt", "Spawn Title", Some("/admitted"));
        let effects = job_effects(vec![kept.clone()], Vec::new());
        stage(&harness, &[kept], &effects, &harness.row)
            .await
            .unwrap();
        let rows = by_key(&harness.node, &job_spawn_key("root", "kept", 0)).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["content"],
            format!(
                "{}\n\nguest prompt",
                crate::lifecycle::materialize::session_message_sender_header(
                    &harness.cause,
                    rows[0]["created_at"].as_str().unwrap()
                )
            )
        );
        let input = input_of(&rows[0]);
        assert_eq!(input["initial_title"]["text"], "Spawn Title");
        assert_eq!(input["cwd"], "/admitted");
        harness.close().await;
    }

    #[tokio::test]
    async fn child_of_a_relative_cwd_gets_the_path_admission_checked() {
        let harness = harness("job-stage-relative-cwd").await;
        let kept = spawn(
            "kept",
            0,
            "guest prompt",
            "Spawn Title",
            Some("review_seeded_bug"),
        );
        let effects = job_effects(vec![kept.clone()], Vec::new());
        stage(&harness, &[kept], &effects, &harness.row)
            .await
            .unwrap();
        let rows = by_key(&harness.node, &job_spawn_key("root", "kept", 0)).await;
        assert_eq!(input_of(&rows[0])["cwd"], "/work/review_seeded_bug");
        harness.close().await;
    }
}

#[cfg(test)]
mod goal_input_tests;
