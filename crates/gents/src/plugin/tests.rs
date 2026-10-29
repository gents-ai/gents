use super::*;
use crate::pack_archive::pack_dir;

fn wat(src: &str) -> Vec<u8> {
    wat::parse_str(src).expect("WAT parses")
}

const PLUGIN_ARTIFACT: &str = "plugins/plugin.afb";

pub(crate) fn constant_output_wat(json: &[u8]) -> String {
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

pub(crate) fn build_plugin_afb(wat_source: &str) -> Vec<u8> {
    build_plugin_afb_bytes(&wat(wat_source))
}

pub(crate) fn build_plugin_afb_bytes(wasm: &[u8]) -> Vec<u8> {
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

pub(crate) fn build_plugin_pack(
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
    let pack = crate::pack_archive::PackArchive::from_bytes(&bytes).expect("reading back");
    let plugin = pack.plugin("plugin").expect("the plugin").clone();
    let afb_bytes = pack
        .plugin_artifact("plugin")
        .expect("the artifact")
        .to_vec();
    (plugin, afb_bytes)
}

pub(crate) const ECHO_WAT: &str = r#"
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

#[test]
fn echo_plugin_returns_exactly_its_input() {
    let (plugin, afb) = build_plugin_pack("echo_pack", ECHO_WAT, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");
    assert_eq!(runner.definition().name, "plugin");

    let arguments = serde_json::json!({"hello": "world", "n": 42});
    let outcome = runner
        .call(&arguments, &PluginBudget::default())
        .expect("call succeeds");
    assert_eq!(outcome.verdict, PluginVerdict::Success);
    assert_eq!(outcome.output, arguments);
}

#[test]
fn two_calls_on_one_runner_never_see_each_others_arguments() {
    let (plugin, afb) = build_plugin_pack("echo_twice_pack", ECHO_WAT, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let first = runner
        .call(&serde_json::json!({"call": 1}), &PluginBudget::default())
        .expect("first call");
    let second = runner
        .call(&serde_json::json!({"call": 2}), &PluginBudget::default())
        .expect("second call");

    assert_eq!(first.output, serde_json::json!({"call": 1}));
    assert_eq!(second.output, serde_json::json!({"call": 2}));
}

#[test]
fn non_json_stdout_is_bad_output() {
    let wat_source = r#"
          (module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "not json")
            (func (export "_start")
              i32.const 16  i32.const 0  i32.store
              i32.const 20  i32.const 8  i32.store
              i32.const 1
              i32.const 16
              i32.const 1
              i32.const 24
              call $fd_write
              drop))
        "#;
    let (plugin, afb) = build_plugin_pack("bad_output_pack", wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let outcome = runner
        .call(&serde_json::json!({}), &PluginBudget::default())
        .expect("call succeeds at the VM level");
    assert_eq!(outcome.verdict, PluginVerdict::BadOutput);
    assert_eq!(outcome.output, serde_json::Value::Null);
    assert!(
        outcome.diagnostics.contains("not a single JSON value"),
        "diagnostics: {}",
        outcome.diagnostics
    );
}

#[test]
fn a_non_zero_exit_is_a_failure_even_with_json_stdout() {
    let wat_source = r#"
          (module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "proc_exit"
              (func $proc_exit (param i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "{}")
            (func (export "_start")
              i32.const 16  i32.const 0  i32.store
              i32.const 20  i32.const 2  i32.store
              i32.const 1
              i32.const 16
              i32.const 1
              i32.const 24
              call $fd_write
              drop
              i32.const 3
              call $proc_exit))
        "#;
    let (plugin, afb) = build_plugin_pack("failing_pack", wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let outcome = runner
        .call(&serde_json::json!({}), &PluginBudget::default())
        .expect("call succeeds at the VM level");
    assert_eq!(outcome.verdict, PluginVerdict::Failed);
    assert_eq!(outcome.output, serde_json::Value::Null);
    assert!(
        outcome.diagnostics.contains("exited with code 3"),
        "diagnostics: {}",
        outcome.diagnostics
    );
}

#[test]
fn an_infinite_loop_exhausts_an_explicit_fuel_ceiling() {
    let wat_source = r#"
          (module
            (func (export "_start")
              (loop $forever
                br $forever)))
        "#;
    let (plugin, afb) = build_plugin_pack("fuel_pack", wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let budget = PluginBudget {
        fuel: Some(10_000),
        ..PluginBudget::default()
    };
    let outcome = runner
        .call(&serde_json::json!({}), &budget)
        .expect("call reports fuel exhaustion, not a hard error");
    assert_eq!(outcome.verdict, PluginVerdict::OutOfFuel);
    assert_eq!(
        outcome.fuel_used, 10_000,
        "exhaustion means the whole ceiling was spent"
    );
    assert!(
        outcome.diagnostics.contains("fuel budget"),
        "a ceiling the caller set is a budget it exhausted: {}",
        outcome.diagnostics
    );
}

#[test]
fn the_default_budget_has_no_fuel_ceiling_so_a_guest_over_100_million_instructions_still_runs() {
    let wat_source = r#"
          (module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "{}")
            (func (export "_start")
              (local $i i32)
              (local.set $i (i32.const 100000000))
              (loop $spin
                (local.set $i (i32.sub (local.get $i) (i32.const 1)))
                (br_if $spin (i32.gt_s (local.get $i) (i32.const 0))))
              i32.const 16  i32.const 0  i32.store
              i32.const 20  i32.const 2  i32.store
              i32.const 1
              i32.const 16
              i32.const 1
              i32.const 24
              call $fd_write
              drop))
        "#;
    let (plugin, afb) = build_plugin_pack("unlimited_fuel_pack", wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    assert_eq!(
        PluginBudget::default().fuel,
        None,
        "the default has no ceiling"
    );
    let outcome = runner
        .call(&serde_json::json!({}), &PluginBudget::default())
        .expect("call succeeds");
    assert_eq!(outcome.verdict, PluginVerdict::Success, "{outcome:?}");
    assert_eq!(outcome.output, serde_json::json!({}));
    assert!(
        outcome.fuel_used > 100_000_000,
        "the loop alone spends more than the old fixed default: {}",
        outcome.fuel_used
    );
}

#[test]
fn a_slow_plugin_is_stopped_by_its_wall_clock_budget() {
    let wat_source = r#"
          (module
            (func (export "_start")
              (loop $forever
                br $forever)))
        "#;
    let (plugin, afb) = build_plugin_pack("timeout_pack", wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let budget = PluginBudget {
        fuel: Some(5_000_000_000),
        wall_clock: std::time::Duration::from_millis(30),
        ..PluginBudget::default()
    };
    let outcome = runner
        .call(&serde_json::json!({}), &budget)
        .expect("call reports a timeout, not a hard error");
    assert_eq!(outcome.verdict, PluginVerdict::Timeout);
}

#[test]
fn a_slow_plugin_is_stopped_by_its_wall_clock_budget_even_with_unlimited_fuel() {
    let wat_source = r#"
          (module
            (func (export "_start")
              (loop $forever
                br $forever)))
        "#;
    let (plugin, afb) = build_plugin_pack("timeout_unlimited_fuel_pack", wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let budget = PluginBudget {
        wall_clock: std::time::Duration::from_millis(30),
        ..PluginBudget::default()
    };
    assert_eq!(
        budget.fuel, None,
        "no ceiling: only the wall clock bounds this call"
    );
    let outcome = runner
        .call(&serde_json::json!({}), &budget)
        .expect("call reports a timeout, not a hard error");
    assert_eq!(outcome.verdict, PluginVerdict::Timeout);
}

#[test]
fn a_plugin_with_no_declared_manifold_gets_no_filesystem() {
    let wat_source = r#"
          (module
            (import "wasi_snapshot_preview1" "fd_write"
              (func $fd_write (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "{\"ok\":true}")
            (data (i32.const 16) "{\"ok\":false}")
            (func (export "_start")
              (local $errno i32)
              i32.const 64  i32.const 32  i32.store
              i32.const 68  i32.const 4   i32.store
              i32.const 3
              i32.const 64
              i32.const 1
              i32.const 72
              call $fd_write
              local.set $errno
              (if (i32.eq (local.get $errno) (i32.const 0))
                (then
                  i32.const 80  i32.const 0   i32.store
                  i32.const 84  i32.const 11  i32.store)
                (else
                  i32.const 80  i32.const 16  i32.store
                  i32.const 84  i32.const 12  i32.store))
              i32.const 1
              i32.const 80
              i32.const 1
              i32.const 88
              call $fd_write
              drop))
        "#;
    let (plugin, afb) = build_plugin_pack("no_manifold_pack", wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");
    assert_eq!(
        runner.manifold,
        Manifold::sealed(),
        "no declared manifold narrows to nothing"
    );

    let outcome = runner
        .call(&serde_json::json!({}), &PluginBudget::default())
        .expect("call succeeds");
    assert_eq!(outcome.verdict, PluginVerdict::Success);
    assert_eq!(
        outcome.output,
        serde_json::json!({"ok": false}),
        "fd 3 must not exist: no filesystem capability was granted"
    );
}

#[test]
fn a_wide_declared_manifold_is_still_narrowed_to_this_runners_ceiling() {
    let wat_source = r#"(module (func (export "_start")))"#;
    let manifold = serde_json::json!({
        "fs": {"ReadWrite": ["/data"]},
        "net": {"OutboundHttp": null},
        "env": "Full",
        "crypto": true,
        "child_process": false,
        "allow_exit": false,
        "http_timeout_ms": null,
        "listen": "None"
    });
    let (plugin, afb) = build_plugin_pack("wide_manifold_pack", wat_source, Some(manifold));
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");
    assert_eq!(runner.manifold, Manifold::sealed());
}

fn source_only_afb(name: &str, language: &str, entry: &str, body: &[u8]) -> Vec<u8> {
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

fn plugin_named(name: &str, language: &str) -> PackPlugin {
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

#[test]
fn a_ruby_source_plugin_is_refused_at_admission_not_silently_run() {
    let afb_bytes = source_only_afb("rb_plugin", "ruby", "source/main.rb", b"puts 'hello'");
    let plugin = plugin_named("rb_plugin", "ruby");

    let error = PluginRunner::compile(&afb_bytes, &plugin)
        .expect_err("a ruby-source plugin must never be admitted");
    let message = format!("{error:#}");
    assert!(message.contains("rb_plugin"), "{message}");
    assert!(message.contains("stdin"), "{message}");
    assert!(message.contains("fuel"), "{message}");
    assert!(message.contains("wall-clock"), "{message}");
}

#[test]
fn a_default_budget_is_raised_to_what_the_artifact_needs_to_start() {
    let wasi = source_only_afb("rs_plugin", "rust", "source/main.rs", b"fn main() {}");
    let wasi = afterburner_afb::Afb::from_bytes(&wasi).expect("readable .afb");
    assert_eq!(
        PluginBudget::for_artifact(&wasi).memory_bytes,
        PluginBudget::default().memory_bytes,
        "an ordinary WASI command needs no floor, so its budget is the default"
    );

    let python = source_only_afb("py_plugin", "python", "source/main.py", b"print(1)");
    let python = afterburner_afb::Afb::from_bytes(&python).expect("readable .afb");
    let budget = PluginBudget::for_artifact(&python);
    let floor = afterburner::afb_run::startup_floor(&python).expect("python declares a floor");
    assert!(
        budget.memory_bytes >= floor.memory_bytes,
        "a budget that cannot instantiate the runtime bounds nothing: {} < {}",
        budget.memory_bytes,
        floor.memory_bytes
    );
    assert_eq!(
        budget.fuel, None,
        "the default has no ceiling, so python's startup cost never needs raising against one"
    );
    assert!(
        budget.wall_clock > PluginBudget::default().wall_clock,
        "a wall clock that cannot cover the boot reports Timeout on every call"
    );
}

#[test]
fn for_plugin_raises_the_default_to_declared_limits() {
    let wasi = source_only_afb("rs_plugin", "rust", "source/main.rs", b"fn main() {}");
    let wasi = afterburner_afb::Afb::from_bytes(&wasi).expect("readable .afb");

    let mut plugin = plugin_named("rs_plugin", "rust");
    plugin.limits = Some(crate::pack::PluginLimits {
        memory_mib: Some(512),
        wall_clock_secs: Some(120),
        max_output_mib: Some(4),
    });
    let budget = PluginBudget::for_plugin(&wasi, &plugin);
    assert_eq!(budget.memory_bytes, 512 * 1024 * 1024);
    assert_eq!(budget.wall_clock, std::time::Duration::from_secs(120));
    assert_eq!(budget.max_output_bytes, 4 * 1024 * 1024);

    let no_limits = plugin_named("rs_plugin", "rust");
    assert_eq!(
        PluginBudget::for_plugin(&wasi, &no_limits).memory_bytes,
        PluginBudget::for_artifact(&wasi).memory_bytes
    );
}

const LARGE_OUTPUT_PREFIX: &[u8] = b"{\"filler\":\"";
const LARGE_OUTPUT_SUFFIX: &[u8] = b"\"}";

fn large_json_output_wat(total_len: usize) -> String {
    let envelope_len = LARGE_OUTPUT_PREFIX.len() + LARGE_OUTPUT_SUFFIX.len();
    assert!(total_len > envelope_len, "need room for the JSON envelope");
    filled_output_wat(
        LARGE_OUTPUT_PREFIX,
        total_len - envelope_len,
        LARGE_OUTPUT_SUFFIX,
    )
}

pub(crate) fn filled_output_wat(prefix: &[u8], filler_len: usize, suffix: &[u8]) -> String {
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

#[test]
fn a_call_over_the_default_output_cap_succeeds_once_the_declared_limit_covers_it() {
    const TOTAL_LEN: usize = 1_500_000;
    let wat_source = large_json_output_wat(TOTAL_LEN);
    let (mut plugin, afb) = build_plugin_pack("large_output_pack", &wat_source, None);
    plugin.limits = Some(crate::pack::PluginLimits {
        memory_mib: None,
        wall_clock_secs: None,
        max_output_mib: Some(2),
    });
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let under_default = runner
        .call(&serde_json::json!({}), &PluginBudget::default())
        .expect("the guest itself runs to completion");
    assert_eq!(
        under_default.verdict,
        PluginVerdict::BadOutput,
        "a {TOTAL_LEN}-byte result must be refused under the default 1 MiB cap: {:?}",
        under_default.diagnostics
    );

    let afb_parsed = afterburner_afb::Afb::from_bytes(&afb).expect("readable .afb");
    let raised_budget = PluginBudget::for_plugin(&afb_parsed, &plugin);
    let raised = runner
        .call(&serde_json::json!({}), &raised_budget)
        .expect("the guest itself runs to completion");
    assert_eq!(
        raised.verdict,
        PluginVerdict::Success,
        "{:?}",
        raised.diagnostics
    );
    assert_eq!(
        raised
            .output
            .get("filler")
            .and_then(serde_json::Value::as_str)
            .map(str::len),
        Some(TOTAL_LEN - LARGE_OUTPUT_PREFIX.len() - LARGE_OUTPUT_SUFFIX.len())
    );
}

fn overflow_stdout_capture_pipe_wat(chunks: u32) -> String {
    const CHUNK_BYTES: u32 = 65536;
    let iovec_ptr = CHUNK_BYTES;
    let iovec_len_ptr = iovec_ptr + 4;
    let nwritten_ptr = iovec_len_ptr + 4;
    format!(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_write"
    (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (func (export "_start")
    (local $i i32)
    (i32.store (i32.const {iovec_ptr}) (i32.const 0))
    (i32.store (i32.const {iovec_len_ptr}) (i32.const {CHUNK_BYTES}))
    (local.set $i (i32.const 0))
    (block $done
      (loop $loop
        (br_if $done (i32.ge_u (local.get $i) (i32.const {chunks})))
        (call $fd_write (i32.const 1) (i32.const {iovec_ptr}) (i32.const 1) (i32.const {nwritten_ptr}))
        drop
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $loop)))
    ))
"#
    )
}

#[test]
fn a_trap_that_fills_the_stdout_capture_pipe_is_bad_output_not_a_hard_error() {
    let wat_source = overflow_stdout_capture_pipe_wat(65);
    let (plugin, afb) = build_plugin_pack("overflow_pack", &wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let outcome = runner
        .call(&serde_json::json!({}), &PluginBudget::default())
        .expect("an output-capacity trap is reclassified, not a hard error");
    assert_eq!(
        outcome.verdict,
        PluginVerdict::BadOutput,
        "{:?}",
        outcome.diagnostics
    );
}

#[test]
fn a_trap_with_little_captured_output_stays_a_hard_error() {
    let wat_source = r#"(module
  (func (export "_start") unreachable))
"#;
    let (plugin, afb) = build_plugin_pack("crash_pack", wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let err = runner
        .call(&serde_json::json!({}), &PluginBudget::default())
        .expect_err("a guest that never wrote stdout is a genuine crash, not BadOutput");
    assert!(err.to_string().contains("trapped"), "{err}");
}

#[test]
fn a_python_plugin_is_admitted_because_its_bounds_are_enforced() {
    let afb_bytes = source_only_afb("py_plugin", "python", "source/main.py", b"print('hello')");
    let plugin = plugin_named("py_plugin", "python");

    PluginRunner::compile(&afb_bytes, &plugin)
        .expect("python's dispatch path enforces every bound a plugin call applies");
}

#[test]
fn a_plugin_whose_artifact_is_not_a_readable_afb_is_refused() {
    let plugin = PackPlugin {
        name: "broken".to_owned(),
        description: "not a real afb".to_owned(),
        artifact: PLUGIN_ARTIFACT.to_owned(),
        source: None,
        language: "rust".to_owned(),
        input_schema: serde_json::json!({"type": "object"}),
        manifold: None,
        instructions: None,
        bind_dir: None,
        limits: None,
        model_slot: None,
        service: None,
        ..Default::default()
    };
    let error = PluginRunner::compile(b"not an afb", &plugin).expect_err("must be refused");
    assert!(format!("{error:#}").contains("broken"), "{error:#}");
}

#[test]
fn an_admitting_caller_can_widen_the_ceiling_but_never_past_the_plugin() {
    use afterburner_core::manifold::FsAccess;

    let sealed = narrow_manifold(
        &Manifold {
            fs: FsAccess::ReadOnly(vec!["/workspace".into()]),
            ..Manifold::sealed()
        },
        &PluginRunner::CEILING,
    );
    assert!(matches!(sealed.fs, FsAccess::None));

    let admitted = narrow_manifold(
        &Manifold {
            fs: FsAccess::ReadOnly(vec!["/workspace".into()]),
            ..Manifold::sealed()
        },
        &Manifold {
            fs: FsAccess::ReadWrite(vec!["/workspace".into(), "/tmp".into()]),
            ..Manifold::sealed()
        },
    );
    match admitted.fs {
        FsAccess::ReadOnly(roots) => {
            assert_eq!(roots, vec![std::path::PathBuf::from("/workspace")])
        }
        other => panic!("a read-only request must stay read-only, got {other:?}"),
    }
}

#[test]
fn narrow_manifold_grants_the_intersection_not_the_union() {
    let declared = Manifold {
        fs: FsAccess::ReadWrite(vec![PathBuf::from("/a"), PathBuf::from("/b")]),
        net: NetAccess::OutboundFull(None),
        env: EnvAccess::Full,
        crypto: true,
        child_process: true,
        allow_exit: true,
        http_timeout_ms: Some(10_000),
        listen: ListenAccess::None,
    };
    let ceiling = Manifold {
        fs: FsAccess::ReadOnly(vec![PathBuf::from("/a")]),
        net: NetAccess::OutboundHttp(Some(vec!["example.com".to_owned()])),
        env: EnvAccess::AllowList(vec!["HOME".to_owned()]),
        crypto: false,
        child_process: true,
        allow_exit: false,
        http_timeout_ms: Some(5_000),
        listen: ListenAccess::None,
    };

    let granted = narrow_manifold(&declared, &ceiling);

    assert_eq!(
        granted.fs,
        FsAccess::ReadOnly(vec![PathBuf::from("/a")]),
        "the narrower access level, over the roots common to both"
    );
    assert_eq!(
        granted.net,
        NetAccess::OutboundHttp(Some(vec!["example.com".to_owned()])),
        "the narrower net kind, over the hosts common to both"
    );
    assert_eq!(granted.env, EnvAccess::AllowList(vec!["HOME".to_owned()]));
    assert!(!granted.crypto, "crypto requires both sides to grant it");
    assert!(granted.child_process, "both sides grant child_process");
    assert!(!granted.allow_exit, "allow_exit requires both sides");
    assert_eq!(granted.http_timeout_ms, Some(5_000), "the tighter cap wins");
}

#[test]
fn narrow_manifold_never_widens_past_what_the_plugin_declared() {
    let declared = Manifold {
        fs: FsAccess::ReadOnly(vec![PathBuf::from("/a")]),
        ..Manifold::sealed()
    };
    let granted = narrow_manifold(&declared, &Manifold::open());
    assert_eq!(
        granted.fs,
        FsAccess::ReadOnly(vec![PathBuf::from("/a")]),
        "a wider ceiling must never widen the plugin's own declared grant"
    );
    assert!(!granted.crypto, "the plugin never declared crypto");
}

#[test]
fn narrow_manifold_with_no_declared_manifold_grants_nothing() {
    assert_eq!(
        narrow_manifold(&Manifold::sealed(), &Manifold::open()),
        Manifold::sealed()
    );
}

#[test]
fn narrow_manifold_always_forces_listen_to_none() {
    let mut declared = Manifold::open();
    declared.listen = ListenAccess::Any;
    let mut ceiling = Manifold::open();
    ceiling.listen = ListenAccess::Any;
    assert_eq!(
        narrow_manifold(&declared, &ceiling).listen,
        ListenAccess::None,
        "a plugin is called, never a server, whatever either side asked for"
    );
}

fn job_tool_ref(job: Option<serde_json::Value>) -> crate::document_config::PluginToolRef {
    crate::document_config::PluginToolRef {
        plugin: "team/plugin".into(),
        digest: None,
        tool_calls: false,
        input_fields: Vec::new(),
        job: job.map(|job| serde_json::from_value(job).unwrap()),
    }
}

#[test]
fn job_block_on_a_plugin_without_job_true_is_refused() {
    let (home, record) = executor::installed_echo();
    assert!(!record.declaration.job.0);
    let executor = std::sync::Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let block = serde_json::json!({"agent_targets": ["worker"]});
    let error =
        crate::plugin::tool::PluginTool::resolve(executor, &job_tool_ref(Some(block)), None)
            .err()
            .expect("a job block needs a plugin that declares job dispatch");
    assert_eq!(
        format!("{error:#}"),
        "plugin team/plugin does not declare job dispatch; install a pack whose plugin sets `job: true`"
    );
}

#[test]
fn plugin_without_job_block_keeps_the_plugin_tool_path() {
    let (home, _) = executor::installed_echo();
    let executor = std::sync::Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    crate::plugin::tool::PluginTool::resolve(executor, &job_tool_ref(None), None)
        .expect("a plugin without a job block resolves as before");
}

fn job_resolutions() -> (
    tempfile::TempDir,
    Vec<(
        crate::document_config::PluginToolRef,
        Option<crate::plugin::store::InstalledPlugin>,
    )>,
) {
    let (home, plain) = executor::installed_echo();
    let mut job_capable = plain.clone();
    job_capable.declaration.job = gents_protocol::job::PluginJobDeclaration(true);
    let mut not_job = job_tool_ref(None);
    not_job.plugin = "team/plain".into();
    let mut not_installed = job_tool_ref(None);
    not_installed.plugin = "team/missing".into();
    let resolutions = vec![
        (job_tool_ref(None), Some(job_capable)),
        (not_job, Some(plain)),
        (not_installed, None),
    ];
    (home, resolutions)
}

#[test]
fn job_tools_are_bound_only_for_job_capable_plugins() {
    let (_home, resolutions) = job_resolutions();
    let job_tools = crate::plugin_job::JobBinding::bind_all(&resolutions);
    assert_eq!(
        job_tools.keys().collect::<Vec<_>>(),
        vec!["plugin"],
        "only the entry whose installed plugin declares job: true binds"
    );
    let binding = &job_tools["plugin"];
    assert_eq!(binding.tool_name, "plugin");
    assert_eq!(binding.plugin, job_tool_ref(None));
    assert_eq!(
        binding.settings, None,
        "an absent block binds as the empty ceiling"
    );
}

#[test]
fn plugin_child_call_naming_a_job_tool_is_hook_owned_child_refused() {
    let (_home, resolutions) = job_resolutions();
    let job_tools: std::collections::BTreeSet<String> =
        crate::plugin_job::JobBinding::bind_all(&resolutions)
            .into_keys()
            .collect();
    assert!(!crate::plugin::tool_calls::supported("plugin", &job_tools));
    assert!(crate::plugin::tool_calls::supported("plain", &job_tools));
}

#[test]
fn plugin_job_keys_reexports_the_encoders_and_the_decoder() {
    use crate::plugin_job::keys::{
        decode_job_retry_key, job_message_key, job_session_id, job_spawn_key,
    };
    let _ = (job_message_key, job_session_id);
    assert_eq!(
        decode_job_retry_key(&job_spawn_key("r", "s", 2)),
        Some(("s".to_string(), 2))
    );
}

pub(crate) mod executor;
#[cfg(target_os = "macos")]
mod trap_handler;

#[test]
fn a_plugin_call_gets_one_attempt_unless_configured() {
    assert_eq!(crate::plugin::attempts_allowed(None), 1);
    assert_eq!(crate::plugin::attempts_allowed(Some(0)), 1);
    assert_eq!(crate::plugin::attempts_allowed(Some(4)), 4);
}

#[test]
fn retry_backoff_doubles_and_is_capped() {
    use std::time::Duration;
    assert_eq!(crate::plugin::retry_backoff(0), Duration::from_secs(1));
    assert_eq!(crate::plugin::retry_backoff(1), Duration::from_secs(1));
    assert_eq!(crate::plugin::retry_backoff(2), Duration::from_secs(2));
    assert_eq!(crate::plugin::retry_backoff(4), Duration::from_secs(8));
    assert_eq!(crate::plugin::retry_backoff(40), Duration::from_secs(60));
}

#[test]
fn tool_instructions_live_beside_the_plugin_and_stay_small_text() {
    let mut plugin: PackPlugin = serde_json::from_value(serde_json::json!({
        "name": "lint", "description": "Lints", "artifact": "plugins/lint.afb",
        "language": "rust", "input_schema": {"type": "object"},
        "instructions": "plugins/lint/TOOL.md",
    }))
    .unwrap();
    plugin.validate().unwrap();
    plugin.instructions = Some("plugins/other/TOOL.md".into());
    assert!(plugin.validate().is_err(), "another plugin's instructions");
    plugin.instructions = Some("plugins/lint/README.md".into());
    assert!(plugin.validate().is_err(), "not a TOOL.md");

    assert_eq!(
        crate::pack::tool_instructions("lint", b"# lint\nUse it.").unwrap(),
        "# lint\nUse it."
    );
    assert!(crate::pack::tool_instructions("lint", &[0xff, 0xfe]).is_err());
    let oversized = vec![b'a'; crate::pack::MAX_TOOL_INSTRUCTIONS_BYTES + 1];
    assert!(crate::pack::tool_instructions("lint", &oversized).is_err());
}

mod call_bound;
pub(crate) use call_bound::{create_file_wat, open_and_copy_wat};

#[test]
fn plugin_requests_select_native_nan_arithmetic() {
    let request = plugin_run_request(Vec::new(), Manifold::sealed(), &PluginBudget::default());
    assert_eq!(request.nan_mode, NanMode::Native);
}

#[test]
fn generated_plugin_resource_cases_bind_budget_and_consent() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases;
    for case in cases["consent"].as_array().unwrap() {
        let requested = case["requested"].as_u64().unwrap() as u32;
        let previous = case["previous"].as_u64().unwrap() as u32;
        let consent = case["consent"].as_bool().unwrap();
        for axis in 0..3 {
            let mut requested_limits = crate::pack::PluginLimits::default();
            let mut previous_limits = crate::pack::PluginLimits::default();
            match axis {
                0 => {
                    requested_limits.memory_mib = Some(requested);
                    previous_limits.memory_mib = Some(previous);
                }
                1 => {
                    requested_limits.wall_clock_secs = Some(requested);
                    previous_limits.wall_clock_secs = Some(previous);
                }
                _ => {
                    requested_limits.max_output_mib = Some(requested);
                    previous_limits.max_output_mib = Some(previous);
                }
            }
            assert_eq!(
                authority::limits_consented(
                    Some(&requested_limits),
                    Some(&previous_limits),
                    consent
                ),
                case["expected"].as_bool().unwrap(),
                "{case}"
            );
        }
    }
    for case in cases["budgets"].as_array().unwrap() {
        let baseline = case["baseline"].as_u64().unwrap();
        let requested = case["requested"].as_u64().unwrap() as u32;
        let ceiling = case["ceiling"].as_u64().unwrap();
        let expected = case["expected"].as_u64().unwrap();
        let mut budget = PluginBudget::default();
        let mut limits = crate::pack::PluginLimits::default();
        match ceiling {
            4096 => {
                budget.memory_bytes = baseline * 1024 * 1024;
                limits.memory_mib = Some(requested);
            }
            900 => {
                budget.wall_clock = std::time::Duration::from_secs(baseline);
                limits.wall_clock_secs = Some(requested);
            }
            4 => {
                budget.max_output_bytes = baseline as usize * 1024 * 1024;
                limits.max_output_mib = Some(requested);
            }
            _ => panic!("unknown resource case: {case}"),
        }
        let effective = budget.with_declared_limits(&limits);
        let actual = match ceiling {
            4096 => effective.memory_bytes / (1024 * 1024),
            900 => effective.wall_clock.as_secs(),
            _ => effective.max_output_bytes as u64 / (1024 * 1024),
        };
        assert_eq!(actual, expected, "{case}");
    }
}

fn path_open_probe(path: &str, create: bool) -> String {
    let flags = if create { 1 } else { 0 };
    let rights = if create { 64 } else { 2 };
    format!(
        r#"(module
      (import "wasi_snapshot_preview1" "path_open" (func $open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
      (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 32) "{path}")
      (data (i32.const 128) "true ")
      (data (i32.const 144) "false")
      (func (export "_start") (local $denied i32)
        (local.set $denied (call $open (i32.const 3) (i32.const 1) (i32.const 32) (i32.const {length}) (i32.const {flags}) (i64.const {rights}) (i64.const 0) (i32.const 0) (i32.const 200)))
        (i32.store (i32.const 256) (select (i32.const 128) (i32.const 144) (local.get $denied)))
        (i32.store (i32.const 260) (i32.const 5))
        (drop (call $write (i32.const 1) (i32.const 256) (i32.const 1) (i32.const 264)))))"#,
        length = path.len()
    )
}

#[test]
#[cfg(unix)]
fn bound_guest_can_read_inside_but_cannot_write_or_follow_a_symlink_out() {
    let root = tempfile::tempdir().unwrap();
    let inside = root.path().join("inside");
    std::fs::create_dir(&inside).unwrap();
    std::fs::write(inside.join("safe.txt"), b"safe").unwrap();
    std::fs::write(root.path().join("outside.txt"), b"private").unwrap();
    std::os::unix::fs::symlink(root.path().join("outside.txt"), inside.join("escape.txt")).unwrap();
    let bound = BoundDir::new(&inside, None).unwrap();
    for (path, create, denied) in [
        ("safe.txt", false, false),
        ("created.txt", true, true),
        ("escape.txt", false, true),
    ] {
        let (mut plugin, afb) =
            build_plugin_pack("containment", &path_open_probe(path, create), None);
        plugin.bind_dir = Some(crate::pack::PluginDirBinding {
            input_field: "root".into(),
            description: "test root".into(),
            access: crate::pack::BindAccess::Read,
            write_fields: Vec::new(),
            original_field: None,
        });
        let runner = PluginRunner::compile(&afb, &plugin).unwrap();
        let result = runner
            .call_bound(&serde_json::json!({}), &PluginBudget::default(), &bound)
            .unwrap();
        assert_eq!(result.verdict, PluginVerdict::Success, "{path}: {result:?}");
        assert_eq!(result.output, serde_json::json!(denied), "{path}");
    }
    assert!(!inside.join("created.txt").exists());
}

#[tokio::test]
async fn generated_plugin_call_access_cases_drive_bind_admission() {
    const CHILD_ENV: &str = "GENTS_PLUGIN_CALL_ACCESS_FIXTURE";
    const TEST: &str = "plugin::tests::generated_plugin_call_access_cases_drive_bind_admission";
    let Some(marker) = std::env::var_os(CHILD_ENV) else {
        // Permission fixtures mutate a process-wide protected-path registry. The
        // modeled cases require a stable filesystem, independent of those tests.
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("verified");
        let cases = tokio::task::spawn_blocking(|| {
            crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases["call_access"]
                .clone()
        })
        .await
        .unwrap();
        tokio::fs::write(
            directory.path().join("cases.json"),
            serde_json::to_vec(&cases).unwrap(),
        )
        .await
        .unwrap();
        let output_path = directory.path().join("output");
        let output = tokio::fs::File::create(&output_path)
            .await
            .unwrap()
            .into_std()
            .await;
        let error = output.try_clone().unwrap();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, &marker)
            .stdin(std::process::Stdio::null())
            .stdout(output)
            .stderr(error)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(180), child.wait()).await;
        if status.is_err() {
            child.kill().await.unwrap();
            child.wait().await.unwrap();
        }
        let mut output = tokio::fs::File::open(output_path).await.unwrap();
        let mut diagnostics = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(
            &mut tokio::io::AsyncReadExt::take(&mut output, 65_536),
            &mut diagnostics,
        )
        .await
        .unwrap();
        assert!(
            matches!(status, Ok(Ok(status)) if status.success()),
            "isolated admission cases failed: {status:?}\n{}",
            String::from_utf8_lossy(&diagnostics)
        );
        assert_eq!(tokio::fs::read(marker).await.unwrap(), b"24 cases verified");
        return;
    };
    use crate::pack::BindAccess;
    let marker = std::path::PathBuf::from(marker);
    let cases: serde_json::Value = serde_json::from_slice(
        &tokio::fs::read(marker.parent().unwrap().join("cases.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    let access = |value: &serde_json::Value| -> Option<BindAccess> {
        value.as_str().map(|text| text.parse().unwrap())
    };
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), 24);
    for case in cases {
        let declared = access(&case["declared"]).unwrap();
        let write_fields = case["write_fields"].as_bool().unwrap();
        let sets = case["sets_write_field"].as_bool().unwrap();
        let (home, mut record) = executor::installed_plugin(ECHO_WAT, Some(declared));
        let binding = record.declaration.bind_dir.as_mut().unwrap();
        if write_fields {
            binding.write_fields = vec!["output".into()];
        }
        record.declaration.input_schema = serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "output": {"type": "string"}},
        });
        assert_eq!(
            record.declaration.validate().is_ok(),
            case["valid"].as_bool().unwrap(),
            "{case}"
        );
        if !case["valid"].as_bool().unwrap() {
            continue;
        }
        crate::plugin::store::write_record(home.path(), &record).unwrap();
        let folder = tempfile::tempdir().unwrap();
        if let Some(granted) = access(&case["granted"]) {
            crate::plugin::allowed::add(home.path(), folder.path(), granted).unwrap();
        }
        let mut input = serde_json::json!({ "path": folder.path() });
        if sets {
            input["output"] = serde_json::json!("out.json");
        }
        let binding = record.declaration.bind_dir.as_ref().unwrap();
        let call = access(&case["call"]).unwrap();
        assert_eq!(binding.call_access(&input), call, "{case}");
        let bound = crate::plugin::executor::PluginExecutor::new(Some(home.path().to_owned()))
            .bind_input(
                &record,
                &input,
                &crate::plugin::executor::BindContext::headless(None),
            )
            .await;
        match bound {
            Ok(Some(bound)) => {
                assert!(case["admitted"].as_bool().unwrap(), "{case}");
                assert_eq!(bound.access(), call, "{case}");
            }
            Ok(None) => panic!("the path field is set: {case}"),
            Err(reason) => {
                assert!(!case["admitted"].as_bool().unwrap(), "{case}: {reason}");
                assert!(reason.contains("gents plugin dirs add"), "{reason}");
            }
        }
    }
    tokio::fs::write(marker, b"24 cases verified")
        .await
        .unwrap();
}

// ---- the `gents::run_script` host import -----------------------------------

/// A guest that hands its whole stdin to `gents::run_script`, then copies the
/// host's answer with `gents::read_result`: to stdout on success, to stderr
/// with exit code 1 when the host answered a negative code.
pub(crate) const IMPORTING_WAT: &str = r#"(module
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

fn importing_plugin(declares: bool) -> PackPlugin {
    PackPlugin {
        name: "plugin".into(),
        description: "runs the call's script through the host".into(),
        artifact: PLUGIN_ARTIFACT.into(),
        language: "rust".into(),
        input_schema: serde_json::json!({"type": "object", "properties": {"script": {"type": "string"}}}),
        job: gents_protocol::job::PluginJobDeclaration(true),
        host_imports: if declares {
            vec![crate::pack::HostImport::RunScript]
        } else {
            Vec::new()
        },
        ..PackPlugin::default()
    }
}

fn importing_runner(declares: bool) -> Result<PluginRunner> {
    PluginRunner::compile(
        &build_plugin_afb(IMPORTING_WAT),
        &importing_plugin(declares),
    )
}

#[test]
fn a_declaring_plugin_gets_the_scripts_output_through_the_import() {
    let runner = importing_runner(true).expect("compiles");
    let arguments =
        serde_json::json!({"pin": null, "script": "module.exports = (i) => ({result: {n: 1}})"});
    let outcome = runner
        .call(&arguments, &PluginBudget::default())
        .expect("call runs");
    assert_eq!(
        outcome.verdict,
        PluginVerdict::Success,
        "{}",
        outcome.diagnostics
    );
    assert_eq!(outcome.output, serde_json::json!({"result": {"n": 1}}));
}

#[test]
fn a_call_without_a_script_gets_no_script_from_the_import() {
    let runner = importing_runner(true).expect("compiles");
    let outcome = runner
        .call(&serde_json::json!({"pin": null}), &PluginBudget::default())
        .expect("call runs");
    assert_eq!(outcome.verdict, PluginVerdict::Failed);
    assert!(
        outcome.diagnostics.contains("no script"),
        "{}",
        outcome.diagnostics
    );
}

#[test]
fn an_engine_failure_keeps_its_verdict_and_message() {
    let runner = importing_runner(true).expect("compiles");
    let broken = runner
        .call(
            &serde_json::json!({"pin": null, "script": "module.exports = (i) => {"}),
            &PluginBudget::default(),
        )
        .expect("call runs");
    assert_eq!(broken.verdict, PluginVerdict::Failed);
    assert!(
        broken
            .diagnostics
            .starts_with("the script does not compile:"),
        "{}",
        broken.diagnostics
    );
    assert!(
        !broken.diagnostics.contains("plugin exited with code"),
        "{}",
        broken.diagnostics
    );
    let looping = runner
        .call(
            &serde_json::json!({"pin": null, "script": "module.exports = (i) => { for (;;) {} }"}),
            &PluginBudget {
                wall_clock: std::time::Duration::from_millis(300),
                ..PluginBudget::default()
            },
        )
        .expect("call runs");
    assert!(
        matches!(
            looping.verdict,
            PluginVerdict::Timeout | PluginVerdict::OutOfFuel
        ),
        "{:?} {}",
        looping.verdict,
        looping.diagnostics
    );
}

#[test]
fn an_undeclared_plugin_that_imports_run_script_does_not_start() {
    let runner = importing_runner(false).expect("compiles: the import is checked at run time");
    let error = runner
        .call(
            &serde_json::json!({"pin": null, "script": "module.exports = (i) => i"}),
            &PluginBudget::default(),
        )
        .expect_err("an undeclared import is refused");
    let text = format!("{error:#}");
    assert!(
        text.contains("gents::run_script") || text.contains("run_script"),
        "{text}"
    );
}

/// Calls `run_script` once with its stdin, then again with an invalid pointer,
/// and writes whatever `read_result` returns after the second call.
const TWICE_WAT: &str = r#"(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "gents" "run_script" (func $run_script (param i32 i32) (result i32)))
  (import "gents" "read_result" (func $read_result (param i32 i32) (result i32)))
  (memory (export "memory") 4)
  (func (export "_start")
    (local $m i32)
    (i32.store (i32.const 1024) (i32.const 65536))
    (i32.store (i32.const 1028) (i32.const 65536))
    (drop (call $fd_read (i32.const 0) (i32.const 1024) (i32.const 1) (i32.const 1040)))
    (drop (call $run_script (i32.const 65536) (i32.load (i32.const 1040))))
    (drop (call $run_script (i32.const -1) (i32.const 1)))
    (local.set $m (call $read_result (i32.const 131072) (i32.const 65536)))
    (i32.store (i32.const 1032) (i32.const 131072))
    (i32.store (i32.const 1036) (local.get $m))
    (drop (call $fd_write (i32.const 1) (i32.const 1032) (i32.const 1) (i32.const 1048)))))
"#;

#[test]
fn a_failed_second_run_script_call_does_not_hand_back_the_first_answer() {
    let runner = PluginRunner::compile(&build_plugin_afb(TWICE_WAT), &importing_plugin(true))
        .expect("compiles");
    let outcome = runner
        .call(
            &serde_json::json!({"pin": null, "script": "module.exports = (i) => ({result: {n: 1}})"}),
            &PluginBudget::default(),
        )
        .expect("call runs");
    // The guest wrote the bytes stored after the failed call: a message, not the
    // first call's JSON answer.
    assert_ne!(outcome.output, serde_json::json!({"result": {"n": 1}}));
    assert_eq!(
        outcome.verdict,
        PluginVerdict::BadOutput,
        "{}",
        outcome.diagnostics
    );
}
