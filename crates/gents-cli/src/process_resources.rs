use anyhow::Result;

/// Embedded DefraDB runtimes need a usable descriptor budget when launched
/// from GUI terminals with a low inherited soft limit. Raise only this process
/// limit, retaining the hard limit and system policy.
#[cfg(unix)]
pub(crate) fn prepare() -> Result<()> {
    use anyhow::Context;
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limits points to initialized storage for one rlimit value.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } != 0 {
        return Err(std::io::Error::last_os_error()).context("read process open-file limit");
    }
    let target = 65_536.min(limits.rlim_max);
    let previous = limits.rlim_cur;
    if previous < target {
        limits.rlim_cur = target;
        // SAFETY: limits is a valid rlimit; only the soft limit increases.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) } != 0 {
            return Err(std::io::Error::last_os_error()).context(
                "raise process open-file limit; set ulimit -Sn in this terminal before retrying",
            );
        }
        tracing::info!(previous, current = target, "raised process open-file limit");
    }
    if limits.rlim_cur < 65_536 {
        tracing::warn!(limit = limits.rlim_cur, "process open-file limit is constrained by the hard limit; reduce runtime concurrency if descriptors are exhausted");
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn prepare() -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn raises_inherited_soft_limit_without_changing_hard_limit() {
        const CHILD: &str = "GENTS_PROCESS_FD_LIMIT_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let mut limits = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: limits is valid writable storage; this subprocess owns
            // its resource limits and has not started a runtime.
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) },
                0
            );
            let hard = limits.rlim_max;
            limits.rlim_cur = 256.min(hard);
            // SAFETY: a valid limit within the unchanged hard limit.
            assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) }, 0);
            super::prepare().unwrap();
            // SAFETY: limits remains valid writable storage.
            assert_eq!(
                unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) },
                0
            );
            assert_eq!(limits.rlim_cur, 65_536.min(hard));
            assert_eq!(limits.rlim_max, hard);
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_resources::tests::raises_inherited_soft_limit_without_changing_hard_limit",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
    }
}
