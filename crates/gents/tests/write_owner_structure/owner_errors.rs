use super::support::{item_cfg_test, owned_by, parse_production, production_sources, repo_root};
use syn::visit::{self, Visit};

/// Owner and library interfaces: the seams where callers decide behavior by
/// which failure occurred. They return module-owned `thiserror` enums and
/// classifiers match those enums; application code may use `anyhow`.
pub(crate) const OWNER_MODULES: &[&str] = &[
    "crates/gents-tools/src/ux_data/create_schema.rs",
    "crates/gents-claude/src",
    "crates/gents-protocol/src",
    "crates/gents-loop/src",
    "crates/gents/src/operations",
    "crates/gents-store/src/config_client/common.rs",
    "crates/gents-store/src/config_client/error.rs",
    "crates/gents-store/src/config_client/graphql.rs",
    "crates/gents-store/src/config_client/mod.rs",
    "crates/gents-documents/src/patch.rs",
    "crates/gents-store/src/config_client/retry.rs",
    "crates/gents-store/src/config_client/schema_contract.rs",
    "crates/gents-store/src/config_client/schema_install.rs",
    "crates/gents-store/src/config_client/schema_management.rs",
    "crates/gents-store/src/config_client/txn",
    "crates/gents-store/src/config_client/write_telemetry.rs",
    "crates/gents-store/src/graphql",
    "crates/gents-store/src/local/home",
    "crates/gents-store/src/identity",
    "crates/gents-store/src/schema/catalog",
    "crates/gents-store/src/local/storage_backend",
    "crates/gents-store/src/local/store_key",
];

#[derive(Default)]
struct AnyhowVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for AnyhowVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path
            .segments
            .iter()
            .any(|segment| segment.ident == "anyhow")
        {
            self.sites += 1;
        }
        visit::visit_path(self, path);
    }

    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        let named = match tree {
            syn::UseTree::Path(syn::UsePath { ident, .. })
            | syn::UseTree::Name(syn::UseName { ident })
            | syn::UseTree::Rename(syn::UseRename { ident, .. }) => ident == "anyhow",
            _ => false,
        };
        if named {
            self.sites += 1;
        }
        visit::visit_use_tree(self, tree);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if item.ident == "anyhow" {
            self.sites += 1;
        }
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let tokens = syn::buffer::TokenBuffer::new2(invocation.tokens.clone());
        if tokens_name_anyhow(tokens.begin()) {
            self.sites += 1;
        }
        visit::visit_macro(self, invocation);
    }

    fn visit_meta_list(&mut self, list: &'ast syn::MetaList) {
        let tokens = syn::buffer::TokenBuffer::new2(list.tokens.clone());
        if tokens_name_anyhow(tokens.begin()) {
            self.sites += 1;
        }
        visit::visit_meta_list(self, list);
    }
}

/// Macro and attribute tokens are not parsed, so any `anyhow` identifier in
/// them counts: `use anyhow as e;` inside a macro would otherwise hide every
/// later use behind the alias.
fn tokens_name_anyhow(mut cursor: syn::buffer::Cursor) -> bool {
    while !cursor.eof() {
        if let Some((inside, _, _, next)) = cursor.any_group() {
            if tokens_name_anyhow(inside) {
                return true;
            }
            cursor = next;
        } else if let Some((ident, next)) = cursor.ident() {
            if ident == "anyhow" {
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

fn anyhow_sites(syntax: &syn::File) -> usize {
    let mut visitor = AnyhowVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn owner_interfaces_use_thiserror() {
    let root = repo_root();
    let missing = OWNER_MODULES
        .iter()
        .filter(|owner| {
            let path = root.join(owner);
            !path.exists() && !path.with_extension("rs").is_file()
        })
        .collect::<Vec<_>>();
    assert!(missing.is_empty(), "owner modules not found: {missing:?}");
    let violations = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter(|(relative, _)| OWNER_MODULES.iter().any(|owner| owned_by(relative, owner)))
        .filter(|(_, syntax)| anyhow_sites(syntax) > 0)
        .map(|(relative, _)| {
            format!("{relative}: names anyhow; return a module-owned thiserror enum")
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "owner interface ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn owner_interface_fence_counts_anyhow_names_outside_tests() {
    for (source, sites) in [
        ("use anyhow::Result;", 1),
        ("use anyhow::{bail, Context};", 1),
        ("fn f() -> anyhow::Result<()> { Ok(()) }", 1),
        ("fn f() { anyhow::bail!(\"x\"); }", 1),
        ("fn f() { let e = anyhow!(\"x\"); }", 1),
        (
            "fn f() { assert!(matches!(g(), Err(e) if e.is::<anyhow::Error>())); }",
            1,
        ),
        ("extern crate anyhow;", 1),
        ("impl T for anyhow::Error {}", 1),
        (
            "#[derive(thiserror::Error)] enum E { #[error(\"anyhow\")] A }",
            0,
        ),
        ("fn f() { let anyhow = 1; }", 0),
        ("macro_rules! m { () => { use anyhow as eh; } }", 1),
        (
            "fn f() { tokio::select! { _ = g() => { use anyhow as eh; } } }",
            1,
        ),
        ("#[attr(anyhow::Error)] fn f() {}", 1),
        ("#[cfg(test)] mod tests { use anyhow::Result; }", 0),
        ("#[cfg(test)] fn f() -> anyhow::Result<()> { Ok(()) }", 0),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(anyhow_sites(&syntax), sites, "{source}");
    }
}
