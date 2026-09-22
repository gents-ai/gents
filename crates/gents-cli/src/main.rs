fn main() -> anyhow::Result<()> {
    let result = gents_server::run_cli();
    if let Err(error) = &result {
        tracing::error!(error = %format!("{error:#}"), "gents exited with an error");
    }
    result
}
