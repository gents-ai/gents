mod question;
mod queue;
pub mod workspace;
pub use question::{CommandApproval, FilesystemApproval, Request};
use queue::Queue;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::allowed;
use super::allowed::PluginAllowedError;
#[cfg(test)]
use crate::pack::BindAccess;

type Result<T, E = PluginApprovalError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum PluginApprovalError {
    #[error("creating the approval home")]
    CreateHome(#[source] std::io::Error),
    #[error("opening the approval lock")]
    OpenLock(#[source] std::io::Error),
    #[error("locking plugin approvals")]
    Lock(#[source] std::io::Error),
    #[error("approval file has no folder")]
    FileWithoutFolder,
    #[error("creating {}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: tempfile::PersistError,
    },
    #[error("creating the approval queue")]
    CreateQueue(#[source] std::io::Error),
    #[error("preparing the approval listener mark")]
    ListenerMark(#[source] tempfile::PersistError),
    #[error("marking {} as listening", path.display())]
    Mark {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid plugin approval question id")]
    InvalidQuestionId,
    #[error("the plugin approval question has expired")]
    Expired,
    #[error("approval wait is too long")]
    WaitTooLong,
    #[error("the plugin approval question id is already in use")]
    IdInUse,
    #[error("the operator did not answer in time")]
    TimedOut,
    #[error("no matching client on this runtime's home is listening to answer; open access requests in an updated client and retry")]
    NoWorkspaceListener,
    #[error("the unscoped approval has no filesystem target")]
    NoFilesystemTarget,
    #[error("no chat or desktop on this server's home is listening to answer; allow it with `gents plugin dirs add {}`", folder.display())]
    NoListener { folder: PathBuf },
    #[error("the plugin approval question format is unsupported or invalid; use matching runtime and client versions")]
    QuestionFormat(#[source] serde_json::Error),
    #[error(
        "the approval question is in the wrong queue; use matching runtime and client versions"
    )]
    WrongQueue,
    #[error("the approval queue contains duplicate question identities")]
    DuplicateIdentity,
    #[error("listing plugin approval requests")]
    ListQueue(#[source] std::io::Error),
    #[error("reading the approval question")]
    ReadQuestion(#[source] std::io::Error),
    #[error("the question is unreadable")]
    UnreadableQuestion(#[source] serde_json::Error),
    #[error("the approval question is in the wrong queue")]
    QuestionWrongQueue,
    #[error("question {id} is not waiting for an answer")]
    NotWaiting { id: String },
    #[error("reading the operator's answer")]
    ReadAnswer(#[source] std::io::Error),
    #[error("the operator's answer is unreadable")]
    UnreadableAnswer(#[source] serde_json::Error),
    #[error("the operator's answer does not match the approval scope")]
    AnswerScopeMismatch,
    #[error("{id:?} is not a question id")]
    NotQuestionId { id: String },
    #[error("the plugin approval question identity changed")]
    IdentityChanged,
    #[error("the plugin approval question was already answered")]
    AlreadyAnswered,
    #[error("this action only supports allow once or deny")]
    AnswerUnsupported,
    #[error("this approval cannot remember filesystem access")]
    CannotRemember,
    #[error("Workspace permissions changed. Retry the action with the current permissions.")]
    SelectionChanged,
    #[error("Choose a runtime and agent before changing permissions.")]
    ChooseRuntime,
    #[error("Choose an absolute workspace folder.")]
    AbsoluteWorkspace,
    #[error("resolving the workspace folder")]
    ResolveWorkspace(#[source] std::io::Error),
    #[error("Choose a workspace folder.")]
    NotWorkspaceFolder,
    #[error("the command approval has no executable")]
    NoExecutable,
    #[error("the writable root exceeds the tool root")]
    WritableRootOutside,
    #[error("the command working directory is outside its tool root")]
    CwdOutsideRoot,
    #[error("the file approval path is not valid UTF-8")]
    PathNotUtf8,
    #[error("The file approval target changed. Retry the file change.")]
    FileTargetChanged,
    #[error("The file approval target is not a regular file.")]
    FileTargetNotRegular,
    #[error("The plugin invocation changed after approval. Retry the action.")]
    PluginChanged,
    #[error("action cancelled while awaiting approval")]
    Cancelled,
    #[error("action approval exceeded the request deadline")]
    RequestDeadline,
    #[error("action approval exceeded the request deadline")]
    ApprovalTimeout(#[source] tokio::time::error::Elapsed),
    #[error("approval deadline is missing")]
    DeadlineMissing,
    #[error("{message}")]
    NotApproved { message: String },
    #[error("workspace permissions are unavailable")]
    WorkspaceUnavailable,
    #[error("the workspace permissions file is unreadable")]
    UnreadableWorkspaceFile(#[source] serde_json::Error),
    #[error("the workspace permissions file contains duplicate scopes")]
    DuplicateScopes,
    #[error("reading workspace permissions")]
    ReadWorkspace(#[source] std::io::Error),
    #[error("Choose a valid access reviewer task.")]
    InvalidReviewTask,
    #[error(transparent)]
    Selection(#[from] gents_loop::tool_policy::workspace_permissions::SelectionError),
    #[error("unchanged workspace permissions are missing")]
    UnchangedMissing,
    #[error("preparing workspace permissions")]
    PrepareWorkspace(#[source] std::io::Error),
    #[error("opening the workspace permissions lock")]
    WorkspaceLockOpen(#[source] std::io::Error),
    #[error("locking workspace permissions")]
    WorkspaceLock(#[source] std::io::Error),
    #[error("encoding workspace permissions")]
    EncodeWorkspace(#[source] serde_json::Error),
    #[error("access review requires an enabled policy Task for this agent without execution side effects")]
    ReviewTaskIneligible,
    #[error("{message}")]
    TaskRender { message: String },
    #[error("access review Task did not render a policy")]
    NoPolicyRendered,
    #[error("access review input is missing")]
    ReviewInputMissing,
    #[error("resolving the approval home")]
    ResolveApprovalHome(#[source] std::io::Error),
    #[error("the plugin and request have different approval homes")]
    ApprovalHomeMismatch,
    #[error("a workspace approval cannot contain a second filesystem target")]
    SecondFilesystemTarget,
    #[error("the approval question has no plugin")]
    NoPlugin,
    #[error("the approval question has no path")]
    NoPath,
    #[error("the approval question has no access level")]
    NoAccess,
    #[error("the approval question's folder does not match its target")]
    FolderMismatch,
    #[error(transparent)]
    Context(#[from] crate::toolset::ContextError),
    #[error(transparent)]
    Allowed(#[from] PluginAllowedError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Encode(#[from] serde_json::Error),
}

// Consent gates plugin calls inside the existing OS-user trust boundary; it does not isolate same-user shell commands.
pub const WAIT: Duration = Duration::from_secs(10 * 60);

const LISTENER_FRESH: Duration = Duration::from_secs(15);

const LISTENER_GRACE: Duration = if cfg!(test) {
    Duration::from_millis(150)
} else {
    Duration::from_secs(5)
};

tokio::task_local! {
    static INTERACTIVE: ();
}

pub async fn scope_interactive<F>(interactive: bool, future: F) -> F::Output
where
    F: std::future::Future,
{
    if interactive {
        INTERACTIVE.scope((), future).await
    } else {
        future.await
    }
}

pub fn carry_interactive<F>(future: F) -> impl std::future::Future<Output = F::Output>
where
    F: std::future::Future,
{
    workspace::carry(scope_interactive(interactive(), future))
}

pub fn interactive() -> bool {
    INTERACTIVE.try_with(|_| ()).is_ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Consent {
    Ask,
    Deny,
}

pub(crate) fn consent(interactive: bool) -> Consent {
    if interactive {
        Consent::Ask
    } else {
        Consent::Deny
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum Answer {
    #[serde(rename = "once")]
    Once,
    #[serde(rename = "file")]
    AlwaysPath,
    #[serde(rename = "always")]
    AlwaysFolder,
    #[serde(rename = "deny")]
    Deny,
}

impl Answer {
    fn remembered(self) -> bool {
        matches!(self, Self::AlwaysPath | Self::AlwaysFolder)
    }
}

#[derive(Serialize, Deserialize)]
struct Decision {
    allow: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    binding: Option<workspace::Binding>,
}

pub fn prepare_listener(home: &Path) -> Result<PathBuf> {
    for queue in Queue::ALL {
        prepare_queue_listener(home, queue)?;
    }
    Ok(queue_dir(home))
}

fn prepare_queue_listener(home: &Path, format: Queue) -> Result<()> {
    let queue = format.directory(home);
    std::fs::create_dir_all(&queue).map_err(PluginApprovalError::CreateQueue)?;
    let mark = format.listener_file(home);
    if !mark.try_exists()? {
        let file = tempfile::NamedTempFile::new_in(&queue)?;
        file.as_file().set_modified(std::time::UNIX_EPOCH)?;
        match file.persist_noclobber(&mark) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(PluginApprovalError::ListenerMark(error)),
        }
    }
    Ok(())
}

pub fn mark_listening(home: &Path) -> Result<()> {
    prepare_listener(home)?;
    for queue in Queue::ALL {
        let path = queue.listener_file(home);
        std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .and_then(|file| file.set_modified(std::time::SystemTime::now()))
            .map_err(|source| PluginApprovalError::Mark {
                path: path.clone(),
                source,
            })?;
    }
    Ok(())
}

fn listener_fresh(home: &Path, queue: Queue) -> bool {
    std::fs::metadata(queue.listener_file(home))
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| {
            std::time::SystemTime::now()
                .duration_since(modified)
                .map_or(true, |age| age < LISTENER_FRESH)
        })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Each queue format has a separate directory so an incompatible responder
/// cannot answer current questions or advertise a listener for their waiters.
pub fn queue_dir(home: &Path) -> PathBuf {
    Queue::Unscoped.directory(home)
}

pub fn queue_dirs(home: &Path) -> [PathBuf; 2] {
    Queue::ALL.map(|queue| queue.directory(home))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|c| c.is_ascii_alphanumeric())
}

fn lock(home: &Path) -> Result<crate::file_lock::FileLock> {
    std::fs::create_dir_all(home).map_err(PluginApprovalError::CreateHome)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_file(home))
        .map_err(PluginApprovalError::OpenLock)?;
    crate::file_lock::FileLock::exclusive(file).map_err(PluginApprovalError::Lock)
}

pub(super) fn lock_file(home: &Path) -> PathBuf {
    home.join(crate::home::PLUGIN_APPROVALS_LOCK_FILE_NAME)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or(PluginApprovalError::FileWithoutFolder)?;
    std::fs::create_dir_all(parent).map_err(|source| PluginApprovalError::CreateDir {
        path: parent.to_path_buf(),
        source,
    })?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut temp, bytes)?;
    temp.persist(path)
        .map_err(|source| PluginApprovalError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

struct Cleanup<'a> {
    home: &'a Path,
    id: &'a str,
    queue: Queue,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let Ok(_lock) = lock(self.home) else {
            tracing::warn!("could not lock plugin approvals for cleanup");
            return;
        };
        let _ = std::fs::remove_file(self.queue.request_file(self.home, self.id));
        let _ = std::fs::remove_file(self.queue.decision_file(self.home, self.id));
    }
}

pub async fn ask(home: &Path, request: &Request, wait: Duration) -> Result<bool> {
    if !valid_id(&request.id) {
        return Err(PluginApprovalError::InvalidQuestionId);
    }
    let queue = Queue::for_request(request);
    let started = tokio::time::Instant::now();
    let now = now_ms();
    let mut filed = request.clone();
    filed.expires_at_ms = filed
        .expires_at_ms
        .min(now.saturating_add(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)));
    if !(filed.created_at_ms <= now && now < filed.expires_at_ms) {
        return Err(PluginApprovalError::Expired);
    }
    let remaining = Duration::from_millis(filed.expires_at_ms - now).min(wait);
    let deadline = started
        .checked_add(remaining)
        .ok_or(PluginApprovalError::WaitTooLong)?;
    {
        let _lock = lock(home)?;
        for format in Queue::ALL {
            if format.request_file(home, &request.id).try_exists()?
                || format.decision_file(home, &request.id).try_exists()?
            {
                return Err(PluginApprovalError::IdInUse);
            }
        }
        if let Some(binding) = request.binding() {
            workspace::validate_binding(home, binding)?;
        }
        write_atomic(
            &queue.request_file(home, &request.id),
            &serde_json::to_vec(&filed)?,
        )?;
    }
    let _cleanup = Cleanup {
        home,
        id: &request.id,
        queue,
    };
    let mut pause = Duration::from_millis(20);
    loop {
        if let Some(binding) = request.binding() {
            workspace::validate_binding(home, binding)?;
        }
        if let Some(allowed) = read_decision(home, request)? {
            return Ok(allowed);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(PluginApprovalError::TimedOut);
        }
        if started.elapsed() >= LISTENER_GRACE && !listener_fresh(home, queue) {
            if queue == Queue::Workspace {
                return Err(PluginApprovalError::NoWorkspaceListener);
            }
            let file = request
                .filesystem()
                .ok_or(PluginApprovalError::NoFilesystemTarget)?;
            return Err(PluginApprovalError::NoListener {
                folder: file.folder.to_path_buf(),
            });
        }
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_millis(250));
    }
}

pub fn pending(home: &Path) -> Result<Vec<Request>> {
    mark_listening(home)?;
    pending_requests(home)
}

pub fn pending_requests(home: &Path) -> Result<Vec<Request>> {
    let now = now_ms();
    let mut requests = Vec::new();
    let mut ids = rapidhash::RapidHashSet::default();
    for queue in Queue::ALL {
        let entries = match std::fs::read_dir(queue.directory(home)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(PluginApprovalError::ListQueue(error)),
        };
        for entry in entries {
            let path = entry.map_err(PluginApprovalError::ListQueue)?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            if value.get("id").and_then(serde_json::Value::as_str)
                != path.file_stem().and_then(|stem| stem.to_str())
            {
                continue;
            }
            let request: Request =
                serde_json::from_value(value).map_err(PluginApprovalError::QuestionFormat)?;
            if !queue.matches(&request) {
                return Err(PluginApprovalError::WrongQueue);
            }
            if !ids.insert(request.id.clone()) {
                return Err(PluginApprovalError::DuplicateIdentity);
            }
            if let Some(binding) = request.binding() {
                if let Err(error) = workspace::validate_binding(home, binding) {
                    if matches!(
                        error,
                        PluginApprovalError::SelectionChanged
                            | PluginApprovalError::Allowed(PluginAllowedError::PathChanged { .. })
                    ) {
                        continue;
                    }
                    return Err(error);
                }
            }
            if valid_id(&request.id)
                && request.created_at_ms <= now
                && now < request.expires_at_ms
                && !queue.decision_file(home, &request.id).try_exists()?
            {
                requests.push(request);
            }
        }
    }
    requests.sort_by_key(|request| request.created_at_ms);
    Ok(requests)
}

fn read_question(home: &Path, id: &str) -> Result<(Queue, Request)> {
    let mut found = None;
    for queue in Queue::ALL {
        let bytes = match std::fs::read(queue.request_file(home, id)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(PluginApprovalError::ReadQuestion(error)),
        };
        let request: Request =
            serde_json::from_slice(&bytes).map_err(PluginApprovalError::UnreadableQuestion)?;
        if !queue.matches(&request) {
            return Err(PluginApprovalError::QuestionWrongQueue);
        }
        if found.is_some() {
            return Err(PluginApprovalError::DuplicateIdentity);
        }
        found = Some((queue, request));
    }
    found.ok_or_else(|| PluginApprovalError::NotWaiting { id: id.to_owned() })
}

fn read_decision(home: &Path, request: &Request) -> Result<Option<bool>> {
    let queue = Queue::for_request(request);
    let bytes = match std::fs::read(queue.decision_file(home, &request.id)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(PluginApprovalError::ReadAnswer(error)),
    };
    let decision: Decision =
        serde_json::from_slice(&bytes).map_err(PluginApprovalError::UnreadableAnswer)?;
    let allowed = match (request.binding(), decision.binding.as_ref()) {
        (Some(expected), Some(received)) => {
            workspace::consume_decision(home, expected, received, decision.allow)?
        }
        (None, None) => decision.allow,
        _ => return Err(PluginApprovalError::AnswerScopeMismatch),
    };
    Ok(Some(allowed))
}

pub fn decide(home: &Path, id: &str, answer: Answer) -> Result<()> {
    decide_with_clock(home, id, answer, now_ms)
}

fn decide_with_clock(
    home: &Path,
    id: &str,
    answer: Answer,
    clock: impl FnOnce() -> u64,
) -> Result<()> {
    if !valid_id(id) {
        return Err(PluginApprovalError::NotQuestionId { id: id.to_owned() });
    }
    let _lock = lock(home)?;
    let (queue, request) = read_question(home, id)?;
    let now = clock();
    if request.id != id {
        return Err(PluginApprovalError::IdentityChanged);
    }
    if !(request.created_at_ms <= now && now < request.expires_at_ms) {
        return Err(PluginApprovalError::Expired);
    }
    if queue.decision_file(home, id).try_exists()? {
        return Err(PluginApprovalError::AlreadyAnswered);
    }
    if let Some(binding) = request.binding() {
        workspace::validate_binding(home, binding)?;
    }
    if !request.allows_answer(answer) {
        return Err(PluginApprovalError::AnswerUnsupported);
    }
    if answer.remembered() {
        let file = request
            .filesystem()
            .ok_or(PluginApprovalError::CannotRemember)?;
        let path = if answer == Answer::AlwaysPath && !file.is_dir {
            file.path
        } else {
            file.folder
        };
        if let Some(binding) = request.binding() {
            workspace::grant_at_least(home, binding, path, file.access)?;
        } else {
            allowed::grant_at_least(home, path, file.access)?;
        }
    }
    write_atomic(
        &queue.decision_file(home, id),
        &serde_json::to_vec(&Decision {
            allow: answer != Answer::Deny,
            binding: request.binding().cloned(),
        })?,
    )
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "approval_contract_tests.rs"]
mod contract_tests;
