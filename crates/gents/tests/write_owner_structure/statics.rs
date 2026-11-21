use super::support::{
    cfg_test, item_cfg_test, parse_production, production_site_counts, production_sources,
    repo_root, shrinking_allowlist,
};
use syn::visit::{self, Visit};

/// Task-local statics that hold no request context, by file and name. The
/// rule is about what a request carries, so these are not counted, provided
/// the static's own docstring says what it holds instead.
const AMBIENT_NOT_REQUEST_CONTEXT: &[(&str, &str)] = &[
    ("crates/gents-cli/src/request_helpers.rs", "CAPTURED_REPORT"),
    (
        "crates/gents-store/src/config_client/txn/native.rs",
        "ACTIVE_EMBEDDED_TRANSACTION",
    ),
];

struct AmbientStatic {
    name: String,
    documented: bool,
}

/// Type names that make a `static` mutable or lazily initialised state
/// shared by the whole process; any `Atomic*` name counts too.
const PROCESS_STATE_TYPES: &[&str] = &[
    "Mutex",
    "RwLock",
    "OnceLock",
    "OnceCell",
    "Once",
    "LazyLock",
    "LazyCell",
    "Lazy",
    "Cell",
    "RefCell",
    "UnsafeCell",
    "Semaphore",
    "Notify",
    "ArcSwap",
    "ArcSwapOption",
    "DashMap",
];

fn names_process_state(ty: &syn::Type) -> bool {
    struct TypeNames(bool);
    impl<'ast> Visit<'ast> for TypeNames {
        fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
            let name = segment.ident.to_string();
            self.0 |= PROCESS_STATE_TYPES.contains(&name.as_str()) || name.starts_with("Atomic");
            visit::visit_path_segment(self, segment);
        }
    }
    let mut names = TypeNames(false);
    names.visit_type(ty);
    names.0
}

/// The production statics of one file: `thread_local!`/`task_local!` statics,
/// which hold per-thread or per-task context, and process-wide statics (a
/// `static` item whose type is interior-mutable or lazily initialised, every
/// `static mut`, and every `lazy_static!` static).
#[derive(Default)]
struct StaticVisitor {
    ambient: Vec<AmbientStatic>,
    process_wide: Vec<String>,
}

impl<'ast> Visit<'ast> for StaticVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        if matches!(item.mutability, syn::StaticMutability::Mut(_)) || names_process_state(&item.ty)
        {
            self.process_wide.push(item.ident.to_string());
        }
        visit::visit_item_static(self, item);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let Some(name) = invocation
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            return;
        };
        let local = name == "thread_local" || name == "task_local";
        if local || name == "lazy_static" {
            let tokens = invocation.tokens.clone().into_iter().collect::<Vec<_>>();
            let mut documented = false;
            for (index, token) in tokens.iter().enumerate() {
                match token {
                    proc_macro2::TokenTree::Punct(punct) if punct.as_char() == ';' => {
                        documented = false;
                    }
                    proc_macro2::TokenTree::Group(group) => {
                        documented |= matches!(
                            group.stream().into_iter().next(),
                            Some(proc_macro2::TokenTree::Ident(ident)) if ident == "doc"
                        );
                    }
                    proc_macro2::TokenTree::Ident(ident) if ident == "static" => {
                        let lifetime = index.checked_sub(1).is_some_and(|previous| {
                            matches!(&tokens[previous], proc_macro2::TokenTree::Punct(punct) if punct.as_char() == '\'')
                        });
                        if lifetime {
                            continue;
                        }
                        let name = tokens[index + 1..]
                            .iter()
                            .map(ToString::to_string)
                            .find(|word| word != "ref" && word != "mut")
                            .unwrap_or_default();
                        if local {
                            self.ambient.push(AmbientStatic { name, documented });
                        } else {
                            self.process_wide.push(name);
                        }
                    }
                    _ => {}
                }
            }
        }
        visit::visit_macro(self, invocation);
    }
}

fn production_statics(syntax: &syn::File) -> StaticVisitor {
    let mut visitor = StaticVisitor::default();
    visitor.visit_file(syntax);
    visitor
}

fn ambient_context_sites(relative: &str, syntax: &syn::File) -> usize {
    production_statics(syntax)
        .ambient
        .iter()
        .filter(|found| {
            !(found.documented
                && AMBIENT_NOT_REQUEST_CONTEXT
                    .iter()
                    .any(|(path, name)| *path == relative && *name == found.name))
        })
        .count()
}

fn process_static_sites(syntax: &syn::File) -> usize {
    production_statics(syntax).process_wide.len()
}

/// Request context (identity, signing, ACP bypass, transaction handles, the
/// requester or session, trace or request IDs) travels as explicit arguments
/// or fields. `tokio::task_local!` loses it across `tokio::spawn`, and Clippy's
/// `disallowed_macros` cannot be allowed at an item-position macro site, so
/// this test owns the ban: no production site remains and none is admitted.
#[test]
fn ambient_request_context_only_shrinks() {
    let root = repo_root();
    let violations = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter_map(|(relative, syntax)| {
            let sites = ambient_context_sites(&relative, &syntax);
            (sites > 0).then(|| {
                format!(
                    "{relative}: {sites} thread_local!/task_local! static(s); pass request context as an argument or field"
                )
            })
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "ambient request context is banned:\n{}",
        violations.join("\n")
    );
}

#[test]
fn ambient_context_counts_statics_outside_test_items() {
    let syntax = syn::parse_file(
        r#"
        tokio::task_local! { static A: &'static str; static B: u8; }
        fn f() { thread_local! { static C: u8 = const { 0 }; } }
        #[cfg(test)]
        tokio::task_local! { static D: u8; }
        #[cfg(test)]
        mod tests { thread_local! { static E: u8 = const { 0 }; } }
        "#,
    )
    .expect("parse snippet");
    assert_eq!(ambient_context_sites("src/lib.rs", &syntax), 3);
}

#[test]
fn ambient_context_skips_only_named_documented_statics() {
    let (path, name) = AMBIENT_NOT_REQUEST_CONTEXT[0];
    for (statics, listed, elsewhere) in [
        (format!("/// Why.\n static {name}: u8;"), 0, 1),
        (format!("static {name}: u8;"), 1, 1),
        (
            format!("/// Why.\n static {name}: u8; static OTHER: u8;"),
            1,
            2,
        ),
    ] {
        let source = format!("tokio::task_local! {{ {statics} }}");
        let syntax = syn::parse_file(&source).expect("parse snippet");
        assert_eq!(ambient_context_sites(path, &syntax), listed, "{source}");
        assert_eq!(
            ambient_context_sites("src/lib.rs", &syntax),
            elsewhere,
            "{source}"
        );
    }
}

/// Configuration and request context travel as arguments or as fields on the
/// owner that reaches the reader, never in process-wide mutable state: a
/// static is shared by every node and Agent in the process, so the last
/// writer wins for all of them, and a test that sets one leaks into every
/// other test in its binary. This counts each production `static` item whose
/// type is interior-mutable or lazily initialised (`PROCESS_STATE_TYPES`,
/// `Atomic*`), every `static mut` and every `lazy_static!` static; a plain
/// constant `static` is not counted. `thread_local!` and `task_local!` belong
/// to `ambient_request_context_only_shrinks`, which admits none. Entries name
/// why each remaining static is a genuine process singleton (an immutable
/// value built once, a process-global resource) or what owner replaces it;
/// the list only shrinks (#1764).
#[rustfmt::skip]
const PROCESS_STATIC_ALLOWLIST: &[(&str, usize, &str)] = &[
    ("apps/gents-desktop/src-tauri/src/bridge/browser.rs", 2, "the desktop window's one embedded browser and its applied-generation counter: a process-global UI resource; stays"),
    ("apps/gents-desktop/src-tauri/src/bridge/notifications.rs", 1, "count of notification permission waits in the one desktop process; stays"),
    ("apps/gents-desktop/src-tauri/src/bridge/windows.rs", 1, "next window label of the one desktop process; stays"),
    ("crates/gents-automation/src/lifecycle_hooks.rs", 2, "the bundled hook registry and composition built once: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-desktop-bridge/src/runtime_setup.rs", 1, "the desktop's one Tokio runtime, built at its entry point; stays"),
    ("crates/gents-desktop-bridge/src/tauri_commands/service_executable.rs", 2, "lock and attempt counter serialising installs of the one service executable a host has; process-global resource, stays"),
    ("crates/gents-eval/src/authoring/dossier.rs", 1, "runtime collection names derived from the schema: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-host/src/http/version.rs", 1, "the build's version response: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-host/src/p2p_status.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-inference/src/admission/config.rs", 1, "per-process HMAC key for credential fingerprints, never persisted by design; stays"),
    ("crates/gents-inference/src/clients/claude_messages/transport.rs", 1, "shared HTTP clients built once from the proxy environment, one keep-alive pool per proxy decision; stays"),
    ("crates/gents-lean-contract/src/lib.rs", 2, "the Lean process output cache and load lock in a crate that links only into tests; stays"),
    ("crates/gents-lean-vocab/src/lib.rs", 1, "the Lean contract snapshot in a crate that links only into tests: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-pack/src/pack/registry.rs", 1, "one registry download at a time per process, an external premise of the registry's throttle; stays"),
    ("crates/gents-pack/src/plugin/allowed.rs", 1, "embedded release digests: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-pack/src/plugin/allowed/protected.rs", 1, "the permission files of every home this process serves: a plugin of any node must mount none of them, so the guard is their union across the process, not one home's; stays"),
    ("crates/gents-pack/src/plugin/script_engine.rs", 1, "the script engine, compiled once: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-pack/src/plugin/ux/lint.rs", 10, "UX lint rules: compiled regular expressions, immutable once built; stay"),
    ("crates/gents-pack/src/plugin/wasi_runtime.rs", 1, "the WASI async runtime, a process-global resource built once; stays"),
    ("crates/gents-protocol/src/failure_text.rs", 5, "secret redaction patterns: compiled regular expressions, immutable once built; stay"),
    ("crates/gents-protocol/src/graphql/transport.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-session/src/exec/process/registry.rs", 2, "registry and id counter of managed child processes, which belong to the process; stays"),
    ("crates/gents-session/src/request/admission.rs", 1, "the default request input: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-session/src/session/output/replay_capture.rs", 1, "log rate limiter for replay capture; stays"),
    ("crates/gents-store/src/config_client/common.rs", 1, "monotonic recreate-identity clock, process-local by design; stays"),
    ("crates/gents-store/src/config_client/graphql.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-store/src/config_client/runtime_endpoints.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-store/src/config_client/subscription.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-store/src/identity/request_receipt.rs", 1, "the default request input: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-store/src/node_extension.rs", 1, "the node extension table, the one owner of per-node attachments and per-node state keyed by node address; stays"),
    ("crates/gents-store/src/schema/catalog.rs", 4, "baseline schemas and the runtime catalog: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-store/src/schema/catalog/install.rs", 1, "lock serialising schema ensure across the desktop and runtime paths of one process; stays"),
    ("crates/gents-store/src/schema/journal.rs", 1, "journal baseline collections: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-store/src/service.rs", 1, "the build identifier: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-toolcalls/src/call/lifecycle/recovery.rs", 1, "rate limit of the unresolved-parent warning; stays"),
    ("crates/gents-toolcalls/src/plugin/executor.rs", 1, "a built-in plugin declaration: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-tools/src/builtin/self_config/configuration_discovery/text.rs", 5, "secret redaction patterns: compiled regular expressions, immutable once built; stay"),
    ("crates/gents-tools/src/native/command_observability.rs", 2, "process-wide command policy counters for metrics; stay"),
    ("crates/gents/src/client_runtime/mod.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
];

#[test]
fn process_wide_statics_only_shrink() {
    shrinking_allowlist(
        "process-wide static",
        PROCESS_STATIC_ALLOWLIST,
        &production_site_counts(|_, syntax| process_static_sites(syntax)),
        "process-wide static(s)",
        "pass the value as an argument or hold it on its owner",
    );
}

#[test]
fn process_statics_count_interior_mutable_statics_outside_test_items() {
    let syntax = syn::parse_file(
        r##"
        static A: std::sync::Mutex<u8> = std::sync::Mutex::new(0);
        static B: AtomicU64 = AtomicU64::new(0);
        static mut C: u8 = 0;
        fn f() { static D: OnceLock<Arc<u8>> = OnceLock::new(); }
        static E: std::sync::LazyLock<Option<RwLock<u8>>> = std::sync::LazyLock::new(|| None);
        lazy_static::lazy_static! { static ref F: u8 = 0; }
        static G: &[&str] = &["Mutex"];
        static H: u64 = 0;
        const I: std::sync::Mutex<u8> = std::sync::Mutex::new(0);
        fn g() { let _ = r#"static J: RwLock<u8> = RwLock::new(0);"#; }
        thread_local! { static K: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }
        #[cfg(test)]
        static L: OnceLock<u8> = OnceLock::new();
        #[cfg(all(test, unix))]
        mod tests { static M: AtomicBool = AtomicBool::new(false); }
        impl S { #[cfg(test)] fn f() { static N: OnceLock<u8> = OnceLock::new(); } }
        "##,
    )
    .expect("parse snippet");
    assert_eq!(process_static_sites(&syntax), 6);
    assert_eq!(ambient_context_sites("src/lib.rs", &syntax), 1);
}
