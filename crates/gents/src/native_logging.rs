//! Thin adapters from our existing tracing events to OS-owned log storage.
//!
//! No files, rotation, collector processes, or retention policy live here.
//! A foreground CLI keeps stderr; installed services opt into the native sink,
//! and their service definition sends what reaches stderr anyway (errors
//! before tracing starts, the exit error, panics) to a file the service
//! manager appends to.

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

/// A service definition sets this explicitly; ordinary CLI output stays local.
pub fn service_logging_enabled() -> bool {
    std::env::var_os("GENTS_SYSTEM_LOG").is_some_and(|value| value == "1")
}

/// Mirror an operator-local diagnostic to stderr when running as a service.
/// Service mode drops the console tracing layer, so without this the exit-error
/// sink (the only stderr the service manager captures) would carry just the
/// presentation-safe bail and never the diagnostic an operator needs.
pub fn mirror_service_diagnostic(diagnostic: &str) {
    if service_logging_enabled() {
        eprintln!("{diagnostic}");
    }
}

/// Human-readable diagnostic instructions, not a pretend file path.
///
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
