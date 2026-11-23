/// mimalloc: glibc arenas retained DefraDB query churn at ~2.5x live heap and memcg-OOM-killed the runtime (#2034).
#[cfg(all(
    not(any(target_os = "android", target_os = "ios")),
    not(all(
        feature = "heap-profiling",
        any(target_os = "linux", target_os = "macos")
    ))
))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Sampling is enabled at process start with `_RJEM_MALLOC_CONF`; the normal
/// binary keeps mimalloc, including when a profiling build is not requested.
#[cfg(all(
    feature = "heap-profiling",
    any(target_os = "linux", target_os = "macos")
))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    let gents_server::CliExit { stderr, result } = gents_server::run_cli();
    if let Err(error) = &result {
        tracing::error!(error = %format!("{error:#}"), "gents exited with an error");
        if let Some(store) = gents_store::local::storage_backend::incompatible_store(
            error.as_ref(),
            std::path::Path::new(""),
        ) {
            stderr.write(&format!("Error: {error:#}"));
            std::process::exit(gents_service::native_service::incompatible_store_exit_code(
                store.kind,
            ));
        }
        if gents_store::local::store_key::store_key_unavailable(error.as_ref()).is_some() {
            stderr.write(&format!("Error: {error:#}"));
            std::process::exit(gents_service::native_service::STORE_KEY_UNAVAILABLE_EXIT_CODE);
        }
    }
    if let Err(error) = result {
        // The layout and exit status of returning the error from `main`, with
        // a passed-through message printed once.
        stderr.write(&format!(
            "Error: {}",
            gents_protocol::error_chain::ErrorReport(error.as_ref())
        ));
        let backtrace = error.backtrace();
        if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            eprintln!("\nStack backtrace:\n{backtrace}");
        }
        std::process::exit(1);
    }
}
