use gents_pack::plugin::store::{self, InstalledPlugin};
use gents_pack::plugin::test_fixtures::build_plugin_pack;

pub fn installed_plugin(
    wat: &str,
    access: Option<gents_pack::pack::BindAccess>,
) -> (tempfile::TempDir, InstalledPlugin) {
    let (mut declaration, bytes) = build_plugin_pack("echo_pack", wat, None);
    declaration.bind_dir = access.map(|access| gents_pack::pack::PluginDirBinding {
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
