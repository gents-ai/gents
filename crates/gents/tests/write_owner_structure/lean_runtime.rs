use super::support::{all_rust_sources, macro_expressions, no_sites, repo_root};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, ExprMethodCall, ItemFn};

/// `gents_lean_contract` entry points that build or run the Lean model with
/// `lake`, synchronously, for as long as the machine takes.
const LEAN_GENERATOR_RUNS: &[&str] = &[
    "load_contract_json",
    "load_contract_snapshot",
    "load_contract_stdout",
    "run_contract_generator",
    "run_lake_build",
    "run_prepared_generator",
    "run_prepared_generator_with_input",
];

struct LeanGeneratorVisitor {
    imports_owner: bool,
    in_async: bool,
    sites: usize,
}

impl LeanGeneratorVisitor {
    fn off_runtime<F>(&mut self, visit_arguments: F)
    where
        F: FnOnce(&mut Self),
    {
        let in_async = std::mem::replace(&mut self.in_async, false);
        visit_arguments(self);
        self.in_async = in_async;
    }

    fn with_async<F>(&mut self, asynchronous: bool, visit_body: F)
    where
        F: FnOnce(&mut Self),
    {
        let in_async = std::mem::replace(&mut self.in_async, asynchronous);
        visit_body(self);
        self.in_async = in_async;
    }
}

impl<'ast> Visit<'ast> for LeanGeneratorVisitor {
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.with_async(item.sig.asyncness.is_some(), |visitor| {
            visit::visit_item_fn(visitor, item);
        });
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.with_async(item.sig.asyncness.is_some(), |visitor| {
            visit::visit_impl_item_fn(visitor, item);
        });
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        self.with_async(item.sig.asyncness.is_some(), |visitor| {
            visit::visit_trait_item_fn(visitor, item);
        });
    }

    fn visit_expr_async(&mut self, block: &'ast syn::ExprAsync) {
        self.with_async(true, |visitor| visit::visit_expr_async(visitor, block));
    }

    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        let asynchronous = self.in_async || closure.asyncness.is_some();
        self.with_async(asynchronous, |visitor| {
            visit::visit_expr_closure(visitor, closure);
        });
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        let Expr::Path(function) = call.func.as_ref() else {
            visit::visit_expr_call(self, call);
            return;
        };
        let segments = &function.path.segments;
        let last = segments.last().map(|segment| segment.ident.to_string());
        let leaves_runtime = last.as_deref() == Some("spawn_blocking")
            || (last.as_deref() == Some("spawn")
                && segments.iter().any(|segment| segment.ident == "thread"));
        if leaves_runtime {
            self.visit_expr(&call.func);
            self.off_runtime(|visitor| {
                for argument in &call.args {
                    visitor.visit_expr(argument);
                }
            });
            return;
        }
        let names_owner = segments
            .iter()
            .any(|segment| segment.ident == "gents_lean_contract")
            || (self.imports_owner && segments.len() == 1);
        if self.in_async
            && names_owner
            && last
                .as_deref()
                .is_some_and(|name| LEAN_GENERATOR_RUNS.contains(&name))
        {
            self.sites += 1;
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if call.method == "spawn_blocking" {
            self.visit_expr(&call.receiver);
            self.off_runtime(|visitor| {
                for argument in &call.args {
                    visitor.visit_expr(argument);
                }
            });
            return;
        }
        visit::visit_expr_method_call(self, call);
    }
}

fn lean_generator_runtime_sites(source: &str, syntax: &syn::File) -> usize {
    let mut visitor = LeanGeneratorVisitor {
        imports_owner: source.contains("use gents_lean_contract::"),
        in_async: false,
        sites: 0,
    };
    visitor.visit_file(syntax);
    visitor.sites
}

/// No async body runs a Lean generator on its runtime thread. On a
/// `#[tokio::test]` runtime that thread is the only worker: a body holding an
/// execution lease starves its renewal for the whole run
/// (`LeaseRenewalStarvation.tla`, `SyncInsideLease`). Code awaits
/// `gents_lean_vocab::snapshot()`, which loads off the runtime, or runs the
/// generator inside `spawn_blocking`.
#[test]
fn lean_generator_runs_stay_off_async_runtime_threads() {
    let root = repo_root();
    let mut sources = Vec::new();
    for directory in ["crates", "apps"] {
        all_rust_sources(&root.join(directory), &mut sources);
    }
    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(&root)
                .expect("source under repo root")
                .to_string_lossy()
                .into_owned();
            if relative.starts_with("crates/gents-lean-contract/src/") {
                return None;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            if !source.contains("gents_lean_contract") {
                return None;
            }
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let sites = lean_generator_runtime_sites(&source, &syntax);
            (sites > 0).then_some((relative, sites))
        })
        .collect::<BTreeMap<_, _>>();
    no_sites(
        "Lean generator runtime",
        &actual,
        "Lean generator run(s) on an async runtime thread",
        "await gents_lean_vocab::snapshot() or run it inside spawn_blocking",
    );
}

#[test]
fn lean_generator_fence_counts_async_bodies_only() {
    for (source, sites) in [
        (
            "async fn f() { gents_lean_contract::run_prepared_generator(d, t, f); }",
            1,
        ),
        (
            "fn f() { gents_lean_contract::run_prepared_generator(d, t, f); }",
            0,
        ),
        (
            "fn f() { let _ = async { gents_lean_contract::load_contract_json() }; }",
            1,
        ),
        (
            "async fn f() { tokio::task::spawn_blocking(|| gents_lean_contract::load_contract_json()).await; }",
            0,
        ),
        (
            "async fn f() { std::thread::spawn(move || gents_lean_contract::run_lake_build(d)); }",
            0,
        ),
        (
            "async fn f() { fn g() { gents_lean_contract::run_lake_build(d); } }",
            0,
        ),
        (
            "async fn f() { let g = || gents_lean_contract::run_lake_build(d); g(); }",
            1,
        ),
        ("async fn f() { gents_lean_contract::proofs_dir(); }", 0),
        (
            "use gents_lean_contract::run_lake_build;\nasync fn f() { run_lake_build(d); }",
            1,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(
            lean_generator_runtime_sites(source, &syntax),
            sites,
            "{source}"
        );
    }
}

/// Calls that start or extend a request's execution lease and its renewal task
/// on the calling runtime, directly or through a test fixture.
const LEASE_STARTS: &[&str] = &[
    "begin_owned_execution",
    "bind_interruptible_request",
    "claim_at",
    "claim_with_identity",
    "fixture_with_ids",
];

/// Counts synchronous `gents_lean_vocab` reads that an async body makes after
/// starting a lease without having awaited `gents_lean_vocab::snapshot()` or
/// read the model before the lease.
/// Such a read can be the process's first, which runs Lean on the runtime
/// thread for minutes and starves the lease renewal (#3704, #3891, #3892).
struct LeanReadAfterLeaseVisitor {
    starts: BTreeSet<String>,
    events: Vec<(usize, usize, LeanEvent)>,
    sites: usize,
}

/// The names of one file's calls that start a lease: [`LEASE_STARTS`] and,
/// to a fixed point, every function of the file whose body calls one, so a
/// file-local fixture that claims counts at its call site (`step0` over
/// `job_chain` in `plugin_job_dispatch.rs`).
fn lease_starts_in_file(syntax: &syn::File) -> BTreeSet<String> {
    #[derive(Default)]
    struct Callees {
        current: Option<String>,
        functions: Vec<(String, BTreeSet<String>)>,
    }
    impl Callees {
        fn enter(&mut self, name: String) -> Option<String> {
            self.functions.push((name.clone(), BTreeSet::new()));
            self.current.replace(name)
        }
        fn called(&mut self, name: String) {
            if self.current.is_some() {
                if let Some((_, calls)) = self.functions.last_mut() {
                    calls.insert(name);
                }
            }
        }
    }
    impl<'ast> Visit<'ast> for Callees {
        fn visit_item_fn(&mut self, item: &'ast ItemFn) {
            let outer = self.enter(item.sig.ident.to_string());
            visit::visit_item_fn(self, item);
            self.current = outer;
        }
        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            let outer = self.enter(item.sig.ident.to_string());
            visit::visit_impl_item_fn(self, item);
            self.current = outer;
        }
        fn visit_expr_call(&mut self, call: &'ast ExprCall) {
            if let Expr::Path(function) = call.func.as_ref() {
                if let Some(last) = function.path.segments.last() {
                    self.called(last.ident.to_string());
                }
            }
            visit::visit_expr_call(self, call);
        }
        fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
            self.called(call.method.to_string());
            visit::visit_expr_method_call(self, call);
        }
        fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
            let (mut parsed, mut loose) = (Vec::new(), Vec::new());
            macro_expressions(invocation.tokens.clone(), &mut parsed, &mut loose);
            for argument in &parsed {
                self.visit_expr(argument);
            }
        }
    }
    let mut callees = Callees::default();
    callees.visit_file(syntax);
    let mut starts = LEASE_STARTS
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    loop {
        let before = starts.len();
        for (name, calls) in &callees.functions {
            if calls.iter().any(|call| starts.contains(call)) {
                starts.insert(name.clone());
            }
        }
        if starts.len() == before {
            return starts;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LeanEvent {
    LeaseStart,
    SnapshotAwait,
    SyncRead,
}

impl LeanReadAfterLeaseVisitor {
    fn record(&mut self, span: proc_macro2::Span, event: LeanEvent) {
        let start = span.start();
        self.events.push((start.line, start.column, event));
    }

    fn settle(&mut self) {
        self.events
            .sort_by_key(|(line, column, _)| (*line, *column));
        let (mut leased, mut loaded) = (false, false);
        for (_, _, event) in self.events.drain(..) {
            match event {
                LeanEvent::LeaseStart => leased = true,
                LeanEvent::SnapshotAwait => loaded = true,
                LeanEvent::SyncRead if leased && !loaded => self.sites += 1,
                // A read before any lease loaded the snapshot already.
                LeanEvent::SyncRead => loaded = true,
            }
        }
    }
}

impl<'ast> Visit<'ast> for LeanReadAfterLeaseVisitor {
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        let outer = std::mem::take(&mut self.events);
        visit::visit_item_fn(self, item);
        if item.sig.asyncness.is_some() {
            self.settle();
        }
        self.events = outer;
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            let segments = &function.path.segments;
            let last = segments.last().map(|segment| segment.ident.to_string());
            if segments
                .iter()
                .any(|segment| segment.ident == "gents_lean_vocab")
            {
                let event = if last.as_deref() == Some("snapshot") {
                    LeanEvent::SnapshotAwait
                } else {
                    LeanEvent::SyncRead
                };
                self.record(call.span(), event);
            } else if last.as_ref().is_some_and(|name| self.starts.contains(name)) {
                self.record(call.span(), LeanEvent::LeaseStart);
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if self.starts.contains(&call.method.to_string()) {
            self.record(call.method.span(), LeanEvent::LeaseStart);
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let (mut parsed, mut loose) = (Vec::new(), Vec::new());
        macro_expressions(invocation.tokens.clone(), &mut parsed, &mut loose);
        for argument in &parsed {
            self.visit_expr(argument);
        }
    }
}

fn lean_reads_after_lease(syntax: &syn::File) -> usize {
    let mut visitor = LeanReadAfterLeaseVisitor {
        starts: lease_starts_in_file(syntax),
        events: Vec::new(),
        sites: 0,
    };
    visitor.visit_file(syntax);
    visitor.sites
}

/// An async test that holds an execution lease awaits
/// `gents_lean_vocab::snapshot()` before any synchronous model read, so the
/// first load runs off its runtime (`LeaseRenewalStarvation.tla`,
/// `OffRuntime`). Syntactic: fixtures are matched by name, a file's own
/// functions that reach a lease start count as one, and macro input is read
/// as [`macro_expressions`] reads it.
#[test]
fn lease_holders_await_the_lean_snapshot() {
    let root = repo_root();
    let mut sources = Vec::new();
    for directory in ["crates", "apps"] {
        all_rust_sources(&root.join(directory), &mut sources);
    }
    let violations = sources
        .into_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            if !source.contains("gents_lean_vocab") {
                return None;
            }
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let sites = lean_reads_after_lease(&syntax);
            (sites > 0).then(|| {
                format!(
                    "{}: {sites} synchronous gents_lean_vocab read(s) after a lease starts; await gents_lean_vocab::snapshot() first",
                    path.strip_prefix(&root).expect("source under repo root").display()
                )
            })
        })
        .collect::<Vec<_>>();
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn lean_read_fence_counts_reads_after_a_lease_without_the_await() {
    for (source, sites) in [
        (
            "async fn f() { l.claim_with_identity().await; gents_lean_vocab::lean_contract_snapshot(); }",
            1,
        ),
        (
            "async fn f() { gents_lean_vocab::lean_contract_snapshot(); l.claim_with_identity().await; }",
            0,
        ),
        (
            "async fn f() { fixture_with_ids(\"x\").await; gents_lean_vocab::snapshot().await; gents_lean_vocab::families::k::cases(); }",
            0,
        ),
        (
            "async fn f() { fixture_with_ids(\"x\").await; for c in gents_lean_vocab::families::k::cases() {} }",
            1,
        ),
        (
            "fn f() { l.claim_with_identity(); gents_lean_vocab::lean_contract_snapshot(); }",
            0,
        ),
        (
            "async fn chain() { l.claim_with_identity().await; }\nasync fn step0() { chain().await; }\nasync fn f() { step0().await; for c in gents_lean_vocab::families::k::cases() {} }",
            1,
        ),
        (
            "async fn step0() { l.claim_with_identity().await; }\nasync fn f() { gents_lean_vocab::snapshot().await; step0().await; gents_lean_vocab::families::k::cases(); }",
            0,
        ),
        (
            "async fn chain() { assert_eq!(l.claim_with_identity().await, Claimed); }\nasync fn f() { chain().await; assert!(gents_lean_vocab::families::k::cases().is_empty()); }",
            1,
        ),
        (
            "async fn f() { gents_lean_vocab::lean_contract_snapshot(); l.claim_with_identity().await; gents_lean_vocab::lean_contract_snapshot(); }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(lean_reads_after_lease(&syntax), sites, "{source}");
    }
}
