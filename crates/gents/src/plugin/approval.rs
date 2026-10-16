
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::allowed;
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
    static COMPOSED: ();
}

pub async fn scope_interactive<F: std::future::Future>(interactive: bool, future: F) -> F::Output {
    if interactive {
        INTERACTIVE.scope((), future).await
    } else {
        future.await
    }
}

pub fn carry_interactive<F: std::future::Future>(
    future: F,
) -> impl std::future::Future<Output = F::Output> {
    let composed = composed();
    let future = scope_interactive(interactive(), future);
    async move {
        if composed {
            COMPOSED.scope((), future).await
        } else {
            future.await
        }
    }
}

pub fn interactive() -> bool {
    INTERACTIVE.try_with(|_| ()).is_ok()
}

pub(crate) async fn scope_composed<F: std::future::Future>(future: F) -> F::Output {
    COMPOSED.scope((), future).await
}

fn composed() -> bool {
    COMPOSED.try_with(|_| ()).is_ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Consent {
    Ask,
    Deny,
}

pub(crate) fn consent_route(interactive: bool, _composed: bool) -> Consent {
    if interactive {
        Consent::Ask
    } else {
        Consent::Deny
    }
}

pub(crate) fn consent(interactive: bool) -> Consent {
    consent_route(interactive, composed())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub plugin: String,
    pub path: String,
    pub folder: PathBuf,
    #[serde(default)]
    pub is_dir: bool,
    pub access: BindAccess,
    #[serde(default)]
    pub session_id: Option<String>,
    pub created_at_ms: u64,
}

impl Request {
    pub fn new(
        plugin: &str,
        resolved: &allowed::Resolved,
        access: BindAccess,
        session_id: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            plugin: plugin.to_owned(),
            path: resolved.target.display().to_string(),
            folder: resolved.folder().to_path_buf(),
            is_dir: resolved.is_dir,
            access,
            session_id,
            created_at_ms: now_ms(),
        }
    }

    pub fn prompt(&self) -> String {
        let verb = match self.access {
            BindAccess::Read => "read",
            BindAccess::ReadWrite => "read and write",
        };
        format!("Allow {} to {verb} {}?", self.plugin, self.path)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Once,
    AlwaysPath,
    AlwaysFolder,
    Deny,
}

#[derive(Serialize, Deserialize)]
struct Decision {
    allow: bool,
}

fn listener_file(home: &Path) -> PathBuf {
    dir(home).join(".listening")
}

pub fn prepare_listener(home: &Path) -> Result<PathBuf> {
    let queue = dir(home);
    std::fs::create_dir_all(&queue).context("creating the approval queue")?;
    let mark = listener_file(home);
    if !mark.try_exists()? {
        let file = tempfile::NamedTempFile::new_in(&queue)?;
        file.as_file().set_modified(std::time::UNIX_EPOCH)?;
        match file.persist_noclobber(&mark) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error).context("preparing the approval listener mark"),
        }
    }
    Ok(queue)
}

pub fn mark_listening(home: &Path) -> Result<()> {
    let path = listener_file(home);
    prepare_listener(home)?;
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .and_then(|file| file.set_modified(std::time::SystemTime::now()))
        .with_context(|| format!("marking {} as listening", path.display()))
}

fn listener_fresh(home: &Path) -> bool {
    std::fs::metadata(listener_file(home))
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

fn dir(home: &Path) -> PathBuf {
    home.join(crate::home::PLUGIN_APPROVALS_DIR_NAME)
}

fn request_file(home: &Path, id: &str) -> PathBuf {
    dir(home).join(format!("{id}.json"))
}

fn decision_file(home: &Path, id: &str) -> PathBuf {
    dir(home).join(format!("{id}.decision"))
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
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(request_file(self.home, self.id));
        let _ = std::fs::remove_file(decision_file(self.home, self.id));
    }
}

pub async fn ask(home: &Path, request: &Request, wait: Duration) -> Result<bool> {
    write_atomic(
        &request_file(home, &request.id),
        &serde_json::to_vec(request)?,
    )?;
    let _cleanup = Cleanup {
        home,
        id: &request.id,
    };
    let started = tokio::time::Instant::now();
    let deadline = started + wait;
    let mut pause = Duration::from_millis(20);
    loop {
        match std::fs::read(decision_file(home, &request.id)) {
            Ok(bytes) => {
                let decision: Decision = serde_json::from_slice(&bytes)
                    .context("the operator's answer is unreadable")?;
                return Ok(decision.allow);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("reading the operator's answer"),
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "the operator did not answer in time"
        );
        anyhow::ensure!(
            started.elapsed() < LISTENER_GRACE || listener_fresh(home),
            "no chat or desktop on this server's home is listening to answer; allow it with `gents plugin dirs add {}`",
            request.folder.display()
        );
        tokio::time::sleep(pause).await;
        pause = (pause * 2).min(Duration::from_millis(250));
    }
}

pub fn pending(home: &Path) -> Result<Vec<Request>> {
    mark_listening(home)?;
    pending_requests(home)
}

pub fn pending_requests(home: &Path) -> Result<Vec<Request>> {
    let entries = match std::fs::read_dir(dir(home)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("listing plugin approval requests"),
    };
    let oldest = now_ms().saturating_sub(WAIT.as_millis() as u64);
    let mut requests = Vec::new();
    for entry in entries {
        let path = entry.context("listing plugin approval requests")?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
            continue;
        };
        if request.created_at_ms >= oldest && !decision_file(home, &request.id).exists() {
            requests.push(request);
        }
    }
    requests.sort_by_key(|request| request.created_at_ms);
    Ok(requests)
}

pub fn decide(home: &Path, id: &str, answer: Answer) -> Result<()> {
    anyhow::ensure!(
        id.chars().all(|c| c.is_ascii_alphanumeric()),
        "{id:?} is not a question id"
    );
    let bytes = std::fs::read(request_file(home, id))
        .with_context(|| format!("question {id} is not waiting for an answer"))?;
    let request: Request = serde_json::from_slice(&bytes).context("the question is unreadable")?;
    let remembered = match answer {
        Answer::AlwaysPath if !request.is_dir => Some(PathBuf::from(&request.path)),
        Answer::AlwaysPath | Answer::AlwaysFolder => Some(request.folder.clone()),
        Answer::Once | Answer::Deny => None,
    };
    if let Some(path) = remembered {
        let held = allowed::list(home)?
            .into_iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.access);
        allowed::add(home, &path, request.access.max(held.unwrap_or_default()))?;
    }
    write_atomic(
        &decision_file(home, id),
        &serde_json::to_vec(&Decision {
            allow: answer != Answer::Deny,
        })?,
    )
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
