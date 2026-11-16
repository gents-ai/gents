//! Gives the WebAssembly build a 16 MiB stack: the SQL planner recurses once per level of
//! expression nesting, and the default 1 MiB is too small for a deep one.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rustc-link-arg-bins=-zstack-size=16777216");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
