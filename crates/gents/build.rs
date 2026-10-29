use std::io::Write;

fn main() -> std::io::Result<()> {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        writeln!(
            std::io::stdout(),
            "cargo:rerun-if-changed=src/managed_exec/ownership/darwin_process.c"
        )?;
        cc::Build::new()
            .file("src/managed_exec/ownership/darwin_process.c")
            .compile("gents_darwin_process");
    }
    Ok(())
}
