//! Opening a URL in the person's own browser.
//!
//! The generic crates run whichever opener they find first and treat exit 0 as
//! proof. Neither holds inside a packaged build: the package puts its own
//! copies of the host's tools first on `PATH`, and an opener from an older
//! distribution can return 0 on a desktop it does not know while opening
//! nothing at all. Both failures are silent, and a sign-in waiting on the
//! browser then waits for a callback that cannot arrive.
//!
//! So this resolves each candidate against the host's own directories, runs it
//! with the package's environment stripped ([`package_tools::prepare_host_command`]),
//! and reports what happened. Nothing here can prove a window appeared, which
//! is why every caller also offers the URL.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::package_tools;

/// How long an opener may run before it counts as launched. A dispatcher like
/// `xdg-open` returns in milliseconds; a browser started directly runs for as
/// long as the browser does, and staying alive is its success signal.
const SETTLE: Duration = Duration::from_millis(1500);

/// Dispatchers first, in the order a desktop session expects them, then the
/// browsers themselves for a session that has no dispatcher at all.
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

/// Open `url` in the person's browser. `Ok` means an opener was started, not
/// that a window appeared: an opener can exit 0 having done nothing, so the
/// caller keeps offering the URL either way.
pub fn open_url(url: &str) -> Result<(), OpenError> {
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
        // A `%s` in BROWSER is the URL's place; without one the URL is the
        // last argument.
        if args.iter().any(|arg| arg.to_string_lossy().contains("%s")) {
            for arg in &mut args {
                let filled = arg.to_string_lossy().replace("%s", url);
                *arg = OsString::from(filled);
            }
        } else {
            args.push(OsString::from(url));
        }
        match launched(&resolved, &args) {
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

/// Whether `program` started and did not fail. Still running once it has had
/// time to settle counts as launched: that is a browser holding the session,
/// not a dispatcher refusing the URL.
fn launched(program: &PathBuf, args: &[OsString]) -> std::io::Result<bool> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    package_tools::prepare_host_command(&mut command);
    let mut child = command.spawn()?;
    let deadline = Instant::now() + SETTLE;
    loop {
        match child.try_wait()? {
            Some(status) => return Ok(status.success()),
            None if Instant::now() >= deadline => {
                // Reap it on its own thread so a long-lived browser leaves no
                // zombie behind.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(true);
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// `BROWSER` as the desktop convention defines it: a colon-separated list of
/// commands, each optionally placing the URL with `%s`.
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

/// Find `program` in the host's own directories, skipping any the package
/// contributed, so the copy that runs is the one built for this system.
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

/// Whether `url` is safe to hand to a system opener. An opener dispatches by
/// scheme, so anything beyond the web schemes could start an unrelated local
/// handler, and a leading `-` would read as an option rather than a URL.
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
        // A local handler, a shell-ish argument, or an embedded newline is
        // never dispatched.
        assert!(!is_openable("file:///etc/passwd"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("-version"));
        assert!(!is_openable("https://example.com\nrm -rf"));
        assert!(!is_openable("https:"));
        assert!(!is_openable("not-a-url"));
    }

    #[test]
    fn browser_is_read_as_the_desktop_convention_defines_it() {
        temp_env("BROWSER", Some("firefox --new-tab %s:chromium"), || {
            assert_eq!(
                browser_env_openers(),
                vec![
                    vec![
                        "firefox".to_owned(),
                        "--new-tab".to_owned(),
                        "%s".to_owned()
                    ],
                    vec!["chromium".to_owned()],
                ]
            );
        });
        temp_env("BROWSER", None, || {
            assert!(browser_env_openers().is_empty());
        });
    }

    #[test]
    fn a_program_with_a_path_is_taken_as_given() {
        assert!(resolve_host_program("/definitely/not/here").is_none());
        assert_eq!(
            resolve_host_program("/bin/sh"),
            std::fs::metadata("/bin/sh")
                .is_ok()
                .then(|| PathBuf::from("/bin/sh"))
        );
    }

    fn temp_env(key: &str, value: Option<&str>, body: impl FnOnce()) {
        let previous = std::env::var_os(key);
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        body();
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}
