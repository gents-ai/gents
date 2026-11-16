use gents_protocol::operations::install_repair::Blocker;
use std::io::SeekFrom;
use std::path::Path;

use gents_protocol::operations::install_repair::IncompatibleStoreKind;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

use crate::native_service::{runtime_error_log, ServiceExit, ServiceRefusal};

const LOG_TAIL_BYTES: u64 = 64 * 1024;
pub fn last_error_line(log: &str) -> Option<&str> {
    log.lines()
        .rev()
        .find_map(|line| line.strip_prefix("Error: "))
}
pub async fn read_last_error(user_home: &Path, legacy: Option<&Path>) -> Option<String> {
    for log in std::iter::once(runtime_error_log(user_home)).chain(legacy.map(Path::to_path_buf)) {
        if let Some(error) = read_tail(&log).await.as_deref().and_then(last_error_line) {
            return Some(error.to_string());
        }
    }
    None
}

async fn read_tail(path: &Path) -> Option<String> {
    let mut file = tokio::fs::File::open(path).await.ok()?;
    let length = file.metadata().await.ok()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(LOG_TAIL_BYTES)))
        .await
        .ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).await.ok()?;
    Some(String::from_utf8_lossy(&tail).into_owned())
}

pub fn blocker_for_exit(exit: &ServiceExit, last_error: Option<String>) -> Blocker {
    match exit.refusal() {
        Some(ServiceRefusal::StoreKeyUnavailable) => Blocker::KeychainUnavailable,
        Some(ServiceRefusal::Store(kind)) if kind != IncompatibleStoreKind::MissingStoreKey => {
            Blocker::StoreRefused {
                kind,
                message: last_error.unwrap_or_else(|| exit.reason.clone()),
            }
        }
        _ => Blocker::KeepsExiting {
            code: exit.code,
            last_error,
        },
    }
}
