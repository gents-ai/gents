//! `--allow-host-bash` declares this process contained. The claim is held only
//! where the launcher's container left its evidence: Linux, the marker the eval
//! image (scripts/evals/container/Dockerfile) bakes in, on the read-only root
//! the launcher runs it with, and Docker's own `/.dockerenv`. The evidence
//! guards against a switch inherited outside the container; it is not proof
//! against someone who controls the host.

use std::path::Path;

use anyhow::Result;

pub(crate) const CONTAINER_MARKER: &str = "/etc/gents/eval-container";
const DOCKER_ENV: &str = "/.dockerenv";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Evidence {
    pub(crate) linux: bool,
    pub(crate) marker: bool,
    pub(crate) marker_read_only: bool,
    pub(crate) docker_env: bool,
}

impl Evidence {
    pub(crate) fn observe() -> Self {
        let marker = Path::new(CONTAINER_MARKER);
        Self {
            linux: cfg!(target_os = "linux"),
            marker: marker.is_file(),
            marker_read_only: read_only(marker),
            docker_env: Path::new(DOCKER_ENV).is_file(),
        }
    }
}

/// Why `evidence` does not show the eval container, if it does not.
pub(crate) fn refusal(evidence: Evidence) -> Option<String> {
    let missing = if !evidence.linux {
        format!("runs on {}, not in a Linux container", std::env::consts::OS)
    } else if !evidence.marker {
        format!("has no {CONTAINER_MARKER}")
    } else if !evidence.marker_read_only {
        format!("has {CONTAINER_MARKER} on a writable filesystem")
    } else if !evidence.docker_env {
        format!("has no {DOCKER_ENV}")
    } else {
        return None;
    };
    Some(format!(
        "--allow-host-bash (GENTS_EVAL_ALLOW_HOST_BASH) is refused: it is only for a run inside the \
         read-only eval container scripts/evals/run-container.sh starts, and this process {missing}"
    ))
}

pub(crate) fn verify() -> Result<()> {
    match refusal(Evidence::observe()) {
        Some(reason) => Err(anyhow::Error::msg(reason)),
        None => Ok(()),
    }
}

#[cfg(target_os = "linux")]
fn read_only(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: the path is NUL terminated and statvfs initializes stats on success.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: statvfs succeeded and initialized stats.
    let stats = unsafe { stats.assume_init() };
    stats.f_flag & libc::ST_RDONLY != 0
}

#[cfg(not(target_os = "linux"))]
fn read_only(_: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTAINED: Evidence = Evidence {
        linux: true,
        marker: true,
        marker_read_only: true,
        docker_env: true,
    };

    #[test]
    fn the_switch_holds_only_with_every_piece_of_container_evidence() {
        assert_eq!(refusal(CONTAINED), None);
        for (evidence, names) in [
            (
                Evidence {
                    linux: false,
                    ..CONTAINED
                },
                "Linux container",
            ),
            (
                Evidence {
                    marker: false,
                    ..CONTAINED
                },
                CONTAINER_MARKER,
            ),
            (
                Evidence {
                    marker_read_only: false,
                    ..CONTAINED
                },
                "writable filesystem",
            ),
            (
                Evidence {
                    docker_env: false,
                    ..CONTAINED
                },
                DOCKER_ENV,
            ),
        ] {
            let reason = refusal(evidence).expect("refused");
            assert!(
                reason.contains("--allow-host-bash") && reason.contains(names),
                "{reason}"
            );
        }
    }

    #[test]
    fn a_development_host_is_refused() {
        let observed = Evidence::observe();
        assert!(
            !observed.marker_read_only || !observed.docker_env,
            "the test host looks like the eval container: {observed:?}"
        );
        assert!(verify().is_err());
    }
}
