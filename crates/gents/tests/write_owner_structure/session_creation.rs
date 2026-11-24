use super::support::{cfg_test, macro_strings, parse_production, production_sources, repo_root};
use std::collections::BTreeMap;
use syn::visit::{self, Visit};
use syn::{ImplItemFn, ItemFn, ItemImpl, ItemMod};

const CREATE: &str = "create_AgentSession(";

/// Each production file that creates a session, with what gives the session
/// its interrupt marker in the same transaction (Lean
/// `Identity.SessionInterrupt`); only shrinks.
#[rustfmt::skip]
const CREATORS: &[(&str, usize, &str)] = &[
    ("crates/gents-session/src/session/sessions.rs", 2, "the session owner: both creates call `interrupt_marker::create_in_txn`, which stages the marker's parent relationship"),
    ("crates/gents-eval/src/runner/embedded/executor/install.rs", 1, "the eval installer calls `interrupt_marker::create_in_txn` beside its create"),
];

#[derive(Default)]
struct Scan {
    sites: usize,
}

impl<'ast> Visit<'ast> for Scan {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_fn(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_impl(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        self.sites += literal.value().matches(CREATE).count();
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let mut strings = String::new();
        let tokens = syn::buffer::TokenBuffer::new2(invocation.tokens.clone());
        macro_strings(tokens.begin(), &mut strings);
        self.sites += strings.matches(CREATE).count();
    }
}

/// A production path that creates a session outside these owners would
/// create it without its marker, and nothing could interrupt it.
#[test]
fn every_session_create_gives_the_session_its_marker() {
    let root = repo_root();
    let mut found = BTreeMap::new();
    for (relative, syntax) in parse_production(&root, production_sources(&root)) {
        let mut scan = Scan::default();
        scan.visit_file(&syntax);
        if scan.sites > 0 {
            found.insert(relative, scan.sites);
        }
    }
    let allowed = CREATORS
        .iter()
        .map(|(path, sites, _)| ((*path).to_owned(), *sites))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        found, allowed,
        "production session creates and their owners"
    );
}
