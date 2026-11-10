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
        eprintln!("{diagnostic}");
    }
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
