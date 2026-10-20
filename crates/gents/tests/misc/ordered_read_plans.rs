use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use syn::parse::Parser;
use syn::visit::Visit;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    path: String,
    collection: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Screen {
    read: Read,
    bindings: Vec<BTreeMap<String, String>>,
}

/// Each exception fixes the complete prefix of its named index, leaving no
/// residual predicate. Keep the templates exact and let this list only shrink.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderedIndexException {
    read: Read,
    index: String,
}

fn test_only(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .meta
                .require_list()
                .is_ok_and(|list| list.tokens.to_string() == "test")
    })
}

fn constant_string(expression: &syn::Expr) -> Option<String> {
    match expression {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(value),
            ..
        }) => Some(value.value()),
        syn::Expr::Macro(expression) if expression.mac.path.is_ident("concat") => {
            let parts = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated
                .parse2(expression.mac.tokens.clone())
                .ok()?;
            parts
                .iter()
                .map(constant_string)
                .collect::<Option<Vec<_>>>()
                .map(|parts| parts.join(""))
        }
        _ => None,
    }
}

#[derive(Default)]
struct Literals(Vec<(String, bool)>);
impl Literals {
    fn tokens(&mut self, tokens: proc_macro2::TokenStream, formatted: bool) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Group(group) => self.tokens(group.stream(), formatted),
                proc_macro2::TokenTree::Literal(literal) => {
                    if let Ok(literal) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                        self.0.push((literal.value(), formatted));
                    }
                }
                _ => {}
            }
        }
    }
}
impl<'ast> Visit<'ast> for Literals {
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if item.ident != "tests" && !test_only(&item.attrs) {
            syn::visit::visit_item_mod(self, item);
        }
    }
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        if !test_only(&item.attrs) {
            syn::visit::visit_item_fn(self, item);
        }
    }
    fn visit_lit_str(&mut self, literal: &'ast syn::LitStr) {
        self.0.push((literal.value(), false));
    }
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if mac.path.is_ident("format") || mac.path.is_ident("format_args") {
            if let Ok(arguments) =
                syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated
                    .parse2(mac.tokens.clone())
            {
                if let Some(template) = arguments.first().and_then(constant_string) {
                    self.0.push((template, true));
                    for argument in arguments.iter().skip(1) {
                        self.visit_expr(argument);
                    }
                    return;
                }
            }
        } else if mac.path.is_ident("concat") {
            let expression = syn::Expr::Macro(syn::ExprMacro {
                attrs: Vec::new(),
                mac: mac.clone(),
            });
            if let Some(template) = constant_string(&expression) {
                self.0.push((template, false));
                return;
            }
        }
        self.tokens(mac.tokens.clone(), false);
    }
}

fn format_markers(template: &str) -> String {
    let mut out = String::new();
    let mut chars = template.chars().peekable();
    let mut positional = 0;
    while let Some(ch) = chars.next() {
        match ch {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                out.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                out.push('}');
            }
            '{' => {
                let field: String = chars.by_ref().take_while(|ch| *ch != '}').collect();
                let name = field.split(':').next().unwrap();
                let name = if name.is_empty() {
                    let name = positional.to_string();
                    positional += 1;
                    name
                } else {
                    name.to_owned()
                };
                out.push_str(&format!("$GENTS_{name}$"));
            }
            _ => out.push(ch),
        }
    }
    out
}

fn matching_paren(text: &str, start: usize) -> Option<usize> {
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if quoted && ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quoted = !quoted;
        }
        if quoted {
            continue;
        }
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + offset);
                }
            }
            _ => {}
        }
    }
    None
}

fn canonical_whitespace(text: &str) -> String {
    let mut result = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut space = false;
    for ch in text.chars() {
        if !quoted && ch.is_whitespace() {
            space = !result.is_empty();
            continue;
        }
        if space {
            result.push(' ');
            space = false;
        }
        result.push(ch);
        if escaped {
            escaped = false;
        } else if quoted && ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        }
    }
    result
}

fn reads(path: &str, template: &str, formatted: bool) -> Vec<Read> {
    if !template.contains("order") || !(template.contains("limit") || template.contains("offset")) {
        return Vec::new();
    }
    let template = if formatted {
        format_markers(template)
    } else {
        template.to_owned()
    };
    let field = Regex::new(r"([A-Z][A-Za-z0-9_]*|\$GENTS_[^$]*\$)\s*\(").unwrap();
    let ordered = Regex::new(r"\border\s*:").unwrap();
    let bounded = Regex::new(r"\b(?:limit|offset)\s*:").unwrap();
    field
        .captures_iter(&template)
        .filter_map(|capture| {
            // GROUP sorts materialized aggregate members, outside collection index selection.
            if &capture[1] == "GROUP" {
                return None;
            }
            let start = capture.get(0).unwrap().end() - 1;
            let end = matching_paren(&template, start)?;
            let arguments = &template[start + 1..end];
            (ordered.is_match(arguments) && bounded.is_match(arguments)).then(|| Read {
                path: path.to_owned(),
                collection: capture[1].to_owned(),
                arguments: canonical_whitespace(arguments),
            })
        })
        .collect()
}

fn javascript_literals(path: &Path, source: &str) -> Vec<(String, bool)> {
    use oxc_ast::ast::{StringLiteral, TemplateLiteral};
    use oxc_ast_visit::{walk, Visit};
    use oxc_span::GetSpan;

    struct Strings<'s> {
        source: &'s str,
        literals: Vec<(String, bool)>,
    }
    impl<'a> Visit<'a> for Strings<'_> {
        fn visit_string_literal(&mut self, literal: &StringLiteral<'a>) {
            self.literals.push((literal.value.to_string(), false));
        }
        fn visit_template_literal(&mut self, literal: &TemplateLiteral<'a>) {
            let mut template = String::new();
            for (index, quasi) in literal.quasis.iter().enumerate() {
                template.push_str(
                    &quasi
                        .value
                        .cooked
                        .as_ref()
                        .unwrap_or(&quasi.value.raw)
                        .to_string(),
                );
                if let Some(expression) = literal.expressions.get(index) {
                    template.push_str("$GENTS_");
                    template.push_str(expression.span().source_text(self.source));
                    template.push('$');
                }
            }
            self.literals.push((template, false));
            walk::walk_template_literal(self, literal);
        }
    }
    let allocator = oxc_allocator::Allocator::default();
    let parsed = oxc_parser::Parser::new(
        &allocator,
        source,
        oxc_span::SourceType::from_path(path).unwrap(),
    )
    .parse();
    assert!(
        parsed.errors.is_empty(),
        "{}: {:?}",
        path.display(),
        parsed.errors
    );
    let mut strings = Strings {
        source,
        literals: Vec::new(),
    };
    strings.visit_program(&parsed.program);
    strings.literals
}

fn python_literals(path: &Path) -> Vec<(String, bool)> {
    let script = r#"
import ast, json, pathlib, sys
class Strings(ast.NodeVisitor):
    def __init__(self): self.literals = []
    def visit_Constant(self, node):
        if isinstance(node.value, str): self.literals.append([node.value, False])
    def visit_JoinedStr(self, node):
        parts = []
        for value in node.values:
            if isinstance(value, ast.Constant): parts.append(value.value)
            else:
                parts.append('$GENTS_' + ast.unparse(value.value) + '$')
                self.visit(value.value)
        self.literals.append([''.join(parts), False])
strings = Strings()
strings.visit(ast.parse(pathlib.Path(sys.argv[1]).read_text()))
sys.stdout.write(json.dumps(strings.literals))
"#;
    let output = std::process::Command::new("python3")
        .args(["-c", script])
        .arg(path)
        .output()
        .expect("Python 3 is required to inspect Python query literals");
    assert!(
        output.status.success(),
        "{}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn source_files(root: &Path, files: &mut Vec<PathBuf>) {
    if !root.exists() {
        return;
    }
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let path = entry.path();
        if kind.is_dir() {
            if ![
                "target",
                "node_modules",
                "tests",
                "dist",
                "test-results",
                ".lake",
                ".git",
            ]
            .iter()
            .any(|name| path.file_name().is_some_and(|found| found == *name))
            {
                source_files(&path, files);
            }
        } else if kind.is_file()
            && path.extension().is_some_and(|extension| {
                [
                    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "mts", "cts", "py",
                ]
                .iter()
                .any(|allowed| extension == *allowed)
            })
            && path.file_stem().is_some_and(|stem| {
                let stem = stem.to_string_lossy();
                stem != "tests"
                    && !stem.starts_with("test_")
                    && !["_tests", "_test", ".test", ".spec", ".node-test"]
                        .iter()
                        .any(|suffix| stem.ends_with(suffix))
            })
        {
            files.push(path);
        }
    }
}

fn inventory(root: &Path) -> BTreeSet<Read> {
    let mut files = Vec::new();
    for directory in ["crates", "apps", "packages", "packs", "scripts"] {
        source_files(&root.join(directory), &mut files);
    }
    let mut found = BTreeSet::new();
    for file in files {
        let path = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let source = std::fs::read_to_string(&file).unwrap();
        let literals = if file.extension().is_some_and(|extension| extension == "rs") {
            let syntax = syn::parse_file(&source).unwrap_or_else(|error| panic!("{path}: {error}"));
            let mut literals = Literals::default();
            literals.visit_file(&syntax);
            literals.0
        } else if file.extension().is_some_and(|extension| extension == "py") {
            python_literals(&file)
        } else {
            javascript_literals(&file, &source)
        };
        for (literal, formatted) in literals {
            found.extend(reads(&path, &literal, formatted));
        }
    }
    found
}

fn binding_value(value: &str) -> String {
    let session = |requester| gents::session::session_scope_filter("probe", "probe", requester);
    match value {
        "@session" => session(Some("probe")),
        "@session-absent" => session(None),
        "@public-session" => gents::session::public_request_filter(&session(Some("probe"))),
        "@public-session-absent" => gents::session::public_request_filter(&session(None)),
        "@public-keyset" => gents::session::public_request_filter(&format!(
            "{}, request_id: {{_gt: \"probe\"}}",
            session(Some("probe"))
        )),
        "@public-node-keyset" => gents::session::public_request_filter(
            "node_did: {_eq: \"probe\"}, requester_did: {_eq: \"probe\"}, request_id: {_gt: \"probe\"}",
        ),
        "@public-created-keyset" => gents::session::public_request_filter(&format!(
            "{}, created_at: {{_gte: \"2026-10-16T00:00:00Z\"}}, request_id: {{_gt: \"probe\"}}",
            session(Some("probe"))
        )),
        literal => literal.to_owned(),
    }
}

fn has_order_node(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.keys().any(|key| key == "orderNode") || fields.values().any(has_order_node)
        }
        Value::Array(values) => values.iter().any(has_order_node),
        _ => false,
    }
}

fn indexes_leave_limits_above_filtering(value: &Value) -> bool {
    fn collect(value: &Value, checks: &mut Vec<bool>) {
        match value {
            Value::Object(fields) => {
                if fields.contains_key("indexName") {
                    checks.push(
                        fields.get("indexLimit").is_some_and(Value::is_null)
                            && fields.get("indexOffset").and_then(Value::as_u64) == Some(0),
                    );
                }
                for child in fields.values() {
                    collect(child, checks);
                }
            }
            Value::Array(children) => {
                for child in children {
                    collect(child, checks);
                }
            }
            _ => {}
        }
    }
    let mut checks = Vec::new();
    collect(value, &mut checks);
    !checks.is_empty() && checks.into_iter().all(|check| check)
}

fn uses_index(value: &Value, index: &str) -> bool {
    match value {
        Value::Object(fields) => {
            fields.get("indexName").and_then(Value::as_str) == Some(index)
                || fields.values().any(|value| uses_index(value, index))
        }
        Value::Array(values) => values.iter().any(|value| uses_index(value, index)),
        _ => false,
    }
}

#[test]
fn extracts_multiline_formats_and_ignores_test_modules() {
    let mut literals = Literals::default();
    literals.visit_file(&syn::parse_file(r###"
        fn f() -> String { format!(r#"{{ AgentRequest(
            filter: {{ node_did: {{ _eq: "{owner}" }} }},
            order: {{request_id: ASC}}, limit: {limit}) {{ _docID }} }}"#) }
        #[cfg(test)] mod tests { fn fixture() { let _ = "{ Goal(order: {created_at: ASC}, limit: 1) {_docID} }"; } }
    "###).unwrap());
    let found: Vec<_> = literals
        .0
        .into_iter()
        .flat_map(|(literal, formatted)| reads("fixture.rs", &literal, formatted))
        .collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].collection, "AgentRequest");
    assert!(found[0].arguments.contains("$GENTS_owner$"));
    assert!(found[0].arguments.contains("$GENTS_limit$"));
}

#[test]
fn extracts_concatenated_query_literals_and_keeps_latest_source_files() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("crates/sample/src");
    std::fs::create_dir_all(&source).unwrap();
    let text = r###"fn query() -> String { format!(concat!(
        "{{ AgentRequest(order: {{ request_id: ASC }}, ",
        "limit: {limit}) {{ _docID }} }}")) }"###;
    std::fs::write(source.join("latest.rs"), text).unwrap();
    std::fs::write(source.join("query_tests.rs"), text).unwrap();
    let found = inventory(dir.path());
    assert_eq!(found.len(), 1);
    let read = found.first().unwrap();
    assert_eq!(read.path, "crates/sample/src/latest.rs");
    assert!(read.arguments.contains("$GENTS_limit$"));
}

#[test]
fn extraction_preserves_whitespace_and_parentheses_inside_values() {
    let found = reads(
        "fixture",
        r#"{AgentRequest(
        filter: {content: {_eq: "a  (b)"}}, order: {created_at: DESC}, limit: 1
    ) {_docID}}"#,
        false,
    );
    assert_eq!(found.len(), 1);
    assert!(found[0].arguments.contains(r#"_eq: "a  (b)""#));
}

#[test]
fn extracts_javascript_templates_past_regular_expressions_and_comments() {
    let source = r###"
        // `AgentRequest(order: {request_id: ASC}, limit: 1)`
        const quote = /['"`]/;
        const query = `query { AgentRequest(
            filter: {node_did: {_eq: "${owner}"}}, order: {request_id: ASC}, limit: ${limit}
        ) { _docID } }`;
    "###;
    let found: Vec<_> = javascript_literals(Path::new("fixture.ts"), source)
        .into_iter()
        .flat_map(|(literal, formatted)| reads("fixture", &literal, formatted))
        .collect();
    assert_eq!(found.len(), 1);
    assert!(found[0].arguments.contains("$GENTS_owner$"));
    assert!(found[0].arguments.contains("$GENTS_limit$"));
}

#[test]
fn extracts_python_multiline_queries_and_formatted_values() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("query.py");
    std::fs::write(
        &path,
        r###"
# query = 'AgentRequest(order: {request_id: ASC}, limit: 1)'
query = f"""{{ AgentRequest(order: {{request_id: ASC}}, limit: {limit}) {{_docID}} }}"""
"###,
    )
    .unwrap();
    let found: Vec<_> = python_literals(&path)
        .into_iter()
        .flat_map(|(literal, formatted)| reads("fixture", &literal, formatted))
        .collect();
    assert_eq!(found.len(), 1);
    assert!(found[0].arguments.contains("$GENTS_limit$"));
}

/// Pinned DefraDB can bound an ordered index scan before its residual filter;
/// an explicit order node keeps the result limit after filtering. Only exact
/// templates whose filters constrain complete index prefixes bypass that guard.
#[tokio::test]
async fn ordered_bounded_reads_keep_filtering_above_unbounded_scans() {
    let proofs = gents_lean_contract::proofs_dir().expect("runtime source anchor");
    let root = proofs.ancestors().nth(3).expect("repository proofs layout");
    let actual = inventory(root);
    let screens: Vec<Screen> =
        serde_json::from_str(include_str!("../fixtures/ordered_read_plans.json")).unwrap();
    let expected: BTreeSet<_> = screens.iter().map(|screen| screen.read.clone()).collect();
    assert_eq!(expected.len(), screens.len(), "duplicate query screens");
    let added: Vec<_> = actual.difference(&expected).collect();
    let removed: Vec<_> = expected.difference(&actual).collect();
    assert!(
        added.is_empty() && removed.is_empty(),
        "query screen inventory changed\nadded: {}\nremoved: {}",
        serde_json::to_string_pretty(&added).unwrap(),
        serde_json::to_string_pretty(&removed).unwrap()
    );
    let dir = tempfile::tempdir().unwrap();
    let identity = gents::KeyIdentity::load_or_create(dir.path().join("node.key"), None).unwrap();
    let node = gents::defra_node::EmbeddedNode::builder()
        .with_node_identity_did(gents::NodeIdentity::did(&identity))
        .data_path(dir.path().join("db"))
        .build()
        .await
        .unwrap();
    gents::ensure_runtime_schemas(&node).await.unwrap();
    let unsafe_query = r#"query @explain { AgentRequest(filter: {request_id: {_eq: "probe"}, content: {_eq: "probe"}}, order: {request_id: ASC}, limit: 1) {_docID} }"#;
    let unsafe_plan = gents::graphql::graphql_with_transaction_retry(
        &node,
        unsafe_query,
        "checking query screen discrimination",
    )
    .await
    .unwrap();
    assert!(unsafe_plan.errors.is_empty(), "{unsafe_plan:?}");
    assert!(
        unsafe_plan
            .data
            .as_ref()
            .is_some_and(indexes_leave_limits_above_filtering),
        "residual filters must run before an index page is bounded: {unsafe_plan:?}"
    );
    let exceptions: Vec<OrderedIndexException> = serde_json::from_str(include_str!(
        "../fixtures/ordered_read_index_exceptions.json"
    ))
    .unwrap();
    assert_eq!(
        exceptions
            .iter()
            .map(|exception| &exception.read)
            .collect::<BTreeSet<_>>()
            .len(),
        exceptions.len(),
        "duplicate ordered-index exceptions"
    );
    let mut used_exceptions = BTreeSet::new();
    let mut failures = Vec::new();
    for screen in screens {
        assert!(
            !screen.bindings.is_empty(),
            "missing bindings: {:?}",
            screen.read
        );
        for bindings in screen.bindings {
            let mut collection = screen.read.collection.clone();
            let mut arguments = screen.read.arguments.clone();
            for (name, value) in bindings {
                let marker = format!("$GENTS_{name}$");
                assert!(
                    collection.contains(&marker) || arguments.contains(&marker),
                    "unused binding {name}: {:?}",
                    screen.read
                );
                let value = binding_value(&value);
                collection = collection.replace(&marker, &value);
                arguments = arguments.replace(&marker, &value);
            }
            assert!(
                !collection.contains("$GENTS_") && !arguments.contains("$GENTS_"),
                "unbound query: {:?}",
                screen.read
            );
            let query = format!("query @explain {{ {collection}({arguments}) {{ _docID }} }}");
            let response = gents::graphql::graphql_with_transaction_retry(
                &node,
                &query,
                "screening ordered read plans",
            )
            .await;
            match response {
                Ok(response) if response.errors.is_empty() => {
                    let sorted = response.data.as_ref().is_some_and(has_order_node);
                    if !sorted
                        && !response
                            .data
                            .as_ref()
                            .is_some_and(indexes_leave_limits_above_filtering)
                    {
                        let exception = exceptions
                            .iter()
                            .find(|exception| exception.read == screen.read);
                        if let Some(exception) = exception.filter(|exception| {
                            response
                                .data
                                .as_ref()
                                .is_some_and(|plan| uses_index(plan, &exception.index))
                        }) {
                            used_exceptions.insert(exception.read.clone());
                        } else {
                            failures.push(format!("{}\n{query}\n{response:?}", screen.read.path));
                        }
                    }
                }
                result => failures.push(format!("{}\n{query}\n{result:?}", screen.read.path)),
            }
        }
    }
    node.shutdown().await;
    for exception in exceptions {
        if !used_exceptions.contains(&exception.read) {
            failures.push(format!(
                "remove obsolete ordered-index exception: {:?}",
                exception.read
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
