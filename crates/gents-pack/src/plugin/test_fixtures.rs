use crate::pack::archive::pack_dir;
use crate::pack::PackPlugin;
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
