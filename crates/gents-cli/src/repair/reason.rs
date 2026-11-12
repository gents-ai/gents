use std::io::SeekFrom;
use std::path::Path;

use gents::store::local::storage_backend::IncompatibleStoreKind;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

use crate::native_service::{runtime_error_log, ServiceExit, ServiceRefusal};
use crate::repair::Blocker;

const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// The message of the last `Error: ` line, for display only: `main.rs` prints
/// `Error: {error:#}` to stderr, which the service manager appends to its log.
pub fn last_error_line(log: &str) -> Option<&str> {
    log.lines()
        .rev()
        .find_map(|line| line.strip_prefix("Error: "))
}

/// The last error the background node logged, from the shared log first and
/// then from `legacy`, the log an older desktop install wrote to.
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

pub fn no_service_manager_message(home: &Path) -> String {
    format!(
        "run `gents runtime server --home {}` in another terminal, then `gents`",
        crate::shell_word(home)
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair::port::PortHolder;

    fn exit(code: i32) -> ServiceExit {
        ServiceExit {
            reason: format!("exited with code {code}"),
            restarts: 3,
            clean: false,
            code: Some(code),
        }
    }

    #[test]
    fn the_last_error_line_wins() {
        assert_eq!(
            last_error_line("Error: a\n...\nError: b\nCaused by:\n x"),
            Some("b")
        );
    }

    #[test]
    fn no_error_line_is_none() {
        assert_eq!(last_error_line("starting\nlistening on 21919\n"), None);
    }

    #[test]
    fn a_locked_keychain_blocks_with_its_reason() {
        let blocker = blocker_for_exit(&exit(69), Some("keychain locked".into()));
        assert_eq!(blocker, Blocker::KeychainUnavailable);
        assert_eq!(
            blocker.to_string(),
            "the macOS Keychain is locked or Gents isn't allowed to read it. Unlock it, then try again."
        );
    }

    #[test]
    fn a_crash_names_its_last_error() {
        let blocker = blocker_for_exit(&exit(1), Some("the store is corrupt".into()));
        assert_eq!(
            blocker.to_string(),
            "the background node keeps exiting: the store is corrupt"
        );
        assert_eq!(
            blocker_for_exit(&exit(68), None).to_string(),
            "the background node keeps exiting: exit code 68"
        );
        assert_eq!(
            blocker_for_exit(&exit(65), None),
            Blocker::StoreRefused {
                kind: IncompatibleStoreKind::UnknownLineage,
                message: "exited with code 65".into()
            }
        );
    }

    #[test]
    fn a_port_holder_is_named() {
        let blocker = Blocker::PortInUse {
            port: 21919,
            holder: Some(PortHolder {
                pid: 4312,
                program: "Docker".into(),
            }),
        };
        assert_eq!(
            blocker.to_string(),
            "port 21919 is in use by Docker (pid 4312). Quit it, then try again."
        );
    }

    #[test]
    fn an_unknown_holder_is_another_app() {
        let blocker = Blocker::PortInUse {
            port: 21919,
            holder: None,
        };
        assert_eq!(
            blocker.to_string(),
            "port 21919 is in use by another app. Quit it, then try again."
        );
    }

    #[tokio::test]
    async fn the_shared_log_is_read_before_the_legacy_one() {
        let temp = tempfile::tempdir().unwrap();
        let shared = runtime_error_log(temp.path());
        std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
        let padding = "x".repeat(70 * 1024);
        std::fs::write(&shared, format!("Error: old\n{padding}\n")).unwrap();
        assert_eq!(read_last_error(temp.path(), None).await, None);
        let legacy = temp.path().join("legacy.log");
        std::fs::write(&legacy, "Error: from the desktop log\n").unwrap();
        assert_eq!(
            read_last_error(temp.path(), Some(&legacy)).await.as_deref(),
            Some("from the desktop log")
        );
        std::fs::write(&shared, format!("Error: old\n{padding}\nError: new\n")).unwrap();
        assert_eq!(
            read_last_error(temp.path(), Some(&legacy)).await.as_deref(),
            Some("new")
        );
    }
}
