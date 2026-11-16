//! Embeds the core pack archives `make core-packs` builds into
//! `target/core-packs/` (or `GENTS_CORE_PACKS_DIR`). A build without them
//! carries none, which init reports; `GENTS_REQUIRE_CORE_PACKS=1`, set by
//! release builds, refuses a build missing any of them.

use std::path::PathBuf;

const CORE_PACKS: [&str; 6] = [
    "pack_builder",
    "web",
    "dynamic_workflows",
    "charts",
    "data_tables",
    "script",
];

fn main() {
    println!("cargo:rerun-if-env-changed=GENTS_CORE_PACKS_DIR");
    println!("cargo:rerun-if-env-changed=GENTS_REQUIRE_CORE_PACKS");
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let dir = std::env::var_os("GENTS_CORE_PACKS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("../../target/core-packs"));
    println!("cargo:rerun-if-changed={}", dir.display());
    let required = std::env::var("GENTS_REQUIRE_CORE_PACKS").is_ok_and(|value| value == "1");
    let mut entries = Vec::new();
    let mut missing = Vec::new();
    for name in CORE_PACKS {
        let path = dir.join(format!("gents.{name}.pack"));
        if path.is_file() {
            println!("cargo:rerun-if-changed={}", path.display());
            let path = path.canonicalize().expect("canonical core pack path");
            entries.push(format!(
                "    (\"gents/{name}\", include_bytes!({:?})),\n",
                path.display().to_string()
            ));
        } else {
            missing.push(path.display().to_string());
        }
    }
    assert!(
        !required || missing.is_empty(),
        "GENTS_REQUIRE_CORE_PACKS=1 but these core packs are not built: {}; run `make core-packs`",
        missing.join(", ")
    );
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("out dir")).join("archives.rs");
    std::fs::write(
        out,
        format!(
            "pub static ARCHIVES: &[(&str, &[u8])] = &[\n{}];\n",
            entries.concat()
        ),
    )
    .expect("write archives.rs");
}
