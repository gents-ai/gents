use super::support::{
    cfg_test, dependency_names, parse_production, path_name, production_sources, repo_root,
    rust_sources, toml_file,
};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, ExprMethodCall, ItemConst, ItemFn, ItemImpl, ItemMod};

const OWNER_FILES: &[&str] = &[
    "crates/gents-store/src/config_client/graphql.rs",
    "crates/gents-store/src/config_client/retry.rs",
    "crates/gents-store/src/config_client/txn/auto_commit.rs",
    "crates/gents-store/src/config_client/txn/execute.rs",
    "crates/gents-store/src/config_client/txn/transact_entry.rs",
    "crates/gents-protocol/src/graphql/transport.rs",
];

const READ_OWNER_FILES: &[&str] = &[
    "crates/gents-store/src/config_client/txn/auto_commit.rs",
    "crates/gents-store/src/graphql.rs",
    "crates/gents-store/src/node_scope/context_calls.rs",
];

const NODE_ONLY_METHODS: &[&str] = &["execute_as", "execute_request", "runner"];

const FORBIDDEN_FUNCTIONS: &[&str] = &[
    "begin_apply_txn",
    "execute_committed",
    "graphql_mutation_with_transaction_retry",
    "graphql_mutation_response_with_transaction_retry",
    "graphql_mutation_once_with_executor",
    "retry_terminal_persistence_operation",
    "is_defradb_transaction_conflict_text",
    "defradb_conflict_retry_backoff",
    "execute_graphql_async_with_tx",
    "execute_graphql_async",
    "execute_graphql_encoded",
];

const FORBIDDEN_METHODS: &[&str] = &[
    "begin_txn",
    "commit_txn",
    "rollback_txn",
    "execute_request_in_txn",
    "begin_apply_txn",
];

fn receiver_name(expression: &Expr) -> Option<String> {
    let Expr::Path(path) = expression else {
        return None;
    };
    path.path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
}

fn names_mutation(expression: &Expr) -> bool {
    match expression {
        Expr::Reference(reference) => names_mutation(&reference.expr),
        Expr::Paren(paren) => names_mutation(&paren.expr),
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident.to_string().contains("mutation")),
        Expr::MethodCall(call) => call.method.to_string().contains("mutation"),
        Expr::Macro(invocation) => invocation
            .mac
            .tokens
            .to_string()
            .to_ascii_lowercase()
            .contains("mutation"),
        Expr::Lit(literal) => match &literal.lit {
            syn::Lit::Str(value) => value.value().to_ascii_lowercase().contains("mutation"),
            _ => false,
        },
        _ => false,
    }
}

#[derive(Default)]
struct WriteVisitor {
    violations: Vec<String>,
}

impl<'ast> Visit<'ast> for WriteVisitor {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        let name = item.sig.ident.to_string();
        if FORBIDDEN_FUNCTIONS.contains(&name.as_str()) {
            self.violations.push(format!("defines `{name}`"));
        }
        visit::visit_item_fn(self, item);
    }

    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        if item.ident == "DEFRA_DB_CONFLICT_MAX_RETRIES" {
            self.violations
                .push("defines feature-local conflict retry policy".to_string());
        }
        visit::visit_item_const(self, item);
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Some(name) = path_name(call) {
            let leaf = name.rsplit("::").next().unwrap_or(&name);
            if FORBIDDEN_FUNCTIONS.contains(&leaf)
                || (leaf == "begin_local" && name.contains("ConfigApplyTxn"))
            {
                self.violations.push(format!("calls `{name}`"));
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        let method = call.method.to_string();
        let receiver = receiver_name(&call.receiver);
        let transaction_receiver = receiver
            .as_deref()
            .is_some_and(|name| name == "txn" || name == "transaction" || name.ends_with("_txn"));
        if FORBIDDEN_METHODS.contains(&method.as_str())
            || ((method == "commit" || method == "discard") && transaction_receiver)
            || (method == "execute"
                && !transaction_receiver
                && call.args.iter().any(names_mutation))
        {
            self.violations.push(format!("calls raw `.{method}(...)`"));
        }
        visit::visit_expr_method_call(self, call);
    }
}

#[test]
fn production_defradb_writes_have_one_owner() {
    let root = repo_root();
    let mut violations = Vec::new();
    for (relative, syntax) in parse_production(&root, production_sources(&root)) {
        if OWNER_FILES.contains(&relative.as_str()) {
            continue;
        }
        violations.extend(
            write_violations(&syntax)
                .into_iter()
                .map(|violation| format!("{relative}: {violation}")),
        );
    }
    assert!(
        violations.is_empty(),
        "production DefraDB writes bypass ConfigAccess::write/transact_write:\n{}",
        violations.join("\n")
    );
}

fn defradb_member_directories(root: &Path) -> Vec<PathBuf> {
    let workspace = toml_file(&root.join("Cargo.toml"));
    let inherited = workspace["workspace"]
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let members = workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members")
        .iter()
        .map(|member| root.join(member.as_str().expect("member path")))
        .map(|directory| {
            let manifest = toml_file(&directory.join("Cargo.toml"));
            let name = manifest["package"]["name"]
                .as_str()
                .expect("package name")
                .to_string();
            (name, (directory, dependency_names(&manifest, &inherited)))
        })
        .collect::<BTreeMap<_, _>>();
    let mut reaching = BTreeSet::from(["defra-node".to_string()]);
    loop {
        let before = reaching.len();
        for (name, (_, dependencies)) in &members {
            if !dependencies.is_disjoint(&reaching) {
                reaching.insert(name.clone());
            }
        }
        if reaching.len() == before {
            break;
        }
    }
    members
        .into_iter()
        .filter(|(name, _)| reaching.contains(name))
        .map(|(_, (directory, _))| directory)
        .collect()
}

fn mentions_embedded_node(ty: &syn::Type) -> bool {
    struct Finder(bool);
    impl<'ast> Visit<'ast> for Finder {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            self.0 |= path
                .segments
                .iter()
                .zip(path.segments.iter().skip(1))
                .any(|(module, ty)| module.ident == "defra_node" && ty.ident == "Node");
            visit::visit_path(self, path);
        }

        fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
            self.0 |= segment.ident == "EmbeddedNode";
            visit::visit_path_segment(self, segment);
        }
    }
    let mut finder = Finder(false);
    finder.visit_type(ty);
    finder.0
}

fn return_mentions_embedded_node(output: &syn::ReturnType) -> bool {
    matches!(output, syn::ReturnType::Type(_, ty) if mentions_embedded_node(ty))
}

struct NodeNames {
    fields: BTreeSet<String>,
    accessors: BTreeSet<String>,
    bindings: BTreeSet<String>,
}

impl NodeNames {
    fn conventional() -> Self {
        let node = BTreeSet::from(["node".to_string()]);
        Self {
            fields: node.clone(),
            accessors: node.clone(),
            bindings: node,
        }
    }
}

impl<'ast> Visit<'ast> for NodeNames {
    fn visit_field(&mut self, field: &'ast syn::Field) {
        if let Some(name) = &field.ident {
            if mentions_embedded_node(&field.ty) {
                self.fields.insert(name.to_string());
            }
        }
        visit::visit_field(self, field);
    }

    fn visit_signature(&mut self, signature: &'ast syn::Signature) {
        if return_mentions_embedded_node(&signature.output) {
            self.accessors.insert(signature.ident.to_string());
        }
        visit::visit_signature(self, signature);
    }

    fn visit_pat_type(&mut self, pattern: &'ast syn::PatType) {
        if let syn::Pat::Ident(binding) = pattern.pat.as_ref() {
            if mentions_embedded_node(&pattern.ty) {
                self.bindings.insert(binding.ident.to_string());
            }
        }
        visit::visit_pat_type(self, pattern);
    }
}

struct ReadVisitor<'names> {
    workspace: &'names NodeNames,
    file: NodeNames,
    aliases: Vec<BTreeSet<String>>,
    node_impl: Vec<bool>,
    sites: usize,
}

impl<'names> ReadVisitor<'names> {
    fn new(workspace: &'names NodeNames, syntax: &syn::File) -> Self {
        let mut file = NodeNames::conventional();
        file.visit_file(syntax);
        Self {
            workspace,
            file,
            aliases: vec![BTreeSet::new()],
            node_impl: Vec::new(),
            sites: 0,
        }
    }

    fn bound_node(&self, name: &str) -> bool {
        self.file.bindings.contains(name) || self.aliases.iter().any(|scope| scope.contains(name))
    }

    fn scoped(&mut self, visit: impl FnOnce(&mut Self)) {
        self.aliases.push(BTreeSet::new());
        visit(self);
        self.aliases.pop();
    }

    fn node_receiver(&self, expression: &Expr) -> bool {
        match expression {
            Expr::Reference(reference) => self.node_receiver(&reference.expr),
            Expr::Paren(paren) => self.node_receiver(&paren.expr),
            Expr::Unary(unary) => self.node_receiver(&unary.expr),
            Expr::Path(path) if path.path.is_ident("self") => {
                self.node_impl.last().copied().unwrap_or(false)
            }
            Expr::Path(path) => path
                .path
                .get_ident()
                .is_some_and(|ident| self.bound_node(&ident.to_string())),
            Expr::Call(call) if call.args.len() == 1 => {
                path_name(call).is_some_and(|name| name.ends_with("::clone"))
                    && self.node_receiver(&call.args[0])
            }
            Expr::Field(field) => match &field.member {
                syn::Member::Named(ident) => self.workspace.fields.contains(&ident.to_string()),
                syn::Member::Unnamed(_) => false,
            },
            Expr::MethodCall(call) if call.args.is_empty() => {
                let method = call.method.to_string();
                if matches!(method.as_str(), "as_ref" | "clone" | "deref" | "borrow") {
                    self.node_receiver(&call.receiver)
                } else {
                    self.workspace.accessors.contains(&method)
                }
            }
            _ => false,
        }
    }
}

impl<'ast> Visit<'ast> for ReadVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if !cfg_test(&item.attrs) {
            self.scoped(|visitor| visit::visit_item_fn(visitor, item));
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !cfg_test(&item.attrs) {
            self.scoped(|visitor| visit::visit_impl_item_fn(visitor, item));
        }
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        visit::visit_local(self, local);
        let binding = match &local.pat {
            syn::Pat::Ident(binding) => Some(&binding.ident),
            syn::Pat::Type(typed) => match typed.pat.as_ref() {
                syn::Pat::Ident(binding) => Some(&binding.ident),
                _ => None,
            },
            _ => None,
        };
        if let (Some(binding), Some(init)) = (binding, &local.init) {
            if self.node_receiver(&init.expr) {
                self.aliases
                    .last_mut()
                    .expect("function scope")
                    .insert(binding.to_string());
            }
        }
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if cfg_test(&item.attrs) {
            return;
        }
        self.node_impl.push(mentions_embedded_node(&item.self_ty));
        visit::visit_item_impl(self, item);
        self.node_impl.pop();
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Some(name) = path_name(call) {
            let mut segments = name.rsplit("::");
            let method = segments.next().unwrap_or_default();
            let ty = segments.next();
            let is_node = ty == Some("EmbeddedNode")
                || (ty == Some("Node") && segments.next() == Some("defra_node"));
            if is_node && (method == "execute" || NODE_ONLY_METHODS.contains(&method)) {
                self.sites += 1;
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        let method = call.method.to_string();
        if NODE_ONLY_METHODS.contains(&method.as_str())
            || (method == "execute" && self.node_receiver(&call.receiver))
        {
            self.sites += 1;
        }
        visit::visit_expr_method_call(self, call);
    }
}

fn node_execution_sites(workspace: &NodeNames, syntax: &syn::File) -> usize {
    let mut visitor = ReadVisitor::new(workspace, syntax);
    visitor.visit_file(syntax);
    visitor.sites
}

fn write_violations(syntax: &syn::File) -> Vec<String> {
    let mut visitor = WriteVisitor::default();
    visitor.visit_file(syntax);
    visitor.violations
}

#[test]
fn production_defradb_node_execution_stays_in_the_store_owners() {
    let root = repo_root();
    let mut sources = Vec::new();
    for directory in defradb_member_directories(&root) {
        for subdirectory in ["src", "examples"] {
            let path = directory.join(subdirectory);
            if path.is_dir() {
                rust_sources(&path, &mut sources);
            }
        }
    }
    let parsed = parse_production(&root, sources);
    let mut workspace = NodeNames::conventional();
    for (_, syntax) in &parsed {
        workspace.visit_file(syntax);
    }

    let violations = parsed
        .iter()
        .filter(|(relative, _)| !READ_OWNER_FILES.contains(&relative.as_str()))
        .filter_map(|(relative, syntax)| {
            let sites = node_execution_sites(&workspace, syntax);
            (sites > 0).then(|| {
                format!(
                    "{relative}: {sites} direct EmbeddedNode execution site(s); use the config_client/graphql owners"
                )
            })
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "direct DefraDB node execution outside the store owners:\n{}",
        violations.join("\n")
    );
}

fn snippet_sites(source: &str) -> usize {
    let syntax = syn::parse_file(source).expect("parse snippet");
    let mut workspace = NodeNames::conventional();
    workspace.visit_file(&syntax);
    node_execution_sites(&workspace, &syntax)
}

#[test]
fn read_fence_counts_direct_node_execution() {
    for source in [
        "fn f(node: &EmbeddedNode, q: &str) { node.execute(q); }",
        r#"#[cfg(any(test, feature = "production"))] fn f(node: &EmbeddedNode, q: &str) { node.execute(q); }"#,
        "fn f(db: &EmbeddedNode, q: &str) { db.execute(q); }",
        "fn f(db: Arc<defra_node::Node>, q: &str) { db.as_ref().execute(q); }",
        "impl S { fn f(&self, q: &str) { self.node.execute(q); } }",
        "struct S { store: Arc<EmbeddedNode> } impl S { fn f(&self, q: &str) { self.store.execute(q); } }",
        "fn f(node: &EmbeddedNode, q: &str) { node.clone().execute(q); }",
        "fn f(node: &EmbeddedNode, q: &str) { (&*node).execute(q); }",
        "fn f(core: &Core, q: &str) { core.node().execute(q); }",
        "fn f(node: &EmbeddedNode, q: &str) { EmbeddedNode::execute(&node, q); }",
        "fn f(db: &defra_node::Node, q: &str) { defra_node::Node::execute(db, q); }",
        "fn f(node: &EmbeddedNode, q: &str) { let db = node.clone(); db.execute(q); }",
        "fn f(node: Arc<EmbeddedNode>, q: &str) { let db = Arc::clone(&node); db.execute(q); }",
        "impl S { fn f(&self, q: &str) { let db = &self.node; db.execute(q); } }",
        "impl EmbeddedNodeExt for EmbeddedNode { fn f(&self, q: &str) { self.execute(q); } }",
        "fn f(x: &X) { x.runner(); }",
        "fn f(x: &X, context: &C, q: &str) { x.execute_as(context, q); }",
        "fn f(x: &X, request: R) { x.execute_request(request); }",
    ] {
        assert_eq!(snippet_sites(source), 1, "{source}");
    }
}

#[test]
fn read_fence_ignores_owner_wrappers_and_tests() {
    for source in [
        "fn f(executor: &Executor, q: &str) { executor.execute(q); }",
        "fn f(db: &domain::Node, q: &str) { db.execute(q); domain::Node::execute(db, q); }",
        "fn f(access: &ConfigAccess, q: &str) { access.execute(q); }",
        "fn f(access: &ConfigAccess, q: &str) { ConfigAccess::execute(access, q); }",
        "fn f(txn: &ConfigApplyTxn, q: &str) { txn.execute(q); }",
        "impl ConfigAccess { fn f(&self, q: &str) { self.execute(q); } }",
        "fn f(node: &EmbeddedNode) {} fn g(access: &ConfigAccess, q: &str) { let db = access; db.execute(q); }",
        "fn f(node: &EmbeddedNode) { let db = node; } fn g(db: &Access, q: &str) { db.execute(q); }",
        "#[cfg(test)] mod tests { fn f(node: &EmbeddedNode, q: &str) { node.execute(q); } }",
        "#[cfg(test)] fn f(node: &EmbeddedNode, q: &str) { node.execute(q); }",
        r#"#[cfg(any(test, feature = "test-fixtures"))] fn f(node: &EmbeddedNode, q: &str) { node.execute(q); }"#,
    ] {
        assert_eq!(snippet_sites(source), 0, "{source}");
    }
}

#[test]
fn write_fence_flags_macro_mutations() {
    for source in [
        r#"fn f(node: &EmbeddedNode, id: &str) { node.execute(&format!("mutation {{ delete_X(docID: \"{id}\") {{ _docID }} }}")); }"#,
        r#"fn f(node: &EmbeddedNode) { node.execute(concat!("mutation ", "{ delete_X { _docID } }")); }"#,
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(write_violations(&syntax).len(), 1, "{source}");
    }
    let query = syn::parse_file(r#"fn f(node: &EmbeddedNode, id: &str) { node.execute(&format!("{{ X(docID: \"{id}\") {{ _docID }} }}")); }"#)
        .expect("parse snippet");
    assert!(write_violations(&query).is_empty());
}
