use super::support::{item_cfg_test, no_sites, owned_by, production_site_counts};
use syn::visit::{self, Visit};

const RIG_OWNER_MODULES: &[&str] = &[
    "crates/gents-loop/src/rig_compat",
    "crates/gents-loop/src/provider_input",
    "crates/gents-loop/src/loop_stream",
    "crates/gents-inference/src/llm/rig_compat.rs",
    "crates/gents/src/turn/provider_input",
    "crates/gents/src/runtime/agent/loop_stream",
];

#[derive(Default)]
struct RigPathVisitor {
    sites: usize,
}

fn rig_use_roots(tree: &syn::UseTree) -> usize {
    match tree {
        syn::UseTree::Path(syn::UsePath { ident, .. })
        | syn::UseTree::Rename(syn::UseRename { ident, .. })
        | syn::UseTree::Name(syn::UseName { ident }) => usize::from(ident == "rig"),
        syn::UseTree::Group(group) => group.items.iter().map(rig_use_roots).sum(),
        _ => 0,
    }
}

fn starts_with_rig(path: &syn::Path) -> bool {
    path.segments.len() > 1
        && path
            .segments
            .first()
            .is_some_and(|first| first.ident == "rig")
}

fn macro_names_rig(mut cursor: syn::buffer::Cursor) -> bool {
    while !cursor.eof() {
        if let Some((inside, _, _, next)) = cursor.any_group() {
            if macro_names_rig(inside) {
                return true;
            }
            cursor = next;
        } else if let Some((ident, next)) = cursor.ident() {
            let path = next.punct().is_some_and(|(colon, after)| {
                colon.as_char() == ':' && after.punct().is_some_and(|(p, _)| p.as_char() == ':')
            });
            if ident == "rig" && path {
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

impl<'ast> Visit<'ast> for RigPathVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.sites += rig_use_roots(&item.tree);
        visit::visit_item_use(self, item);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if item.ident == "rig" {
            self.sites += 1;
        }
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if starts_with_rig(path) {
            self.sites += 1;
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let tokens = syn::buffer::TokenBuffer::new2(invocation.tokens.clone());
        if macro_names_rig(tokens.begin()) {
            self.sites += 1;
        }
        visit::visit_macro(self, invocation);
    }
}

fn rig_path_sites(syntax: &syn::File) -> usize {
    let mut visitor = RigPathVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

/// No file outside the owner modules names `rig`. A layer over the provider
/// exchange implements `rig_compat::http::Transport`, and a model this
/// workspace implements itself `rig_compat::model::Model`; `rig_compat` lifts
/// both into Rig's traits.
#[test]
fn rig_types_stay_behind_their_owners() {
    no_sites(
        "rig boundary",
        &production_site_counts(|relative, syntax| {
            if RIG_OWNER_MODULES
                .iter()
                .any(|owner| owned_by(relative, owner))
            {
                0
            } else {
                rig_path_sites(syntax)
            }
        }),
        "path(s) naming `rig`",
        "use a native type and cross at `gents_loop::rig_compat` or `provider_input`",
    );
}

#[test]
fn rig_owners_match_whole_module_paths() {
    let owner = "crates/gents-loop/src/provider_input";
    assert!(owned_by("crates/gents-loop/src/provider_input.rs", owner));
    assert!(owned_by(
        "crates/gents-loop/src/provider_input/budget.rs",
        owner
    ));
    assert!(!owned_by(
        "crates/gents-loop/src/provider_input_budget.rs",
        owner
    ));
    let file = "crates/gents-loop/src/rig_compat.rs";
    assert!(owned_by(file, file));
    assert!(!owned_by("crates/gents-loop/src/rig_compat.rs.bak", file));
}

#[test]
fn rig_fence_counts_paths_uses_and_macros_outside_tests() {
    for (source, sites) in [
        ("use rig::completion::Usage;", 1),
        ("use rig::{client, completion};", 1),
        ("use rig as provider;", 1),
        ("extern crate rig as provider;", 1),
        ("use {rig as provider};", 1),
        ("pub use rig;", 1),
        ("pub use {rig};", 1),
        ("fn f(u: rig::completion::Usage) {}", 1),
        (
            "fn f() { let s = try_stream! { rig::streaming::x(); }; }",
            1,
        ),
        ("fn f(rig: R) { rig.call(); }", 0),
        ("#[cfg(test)] mod tests { use rig::completion::Usage; }", 0),
        ("#[cfg(test)] use rig::completion::Usage;", 0),
        (
            "#[cfg(test)] impl rig::completion::CompletionModel for M {}",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(rig_path_sites(&syntax), sites, "{source}");
    }
}
