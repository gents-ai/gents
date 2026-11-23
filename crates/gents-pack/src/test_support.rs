/// The workspace's pack fixtures, found from the runtime CARGO_MANIFEST_DIR of
/// whichever workspace crate's test calls this: an archived test binary runs
/// against another checkout, where this crate's compile-time path is absent.
#[cfg(any(test, feature = "test-fixtures"))]
pub fn fixture_packs_dir() -> std::path::PathBuf {
    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("runtime CARGO_MANIFEST_DIR"),
    );
    manifest_dir
        .ancestors()
        .map(|ancestor| ancestor.join("crates/gents/tests/fixtures/packs"))
        .find(|candidate| candidate.is_dir())
        .unwrap_or_else(|| {
            panic!(
                "no crates/gents/tests/fixtures/packs above {}",
                manifest_dir.display()
            )
        })
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn fixture_pack_copy(
    name: &str,
    plugin_output: &serde_json::Value,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let source = fixture_packs_dir().join(name);
    let manifest: crate::pack::PackManifest = serde_json::from_slice(
        &std::fs::read(source.join("manifest.json"))
            .unwrap_or_else(|error| panic!("fixture {name:?} manifest: {error}")),
    )
    .unwrap_or_else(|error| panic!("fixture {name:?} manifest: {error}"));
    let artifacts: std::collections::BTreeSet<&str> = manifest
        .metadata
        .plugins
        .iter()
        .map(|plugin| plugin.artifact.as_str())
        .collect();
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(name);
    for path in crate::pack::declared_paths(&manifest) {
        let target = root.join(&path);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
        if artifacts.contains(path.as_str()) {
            let wat = crate::plugin::test_fixtures::constant_output_wat(
                &serde_json::to_vec(plugin_output).expect("encode plugin output"),
            );
            std::fs::write(
                &target,
                crate::plugin::test_fixtures::build_plugin_afb(&wat),
            )
            .expect("write artifact");
        } else {
            std::fs::copy(source.join(&path), &target)
                .unwrap_or_else(|error| panic!("fixture {name:?} file {path:?}: {error}"));
        }
    }
    (dir, root)
}

#[cfg(test)]
pub fn fixture_pack_names() -> Vec<String> {
    const KNOWN: [&str; 8] = [
        "assets_fixture",
        "documents_fixture",
        "slot_fixture",
        "dependent_fixture",
        "dependee_fixture",
        "bind_plugin_fixture",
        "job_plugin_fixture",
        "plugin_documents_fixture",
    ];
    let root = fixture_packs_dir();
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("reading {}: {error}", root.display()))
    {
        let path = entry
            .unwrap_or_else(|error| panic!("reading an entry of {}: {error}", root.display()))
            .path();
        if !path.is_dir() {
            continue;
        }
        let manifest = path.join("manifest.json");
        let bytes = std::fs::read(&manifest)
            .unwrap_or_else(|error| panic!("reading {}: {error}", manifest.display()));
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .unwrap_or_else(|error| panic!("parsing {}: {error}", manifest.display()));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_else(|| panic!("{} is not a UTF-8 name", path.display()));
        names.push(name.to_owned());
    }
    names.sort();
    for known in KNOWN {
        assert!(
            names.iter().any(|name| name == known),
            "fixture {known} is missing from {}",
            root.display()
        );
    }
    names
}

use crate::plugin::store::{self, InstalledPlugin};
use crate::plugin::test_fixtures::{build_plugin_pack, ECHO_WAT};

pub fn installed_echo() -> (tempfile::TempDir, InstalledPlugin) {
    installed_plugin(ECHO_WAT, None)
}

pub fn installed_plugin(
    wat: &str,
    access: Option<crate::pack::BindAccess>,
) -> (tempfile::TempDir, InstalledPlugin) {
    let (mut declaration, bytes) = build_plugin_pack("echo_pack", wat, None);
    declaration.bind_dir = access.map(|access| crate::pack::PluginDirBinding {
        input_field: "path".into(),
        original_field: None,
        description: "a directory".into(),
        access,
        write_fields: Vec::new(),
    });
    let home = tempfile::tempdir().unwrap();
    let hex = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&bytes));
    store::store_bytes(home.path(), &hex, &bytes).unwrap();
    let record = InstalledPlugin {
        namespace: "team".into(),
        name: declaration.name.clone(),
        version: "0.1.0".into(),
        digest: format!("sha256:{hex}"),
        language: "rust".into(),
        declaration,
        granted: None,
        instructions: None,
        owner_pack_coordinate: None,
        owner_pack_digest: None,
        model_binding: None,
        data_unique_keys: Default::default(),
        data_collection_fields: Default::default(),
        data_collection_schemas: Default::default(),
        gents_version: None,
    };
    store::write_record(home.path(), &record).unwrap();
    (home, record)
}

pub fn asking_plugin_wat(
    service: &str,
    results: &str,
    canned: &serde_json::Value,
    forever: bool,
) -> String {
    let escape = |text: &str| {
        text.bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let canned = canned.to_string();
    let calls = format!("\"{service}\":true");
    let stop = if forever {
        "(i32.const 1)".to_owned()
    } else {
        format!(
            "(i32.eqz (call $contains (i32.const 50000) (i32.const {}) (local.get $n)))",
            results.len()
        )
    };
    format!(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (data (i32.const 8192) "{canned_bytes}")
  (data (i32.const 50000) "{results_bytes}")
  (data (i32.const 50100) "{calls_bytes}")
  (func $contains (param $needle i32) (param $nlen i32) (param $hlen i32) (result i32)
    (local $i i32) (local $j i32)
    (block $notfound
      (loop $outer
        (br_if $notfound (i32.gt_u (i32.add (local.get $i) (local.get $nlen)) (local.get $hlen)))
        (local.set $j (i32.const 0))
        (block $mismatch
          (loop $inner
            (if (i32.eq (local.get $j) (local.get $nlen)) (then (return (i32.const 1))))
            (br_if $mismatch (i32.ne
              (i32.load8_u (i32.add (local.get $i) (local.get $j)))
              (i32.load8_u (i32.add (local.get $needle) (local.get $j)))))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $inner)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $outer)))
    (i32.const 0))
  (func (export "_start")
    (local $n i32)
    (i32.store (i32.const 60000) (i32.const 0))
    (i32.store (i32.const 60004) (i32.const 8000))
    (drop (call $fd_read (i32.const 0) (i32.const 60000) (i32.const 1) (i32.const 60008)))
    (local.set $n (i32.load (i32.const 60008)))
    (if (call $contains (i32.const 50100) (i32.const {calls_len}) (local.get $n))
      (then
        (if {stop}
          (then
            (i32.store (i32.const 60000) (i32.const 8192))
            (i32.store (i32.const 60004) (i32.const {canned_len}))
            (drop (call $fd_write (i32.const 1) (i32.const 60000) (i32.const 1) (i32.const 60012)))
            (return)))))
    (i32.store (i32.const 60000) (i32.const 0))
    (i32.store (i32.const 60004) (local.get $n))
    (drop (call $fd_write (i32.const 1) (i32.const 60000) (i32.const 1) (i32.const 60012)))))"#,
        canned_bytes = escape(&canned),
        results_bytes = escape(results),
        calls_bytes = escape(&calls),
        calls_len = calls.len(),
        canned_len = canned.len(),
    )
}

pub fn load_pack_system_prompt(root: &std::path::Path) -> anyhow::Result<String> {
    use crate::pack::{load_pack_config, PackInstallOptions, PackManifest};
    use anyhow::Context;

    let manifest: PackManifest =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json"))?)?;
    let config = load_pack_config(
        &manifest,
        &PackInstallOptions {
            node_did: "did:key:lsp-live-fixture".into(),
        },
        &|path| Ok(std::fs::read(root.join(path))?),
        &|name| std::env::var(name).ok(),
    )?;
    let agent = config
        .agents
        .iter()
        .find(|agent| agent.agent_did == "lsp-coder")
        .context("pack has no lsp-coder Agent")?;
    let context_id = agent
        .context_id
        .as_deref()
        .context("lsp-coder has no Context")?;
    config
        .contexts
        .iter()
        .find(|context| context.context_id == context_id)
        .context("lsp-coder Context is missing")?
        .system_prompt
        .clone()
        .context("lsp-coder Context has no system prompt")
}

/// One documents-only archive per core pack, each shipping a single skill, as
/// `(coordinate, .pack bytes)`.
pub fn skill_only_core_pack_archives() -> Vec<(String, Vec<u8>)> {
    let packs = tempfile::tempdir().unwrap();
    crate::pack::core_packs::CorePack::ALL
        .into_iter()
        .map(|pack| {
            let (_, name) = crate::pack::registry::split_pack_coordinate(pack.coordinate());
            let dir = packs.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let manifest = serde_json::json!({
                "manifest_version": 1, "name": name, "namespace": "gents",
                "version": "1.0.0", "description": "Core pack fixture",
                "authors": ["tests"], "tags": [], "kind": "documents",
                "assets": ["README.md", "pack_config.json"], "dependencies": [],
                "config": "pack_config.json",
            });
            let config = serde_json::json!({
                "node": {"node_did": "${GENTS_PACK_NODE_DID}"},
                "skills": [{"skill_id": format!("core-{name}"),
                    "node_did": "${GENTS_PACK_NODE_DID}", "name": name,
                    "description": "Core pack fixture", "instructions": "Use it."}],
            });
            for (file, bytes) in [
                ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
                ("pack_config.json", serde_json::to_vec(&config).unwrap()),
                ("README.md", b"# fixture".to_vec()),
            ] {
                std::fs::write(dir.join(file), bytes).unwrap();
            }
            let mut bytes = Vec::new();
            crate::pack::archive::write_pack(&dir, &mut bytes).unwrap();
            (pack.coordinate().to_owned(), bytes)
        })
        .collect()
}
