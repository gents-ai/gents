use crate::pack::archive::pack_dir;
use crate::pack::PackPlugin;
use crate::plugin::store::{self, InstalledPlugin};
use crate::plugin::Manifold;

pub(crate) fn wat(src: &str) -> Vec<u8> {
    wat::parse_str(src).expect("WAT parses")
}

pub const PLUGIN_ARTIFACT: &str = "plugins/plugin.afb";

pub fn constant_output_wat(json: &[u8]) -> String {
    let mut escaped = String::with_capacity(json.len() * 4);
    for byte in json {
        escaped.push_str(&format!("\\{byte:02x}"));
    }
    let len = json.len();
    // The iovec sits past the output, so an output over 8 KiB stays whole.
    let iov = len.next_multiple_of(8).max(8192);
    let pages = (iov + 16).div_ceil(65536);
    format!(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_write"
(func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") {pages})
  (data (i32.const 0) "{escaped}")
  (func (export "_start")
(i32.store (i32.const {iov}) (i32.const 0))
(i32.store (i32.const {iov_len}) (i32.const {len}))
(call $fd_write (i32.const 1) (i32.const {iov}) (i32.const 1) (i32.const {written}))
drop))
"#,
        iov_len = iov + 4,
        written = iov + 8,
    )
}

pub fn build_plugin_afb(wat_source: &str) -> Vec<u8> {
    build_plugin_afb_bytes(&wat(wat_source))
}

pub fn build_plugin_afb_bytes(wasm: &[u8]) -> Vec<u8> {
    use afterburner_afb::manifest::{Format, Manifest, Package, Runtime};
    use afterburner_afb::pack::Builder;

    let manifest = Manifest {
        format: Format {
            version: afterburner_afb::reader_format_version(),
            min_reader: None,
        },
        package: Package {
            name: "plugin".into(),
            namespace: "test".into(),
            version: "0.1.0".into(),
            language: "rust".into(),
            entry: "source/main".into(),
            description: None,
            homepage: None,
            license: None,
            keywords: vec![],
        },
        runtime: Runtime {
            min: afterburner_core::VERSION.to_owned(),
            target: Some("wasm32-wasip1".to_owned()),
        },
        dependencies: Default::default(),
        npm: Default::default(),
        pip: Default::default(),
        gem: Default::default(),
        signature: None,
        metadata: Default::default(),
        extra: Default::default(),
    };
    let (bytes, _digest) = Builder::new(manifest, Manifold::sealed())
        .precompiled("precompiled/wasm32-wasip1/main.wasm", wasm.to_vec())
        .build()
        .expect("build a real fixture .afb");
    bytes
}

pub fn build_plugin_pack(
    pack_name: &str,
    wat_source: &str,
    manifold: Option<serde_json::Value>,
) -> (PackPlugin, Vec<u8>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(pack_name);
    std::fs::create_dir_all(root.join("plugins")).expect("mkdir");
    std::fs::write(root.join("README.md"), b"# test pack").expect("write");
    std::fs::write(root.join(PLUGIN_ARTIFACT), build_plugin_afb(wat_source)).expect("write");

    let mut plugin = serde_json::json!({
        "name": "plugin",
        "description": "a test plugin",
        "language": "rust",
        "artifact": PLUGIN_ARTIFACT,
        "input_schema": {"type": "object"},
    });
    if let Some(manifold) = manifold {
        plugin["manifold"] = manifold;
    }
    let manifest = serde_json::json!({
        "manifest_version": 1,
        "name": pack_name,
        "version": "0.1.0",
        "description": "a test pack",
        "authors": ["test"],
        "tags": [],
        "kind": "plugins",
        "assets": ["README.md", PLUGIN_ARTIFACT],
        "plugins": [plugin],
    });
    std::fs::write(
        root.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("encode"),
    )
    .expect("write");

    let (bytes, _) = pack_dir(&root).expect("packing");
    let pack = crate::pack::archive::PackArchive::from_bytes(&bytes).expect("reading back");
    let plugin = pack.plugin("plugin").expect("the plugin").clone();
    let afb_bytes = pack
        .plugin_artifact("plugin")
        .expect("the artifact")
        .to_vec();
    (plugin, afb_bytes)
}

pub const ECHO_WAT: &str = r#"
      (module
        (import "wasi_snapshot_preview1" "fd_read"
          (func $fd_read (param i32 i32 i32 i32) (result i32)))
        (import "wasi_snapshot_preview1" "fd_write"
          (func $fd_write (param i32 i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "_start")
          i32.const 256  i32.const 0    i32.store
          i32.const 260  i32.const 256  i32.store
          i32.const 0
          i32.const 256
          i32.const 1
          i32.const 264
          call $fd_read
          drop
          i32.const 260  i32.const 264 i32.load  i32.store
          i32.const 1
          i32.const 256
          i32.const 1
          i32.const 268
          call $fd_write
          drop))
    "#;

pub fn filled_output_wat(prefix: &[u8], filler_len: usize, suffix: &[u8]) -> String {
    let total_len = prefix.len() + filler_len + suffix.len();
    let suffix_offset = total_len - suffix.len();
    let iovec_offset = total_len.next_multiple_of(4);
    let nwritten_offset = iovec_offset + 8;
    let pages = (nwritten_offset + 4).div_ceil(65536);

    let mut stores = String::new();
    for (offset, byte) in prefix.iter().enumerate() {
        stores.push_str(&format!(
            "    (i32.store8 (i32.const {offset}) (i32.const {byte}))\n"
        ));
    }
    for (index, byte) in suffix.iter().enumerate() {
        stores.push_str(&format!(
            "    (i32.store8 (i32.const {}) (i32.const {byte}))\n",
            suffix_offset + index
        ));
    }

    format!(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_write"
    (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") {pages})
  (func (export "_start")
{stores}    (memory.fill (i32.const {prefix_len}) (i32.const 0x61) (i32.const {filler_len}))
    (i32.store (i32.const {iovec_offset}) (i32.const 0))
    (i32.store (i32.const {iovec_len_offset}) (i32.const {total_len}))
    (call $fd_write (i32.const 1) (i32.const {iovec_offset}) (i32.const 1) (i32.const {nwritten_offset}))
    drop))
"#,
        prefix_len = prefix.len(),
        iovec_len_offset = iovec_offset + 4,
    )
}

/// A guest that hands its whole stdin to `gents::run_script`, then copies the
/// host's answer with `gents::read_result`: to stdout on success, to stderr
/// with exit code 1 when the host answered a negative code.
pub const IMPORTING_WAT: &str = r#"(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (import "gents" "run_script" (func $run_script (param i32 i32) (result i32)))
  (import "gents" "read_result" (func $read_result (param i32 i32) (result i32)))
  (memory (export "memory") 4)
  (func (export "_start")
    (local $n i32) (local $m i32)
    (i32.store (i32.const 1024) (i32.const 65536))
    (i32.store (i32.const 1028) (i32.const 65536))
    (drop (call $fd_read (i32.const 0) (i32.const 1024) (i32.const 1) (i32.const 1040)))
    (local.set $n (call $run_script (i32.const 65536) (i32.load (i32.const 1040))))
    (local.set $m (call $read_result (i32.const 131072) (i32.const 65536)))
    (i32.store (i32.const 1032) (i32.const 131072))
    (i32.store (i32.const 1036) (local.get $m))
    (if (i32.lt_s (local.get $n) (i32.const 0))
      (then
        (drop (call $fd_write (i32.const 2) (i32.const 1032) (i32.const 1) (i32.const 1048)))
        (call $proc_exit (i32.const 1)))
      (else
        (drop (call $fd_write (i32.const 1) (i32.const 1032) (i32.const 1) (i32.const 1048)))))))
"#;

pub fn source_only_afb(name: &str, language: &str, entry: &str, body: &[u8]) -> Vec<u8> {
    use afterburner_afb::manifest::{Format, Manifest, Package, Runtime};
    use afterburner_afb::pack::Builder;

    let manifest = Manifest {
        format: Format {
            version: afterburner_afb::reader_format_version(),
            min_reader: None,
        },
        package: Package {
            name: name.into(),
            namespace: "test".into(),
            version: "0.1.0".into(),
            language: language.into(),
            entry: entry.into(),
            description: None,
            homepage: None,
            license: None,
            keywords: vec![],
        },
        runtime: Runtime {
            min: afterburner_core::VERSION.to_owned(),
            target: None,
        },
        dependencies: Default::default(),
        npm: Default::default(),
        pip: Default::default(),
        gem: Default::default(),
        signature: None,
        metadata: Default::default(),
        extra: Default::default(),
    };
    let (bytes, _digest) = Builder::new(manifest, Manifold::sealed())
        .source(entry, body.to_vec())
        .build()
        .expect("build a source-only fixture .afb");
    bytes
}

pub fn plugin_named(name: &str, language: &str) -> PackPlugin {
    PackPlugin {
        name: name.to_owned(),
        description: format!("a {language} plugin"),
        artifact: PLUGIN_ARTIFACT.to_owned(),
        source: None,
        language: language.to_owned(),
        input_schema: serde_json::json!({"type": "object"}),
        manifold: None,
        instructions: None,
        bind_dir: None,
        limits: None,
        model_slot: None,
        service: None,
        ..Default::default()
    }
}

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
        plugin_interface: None,
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

pub fn create_file_wat(path: &str) -> String {
    format!(
        r#"(module
          (import "wasi_snapshot_preview1" "path_open"
            (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 512) "{path}")
          (data (i32.const 600) "{{}}")
          (func (export "_start")
            (if (call $path_open (i32.const 3) (i32.const 0) (i32.const 512) (i32.const {len})
                  (i32.const 1) (i64.const 64) (i64.const 0) (i32.const 0) (i32.const 272))
              (then (return)))
            i32.const 256  i32.const 600  i32.store
            i32.const 260  i32.const 2    i32.store
            (drop (call $fd_write (i32.load (i32.const 272)) (i32.const 256) (i32.const 1) (i32.const 268)))
            (drop (call $fd_write (i32.const 1) (i32.const 256) (i32.const 1) (i32.const 268)))))"#,
        len = path.len()
    )
}

pub fn open_and_copy_wat(path: &str, oflags: u32) -> String {
    format!(
        r#"(module
          (import "wasi_snapshot_preview1" "path_open"
            (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_read"
            (func $fd_read (param i32 i32 i32 i32) (result i32)))
          (import "wasi_snapshot_preview1" "fd_write"
            (func $fd_write (param i32 i32 i32 i32) (result i32)))
          (memory (export "memory") 1)
          (data (i32.const 512) "{path}")
          (func (export "_start")
            (if (call $path_open (i32.const 3) (i32.const 0) (i32.const 512) (i32.const {len})
                  (i32.const {oflags}) (i64.const {rights}) (i64.const 0) (i32.const 0) (i32.const 272))
              (then (return)))
            i32.const 256  i32.const 0    i32.store
            i32.const 260  i32.const 128  i32.store
            (drop (call $fd_read (i32.load (i32.const 272)) (i32.const 256) (i32.const 1) (i32.const 264)))
            i32.const 260  i32.const 264 i32.load  i32.store
            (drop (call $fd_write (i32.const 1) (i32.const 256) (i32.const 1) (i32.const 268)))))"#,
        len = path.len(),
        rights = if oflags == 0 { 2 } else { 66 }
    )
}
