use std::sync::OnceLock;

use crate::config::TracingConfig;
use crate::logging;

static TAURI_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

/// Install the large-stack Tokio runtime Tauri will use. Must run **before**
/// `tauri::Builder`. Idempotent. Required on iOS (DefraDB history replay).
pub fn install_runtime() {
    let runtime = TAURI_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_stack_size(32 * 1024 * 1024)
            .build()
            .expect("failed to build Tauri Tokio runtime")
    });
    tauri::async_runtime::set(runtime.handle().clone());
}

pub fn init_tracing(config: TracingConfig) {
    logging::init_tracing_with_config(config);
}
