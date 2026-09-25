fn main() -> anyhow::Result<()> {
    let result = gents_server::run_cli();
    if let Err(error) = &result {
        tracing::error!(error = %format!("{error:#}"), "gents exited with an error");
        if let Some(store) =
            gents::storage_backend::incompatible_store(error, std::path::Path::new(""))
        {
            eprintln!("Error: {error:#}");
            std::process::exit(gents_server::native_service::incompatible_store_exit_code(
                store.kind,
            ));
        }
    }
    result
}
