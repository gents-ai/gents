use super::support::{all_rust_sources, repo_root};
use std::collections::BTreeSet;
use syn::visit::{self, Visit};

const STD_HASH_NAMES: &[&str] = &["HashMap", "HashSet", "DefaultHasher"];

#[derive(Default)]
struct StdHashVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for StdHashVisitor {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path
            .segments
            .iter()
            .any(|segment| STD_HASH_NAMES.iter().any(|name| segment.ident == name))
        {
            self.sites += 1;
        }
        visit::visit_path(self, path);
    }

    fn visit_use_name(&mut self, name: &'ast syn::UseName) {
        if STD_HASH_NAMES
            .iter()
            .any(|forbidden| name.ident == forbidden)
        {
            self.sites += 1;
        }
    }

    fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
        if STD_HASH_NAMES
            .iter()
            .any(|forbidden| rename.ident == forbidden)
        {
            self.sites += 1;
        }
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let tokens = syn::buffer::TokenBuffer::new2(invocation.tokens.clone());
        if macro_names_std_hash(tokens.begin()) {
            self.sites += 1;
        }
        visit::visit_macro(self, invocation);
    }
}

fn macro_names_std_hash(mut cursor: syn::buffer::Cursor) -> bool {
    while !cursor.eof() {
        if let Some((inside, _, _, next)) = cursor.any_group() {
            if macro_names_std_hash(inside) {
                return true;
            }
            cursor = next;
        } else if let Some((ident, next)) = cursor.ident() {
            if STD_HASH_NAMES.iter().any(|name| ident == name) {
                return true;
            }
            cursor = next;
        } else if let Some((_, next)) = cursor.token_tree() {
            cursor = next;
        } else {
            break;
        }
    }
    false
}

fn std_hash_sites(syntax: &syn::File) -> usize {
    let mut visitor = StdHashVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

/// Maps and sets use `rapidhash::RapidHashMap`/`RapidHashSet` and hashers use
/// `rapidhash::fast::RapidHasher`, matching DefraDB; this covers tests too.
/// A third-party parameter typed as std's map takes a converted value
/// (`into_iter().collect()`, `.into()`), so no file names the std type.
#[test]
fn hash_maps_use_rapidhash() {
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
            (std_hash_sites(&syntax) > 0).then(|| {
                format!(
                    "{}: names a std hash map, set or hasher; use rapidhash",
                    path.strip_prefix(&root)
                        .expect("source under repo root")
                        .display()
                )
            })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "rapidhash rule failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rapidhash_fence_counts_std_names_only() {
    for (source, sites) in [
        ("use std::collections::HashMap;", 1),
        ("use std::collections::{BTreeMap, HashSet};", 1),
        ("use std::collections::HashMap as Map;", 1),
        ("fn f(m: std::collections::HashMap<u8, u8>) {}", 1),
        ("fn f() { let h = std::hash::DefaultHasher::new(); }", 1),
        ("fn f() { let v = vec![HashSet::<u8>::new()]; }", 1),
        ("use rapidhash::{HashMapExt, RapidHashMap};", 0),
        (
            "fn f(m: RapidHashMap<u8, u8>) { let _ = RapidHashSet::<u8>::new(); }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(std_hash_sites(&syntax), sites, "{source}");
    }
}
