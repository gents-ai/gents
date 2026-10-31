mod question;
mod queue;
#[cfg(test)]
pub(crate) mod testing;
pub mod workspace;
pub use question::{CommandApproval, FilesystemApproval, Request};
use queue::Queue;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::allowed;
#[cfg(test)]
use crate::pack::BindAccess;

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
    #[serde(rename = "invocation")]
    AlwaysInvocation,
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
    std::fs::create_dir_all(&queue).context("creating the approval queue")?;
    let mark = format.listener_file(home);
    if !mark.try_exists()? {
        let file = tempfile::NamedTempFile::new_in(&queue)?;
        file.as_file().set_modified(std::time::UNIX_EPOCH)?;
        match file.persist_noclobber(&mark) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("preparing the approval listener mark"),
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
            .with_context(|| format!("marking {} as listening", path.display()))?;
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

fn lock(home: &Path) -> Result<crate::store::local::file_lock::FileLock> {
    std::fs::create_dir_all(home).context("creating the approval home")?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_file(home))
        .context("opening the approval lock")?;
    crate::store::local::file_lock::FileLock::exclusive(file).context("locking plugin approvals")
}

pub(super) fn lock_file(home: &Path) -> PathBuf {
    home.join(crate::store::local::home::PLUGIN_APPROVALS_LOCK_FILE_NAME)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("approval file has no folder")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    std::io::Write::write_all(&mut temp, bytes)?;
    temp.persist(path)
        .with_context(|| format!("writing {}", path.display()))?;
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
    anyhow::ensure!(valid_id(&request.id), "invalid plugin approval question id");
    let queue = Queue::for_request(request);
    let started = tokio::time::Instant::now();
    let now = now_ms();
    let mut filed = request.clone();
    filed.expires_at_ms = filed
        .expires_at_ms
        .min(now.saturating_add(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)));
    anyhow::ensure!(
        filed.created_at_ms <= now && now < filed.expires_at_ms,
        "the plugin approval question has expired"
    );
    let remaining = Duration::from_millis(filed.expires_at_ms - now).min(wait);
    let deadline = started
        .checked_add(remaining)
        .context("approval wait is too long")?;
    {
        let _lock = lock(home)?;
        for format in Queue::ALL {
            anyhow::ensure!(
                !format.request_file(home, &request.id).try_exists()?
                    && !format.decision_file(home, &request.id).try_exists()?,
                "the plugin approval question id is already in use"
            );
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
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "the operator did not answer in time"
        );
        if started.elapsed() >= LISTENER_GRACE && !listener_fresh(home, queue) {
            if queue == Queue::Workspace {
                anyhow::bail!("no matching client on this runtime's home is listening to answer; open access requests in an updated client and retry");
            }
            let file = request
                .filesystem()
                .context("the unscoped approval has no filesystem target")?;
            anyhow::bail!("no chat or desktop on this server's home is listening to answer; allow it with `gents plugin dirs add {}`", file.folder.display());
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
            Err(error) => return Err(error).context("listing plugin approval requests"),
        };
        for entry in entries {
            let path = entry.context("listing plugin approval requests")?.path();
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
            let request: Request = serde_json::from_value(value).context("the plugin approval question format is unsupported or invalid; use matching runtime and client versions")?;
            anyhow::ensure!(queue.matches(&request), "the approval question is in the wrong queue; use matching runtime and client versions");
            anyhow::ensure!(
                ids.insert(request.id.clone()),
                "the approval queue contains duplicate question identities"
            );
            if let Some(binding) = request.binding() {
                if let Err(error) = workspace::validate_binding(home, binding) {
                    if error.is::<workspace::SelectionChanged>()
                        || error.is::<allowed::PathChanged>()
                    {
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
            Err(error) => return Err(error).context("reading the approval question"),
        };
        let request: Request =
            serde_json::from_slice(&bytes).context("the question is unreadable")?;
        anyhow::ensure!(
            queue.matches(&request),
            "the approval question is in the wrong queue"
        );
        anyhow::ensure!(
            found.is_none(),
            "the approval queue contains duplicate question identities"
        );
        found = Some((queue, request));
    }
    found.with_context(|| format!("question {id} is not waiting for an answer"))
}

fn read_decision(home: &Path, request: &Request) -> Result<Option<bool>> {
    let queue = Queue::for_request(request);
    let bytes = match std::fs::read(queue.decision_file(home, &request.id)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading the operator's answer"),
    };
    let decision: Decision =
        serde_json::from_slice(&bytes).context("the operator's answer is unreadable")?;
    let allowed = match (request.binding(), decision.binding.as_ref()) {
        (Some(expected), Some(received)) => {
            workspace::consume_decision(home, expected, received, decision.allow)?
        }
        (None, None) => decision.allow,
        _ => anyhow::bail!("the operator's answer does not match the approval scope"),
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
    anyhow::ensure!(valid_id(id), "{id:?} is not a question id");
    let _lock = lock(home)?;
    let (queue, request) = read_question(home, id)?;
    let now = clock();
    anyhow::ensure!(
        request.id == id,
        "the plugin approval question identity changed"
    );
    anyhow::ensure!(
        request.created_at_ms <= now && now < request.expires_at_ms,
        "the plugin approval question has expired"
    );
    anyhow::ensure!(
        !queue.decision_file(home, id).try_exists()?,
        "the plugin approval question was already answered"
    );
    if let Some(binding) = request.binding() {
        workspace::validate_binding(home, binding)?;
    }
    anyhow::ensure!(
        request.allows_answer(answer),
        "This answer is not available for this action. Reopen the access request."
    );
    if answer == Answer::AlwaysInvocation {
        workspace::remember_command(
            home,
            request
                .binding()
                .context("This approval has no bound command.")?,
        )?;
    } else if answer.remembered() {
        let file = request
            .filesystem()
            .context("this approval cannot remember filesystem access")?;
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

#[cfg(test)]
mod command_grant_contract_tests;

#[cfg(all(test, target_os = "macos"))]
mod command_browser_approval_tests;
