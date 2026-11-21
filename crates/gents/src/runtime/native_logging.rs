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

/// Mirror an operator-local diagnostic to stderr when the host logs to the
/// system log (`gents server --system-log`): that mode drops the console
/// tracing layer, so the exit-error sink (the only stderr the service manager
/// captures) would otherwise carry just the presentation-safe bail.
pub fn mirror_service_diagnostic(system_log: bool, diagnostic: &str) {
    if system_log {
        Stderr::new(system_log).write(diagnostic);
    }
}

/// A process's stderr. Under `--system-log` it is the service manager's error
/// log, where nothing else dates a line, so each diagnostic opens with its
/// RFC 3339 UTC instant; continuation lines stay with it unstamped. A
/// terminal's stderr is written unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stderr {
    system_log: bool,
}

impl Stderr {
    pub fn new(system_log: bool) -> Self {
        Self { system_log }
    }

    pub fn write(self, diagnostic: &str) {
        let _ = self.write_at(
            &mut std::io::stderr().lock(),
            chrono::Utc::now(),
            diagnostic,
        );
    }

    fn write_at(
        self,
        out: &mut impl std::io::Write,
        at: chrono::DateTime<chrono::Utc>,
        diagnostic: &str,
    ) -> std::io::Result<()> {
        if self.system_log {
            write!(
                out,
                "{} ",
                at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            )?;
        }
        writeln!(out, "{diagnostic}")
    }

    /// Dates a panic in the service log before the previous hook reports it.
    /// Called once, by the process's entry point.
    pub fn install_panic_hook(self) {
        if !self.system_log {
            return;
        }
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            self.write(&format!("gents runtime: panic at {}", panic_location(info)));
            previous(info);
        }));
    }
}

fn panic_location(info: &std::panic::PanicHookInfo<'_>) -> String {
    info.location()
        .map_or_else(|| "an unknown location".to_owned(), ToString::to_string)
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

    fn written(stderr: Stderr, diagnostic: &str) -> String {
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-17T01:30:00.250Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let mut out = Vec::new();
        stderr.write_at(&mut out, at, diagnostic).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_service_log_diagnostic_opens_with_its_utc_instant() {
        assert_eq!(
            written(Stderr::new(true), "gents runtime: the store is busy"),
            "2026-11-17T01:30:00.250Z gents runtime: the store is busy\n"
        );
    }

    #[test]
    fn a_multi_line_diagnostic_carries_one_stamp() {
        assert_eq!(
            written(Stderr::new(true), "Error: refused\n  caused by: busy"),
            "2026-11-17T01:30:00.250Z Error: refused\n  caused by: busy\n"
        );
    }

    #[test]
    fn a_terminal_diagnostic_is_written_unchanged() {
        assert_eq!(
            written(Stderr::new(false), "Error: refused"),
            "Error: refused\n"
        );
    }
}
