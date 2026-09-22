//! Thin adapters from our existing tracing events to OS-owned log storage.
//!
//! No files, rotation, collector processes, or retention policy live here.
//! A foreground CLI keeps stderr; installed services opt into the native sink.

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

/// Human-readable diagnostic instructions, not a pretend file path.
pub fn diagnostics_hint() -> &'static str {
    #[cfg(target_vendor = "apple")]
    {
        "Console.app: subsystem ai.gents; or log show --last 1h --predicate 'subsystem == \"ai.gents\"'"
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
