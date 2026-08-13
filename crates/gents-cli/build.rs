use std::env;
use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=GENTS_BUILD_GIT_SHA");
    println!("cargo:rerun-if-env-changed=GENTS_BUILD_GIT_REF");
    println!("cargo:rerun-if-env-changed=GENTS_BUILD_GIT_TAG");
    println!("cargo:rerun-if-env-changed=GENTS_BUILD_GIT_DIRTY");

    // Release workflows provide immutable metadata explicitly. In that mode,
    // do not watch Git's administrative files: actions/checkout rewrites them
    // on every retry, which used to invalidate and relink gents-cli even when
    // the source commit and trusted Cargo target were unchanged.
    let explicit_git_metadata = nonempty_env("GENTS_BUILD_GIT_SHA").is_some();
    if !explicit_git_metadata {
        if let Some(head_path) = git_path("HEAD") {
            println!("cargo:rerun-if-changed={head_path}");
        } else if let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") {
            let git_marker = Path::new(&manifest_dir).join("../../.git");
            if git_marker.exists() {
                println!("cargo:rerun-if-changed={}", git_marker.display());
            }
        }
        if let Some(ref_name) = git_output(&["symbolic-ref", "-q", "HEAD"]) {
            if let Some(ref_path) = git_path(&ref_name) {
                println!("cargo:rerun-if-changed={ref_path}");
            }
        }
    }

    if let Some(value) = git_metadata(
        "GENTS_BUILD_GIT_SHA",
        explicit_git_metadata,
        &["rev-parse", "HEAD"],
    ) {
        println!("cargo:rustc-env=GENTS_BUILD_GIT_SHA={value}");
    }

    if let Some(value) = git_metadata(
        "GENTS_BUILD_GIT_REF",
        explicit_git_metadata,
        &["rev-parse", "--abbrev-ref", "HEAD"],
    ) {
        println!("cargo:rustc-env=GENTS_BUILD_GIT_REF={value}");
    }

    if let Some(value) = git_metadata(
        "GENTS_BUILD_GIT_TAG",
        explicit_git_metadata,
        &["describe", "--tags", "--exact-match", "HEAD"],
    ) {
        println!("cargo:rustc-env=GENTS_BUILD_GIT_TAG={value}");
    }

    if let Some(value) = nonempty_env("GENTS_BUILD_GIT_DIRTY") {
        println!("cargo:rustc-env=GENTS_BUILD_GIT_DIRTY={value}");
    } else if !explicit_git_metadata {
        if let Some(value) = git_output(&["status", "--porcelain", "--untracked-files=no"]) {
            println!(
                "cargo:rustc-env=GENTS_BUILD_GIT_DIRTY={}",
                !value.is_empty()
            );
        }
    }

    if let Ok(target) = env::var("TARGET") {
        println!("cargo:rustc-env=GENTS_BUILD_TARGET={target}");
    }
    if let Ok(profile) = env::var("PROFILE") {
        println!("cargo:rustc-env=GENTS_BUILD_PROFILE={profile}");
    }
    if let Some(rustc) = command_output(Command::new("rustc").arg("--version")) {
        println!("cargo:rustc-env=GENTS_BUILD_RUSTC={rustc}");
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

fn git_metadata(name: &str, explicit: bool, fallback_args: &[&str]) -> Option<String> {
    nonempty_env(name).or_else(|| (!explicit).then(|| git_output(fallback_args)).flatten())
}

fn git_output(args: &[&str]) -> Option<String> {
    command_output(Command::new("git").args(args))
}

fn command_output(command: &mut Command) -> Option<String> {
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_string();
    Some(value)
}

fn git_path(path: &str) -> Option<String> {
    git_output(&["rev-parse", "--git-path", path]).filter(|value| !value.is_empty())
}
