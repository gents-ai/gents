mod support;

#[cfg(unix)]
#[path = "editor_checkpoints.rs"]
mod editor_checkpoints;
#[cfg(target_os = "macos")]
#[path = "editor_store.rs"]
mod editor_store;
#[cfg(all(target_os = "macos", feature = "editor-e2e"))]
#[path = "editor_store_installed.rs"]
mod editor_store_installed;
#[cfg(any(unix, windows))]
#[path = "installation.rs"]
mod installation;
#[cfg(any(unix, windows))]
#[path = "installation_capacity.rs"]
mod installation_capacity;
#[cfg(any(unix, windows))]
#[path = "installation_handoff.rs"]
mod installation_handoff;
#[path = "metadata.rs"]
mod metadata;
#[cfg(all(any(unix, windows), feature = "runtime-e2e"))]
#[path = "runtime.rs"]
mod runtime;
#[path = "runtime_cli.rs"]
mod runtime_cli;
