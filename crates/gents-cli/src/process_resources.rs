#[derive(Debug, thiserror::Error)]
pub(crate) enum ProcessResourcesError {
    #[cfg(target_os = "macos")]
    #[error("read process Keychain UI policy")]
    KeychainUiPolicy(#[source] security_framework::base::Error),
    #[cfg(target_os = "macos")]
    #[error("disable process Keychain access dialogs")]
    DisableKeychainDialogs(#[source] security_framework::base::Error),
    #[cfg(unix)]
    #[error("read process open-file limit")]
    ReadOpenFileLimit(#[source] std::io::Error),
    #[cfg(unix)]
    #[error("raise process open-file limit; set ulimit -Sn in this terminal before retrying")]
    RaiseOpenFileLimit(#[source] std::io::Error),
}

type Result<T, E = ProcessResourcesError> = std::result::Result<T, E>;

/// macOS applies this setting to the process, not a Keychain item or its ACL.
/// The CLI owns this guard from before credential loading until server exit;
/// an inherited prohibition must remain in force when the guard is dropped.
#[cfg(target_os = "macos")]
pub(crate) fn prevent_keychain_prompts(
    prevent: bool,
) -> Result<Option<security_framework::os::macos::keychain::KeychainUserInteractionLock>> {
    use security_framework::os::macos::keychain::SecKeychain;

    if !prevent
        || !SecKeychain::user_interaction_allowed()
            .map_err(ProcessResourcesError::KeychainUiPolicy)?
    {
        return Ok(None);
    }
    SecKeychain::disable_user_interaction()
        .map(Some)
        .map_err(ProcessResourcesError::DisableKeychainDialogs)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn prevent_keychain_prompts(_prevent: bool) -> Result<()> {
    Ok(())
}

/// Embedded DefraDB runtimes need a usable descriptor budget when launched
/// from GUI terminals with a low inherited soft limit. Raise only this process
/// limit, retaining the hard limit and system policy.
#[cfg(unix)]
pub(crate) fn prepare() -> Result<()> {
    let mut limits = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limits points to initialized storage for one rlimit value.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } != 0 {
        return Err(ProcessResourcesError::ReadOpenFileLimit(
            std::io::Error::last_os_error(),
        ));
    }
    let target = 65_536.min(limits.rlim_max);
    let previous = limits.rlim_cur;
    if previous < target {
        limits.rlim_cur = target;
        // SAFETY: limits is a valid rlimit; only the soft limit increases.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) } != 0 {
            return Err(ProcessResourcesError::RaiseOpenFileLimit(
                std::io::Error::last_os_error(),
            ));
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
    fn server_keychain_dialog_prevention_is_opt_in() {
        use crate::cli::{Cli, Command};
        use clap::Parser;

        for (arguments, expected) in [
            (vec!["gents", "server"], false),
            (vec!["gents", "server", "--no-keychain-prompts"], true),
        ] {
            let cli = Cli::try_parse_from(arguments).unwrap();
            let Command::Server(args) = cli.command else {
                panic!("expected server command");
            };
            assert_eq!(args.no_keychain_prompts, expected);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn server_keychain_prompt_guard_restores_only_its_process_policy() {
        const CHILD: &str = "GENTS_KEYCHAIN_UI_POLICY_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            use security_framework::os::macos::keychain::SecKeychain;

            assert!(SecKeychain::user_interaction_allowed().unwrap());
            let unchanged = super::prevent_keychain_prompts(false).unwrap();
            assert!(SecKeychain::user_interaction_allowed().unwrap());
            drop(unchanged);
            let guard = super::prevent_keychain_prompts(true).unwrap();
            assert!(!SecKeychain::user_interaction_allowed().unwrap());
            for prevent in [false, true] {
                let inherited = super::prevent_keychain_prompts(prevent).unwrap();
                assert!(!SecKeychain::user_interaction_allowed().unwrap());
                drop(inherited);
                assert!(!SecKeychain::user_interaction_allowed().unwrap());
            }
            drop(guard);
            assert!(SecKeychain::user_interaction_allowed().unwrap());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_resources::tests::server_keychain_prompt_guard_restores_only_its_process_policy",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

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
