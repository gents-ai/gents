use super::support::{cfg_test, named_exceptions, parse_production, production_sources, repo_root};
use std::collections::BTreeMap;
use syn::visit::{self, Visit};
use syn::{ImplItemFn, ItemFn, ItemImpl, ItemMod};

const RULE: &str = "session binding (Lean `TransactionFence.a_bound_session_stays_bound`)";

/// Each production place that writes `relationships_pending: true`, with why
/// it is a create and never re-pends a session's published relationships.
#[rustfmt::skip]
const PENDING_WRITES: &[(&str, usize, &str)] = &[
    ("crates/gents-automation/src/goal/session_goal_control.rs", 1, "creates an AgentSessionGoalReceipt, never a session"),
    ("crates/gents-protocol/src/request_admission.rs", 1, "creates a client AgentRequest (`graphql_client_publication_mutation`), never a session"),
    ("crates/gents-session/src/session/sessions.rs", 1, "creates an Agent's new child AgentSession after reading that none exists in the same transaction"),
];

/// Counts writes of `relationships_pending` as true in a production body: a
/// JSON input key, a GraphQL input field, or an assignment into an input.
#[derive(Default)]
struct Scan {
    sites: usize,
}

impl Scan {
    fn body(&mut self, tokens: String) {
        let compact = tokens
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        self.sites += [
            "\"relationships_pending\":true",
            "relationships_pending:true",
            "[\"relationships_pending\"]=true",
        ]
        .iter()
        .map(|needle| compact.matches(needle).count())
        .sum::<usize>();
    }
}

impl<'ast> Visit<'ast> for Scan {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_impl(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if !cfg_test(&item.attrs) {
            self.body(quote::ToTokens::to_token_stream(&item.block).to_string());
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if !cfg_test(&item.attrs) {
            self.body(quote::ToTokens::to_token_stream(&item.block).to_string());
        }
    }
}

/// A client send reads a published session's binding before its transaction
/// and does not read the session in it (`bound_published_session`). That is
/// sound only while the binding cannot change: its requester and Agent are
/// immutable, nothing deletes a session (`session_custody`), and nothing
/// writes its relationships back to pending.
#[test]
fn a_session_binding_is_never_rewritten() {
    let root = repo_root();
    let schema = std::fs::read_to_string(
        root.join("crates/gents-schemas/schemas/agent/agent_session.graphql"),
    )
    .expect("AgentSession schema");
    for field in ["requester_did", "agent_did"] {
        let line = schema
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{field}:")))
            .unwrap_or_else(|| panic!("AgentSession.{field} is in the schema"));
        assert!(
            line.contains("@immutable"),
            "{RULE}: AgentSession.{field} must stay @immutable: {line}"
        );
    }
    let mut sites = BTreeMap::new();
    for (relative, syntax) in parse_production(&root, production_sources(&root)) {
        let mut scan = Scan::default();
        scan.visit_file(&syntax);
        if scan.sites > 0 {
            sites.insert(relative, scan.sites);
        }
    }
    named_exceptions(
        RULE,
        PENDING_WRITES,
        &sites,
        "write(s) of relationships_pending as true",
        "a session's relationships go only from pending to published; list a new create with what makes it one, and never re-pend a session (or decide that first in Lean `TransactionFence`)",
    );
}

#[test]
fn session_binding_scan_finds_each_way_of_writing_pending() {
    let syntax = syn::parse_file(
        r#"
        fn a(txn: &Txn, input: &mut Value) {
            txn.execute(&json!({"input": {"relationships_pending": true}}));
            txn.execute("mutation { update_AgentSession(docID: \"x\", input: { relationships_pending: true }) { _docID } }");
            input["relationships_pending"] = true.into();
            txn.execute("{ AgentSession(filter: { relationships_pending: { _eq: true } }) { _docID } }");
            txn.execute(&json!({"relationships_pending": false}));
        }
        #[cfg(test)]
        fn b(input: &mut Value) { input["relationships_pending"] = true.into(); }
        "#,
    )
    .unwrap();
    let mut scan = Scan::default();
    scan.visit_file(&syntax);
    assert_eq!(scan.sites, 3);
}
