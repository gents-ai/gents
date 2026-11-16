//! The nuclei runtime that drives a plugin's WASI host calls.
//!
//! Afterburner's WASI host answers each synchronous host call by driving its
//! future with `nuclei::block_on` on the calling thread, inside the nuclei
//! runtime set on that thread, else nuclei's process default runtime. That
//! default starts one worker per core plus a monitor the first time a guest
//! calls into WASI, a second executor beside Gents' Tokio runtime that
//! oversubscribes the node's cores. A plugin runs on a Tokio blocking thread,
//! so its host calls need no workers of their own: this runtime has none, and
//! every task it is handed runs on the plugin's thread inside that thread's
//! `block_on`. Its blocking pool still runs WASI filesystem calls off that
//! thread. Leaving the default runtime unbuilt also leaves it to whichever
//! other library in the process configures it.
use std::sync::OnceLock;

use anyhow::{Context, Result};

/// Sets the plugin WASI runtime on the calling thread until the guard drops.
///
/// The caller is a dedicated blocking thread that runs one plugin call to its
/// end; the guard stays on that thread.
pub(super) fn enter() -> Result<nuclei::runtime::EnterGuard<'static>> {
    static RUNTIME: OnceLock<Result<nuclei::Runtime, String>> = OnceLock::new();
    let runtime = RUNTIME
        .get_or_init(|| {
            nuclei::Runtime::builder()
                .worker_threads(0)
                .thread_name("gents-plugin-wasi")
                .build()
                .map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("starting the plugin WASI runtime")?;
    Ok(runtime.enter())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_task_handed_to_the_plugin_runtime_runs_on_the_calling_thread() {
        let _guard = super::enter().expect("the plugin WASI runtime starts");
        assert_eq!(nuclei::RuntimeHandle::current().workers(), 0);
        let caller = std::thread::current().id();
        let task = nuclei::spawn(async { std::thread::current().id() });
        assert_eq!(nuclei::block_on(task), caller);
    }
}
