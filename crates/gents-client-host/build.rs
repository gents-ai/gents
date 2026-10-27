/// The initial Windows thread does not inherit `RUST_MIN_STACK` or Tokio's
/// worker reservations. Native CLI dispatch polls runtime futures on that thread.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=gents-client-host=/STACK:16777216");
    }
}
