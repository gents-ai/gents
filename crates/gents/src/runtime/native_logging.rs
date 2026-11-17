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
/// presentation-safe bail.
pub fn mirror_service_diagnostic(diagnostic: &str) {
    if service_logging_enabled() {
        eprintln!("{}", timestamped_line(diagnostic, chrono::Utc::now()));
    }
}

/// One line as the error log carries it: an RFC 3339 UTC timestamp, then the
/// diagnostic. The service manager captures this process's stderr into
/// `runtime-errors.log` (launchd's `StandardErrorPath`, systemd's
/// `StandardError`), so the sink here is the only place a line can be stamped;
/// stamping it there also covers a line that itself spans several lines.
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

    /// An outage has to be placeable from the log itself, not from the file's
    /// modification time: every line starts with an RFC 3339 UTC instant.
    #[test]
    fn a_mirrored_diagnostic_starts_with_an_rfc3339_utc_timestamp() {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-17T01:30:00.250Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let line = timestamped_line("gents runtime: agent_config did:key:a unavailable: x", at);
        assert_eq!(
            line,
            "2026-11-17T01:30:00.250Z gents runtime: agent_config did:key:a unavailable: x"
        );
        // The stamp is UTC, since a local offset would move an outage across a
        // day boundary for whoever reads it later.
        assert!(line.starts_with("2026-11-17T01:30:00.250Z "));
        assert!(!line.contains("+00:00"), "UTC is spelled Z: {line}");
        // A reader can parse the opening back into an instant.
        let stamp = line.split(' ').next().unwrap();
        assert_eq!(
            chrono::DateTime::parse_from_rfc3339(stamp).unwrap(),
            at.fixed_offset()
        );
    }

    /// The diagnostic itself is untouched: only the leading stamp is added, and
    /// the message keeps the shape every reader of the log matches on.
    #[test]
    fn the_stamp_is_prefixed_without_altering_the_diagnostic() {
        let at = chrono::Utc::now();
        let diagnostic = "gents runtime: service exited with code 1";
        let line = timestamped_line(diagnostic, at);
        assert!(line.ends_with(diagnostic), "{line}");
        assert_eq!(
            line.strip_prefix(line.split(' ').next().unwrap())
                .and_then(|rest| rest.strip_prefix(' ')),
            Some(diagnostic),
            "one space, then the diagnostic unchanged"
        );
        // A multi-line diagnostic keeps its own lines after the stamp, so the
        // first line is never lost when grepping for `gents runtime:`.
        let multi = "gents runtime: first\n  caused by: second";
        let stamped = timestamped_line(multi, at);
        assert!(stamped.contains("gents runtime: first"));
        assert!(stamped.contains("  caused by: second"));
        assert_eq!(
            stamped.matches("2026-").count(),
            if at.format("%Y").to_string() == "2026" {
                1
            } else {
                0
            },
            "exactly one stamp, the leading one: {stamped}"
        );
    }
}
