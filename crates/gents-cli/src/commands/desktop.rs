use anyhow::{bail, Context, Result};
use gents_desktop_core::{
    client::DesktopPaths,
    home_reset::{
        lock_client_state, lock_desktop_process, lock_reset, plan_full_reset, resolve_user_home,
        HomeResetDisposition, ManagedServerResetResult,
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
        delete,
        confirm,
        wait_for_desktop,
        desktop_pid,
        desktop_executable,
    } = command;
    let mut ready = false;
    let result = reset(
        home,
        delete,
        confirm,
        wait_for_desktop,
        desktop_pid,
        desktop_executable,
        &mut ready,
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
                "Reset could not finish: {error:#}. Run gents desktop reset to review the remaining state and retry."
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

async fn reset(
    home: Option<std::path::PathBuf>,
    delete: bool,
    confirm: Option<String>,
    wait_for_desktop: bool,
    desktop_pid: Option<u32>,
    desktop_executable: Option<std::path::PathBuf>,
    ready: &mut bool,
) -> Result<ManagedServerResetResult> {
    let desktop = DesktopPaths::discover()?;
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
    let backup = if delete {
        None
    } else {
        let path =
            plan.backup_path(&chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string())?;
        plan.preflight(&path)?;
        Some(path)
    };
    let service = NativeServiceManager::new(config)?;
    service.stop(true)?;
    if service.status()?.is_active_or_transitioning() {
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
    clear_browser_data(desktop_executable.as_deref())?;
    let settled = plan_full_reset(Some(&home), &desktop, &user_home, &definition)?;
    settled.check_confirmation(&confirmation, disposition)?;
    let mut result = settled.retire(disposition, backup.as_deref())?;
    result.cleared_browser_paths = result.browser_paths.clone();
    Ok(result)
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
            // Signal zero observes process existence without sending a signal.
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

/// WebKit chooses its store by the owning app identity. The CLI therefore asks
/// the installed desktop executable to clear it and requires a native completion
/// receipt; it never removes browser directories itself.
fn clear_browser_data(executable: Option<&std::path::Path>) -> Result<()> {
    let executable = match executable {
        Some(path) => path.to_path_buf(),
        None => installed_desktop_executable()?,
    };
    clear_browser_data_with_timeout(&executable, std::time::Duration::from_secs(75))
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

fn clear_browser_data_with_timeout(
    executable: &std::path::Path,
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
    let mut child = Command::new(executable)
        .arg("__clear-browser-data").arg(&receipt)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(stderr.try_clone()?)
        .spawn().context("starting Gents to clear browser data; install the matching Gents desktop app before resetting")?;
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        result => {
            let kill_error = child.kill().err();
            let reap_error = child.wait().err();
            bail!(
                "Gents browser clearing did not finish within its deadline ({result:?}; kill error: {kill_error:?}; reap error: {reap_error:?}). Install the matching desktop app and retry. The managed home and client state were not retired."
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
        bail!(
            "Gents could not clear browser data: {error}. The managed home and client state were not retired."
        );
    }
    if !status.success() {
        stderr.seek(SeekFrom::Start(0))?;
        let mut diagnostic = String::new();
        stderr.take(64 * 1024).read_to_string(&mut diagnostic)?;
        bail!(
            "Gents could not clear browser data: {}. Install the matching Gents desktop app and retry. The managed home and client state were not retired.",
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
        bail!(
            "Gents did not provide a valid browser-clearing completion receipt; the managed home and client state were not retired."
        );
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
        let error =
            clear_browser_data_with_timeout(&executable, std::time::Duration::from_millis(50))
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
            assert_eq!(clear_browser_data(Some(&executable)).is_ok(), expected);
        }
    }
}
