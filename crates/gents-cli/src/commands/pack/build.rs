use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::args::PackBuildArgs;

pub(crate) use gents::pack::authoring::BuildReport;

pub(crate) const BUILDER_ID: &str = concat!("gents-cli/", env!("CARGO_PKG_VERSION"));

struct CliPluginCompiler;

impl gents::pack::authoring::PluginCompiler for CliPluginCompiler {
    fn builder_id(&self) -> &str {
        BUILDER_ID
    }

    fn load_and_validate(&self, owner: &str, dir: &Path) -> Result<()> {
        crate::commands::afterburner_build::load_and_validate(owner, dir).map(|_| ())
    }

    fn compile(&self, owner: &str, dir: &Path, out_path: &Path) -> Result<()> {
        let package = crate::commands::afterburner_build::load_and_validate(owner, dir)?;
        crate::commands::afterburner_build::compile(owner, dir, package, out_path)
    }
}

pub(crate) fn dispatch(args: PackBuildArgs) -> Result<()> {
    if args.all {
        let reports = build_all(Path::new("packs"))?;
        return Ok(crate::print_json(&json!({ "packs": reports }))?);
    }
    let dir = args.dir.unwrap_or_else(|| Path::new(".").to_path_buf());
    let report = build_pack(&dir, args.out.as_deref())?;
    Ok(crate::print_json(&serde_json::to_value(report)?)?)
}

pub(crate) fn build_all(root: &Path) -> Result<Vec<BuildReport>> {
    gents::pack::authoring::build_all(root, &CliPluginCompiler, |_| Ok(None))
}

pub(crate) fn build_pack(dir: &Path, out: Option<&Path>) -> Result<BuildReport> {
    gents::pack::authoring::build_pack(dir, &CliPluginCompiler, out)
}

pub(crate) fn build_plugin(
    dir: &Path,
    manifest: &gents::pack::PackManifest,
    plugin: &gents::pack::PackPlugin,
) -> Result<()> {
    gents::pack::authoring::build_plugin(dir, manifest, plugin, &CliPluginCompiler)
}

pub(crate) fn plugin_entry(language: &str) -> Option<&'static str> {
    gents::pack::authoring::plugin_entry(language)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents::pack_archive::PackArchive;
    use gents_protocol::error_chain::ErrorChain;

    fn write_plain_pack(root: &Path, name: &str) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("README.md"), b"# a plain pack").unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": name,
            "version": "1.0.0",
            "description": "A plain pack with no plugins",
            "authors": ["gents-ai contributors"],
            "tags": ["example"],
            "kind": "assets",
            "assets": ["README.md"],
            "dependencies": [],
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }
    fn write_plugin_pack(root: &Path, name: &str, plugin: serde_json::Value) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("README.md"), b"# a plugins pack").unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": name,
            "version": "0.1.0",
            "description": "A pack that is nothing but capabilities",
            "authors": ["gents-ai contributors"],
            "tags": ["plugins"],
            "kind": "plugins",
            "assets": ["README.md", plugin["artifact"]],
            "plugins": [plugin],
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_pack_with_no_plugins_builds_and_packs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plain_pack");
        write_plain_pack(&root, "plain_pack");

        let out = dir.path().join("out.pack");
        let report = build_pack(&root, Some(&out)).unwrap();

        assert_eq!(report.pack, "plain_pack");
        assert_eq!(report.version, "1.0.0");
        assert!(report.plugins.is_empty());
        assert_eq!(report.out, out);
        assert!(out.is_file());
        assert_eq!(report.size_bytes, std::fs::metadata(&out).unwrap().len());

        let packed = PackArchive::from_bytes(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(packed.digest(), report.digest);
        let staging_left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".building-")
            })
            .collect();
        assert!(
            staging_left.is_empty(),
            "the staging file is renamed, not left"
        );
    }

    #[test]
    fn default_out_path_lands_beside_the_pack_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("plain_pack");
        write_plain_pack(&root, "plain_pack");

        let report = build_pack(&root, None).unwrap();
        assert_eq!(report.out, dir.path().join("gents.plain_pack-1.0.0.pack"));
        assert!(report.out.is_file());
    }

    #[test]
    fn a_plugin_with_no_source_and_a_missing_artifact_names_both() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "artifact": "plugins/format_check.afb",
                "language": "rust",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );

        let error = build_pack(&root, None).unwrap_err();
        let message = ErrorChain(error.as_ref()).to_string();
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("plugins/format_check.afb"), "{message}");
        assert!(message.contains("missing"), "{message}");
    }

    #[test]
    fn a_plugin_source_that_is_a_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "artifact": "plugins/format_check.afb",
                "source": "plugin_src",
                "language": "rust",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        std::fs::write(root.join("plugin_src"), b"not a directory").unwrap();

        let error = build_pack(&root, None).unwrap_err();
        let message = ErrorChain(error.as_ref()).to_string();
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("is not a directory"), "{message}");
    }

    #[test]
    fn an_unchanged_plugin_is_not_recompiled() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("format_check");
        super::super::scaffold::scaffold(
            &dir,
            "format_check",
            &crate::cli::PackScaffoldArgs {
                kind: None,
                namespace: "acme".into(),
                template: Some(crate::cli::PackTemplate::PluginTool),
                language: None,
                ux: false,
            },
        )
        .unwrap();
        let _guard = crate::commands::afterburner_build::compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let artifact = dir.join("plugins/format_check.afb");
        build_pack(&dir, Some(&root.path().join("a.pack"))).unwrap();
        let first = std::fs::metadata(&artifact).unwrap().modified().unwrap();
        build_pack(&dir, Some(&root.path().join("b.pack"))).unwrap();
        assert_eq!(
            std::fs::metadata(&artifact).unwrap().modified().unwrap(),
            first,
            "unchanged source must not be recompiled"
        );

        let stamp = dir.join("target/plugins/format_check/target/gents-build.stamp");
        let recorded = std::fs::read_to_string(&stamp).unwrap();
        let other_builder = recorded.replacen(super::BUILDER_ID, "gents-cli/0.0.0-other", 1);
        assert_ne!(recorded, other_builder, "the stamp records the builder");
        std::fs::write(&stamp, other_builder).unwrap();
        build_pack(&dir, Some(&root.path().join("b2.pack"))).unwrap();
        let rebuilt = std::fs::metadata(&artifact).unwrap().modified().unwrap();
        assert_ne!(rebuilt, first, "another compiler's artifact is rebuilt");
        let first = rebuilt;

        let source = dir.join("plugins/format_check/source/main.rs");
        let edited = std::fs::read_to_string(&source).unwrap() + "\n// edited\n";
        std::fs::write(&source, edited).unwrap();
        build_pack(&dir, Some(&root.path().join("c.pack"))).unwrap();
        assert_ne!(
            std::fs::metadata(&artifact).unwrap().modified().unwrap(),
            first,
            "an edited source is rebuilt"
        );
    }

    #[test]
    fn a_plugin_source_without_its_entry_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "artifact": "plugins/format_check.afb",
                "source": "plugin_src",
                "language": "rust",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        std::fs::create_dir_all(root.join("plugin_src")).unwrap();
        std::fs::write(root.join("plugin_src/main.rs"), b"fn main() {}").unwrap();

        let error = build_pack(&root, None).unwrap_err();
        let message = ErrorChain(error.as_ref()).to_string();
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("has no source/main.rs"), "{message}");
    }

    #[test]
    fn a_plugin_with_an_unknown_language_is_refused_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting",
                "artifact": "plugins/format_check.afb",
                "language": "haskell",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        std::fs::create_dir_all(root.join("plugins")).unwrap();
        std::fs::write(root.join("plugins/format_check.afb"), b"stub").unwrap();

        let error = build_pack(&root, None).unwrap_err();
        let message = ErrorChain(error.as_ref()).to_string();
        assert!(message.contains("format_check"), "{message}");
        assert!(message.contains("haskell"), "{message}");
    }

    #[test]
    fn missing_pack_dir_names_a_clear_error() {
        let error = build_pack(Path::new("/no/such/pack"), None).unwrap_err();
        assert!(ErrorChain(error.as_ref()).to_string().contains("manifest"));
    }

    #[test]
    fn build_all_builds_every_pack_directory_under_root() {
        let dir = tempfile::tempdir().unwrap();
        write_plain_pack(&dir.path().join("alpha"), "alpha");
        write_plain_pack(&dir.path().join("beta"), "beta");
        std::fs::write(dir.path().join("catalog.json"), b"[]").unwrap();

        let reports = build_all(dir.path()).unwrap();
        let mut names: Vec<_> = reports.iter().map(|report| report.pack.clone()).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"]);
        for report in &reports {
            assert!(report.out.is_file());
        }
    }

    #[test]
    fn a_plugin_with_a_rust_source_is_compiled_through_afterburner_and_packed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("shipping_plugins");
        write_plugin_pack(
            &root,
            "shipping_plugins",
            json!({
                "name": "format_check",
                "description": "Checks formatting and says what is wrong",
                "artifact": "plugins/format_check.afb",
                "source": "plugin_src",
                "language": "rust",
                "input_schema": {"type": "object", "properties": {}},
            }),
        );
        let plugin_src = root.join("plugin_src");
        std::fs::create_dir_all(plugin_src.join("source")).unwrap();
        std::fs::write(
            plugin_src.join("Cargo.toml"),
            b"[workspace]\n\n\
              [package]\nname = \"format_check\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
              [[bin]]\nname = \"format_check\"\npath = \"source/main.rs\"\n",
        )
        .unwrap();
        std::fs::write(
            plugin_src.join("source/main.rs"),
            b"fn main() { println!(\"ok\"); }",
        )
        .unwrap();

        let report = build_pack(&root, None).unwrap();
        assert_eq!(report.plugins.len(), 1);
        assert_eq!(report.plugins[0].name, "format_check");
        assert_eq!(report.plugins[0].artifact, "plugins/format_check.afb");

        let bytes = std::fs::read(&report.out).unwrap();
        let packed = PackArchive::from_bytes(&bytes).unwrap();
        let artifact_bytes = packed.plugin_artifact("format_check").unwrap();
        let artifact_afb = afterburner_cloud::Afb::from_bytes(artifact_bytes)
            .expect("the compiled plugin artifact is a real .afb");
        assert_eq!(artifact_afb.manifest.package.language, "rust");
        assert_eq!(
            artifact_afb.manifest.runtime.target.as_deref(),
            Some("wasm32-wasip1"),
            "a Rust plugin compiles to a wasm32-wasip1 WASI command module"
        );
    }

    fn committed_job_fixture_path() -> std::path::PathBuf {
        crate::commands::pack::test_support::fixture_dir("job_plugin_fixture")
            .join("plugins/job_fixture.afb")
    }

    const JOB_FIXTURE_BUILD_ROOT: &str = "GENTS_JOB_FIXTURE_BUILD_ROOT";

    /// Rust embeds registry and sysroot paths in panic locations, even in release
    /// WASM. The committed fixture uses stable logical paths with the repository's
    /// pinned toolchain; compiler environment changes stay in its subprocess.
    fn job_fixture_rustflags(root: &Path) -> String {
        let manifest = root.join("plugins/job_fixture/Cargo.toml");
        let metadata = std::process::Command::new(env!("CARGO"))
            .args([
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--manifest-path",
            ])
            .arg(&manifest)
            .output()
            .expect("query job fixture dependencies");
        assert!(
            metadata.status.success(),
            "job fixture metadata: {}",
            String::from_utf8_lossy(&metadata.stderr)
        );
        let metadata: serde_json::Value =
            serde_json::from_slice(&metadata.stdout).expect("job fixture metadata is JSON");
        let mut remappings = vec![(root.to_path_buf(), "/fixture".to_owned())];
        let canonical = root.canonicalize().expect("canonical fixture root");
        if canonical != root {
            remappings.push((canonical, "/fixture".to_owned()));
        }
        for package in metadata["packages"].as_array().expect("fixture packages") {
            if package["source"].is_null() {
                continue;
            }
            let manifest = Path::new(package["manifest_path"].as_str().expect("package manifest"));
            remappings.push((
                manifest.parent().expect("package directory").to_path_buf(),
                format!(
                    "/cargo/{}-{}",
                    package["name"].as_str().expect("package name"),
                    package["version"].as_str().expect("package version")
                ),
            ));
        }
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let sysroot = std::process::Command::new(rustc)
            .args(["--print", "sysroot"])
            .output()
            .expect("query fixture Rust sysroot");
        assert!(
            sysroot.status.success(),
            "fixture Rust sysroot query failed"
        );
        let sysroot = String::from_utf8(sysroot.stdout).expect("Rust sysroot is UTF-8");
        let sysroot = std::path::PathBuf::from(sysroot.trim());
        let canonical = sysroot.canonicalize().expect("canonical Rust sysroot");
        if canonical != sysroot {
            remappings.push((canonical, "/rust".to_owned()));
        }
        remappings.push((sysroot, "/rust".to_owned()));
        remappings
            .into_iter()
            .map(|(from, to)| {
                format!(
                    "--remap-path-prefix={}={to}",
                    from.to_str().expect("fixture compiler paths are UTF-8")
                )
            })
            .collect::<Vec<_>>()
            .join("\u{1f}")
    }

    fn build_job_fixture() -> Vec<u8> {
        let _guard = crate::commands::afterburner_build::compile_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        crate::commands::pack::test_support::copy_tree(
            &crate::commands::pack::test_support::fixture_dir("job_plugin_fixture"),
            tmp.path(),
        )
        .unwrap();
        let cache = tmp.path().join("target");
        if cache.exists() {
            std::fs::remove_dir_all(cache).expect("discard copied fixture build cache");
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "commands::pack::build::tests::write_job_fixture_artifact",
                "--ignored",
            ])
            .env(JOB_FIXTURE_BUILD_ROOT, tmp.path())
            .env("CARGO_ENCODED_RUSTFLAGS", job_fixture_rustflags(tmp.path()))
            .env("CARGO_INCREMENTAL", "0")
            .env("CARGO_NET_OFFLINE", "true")
            .env("RUSTC_WRAPPER", "")
            .env("RUSTC_WORKSPACE_WRAPPER", "")
            .env_remove("RUSTFLAGS");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("CARGO_PROFILE_RELEASE_") {
                command.env_remove(key);
            }
        }
        let result = command.output().expect("build the canonical job fixture");
        assert!(
            result.status.success(),
            "job fixture build failed:\n{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        std::fs::read(tmp.path().join("plugins/job_fixture.afb")).unwrap()
    }

    #[test]
    fn committed_job_fixture_artifact_matches_its_source() {
        let committed = std::fs::read(committed_job_fixture_path()).unwrap_or_default();
        assert!(
            build_job_fixture() == committed,
            "job_fixture.afb drifted from plugins/job_fixture/source; regenerate with: cargo test -p gents-cli --lib commands::pack::build::tests::write_job_fixture_artifact -- --ignored"
        );
    }

    #[test]
    #[ignore = "run explicitly to regenerate crates/gents/tests/fixtures/packs/job_plugin_fixture/plugins/job_fixture.afb"]
    fn write_job_fixture_artifact() {
        if let Some(root) = std::env::var_os(JOB_FIXTURE_BUILD_ROOT) {
            let root = Path::new(&root);
            build_pack(root, Some(&root.join("out.pack"))).unwrap();
            return;
        }
        std::fs::write(committed_job_fixture_path(), build_job_fixture()).unwrap();
    }
}
