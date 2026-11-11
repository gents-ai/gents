//! Firing one Task on demand as the tool layer sees it. The Task delivery
//! owner lives above this crate; the runtime hands its implementation in.

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use gents_documents::Task;
use gents_protocol::request_input::RequestInput;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity;
use serde::Serialize;
use serde_json::Value;

/// What a caller supplies for one fire.
#[derive(Debug, Clone)]
pub struct TaskRunOptions {
    /// A JSON object bound as the `args.*` template scope.
    pub args: Value,
    /// A goal-backed Task's stable invocation key; otherwise a session label.
    pub session_id: Option<String>,
    /// An existing session to continue with this Task.
    pub continue_session: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRunOutput {
    pub task_id: String,
    pub agent_did: String,
    pub node_did: String,
    pub request_id: String,
    pub session_id: String,
    pub request_doc_id: String,
    pub input: RequestInput,
    pub status: &'static str,
    pub duplicate: bool,
}

/// A Task loaded for firing, with its enabled agent verified.
pub struct RunnableTask {
    pub task: Task,
    pub doc_id: String,
}

#[async_trait]
pub trait TaskRunService: Send + Sync {
    /// Loads an enabled Task whose agent is enabled, or fails naming which is not.
    async fn read_runnable_task(
        &self,
        access: &ConfigAccess,
        node_did: &str,
        task_id: &str,
    ) -> Result<RunnableTask>;

    /// Fires `runnable` once, now, signed as `signer`, which must be the actor
    /// `access` acts for, and admits it through the Task delivery owner.
    async fn enqueue_task_run(
        &self,
        access: &ConfigAccess,
        node_did: &str,
        signer: &dyn NodeIdentity,
        runnable: RunnableTask,
        options: &TaskRunOptions,
    ) -> Result<TaskRunOutput>;
}

/// The Task runtime of a context that has none attached. Every call fails.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoTaskRunService;

fn no_runtime() -> anyhow::Error {
    anyhow!("no task runtime is attached")
}

#[async_trait]
impl TaskRunService for NoTaskRunService {
    async fn read_runnable_task(
        &self,
        _access: &ConfigAccess,
        _node_did: &str,
        _task_id: &str,
    ) -> Result<RunnableTask> {
        Err(no_runtime())
    }

    async fn enqueue_task_run(
        &self,
        _access: &ConfigAccess,
        _node_did: &str,
        _signer: &dyn NodeIdentity,
        _runnable: RunnableTask,
        _options: &TaskRunOptions,
    ) -> Result<TaskRunOutput> {
        Err(no_runtime())
    }
}
