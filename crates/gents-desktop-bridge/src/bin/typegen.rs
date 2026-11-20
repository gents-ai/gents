fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/gents-desktop-client/src/generated")
        });
    gents_desktop_bridge::typegen::write_bindings(&directory).map_err(std::io::Error::other)?;
    Ok(())
}
