use super::support::{all_rust_sources, repo_root};
use std::collections::BTreeSet;
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, ExprMethodCall, ItemFn};

/// Functions that create a file in-process (`fs::write`, `fs::copy`,
/// `File::create`, an `OpenOptions` open with `create`) and then give that
/// path an exec bit, or that `fs::copy` an executable (a source naming a
/// binary, which the copy keeps executable). Linux execve refuses a file any
/// process holds open for writing (ETXTBSY), and a concurrent spawn copies
/// this process's write descriptor into its child until that child execs,
/// even with O_CLOEXEC (`tla/ScriptFixtureExec.tla`). A static script is
/// checked in; a generated one goes through
/// `gents_store::test_support::write_executable_script` and a copied one
/// through `gents_store::test_support::copy_executable`. No site
/// remains and none is admitted.
#[derive(Default)]
struct ExecutableWriteScan {
    written: BTreeSet<String>,
    permitted: Vec<String>,
    executable_mode: bool,
    executable_copy: bool,
}

/// A copy source that names a binary: Cargo's `CARGO_BIN_EXE_*`,
/// `current_exe`, or a `*_bin`/`*_exe`/`executable*` binding or helper.
static EXECUTABLE_SOURCE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\b(CARGO_BIN_EXE\w*|current_exe|\w+_bin|\w+_exe|\w*executable\w*)\b")
        .expect("executable source pattern")
});

fn method_chain_calls(expression: &Expr, names: &[&str]) -> bool {
    match expression {
        Expr::MethodCall(call) => {
            names.iter().any(|name| call.method == name)
                || method_chain_calls(&call.receiver, names)
        }
        _ => false,
    }
}

fn path_argument_key(expression: &Expr) -> String {
    match expression {
        Expr::Reference(reference) => path_argument_key(&reference.expr),
        Expr::Paren(inner) => path_argument_key(&inner.expr),
        other => quote::ToTokens::to_token_stream(other).to_string(),
    }
}

fn executable_literal(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(literal), .. })
            if literal.base10_parse::<u32>().is_ok_and(|mode| mode & 0o111 != 0)
    )
}

impl<'ast> Visit<'ast> for ExecutableWriteScan {
    fn visit_item_fn(&mut self, _: &'ast ItemFn) {}

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(path) = call.func.as_ref() {
            let segments = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            let first = call.args.first();
            match segments.as_slice() {
                [.., fs, write] if fs == "fs" && write == "write" => {
                    self.written.extend(first.map(path_argument_key));
                }
                [.., fs, copy] if fs == "fs" && copy == "copy" => {
                    self.written
                        .extend(call.args.iter().nth(1).map(path_argument_key));
                    self.executable_copy |= first.is_some_and(|source| {
                        EXECUTABLE_SOURCE.is_match(&path_argument_key(source))
                    });
                }
                [.., file, create]
                    if file == "File" && (create == "create" || create == "create_new") =>
                {
                    self.written.extend(first.map(path_argument_key));
                }
                [.., set] if set == "set_permissions" => {
                    self.permitted.extend(first.map(path_argument_key));
                }
                [.., from_mode] if from_mode == "from_mode" => {
                    self.executable_mode |= first.is_some_and(executable_literal);
                }
                _ => {}
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if call.method == "set_mode" {
            self.executable_mode |= call.args.first().is_some_and(executable_literal);
        }
        if call.method == "open" && method_chain_calls(&call.receiver, &["create", "create_new"]) {
            self.written
                .extend(call.args.first().map(path_argument_key));
        }
        visit::visit_expr_method_call(self, call);
    }
}

#[derive(Default)]
struct ExecutableWriteVisitor {
    sites: usize,
}

impl ExecutableWriteVisitor {
    fn scan(&mut self, block: &syn::Block) {
        let mut scan = ExecutableWriteScan::default();
        scan.visit_block(block);
        if scan.executable_copy
            || scan.executable_mode
                && scan
                    .permitted
                    .iter()
                    .any(|path| scan.written.contains(path))
        {
            self.sites += 1;
        }
    }
}

impl<'ast> Visit<'ast> for ExecutableWriteVisitor {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        self.scan(&function.block);
        visit::visit_item_fn(self, function);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
        self.scan(&function.block);
        visit::visit_impl_item_fn(self, function);
    }
}

fn executable_write_sites(syntax: &syn::File) -> usize {
    let mut visitor = ExecutableWriteVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn executables_are_never_written_in_process() {
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
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let sites = executable_write_sites(&syntax);
            (sites > 0).then(|| {
                let relative = path
                    .strip_prefix(&root)
                    .expect("source under repo root")
                    .to_string_lossy()
                    .into_owned();
                format!(
                    "{relative}: {sites} function(s) write a file and then make it executable; check in a static script or use gents_store::test_support::write_executable_script"
                )
            })
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "executable write ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn executable_write_fence_pairs_a_write_with_an_exec_bit_on_its_path() {
    for (source, sites) in [
        (
            "fn f() { std::fs::write(&p, b\"x\").unwrap(); std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); }",
            1,
        ),
        (
            "fn f() { fs::write(&p, b\"x\")?; let mut m = fs::metadata(&p)?.permissions(); m.set_mode(0o700); fs::set_permissions(p, m)?; }",
            1,
        ),
        (
            "fn f() { let stub = |body: &str| { std::fs::write(&b, body).unwrap(); std::fs::set_permissions(&b, Permissions::from_mode(0o751)).unwrap(); }; }",
            1,
        ),
        (
            "impl S { fn f(&self) { fs::write(&p, b\"x\")?; fs::set_permissions(&p, Permissions::from_mode(0o755))?; } }",
            1,
        ),
        (
            "fn f() { std::fs::write(&p, b\"x\").unwrap(); std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap(); }",
            0,
        ),
        (
            "fn f() { std::fs::write(&p.join(\"a\"), b\"x\").unwrap(); std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); }",
            0,
        ),
        (
            "fn f() { write_executable_script(&p, \"#!/bin/sh\\n\").unwrap(); }",
            0,
        ),
        (
            "fn f() { fs::copy(source, target)?; fs::set_permissions(target, fs::Permissions::from_mode(0o700))?; }",
            1,
        ),
        ("fn f() { fs::copy(cli_bin(), &copy)?; }", 1),
        (
            "fn f() { std::fs::copy(std::env::current_exe()?, &copy)?; }",
            1,
        ),
        ("fn f() { std::fs::copy(entry.path(), &target)?; }", 0),
        (
            "fn f() { let f = File::create(&p)?; drop(f); fs::set_permissions(&p, Permissions::from_mode(0o755))?; }",
            1,
        ),
        (
            "fn f() { let f = OpenOptions::new().write(true).create(true).open(&p)?; fs::set_permissions(&p, Permissions::from_mode(0o755))?; }",
            1,
        ),
        (
            "fn f() { let f = OpenOptions::new().write(true).open(&p)?; fs::set_permissions(&p, Permissions::from_mode(0o755))?; }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(executable_write_sites(&syntax), sites, "{source}");
    }
}
