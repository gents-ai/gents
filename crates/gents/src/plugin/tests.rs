use super::*;
use crate::pack_archive::pack_dir;

fn wat(src: &str) -> Vec<u8> {
    wat::parse_str(src).expect("WAT parses")
}

/// The archive path a plugin's compiled `.afb` must live under, by
/// `crate::pack::PLUGIN_ARTIFACT_PREFIX`'s own convention.
const PLUGIN_ARTIFACT: &str = "plugins/plugin.afb";

/// A guest that ignores stdin and writes exactly `json` to stdout: the
/// prepare plugin fixture for tests that only need a deterministic
/// result, not a real transformation of the host facts.
pub(crate) fn constant_output_wat(json: &[u8]) -> String {
    let mut escaped = String::with_capacity(json.len() * 4);
    for byte in json {
        escaped.push_str(&format!("\\{byte:02x}"));
    }
    let len = json.len();
    format!(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_write"
(func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{escaped}")
  (func (export "_start")
(i32.store (i32.const 8192) (i32.const 0))
(i32.store (i32.const 8196) (i32.const {len}))
(call $fd_write (i32.const 1) (i32.const 8192) (i32.const 1) (i32.const 8200))
drop))
"#
    )
}

/// Builds a real Afterburner `.afb` around a compiled Wasm module,
/// mirroring the minimal manifest `afterburner::afb_run`'s own tests use
/// (`minimal_manifest` in its `tests.rs`) so `PluginRunner` sees exactly
/// the shape `run_afb_bytes` dispatches through `run_wasm`: a
/// `precompiled/wasm32-wasip1/main.wasm` member and a `[runtime]
/// target = "wasm32-wasip1"`.
pub(crate) fn build_plugin_afb(wat_source: &str) -> Vec<u8> {
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
        .precompiled("precompiled/wasm32-wasip1/main.wasm", wat(wat_source))
        .build()
        .expect("build a real fixture .afb");
    bytes
}

/// Packs a one-plugin pack around a compiled `.afb`, so `PluginRunner`
/// is exercised exactly as an installed pack ships it.
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

/// Reads fd 0 in one shot and writes exactly what it read to fd 1:
/// the identity plugin, and the vehicle for every "does the ABI carry
/// arguments through" test below.
pub(crate) const ECHO_WAT: &str = r#"
      (module
        (import "wasi_snapshot_preview1" "fd_read"
          (func $fd_read (param i32 i32 i32 i32) (result i32)))
        (import "wasi_snapshot_preview1" "fd_write"
          (func $fd_write (param i32 i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "_start")
          ;; iovec at 256: buf=0, buf_len=256
          i32.const 256  i32.const 0    i32.store
          i32.const 260  i32.const 256  i32.store
          ;; fd_read(fd=0, iovs_ptr=256, iovs_len=1, nread_ptr=264)
          i32.const 0
          i32.const 256
          i32.const 1
          i32.const 264
          call $fd_read
          drop
          ;; reuse the iovec, buf_len = bytes actually read
          i32.const 260  i32.const 264 i32.load  i32.store
          ;; fd_write(fd=1, iovs_ptr=256, iovs_len=1, nwritten_ptr=268)
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

/// Rule 1: every call gets a fresh instance, so two calls on the same
/// compiled runner never see each other's arguments.
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

/// Rule 6: stdout that is not a single JSON value is `BadOutput`,
/// naming why, never a silent empty result.
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

/// A non-zero exit is a failure even when stdout is valid JSON: the exit
/// code is the plugin's own verdict on the call.
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

/// Rule 5: a plugin that loops forever is bounded by an explicit fuel
/// ceiling, and the exhaustion is its own verdict, not a generic failure.
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

/// The default budget sets no fuel ceiling at all: a guest that burns far
/// more than Afterburner's own 100-million-instruction family default
/// still completes, bounded only by the wall clock. This is the regression
/// that matters: a real plugin (a source-scanning pre-pass) measured at
/// roughly 166 fuel per byte plus a 25 million fixed cost needs on the
/// order of 6.5 billion fuel for a 39 MB input, which the old fixed
/// default failed well under a megabyte in.
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

/// Rule 5: `budget.wall_clock` is a real, independent bound, not
/// merely a description of the fuel bound. Fuel is set far larger than
/// what could exhaust within the wall-clock deadline (so the deadline
/// reliably wins the race) but still finite (so the detached
/// background thread this call leaves running eventually exhausts its
/// own fuel and exits, rather than spinning forever).
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

/// The same bound, with no fuel ceiling at all: the wall clock is the only
/// backstop an unlimited-fuel budget has against a guest that never
/// returns, and it must still fire.
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

/// Rule 2: a plugin that declared no manifold gets no capabilities. Fd
/// 3 only exists when a preopen was configured; this runner never
/// configures one, so the guest observes a `fd_write` failure on it
/// regardless of what it might have wanted.
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

/// This runner's ceiling narrows even a wide-open declared manifold
/// to nothing, because nothing beyond stdin/stdout/stderr crosses
/// this plugin ABI today (`PluginRunner::compile`'s own doc).
#[test]
fn a_wide_declared_manifold_is_still_narrowed_to_this_runners_ceiling() {
    let wat_source = r#"(module (func (export "_start")))"#;
    let manifold = serde_json::json!({
        "fs": {"ReadWrite": ["/data"]},
        "net": {"OutboundFull": null},
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

/// A source-only fixture `.afb` for `language`, carrying `entry` and
/// nothing precompiled - the shape an interpreted package takes before
/// `burn compile` has produced anything for it.
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

/// A plugin declaration pointing at [`PLUGIN_ARTIFACT`], for a fixture that
/// supplies its own `.afb` bytes.
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
    }
}

/// Rule 4, the coordinating requirement this module exists to enforce:
/// a plugin whose real artifact would run through a dispatch path that
/// cannot honour the bounds a call applies is refused at admission, with a
/// reason naming what would not be enforced, never silently run.
///
/// Ruby source is that path: its runner exposes no fuel, memory, stdin or
/// manifold override at all, so every axis a call asks for would be
/// dropped.
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

/// A default budget is raised to what the artifact needs to start at all.
///
/// This is the other half of admitting Python. Admission says the bounds
/// are enforceable; this says the default ones are survivable. Handing a
/// Pyodide-backed plugin `PluginBudget::default()` used to fail before its
/// first line: CPython's linear memory will not instantiate under 64 MiB.
/// The default no longer carries a fuel ceiling at all, so only the memory
/// floor and the wall clock need raising.
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

    // A plugin that declares nothing gets exactly `for_artifact`'s answer.
    let no_limits = plugin_named("rs_plugin", "rust");
    assert_eq!(
        PluginBudget::for_plugin(&wasi, &no_limits).memory_bytes,
        PluginBudget::for_artifact(&wasi).memory_bytes
    );
}

/// The literal prefix of the JSON this guest writes: `{"filler":"`. Kept as
/// bytes (not a WAT text literal) because embedding an unescaped `"` inside
/// a WAT string would terminate it early; every byte is written with its
/// own `i32.store8` instead.
const LARGE_OUTPUT_PREFIX: &[u8] = b"{\"filler\":\"";
const LARGE_OUTPUT_SUFFIX: &[u8] = b"\"}";

/// WAT for a guest that writes exactly `total_len` bytes of one valid JSON
/// value to stdout: `{"filler":"aaa...a"}`, the `a`s filled in bulk with
/// `memory.fill` rather than a giant literal in the module source. Used to
/// prove `limits.max_output_mib` actually changes what a real call accepts,
/// not just the arithmetic in [`PluginBudget::for_plugin`].
fn large_json_output_wat(total_len: usize) -> String {
    let envelope_len = LARGE_OUTPUT_PREFIX.len() + LARGE_OUTPUT_SUFFIX.len();
    assert!(total_len > envelope_len, "need room for the JSON envelope");
    let filler_len = total_len - envelope_len;
    let suffix_offset = total_len - LARGE_OUTPUT_SUFFIX.len();
    let iovec_offset = total_len;
    let nwritten_offset = iovec_offset + 8;
    let pages = (nwritten_offset + 4).div_ceil(65536);

    let mut stores = String::new();
    for (offset, byte) in LARGE_OUTPUT_PREFIX.iter().enumerate() {
        stores.push_str(&format!(
            "    (i32.store8 (i32.const {offset}) (i32.const {byte}))\n"
        ));
    }
    for (index, byte) in LARGE_OUTPUT_SUFFIX.iter().enumerate() {
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
        prefix_len = LARGE_OUTPUT_PREFIX.len(),
        iovec_len_offset = iovec_offset + 4,
    )
}

/// The real bound end to end: a guest that actually writes past the default
/// 1 MiB output cap is `BadOutput` under the default budget, and succeeds
/// once `limits.max_output_mib` raises it - proving the declared limit
/// changes what a real call accepts, not only what [`PluginBudget::for_plugin`]
/// computes on paper.
#[test]
fn a_call_over_the_default_output_cap_succeeds_once_the_declared_limit_covers_it() {
    const TOTAL_LEN: usize = 1_500_000; // > default 1 MiB, <= a declared 2 MiB limit
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

/// WAT for a guest that writes one 64 KiB chunk to stdout in a loop until it
/// overflows [`super::STDOUT_CAPTURE_PIPE_BYTES`], the real fixed capture
/// pipe every compiled plugin's stdout runs through regardless of what its
/// own budget declares. `chunks * 65536` must exceed the pipe; the write
/// that crosses it traps.
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

/// The blocker this test pins: a guest that fills the host's fixed 4 MiB
/// stdout capture pipe before it finishes traps inside `wasmtime-wasi`'s
/// preview1 shim (`wasm trap: wasm unreachable instruction executed`), not
/// because it wrote anything invalid. Before this was classified, that trap
/// surfaced as an opaque hard `Err`, indistinguishable from a genuine guest
/// crash, and never reached `describe_prepare_plugin_failure`'s "narrow
/// base..head" message. It must come back as `BadOutput` instead.
#[test]
fn a_trap_that_fills_the_stdout_capture_pipe_is_bad_output_not_a_hard_error() {
    // 4 MiB / 64 KiB = 64 whole chunks fit exactly; the 65th overflows.
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

/// A guest that traps having written almost nothing (a genuine crash, not
/// an output-capacity problem) must stay a hard `Err`: reclassifying every
/// trap as `BadOutput` would hide a real bug behind an output-size message.
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

/// The same gate, the other way round: Python is admitted, because its
/// dispatch path really does enforce every axis a call asks for.
///
/// This is the regression that matters. The gate used to be a hand-written
/// table of language names in this file, and it went on refusing Python
/// long after Python's bounds had been wired - a language that ran fully
/// contained reported as unsafe. Asking
/// `afterburner::afb_run::bounds_for` is what makes that impossible, and
/// this test fails if anything reintroduces a second copy of the answer.
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
    };
    let error = PluginRunner::compile(b"not an afb", &plugin).expect_err("must be refused");
    assert!(format!("{error:#}").contains("broken"), "{error:#}");
}

// ---- narrow_manifold: the intersection rule from (2) --------------

#[test]
fn an_admitting_caller_can_widen_the_ceiling_but_never_past_the_plugin() {
    use afterburner_core::manifold::FsAccess;

    // A plugin that asked to read a workspace gets nothing under the
    // sealed default, because nothing has admitted it yet.
    let sealed = narrow_manifold(
        &Manifold {
            fs: FsAccess::ReadOnly(vec!["/workspace".into()]),
            ..Manifold::sealed()
        },
        &PluginRunner::CEILING,
    );
    assert!(matches!(sealed.fs, FsAccess::None));

    // An operator that allows the workspace admits exactly what the
    // plugin asked for, and no more.
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

#[cfg(target_os = "macos")]
#[test]
fn masked_trap_startup_uses_the_plugin_request_engine() {
    let startup = start_wasm_trap_handler_with_signals_blocked()
        .unwrap()
        .unwrap();
    let request = plugin_run_request(Vec::new(), Manifold::sealed(), &PluginBudget::default());
    let selected = afterburner::wasi::embedder_vm::shared_epoch_vm_with(request.nan_mode).unwrap();
    assert!(std::ptr::eq(startup, selected));
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
