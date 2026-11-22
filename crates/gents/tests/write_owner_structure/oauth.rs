use super::support::{
    cfg_test, macro_strings, owned_by, parse_production, path_name, production_sources, repo_root,
};
use syn::visit::{self, Visit};
use syn::{ExprCall, ExprMethodCall, ItemFn, ItemImpl, ItemMod};

const OAUTH_CREDENTIAL_OWNER: &str = "crates/gents-inference/src/auth/credential";

const REMOVED_OAUTH_PICKERS: &[&str] = &[
    "lookup_oauth_credential",
    "oauth_credential_query",
    "load_oauth_credential",
    "has_enabled_oauth_credential",
];

const OAUTH_ROW_READERS: &[&str] = &[
    "list_oauth_credentials",
    "list_oauth_credentials_on",
    "oauth_credentials_from_response",
    "oauth_credentials_for_node_query",
    "oauth_credential_by_id_query",
    "oauth_credential_by_doc_id_query",
    "lookup_oauth_credential_by_doc_id",
];

#[derive(Default)]
struct OAuthPickVisitor {
    violations: Vec<String>,
    reads: usize,
}

static RAW_OAUTH_QUERY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"OAuthCredential\s*(\(|\{[{\s])").expect("raw query pattern")
});

impl OAuthPickVisitor {
    fn raw_query(&mut self, text: &str) {
        if RAW_OAUTH_QUERY.is_match(text) {
            self.violations
                .push("holds a raw `OAuthCredential` query".to_string());
        }
    }

    fn call(&mut self, name: &str) {
        if OAUTH_ROW_READERS.contains(&name) {
            self.reads += 1;
        }
    }
}

impl<'ast> Visit<'ast> for OAuthPickVisitor {
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

    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        let name = ident.to_string();
        if REMOVED_OAUTH_PICKERS.contains(&name.as_str()) {
            self.violations.push(format!("names `{name}`"));
        }
    }

    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        self.raw_query(&literal.value());
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let tokens = syn::buffer::TokenBuffer::new2(invocation.tokens.clone());
        let mut strings = String::new();
        macro_strings(tokens.begin(), &mut strings);
        self.raw_query(&strings);
        visit::visit_macro(self, invocation);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Some(name) = path_name(call) {
            self.call(name.rsplit("::").next().unwrap_or(&name));
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        self.call(&call.method.to_string());
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_field(&mut self, field: &'ast syn::ExprField) {
        if matches!(&field.member, syn::Member::Named(ident) if ident == "oauth_credentials") {
            self.reads += 1;
        }
        visit::visit_expr_field(self, field);
    }
}

fn oauth_pick_findings(syntax: &syn::File) -> (Vec<String>, usize) {
    let mut visitor = OAuthPickVisitor::default();
    visitor.visit_file(syntax);
    (visitor.violations, visitor.reads)
}

#[test]
fn oauth_credential_picks_go_through_the_resolver() {
    let root = repo_root();
    let mut violations = Vec::new();
    for (relative, syntax) in parse_production(&root, production_sources(&root)) {
        if owned_by(&relative, OAUTH_CREDENTIAL_OWNER) {
            continue;
        }
        let (found, reads) = oauth_pick_findings(&syntax);
        violations.extend(
            found
                .into_iter()
                .map(|found| format!("{relative}: {found}")),
        );
        if reads > 0 {
            violations.push(format!(
                "{relative}: {reads} OAuthCredential row read(s); pick through inference::auth::credential::resolve_oauth_credential"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "OAuth credential picks bypass the resolver:\n{}",
        violations.join("\n")
    );
}

#[test]
fn oauth_pick_fence_flags_pickers_raw_queries_and_reads() {
    for (source, violations, reads) in [
        (
            "fn f(n: &N) { lookup_oauth_credential(n, \"did\", \"p\"); }",
            1,
            0,
        ),
        (
            "fn f(a: &A) { gents::inference::auth::credential::oauth_credential_query(\"d\", \"p\"); }",
            1,
            0,
        ),
        ("async fn load_oauth_credential() {}", 1, 0),
        (
            "fn f(v: &V) { v.has_enabled_oauth_credential(\"p\"); }",
            1,
            0,
        ),
        (
            r#"fn f(d: &str) -> String { format!("query {{ OAuthCredential(filter: {{ node_did: {{ _eq: \"{d}\" }} }}) {{ _docID }} }}") }"#,
            1,
            0,
        ),
        ("fn f(a: &A) { list_oauth_credentials_on(a, \"d\"); }", 0, 1),
        (
            "fn f(v: &V) { v.oauth_credentials.values().find(|r| r.enabled); }",
            0,
            1,
        ),
        (
            "fn f(n: &N) { lookup_oauth_credential_by_doc_id(n, \"id\"); load_oauth_credential_for_discovery(); }",
            0,
            1,
        ),
        (
            "#[cfg(test)] mod tests { fn f(n: &N) { lookup_oauth_credential(n); list_oauth_credentials(n); } }",
            0,
            0,
        ),
        (
            r#"const Q: &str = "query { OAuthCredential (filter: {}) { _docID } }";"#,
            1,
            0,
        ),
        (
            r#"fn f() -> String { format!("{{ OAuthCredential \n (filter: {{}}) {{ _docID }} }}") }"#,
            1,
            0,
        ),
        (
            r#"const Q: &str = concat!("query { OAuthCredential", "(filter: {}) { _docID } }");"#,
            1,
            0,
        ),
        (
            r#"fn f() -> String { format!("query {{ OAuthCredential {{ _docID }} }}") }"#,
            1,
            0,
        ),
        (
            r#"fn f(id: &str) -> String { format!("duplicate OAuthCredential {id}") }"#,
            0,
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        let (found, counted) = oauth_pick_findings(&syntax);
        assert_eq!(
            (found.len(), counted),
            (violations, reads),
            "{source}: {found:?}"
        );
    }
}
