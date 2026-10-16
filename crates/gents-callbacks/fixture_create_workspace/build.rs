use std::env;
use std::path::PathBuf;
use std::process::Command;

const WASI_TARGET: &str = "wasm32-wasip1";

fn main() {
    println!("cargo:rustc-check-cfg=cfg(callback_fixture_skipped)");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=../../../Cargo.lock");
    println!("cargo:rerun-if-env-changed=GENTS_SKIP_CALLBACK_WASM_BUILD");
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        return;
    }
    if env::var_os("GENTS_SKIP_CALLBACK_WASM_BUILD").is_some() {
        println!("cargo:rustc-cfg=callback_fixture_skipped");
        println!(
            "cargo:warning=GENTS_SKIP_CALLBACK_WASM_BUILD set; callback planner e2e tests skip"
        );
        return;
    }

    let package = env::var("CARGO_PKG_NAME").expect("CARGO_PKG_NAME");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let target_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("wasi-target");
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .args(["build", "--bin", &package, "--target", WASI_TARGET])
        .args(["--config", "profile.dev.panic=\"abort\"", "--target-dir"])
        .arg(&target_dir)
        .current_dir(&manifest_dir)
        .status()
        .expect("spawn cargo for the callback fixture WASI build");
    assert!(
        status.success(),
        "callback fixture WASI build failed with {status}; install {WASI_TARGET} \
         (`rustup target add {WASI_TARGET}`) or set GENTS_SKIP_CALLBACK_WASM_BUILD=1"
    );
    let artifact = target_dir
        .join(WASI_TARGET)
        .join("debug")
        .join(format!("{package}.wasm"));
    assert!(
        artifact.is_file(),
        "expected callback fixture at {}",
        artifact.display()
    );
    println!(
        "cargo:rustc-env=GENTS_CALLBACK_FIXTURE_WASI_COMMAND={}",
        artifact.display()
    );
}
