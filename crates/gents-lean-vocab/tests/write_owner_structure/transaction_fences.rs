use super::fences::FENCES;
use super::support::{cfg_test, named_exceptions, parse_production, production_sources, repo_root};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{ImplItemFn, ItemFn, ItemImpl, ItemMod};

const MODEL: &str = "crates/gents/proofs/Proofs/Store/TransactionFence.lean";

/// The fence kinds of Lean `TransactionFence.DeclaredFence`.
const KINDS: &[&str] = &[
    "CommonWrite",
    "PointRead",
    "UniqueIndex",
    "SingleWriter",
    "Idempotent",
];

/// The production schema directories whose `@index(unique: true)` keys make
/// an equality filter a read by id.
const SCHEMAS: &[&str] = &[
    "crates/gents-schemas/schemas/agent",
    "crates/gents-protocol/schemas/inference",
    "crates/gents-protocol/schemas/services",
];

/// Each collection's unique keys, by field set; `_docID` is unique in every
/// collection.
pub(crate) struct UniqueKeys(BTreeMap<String, Vec<BTreeSet<String>>>);

static TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"type\s+(\w+)([^{]*)\{([^}]*)\}").expect("type regex"));
static INDEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@index\(([^)]*)\)").expect("index regex"));
static INDEX_FIELDS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"fields:\s*\[([^\]]*)\]").expect("fields regex"));
static FIELD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*(\w+)\s*:[^@]*(@.*)?$").expect("field regex"));
static QUOTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""(\w+)""#).expect("quoted regex"));
static UNIQUE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"unique:\s*true").expect("unique regex"));

impl UniqueKeys {
    pub(crate) fn parse(sdl: &str) -> Self {
        let mut keys = BTreeMap::<String, Vec<BTreeSet<String>>>::new();
        let sdl = sdl
            .lines()
            .map(|line| line.split('#').next().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        for ty in TYPE.captures_iter(&sdl) {
            let entry = keys.entry(ty[1].to_owned()).or_default();
            for index in INDEX.captures_iter(&ty[2]) {
                if let (true, Some(fields)) =
                    (UNIQUE.is_match(&index[1]), INDEX_FIELDS.captures(&index[1]))
                {
                    entry.push(
                        QUOTED
                            .captures_iter(&fields[1])
                            .map(|f| f[1].to_owned())
                            .collect(),
                    );
                }
            }
            for line in ty[3].lines() {
                let Some(field) = FIELD.captures(line) else {
                    continue;
                };
                let Some(directives) = field.get(2) else {
                    continue;
                };
                for index in INDEX.captures_iter(directives.as_str()) {
                    if UNIQUE.is_match(&index[1]) && !index[1].contains("fields") {
                        entry.push(BTreeSet::from([field[1].to_owned()]));
                    }
                }
            }
        }
        Self(keys)
    }

    fn load() -> Self {
        let root = repo_root();
        let mut sdl = String::new();
        for directory in SCHEMAS {
            let mut files = std::fs::read_dir(root.join(directory))
                .unwrap_or_else(|error| panic!("read {directory}: {error}"))
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "graphql")
                })
                .collect::<Vec<_>>();
            files.sort();
            for file in files {
                sdl.push_str(&std::fs::read_to_string(&file).expect("read schema"));
                sdl.push('\n');
            }
        }
        Self::parse(&sdl)
    }

    fn collection(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    /// Whether an equality filter on `fields` finds at most one document of
    /// `collection`, so the store validates it like a read by id.
    fn point_read(&self, collection: Option<&str>, fields: &BTreeSet<String>) -> bool {
        fields.contains("_docID")
            || collection
                .and_then(|name| self.0.get(name))
                .is_some_and(|keys| {
                    keys.iter()
                        .any(|key| !key.is_empty() && key.is_subset(fields))
                })
    }
}

fn camel(snake: &str) -> String {
    snake
        .split('_')
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        })
        .collect()
}

fn matching_close(text: &str, open_at: usize, open: char, close: char) -> usize {
    let mut depth = 0usize;
    for (offset, character) in text[open_at..].char_indices() {
        if character == open {
            depth += 1;
        } else if character == close {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return open_at + offset;
            }
        }
    }
    text.len()
}

static QUERY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Query::").expect("query regex"));
static FILTER_OP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Filter::(\w+)\(").expect("filter op regex"));
static FILTER_EQ: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"Filter::eq\(\s*([\w:]+)").expect("filter eq regex"));
static GRAPHQL_READ: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w])([A-Z][A-Za-z0-9]*)\s*\(").expect("read regex"));
static GRAPHQL_LIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\s*([A-Z][A-Za-z0-9]*)\s*\{").expect("list regex"));
static GRAPHQL_FILTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"filter\s*:\s*\{").expect("filter regex"));
static GRAPHQL_EQ: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(\w+)\s*:\s*\{\s*_eq\b").expect("eq regex"));
static GRAPHQL_RANGE_OP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b_(ne|in|nin|gt|ge|gte|lt|le|lte|like|ilike|nlike|and|or|not)\b")
        .expect("range op regex")
});
static DOC_ID_ARGUMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bdocIDs?\s*:").expect("doc id regex"));
static WRITES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\bmutation\b|\b(Create|Update|Delete)::|\b(create|update|delete|upsert)_[A-Z]|\badd_\w*relationship\b|\bdelete_\w*relationship\b",
    )
    .expect("write regex")
});

/// The collection a field-path alias (`fields::agent_request as request`)
/// names, from its last import before `at`.
fn field_module(keys: &UniqueKeys, file: &str, at: usize, alias: &str) -> Option<String> {
    if keys.collection(&camel(alias)) {
        return Some(camel(alias));
    }
    let import = Regex::new(&format!(r"\b(\w+)\s+as\s+{}\b", regex::escape(alias))).ok()?;
    let mut at = at.min(file.len());
    while !file.is_char_boundary(at) {
        at -= 1;
    }
    import
        .captures_iter(&file[..at])
        .map(|capture| camel(&capture[1]))
        .filter(|collection| keys.collection(collection))
        .last()
}

/// Why one read of `unit` (at `base` in `file`) is a range read: a count, an
/// order, a list, a filter with an operator other than equality, or an
/// equality filter on no unique key. A read by id or by a unique key is not.
fn range_reads(keys: &UniqueKeys, file: &str, base: usize, unit: &str) -> Vec<String> {
    let mut reasons = Vec::new();
    for found in QUERY.find_iter(unit) {
        let rest = &unit[found.start()..];
        let segment = &rest[..rest.find(".render()").unwrap_or(rest.len())];
        if segment.contains(".doc_id(") {
            continue;
        }
        if segment.starts_with("Query::count") || segment.contains(".counted()") {
            reasons.push("count".to_owned());
            continue;
        }
        if segment.contains(".order_by(") {
            reasons.push("order".to_owned());
            continue;
        }
        let operators = FILTER_OP
            .captures_iter(segment)
            .map(|capture| capture[1].to_owned())
            .collect::<Vec<_>>();
        if operators.is_empty() {
            reasons.push("list".to_owned());
            continue;
        }
        if operators.iter().any(|operator| operator != "eq") {
            reasons.push(format!("filter {}", operators.join(",")));
            continue;
        }
        let mut fields = BTreeSet::new();
        let mut collection = None;
        for path in FILTER_EQ.captures_iter(segment) {
            let parts = path[1].split("::").collect::<Vec<_>>();
            let field = parts.last().copied().unwrap_or_default().to_lowercase();
            fields.insert(if field == "doc_id" {
                "_docID".to_owned()
            } else {
                field
            });
            if collection.is_none() && parts.len() >= 2 {
                collection = field_module(keys, file, base + found.start(), parts[parts.len() - 2]);
            }
        }
        if !keys.point_read(collection.as_deref(), &fields) {
            reasons.push(format!(
                "filter {}({})",
                collection.unwrap_or_default(),
                fields.into_iter().collect::<Vec<_>>().join(",")
            ));
        }
    }
    for read in GRAPHQL_READ.captures_iter(unit) {
        let name = read.get(1).expect("collection");
        if !keys.collection(name.as_str()) {
            continue;
        }
        let open = name.end() + unit[name.end()..].find('(').unwrap_or_default();
        let arguments = &unit[open..=matching_close(unit, open, '(', ')').min(unit.len() - 1)];
        if DOC_ID_ARGUMENT.is_match(arguments) {
            continue;
        }
        if arguments.contains("order:") {
            reasons.push(format!("order {}", name.as_str()));
            continue;
        }
        let Some(filter) = GRAPHQL_FILTER.find(arguments) else {
            reasons.push(format!("list {}", name.as_str()));
            continue;
        };
        let open = filter.end() - 1;
        let object =
            &arguments[open..=matching_close(arguments, open, '{', '}').min(arguments.len() - 1)];
        if GRAPHQL_RANGE_OP.is_match(object) {
            reasons.push(format!("filter {}", name.as_str()));
            continue;
        }
        let fields = GRAPHQL_EQ
            .captures_iter(object)
            .map(|capture| capture[1].to_owned())
            .collect::<BTreeSet<_>>();
        if !keys.point_read(Some(name.as_str()), &fields) {
            reasons.push(format!(
                "filter {}({})",
                name.as_str(),
                fields.into_iter().collect::<Vec<_>>().join(",")
            ));
        }
    }
    for list in GRAPHQL_LIST.captures_iter(unit) {
        if keys.collection(&list[1]) {
            reasons.push(format!("list {}", &list[1]));
        }
    }
    if unit.contains("_count(") {
        reasons.push("count".to_owned());
    }
    reasons
}

/// Comments removed and `format!` brace escapes undone, so the text reads as
/// the GraphQL it renders.
fn normalized(text: &str) -> String {
    text.lines()
        .map(|line| match line.find("//") {
            Some(at) if !line[..at].ends_with(':') && line[..at].matches('"').count() % 2 == 0 => {
                &line[..at]
            }
            _ => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace("{{", "{")
        .replace("}}", "}")
}

/// Whether `unit` reads a range and writes.
fn reads_a_range_and_writes(keys: &UniqueKeys, file: &str, base: usize, unit: &str) -> bool {
    WRITES.is_match(unit) && !range_reads(keys, file, base, unit).is_empty()
}

fn byte_offset(source: &str, line: usize, column: usize) -> usize {
    source
        .lines()
        .take(line.saturating_sub(1))
        .map(|text| text.len() + 1)
        .sum::<usize>()
        + source
            .lines()
            .nth(line.saturating_sub(1))
            .map_or(0, |text| {
                text.char_indices()
                    .nth(column)
                    .map_or(text.len(), |(at, _)| at)
            })
}

fn span_range(source: &str, span: proc_macro2::Span) -> (usize, usize) {
    let (start, end) = (span.start(), span.end());
    (
        byte_offset(source, start.line, start.column),
        byte_offset(source, end.line, end.column).min(source.len()),
    )
}

/// The production units that run inside a transaction: a function that takes
/// a `ConfigApplyTxn`, and the callback of a `transact_*` call other than a
/// read-only one, named after its enclosing function.
struct Units<'a> {
    keys: &'a UniqueKeys,
    readers: &'a BTreeSet<String>,
    source: &'a str,
    function: Vec<String>,
    found: Vec<String>,
}

impl Units<'_> {
    fn unit(&self, span: proc_macro2::Span) -> bool {
        let (start, end) = span_range(self.source, span);
        let unit = normalized(&self.source[start..end]);
        reads_a_range_and_writes(self.keys, self.source, start, &unit)
            || (WRITES.is_match(&unit) && calls_a_reader(self.readers, &unit))
    }

    fn function(
        &mut self,
        name: String,
        signature: &syn::Signature,
        body: &syn::Block,
        visit_body: impl FnOnce(&mut Self),
    ) {
        let (start, end) = span_range(self.source, signature.span());
        if self.source[start..end].contains("ConfigApplyTxn") && self.unit(body.span()) {
            self.found.push(name.clone());
        }
        self.function.push(name);
        visit_body(self);
        self.function.pop();
    }

    fn call(&mut self, method: &str, arguments: proc_macro2::Span) {
        if method.starts_with("transact_")
            && !method.starts_with("transact_readonly")
            && self.unit(arguments)
        {
            let enclosing = self.function.last().cloned().unwrap_or_default();
            self.found.push(format!("{enclosing}::{method}"));
        }
    }
}

impl<'ast> Visit<'ast> for Units<'_> {
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
            self.function(
                item.sig.ident.to_string(),
                &item.sig,
                &item.block,
                |units| {
                    visit::visit_item_fn(units, item);
                },
            );
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if !cfg_test(&item.attrs) {
            self.function(
                item.sig.ident.to_string(),
                &item.sig,
                &item.block,
                |units| {
                    visit::visit_impl_item_fn(units, item);
                },
            );
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.call(&call.method.to_string(), call.args.span());
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref() {
            if let Some(last) = path.path.segments.last() {
                self.call(&last.ident.to_string(), call.args.span());
            }
        }
        visit::visit_expr_call(self, call);
    }
}

static CALL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b(\w+)\s*\(").expect("call regex"));

fn calls_a_reader(readers: &BTreeSet<String>, unit: &str) -> bool {
    CALL.captures_iter(unit)
        .any(|call| readers.contains(&call[1]))
}

/// The production functions that read a range and write nothing, which a
/// unit calls to read for it: a function taking a `ConfigApplyTxn` or one
/// building a query (`*_query`), named with more than one word.
struct Readers<'a> {
    keys: &'a UniqueKeys,
    source: &'a str,
    found: BTreeSet<String>,
}

impl Readers<'_> {
    fn function(&mut self, name: String, signature: &syn::Signature, body: &syn::Block) {
        let (start, end) = span_range(self.source, signature.span());
        let builds = self.source[start..end].contains("ConfigApplyTxn") || name.ends_with("_query");
        let (start, end) = span_range(self.source, body.span());
        let body = normalized(&self.source[start..end]);
        if builds
            && name.contains('_')
            && !WRITES.is_match(&body)
            && !range_reads(self.keys, self.source, start, &body).is_empty()
        {
            self.found.insert(name);
        }
    }
}

impl<'ast> Visit<'ast> for Readers<'_> {
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
            self.function(item.sig.ident.to_string(), &item.sig, &item.block);
            visit::visit_item_fn(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast ImplItemFn) {
        if !cfg_test(&item.attrs) {
            self.function(item.sig.ident.to_string(), &item.sig, &item.block);
            visit::visit_impl_item_fn(self, item);
        }
    }
}

fn units_reading_a_range_and_writing() -> BTreeMap<String, usize> {
    let keys = UniqueKeys::load();
    let root = repo_root();
    let production = parse_production(&root, production_sources(&root))
        .into_iter()
        .map(|(relative, syntax)| {
            let source = std::fs::read_to_string(root.join(&relative)).expect("read Rust source");
            (relative, syntax, source)
        })
        .collect::<Vec<_>>();
    let mut readers = BTreeSet::new();
    for (_, syntax, source) in &production {
        let mut found = Readers {
            keys: &keys,
            source,
            found: BTreeSet::new(),
        };
        found.visit_file(syntax);
        readers.extend(found.found);
    }
    let mut sites = BTreeMap::new();
    for (relative, syntax, source) in &production {
        let source = source.as_str();
        let mut units = Units {
            keys: &keys,
            readers: &readers,
            source,
            function: Vec::new(),
            found: Vec::new(),
        };
        units.visit_file(syntax);
        for name in units.found {
            *sites.entry(format!("{relative}::{name}")).or_insert(0) += 1;
        }
    }
    sites
}

fn malformed_fence(fence: &str) -> bool {
    let Some((kind, rest)) = fence.split_once('(') else {
        return true;
    };
    let named = rest
        .split_once("):")
        .map_or(rest.strip_suffix(')'), |(named, _)| Some(named));
    !KINDS.contains(&kind) || named.is_none_or(|named| named.trim().is_empty())
}

/// A production transaction that reads by filter, list, count or order and
/// then writes decides on rows the store does not validate at its commit, so
/// each names the fence it rests on in the vocabulary of Lean
/// `TransactionFence.DeclaredFence`. A unit reads a range itself or through a
/// function that only reads one (`Readers`). An equality filter on a unique
/// key of the schema, or on `_docID`, is a read by id and needs no entry.
#[test]
fn every_range_read_then_write_names_its_fence() {
    let malformed = FENCES
        .iter()
        .filter(|(_, _, fence)| malformed_fence(fence))
        .map(|(site, _, fence)| format!("{site}: `{fence}` is not one of {KINDS:?}(..)"))
        .collect::<Vec<_>>();
    assert!(
        malformed.is_empty(),
        "fences are named in the vocabulary of {MODEL}:\n{}",
        malformed.join("\n")
    );
    named_exceptions(
        "transaction fence",
        FENCES,
        &units_reading_a_range_and_writing(),
        "transaction unit(s) that read by filter, list, count or order and write",
        &format!(
            "name the fence its decision rests on as CommonWrite(..), PointRead(..), UniqueIndex(..), SingleWriter(..) or Idempotent(..) from {MODEL}, verified by reading the unit"
        ),
    );
}

#[test]
fn range_reads_are_told_from_reads_by_id_and_unique_key() {
    let keys = UniqueKeys::parse(
        r#"type AgentRequest @index(fields: ["node_did", "request_id"], unique: true) {
            retry_key: String @index(unique: true) @immutable
            request_id: String @index
            node_did: String
        }"#,
    );
    let ranged = |unit: &str| {
        let unit = normalized(unit);
        reads_a_range_and_writes(&keys, &unit, 0, &unit)
    };
    let write = r#"txn.execute("mutation { update_AgentRequest(docID: \"a\") { _docID } }")"#;
    let typed = "use gents_protocol::document_query::fields::agent_request as fields;";
    assert!(ranged(&format!(
        "{typed} Query::select(x).filter(Filter::eq(fields::REQUEST_ID, id)).render(); {write}"
    )));
    assert!(!ranged(&format!(
        "{typed} Query::select(x).filter(Filter::eq(fields::RETRY_KEY, key)).limit(2).render(); {write}"
    )));
    assert!(!ranged(&format!(
        "{typed} Query::select(x).filter(Filter::eq(fields::NODE_DID, n)).filter(Filter::eq(fields::REQUEST_ID, r)).render(); {write}"
    )));
    assert!(!ranged(&format!(
        "{typed} Query::select(x).filter(Filter::eq(fields::DOC_ID, d)).render(); {write}"
    )));
    assert!(ranged(&format!(
        "{typed} Query::select(x).filter(Filter::eq(fields::RETRY_KEY, k).and(Filter::ne(fields::NODE_DID, n))).render(); {write}"
    )));
    assert!(ranged(&format!("{typed} Query::count().render(); {write}")));
    assert!(ranged(&format!(
        "format!(\"{{{{ AgentRequest(filter: {{{{request_id: {{{{_eq: \\\"r\\\"}}}}}}}}) {{{{ _docID }}}} }}}}\"); {write}"
    )));
    assert!(!ranged(&format!(
        "\"{{ AgentRequest(filter: {{retry_key: {{_eq: \\\"k\\\"}}}}) {{ _docID }} }}\"; {write}"
    )));
    assert!(!ranged(&format!(
        "\"{{ AgentRequest(docID: \\\"a\\\") {{ n }} }}\"; {write}"
    )));
    assert!(ranged(&format!(
        "\"{{ AgentRequest(filter: {{retry_key: {{_in: [\\\"k\\\"]}}}}) {{ _docID }} }}\"; {write}"
    )));
    assert!(ranged(&format!(
        "\"{{ AgentRequest {{ _docID }} }}\"; {write}"
    )));
    assert!(!ranged(
        "\"{ AgentRequest(filter: {request_id: {_eq: \\\"r\\\"}}) { n } }\" // mutation"
    ));
    assert!(!ranged(&format!(
        "let kept = rows.iter().filter(|row| row.live).count(); {write}"
    )));
    let readers = BTreeSet::from(["pending_rows_in_txn".to_owned()]);
    assert!(calls_a_reader(
        &readers,
        "let rows = pending_rows_in_txn(txn, id).await?;"
    ));
    assert!(!calls_a_reader(
        &readers,
        "let rows = pending_rows(txn, id).await?;"
    ));
    assert!(!malformed_fence("UniqueIndex(AgentRequest(retry_key))"));
    assert!(!malformed_fence(
        "CommonWrite(Goal): every continuation writes the Goal"
    ));
    assert!(malformed_fence("unique index on retry_key"));
    assert!(malformed_fence("Serial(one at a time)"));
    assert!(malformed_fence("PointRead()"));
}
