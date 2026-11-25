use super::support::{
    cfg_test, item_cfg_test, parse_production, path_name, production_sources, repo_root,
};
use std::collections::BTreeSet;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{ExprCall, ItemFn, ItemMod};

const CLIENT_ENDPOINT_CRATES: &[&str] = &[
    "crates/gents-desktop-",
    "crates/gents-client-host/",
    "apps/",
];

#[derive(Default)]
struct EndpointConstructionVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for EndpointConstructionVisitor {
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

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if path_name(call).is_some_and(|name| {
            [
                "GraphqlEndpoint::as_principal",
                "GraphqlEndpoint::anonymous",
            ]
            .iter()
            .any(|constructor| name.ends_with(constructor))
        }) {
            self.sites += 1;
        }
        visit::visit_expr_call(self, call);
    }
}

const HOME_CHOICE_CHECKS: &[&str] = &[
    "document_acp::ensure_home_opens",
    "document_acp::ensure_admissible",
];

const HOME_STORE_CLAIMS: &[&str] = &[
    "lock_store",
    "lock_store_at",
    "lock_home_store",
    "try_store",
    "acquire_store",
    "lock_init_store",
    "reserve_runtime_startup",
    "open_home_store_key",
    "open_home_store_key_with_custody",
    "ProtectedBackendStore::open",
    "FreshBackendStore::observe",
];

#[derive(Clone, Copy, PartialEq)]
enum HomeEvent {
    Check,
    Claim,
}

type HomeEvents = Vec<(usize, usize, HomeEvent)>;

fn names_one_of(name: &str, entries: &[&str]) -> bool {
    entries
        .iter()
        .any(|entry| name == *entry || name.ends_with(&format!("::{entry}")))
}

/// The production functions of one file with their choice checks and store
/// claims in source order. A function of the same file that checks the
/// choice counts as a check where it is called by its bare name.
struct HomeChoiceVisitor<'a> {
    wrappers: &'a BTreeSet<String>,
    functions: Vec<(String, HomeEvents)>,
    events: HomeEvents,
}

impl HomeChoiceVisitor<'_> {
    fn function(
        &mut self,
        name: String,
        attributes: &[syn::Attribute],
        visit: impl FnOnce(&mut Self),
    ) {
        if cfg_test(attributes) {
            return;
        }
        let outer = std::mem::take(&mut self.events);
        visit(self);
        let mut events = std::mem::replace(&mut self.events, outer);
        events.sort_by_key(|(line, column, _)| (*line, *column));
        self.functions.push((name, events));
    }
}

impl<'ast> Visit<'ast> for HomeChoiceVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.function(item.sig.ident.to_string(), &item.attrs, |visitor| {
            visit::visit_item_fn(visitor, item);
        });
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.function(item.sig.ident.to_string(), &item.attrs, |visitor| {
            visit::visit_impl_item_fn(visitor, item);
        });
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Some(name) = path_name(call) {
            let event = if names_one_of(&name, HOME_CHOICE_CHECKS) || self.wrappers.contains(&name)
            {
                Some(HomeEvent::Check)
            } else if names_one_of(&name, HOME_STORE_CLAIMS) {
                Some(HomeEvent::Claim)
            } else {
                None
            };
            if let Some(event) = event {
                let start = call.span().start();
                self.events.push((start.line, start.column, event));
            }
        }
        visit::visit_expr_call(self, call);
    }
}

fn endpoint_construction_sites(syntax: &syn::File) -> usize {
    let mut visitor = EndpointConstructionVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

/// A client builds every endpoint to a runtime through `RuntimeEndpoints`, so
/// the audience the runtime reports reaches the one minting point
/// (`Identity.HeldAudience.recordCredential`).
#[test]
fn client_endpoints_are_built_by_their_owner() {
    let root = repo_root();
    let violations = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter(|(relative, _)| {
            CLIENT_ENDPOINT_CRATES
                .iter()
                .any(|prefix| relative.starts_with(prefix))
        })
        .filter(|(_, syntax)| endpoint_construction_sites(syntax) > 0)
        .map(|(relative, _)| {
            format!("{relative}: builds a GraphqlEndpoint; use gents_store::config_client::runtime_endpoints::RuntimeEndpoints")
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "client endpoint rule failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn client_endpoint_fence_counts_constructors_outside_tests() {
    for (source, sites) in [
        ("fn f() { GraphqlEndpoint::as_principal(u, d); }", 1),
        (
            "fn f() { gents_store::config_client::GraphqlEndpoint::anonymous(u); }",
            1,
        ),
        ("fn f() { RuntimeEndpoints::new(c).endpoint(n, u); }", 0),
        (
            "#[cfg(test)] mod tests { fn f() { GraphqlEndpoint::anonymous(u); } }",
            0,
        ),
        (
            "impl S { #[cfg(test)] fn f() { GraphqlEndpoint::anonymous(u); } }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(endpoint_construction_sites(&syntax), sites, "{source}");
    }
}

fn home_choice_functions(
    syntax: &syn::File,
    wrappers: &BTreeSet<String>,
) -> Vec<(String, HomeEvents)> {
    let mut visitor = HomeChoiceVisitor {
        wrappers,
        functions: Vec::new(),
        events: Vec::new(),
    };
    visitor.visit_file(syntax);
    visitor.functions
}

fn checks_the_choice(events: &[(usize, usize, HomeEvent)]) -> bool {
    events
        .iter()
        .any(|(_, _, event)| *event == HomeEvent::Check)
}

fn stores_claimed_before_the_choice(syntax: &syn::File) -> Vec<String> {
    let mut wrappers = BTreeSet::new();
    let functions = loop {
        let functions = home_choice_functions(syntax, &wrappers);
        let checking = functions
            .iter()
            .filter(|(_, events)| checks_the_choice(events))
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        if checking == wrappers {
            break functions;
        }
        wrappers = checking;
    };
    functions
        .into_iter()
        .filter(|(_, events)| {
            checks_the_choice(events)
                && events
                    .iter()
                    .take_while(|(_, _, event)| *event != HomeEvent::Check)
                    .any(|(_, _, event)| *event == HomeEvent::Claim)
        })
        .map(|(name, _)| name)
        .collect()
}

/// A function that checks a home's document-ACP choice does so before it
/// claims or opens the home's store, so a refused home is left untouched
/// (Lean `DocumentAcpChoice.admit`). Syntactic: claims are matched by name,
/// and a function that never checks the choice is not held to it.
#[test]
fn a_homes_choice_is_checked_before_its_store_is_claimed() {
    let root = repo_root();
    let violations = parse_production(&root, production_sources(&root))
        .into_iter()
        .flat_map(|(relative, syntax)| {
            stores_claimed_before_the_choice(&syntax)
                .into_iter()
                .map(move |name| format!("{relative}: `{name}` claims the home's store before checking its document-ACP choice"))
        })
        .collect::<Vec<_>>();
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn home_choice_fence_flags_a_claim_before_the_check() {
    for (source, flagged) in [
        (
            "async fn f() { let k = store_key::open_home_store_key(h, d).await?; document_acp::ensure_home_opens(&c)?; }",
            vec!["f"],
        ),
        (
            "fn f() { let l = home::lock_store(h, d)?; gents_store::local::document_acp::ensure_admissible(r, q, s)?; }",
            vec!["f"],
        ),
        (
            "fn f() { document_acp::ensure_home_opens(&c)?; let l = home::lock_store(h, d)?; ProtectedBackendStore::open(d, k, l); }",
            vec![],
        ),
        (
            "fn admit() { document_acp::ensure_admissible(r, q, s)?; } fn f() { let l = home::acquire_store(h, d, w)?; admit()?; }",
            vec!["f"],
        ),
        (
            "fn admit() { document_acp::ensure_admissible(r, q, s)?; } fn f() { admit()?; let l = home::acquire_store(h, d, w)?; }",
            vec![],
        ),
        ("fn f() { let l = home::lock_store(h, d)?; }", vec![]),
        (
            "#[cfg(test)] fn f() { let l = home::lock_store(h, d)?; document_acp::ensure_home_opens(&c)?; }",
            vec![],
        ),
        (
            "fn f() { patch::ensure_admissible(t, p)?; let l = home::lock_store(h, d)?; document_acp::ensure_home_opens(&c)?; }",
            vec!["f"],
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(
            stores_claimed_before_the_choice(&syntax),
            flagged,
            "{source}"
        );
    }
}
