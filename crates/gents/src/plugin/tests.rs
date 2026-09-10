use super::*;
use crate::pack_archive::pack_dir;

fn wat(src: &str) -> Vec<u8> {
    wat::parse_str(src).expect("WAT parses")
}

/// The archive path a plugin's compiled `.afb` must live under, by
/// `crate::pack::PLUGIN_ARTIFACT_PREFIX`'s own convention.
const PLUGIN_ARTIFACT: &str = "plugins/plugin.afb";

/// Builds a real Afterburner `.afb` around a compiled Wasm module,
/// mirroring the minimal manifest `afterburner::afb_run`'s own tests use
/// (`minimal_manifest` in its `tests.rs`) so `PluginRunner` sees exactly
/// the shape `run_afb_bytes` dispatches through `run_wasm`: a
/// `precompiled/wasm32-wasip1/main.wasm` member and a `[runtime]
/// target = "wasm32-wasip1"`.
fn build_plugin_afb(wat_source: &str) -> Vec<u8> {
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
        metadata: toml::Table::new(),
        extra: toml::Table::new(),
    };
    let (bytes, _digest) = Builder::new(manifest, Manifold::sealed())
        .precompiled("precompiled/wasm32-wasip1/main.wasm", wat(wat_source))
        .build()
        .expect("build a real fixture .afb");
    bytes
}

/// Packs a one-plugin pack around a compiled `.afb`, so `PluginRunner`
/// is exercised exactly as an installed pack ships it.
fn build_plugin_pack(
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
const ECHO_WAT: &str = r#"
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

/// Rule 5: a plugin that loops forever is bounded by fuel, and the
/// exhaustion is its own verdict, not a generic failure.
#[test]
fn an_infinite_loop_exhausts_its_fuel_budget() {
    let wat_source = r#"
          (module
            (func (export "_start")
              (loop $forever
                br $forever)))
        "#;
    let (plugin, afb) = build_plugin_pack("fuel_pack", wat_source, None);
    let runner = PluginRunner::compile(&afb, &plugin).expect("compiles");

    let budget = PluginBudget {
        fuel: 10_000,
        ..PluginBudget::default()
    };
    let outcome = runner
        .call(&serde_json::json!({}), &budget)
        .expect("call reports fuel exhaustion, not a hard error");
    assert_eq!(outcome.verdict, PluginVerdict::OutOfFuel);
    assert_eq!(
        outcome.fuel_used, 10_000,
        "exhaustion means the whole budget was spent"
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
        fuel: 5_000_000_000,
        memory_bytes: PluginBudget::default().memory_bytes,
        wall_clock: std::time::Duration::from_millis(30),
    };
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
        metadata: toml::Table::new(),
        extra: toml::Table::new(),
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
