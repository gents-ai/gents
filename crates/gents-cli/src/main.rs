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

fn main() -> anyhow::Result<()> {
    let result = gents_server::run_cli();
    if let Err(error) = &result {
        tracing::error!(error = %format!("{error:#}"), "gents exited with an error");
        let message = format!("Error: {error:#}");
        if let Some(store) = gents_store::local::storage_backend::incompatible_store(
            error.as_ref(),
            std::path::Path::new(""),
        ) {
            exit_with_error(
                &message,
                gents_service::native_service::incompatible_store_exit_code(store.kind),
            );
        }
        if gents_store::local::store_key::store_key_unavailable(error.as_ref()).is_some() {
            exit_with_error(
                &message,
                gents_service::native_service::STORE_KEY_UNAVAILABLE_EXIT_CODE,
            );
        }
        // The exit-error sink. Rust would print `Error: {error:?}` on stderr on
        // the way out; print it here instead, so the service runtime's copy is
        // stamped like every other line it writes.
        if gents::runtime::native_logging::service_runtime_installed() {
            exit_with_error(&message, 1);
        }
    }
    result
}

/// Print one fatal line to stderr and exit with `code`. The service runtime's
/// stderr is `runtime-errors.log`, so the line is stamped there; a terminal
/// command keeps the plain wording it always had.
fn exit_with_error(message: &str, code: i32) -> ! {
    gents::runtime::native_logging::service_stderr(message);
    std::process::exit(code);
}
