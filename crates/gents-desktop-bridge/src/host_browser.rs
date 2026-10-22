//! Opening a URL in the person's own browser: resolved against the host's own
//! directories and run with the package's environment stripped
//! ([`package_tools::prepare_host_command`]), because a packaged opener from an
//! older distribution can exit 0 on an unknown desktop having opened nothing.
//! Nothing here can prove a window appeared, so every caller also offers the URL.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::package_tools;

const SETTLE: Duration = Duration::from_millis(1500);

const OPENERS: &[&[&str]] = &[
    &["xdg-open"],
    &["gio", "open"],
    &["x-www-browser"],
    &["sensible-browser"],
    &["gnome-open"],
    &["kde-open"],
    &["exo-open"],
    &["firefox"],
    &["chromium"],
    &["google-chrome"],
    &["brave-browser"],
    &["microsoft-edge"],
    &["vivaldi"],
    &["opera"],
    &["epiphany"],
    &["falkon"],
    &["qutebrowser"],
];

#[derive(Debug)]
pub struct OpenError {
    attempted: Vec<String>,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.attempted.is_empty() {
            f.write_str("no browser opener is installed on this system")
        } else {
            write!(
                f,
                "no browser opener on this system could open the page (tried {})",
                self.attempted.join(", ")
            )
        }
    }
}

impl std::error::Error for OpenError {}

pub async fn open_url(url: &str) -> Result<(), OpenError> {
    // macOS and Windows use their launch service in-process, so no package
    // environment reaches a child.
    if cfg!(any(target_os = "macos", windows)) {
        return webbrowser::open(url).map_err(|error| OpenError {
            attempted: vec![format!("the system launch service ({error})")],
        });
    }
    let mut candidates = browser_env_openers();
    candidates.extend(
        OPENERS
            .iter()
            .map(|argv| argv.iter().map(|part| (*part).to_owned()).collect()),
    );
    let mut attempted = Vec::new();
    for argv in candidates {
        let Some((program, mut args)) = argv.split_first().map(|(program, rest)| {
            (
                program.clone(),
                rest.iter().map(OsString::from).collect::<Vec<_>>(),
            )
        }) else {
            continue;
        };
        let Some(resolved) = resolve_host_program(&program) else {
            continue;
        };
        attempted.push(program.clone());
        // A `%s` in BROWSER marks the URL's place.
        if args.iter().any(|arg| arg.to_string_lossy().contains("%s")) {
            for arg in &mut args {
                let filled = arg.to_string_lossy().replace("%s", url);
                *arg = OsString::from(filled);
            }
        } else {
            args.push(OsString::from(url));
        }
        match launched(&resolved, &args).await {
            Ok(true) => {
                tracing::info!(opener = %program, "opened the page in the host browser");
                return Ok(());
            }
            Ok(false) => {
                tracing::debug!(opener = %program, "browser opener reported failure");
            }
            Err(error) => {
                tracing::debug!(opener = %program, %error, "browser opener could not be started");
            }
        }
    }
    Err(OpenError { attempted })
}

/// Whether `program` started and did not fail: still running once it has had
/// time to settle is a browser holding the session, not a dispatcher refusing.
async fn launched(program: &PathBuf, args: &[OsString]) -> std::io::Result<bool> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    package_tools::prepare_host_command(&mut command);
    // A dropped tokio child is reaped by the runtime, so a long-lived browser
    // leaves no zombie.
    let mut child = tokio::process::Command::from(command).spawn()?;
    match tokio::time::timeout(SETTLE, child.wait()).await {
        Ok(status) => Ok(status?.success()),
        Err(_) => Ok(true),
    }
}

fn browser_env_openers() -> Vec<Vec<String>> {
    let Some(browser) = std::env::var_os("BROWSER") else {
        return Vec::new();
    };
    browser
        .to_string_lossy()
        .split(':')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| {
            entry
                .split_whitespace()
                .map(ToOwned::to_owned)
                .collect::<Vec<String>>()
        })
        .filter(|argv| !argv.is_empty())
        .collect()
}

fn resolve_host_program(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return executable(&path).then_some(path);
    }
    let app_dir = std::env::var_os("APPDIR").map(PathBuf::from);
    let app_image = std::env::var_os("APPIMAGE").map(PathBuf::from);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|directory| {
            !gents_server::native_service::inside_temporary_package(
                directory,
                app_dir.as_deref(),
                app_image.as_deref(),
            )
        })
        .map(|directory| directory.join(program))
        .find(|candidate| executable(candidate))
}

fn executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Whether `url` is safe to hand to a system opener: web schemes only, no
/// leading option-like `-`, no embedded line breaks or NULs.
pub fn is_openable(url: &str) -> bool {
    !url.starts_with('-')
        && url.split_once(':').is_some_and(|(scheme, rest)| {
            matches!(
                scheme.to_ascii_lowercase().as_str(),
                "http" | "https" | "mailto"
            ) && !rest.is_empty()
        })
        && !url.contains(['\n', '\r', '\0'])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_schemes_reach_a_system_opener() {
        assert!(is_openable("https://claude.ai/oauth/authorize?code=1"));
        assert!(is_openable("http://localhost:8123/callback"));
        assert!(is_openable("mailto:someone@example.com"));
        assert!(!is_openable("file:///etc/passwd"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("-version"));
        assert!(!is_openable("https://example.com\nrm -rf"));
        assert!(!is_openable("https:"));
        assert!(!is_openable("not-a-url"));
    }
}
