use std::process::ExitCode;

use gents_protocol::error_chain::ErrorChain;
/// mimalloc: glibc arenas retained DefraDB query churn at ~2.5x live heap and memcg-OOM-killed the runtime (#2034).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Prints a failed command's error with its source chain and exits non-zero.
fn main() -> ExitCode {
    let Err(error) = gents_server::run_cli() else {
        return ExitCode::SUCCESS;
    };
    tracing::error!(error = %ErrorChain(error.as_ref()), "gents exited with an error");
    if let Some(store) =
        gents::storage_backend::incompatible_store(error.as_ref(), std::path::Path::new(""))
    {
        eprintln!("Error: {}", ErrorChain(error.as_ref()));
        std::process::exit(gents_server::native_service::incompatible_store_exit_code(
            store.kind,
        ));
    }
    eprintln!("Error: {}", ErrorChain(error.as_ref()));
    ExitCode::FAILURE
}
