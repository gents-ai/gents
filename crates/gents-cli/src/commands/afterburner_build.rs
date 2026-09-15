//! One canonical way to compile an Afterburner package source directory
//! into a `.afb`, shared by `gents pack build` (compiling a pack's
//! declared plugins, `commands::pack::build`) and `gents plugin build`
//! (compiling one standalone, `commands::plugin::build`). Two call sites
//! that must agree on which languages are safe to ship call this one
//! function rather than each deciding for itself.
//!
//! ## The bounded-run gate
//!
//! Compiling is not the whole story: a plugin has to be runnable under
//! every bound it declares, or "it compiles" is a false safety claim.
//! [`gents::plugin::PluginRunner`] hands a plugin's `.afb` to
//! `afterburner::afb_run::run_afb_bytes`, the one place in this workspace
//! that actually runs a plugin (`commands::plugin::run` uses the exact
//! same runner), and that function cannot enforce every bound for every
//! language (see its own module doc's "Known gaps"). Rather than keep a
//! second, hand-maintained list of which languages are safe here, this
//! module validates against the real compiler
//! ([`SourceLang::from_str`]) and then against
//! [`gents::pack::SUPPORTED_PLUGIN_LANGUAGES`] - the exact list
//! `gents::pack::PackPlugin::validate` already enforces for a
//! pack-declared plugin - so a language this CLI will build and a language
//! a pack may declare can never drift apart.
//!
//! Neither list decides whether a *built* artifact is actually runnable
//! under its bounds. That question is answered against the artifact
//! itself, by `PluginRunner::compile_within`, which asks
//! `afterburner::afb_run::bounds_for` rather than guessing from a language
//! name. It has to be that way round: one language compiles to more than
//! one shape (Ruby to an ordinary WASI command, Python to an
//! emscripten-pyodide bundle), and the shape, not the name, is what
//! decides what can be enforced.

use std::path::Path;
use std::str::FromStr;

use afterburner::cli::compile::dispatch_compile;
use afterburner::cli::compile::lang::SourceLang;
use afterburner_cloud::pkg;
use anyhow::{Context, Result};

/// What is missing or malformed at `path` for a source directory that was
/// supposed to be an Afterburner package.
pub(crate) fn describe_source(path: &Path) -> String {
    if !path.exists() {
        "which is missing".to_owned()
    } else if path.is_file() {
        "which is a file, not a directory".to_owned()
    } else {
        "a directory with no afb.toml".to_owned()
    }
}

/// Parses `declared` against Afterburner's own [`SourceLang::from_str`] -
/// the real, single source of truth for what `dispatch_compile` accepts,
/// rather than a hand-copied list that can drift from it - then refuses a
/// language [`gents::pack::SUPPORTED_PLUGIN_LANGUAGES`] does not admit
/// (see this module's own doc for why that is the right second gate).
fn validate_source_language(owner: &str, declared: &str) -> Result<SourceLang> {
    let lang = SourceLang::from_str(declared).with_context(|| {
        format!(
            "{owner} declares language {declared:?}, which Afterburner does not know how to \
             compile"
        )
    })?;
    let normalized = declared.trim().to_ascii_lowercase();
    anyhow::ensure!(
        gents::pack::SUPPORTED_PLUGIN_LANGUAGES.contains(&normalized.as_str()),
        "{owner} declares language {declared:?}; Afterburner can compile it, but gents plugin \
         run cannot run it under its declared bounds yet, so it is refused rather than built \
         unusable; supported languages: {}",
        gents::pack::SUPPORTED_PLUGIN_LANGUAGES.join(", ")
    );
    Ok(lang)
}

/// Loads the Afterburner package at `dir` and gates its declared language,
/// without compiling yet. Split from [`compile`] so a caller that needs
/// the loaded manifest (a default output filename, a report) has it before
/// paying for the compile.
pub(crate) fn load_and_validate(owner: &str, dir: &Path) -> Result<pkg::LocalPackage> {
    anyhow::ensure!(
        dir.join("afb.toml").is_file(),
        "{owner} declares source {dir:?}, {}; its source must be its own Afterburner package \
         (a directory with its own afb.toml)",
        describe_source(dir),
    );
    let local = pkg::LocalPackage::load(dir).with_context(|| {
        format!(
            "loading the Afterburner package for {owner} at {}",
            dir.display()
        )
    })?;
    validate_source_language(owner, &local.manifest.package.language)?;
    Ok(local)
}

/// Compiles an already-loaded, already-gated package to `out_path`.
pub(crate) fn compile(
    owner: &str,
    dir: &Path,
    local: pkg::LocalPackage,
    out_path: &Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    dispatch_compile(dir, local, out_path, false)
        .with_context(|| format!("compiling {owner} from {}", dir.display()))
}

/// Serializes every `dispatch_compile` call in this crate's tests. Each
/// shells out to an external toolchain (cargo, go, clang, javy, wasi-vfs);
/// running two at once was observed to race on those toolchains' own
/// shared scratch files (a transient `No such file or directory`, not
/// reproducible when the same test runs alone) - a test-concurrency hazard
/// in the external tooling, not in this module's own logic. Mirrors
/// `pack::registry::tests::env_lock`'s identical shape for the identical
/// reason: a shared external resource these tests must not contend over in
/// parallel.
///
/// Module-level rather than inside `tests`, and `pub(crate)`, because
/// `commands::plugin`'s fixtures compile through the same toolchains: two
/// locks would not serialize anything.
#[cfg(test)]
pub(crate) fn compile_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use afterburner_cloud::Afb;

    fn write(path: &Path, contents: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// Writes a minimal, otherwise-valid Afterburner package declaring
    /// `language` with `entry_rel`/`entry_contents` as its one source
    /// file, ready for [`load_and_validate`] + [`compile`].
    fn write_package(root: &Path, language: &str, entry_rel: &str, entry_contents: &[u8]) {
        write(
            &root.join("afb.toml"),
            format!(
                "[format]\nversion = \"1.0\"\n\n\
                 [package]\nname = \"sample\"\nnamespace = \"gents\"\nversion = \"0.1.0\"\n\
                 language = \"{language}\"\nentry = \"{entry_rel}\"\n\n\
                 [runtime]\nmin = \"0.1.0\"\n"
            )
            .as_bytes(),
        );
        write(
            &root.join("manifold.json"),
            br#"{"fs":"None","net":"None","env":"None","crypto":false,"child_process":false}"#,
        );
        write(&root.join(entry_rel), entry_contents);
    }

    /// Compiles `root` (already written by [`write_package`]) and asserts
    /// the produced `.afb` is genuine: readable, declares `language`, and
    /// carries a `wasm32-wasip1` precompiled WASI command module.
    fn assert_compiles_to_wasm32_wasip1(root: &Path, language: &str) {
        let owner = format!("test plugin ({language})");
        let out = root.parent().unwrap().join(format!("{language}.afb"));
        let local = load_and_validate(&owner, root).unwrap_or_else(|error| {
            panic!("loading the {language} package must succeed: {error:#}")
        });
        let _guard = compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        compile(&owner, root, local, &out)
            .unwrap_or_else(|error| panic!("compiling {language} must succeed: {error:#}"));

        let bytes = std::fs::read(&out).unwrap();
        let afb = Afb::from_bytes(&bytes)
            .unwrap_or_else(|error| panic!("{language}'s output must be a readable .afb: {error}"));
        assert_eq!(afb.manifest.package.language, language);
        assert_eq!(
            afb.manifest.runtime.target.as_deref(),
            Some("wasm32-wasip1"),
            "{language} must compile to a wasm32-wasip1 WASI command module"
        );
        assert!(
            afb.precompiled
                .contains_key("precompiled/wasm32-wasip1/main.wasm"),
            "{language}'s .afb must carry the precompiled WASI command module"
        );
    }

    /// `rustc` with the `wasm32-wasip1` target: verified present on this
    /// machine before writing this test (`rustc --version`, `rustup target
    /// list --installed`).
    #[test]
    fn rust_compiles_to_a_genuine_wasm32_wasip1_afb() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(
            &root,
            "rust",
            "source/main.rs",
            b"fn main() { println!(\"{{}}\"); }",
        );
        // Rust is the one language whose compile path shells out to `cargo
        // build` directly (`lang::compile_rust`), so it is also the one
        // that needs its own Cargo.toml on disk; the others need only the
        // afb.toml + source `write_package` already writes.
        write(
            &root.join("Cargo.toml"),
            b"[workspace]\n\n\
              [package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
              [[bin]]\nname = \"sample\"\npath = \"source/main.rs\"\n",
        );
        assert_compiles_to_wasm32_wasip1(&root, "rust");
    }

    /// `go` 1.27.1: verified present on this machine (`go version`).
    #[test]
    fn go_compiles_to_a_genuine_wasm32_wasip1_afb() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(
            &root,
            "go",
            "source/main.go",
            b"package main\n\nfunc main() { println(\"ok\") }\n",
        );
        assert_compiles_to_wasm32_wasip1(&root, "go");
    }

    /// `clang` 22.1.8 with a cached wasi-sdk under `~/.burn`: verified
    /// present on this machine (`clang --version`,
    /// `find ~/.burn -iname '*wasi-sdk*'`).
    #[test]
    fn c_compiles_to_a_genuine_wasm32_wasip1_afb() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(
            &root,
            "c",
            "source/main.c",
            b"int main(void) { return 0; }\n",
        );
        assert_compiles_to_wasm32_wasip1(&root, "c");
    }

    /// `clang++` 22.1.8 with the same cached wasi-sdk: verified present
    /// alongside `clang` above.
    #[test]
    fn cpp_compiles_to_a_genuine_wasm32_wasip1_afb() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(
            &root,
            "cpp",
            "source/main.cpp",
            b"int main() { return 0; }\n",
        );
        assert_compiles_to_wasm32_wasip1(&root, "cpp");
    }

    /// `javy` 8.1.1: verified present on this machine (`javy --version`).
    #[test]
    fn js_compiles_to_a_genuine_wasm32_wasip1_afb() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(
            &root,
            "js",
            "source/main.js",
            b"export function main() {}\n",
        );
        assert_compiles_to_wasm32_wasip1(&root, "js");
    }

    /// Same Javy path as JS, transpiled first: `javy` present as above.
    #[test]
    fn ts_compiles_to_a_genuine_wasm32_wasip1_afb() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(
            &root,
            "ts",
            "source/main.ts",
            b"export function main(): void {}\n",
        );
        assert_compiles_to_wasm32_wasip1(&root, "ts");
    }

    /// `wasi-vfs` cached under `~/.burn`: verified present on this machine
    /// (`find ~/.burn -iname '*wasi-vfs*'`). Compiled Ruby lands on the
    /// same `wasm32-wasip1` WASI-command shape as the other compiled
    /// languages, so `gents::pack::SUPPORTED_PLUGIN_LANGUAGES` admits it
    /// and this CLI builds it.
    #[test]
    fn ruby_compiles_to_a_genuine_wasm32_wasip1_afb() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(&root, "ruby", "source/main.rb", b"puts 'ok'\n");
        assert_compiles_to_wasm32_wasip1(&root, "ruby");
    }

    /// Python is the one language that does not compile to a WASI command:
    /// `burn compile` emits a self-contained `emscripten-pyodide` bundle.
    /// The test that matters is therefore not "does it compile" but "is the
    /// thing it compiled admissible as a plugin", so it runs the real
    /// artifact past the real admission gate
    /// ([`gents::plugin::PluginRunner`]), which is what decides whether a
    /// call's bounds would actually be enforced.
    #[test]
    fn python_compiles_to_a_pyodide_bundle_a_plugin_runner_admits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(&root, "python", "source/main.py", b"print('ok')\n");

        let owner = "test plugin (python)";
        let out = root.parent().unwrap().join("python.afb");
        let local = load_and_validate(owner, &root)
            .unwrap_or_else(|error| panic!("loading the python package must succeed: {error:#}"));
        let _guard = compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        compile(owner, &root, local, &out)
            .unwrap_or_else(|error| panic!("compiling python must succeed: {error:#}"));

        let bytes = std::fs::read(&out).unwrap();
        let afb = Afb::from_bytes(&bytes).expect("python's output must be a readable .afb");
        assert_eq!(afb.manifest.package.language, "python");
        assert_eq!(
            afb.manifest.runtime.target.as_deref(),
            Some("emscripten-pyodide"),
            "python compiles to a self-contained pyodide bundle, not a WASI command"
        );

        let plugin = gents::pack::PackPlugin {
            name: "py_plugin".to_owned(),
            description: "a python plugin".to_owned(),
            artifact: "plugins/py_plugin.afb".to_owned(),
            source: None,
            language: "python".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
            manifold: None,
        };
        gents::plugin::PluginRunner::compile(&bytes, &plugin).unwrap_or_else(|error| {
            panic!("a compiled python bundle runs under every bound a call applies: {error:#}")
        });
    }

    #[test]
    fn an_unknown_language_is_refused_against_the_real_supported_list() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(
            &root,
            "haskell",
            "source/main.hs",
            b"main = putStrLn \"ok\"\n",
        );
        let error = load_and_validate("test plugin (haskell)", &root)
            .map(drop)
            .expect_err("haskell must be refused");
        let message = format!("{error:#}");
        assert!(message.contains("haskell"), "{message}");
        assert!(
            message.contains("does not know how to compile"),
            "{message}"
        );
    }
}
