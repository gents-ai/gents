use anyhow::{bail, Context, Result};
use gents_desktop_core::{
    client::DesktopPaths,
    home_reset::{
        lock_client_state, lock_desktop_process, lock_reset, plan_full_reset, resolve_user_home,
        HomeResetDisposition, HomeResetPlan, ManagedServerResetResult,
    },
};

use crate::{
    cli::DesktopCommand,
    native_service::{NativeServiceConfig, NativeServiceManager, NativeServicePlatform},
};

pub(crate) async fn run(command: DesktopCommand) -> Result<()> {
    if !cfg!(target_os = "macos") {
        bail!("Full desktop reset is supported on macOS.");
    }
    let DesktopCommand::Reset {
        home,
        desktop_home,
        delete,
        confirm,
        wait_for_desktop,
        desktop_pid,
        desktop_executable,
    } = command;
    let mut ready = false;
    let mut retry = "gents desktop reset".to_owned();
    let result = reset(
        home,
        desktop_home,
        delete,
        confirm,
        wait_for_desktop,
        desktop_pid,
        desktop_executable,
        &mut ready,
        &mut retry,
    )
    .await;
    if wait_for_desktop && ready {
        let message = match &result {
            Ok(result) => match &result.backup_path {
                Some(path) => format!(
                    "Reset finished. Your backup is at {path}. Reopen Gents to set up a new node."
                ),
                None => "Reset finished. Reopen Gents to set up a new node.".into(),
            },
            Err(error) => format!(
                "Reset could not finish: {error:#}. Run {retry} to review the remaining state and retry."
            ),
        };
        notify(&message);
        result.map(|_| ())
    } else if wait_for_desktop {
        result.map(|_| ())
    } else {
        crate::print_json(&serde_json::to_value(result?)?)
    }
}

#[allow(clippy::too_many_arguments)]
async fn reset(
    home: Option<std::path::PathBuf>,
    desktop_home: Option<std::path::PathBuf>,
    delete: bool,
    confirm: Option<String>,
    wait_for_desktop: bool,
    desktop_pid: Option<u32>,
    desktop_executable: Option<std::path::PathBuf>,
    ready: &mut bool,
    retry: &mut String,
) -> Result<ManagedServerResetResult> {
    let desktop = select_desktop(
        desktop_home,
        std::env::var_os(gents_desktop_core::client::DESKTOP_HOME_ENV),
        &std::env::current_dir().context("resolving current directory")?,
    )?;
    let explicit_home = home.or_else(|| {
        std::env::var_os("GENTS_HOME")
            .filter(|value| !value.is_empty())
            .map(Into::into)
    });
    let home = match explicit_home {
        Some(home) => home,
        None => gents_desktop_core::home_reset::reviewed_reset_home(&desktop)?
            .unwrap_or_else(|| crate::resolve_home_dir(None)),
    };
    let home = crate::absolutize(home)?;
    *retry = retry_command(&home, desktop.root())?;
    let config = NativeServiceConfig::new(home.clone(), std::env::current_exe()?)?;
    let user_home = resolve_user_home(Some(config.user_home.clone()))?;
    let definition = config.definition_path(NativeServicePlatform::current()?);
    let plan = plan_full_reset(Some(&home), &desktop, &user_home, &definition)?;
    let Some(confirmation) = confirm else {
        return Ok(plan.preview());
    };
    let disposition = if delete {
        HomeResetDisposition::Delete
    } else {
        HomeResetDisposition::Archive
    };
    plan.check_confirmation(&confirmation, disposition)?;
    let _reservation = lock_reset(&desktop)?;
    if wait_for_desktop {
        use std::io::Write;
        std::io::stdout().write_all(b"R")?;
        std::io::stdout().flush()?;
        *ready = true;
    }
    let _process = lock_desktop_process(&desktop, wait_for_desktop)?;
    if let Some(pid) = desktop_pid {
        wait_for_exit(pid).await?;
    }
    let executable = match desktop_executable {
        Some(path) => path,
        None => installed_desktop_executable()?,
    };
    run_browser_helper(&executable, BrowserHelper::Check, BROWSER_CHECK_TIMEOUT)
        .context("Browser storage is in use or cannot be checked; nothing was retired")?;
    let backup = if delete {
        None
    } else {
        let path =
            plan.backup_path(&chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string())?;
        plan.preflight(&path)?;
        Some(path)
    };
    let service = NativeServiceManager::new(config)?;
    let still_running = tokio::task::spawn_blocking(move || -> Result<bool> {
        service.stop(true)?;
        Ok(service.status()?.is_active_or_transitioning())
    })
    .await
    .context("stopping the managed service")??;
    if still_running {
        bail!("The managed service is still running; quit Gents and stop it before resetting.");
    }
    let server = crate::server_host::ServerConfig::standard(home.clone());
    let address = std::net::SocketAddr::new(server.http_addr, server.http_port);
    match std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_millis(150)) {
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        _ => bail!(
            "Cannot confirm the managed runtime endpoint is stopped; stop it before resetting."
        ),
    }
    let _home_lock = if plan.runtime.is_some() {
        Some(gents::home::lock_home_store(&home)?)
    } else {
        None
    };
    let _client_lock = lock_client_state(&desktop)?;
    let settled = plan_full_reset(Some(&home), &desktop, &user_home, &definition)?;
    settled.check_confirmation(&confirmation, disposition)?;
    retire_then_clear(&settled, disposition, backup.as_deref(), retry, || {
        run_browser_helper(&executable, BrowserHelper::Clear, BROWSER_CLEAR_TIMEOUT)
    })
}

fn retire_then_clear(
    settled: &HomeResetPlan,
    disposition: HomeResetDisposition,
    backup: Option<&std::path::Path>,
    retry: &str,
    clear: impl FnOnce() -> Result<()>,
) -> Result<ManagedServerResetResult> {
    let mut result = settled.retire(disposition, backup)?;
    clear().with_context(|| {
        let retired = match &result.backup_path {
            Some(path) => format!("Your local state was archived to {path}"),
            None => "Your local state was permanently deleted".into(),
        };
        format!(
            "{retired}, but browser data was not cleared. Quit every Gents window, then run `{retry}` to review and clear it"
        )
    })?;
    result.cleared_browser_paths = result.browser_paths.clone();
    Ok(result)
}

fn select_desktop(
    flag: Option<std::path::PathBuf>,
    env: Option<std::ffi::OsString>,
    cwd: &std::path::Path,
) -> Result<DesktopPaths> {
    let root = match flag.or_else(|| env.filter(|value| !value.is_empty()).map(Into::into)) {
        Some(root) => root,
        None => DesktopPaths::discover()?.root().to_path_buf(),
    };
    Ok(DesktopPaths::from_root(cwd.join(root)))
}

fn retry_command(home: &std::path::Path, desktop: &std::path::Path) -> Result<String> {
    let quote = |path: &std::path::Path| -> Result<String> {
        let path = path
            .to_str()
            .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
        Ok(shlex::try_quote(path)
            .with_context(|| format!("{path} cannot be quoted for a shell"))?
            .into_owned())
    };
    Ok(format!(
        "gents desktop reset --home {} --desktop-home {}",
        quote(home)?,
        quote(desktop)?
    ))
}

fn notify(message: &str) {
    tracing::info!(message, "desktop reset finished");
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("/usr/bin/osascript")
            .args([
                "-e",
                "on run argv\ndisplay alert \"Gents reset\" message (item 1 of argv)\nend run",
                message,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// The Tauri state can drop before native browser teardown finishes; the
/// helper waits for process death as well as the file-lock handoff.
async fn wait_for_exit(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        let pid = i32::try_from(pid).context("invalid desktop process ID")?;
        if pid <= 0 {
            bail!("invalid desktop process ID");
        }
        loop {
            let result = unsafe { libc::kill(pid, 0) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(());
                }
                return Err(error).context("waiting for desktop process shutdown");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        bail!("desktop reset handoff is unsupported on this platform")
    }
}

/// The installed desktop owns its WebKit store; bound the wait for its helper
/// receipt so a stalled process cannot hold reset indefinitely.
const BROWSER_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
const BROWSER_CLEAR_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

/// WebKit chooses its store by the owning app identity. The CLI therefore asks
/// the installed desktop executable, which owns that identity and the store's
/// lock, to check or clear it, and requires a native completion receipt; it
/// never removes browser directories itself.
#[derive(Debug, Clone, Copy)]
enum BrowserHelper {
    Check,
    Clear,
}

#[cfg(target_os = "macos")]
fn installed_desktop_executable() -> Result<std::path::PathBuf> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSBundle, NSString};
    let url = NSWorkspace::sharedWorkspace()
        .URLForApplicationWithBundleIdentifier(&NSString::from_str("com.source-inc.gents"))
        .context("install the matching Gents desktop app before resetting browser data")?;
    let path = NSBundle::bundleWithURL(&url)
        .and_then(|bundle| bundle.executableURL())
        .and_then(|url| url.path())
        .context("the installed Gents desktop app has no executable")?;
    Ok(std::path::PathBuf::from(path.to_string()))
}

#[cfg(not(target_os = "macos"))]
fn installed_desktop_executable() -> Result<std::path::PathBuf> {
    bail!("desktop browser reset is unsupported on this platform")
}

fn run_browser_helper(
    executable: &std::path::Path,
    helper: BrowserHelper,
    timeout: std::time::Duration,
) -> Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    use std::process::{Command, Stdio};
    use wait_timeout::ChildExt as _;
    let temporary = tempfile::tempdir().context("creating browser clearing receipt directory")?;
    let receipt = temporary.path().join("browser-clear.json");
    let mut stderr = tempfile::tempfile().context("creating browser clearing stderr capture")?;
    // Launch the executable directly so timeout cleanup owns the exact process,
    // including an older app that does not recognize the headless command.
    let mut command = Command::new(executable);
    command.arg("__clear-browser-data");
    if matches!(helper, BrowserHelper::Check) {
        command.arg("--check");
    }
    let mut child = command
        .arg(&receipt)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(stderr.try_clone()?)
        .spawn().context("starting Gents to clear browser data; install the matching Gents desktop app before resetting")?;
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        result => {
            let kill_error = child.kill().err();
            let reap_error = child.wait().err();
            bail!(
                "Gents browser clearing did not finish within its deadline ({result:?}; kill error: {kill_error:?}; reap error: {reap_error:?}). Install the matching desktop app and retry."
            );
        }
    };
    let receipt_bytes = std::fs::read(&receipt);
    let completion = receipt_bytes
        .as_ref()
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok());
    if let Some(error) = completion
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(serde_json::Value::as_str)
    {
        bail!("Gents could not clear browser data: {error}");
    }
    if !status.success() {
        stderr.seek(SeekFrom::Start(0))?;
        let mut diagnostic = String::new();
        stderr.take(64 * 1024).read_to_string(&mut diagnostic)?;
        bail!(
            "Gents could not clear browser data: {}. Install the matching Gents desktop app and retry.",
            diagnostic.trim()
        );
    }
    receipt_bytes.context(
        "Gents did not confirm browser clearing; install the matching desktop app and retry",
    )?;
    if completion
        .as_ref()
        .and_then(|value| value.get("completed"))
        .and_then(serde_json::Value::as_bool)
        != Some(true)
    {
        bail!("Gents did not provide a valid browser-clearing completion receipt");
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn stalled_browser_helper_is_killed_and_reaped() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("desktop");
        std::fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let start = std::time::Instant::now();
        let error = run_browser_helper(
            &executable,
            BrowserHelper::Clear,
            std::time::Duration::from_millis(50),
        )
        .unwrap_err();
        assert!(error.to_string().contains("deadline"));
        assert!(error.to_string().contains("reap error: None"));
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn browser_clear_requires_a_successful_native_completion_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("desktop");
        for (body, expected) in [
            ("exit 0\n", false),
            ("printf '%s' '{\"completed\":false}' > \"$2\"\n", false),
            ("printf '%s' '{\"completed\":true}' > \"$2\"\n", true),
        ] {
            std::fs::write(&executable, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                run_browser_helper(&executable, BrowserHelper::Clear, BROWSER_CLEAR_TIMEOUT)
                    .is_ok(),
                expected
            );
        }
    }

    #[test]
    fn browser_check_reports_a_store_held_by_another_desktop() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("desktop");
        std::fs::write(
            &executable,
            "#!/bin/sh\n[ \"$2\" = --check ] || exit 9\nprintf '%s' '{\"completed\":false,\"error\":\"store held\"}' > \"$3\"\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let error = run_browser_helper(&executable, BrowserHelper::Check, BROWSER_CHECK_TIMEOUT)
            .unwrap_err();
        assert!(format!("{error:#}").contains("store held"), "{error:#}");
    }

    fn initialized_reset(user: &std::path::Path) -> (std::path::PathBuf, HomeResetPlan) {
        let home = user.join(".gents");
        std::fs::create_dir_all(home.join("data")).unwrap();
        std::fs::write(home.join("data/MANIFEST"), "store").unwrap();
        std::fs::write(
            home.join("init.json"),
            serde_json::json!({
                "home": home,
                "node_name": "local",
                "node_did": "did:key:zlocal",
                "key_path": null,
                "tool_ceiling": "workspace",
                "tool_root": null,
            })
            .to_string(),
        )
        .unwrap();
        let desktop = DesktopPaths::from_root(user.join("desktop"));
        let plan =
            plan_full_reset(Some(&home), &desktop, user, &user.join("service.plist")).unwrap();
        (home, plan)
    }

    #[test]
    fn a_failed_archive_never_clears_browser_data() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, plan) = initialized_reset(&user);
        let backup = user.join("missing-parent/backup");
        let mut cleared = false;
        let retry = retry_command(&home, &user.join("desktop")).unwrap();
        let error = retire_then_clear(
            &plan,
            HomeResetDisposition::Archive,
            Some(&backup),
            &retry,
            || {
                cleared = true;
                Ok(())
            },
        )
        .unwrap_err();
        assert!(!cleared, "browser data was cleared after {error:#}");
        assert!(home.join("data/MANIFEST").is_file());
        assert!(home.join("init.json").is_file());
    }

    #[tokio::test]
    async fn browser_clearing_failure_advertises_a_retry_that_reviews_the_same_state() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().canonicalize().unwrap();
        let (home, plan) = initialized_reset(&user);
        let desktop = user.join("desktop");
        let backup = plan.backup_path("test").unwrap();
        let retry = retry_command(&home, &desktop).unwrap();
        let error = retire_then_clear(
            &plan,
            HomeResetDisposition::Archive,
            Some(&backup),
            &retry,
            || {
                assert!(backup.join("home/data/MANIFEST").is_file());
                bail!("WebKit refused")
            },
        )
        .unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains(&backup.display().to_string()), "{message}");
        assert!(
            message.contains("browser data was not cleared"),
            "{message}"
        );
        assert!(!home.join("data").exists());

        let advertised = message.split('`').nth(1).expect("retry command");
        let argv = shlex::split(advertised).expect("shell words");
        let crate::cli::Command::Desktop {
            command:
                DesktopCommand::Reset {
                    home: retry_home,
                    desktop_home,
                    delete,
                    confirm,
                    ..
                },
        } = <crate::cli::Cli as clap::Parser>::try_parse_from(argv)
            .unwrap()
            .command
        else {
            panic!("the retry is not a desktop reset: {advertised}");
        };
        let mut ready = false;
        let mut next_retry = String::new();
        let preview = reset(
            retry_home,
            desktop_home,
            delete,
            confirm,
            false,
            None,
            None,
            &mut ready,
            &mut next_retry,
        )
        .await
        .unwrap();
        assert!(!preview.completed);
        assert_eq!(preview.managed_home.as_deref(), home.to_str());
        assert_eq!(next_retry, retry);
    }

    #[test]
    fn an_env_selected_relative_desktop_is_retried_from_another_directory() {
        let temp = tempfile::tempdir().unwrap();
        let (first, second) = (temp.path().join("first"), temp.path().join("second"));
        let selected = select_desktop(None, Some("rel/desktop".into()), &first).unwrap();
        assert_eq!(selected.root(), first.join("rel/desktop"));
        let retry = retry_command(&temp.path().join("home"), selected.root()).unwrap();
        let crate::cli::Command::Desktop {
            command: DesktopCommand::Reset { desktop_home, .. },
        } = <crate::cli::Cli as clap::Parser>::try_parse_from(shlex::split(&retry).unwrap())
            .unwrap()
            .command
        else {
            panic!("the retry is not a desktop reset: {retry}");
        };
        let retried = select_desktop(desktop_home, None, &second).unwrap();
        assert_eq!(retried.root(), selected.root());
    }

    #[test]
    fn retry_command_quotes_paths_with_spaces() {
        let command = retry_command(
            std::path::Path::new("/Users/a b/.gents"),
            std::path::Path::new("/Users/a b/Library/Application Support/gents/desktop"),
        )
        .unwrap();
        assert_eq!(
            shlex::split(&command).unwrap(),
            [
                "gents",
                "desktop",
                "reset",
                "--home",
                "/Users/a b/.gents",
                "--desktop-home",
                "/Users/a b/Library/Application Support/gents/desktop",
            ]
        );
    }
}
