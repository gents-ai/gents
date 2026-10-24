use std::path::{Path, PathBuf};
use std::str::FromStr;

use afterburner::cli::compile::dispatch_compile;
use afterburner::cli::compile::lang::SourceLang;
use afterburner_cloud::pkg;
use anyhow::{Context, Result};

#[derive(Debug, thiserror::Error)]
enum AfterburnerBuildError {
    #[error("flushing standard output")]
    Flush(#[source] std::io::Error),
    #[cfg(unix)]
    #[error("could not save standard output")]
    Save,
    #[error("could not send compiler output to standard error")]
    Redirect,
    #[error("creating {}", path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "{owner} declares language {declared:?}; Afterburner can compile it, but gents plugin run cannot run it under its declared bounds yet, so it is refused rather than built unusable; supported languages: {supported}"
    )]
    UnsupportedLanguage {
        owner: String,
        declared: String,
        supported: String,
    },
    #[error(
        "{owner} declares source {dir:?}, {state}; its source must be its own Afterburner package (a directory with its own afb.toml)"
    )]
    NotAPackage {
        owner: String,
        dir: PathBuf,
        state: String,
    },
}

pub(crate) fn describe_source(path: &Path) -> String {
    if !path.exists() {
        "which is missing".to_owned()
    } else if path.is_file() {
        "which is a file, not a directory".to_owned()
    } else {
        "a directory with no afb.toml".to_owned()
    }
}

fn validate_source_language(owner: &str, declared: &str) -> Result<SourceLang> {
    let lang = SourceLang::from_str(declared).with_context(|| {
        format!(
            "{owner} declares language {declared:?}, which Afterburner does not know how to \
             compile"
        )
    })?;
    let normalized = declared.trim().to_ascii_lowercase();
    if !gents::pack::SUPPORTED_PLUGIN_LANGUAGES.contains(&normalized.as_str()) {
        return Err(AfterburnerBuildError::UnsupportedLanguage {
            owner: owner.to_owned(),
            declared: declared.to_owned(),
            supported: gents::pack::SUPPORTED_PLUGIN_LANGUAGES.join(", "),
        }
        .into());
    }
    Ok(lang)
}

pub(crate) fn load_and_validate(owner: &str, dir: &Path) -> Result<pkg::LocalPackage> {
    if !dir.join("afb.toml").is_file() {
        return Err(AfterburnerBuildError::NotAPackage {
            owner: owner.to_owned(),
            dir: dir.to_path_buf(),
            state: describe_source(dir),
        }
        .into());
    }
    let local = pkg::LocalPackage::load(dir).with_context(|| {
        format!(
            "loading the Afterburner package for {owner} at {}",
            dir.display()
        )
    })?;
    validate_source_language(owner, &local.manifest.package.language)?;
    Ok(local)
}

pub(crate) fn compile(
    owner: &str,
    dir: &Path,
    local: pkg::LocalPackage,
    out_path: &Path,
) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| AfterburnerBuildError::CreateDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let _progress_to_stderr = StdoutToStderr::start()?;
    dispatch_compile(dir, local, out_path, false)
        .with_context(|| format!("compiling {owner} from {}", dir.display()))
}

/// Redirects compiler progress so command stdout remains available for JSON output.
struct StdoutToStderr {
    #[cfg(unix)]
    saved: libc::c_int,
    #[cfg(windows)]
    saved: windows_sys::Win32::Foundation::HANDLE,
}

impl StdoutToStderr {
    fn start() -> Result<Self, AfterburnerBuildError> {
        use std::io::Write;
        std::io::stdout()
            .flush()
            .map_err(AfterburnerBuildError::Flush)?;
        #[cfg(unix)]
        {
            // SAFETY: dup/dup2 on the process's own standard descriptors; each
            // result is checked before it is used.
            let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
            if saved < 0 {
                return Err(AfterburnerBuildError::Save);
            }
            if unsafe { libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) } < 0 {
                unsafe { libc::close(saved) };
                return Err(AfterburnerBuildError::Redirect);
            }
            Ok(Self { saved })
        }
        #[cfg(windows)]
        {
            use windows_sys::Win32::System::Console::{
                GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
            };
            // SAFETY: reads and swaps the process's standard handles; restored on drop.
            unsafe {
                let saved = GetStdHandle(STD_OUTPUT_HANDLE);
                if SetStdHandle(STD_OUTPUT_HANDLE, GetStdHandle(STD_ERROR_HANDLE)) == 0 {
                    return Err(AfterburnerBuildError::Redirect);
                }
                Ok(Self { saved })
            }
        }
    }
}

impl Drop for StdoutToStderr {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = std::io::stdout().flush();
        #[cfg(unix)]
        // SAFETY: restores the descriptor saved in `start`, then releases the copy.
        unsafe {
            if libc::dup2(self.saved, libc::STDOUT_FILENO) < 0 {
                tracing::error!("could not restore standard output after compiling");
            }
            libc::close(self.saved);
        }
        #[cfg(windows)]
        // SAFETY: restores the handle saved in `start`.
        unsafe {
            if windows_sys::Win32::System::Console::SetStdHandle(
                windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE,
                self.saved,
            ) == 0
            {
                tracing::error!("could not restore standard output after compiling");
            }
        }
    }
}

#[cfg(test)]
/// Serializes external compiler calls because their toolchains share scratch files.
pub(crate) fn compile_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use afterburner_cloud::Afb;
    use gents_protocol::error_chain::ErrorChain;

    fn write(path: &Path, contents: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

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

    fn assert_compiles_to_wasm32_wasip1(root: &Path, language: &str) -> Vec<u8> {
        let owner = format!("test plugin ({language})");
        let out = root.parent().unwrap().join(format!("{language}.afb"));
        let local = load_and_validate(&owner, root).unwrap_or_else(|error| {
            panic!(
                "loading the {language} package must succeed: {}",
                ErrorChain(error.as_ref())
            )
        });
        let _guard = compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        compile(&owner, root, local, &out).unwrap_or_else(|error| {
            panic!(
                "compiling {language} must succeed: {}",
                ErrorChain(error.as_ref())
            )
        });

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
        bytes
    }

    fn write_rust_package(root: &Path) {
        write_package(
            root,
            "rust",
            "source/main.rs",
            b"fn main() { println!(\"{{}}\"); }",
        );
        write(
            &root.join("Cargo.toml"),
            b"[workspace]\n\n\
              [package]\nname = \"sample\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
              [[bin]]\nname = \"sample\"\npath = \"source/main.rs\"\n",
        );
    }

    #[test]
    fn rust_compiles_to_a_genuine_wasm32_wasip1_afb() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_rust_package(&root);
        assert_compiles_to_wasm32_wasip1(&root, "rust");
    }

    #[test]
    fn rust_compiles_with_configured_target_directories() {
        const PACKAGE_ENV: &str = "GENTS_TEST_RUST_PLUGIN_PACKAGE";
        let inherited_target = std::env::var_os("CARGO_TARGET_DIR");
        if let Some(root) = std::env::var_os(PACKAGE_ENV) {
            let root = Path::new(&root);
            let bytes = assert_compiles_to_wasm32_wasip1(root, "rust");
            assert!(root
                .join("target/wasm32-wasip1/release/sample.wasm")
                .is_file());
            assert_eq!(std::env::var_os("CARGO_TARGET_DIR"), inherited_target);

            let plugin: gents::pack::PackPlugin = serde_json::from_value(serde_json::json!({
                "name": "sample",
                "description": "a test plugin",
                "artifact": "plugins/rust.afb",
                "language": "rust",
                "input_schema": {"type": "object"},
            }))
            .unwrap();
            let result = gents::plugin::PluginRunner::compile(&bytes, &plugin)
                .unwrap()
                .call(
                    &serde_json::json!({}),
                    &gents::plugin::PluginBudget::default(),
                )
                .unwrap();
            assert_eq!(result.verdict, gents::plugin::PluginVerdict::Success);
            assert_eq!(result.output, serde_json::json!({}));
            return;
        }

        let test_name = std::thread::current().name().unwrap().to_owned();
        for (environment, config) in [(true, false), (false, true), (true, true)] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("pkg");
            write_rust_package(&root);
            if config {
                write(
                    &root.join(".cargo/config.toml"),
                    b"[build]\ntarget-dir = \"configured-target\"\n",
                );
            }
            let redirected = dir.path().join("environment-target");
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .args([test_name.as_str(), "--exact", "--nocapture"])
                .current_dir(dir.path())
                .env(PACKAGE_ENV, "pkg");
            if environment {
                child.env("CARGO_TARGET_DIR", &redirected);
            } else {
                child.env_remove("CARGO_TARGET_DIR");
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "Rust compilation failed with environment={environment}, config={config}:\n{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(dir.path().join("rust.afb").is_file());
            assert!(!redirected.exists());
            assert!(!root.join("configured-target").exists());
        }
        assert_eq!(std::env::var_os("CARGO_TARGET_DIR"), inherited_target);
    }

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

    #[test]
    fn ruby_compiles_to_a_genuine_wasm32_wasip1_afb() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(&root, "ruby", "source/main.rb", b"puts 'ok'\n");
        assert_compiles_to_wasm32_wasip1(&root, "ruby");
    }

    #[test]
    fn python_compiles_to_a_pyodide_bundle_a_plugin_runner_admits() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("pkg");
        write_package(&root, "python", "source/main.py", b"print('ok')\n");

        let owner = "test plugin (python)";
        let out = root.parent().unwrap().join("python.afb");
        let local = load_and_validate(owner, &root).unwrap_or_else(|error| {
            panic!(
                "loading the python package must succeed: {}",
                ErrorChain(error.as_ref())
            )
        });
        let _guard = compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        compile(owner, &root, local, &out).unwrap_or_else(|error| {
            panic!(
                "compiling python must succeed: {}",
                ErrorChain(error.as_ref())
            )
        });

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
            instructions: None,
            bind_dir: None,
            limits: None,
            model_slot: None,
            service: None,
            ..Default::default()
        };
        gents::plugin::PluginRunner::compile(&bytes, &plugin).unwrap_or_else(|error| {
            panic!(
                "a compiled python bundle runs under every bound a call applies: {}",
                ErrorChain(&error)
            )
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
        let message = format!("{}", ErrorChain(error.as_ref()));
        assert!(message.contains("haskell"), "{message}");
        assert!(
            message.contains("does not know how to compile"),
            "{message}"
        );
    }
}
