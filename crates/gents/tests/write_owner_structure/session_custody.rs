use super::support::{
    cfg_test, macro_expressions, named_exceptions, no_sites, parse_production, production_sources,
    repo_root,
};
use std::collections::{BTreeMap, BTreeSet};
use syn::visit::{self, Visit};
use syn::{Expr, ExprMethodCall, ImplItemFn, ItemConst, ItemFn, ItemImpl, ItemMod};

const RULE: &str = "session custody (Lean `Tools.SessionCustody`)";

const SESSION: &str = "AgentSession";

const SESSION_NAME: &str = "AGENT_SESSION_NAME";

/// The store methods that remove a relationship or delete a document of the
/// collection named by their first argument.
const REMOVALS: &[&str] = &[
    "delete_actor_relationship",
    "delete_resource_relationship",
    "delete_entity_set_relationship",
    "delete_document_with_schema",
];

/// The primitives the scanned calls reach; their callers are what is scanned.
const PRIMITIVES: &[&str] = &[
    "crates/gents-store/src/config_client/relationships.rs",
    "crates/gents-store/src/config_client/txn/execute.rs",
    "crates/gents-store/src/config_client/txn/delete_confirmation.rs",
    "crates/gents-protocol/src/document_query.rs",
];

/// Each place that removes a relationship or deletes a document of a computed
/// collection, with what keeps the session collection out of its domain.
#[rustfmt::skip]
const COMPUTED: &[(&str, usize, &str)] = &[
    ("crates/gents-documents/src/desired_state/selection.rs", 1, "the collection is a desired-state configuration `Collection`, which has no session"),
    ("crates/gents-session/src/request/hooks/native.rs", 2, "a hook writes only collections its plugin manifest owns, and no manifest owns a core collection (`hooks::effect::validate`)"),
    ("crates/gents-store/src/config_client/client_pack_evidence.rs", 1, "pack evidence deletes only `clientSignedDeleteCollection`, which excludes the session (Lean `client_delete_excludes_session`)"),
    ("crates/gents-store/src/config_client/txn/configuration_reader.rs", 1, "the collection is a configuration document kind, never a session"),
    ("crates/gents-toolcalls/src/plugin/application_write/mod.rs", 1, "the write tool refuses every Gents-managed collection before it builds a mutation"),
    ("crates/gents-toolcalls/src/plugin/application_write/plugin_writes.rs", 2, "plugin data writes refuse every runtime collection (`writable_output_collection`)"),
    ("crates/gents-tools/src/builtin/self_config/command/policy.rs", 1, "the policy tool never removes a relation of a session document (Lean `Tools.SessionCustody.relationChangeOffered`)"),
];

#[derive(Default)]
struct Sites {
    session: usize,
    computed: usize,
}

struct Scan<'a> {
    constants: &'a BTreeMap<String, BTreeSet<String>>,
    sites: Sites,
}

enum Named {
    Session,
    Other,
    Computed,
}

impl Scan<'_> {
    fn collection(&self, expression: &Expr) -> Named {
        match expression {
            Expr::Reference(reference) => self.collection(&reference.expr),
            Expr::Paren(paren) => self.collection(&paren.expr),
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(literal),
                ..
            }) if literal.value() == SESSION => Named::Session,
            Expr::Lit(_) => Named::Other,
            Expr::Path(path) => {
                let name = path
                    .path
                    .segments
                    .last()
                    .map(|segment| segment.ident.to_string())
                    .unwrap_or_default();
                match self.constants.get(&name) {
                    _ if name == SESSION_NAME => Named::Session,
                    Some(values) if values.contains(SESSION) => Named::Session,
                    Some(_) => Named::Other,
                    None if name.ends_with("_NAME") && name.chars().all(|c| !c.is_lowercase()) => {
                        Named::Other
                    }
                    None => Named::Computed,
                }
            }
            _ => Named::Computed,
        }
    }

    fn strings(&mut self, text: &str) {
        let compact = text
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        let needle = format!("delete_{SESSION}");
        self.sites.session += compact
            .match_indices(&needle)
            .filter(|(at, _)| {
                compact[at + needle.len()..]
                    .chars()
                    .next()
                    .is_none_or(|next| !(next.is_alphanumeric() || next == '_'))
            })
            .count();
        self.sites.computed += compact.matches("delete_{").count();
    }

    fn typed_delete(&mut self, tokens: String) {
        let compact = tokens
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        if compact.contains("Delete::") && compact.contains("agent_session::") {
            self.sites.session += 1;
        }
    }
}

impl<'ast> Visit<'ast> for Scan<'_> {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if !cfg_test(&item.attrs) {
            self.typed_delete(quote_tokens(&item.block));
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
            self.typed_delete(quote_tokens(&item.block));
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if REMOVALS.contains(&call.method.to_string().as_str()) {
            match call.args.first().map(|argument| self.collection(argument)) {
                Some(Named::Session) => self.sites.session += 1,
                Some(Named::Other) => {}
                Some(Named::Computed) | None => self.sites.computed += 1,
            }
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        self.strings(&literal.value());
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let mut parsed = Vec::new();
        let mut loose = Vec::new();
        macro_expressions(invocation.tokens.clone(), &mut parsed, &mut loose);
        for expression in &parsed {
            self.visit_expr(expression);
        }
        for token in loose {
            if let proc_macro2::TokenTree::Literal(literal) = token {
                if let syn::Lit::Str(literal) = syn::Lit::new(literal) {
                    self.strings(&literal.value());
                }
            }
        }
    }
}

fn quote_tokens(block: &syn::Block) -> String {
    quote::ToTokens::to_token_stream(block).to_string()
}

#[derive(Default)]
struct Constants(BTreeMap<String, BTreeSet<String>>);

impl<'ast> Visit<'ast> for Constants {
    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        if let Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(literal),
            ..
        }) = item.expr.as_ref()
        {
            self.0
                .entry(item.ident.to_string())
                .or_default()
                .insert(literal.value());
        }
        visit::visit_item_const(self, item);
    }
}

/// A managed Agent is the native owner of a session it publishes, so ACP
/// admits its removal of the session's relations and its delete of the
/// session. No production path offers either: none names the session
/// collection, and each that computes its collection keeps the session out.
#[test]
fn agent_reachable_surfaces_never_detach_or_delete_a_session() {
    let root = repo_root();
    let parsed = parse_production(&root, production_sources(&root));
    let mut constants = Constants::default();
    for (_, syntax) in &parsed {
        constants.visit_file(syntax);
    }
    let mut session = BTreeMap::new();
    let mut computed = BTreeMap::new();
    for (relative, syntax) in &parsed {
        if PRIMITIVES.contains(&relative.as_str()) {
            continue;
        }
        let mut scan = Scan {
            constants: &constants.0,
            sites: Sites::default(),
        };
        scan.visit_file(syntax);
        if scan.sites.session > 0 {
            session.insert(relative.clone(), scan.sites.session);
        }
        if scan.sites.computed > 0 {
            computed.insert(relative.clone(), scan.sites.computed);
        }
    }
    no_sites(
        RULE,
        &session,
        "removal(s) of an AgentSession relation or delete(s) of an AgentSession",
        "an Agent never detaches or deletes a session it published, and ACP cannot withhold that from the session's native owner, so no production path offers it; remove the path, or decide a User-signed session removal in Lean `Tools.SessionCustody` first",
    );
    named_exceptions(
        RULE,
        COMPUTED,
        &computed,
        "relation removal(s) or document delete(s) of a computed collection",
        "keep the session collection out of the computed collection's domain at the surface's owner (Lean `Tools.SessionCustody`) and list the file with what keeps it out",
    );
}

#[test]
fn session_custody_scan_finds_named_and_computed_sites() {
    let syntax = syn::parse_file(
        r#"
        const COLLECTION: &str = "AgentSession";
        async fn a(txn: &Txn, doc: &str, did: &Did) {
            txn.delete_actor_relationship("AgentSession", doc, "reader", did).await;
            txn.delete_resource_relationship(COLLECTION, doc, "parent", "agents", doc).await;
            txn.delete_actor_relationship(schemas::AGENT_SESSION_NAME, doc, "reader", did).await;
            txn.execute(&format!("mutation {{ delete_AgentSession(docID: \"{doc}\") {{ _docID }} }}")).await;
            txn.execute(&Delete::<agent_session::Doc>::doc(doc).render()).await;
            txn.delete_actor_relationship("Agent", doc, "reader", did).await;
            txn.execute("mutation { delete_AgentSessionGoalReceipt(docID: \"x\") { _docID } }").await;
            txn.delete_actor_relationship(collection, doc, "reader", did).await;
            txn.execute(&format!("mutation {{ delete_{collection}(docID: \"{doc}\") {{ _docID }} }}")).await;
        }
        #[cfg(test)]
        fn b(txn: &Txn) { txn.delete_actor_relationship("AgentSession", "d", "reader", &x); }
        "#,
    )
    .expect("parse snippet");
    let mut constants = Constants::default();
    constants.visit_file(&syntax);
    let mut scan = Scan {
        constants: &constants.0,
        sites: Sites::default(),
    };
    scan.visit_file(&syntax);
    assert_eq!((scan.sites.session, scan.sites.computed), (5, 2));
}
