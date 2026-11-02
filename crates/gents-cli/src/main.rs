/// mimalloc: glibc arenas retained DefraDB query churn at ~2.5x live heap and memcg-OOM-killed the runtime (#2034).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    let result = gents_server::run_cli();
    if let Err(error) = &result {
        tracing::error!(error = %format!("{error:#}"), "gents exited with an error");
        if let Some(store) = gents::store::local::storage_backend::incompatible_store(
            error.as_ref(),
            std::path::Path::new(""),
        ) {
            eprintln!("Error: {error:#}");
            std::process::exit(gents_server::native_service::incompatible_store_exit_code(
                store.kind,
            ));
        }
    }
    result
}
