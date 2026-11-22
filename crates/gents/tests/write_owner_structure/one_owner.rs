use super::support::{cfg_test, macro_strings, parse_production, production_sources, repo_root};
use syn::visit::{self, Visit};
use syn::{ExprMethodCall, ItemFn, ItemImpl, ItemMod};

#[test]
fn workspace_consent_resolution_has_one_owner() {
    #[rustfmt::skip]
    const EXACT_KEY_READER_SITES: &[(&str, usize)] = &[
        ("crates/gents-tools/src/plugin/approval/workspace.rs", 1),
        ("crates/gents-tools/src/plugin/approval/workspace/context.rs", 1),
    ];
    let root = repo_root();
    let owner = root.join("crates/gents-tools/src/plugin/approval/workspace.rs");
    let source = std::fs::read_to_string(&owner).expect("workspace consent owner");
    assert_eq!(
        source
            .matches("fn entry(home: &Path, key: &WorkspaceKey)")
            .count(),
        1,
        "the exact-key reader moved or changed shape"
    );
    assert!(
        !source.contains("pub fn entry")
            && !source.contains("pub(crate) fn entry")
            && !source.contains("pub(super) fn entry"),
        "the exact-key reader must stay private to its module"
    );
    let mut violations = Vec::new();
    for path in production_sources(&root) {
        let relative = path
            .strip_prefix(&root)
            .expect("repository path")
            .to_string_lossy()
            .replace('\\', "/");
        let count = std::fs::read_to_string(&path)
            .expect("production source")
            .matches("entry(home")
            .count();
        let allowed = EXACT_KEY_READER_SITES
            .iter()
            .find(|(file, _)| *file == relative)
            .map_or(0, |(_, count)| *count);
        if count != allowed {
            violations.push(format!(
                "{relative}: {count} exact-key reads, expected {allowed}"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "workspace consent is read without resolving the session:\n{}",
        violations.join("\n")
    );
}

#[derive(Default)]
struct RuntimeRequestWriteSites(Vec<String>);

impl<'ast> Visit<'ast> for RuntimeRequestWriteSites {
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

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        if literal
            .value()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
            .contains("create_AgentRequest(")
        {
            self.0.push("raw AgentRequest create".to_owned());
        }
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let tokens = syn::buffer::TokenBuffer::new2(invocation.tokens.clone());
        let mut strings = String::new();
        macro_strings(tokens.begin(), &mut strings);
        if strings
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
            .contains("create_AgentRequest(")
        {
            self.0.push("raw AgentRequest create in macro".to_owned());
        }
        visit::visit_macro(self, invocation);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if matches!(
            call.method.to_string().as_str(),
            "graphql_mutation" | "graphql_mutation_selecting"
        ) {
            self.0
                .push("unowned AgentRequest mutation builder".to_owned());
        }
        visit::visit_expr_method_call(self, call);
    }
}

#[test]
fn runtime_requests_have_one_publication_owner() {
    let root = repo_root();
    let mut violations = Vec::new();
    for (path, syntax) in parse_production(&root, production_sources(&root)) {
        if path == "crates/gents-session/src/request/lifecycle/client.rs" {
            continue;
        }
        if !path.starts_with("crates/gents-session/src/request/lifecycle/")
            && !path.starts_with("crates/gents-automation/src/")
            && !path.starts_with("crates/gents/src/runtime/")
        {
            continue;
        }
        let mut sites = RuntimeRequestWriteSites::default();
        sites.visit_file(&syntax);
        violations.extend(sites.0.into_iter().map(|site| format!("{path}: {site}")));
    }
    assert!(
        violations.is_empty(),
        "runtime requests bypass their publication owner: {violations:?}"
    );
    let mut probe = RuntimeRequestWriteSites::default();
    probe.visit_file(&syn::parse_file(r#"async fn bare(txn: &Txn) { txn.execute("mutation { create_AgentRequest(input: $input) { _docID } }").await; }"#).unwrap());
    assert!(!probe.0.is_empty(), "a new bare create must fail the guard");
}

/// The one owner of change subscriptions: a watcher subscribes through it, so
/// on a home whose access settles after a row commits it is also woken when
/// the access settles (Lean `AccessWake.listens`, TLA `AccessWake`).
const CHANGE_SUBSCRIPTION_OWNER: &str = "crates/gents-store/src/subscription_source.rs";

#[derive(Default)]
struct ChangeSubscriptionSites(usize);

impl<'ast> Visit<'ast> for ChangeSubscriptionSites {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_fn(self, item);
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "subscribe_collection_changes" {
            self.0 += 1;
        }
        visit::visit_expr_method_call(self, call);
    }
}

#[test]
fn change_subscriptions_have_one_owner() {
    let root = repo_root();
    let violations = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter(|(relative, _)| relative != CHANGE_SUBSCRIPTION_OWNER)
        .filter(|(_, syntax)| {
            let mut sites = ChangeSubscriptionSites::default();
            sites.visit_file(syntax);
            sites.0 > 0
        })
        .map(|(relative, _)| relative)
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "subscribe through `gents_store::subscription_source` instead of the node's \
         `subscribe_collection_changes`, so access changes wake the watcher: {violations:?}"
    );
}
