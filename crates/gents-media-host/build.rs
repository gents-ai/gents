use std::io::Write;

fn main() -> std::io::Result<()> {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return Ok(());
    }
    let root =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo output directory"));
    let manifest = root.join("Info.plist");
    std::fs::write(
        &manifest,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.source-inc.gents.media-host</string>
<key>CFBundleName</key><string>Gents Voice</string>
<key>CFBundleDisplayName</key><string>Gents Voice</string>
<key>CFBundleExecutable</key><string>gents-media-host</string>
<key>CFBundleVersion</key><string>{}</string>
<key>NSMicrophoneUsageDescription</key><string>Gents records speech when you start dictation or a voice conversation.</string>
</dict></plist>
"#,
            std::env::var("CARGO_PKG_VERSION").expect("Cargo package version")
        ),
    )?;
    let mut output = std::io::stdout().lock();
    for argument in [
        "-Xlinker",
        "-sectcreate",
        "-Xlinker",
        "__TEXT",
        "-Xlinker",
        "__info_plist",
        "-Xlinker",
    ] {
        writeln!(
            output,
            "cargo:rustc-link-arg-bin=gents-media-host={argument}"
        )?;
    }
    writeln!(
        output,
        "cargo:rustc-link-arg-bin=gents-media-host={}",
        manifest.display()
    )?;
    writeln!(output, "cargo:rerun-if-changed=build.rs")?;
    Ok(())
}
