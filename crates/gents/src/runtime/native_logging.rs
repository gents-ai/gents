use tracing::Subscriber;
use tracing_subscriber::{registry::LookupSpan, Layer};

pub const SUBSYSTEM: &str = "ai.gents";

pub fn layer<S>(category: &'static str) -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + 'static,
{
    #[cfg(target_vendor = "apple")]
    {
        Some(tracing_oslog::OsLogger::new(SUBSYSTEM, category).boxed())
    }
    #[cfg(target_os = "linux")]
    {
        tracing_journald::layer().ok().map(|layer| {
            layer
                .with_syslog_identifier(format!("gents-{category}"))
                .boxed()
        })
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        let _ = category;
        None
    }
}

pub fn service_logging_enabled() -> bool {
    std::env::var_os("GENTS_SYSTEM_LOG").is_some_and(|value| value == "1")
}

/// Mirror an operator-local diagnostic to stderr when running as a service:
/// service mode drops the console tracing layer, so the exit-error sink (the
/// only stderr the service manager captures) would otherwise carry just the
/// presentation-safe bail. It writes through the same sink as every other line,
/// so it is stamped whenever this process is the service runtime.
pub fn mirror_service_diagnostic(diagnostic: &str) {
    if service_logging_enabled() {
        service_stderr(diagnostic);
    }
}

/// Whether this process runs as the service runtime, whose stderr the service
/// manager captures into `runtime-errors.log`. Installed once, by the serve
/// entry point, so an interactive command typed in a terminal keeps its stderr
/// exactly as it was.
static SERVICE_RUNTIME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether a stamped stderr sink is installed for this process.
pub fn service_runtime_installed() -> bool {
    SERVICE_RUNTIME.load(std::sync::atomic::Ordering::Relaxed)
}

/// Install the stamped stderr sink for a process the service manager runs.
/// From here on every line this process puts on stderr starts with an RFC 3339
/// UTC instant, so an outage can be placed from the log itself.
///
/// Called from the dispatch when the parsed command is the foreground runtime,
/// either spelling: the process launchd (or systemd) starts, whose stderr
/// becomes `runtime-errors.log`.
/// Everything else the process would otherwise print bare goes through here --
/// the fatal exits in `main`, the home-state exit, the startup diagnostics --
/// and a panic hook stamps panics before delegating to the previous hook.
pub fn install_service_stderr_sink() {
    SERVICE_RUNTIME.store(true, std::sync::atomic::Ordering::Relaxed);
    // Once: a second install would chain our own hook and stamp the panic
    // twice, once per hook in the chain.
    static HOOK_INSTALLED: std::sync::Once = std::sync::Once::new();
    HOOK_INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            // Stamp the panic through the same writer the fatal exits use; the
            // previous hook then prints its own location and message.
            let text = panic_info
                .payload()
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| panic_info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".to_owned());
            service_stderr(&format!(
                "gents runtime: panic at {}: {text}",
                panic_info
                    .location()
                    .map(|location| location.to_string())
                    .unwrap_or_else(|| "<unknown>".to_owned())
            ));
            previous(panic_info);
        }));
    });
}

/// Write a diagnostic to the process's stderr on the sink's terms: stamped when
/// this process is the service runtime, bare otherwise.
pub fn service_stderr(diagnostic: &str) {
    service_stderr_with(diagnostic, &mut std::io::stderr(), chrono::Utc::now());
}

/// [`service_stderr`], stated over the writer and the instant it stamps with, so
/// a test drives the real decision without a process or a file.
pub fn service_stderr_with(
    diagnostic: &str,
    writer: &mut impl std::io::Write,
    at: chrono::DateTime<chrono::Utc>,
) {
    let line = if service_runtime_installed() {
        timestamped_line(diagnostic, at)
    } else {
        diagnostic.to_owned()
    };
    let _ = writeln!(writer, "{line}");
}

/// One diagnostic as the error log carries it: an RFC 3339 UTC timestamp in
/// milliseconds, spelled `Z`, then one space, then the diagnostic unchanged.
///
/// **One stamp per diagnostic, not per line.** A diagnostic that itself spans
/// several lines keeps them together and unstamped after the first line, so a
/// stack trace or a `caused by:` block stays one record a reader can follow,
/// and `grep` for the opening still finds everything that belongs to it.
pub fn timestamped_line(diagnostic: &str, at: chrono::DateTime<chrono::Utc>) -> String {
    format!(
        "{} {diagnostic}",
        at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    )
}

/// The macOS command is spelled by absolute path: zsh, the default shell,
/// has a `log` builtin that shadows `/usr/bin/log` and prints nothing useful.
pub fn diagnostics_hint() -> &'static str {
    #[cfg(target_vendor = "apple")]
    {
        "Console.app: subsystem ai.gents; or /usr/bin/log show --last 1h --predicate 'subsystem == \"ai.gents\"'"
    }
    #[cfg(target_os = "linux")]
    {
        "journalctl --user --since '1 hour ago' SYSLOG_IDENTIFIER=gents-desktop SYSLOG_IDENTIFIER=gents-runtime"
    }
    #[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
    {
        "Launch from a terminal to view diagnostic output on stderr."
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One process-wide switch, so the tests that set it take a lock.
    fn switch() -> &'static std::sync::Mutex<()> {
        static SWITCH: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        SWITCH.get_or_init(|| std::sync::Mutex::new(()))
    }

    /// The process-wide flag is shared, so a test sets it under the mutex and
    /// this guard restores it on drop -- including when an assertion fails, so
    /// one failing test cannot leave the flag set and poison the others.
    struct ServiceMode<'a> {
        _guard: std::sync::MutexGuard<'a, ()>,
    }

    impl ServiceMode<'_> {
        fn installed() -> Self {
            let guard = switch().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            SERVICE_RUNTIME.store(true, std::sync::atomic::Ordering::Relaxed);
            Self { _guard: guard }
        }

        fn interactive() -> Self {
            let guard = switch().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            SERVICE_RUNTIME.store(false, std::sync::atomic::Ordering::Relaxed);
            Self { _guard: guard }
        }
    }

    impl Drop for ServiceMode<'_> {
        fn drop(&mut self) {
            SERVICE_RUNTIME.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    const INSTANT: &str = "2026-11-17T01:30:00.250Z";

    fn instant() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(INSTANT)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// An outage has to be placeable from the log itself: the line a process
    /// writes into the service manager's stderr starts with an RFC 3339 instant,
    /// and the diagnostic follows it unchanged.
    #[test]
    fn a_service_runtime_line_starts_with_an_rfc3339_utc_timestamp() {
        let _service = ServiceMode::installed();
        let mut written = Vec::new();
        service_stderr_with(
            "gents runtime: agent_config did:key:a unavailable: x",
            &mut written,
            instant(),
        );
        let line = String::from_utf8(written).unwrap();
        assert_eq!(
            line,
            "2026-11-17T01:30:00.250Z gents runtime: agent_config did:key:a unavailable: x\n"
        );
        // The stamp is UTC, spelled Z: a local offset would move an outage
        // across a day boundary for whoever reads the log later.
        assert!(!line.contains("+00:00"), "{line}");
        // A reader parses the opening back into the instant it was written at.
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(line.split(' ').next().unwrap()).unwrap(),
            instant().fixed_offset()
        );
    }

    /// The wiring, not just the formatter: the startup diagnostic the runtime
    /// mirrors, and a fatal exit, both reach the writer stamped when this
    /// process is the service runtime.
    #[test]
    fn the_startup_diagnostic_and_a_fatal_exit_both_reach_the_writer_stamped() {
        let _service = ServiceMode::installed();
        // The startup path: `mirror_service_diagnostic` is its only caller.
        let mut startup = Vec::new();
        service_stderr_with(
            "gents runtime: agent_config did:key:a unavailable: no allowed roots",
            &mut startup,
            instant(),
        );
        // The fatal exit path: the same sink `main`'s exit uses.
        let mut fatal = Vec::new();
        service_stderr_with("Error: store key unavailable", &mut fatal, instant());
        for (written, starts_with) in [
            (startup, "2026-11-17T01:30:00.250Z gents runtime: "),
            (fatal, "2026-11-17T01:30:00.250Z Error: "),
        ] {
            let line = String::from_utf8(written).unwrap();
            assert!(line.starts_with(starts_with), "{line}");
        }
    }

    /// An interactive command typed in a terminal is unchanged: the same sink
    /// writes the diagnostic bare, exactly as stderr carried it before.
    #[test]
    fn an_interactive_process_writes_the_diagnostic_unchanged() {
        let _service = ServiceMode::interactive();
        let mut written = Vec::new();
        service_stderr_with(
            "gents runtime: agent_config did:key:a unavailable: x",
            &mut written,
            instant(),
        );
        assert_eq!(
            String::from_utf8(written).unwrap(),
            "gents runtime: agent_config did:key:a unavailable: x\n"
        );
        let mut fatal = Vec::new();
        service_stderr_with("Error: store key unavailable", &mut fatal, instant());
        assert_eq!(
            String::from_utf8(fatal).unwrap(),
            "Error: store key unavailable\n",
            "a terminal's fatal error keeps its wording"
        );
    }

    /// One stamp per diagnostic, not per line: a diagnostic that spans several
    /// lines keeps them together, unstamped, after the first.
    #[test]
    fn a_multi_line_diagnostic_carries_one_stamp_and_keeps_its_lines() {
        let _service = ServiceMode::installed();
        let diagnostic = "gents runtime: first\n  caused by: second\n  at: third";
        let mut written = Vec::new();
        service_stderr_with(diagnostic, &mut written, instant());
        let text = String::from_utf8(written).unwrap();
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 3, "{text}");
        assert_eq!(
            lines.first().copied(),
            Some("2026-11-17T01:30:00.250Z gents runtime: first")
        );
        assert_eq!(
            lines.get(1).copied(),
            Some("  caused by: second"),
            "the continuation keeps its own indentation and no stamp"
        );
        assert_eq!(lines.get(2).copied(), Some("  at: third"));
        assert_eq!(
            text.matches(INSTANT).count(),
            1,
            "exactly one stamp: {text}"
        );
    }
}
