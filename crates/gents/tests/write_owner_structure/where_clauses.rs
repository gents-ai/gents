use super::support::{all_rust_sources, repo_root};
use std::collections::BTreeSet;
use syn::visit::{self, Visit};

#[derive(Default)]
struct InlineBoundVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for InlineBoundVisitor {
    fn visit_generic_param(&mut self, param: &'ast syn::GenericParam) {
        let inline = match param {
            syn::GenericParam::Type(parameter) => !parameter.bounds.is_empty(),
            syn::GenericParam::Lifetime(parameter) => !parameter.bounds.is_empty(),
            syn::GenericParam::Const(_) => false,
        };
        if inline {
            self.sites += 1;
        }
        visit::visit_generic_param(self, param);
    }
}

fn inline_bound_sites(syntax: &syn::File) -> usize {
    let mut visitor = InlineBoundVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

/// Trait and lifetime bounds go in a where clause, never inline in the generic
/// parameter list, matching DefraDB; this covers tests too.
#[test]
fn trait_bounds_live_in_where_clauses() {
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
            let sites = inline_bound_sites(&syntax);
            (sites > 0).then(|| {
                let relative = path
                    .strip_prefix(&root)
                    .expect("source under repo root")
                    .to_string_lossy()
                    .into_owned();
                format!("{relative}: {sites} inline generic bounds; move them to a where clause")
            })
        })
        .collect::<BTreeSet<_>>();
    assert!(
        violations.is_empty(),
        "where-clause ratchet failed:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn where_clause_fence_counts_inline_bounds_only() {
    for (source, sites) in [
        ("fn f<T: Clone>(t: T) {}", 1),
        ("fn f<'a, 'b: 'a, T>(t: &'b T) {}", 1),
        ("struct S<T: Default = u8>(T);", 1),
        ("impl<T: Send, U: Sync> Tr for S<T, U> {}", 2),
        ("trait Tr { fn f<T: Clone>(t: T); type A<B: Copy>; }", 2),
        ("impl S { fn f<T: Clone>(t: T) {} }", 1),
        ("fn f<T>(t: T) where T: Clone {}", 0),
        ("struct S<T = u8>(T) where T: Default;", 0),
        ("fn f<const N: usize>(t: impl Clone) {}", 0),
        ("fn f<F>(f: F) where F: for<'a> Fn(&'a u8) {}", 0),
        ("fn f() { m!(fn g<T: Clone>(t: T) {}); }", 0),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(inline_bound_sites(&syntax), sites, "{source}");
    }
}
