use super::support::{cfg_test, named_exceptions, parse_production, production_sources, repo_root};
use regex::Regex;
use std::collections::BTreeMap;
use std::sync::LazyLock;
use syn::visit::{self, Visit};
use syn::{ImplItemConst, ImplItemFn, ItemConst, ItemFn, ItemImpl, ItemMod, ItemStatic};

const RULE: &str = "session binding (Lean `TransactionFence.DeclaredFence.immutableBinding`)";

/// Each production place that writes `relationships_pending: true`, with why
/// it is a create and never re-pends a session's published relationships.
#[rustfmt::skip]
const PENDING_WRITES: &[(&str, usize, &str)] = &[
    ("crates/gents-automation/src/goal/session_goal_control.rs", 1, "creates an AgentSessionGoalReceipt, never a session"),
    ("crates/gents-protocol/src/request_admission.rs", 1, "creates a client AgentRequest (`graphql_client_publication_mutation`), never a session"),
    ("crates/gents-session/src/session/sessions.rs", 1, "creates an Agent's new child AgentSession after reading that none exists in the same transaction"),
];

/// A write of `relationships_pending` as true, in source with its whitespace
/// removed: a JSON or GraphQL input key (quoted, escaped or bare), an
/// assignment into an input, a typed input field, or a map insert, with the
/// value `true` bare or wrapped as a JSON value.
static PENDING_TRUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?:\brelationships_pending(?:\\?")?:|\["relationships_pending"\]=|\.set\([\w:]*RELATIONSHIPS_PENDING,|\.insert\("relationships_pending"(?:\.into\(\)|\.to_owned\(\)|\.to_string\(\))?,)(?:(?:serde_json::)?json!\(|(?:serde_json::)?Value::Bool\()?true\b"#,
    )
    .expect("pending write regex")
});

/// Counts writes of `relationships_pending` as true in production code:
/// function bodies, constants and statics outside test-only items.
#[derive(Default)]
struct Scan {
    sites: usize,
}

impl Scan {
    fn tokens(&mut self, tokens: impl quote::ToTokens) {
        let compact = tokens
            .to_token_stream()
            .to_string()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        self.sites += PENDING_TRUE.find_iter(&compact).count();
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
            self.tokens(&item.block);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if !cfg_test(&item.attrs) {
            self.tokens(&item.block);
        }
    }

    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        if !cfg_test(&item.attrs) {
            self.tokens(&item.expr);
        }
    }

    fn visit_item_static(&mut self, item: &'ast ItemStatic) {
        if !cfg_test(&item.attrs) {
            self.tokens(&item.expr);
        }
    }

    fn visit_impl_item_const(&mut self, item: &'ast ImplItemConst) {
        if !cfg_test(&item.attrs) {
            self.tokens(&item.expr);
        }
    }
}

/// The premise of Lean `TransactionFence.DeclaredFence.immutableBinding`, the
/// fence of a client send that reads a published session's binding before its
/// transaction and does not read the session in it (`bound_published_session`):
/// the session's requester and Agent are immutable, and no production path
/// writes its relationships back to pending. That no production path deletes
/// a session is `agent_reachable_surfaces_never_detach_or_delete_a_session`.
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
        "a session's relationships go only from pending to published; list a new create with what makes it one, and never re-pend a session (or decide that first in Lean `TransactionFence.DeclaredFence.immutableBinding`)",
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
            txn.execute(&Update::doc(d).input(Input::new().set(fields::RELATIONSHIPS_PENDING, true)).render());
            txn.execute(&Update::doc(d).input(Input::new().set(fields::RELATIONSHIPS_PENDING, false)).render());
            input.insert("relationships_pending".into(), json!(true));
            input.insert("relationships_pending".to_owned(), Value::Bool(false));
            txn.execute("{\"input\": {\"relationships_pending\": true}}");
            let rows = Query::select(x).filter(Filter::eq(fields::RELATIONSHIPS_PENDING, true));
            let re_pended = row["relationships_pending"] == true;
        }
        const REPEND: &str = "mutation { update_AgentSession(docID: $d, input: { relationships_pending: true }) { _docID } }";
        impl Session {
            const QUERY: &'static str = "{ AgentSession(filter: { relationships_pending: { _eq: true } }) { _docID } }";
        }
        #[cfg(test)]
        fn b(input: &mut Value) { input["relationships_pending"] = true.into(); }
        #[cfg(test)]
        const FIXTURE: &str = "mutation { update_AgentSession(docID: $d, input: { relationships_pending: true }) { _docID } }";
        "#,
    )
    .unwrap();
    let mut scan = Scan::default();
    scan.visit_file(&syntax);
    assert_eq!(scan.sites, 7);
}
