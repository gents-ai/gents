use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::store::local::file_lock::FileLock;

use crate::support::process::ServeProcess;

const RETAINED_MARKER: &str = ".gents-failed-fixture-v1";
const MARKER_CONTENTS: &[u8] = b"completed failed fixture\n";
const DIRECTORY_PREFIX: &str = "failure-";
const MAX_RETAINED: usize = 5;

/// Retained homes contain accepted model inputs and credentials; only
/// completed failures bearing this owner's marker may be pruned. The server is
/// reaped before the marker is published, so concurrent active homes are never
/// retention candidates.
pub struct FailureArtifacts {
    directory: Option<tempfile::TempDir>,
    server: Option<ServeProcess>,
    root: PathBuf,
    retain_failures: bool,
    succeeded: bool,
    limit: usize,
}

impl FailureArtifacts {
    pub fn new(root: &Path, retain_failures: bool) -> Result<Self> {
        let directory = if retain_failures {
            fs::create_dir_all(root)
                .with_context(|| format!("creating failure artifact root {}", root.display()))?;
            tempfile::Builder::new()
                .prefix(DIRECTORY_PREFIX)
                .tempdir_in(root)?
        } else {
            tempfile::tempdir()?
        };
        Ok(Self {
            directory: Some(directory),
            server: None,
            root: root.to_owned(),
            retain_failures,
            succeeded: false,
            limit: MAX_RETAINED,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.as_ref().expect("unfinished fixture").path()
    }

    pub fn attach_server(&mut self, server: ServeProcess) {
        assert!(self.server.is_none(), "fixture already has a server");
        self.server = Some(server);
    }

    pub fn captured_output(&self) -> Result<(String, String)> {
        self.server
            .as_ref()
            .context("fixture server was not started")?
            .captured_output()
    }

    pub fn finish<T>(mut self, result: Result<T>) -> Result<T> {
        self.succeeded = result.is_ok();
        match (result, self.finalize()) {
            (Err(error), Ok(Some(path))) => {
                Err(error.context(format!("failed fixture retained at {}", path.display())))
            }
            (Err(error), Err(retention)) => {
                Err(error.context(format!("finalizing failure artifacts: {retention:#}")))
            }
            (Ok(_), Err(error)) => Err(error),
            (result, _) => result,
        }
    }

    fn finalize(&mut self) -> Result<Option<PathBuf>> {
        drop(self.server.take());
        let Some(directory) = self.directory.take() else {
            return Ok(None);
        };
        if self.succeeded || !self.retain_failures {
            directory.close().context("cleaning fixture artifacts")?;
            return Ok(None);
        }

        let path = directory.keep();
        let retained = (|| {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.root.join(".retention.lock"))?;
            let _lock = FileLock::exclusive(lock)?;
            fs::write(path.join(RETAINED_MARKER), MARKER_CONTENTS)?;
            prune_completed_failures(&self.root, &path, self.limit)
        })();
        retained.with_context(|| format!("retaining failed fixture at {}", path.display()))?;
        tracing::error!(path = %path.display(), "retained failed fixture for investigation");
        Ok(Some(path))
    }
}

impl Drop for FailureArtifacts {
    fn drop(&mut self) {
        if let Err(error) = self.finalize() {
            tracing::error!(%error, "could not finalize fixture artifacts");
        }
    }
}

fn prune_completed_failures(root: &Path, current: &Path, limit: usize) -> Result<()> {
    let mut retained = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || !entry
                .file_name()
                .to_string_lossy()
                .starts_with(DIRECTORY_PREFIX)
        {
            continue;
        }
        let marker = entry.path().join(RETAINED_MARKER);
        let metadata = match marker.symlink_metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if fs::read(&marker)? == MARKER_CONTENTS {
            retained.push((entry.path(), metadata.modified()?));
        }
    }
    retained.sort_by(|(left, left_time), (right, right_time)| {
        (left == current)
            .cmp(&(right == current))
            .then_with(|| left_time.cmp(right_time))
            .then_with(|| left.cmp(right))
    });
    let remove = retained.len().saturating_sub(limit.max(1));
    for (path, _) in retained.into_iter().take(remove) {
        fs::remove_dir_all(&path)
            .with_context(|| format!("pruning completed failure {}", path.display()))?;
    }
    Ok(())
}
