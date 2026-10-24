use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, ExprMethodCall, ItemConst, ItemFn, ItemImpl, ItemMod};

const OWNER_FILES: &[&str] = &[
    "crates/gents/src/config_client/graphql.rs",
    "crates/gents/src/config_client/retry.rs",
    "crates/gents/src/config_client/txn.rs",
    "crates/gents-protocol/src/graphql.rs",
];

const READ_OWNER_FILES: &[&str] = &[
    "crates/gents/src/config_client/txn.rs",
    "crates/gents/src/graphql.rs",
];

#[rustfmt::skip]
const NODE_EXECUTE_ALLOWLIST: &[(&str, usize)] = &[
    ("crates/gents-migration/src/materialize.rs", 1),
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
];

const FORBIDDEN_METHODS: &[&str] = &[
    "begin_txn",
    "commit_txn",
    "rollback_txn",
    "execute_request_in_txn",
    "begin_apply_txn",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("gents manifest is under <repo>/crates/gents")
        .to_path_buf()
}

fn rust_sources(root: &Path, output: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_none_or(|name| name != "tests") {
                rust_sources(&path, output);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs")
            && path
                .file_name()
                .is_none_or(|name| !name.to_string_lossy().ends_with("tests.rs"))
        {
            output.push(path);
        }
    }
}

fn production_sources(root: &Path) -> Vec<PathBuf> {
    let workspace = toml_file(&root.join("Cargo.toml"));
    let mut sources = Vec::new();
    for member in workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members")
    {
        let member = root.join(member.as_str().expect("member path"));
        for directory in ["src", "examples"] {
            let path = member.join(directory);
            if path.is_dir() {
                rust_sources(&path, &mut sources);
            }
        }
        let build = member.join("build.rs");
        if build.is_file() {
            sources.push(build);
        }
    }
    sources.sort();
    sources
}

fn cfg_test(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .meta
                .require_list()
                .is_ok_and(|list| list.tokens.to_string() == "test")
    })
}

fn path_name(call: &ExprCall) -> Option<String> {
    let Expr::Path(path) = call.func.as_ref() else {
        return None;
    };
    Some(
        path.path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
    )
}

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

fn toml_file(path: &Path) -> toml::Table {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .parse()
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn package_name(key: &str, specification: &toml::Value, inherited: &toml::Table) -> String {
    let renamed = |value: &toml::Value| {
        value
            .get("package")
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };
    renamed(specification)
        .or_else(|| {
            specification
                .get("workspace")
                .and_then(toml::Value::as_bool)
                .filter(|inherits| *inherits)
                .and_then(|_| inherited.get(key))
                .and_then(renamed)
        })
        .unwrap_or_else(|| key.to_string())
}

fn dependency_names(manifest: &toml::Table, inherited: &toml::Table) -> BTreeSet<String> {
    let mut tables = vec![manifest.get("dependencies")];
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values().map(|target| target.get("dependencies")));
    }
    tables
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
        .flat_map(|table| table.iter())
        .map(|(key, specification)| package_name(key, specification, inherited))
        .collect()
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

fn module_directory(file: &Path) -> PathBuf {
    let parent = file.parent().expect("source has a parent").to_path_buf();
    match file.file_stem().and_then(|stem| stem.to_str()) {
        Some("mod" | "lib" | "main") => parent,
        Some(stem) => parent.join(stem),
        None => parent,
    }
}

fn path_attribute(attributes: &[syn::Attribute]) -> Option<String> {
    attributes.iter().find_map(|attribute| {
        let syn::Meta::NameValue(pair) = &attribute.meta else {
            return None;
        };
        let Expr::Lit(literal) = &pair.value else {
            return None;
        };
        let syn::Lit::Str(value) = &literal.lit else {
            return None;
        };
        pair.path.is_ident("path").then(|| value.value())
    })
}

fn test_module_paths(file: &Path, items: &[syn::Item], output: &mut Vec<PathBuf>) {
    let parent = file.parent().expect("source has a parent");
    test_module_paths_in(parent, &module_directory(file), items, output);
}

fn test_module_paths_in(
    path_base: &Path,
    directory: &Path,
    items: &[syn::Item],
    output: &mut Vec<PathBuf>,
) {
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        let name = module.ident.to_string();
        match &module.content {
            Some((_, nested)) if !cfg_test(&module.attrs) => {
                let inline = directory.join(&name);
                test_module_paths_in(&inline, &inline, nested, output)
            }
            Some(_) => {}
            None if cfg_test(&module.attrs) => match path_attribute(&module.attrs) {
                Some(path) => output.push(path_base.join(path)),
                None => {
                    output.push(directory.join(format!("{name}.rs")));
                    output.push(directory.join(name));
                }
            },
            None => {}
        }
    }
}

fn parse_production(root: &Path, sources: Vec<PathBuf>) -> Vec<(String, syn::File)> {
    let parsed = sources
        .into_iter()
        .map(|path| {
            let source = std::fs::read_to_string(&path).expect("read Rust source");
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            (path, syntax)
        })
        .collect::<Vec<_>>();
    let mut test_modules = Vec::new();
    for (path, syntax) in &parsed {
        test_module_paths(path, &syntax.items, &mut test_modules);
    }
    parsed
        .into_iter()
        .filter(|(path, _)| !test_modules.iter().any(|module| path.starts_with(module)))
        .map(|(path, syntax)| {
            let relative = path
                .strip_prefix(root)
                .expect("source is in repository")
                .to_string_lossy()
                .replace('\\', "/");
            (relative, syntax)
        })
        .collect()
}

fn mentions_embedded_node(ty: &syn::Type) -> bool {
    struct Finder(bool);
    impl<'ast> Visit<'ast> for Finder {
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
            if segments.next() == Some("EmbeddedNode")
                && (method == "execute" || NODE_ONLY_METHODS.contains(&method))
            {
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
fn production_defradb_node_execution_only_shrinks() {
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

    let mut actual = BTreeMap::new();
    for (relative, syntax) in &parsed {
        if READ_OWNER_FILES.contains(&relative.as_str()) {
            continue;
        }
        let sites = node_execution_sites(&workspace, syntax);
        if sites > 0 {
            actual.insert(relative.clone(), sites);
        }
    }

    let allowed = NODE_EXECUTE_ALLOWLIST
        .iter()
        .map(|(path, count)| (path.to_string(), *count))
        .collect::<BTreeMap<_, _>>();
    let mut violations = Vec::new();
    for path in allowed.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let found = actual.get(path).copied().unwrap_or_default();
        match allowed.get(path).copied() {
            None => violations.push(format!(
                "{path}: {found} new direct EmbeddedNode execution site(s); use the config_client/graphql owners"
            )),
            Some(limit) if found > limit => violations.push(format!(
                "{path}: {found} direct EmbeddedNode execution sites exceed the allowlisted {limit}"
            )),
            Some(limit) if found < limit => violations.push(format!(
                "{path}: {found} direct EmbeddedNode execution sites; lower the allowlist entry from {limit}"
            )),
            Some(_) => {}
        }
    }
    let current = actual
        .iter()
        .map(|(path, count)| format!("    (\"{path}\", {count}),"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        violations.is_empty(),
        "direct DefraDB node execution ratchet failed:\n{}\n\ncurrent sites:\n{current}",
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
        "fn f(db: &EmbeddedNode, q: &str) { db.execute(q); }",
        "fn f(db: Arc<defra_node::EmbeddedNode>, q: &str) { db.as_ref().execute(q); }",
        "impl S { fn f(&self, q: &str) { self.node.execute(q); } }",
        "struct S { store: Arc<EmbeddedNode> } impl S { fn f(&self, q: &str) { self.store.execute(q); } }",
        "fn f(node: &EmbeddedNode, q: &str) { node.clone().execute(q); }",
        "fn f(node: &EmbeddedNode, q: &str) { (&*node).execute(q); }",
        "fn f(core: &Core, q: &str) { core.node().execute(q); }",
        "fn f(node: &EmbeddedNode, q: &str) { EmbeddedNode::execute(&node, q); }",
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
        "fn f(access: &ConfigAccess, q: &str) { access.execute(q); }",
        "fn f(access: &ConfigAccess, q: &str) { ConfigAccess::execute(access, q); }",
        "fn f(txn: &ConfigApplyTxn, q: &str) { txn.execute(q); }",
        "impl ConfigAccess { fn f(&self, q: &str) { self.execute(q); } }",
        "fn f(node: &EmbeddedNode) {} fn g(access: &ConfigAccess, q: &str) { let db = access; db.execute(q); }",
        "fn f(node: &EmbeddedNode) { let db = node; } fn g(db: &Access, q: &str) { db.execute(q); }",
        "#[cfg(test)] mod tests { fn f(node: &EmbeddedNode, q: &str) { node.execute(q); } }",
        "#[cfg(test)] fn f(node: &EmbeddedNode, q: &str) { node.execute(q); }",
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

#[test]
fn test_modules_resolve_by_rust_module_rules() {
    let source = syn::parse_file(
        r#"
        #[cfg(test)] mod tests;
        #[cfg(test)] #[path = "top_fixture.rs"] mod top;
        mod inner {
            #[cfg(test)] mod nested_tests;
            #[cfg(test)] #[path = "inner_fixture.rs"] mod fixture;
            mod production;
        }
        #[cfg(test)] mod inline { fn f() {} }
        mod live;
        "#,
    )
    .expect("parse snippet");
    let mut modules = Vec::new();
    test_module_paths(
        Path::new("crate/src/feature.rs"),
        &source.items,
        &mut modules,
    );
    let expected = [
        "crate/src/feature/tests.rs",
        "crate/src/feature/tests",
        "crate/src/top_fixture.rs",
        "crate/src/feature/inner/nested_tests.rs",
        "crate/src/feature/inner/nested_tests",
        "crate/src/feature/inner/inner_fixture.rs",
    ]
    .map(PathBuf::from);
    assert_eq!(modules, expected);

    let mut modules = Vec::new();
    test_module_paths(Path::new("crate/src/lib.rs"), &source.items, &mut modules);
    assert!(modules.contains(&PathBuf::from("crate/src/top_fixture.rs")));
    assert!(modules.contains(&PathBuf::from("crate/src/inner/inner_fixture.rs")));
}

#[test]
fn dependency_reachability_resolves_renamed_packages() {
    let inherited: toml::Table = r#"store = { git = "x", package = "defra-node" }"#
        .parse()
        .expect("parse workspace dependencies");
    let manifest: toml::Table = r#"
        [dependencies]
        db = { package = "defra-node", version = "1" }
        store = { workspace = true }
        plain = "1"
        [target.'cfg(unix)'.dependencies]
        gents = { path = "../gents" }
    "#
    .parse()
    .expect("parse manifest");
    assert_eq!(
        dependency_names(&manifest, &inherited),
        BTreeSet::from(["defra-node", "gents", "plain"].map(str::to_string))
    );
}

const OAUTH_CREDENTIAL_OWNER: &str = "crates/gents/src/oauth_credential.rs";

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
    "lookup_oauth_credential_by_id",
    "lookup_oauth_credential_by_doc_id",
];

#[rustfmt::skip]
const OAUTH_ROW_READ_ALLOWLIST: &[(&str, usize)] = &[
    ("crates/gents-desktop-bridge/src/accounts.rs", 2),
    ("crates/gents/src/agent/document_view/load.rs", 1),
    ("crates/gents/src/agent/document_view/mod.rs", 1),
];

#[derive(Default)]
struct OAuthPickVisitor {
    violations: Vec<String>,
    reads: usize,
}

static RAW_OAUTH_QUERY: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"OAuthCredential\s*(\(|\{[{\s])").expect("raw query pattern")
});

fn macro_strings(mut cursor: syn::buffer::Cursor, strings: &mut String) {
    while let Some((_, next)) = cursor.token_tree() {
        if let Some((literal, _)) = cursor.literal() {
            if let syn::Lit::Str(literal) = syn::Lit::new(literal) {
                strings.push_str(&literal.value());
            }
        } else if let Some((inside, ..)) = cursor.any_group() {
            macro_strings(inside, strings);
        }
        cursor = next;
    }
}

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
    let mut actual = BTreeMap::new();
    for (relative, syntax) in parse_production(&root, production_sources(&root)) {
        if relative == OAUTH_CREDENTIAL_OWNER {
            continue;
        }
        let (found, reads) = oauth_pick_findings(&syntax);
        violations.extend(
            found
                .into_iter()
                .map(|found| format!("{relative}: {found}")),
        );
        if reads > 0 {
            actual.insert(relative, reads);
        }
    }
    let allowed = OAUTH_ROW_READ_ALLOWLIST
        .iter()
        .map(|(path, count)| (path.to_string(), *count))
        .collect::<BTreeMap<_, _>>();
    for path in allowed.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let found = actual.get(path).copied().unwrap_or_default();
        let limit = allowed.get(path).copied().unwrap_or_default();
        if found != limit {
            violations.push(format!(
                "{path}: {found} OAuthCredential row read(s), allowlisted {limit}; pick through oauth_credential::resolve_oauth_credential"
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
        ("fn f(n: &N) { lookup_oauth_credential(n, \"did\", \"p\"); }", 1, 0),
        ("fn f(a: &A) { gents::oauth_credential::oauth_credential_query(\"d\", \"p\"); }", 1, 0),
        ("async fn load_oauth_credential() {}", 1, 0),
        ("fn f(v: &V) { v.has_enabled_oauth_credential(\"p\"); }", 1, 0),
        (r#"fn f(d: &str) -> String { format!("query {{ OAuthCredential(filter: {{ node_did: {{ _eq: \"{d}\" }} }}) {{ _docID }} }}") }"#, 1, 0),
        ("fn f(a: &A) { list_oauth_credentials_on(a, \"d\"); }", 0, 1),
        ("fn f(v: &V) { v.oauth_credentials.values().find(|r| r.enabled); }", 0, 1),
        ("fn f(n: &N) { lookup_oauth_credential_by_id(n, \"id\"); load_oauth_credential_for_discovery(); }", 0, 1),
        ("#[cfg(test)] mod tests { fn f(n: &N) { lookup_oauth_credential(n); list_oauth_credentials(n); } }", 0, 0),
        (r#"const Q: &str = "query { OAuthCredential (filter: {}) { _docID } }";"#, 1, 0),
        (r#"fn f() -> String { format!("{{ OAuthCredential \n (filter: {{}}) {{ _docID }} }}") }"#, 1, 0),
        (r#"const Q: &str = concat!("query { OAuthCredential", "(filter: {}) { _docID } }");"#, 1, 0),
        (r#"fn f() -> String { format!("query {{ OAuthCredential {{ _docID }} }}") }"#, 1, 0),
        (r#"fn f(id: &str) -> String { format!("duplicate OAuthCredential {id}") }"#, 0, 0),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        let (found, counted) = oauth_pick_findings(&syntax);
        assert_eq!((found.len(), counted), (violations, reads), "{source}: {found:?}");
    }
}

const RIG_OWNER_MODULES: &[&str] = &[
    "crates/gents-loop/src/rig_compat.rs",
    "crates/gents-loop/src/provider_input",
    "crates/gents-loop/src/loop_stream",
    "crates/gents/src/llm/rig_compat.rs",
    "crates/gents/src/provider_input",
    "crates/gents/src/agent/loop_stream",
];

#[rustfmt::skip]
const RIG_PATH_ALLOWLIST: &[&str] = &[
    "crates/gents-loop/src/claude_messages_body.rs",
    "crates/gents-loop/src/provider_stream.rs",
    "crates/gents-loop/src/provider_usage.rs",
    "crates/gents-loop/src/rendered_request/transport.rs",
    "crates/gents/src/chatgpt_codex.rs",
    "crates/gents/src/claude_messages.rs",
    "crates/gents/src/claude_subscription.rs",
    "crates/gents/src/inference_http.rs",
    "crates/gents/src/llm/backend_client.rs",
    "crates/gents/src/oauth_http.rs",
    "crates/gents/src/provider_http.rs",
    "crates/gents/src/usage_observation.rs",
    "crates/gents/src/xai_grok_oauth.rs",
];

#[derive(Default)]
struct RigPathVisitor {
    sites: usize,
}

fn owned_by(relative: &str, owner: &str) -> bool {
    relative == owner
        || relative
            .strip_prefix(owner)
            .is_some_and(|rest| rest == ".rs" || rest.starts_with('/'))
}

fn rig_use_roots(tree: &syn::UseTree) -> usize {
    match tree {
        syn::UseTree::Path(syn::UsePath { ident, .. })
        | syn::UseTree::Rename(syn::UseRename { ident, .. })
        | syn::UseTree::Name(syn::UseName { ident }) => usize::from(ident == "rig"),
        syn::UseTree::Group(group) => group.items.iter().map(rig_use_roots).sum(),
        _ => 0,
    }
}

fn starts_with_rig(path: &syn::Path) -> bool {
    path.segments.len() > 1
        && path
            .segments
            .first()
            .is_some_and(|first| first.ident == "rig")
}

fn macro_names_rig(mut cursor: syn::buffer::Cursor) -> bool {
    while !cursor.eof() {
        if let Some((inside, _, _, next)) = cursor.any_group() {
            if macro_names_rig(inside) {
                return true;
            }
            cursor = next;
        } else if let Some((ident, next)) = cursor.ident() {
            let path = next.punct().is_some_and(|(colon, after)| {
                colon.as_char() == ':' && after.punct().is_some_and(|(p, _)| p.as_char() == ':')
            });
            if ident == "rig" && path {
                return true;
            }
            cursor = next;
        } else if let Some((_, next)) = cursor.token_tree() {
            cursor = next;
        } else {
            break;
        }
    }
    false
}

fn item_cfg_test(item: &syn::Item) -> bool {
    let attributes = match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => return false,
    };
    cfg_test(attributes)
}

impl<'ast> Visit<'ast> for RigPathVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.sites += rig_use_roots(&item.tree);
        visit::visit_item_use(self, item);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if item.ident == "rig" {
            self.sites += 1;
        }
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if starts_with_rig(path) {
            self.sites += 1;
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let tokens = syn::buffer::TokenBuffer::new2(invocation.tokens.clone());
        if macro_names_rig(tokens.begin()) {
            self.sites += 1;
        }
        visit::visit_macro(self, invocation);
    }
}

fn rig_path_sites(syntax: &syn::File) -> usize {
    let mut visitor = RigPathVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn rig_types_stay_behind_their_owners() {
    let root = repo_root();
    let actual = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter(|(relative, _)| {
            !RIG_OWNER_MODULES
                .iter()
                .any(|owner| owned_by(relative, owner))
        })
        .filter(|(_, syntax)| rig_path_sites(syntax) > 0)
        .map(|(relative, _)| relative)
        .collect::<BTreeSet<_>>();
    let allowed = RIG_PATH_ALLOWLIST
        .iter()
        .map(|path| path.to_string())
        .collect::<BTreeSet<_>>();
    let mut violations = actual
        .difference(&allowed)
        .map(|path| format!("{path}: names `rig`; use a native type at the rig_compat boundary"))
        .collect::<Vec<_>>();
    violations.extend(
        allowed
            .difference(&actual)
            .map(|path| format!("{path}: no longer names `rig`; remove its allowlist entry")),
    );
    let current = actual
        .iter()
        .map(|path| format!("    \"{path}\","))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        violations.is_empty(),
        "rig boundary ratchet failed:\n{}\n\ncurrent files:\n{current}",
        violations.join("\n")
    );
}

#[test]
fn rig_owners_match_whole_module_paths() {
    let owner = "crates/gents-loop/src/provider_input";
    assert!(owned_by("crates/gents-loop/src/provider_input.rs", owner));
    assert!(owned_by(
        "crates/gents-loop/src/provider_input/budget.rs",
        owner
    ));
    assert!(!owned_by(
        "crates/gents-loop/src/provider_input_budget.rs",
        owner
    ));
    let file = "crates/gents-loop/src/rig_compat.rs";
    assert!(owned_by(file, file));
    assert!(!owned_by("crates/gents-loop/src/rig_compat.rs.bak", file));
}

#[test]
fn rig_fence_counts_paths_uses_and_macros_outside_tests() {
    for (source, sites) in [
        ("use rig::completion::Usage;", 1),
        ("use rig::{client, completion};", 1),
        ("use rig as provider;", 1),
        ("extern crate rig as provider;", 1),
        ("use {rig as provider};", 1),
        ("pub use rig;", 1),
        ("pub use {rig};", 1),
        ("fn f(u: rig::completion::Usage) {}", 1),
        (
            "fn f() { let s = try_stream! { rig::streaming::x(); }; }",
            1,
        ),
        ("fn f(rig: R) { rig.call(); }", 0),
        ("#[cfg(test)] mod tests { use rig::completion::Usage; }", 0),
        ("#[cfg(test)] use rig::completion::Usage;", 0),
        (
            "#[cfg(test)] impl rig::completion::CompletionModel for M {}",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(rig_path_sites(&syntax), sites, "{source}");
    }
}

/// Files whose std-hasher maps are fixed by a third-party signature: the lens
/// SDK's document map, rmcp's `custom_headers` and Rig's `additional_props`.
#[rustfmt::skip]
const STD_HASH_ALLOWLIST: &[&str] = &[
    "crates/gents-lenses/fixture_add_label/src/lib.rs",
    "crates/gents/src/mcp_pool.rs",
    "crates/gents/src/provider_input/tests.rs",
];

const STD_HASH_NAMES: &[&str] = &["HashMap", "HashSet", "DefaultHasher"];

fn all_rust_sources(root: &Path, output: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root)
        .unwrap_or_else(|error| panic!("read {}: {error}", root.display()))
        .flatten()
    {
        let path = entry.path();
        if path.is_dir() {
            if path
                .file_name()
                .is_none_or(|name| name != "target" && name != "node_modules")
            {
                all_rust_sources(&path, output);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

#[derive(Default)]
struct StdHashVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for StdHashVisitor {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path
            .segments
            .iter()
            .any(|segment| STD_HASH_NAMES.iter().any(|name| segment.ident == name))
        {
            self.sites += 1;
        }
        visit::visit_path(self, path);
    }

    fn visit_use_name(&mut self, name: &'ast syn::UseName) {
        if STD_HASH_NAMES
            .iter()
            .any(|forbidden| name.ident == forbidden)
        {
            self.sites += 1;
        }
    }

    fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
        if STD_HASH_NAMES
            .iter()
            .any(|forbidden| rename.ident == forbidden)
        {
            self.sites += 1;
        }
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let tokens = syn::buffer::TokenBuffer::new2(invocation.tokens.clone());
        if macro_names_std_hash(tokens.begin()) {
            self.sites += 1;
        }
        visit::visit_macro(self, invocation);
    }
}

fn macro_names_std_hash(mut cursor: syn::buffer::Cursor) -> bool {
    while !cursor.eof() {
        if let Some((inside, _, _, next)) = cursor.any_group() {
            if macro_names_std_hash(inside) {
                return true;
            }
            cursor = next;
        } else if let Some((ident, next)) = cursor.ident() {
            if STD_HASH_NAMES.iter().any(|name| ident == name) {
                return true;
            }
            cursor = next;
        } else if let Some((_, next)) = cursor.token_tree() {
            cursor = next;
        } else {
            break;
        }
    }
    false
}

fn std_hash_sites(syntax: &syn::File) -> usize {
    let mut visitor = StdHashVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

/// Maps and sets use `rapidhash::RapidHashMap`/`RapidHashSet` and hashers use
/// `rapidhash::fast::RapidHasher`, matching DefraDB; this covers tests too.
#[test]
fn hash_maps_use_rapidhash() {
    let root = repo_root();
    let mut sources = Vec::new();
    for directory in ["crates", "apps"] {
        all_rust_sources(&root.join(directory), &mut sources);
    }
    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            (std_hash_sites(&syntax) > 0).then(|| {
                path.strip_prefix(&root)
                    .expect("source under repo root")
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect::<BTreeSet<_>>();
    let allowed = STD_HASH_ALLOWLIST
        .iter()
        .map(|path| path.to_string())
        .collect::<BTreeSet<_>>();
    let mut violations = actual
        .difference(&allowed)
        .map(|path| format!("{path}: names a std hash map, set or hasher; use rapidhash"))
        .collect::<Vec<_>>();
    violations.extend(
        allowed.difference(&actual).map(|path| {
            format!("{path}: no longer names a std hasher; remove its allowlist entry")
        }),
    );
    assert!(
        violations.is_empty(),
        "rapidhash ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rapidhash_fence_counts_std_names_only() {
    for (source, sites) in [
        ("use std::collections::HashMap;", 1),
        ("use std::collections::{BTreeMap, HashSet};", 1),
        ("use std::collections::HashMap as Map;", 1),
        ("fn f(m: std::collections::HashMap<u8, u8>) {}", 1),
        ("fn f() { let h = std::hash::DefaultHasher::new(); }", 1),
        ("fn f() { let v = vec![HashSet::<u8>::new()]; }", 1),
        ("use rapidhash::{HashMapExt, RapidHashMap};", 0),
        (
            "fn f(m: RapidHashMap<u8, u8>) { let _ = RapidHashSet::<u8>::new(); }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(std_hash_sites(&syntax), sites, "{source}");
    }
}

#[derive(Default)]
struct InlineBoundVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for InlineBoundVisitor {
    fn visit_generic_param(&mut self, param: &'ast syn::GenericParam) {
        let inline = match param {
            syn::GenericParam::Type(parameter) => !parameter.bounds.is_empty(),
            syn::GenericParam::Lifetime(parameter) => !parameter.bounds.is_empty(),
            syn::GenericParam::Const(_) => false,
        };
        if inline {
            self.sites += 1;
        }
        visit::visit_generic_param(self, param);
    }
}

fn inline_bound_sites(syntax: &syn::File) -> usize {
    let mut visitor = InlineBoundVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

/// Trait and lifetime bounds go in a where clause, never inline in the generic
/// parameter list, matching DefraDB; this covers tests too.
#[test]
fn trait_bounds_live_in_where_clauses() {
    let root = repo_root();
    let mut sources = Vec::new();
    for directory in ["crates", "apps"] {
        all_rust_sources(&root.join(directory), &mut sources);
    }
    let violations = sources
        .into_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let sites = inline_bound_sites(&syntax);
            (sites > 0).then(|| {
                let relative = path
                    .strip_prefix(&root)
                    .expect("source under repo root")
                    .to_string_lossy()
                    .into_owned();
                format!("{relative}: {sites} inline generic bounds; move them to a where clause")
            })
        })
        .collect::<BTreeSet<_>>();
    assert!(
        violations.is_empty(),
        "where-clause ratchet failed:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn where_clause_fence_counts_inline_bounds_only() {
    for (source, sites) in [
        ("fn f<T: Clone>(t: T) {}", 1),
        ("fn f<'a, 'b: 'a, T>(t: &'b T) {}", 1),
        ("struct S<T: Default = u8>(T);", 1),
        ("impl<T: Send, U: Sync> Tr for S<T, U> {}", 2),
        ("trait Tr { fn f<T: Clone>(t: T); type A<B: Copy>; }", 2),
        ("impl S { fn f<T: Clone>(t: T) {} }", 1),
        ("fn f<T>(t: T) where T: Clone {}", 0),
        ("struct S<T = u8>(T) where T: Default;", 0),
        ("fn f<const N: usize>(t: impl Clone) {}", 0),
        ("fn f<F>(f: F) where F: for<'a> Fn(&'a u8) {}", 0),
        ("fn f() { m!(fn g<T: Clone>(t: T) {}); }", 0),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(inline_bound_sites(&syntax), sites, "{source}");
    }
}

/// Request context (identity, signing, ACP bypass, transaction handles, the
/// requester or session, trace or request IDs) travels as explicit arguments
/// or fields. `tokio::task_local!` loses it across `tokio::spawn`, and Clippy's
/// `disallowed_macros` cannot be allowed at an item-position macro site, so
/// this ratchet owns the ban. Entries name why each production site remains;
/// the list only shrinks.
#[rustfmt::skip]
const AMBIENT_CONTEXT_ALLOWLIST: &[(&str, usize, &str)] = &[
    ("crates/gents-cli/src/request_helpers.rs", 1, "captures an embedded command's printed report; carries no request identity"),
    ("crates/gents-loop/src/provider_input/routing_affinity.rs", 1, "request context read inside Rig's HTTP client; thread through a per-call provider client"),
    ("crates/gents-loop/src/rendered_request/scope.rs", 1, "request context read inside Rig's HTTP client; thread through a per-call provider client"),
    ("crates/gents-loop/src/tool_call_lifecycle/runtime.rs", 2, "request context for tool calls; thread through a ToolDyn::call context argument"),
    ("crates/gents-loop/src/tool_effects.rs", 1, "request context for tool calls; thread through a ToolDyn::call context argument"),
    ("crates/gents/src/admission/client.rs", 1, "request context read inside Rig's CompletionModel and HTTP client; thread through a per-call provider client"),
    ("crates/gents/src/config_client/txn.rs", 1, "re-entrant write fence inside a transaction callback; the handle itself is passed explicitly"),
    ("crates/gents/src/managed_exec/ownership.rs", 1, "request context for tool calls; thread through a ToolDyn::call context argument"),
    ("crates/gents/src/plugin/approval.rs", 1, "request context for tool calls; thread through a ToolDyn::call context argument"),
    ("crates/gents/src/plugin/approval/workspace/context.rs", 1, "request context for tool calls; thread through a ToolDyn::call context argument"),
    ("crates/gents/src/tool_call_lifecycle/runtime.rs", 1, "request context for tool calls; thread through a ToolDyn::call context argument"),
];

#[derive(Default)]
struct AmbientContextVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for AmbientContextVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        if invocation
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "thread_local" || segment.ident == "task_local")
        {
            let tokens = invocation.tokens.clone().into_iter().collect::<Vec<_>>();
            self.sites += tokens
                .iter()
                .enumerate()
                .filter(|(index, token)| {
                    let lifetime = index.checked_sub(1).is_some_and(|previous| {
                        matches!(&tokens[previous], proc_macro2::TokenTree::Punct(punct) if punct.as_char() == '\'')
                    });
                    !lifetime
                        && matches!(token, proc_macro2::TokenTree::Ident(ident) if ident == "static")
                })
                .count();
        }
        visit::visit_macro(self, invocation);
    }
}

fn ambient_context_sites(syntax: &syn::File) -> usize {
    let mut visitor = AmbientContextVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn ambient_request_context_only_shrinks() {
    let root = repo_root();
    let actual = parse_production(&root, production_sources(&root))
        .into_iter()
        .map(|(relative, syntax)| (relative, ambient_context_sites(&syntax)))
        .filter(|(_, sites)| *sites > 0)
        .collect::<BTreeMap<_, _>>();
    let allowed = AMBIENT_CONTEXT_ALLOWLIST
        .iter()
        .map(|(path, count, _)| (path.to_string(), *count))
        .collect::<BTreeMap<_, _>>();
    let mut violations = Vec::new();
    for path in allowed.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let found = actual.get(path).copied().unwrap_or_default();
        match allowed.get(path).copied() {
            None => violations.push(format!(
                "{path}: {found} new thread_local!/task_local! static(s); pass request context as an argument or field"
            )),
            Some(limit) if found > limit => violations.push(format!(
                "{path}: {found} thread_local!/task_local! statics exceed the allowlisted {limit}"
            )),
            Some(limit) if found < limit => violations.push(format!(
                "{path}: {found} thread_local!/task_local! statics; lower the allowlist entry from {limit}"
            )),
            Some(_) => {}
        }
    }
    let current = actual
        .iter()
        .map(|(path, count)| format!("    (\"{path}\", {count}, \"<reason>\"),"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        violations.is_empty(),
        "ambient request context ratchet failed:\n{}\n\ncurrent sites:\n{current}",
        violations.join("\n")
    );
}

#[test]
fn ambient_context_counts_statics_outside_test_items() {
    let syntax = syn::parse_file(
        r#"
        tokio::task_local! { static A: &'static str; static B: u8; }
        fn f() { thread_local! { static C: u8 = const { 0 }; } }
        #[cfg(test)]
        tokio::task_local! { static D: u8; }
        #[cfg(test)]
        mod tests { thread_local! { static E: u8 = const { 0 }; } }
        "#,
    )
    .expect("parse snippet");
    assert_eq!(ambient_context_sites(&syntax), 3);
}

/// Files that still name `anyhow`. Errors are module-owned `thiserror` enums,
/// matching DefraDB; this covers tests too and the list only shrinks.
#[rustfmt::skip]
const ANYHOW_ALLOWLIST: &[&str] = &[
    "apps/gents-desktop/src-tauri/src/bin/bridge_runner.rs",
    "apps/gents-desktop/src-tauri/src/bin/bridge_runner/http.rs",
    "apps/gents-desktop/src-tauri/src/bin/bridge_runner/http/protocol.rs",
    "apps/gents-desktop/src-tauri/src/bin/bridge_runner/http/routes.rs",
    "apps/gents-desktop/src-tauri/src/bridge/windows.rs",
    "apps/gents-desktop/src-tauri/src/browser_data.rs",
    "apps/gents-desktop/src-tauri/src/main.rs",
    "apps/gents-desktop/src-tauri/src/runner/live_fixture.rs",
    "apps/gents-desktop/src-tauri/src/runner/live_fixture/agent.rs",
    "apps/gents-desktop/src-tauri/src/runner/live_fixture/backend.rs",
    "apps/gents-desktop/src-tauri/src/runner/live_fixture/inference.rs",
    "apps/gents-desktop/src-tauri/src/runner/live_fixture/replication.rs",
    "apps/gents-desktop/src-tauri/src/runner/live_fixture/workspace.rs",
    "crates/gents-cli/src/caused_sessions.rs",
    "crates/gents-cli/src/cli/output_format.rs",
    "crates/gents-cli/src/commands/accounts.rs",
    "crates/gents-cli/src/commands/afterburner_build.rs",
    "crates/gents-cli/src/commands/background.rs",
    "crates/gents-cli/src/commands/chain/key.rs",
    "crates/gents-cli/src/commands/chain/mod.rs",
    "crates/gents-cli/src/commands/chain/query.rs",
    "crates/gents-cli/src/commands/chat/approvals.rs",
    "crates/gents-cli/src/commands/chat/interactive.rs",
    "crates/gents-cli/src/commands/chat/mod.rs",
    "crates/gents-cli/src/commands/chat/streaming.rs",
    "crates/gents-cli/src/commands/claude_login.rs",
    "crates/gents-cli/src/commands/cloud.rs",
    "crates/gents-cli/src/commands/codex_auth_probe.rs",
    "crates/gents-cli/src/commands/codex_login.rs",
    "crates/gents-cli/src/commands/codex_shim.rs",
    "crates/gents-cli/src/commands/codex_shim/background.rs",
    "crates/gents-cli/src/commands/codex_shim/bound_behavior.rs",
    "crates/gents-cli/src/commands/codex_shim/caused_threads.rs",
    "crates/gents-cli/src/commands/codex_shim/child_stream.rs",
    "crates/gents-cli/src/commands/codex_shim/compat.rs",
    "crates/gents-cli/src/commands/codex_shim/compat/empty_local.rs",
    "crates/gents-cli/src/commands/codex_shim/continuation_stream.rs",
    "crates/gents-cli/src/commands/codex_shim/handlers.rs",
    "crates/gents-cli/src/commands/codex_shim/handlers/background.rs",
    "crates/gents-cli/src/commands/codex_shim/handlers/basic.rs",
    "crates/gents-cli/src/commands/codex_shim/handlers/host.rs",
    "crates/gents-cli/src/commands/codex_shim/handlers/models.rs",
    "crates/gents-cli/src/commands/codex_shim/handlers/skills.rs",
    "crates/gents-cli/src/commands/codex_shim/handlers/thread.rs",
    "crates/gents-cli/src/commands/codex_shim/handlers/turns.rs",
    "crates/gents-cli/src/commands/codex_shim/history_projection.rs",
    "crates/gents-cli/src/commands/codex_shim/progress.rs",
    "crates/gents-cli/src/commands/codex_shim/protocol.rs",
    "crates/gents-cli/src/commands/codex_shim/server.rs",
    "crates/gents-cli/src/commands/codex_shim/store.rs",
    "crates/gents-cli/src/commands/codex_shim/thread_projection.rs",
    "crates/gents-cli/src/commands/codex_shim/thread_projection/goal.rs",
    "crates/gents-cli/src/commands/codex_shim/thread_projection/mutations.rs",
    "crates/gents-cli/src/commands/codex_shim/thread_projection/storage.rs",
    "crates/gents-cli/src/commands/codex_shim/thread_projection/usage.rs",
    "crates/gents-cli/src/commands/codex_shim/thread_routes.rs",
    "crates/gents-cli/src/commands/codex_shim/turn_projection.rs",
    "crates/gents-cli/src/commands/codex_shim/turn.rs",
    "crates/gents-cli/src/commands/codex_shim/turn/active.rs",
    "crates/gents-cli/src/commands/codex_shim/turn/stream.rs",
    "crates/gents-cli/src/commands/codex_shim/turn/submission.rs",
    "crates/gents-cli/src/commands/codex.rs",
    "crates/gents-cli/src/commands/config/apply.rs",
    "crates/gents-cli/src/commands/config/backend.rs",
    "crates/gents-cli/src/commands/config/behavior.rs",
    "crates/gents-cli/src/commands/config/binding.rs",
    "crates/gents-cli/src/commands/config/binding/owner.rs",
    "crates/gents-cli/src/commands/config/crud.rs",
    "crates/gents-cli/src/commands/config/diff.rs",
    "crates/gents-cli/src/commands/config/export.rs",
    "crates/gents-cli/src/commands/config/mod.rs",
    "crates/gents-cli/src/commands/config/profile.rs",
    "crates/gents-cli/src/commands/config/skill.rs",
    "crates/gents-cli/src/commands/config/task_run.rs",
    "crates/gents-cli/src/commands/config/tools.rs",
    "crates/gents-cli/src/commands/config/validate.rs",
    "crates/gents-cli/src/commands/config/workspace_root.rs",
    "crates/gents-cli/src/commands/desktop.rs",
    "crates/gents-cli/src/commands/diagnose/backends.rs",
    "crates/gents-cli/src/commands/diagnose/mod.rs",
    "crates/gents-cli/src/commands/diagnose/schema.rs",
    "crates/gents-cli/src/commands/document.rs",
    "crates/gents-cli/src/commands/eval/batch.rs",
    "crates/gents-cli/src/commands/eval/checks.rs",
    "crates/gents-cli/src/commands/eval/compare.rs",
    "crates/gents-cli/src/commands/eval/init/dossier.rs",
    "crates/gents-cli/src/commands/eval/init/mod.rs",
    "crates/gents-cli/src/commands/eval/init/pilot.rs",
    "crates/gents-cli/src/commands/eval/init/readiness.rs",
    "crates/gents-cli/src/commands/eval/init/turn.rs",
    "crates/gents-cli/src/commands/eval/init/write.rs",
    "crates/gents-cli/src/commands/eval/init/write/recovery.rs",
    "crates/gents-cli/src/commands/eval/inspect.rs",
    "crates/gents-cli/src/commands/eval/manage.rs",
    "crates/gents-cli/src/commands/eval/mod.rs",
    "crates/gents-cli/src/commands/eval/run.rs",
    "crates/gents-cli/src/commands/eval/testing.rs",
    "crates/gents-cli/src/commands/eval/watch.rs",
    "crates/gents-cli/src/commands/fleet.rs",
    "crates/gents-cli/src/commands/goal.rs",
    "crates/gents-cli/src/commands/graph.rs",
    "crates/gents-cli/src/commands/grok_auth_probe.rs",
    "crates/gents-cli/src/commands/grok_login.rs",
    "crates/gents-cli/src/commands/grok_shim.rs",
    "crates/gents-cli/src/commands/grok_shim/acp.rs",
    "crates/gents-cli/src/commands/grok_shim/binding.rs",
    "crates/gents-cli/src/commands/grok_shim/goals.rs",
    "crates/gents-cli/src/commands/grok_shim/graph_runs.rs",
    "crates/gents-cli/src/commands/grok_shim/job_runs.rs",
    "crates/gents-cli/src/commands/grok_shim/observation_cursor.rs",
    "crates/gents-cli/src/commands/grok_shim/projection.rs",
    "crates/gents-cli/src/commands/grok_shim/projection/caused_sessions.rs",
    "crates/gents-cli/src/commands/grok_shim/projection/context.rs",
    "crates/gents-cli/src/commands/grok_shim/projection/messages.rs",
    "crates/gents-cli/src/commands/grok_shim/projection/tools.rs",
    "crates/gents-cli/src/commands/grok_shim/server.rs",
    "crates/gents-cli/src/commands/grok_shim/session_record.rs",
    "crates/gents-cli/src/commands/grok_shim/sessions.rs",
    "crates/gents-cli/src/commands/grok_shim/task_control.rs",
    "crates/gents-cli/src/commands/grok_shim/turn.rs",
    "crates/gents-cli/src/commands/grok_shim/usage.rs",
    "crates/gents-cli/src/commands/grok_shim/workflow_catalog.rs",
    "crates/gents-cli/src/commands/grok_shim/workflow_command.rs",
    "crates/gents-cli/src/commands/inference_binding.rs",
    "crates/gents-cli/src/commands/init.rs",
    "crates/gents-cli/src/commands/job.rs",
    "crates/gents-cli/src/commands/mailbox.rs",
    "crates/gents-cli/src/commands/mcp.rs",
    "crates/gents-cli/src/commands/native_fs_runner.rs",
    "crates/gents-cli/src/commands/optimization/behavior_proposer.rs",
    "crates/gents-cli/src/commands/optimization/mod.rs",
    "crates/gents-cli/src/commands/optimization/testing.rs",
    "crates/gents-cli/src/commands/p2p/access.rs",
    "crates/gents-cli/src/commands/p2p/collections.rs",
    "crates/gents-cli/src/commands/p2p/connect.rs",
    "crates/gents-cli/src/commands/p2p/documents.rs",
    "crates/gents-cli/src/commands/p2p/enrollment_admin.rs",
    "crates/gents-cli/src/commands/p2p/mod.rs",
    "crates/gents-cli/src/commands/p2p/network.rs",
    "crates/gents-cli/src/commands/p2p/output.rs",
    "crates/gents-cli/src/commands/p2p/pairings.rs",
    "crates/gents-cli/src/commands/p2p/replicators.rs",
    "crates/gents-cli/src/commands/p2p/templates.rs",
    "crates/gents-cli/src/commands/pack/account.rs",
    "crates/gents-cli/src/commands/pack/build.rs",
    "crates/gents-cli/src/commands/pack/cache.rs",
    "crates/gents-cli/src/commands/pack/check.rs",
    "crates/gents-cli/src/commands/pack/cli_process.rs",
    "crates/gents-cli/src/commands/pack/edit.rs",
    "crates/gents-cli/src/commands/pack/import.rs",
    "crates/gents-cli/src/commands/pack/inspect.rs",
    "crates/gents-cli/src/commands/pack/mod.rs",
    "crates/gents-cli/src/commands/pack/registry.rs",
    "crates/gents-cli/src/commands/pack/remove.rs",
    "crates/gents-cli/src/commands/pack/remove/tests.rs",
    "crates/gents-cli/src/commands/pack/scaffold.rs",
    "crates/gents-cli/src/commands/pack/scenario.rs",
    "crates/gents-cli/src/commands/pack/scenario/prepare.rs",
    "crates/gents-cli/src/commands/pack/server.rs",
    "crates/gents-cli/src/commands/pack/test.rs",
    "crates/gents-cli/src/commands/pack/update.rs",
    "crates/gents-cli/src/commands/pack/wire.rs",
    "crates/gents-cli/src/commands/plugin/bind.rs",
    "crates/gents-cli/src/commands/plugin/build.rs",
    "crates/gents-cli/src/commands/plugin/dirs.rs",
    "crates/gents-cli/src/commands/plugin/install.rs",
    "crates/gents-cli/src/commands/plugin/mod.rs",
    "crates/gents-cli/src/commands/plugin/run.rs",
    "crates/gents-cli/src/commands/provision.rs",
    "crates/gents-cli/src/commands/query.rs",
    "crates/gents-cli/src/commands/request.rs",
    "crates/gents-cli/src/commands/reset.rs",
    "crates/gents-cli/src/commands/response.rs",
    "crates/gents-cli/src/commands/schema.rs",
    "crates/gents-cli/src/commands/serve.rs",
    "crates/gents-cli/src/commands/session.rs",
    "crates/gents-cli/src/commands/status.rs",
    "crates/gents-cli/src/commands/task.rs",
    "crates/gents-cli/src/commands/tools.rs",
    "crates/gents-cli/src/commands/trace.rs",
    "crates/gents-cli/src/commands/trace/projection_acp.rs",
    "crates/gents-cli/src/commands/ux.rs",
    "crates/gents-cli/src/commands/write.rs",
    "crates/gents-cli/src/config_bundle.rs",
    "crates/gents-cli/src/config_import.rs",
    "crates/gents-cli/src/config_import/transaction_tests.rs",
    "crates/gents-cli/src/desired_state/convert.rs",
    "crates/gents-cli/src/desired_state/diff.rs",
    "crates/gents-cli/src/desired_state/load.rs",
    "crates/gents-cli/src/desired_state/prune.rs",
    "crates/gents-cli/src/desired_state/validate/live.rs",
    "crates/gents-cli/src/desired_state/validate/storage.rs",
    "crates/gents-cli/src/desired_state/write.rs",
    "crates/gents-cli/src/graphql_access.rs",
    "crates/gents-cli/src/home_state.rs",
    "crates/gents-cli/src/http/enrollment.rs",
    "crates/gents-cli/src/http/explorer.rs",
    "crates/gents-cli/src/http/fleet_slots.rs",
    "crates/gents-cli/src/http/fleet.rs",
    "crates/gents-cli/src/http/identity_decide.rs",
    "crates/gents-cli/src/http/mcp_pool.rs",
    "crates/gents-cli/src/http/mcp_server.rs",
    "crates/gents-cli/src/http/prometheus.rs",
    "crates/gents-cli/src/http/self_view.rs",
    "crates/gents-cli/src/http/sessions.rs",
    "crates/gents-cli/src/interactive_backend.rs",
    "crates/gents-cli/src/lib.rs",
    "crates/gents-cli/src/main.rs",
    "crates/gents-cli/src/native_service.rs",
    "crates/gents-cli/src/network_settings.rs",
    "crates/gents-cli/src/packs.rs",
    "crates/gents-cli/src/process_resources.rs",
    "crates/gents-cli/src/request_helpers.rs",
    "crates/gents-cli/src/resolve_helpers.rs",
    "crates/gents-cli/src/server_host.rs",
    "crates/gents-cli/src/shared.rs",
    "crates/gents-cli/src/telemetry.rs",
    "crates/gents-cli/tests/cli_codex_shim.rs",
    "crates/gents-cli/tests/cli_codex_shim/helpers/capture.rs",
    "crates/gents-cli/tests/cli_codex_shim/helpers/fixtures.rs",
    "crates/gents-cli/tests/cli_codex_shim/helpers/terminal.rs",
    "crates/gents-cli/tests/cli_codex_shim/live_backend.rs",
    "crates/gents-cli/tests/cli_codex_shim/turn_control.rs",
    "crates/gents-cli/tests/cli_enrollment.rs",
    "crates/gents-cli/tests/cli_enrollment/contract.rs",
    "crates/gents-cli/tests/cli_enrollment/contract/streaming.rs",
    "crates/gents-cli/tests/cli_graph.rs",
    "crates/gents-cli/tests/cli_server.rs",
    "crates/gents-cli/tests/native_service.rs",
    "crates/gents-cli/tests/port_allocation.rs",
    "crates/gents-cli/tests/suites/cli_adapter_interop_roundtrip.rs",
    "crates/gents-cli/tests/suites/cli_background.rs",
    "crates/gents-cli/tests/suites/cli_chat.rs",
    "crates/gents-cli/tests/suites/cli_config_agent.rs",
    "crates/gents-cli/tests/suites/cli_config_apply_e2e.rs",
    "crates/gents-cli/tests/suites/cli_config_apply_graphql.rs",
    "crates/gents-cli/tests/suites/cli_config_apply_local.rs",
    "crates/gents-cli/tests/suites/cli_config_apply_running.rs",
    "crates/gents-cli/tests/suites/cli_config_apply_transactional_rollback.rs",
    "crates/gents-cli/tests/suites/cli_config_backend.rs",
    "crates/gents-cli/tests/suites/cli_config_crud.rs",
    "crates/gents-cli/tests/suites/cli_config_diff.rs",
    "crates/gents-cli/tests/suites/cli_config_export_import.rs",
    "crates/gents-cli/tests/suites/cli_config_native_root.rs",
    "crates/gents-cli/tests/suites/cli_config_read.rs",
    "crates/gents-cli/tests/suites/cli_config_skills.rs",
    "crates/gents-cli/tests/suites/cli_config_task_run.rs",
    "crates/gents-cli/tests/suites/cli_config_tools.rs",
    "crates/gents-cli/tests/suites/cli_config_validate.rs",
    "crates/gents-cli/tests/suites/cli_config_workspace_root.rs",
    "crates/gents-cli/tests/suites/cli_diagnose.rs",
    "crates/gents-cli/tests/suites/cli_document.rs",
    "crates/gents-cli/tests/suites/cli_eval.rs",
    "crates/gents-cli/tests/suites/cli_goal.rs",
    "crates/gents-cli/tests/suites/cli_help.rs",
    "crates/gents-cli/tests/suites/cli_init.rs",
    "crates/gents-cli/tests/suites/cli_job.rs",
    "crates/gents-cli/tests/suites/cli_live.rs",
    "crates/gents-cli/tests/suites/cli_mailbox.rs",
    "crates/gents-cli/tests/suites/cli_mcp_probe.rs",
    "crates/gents-cli/tests/suites/cli_pack.rs",
    "crates/gents-cli/tests/suites/cli_provision.rs",
    "crates/gents-cli/tests/suites/cli_reconciliation.rs",
    "crates/gents-cli/tests/suites/cli_request.rs",
    "crates/gents-cli/tests/suites/cli_response.rs",
    "crates/gents-cli/tests/suites/cli_schema.rs",
    "crates/gents-cli/tests/suites/cli_session.rs",
    "crates/gents-cli/tests/suites/cli_status.rs",
    "crates/gents-cli/tests/suites/cli_trace_export.rs",
    "crates/gents-cli/tests/suites/cli_web_research_live.rs",
    "crates/gents-cli/tests/support/failure_artifacts.rs",
    "crates/gents-cli/tests/support/fs.rs",
    "crates/gents-cli/tests/support/graphql.rs",
    "crates/gents-cli/tests/support/mocks/chat.rs",
    "crates/gents-cli/tests/support/mocks/fake_llm.rs",
    "crates/gents-cli/tests/support/mocks/model.rs",
    "crates/gents-cli/tests/support/mocks/openai.rs",
    "crates/gents-cli/tests/support/mod.rs",
    "crates/gents-cli/tests/support/ports.rs",
    "crates/gents-cli/tests/support/process.rs",
    "crates/gents-cli/tests/support/waits.rs",
    "crates/gents-client-host/src/editor_store.rs",
    "crates/gents-client-host/src/main.rs",
    "crates/gents-client-host/src/native.rs",
    "crates/gents-client-host/tests/runtime.rs",
    "crates/gents-client-host/tests/support/configured_actions.rs",
    "crates/gents-client-host/tests/support/console.rs",
    "crates/gents-client-host/tests/support/editor_goal_recovery.rs",
    "crates/gents-client-host/tests/support/editor_goal_removal.rs",
    "crates/gents-client-host/tests/support/editor_goal_replacement.rs",
    "crates/gents-client-host/tests/support/editor_history.rs",
    "crates/gents-client-host/tests/support/editor_related_work.rs",
    "crates/gents-client-host/tests/support/editor_retry.rs",
    "crates/gents-client-host/tests/support/editor_review.rs",
    "crates/gents-client-host/tests/support/editor_workspace.rs",
    "crates/gents-client-host/tests/support/initialization.rs",
    "crates/gents-client-host/tests/support/mod.rs",
    "crates/gents-client-host/tests/support/speech.rs",
    "crates/gents-desktop-bridge/src/accounts.rs",
    "crates/gents-desktop-bridge/src/accounts/login.rs",
    "crates/gents-desktop-bridge/src/approvals_watch.rs",
    "crates/gents-desktop-bridge/src/commands/background.rs",
    "crates/gents-desktop-bridge/src/commands/chat_resolve.rs",
    "crates/gents-desktop-bridge/src/commands/chat.rs",
    "crates/gents-desktop-bridge/src/commands/config.rs",
    "crates/gents-desktop-bridge/src/commands/goal.rs",
    "crates/gents-desktop-bridge/src/commands/mailbox.rs",
    "crates/gents-desktop-bridge/src/commands/mcp_health.rs",
    "crates/gents-desktop-bridge/src/commands/peer.rs",
    "crates/gents-desktop-bridge/src/commands/session.rs",
    "crates/gents-desktop-bridge/src/commands/task.rs",
    "crates/gents-desktop-bridge/src/commands/tool_service.rs",
    "crates/gents-desktop-bridge/src/commands/util.rs",
    "crates/gents-desktop-bridge/src/host.rs",
    "crates/gents-desktop-bridge/src/host/actions.rs",
    "crates/gents-desktop-bridge/src/host/connection.rs",
    "crates/gents-desktop-bridge/src/host/local.rs",
    "crates/gents-desktop-bridge/src/host/local/unix.rs",
    "crates/gents-desktop-bridge/src/host/local/windows.rs",
    "crates/gents-desktop-bridge/src/host/local/windows/security.rs",
    "crates/gents-desktop-bridge/src/host/permissions.rs",
    "crates/gents-desktop-bridge/src/host/service.rs",
    "crates/gents-desktop-bridge/src/host/settings.rs",
    "crates/gents-desktop-bridge/src/host/speech.rs",
    "crates/gents-desktop-bridge/src/host/speech/transport.rs",
    "crates/gents-desktop-bridge/src/inference_setup.rs",
    "crates/gents-desktop-bridge/src/plugin.rs",
    "crates/gents-desktop-bridge/src/provenance.rs",
    "crates/gents-desktop-bridge/src/snapshot/session/live_delta.rs",
    "crates/gents-desktop-bridge/src/snapshot/session/request_context.rs",
    "crates/gents-desktop-bridge/src/tauri_commands/allowed_dirs.rs",
    "crates/gents-desktop-bridge/src/tauri_commands/inference_setup.rs",
    "crates/gents-desktop-bridge/src/tauri_commands/lifecycle.rs",
    "crates/gents-desktop-bridge/src/tauri_commands/managed_server.rs",
    "crates/gents-desktop-bridge/src/tauri_commands/operations.rs",
    "crates/gents-desktop-bridge/src/tauri_commands/packs.rs",
    "crates/gents-desktop-bridge/src/tauri_commands/ux_plugins.rs",
    "crates/gents-desktop-bridge/src/tests/support.rs",
    "crates/gents-desktop-core/src/client/collection_resolver.rs",
    "crates/gents-desktop-core/src/client/core.rs",
    "crates/gents-desktop-core/src/client/core/bootstrap.rs",
    "crates/gents-desktop-core/src/client/core/enrollment.rs",
    "crates/gents-desktop-core/src/client/core/file_review.rs",
    "crates/gents-desktop-core/src/client/core/goal.rs",
    "crates/gents-desktop-core/src/client/core/observation_scope.rs",
    "crates/gents-desktop-core/src/client/core/p2p_ops.rs",
    "crates/gents-desktop-core/src/client/core/route_manager.rs",
    "crates/gents-desktop-core/src/client/core/supervisor.rs",
    "crates/gents-desktop-core/src/client/core/sync_state.rs",
    "crates/gents-desktop-core/src/client/core/tests.rs",
    "crates/gents-desktop-core/src/client/core/writes.rs",
    "crates/gents-desktop-core/src/client/core/writes/pending_submissions.rs",
    "crates/gents-desktop-core/src/client/mutations/chat/binding.rs",
    "crates/gents-desktop-core/src/client/mutations/chat/request.rs",
    "crates/gents-desktop-core/src/client/mutations/chat/request/receipt.rs",
    "crates/gents-desktop-core/src/client/mutations/chat/request/receipt/journal.rs",
    "crates/gents-desktop-core/src/client/mutations/chat/request/tests.rs",
    "crates/gents-desktop-core/src/client/mutations/chat/session.rs",
    "crates/gents-desktop-core/src/client/mutations/graphql.rs",
    "crates/gents-desktop-core/src/client/mutations/manage/behavior.rs",
    "crates/gents-desktop-core/src/client/mutations/manage/inference.rs",
    "crates/gents-desktop-core/src/client/mutations/manage/mod.rs",
    "crates/gents-desktop-core/src/client/mutations/manage/principal.rs",
    "crates/gents-desktop-core/src/client/mutations/manage/principal/editor.rs",
    "crates/gents-desktop-core/src/client/mutations/manage/profile.rs",
    "crates/gents-desktop-core/src/client/mutations/manage/skill.rs",
    "crates/gents-desktop-core/src/client/mutations/manage/task.rs",
    "crates/gents-desktop-core/src/client/mutations/manage/tools.rs",
    "crates/gents-desktop-core/src/client/paths.rs",
    "crates/gents-desktop-core/src/client/peer_directory.rs",
    "crates/gents-desktop-core/src/client/principal_identity.rs",
    "crates/gents-desktop-core/src/client/query.rs",
    "crates/gents-desktop-core/src/client/query/request_closure.rs",
    "crates/gents-desktop-core/src/client/query/session_tip.rs",
    "crates/gents-desktop-core/src/client/query/session_transcript.rs",
    "crates/gents-desktop-core/src/client/query/snapshot_loaders.rs",
    "crates/gents-desktop-core/src/client/schema.rs",
    "crates/gents-desktop-core/src/error.rs",
    "crates/gents-desktop-core/src/home_reset.rs",
    "crates/gents-desktop-core/src/inference.rs",
    "crates/gents-desktop-core/src/local_runtime/goal_control.rs",
    "crates/gents-desktop-core/src/local_runtime/http.rs",
    "crates/gents-desktop-core/src/local_runtime/mod.rs",
    "crates/gents-desktop-core/src/local_runtime/request_input.rs",
    "crates/gents-desktop-core/src/local_runtime/workspace_permissions.rs",
    "crates/gents-desktop-core/tests/client_core.rs",
    "crates/gents-desktop-core/tests/client_store.rs",
    "crates/gents-desktop-core/tests/manage_view.rs",
    "crates/gents-desktop-core/tests/submission_api.rs",
    "crates/gents-loop/src/compaction_history.rs",
    "crates/gents-loop/src/compaction.rs",
    "crates/gents-loop/src/end_to_end_test.rs",
    "crates/gents-loop/src/live_output.rs",
    "crates/gents-loop/src/loop_stream.rs",
    "crates/gents-loop/src/loop_stream/contract.rs",
    "crates/gents-loop/src/loop_stream/one_shot.rs",
    "crates/gents-loop/src/loop_stream/request_assembly.rs",
    "crates/gents-loop/src/output_obligation.rs",
    "crates/gents-loop/src/provider_audit.rs",
    "crates/gents-loop/src/rendered_request.rs",
    "crates/gents-loop/src/rendered_request/scope.rs",
    "crates/gents-loop/src/rendered_request/transport.rs",
    "crates/gents-loop/src/request_lifecycle.rs",
    "crates/gents-loop/src/session_hook.rs",
    "crates/gents-loop/src/stream_processor.rs",
    "crates/gents-loop/src/stream_writer.rs",
    "crates/gents/examples/serve_default_agent.rs",
    "crates/gents/src/admission/persistence.rs",
    "crates/gents/src/admission/recovery.rs",
    "crates/gents/src/agent.rs",
    "crates/gents/src/agent/builder.rs",
    "crates/gents/src/agent/daemon.rs",
    "crates/gents/src/agent/daemon/auxiliary_tasks.rs",
    "crates/gents/src/agent/daemon/inference.rs",
    "crates/gents/src/agent/daemon/pre_inference_interrupt_tests.rs",
    "crates/gents/src/agent/daemon/request.rs",
    "crates/gents/src/agent/daemon/title.rs",
    "crates/gents/src/agent/daemon/title/owned_tests.rs",
    "crates/gents/src/agent/daemon/title/review.rs",
    "crates/gents/src/agent/daemon/title/review/task.rs",
    "crates/gents/src/agent/daemon/wake_input.rs",
    "crates/gents/src/agent/directory_projection.rs",
    "crates/gents/src/agent/document_view/apply.rs",
    "crates/gents/src/agent/document_view/load.rs",
    "crates/gents/src/agent/document_view/mod.rs",
    "crates/gents/src/agent/document_view/snapshot.rs",
    "crates/gents/src/agent/loop_stream/tests/budgeting.rs",
    "crates/gents/src/agent/loop_stream/tests/capture.rs",
    "crates/gents/src/agent/output_obligation.rs",
    "crates/gents/src/agent/p2p_reconcile/embedded_impl.rs",
    "crates/gents/src/agent/p2p_reconcile/endpoint.rs",
    "crates/gents/src/agent/p2p_reconcile/engine.rs",
    "crates/gents/src/agent/p2p_reconcile/engine/desired.rs",
    "crates/gents/src/agent/p2p_reconcile/engine/reconcile.rs",
    "crates/gents/src/agent/p2p_reconcile/engine/remote_topology.rs",
    "crates/gents/src/agent/p2p_reconcile/engine/store.rs",
    "crates/gents/src/agent/p2p_reconcile/engine/sweep.rs",
    "crates/gents/src/agent/p2p_reconcile/engine/tests.rs",
    "crates/gents/src/agent/p2p_reconcile/engine/tests/support.rs",
    "crates/gents/src/agent/p2p_reconcile/enrollment_candidate.rs",
    "crates/gents/src/agent/p2p_reconcile/enrollment_offer.rs",
    "crates/gents/src/agent/p2p_reconcile/enrollment_reconcile.rs",
    "crates/gents/src/agent/p2p_reconcile/enrollment_store.rs",
    "crates/gents/src/agent/p2p_reconcile/policy.rs",
    "crates/gents/src/agent/p2p_reconcile/profiles.rs",
    "crates/gents/src/agent/p2p_reconcile/registry.rs",
    "crates/gents/src/agent/p2p_reconcile/session_command_authority.rs",
    "crates/gents/src/agent/p2p_reconcile/session_commands.rs",
    "crates/gents/src/agent/p2p_reconcile/session_goal_control.rs",
    "crates/gents/src/agent/p2p_reconcile/session_hydration_closure.rs",
    "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile.rs",
    "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/file_images.rs",
    "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/file_images/tests.rs",
    "crates/gents/src/agent/p2p_reconcile/session_hydration_reconcile/file_images/tests/live.rs",
    "crates/gents/src/agent/p2p_reconcile/session_input_edit.rs",
    "crates/gents/src/agent/p2p_reconcile/templates.rs",
    "crates/gents/src/agent/principal_assembly.rs",
    "crates/gents/src/agent/reconcile.rs",
    "crates/gents/src/agent/reconcile/slot.rs",
    "crates/gents/src/agent/reconcile/tests.rs",
    "crates/gents/src/agent/runtime/context.rs",
    "crates/gents/src/agent/runtime/control_watcher.rs",
    "crates/gents/src/agent/runtime/router.rs",
    "crates/gents/src/agent/runtime/startup.rs",
    "crates/gents/src/agent/runtime/tests/control_watcher.rs",
    "crates/gents/src/agent/runtime/tests/router.rs",
    "crates/gents/src/agent/runtime/tests/startup_recovery.rs",
    "crates/gents/src/agent/runtime/tests/support.rs",
    "crates/gents/src/agent/supervision.rs",
    "crates/gents/src/agent/tests/document_loading.rs",
    "crates/gents/src/application_write/mod.rs",
    "crates/gents/src/application_write/plugin_writes.rs",
    "crates/gents/src/backend_health.rs",
    "crates/gents/src/backend_registry.rs",
    "crates/gents/src/backend_registry/tests.rs",
    "crates/gents/src/background_completion_diagnostics.rs",
    "crates/gents/src/background_completion.rs",
    "crates/gents/src/background_completion/notification_delivery.rs",
    "crates/gents/src/background_completion/observer.rs",
    "crates/gents/src/background_completion/side_effects.rs",
    "crates/gents/src/background_tools.rs",
    "crates/gents/src/background_tools/final_output.rs",
    "crates/gents/src/blocked_turn.rs",
    "crates/gents/src/callback/claim.rs",
    "crates/gents/src/callback/documents.rs",
    "crates/gents/src/callback/mod.rs",
    "crates/gents/src/callback/plugin.rs",
    "crates/gents/src/callback/run.rs",
    "crates/gents/src/callback/scan.rs",
    "crates/gents/src/callback/workers.rs",
    "crates/gents/src/chatgpt_codex.rs",
    "crates/gents/src/claude_subscription.rs",
    "crates/gents/src/compaction.rs",
    "crates/gents/src/completion_factory.rs",
    "crates/gents/src/config_client/agent_behavior.rs",
    "crates/gents/src/config_client/desired_state.rs",
    "crates/gents/src/config_client/event_source_cursor.rs",
    "crates/gents/src/config_client/graphql.rs",
    "crates/gents/src/config_client/inference_backend.rs",
    "crates/gents/src/config_client/inference_profile.rs",
    "crates/gents/src/config_client/mod.rs",
    "crates/gents/src/config_client/profile_switch.rs",
    "crates/gents/src/config_client/query_tests.rs",
    "crates/gents/src/config_client/receipt_owner_tests.rs",
    "crates/gents/src/config_client/retry.rs",
    "crates/gents/src/config_client/schema_install.rs",
    "crates/gents/src/config_client/schema_management.rs",
    "crates/gents/src/config_client/tools.rs",
    "crates/gents/src/config_client/txn_native_claim_lifetime_conformance.rs",
    "crates/gents/src/config_client/txn_owner_tests.rs",
    "crates/gents/src/config_client/txn_stream_waiter_conformance.rs",
    "crates/gents/src/config_client/txn.rs",
    "crates/gents/src/defra_query/bounded.rs",
    "crates/gents/src/defra_query/command.rs",
    "crates/gents/src/defra_query/field_page.rs",
    "crates/gents/src/defra_query/mod.rs",
    "crates/gents/src/defra_query/native_filter.rs",
    "crates/gents/src/defra_query/query.rs",
    "crates/gents/src/defra_query/render.rs",
    "crates/gents/src/defra_query/schema.rs",
    "crates/gents/src/defra_query/search.rs",
    "crates/gents/src/defra_write/input.rs",
    "crates/gents/src/defra_write/mod.rs",
    "crates/gents/src/document_config/behavior.rs",
    "crates/gents/src/document_config/chain_key_binding.rs",
    "crates/gents/src/document_config/context.rs",
    "crates/gents/src/document_config/datastore_tool_surface.rs",
    "crates/gents/src/document_config/eth_tool.rs",
    "crates/gents/src/document_config/eth_validation_tests.rs",
    "crates/gents/src/document_config/event_trigger.rs",
    "crates/gents/src/document_config/inference_profile.rs",
    "crates/gents/src/document_config/mod.rs",
    "crates/gents/src/document_config/principal.rs",
    "crates/gents/src/document_config/references.rs",
    "crates/gents/src/document_config/schedule.rs",
    "crates/gents/src/document_config/surface_tool.rs",
    "crates/gents/src/document_config/tests.rs",
    "crates/gents/src/document_config/tools.rs",
    "crates/gents/src/document_config/write_tool.rs",
    "crates/gents/src/eth/call_tool.rs",
    "crates/gents/src/eth/submit.rs",
    "crates/gents/src/eval/checks/schema_matches.rs",
    "crates/gents/src/eval/documents.rs",
    "crates/gents/src/eval/report/breakdown.rs",
    "crates/gents/src/eval/report/build.rs",
    "crates/gents/src/eval/report/compare.rs",
    "crates/gents/src/eval/report/evidence.rs",
    "crates/gents/src/eval/report/mod.rs",
    "crates/gents/src/eval/report/store.rs",
    "crates/gents/src/eval/runner/embedded/executor.rs",
    "crates/gents/src/eval/runner/embedded/home.rs",
    "crates/gents/src/eval/runner/embedded/inspect.rs",
    "crates/gents/src/eval/runner/embedded/live.rs",
    "crates/gents/src/eval/runner/embedded/observe.rs",
    "crates/gents/src/eval/runner/files.rs",
    "crates/gents/src/eval/runner/freeze.rs",
    "crates/gents/src/eval/runner/mod.rs",
    "crates/gents/src/eval/runner/record.rs",
    "crates/gents/src/eval/runner/review.rs",
    "crates/gents/src/eval/runner/view.rs",
    "crates/gents/src/goal.rs",
    "crates/gents/src/goal/claimed_publication.rs",
    "crates/gents/src/goal/claimed_publication/wait_observation.rs",
    "crates/gents/src/goal/client_control.rs",
    "crates/gents/src/goal/client_submission.rs",
    "crates/gents/src/goal/edit.rs",
    "crates/gents/src/goal/first_request_binding.rs",
    "crates/gents/src/goal/first_request_binding/contract_tests.rs",
    "crates/gents/src/goal/history.rs",
    "crates/gents/src/goal/observation.rs",
    "crates/gents/src/goal/operator_resume.rs",
    "crates/gents/src/goal/request_assignment.rs",
    "crates/gents/src/goal/request_assignment/history_tests.rs",
    "crates/gents/src/goal/request_head.rs",
    "crates/gents/src/goal/tool_context.rs",
    "crates/gents/src/graph_package/catalog.rs",
    "crates/gents/src/graph_package/entry.rs",
    "crates/gents/src/graph_package/install.rs",
    "crates/gents/src/graph_pipeline/live.rs",
    "crates/gents/src/graph_pipeline/logical_invocation.rs",
    "crates/gents/src/graph_pipeline/run.rs",
    "crates/gents/src/graph_pipeline/runtime.rs",
    "crates/gents/src/graph_pipeline/tools.rs",
    "crates/gents/src/graph_pipeline/workspace_lineage_contract_tests.rs",
    "crates/gents/src/graph_pipeline/workspace_lineage.rs",
    "crates/gents/src/graphql.rs",
    "crates/gents/src/health_checker.rs",
    "crates/gents/src/hook.rs",
    "crates/gents/src/hook/persistence/background_control.rs",
    "crates/gents/src/hook/persistence/background_tools.rs",
    "crates/gents/src/hook/persistence/foreground_handoff.rs",
    "crates/gents/src/hook/persistence/goal_tools.rs",
    "crates/gents/src/hook/persistence/helpers.rs",
    "crates/gents/src/hook/persistence/prompt_hook.rs",
    "crates/gents/src/hook/persistence/session_message.rs",
    "crates/gents/src/hook/tests.rs",
    "crates/gents/src/hook/tests/goal_context.rs",
    "crates/gents/src/identity.rs",
    "crates/gents/src/interrupt.rs",
    "crates/gents/src/interrupt/queue_tests.rs",
    "crates/gents/src/lean_vocab_test/canonical_execution/native_adapter.rs",
    "crates/gents/src/lean_vocab_test/canonical_presentation/native_adapter.rs",
    "crates/gents/src/lib.rs",
    "crates/gents/src/lifecycle.rs",
    "crates/gents/src/lifecycle/background_wake_recovery.rs",
    "crates/gents/src/lifecycle/canonical_recovery.rs",
    "crates/gents/src/lifecycle/claim.rs",
    "crates/gents/src/lifecycle/execution_lease.rs",
    "crates/gents/src/lifecycle/execution_renewal.rs",
    "crates/gents/src/lifecycle/manual.rs",
    "crates/gents/src/lifecycle/materialize.rs",
    "crates/gents/src/lifecycle/query.rs",
    "crates/gents/src/lifecycle/queue.rs",
    "crates/gents/src/lifecycle/queue/atomic_inputs.rs",
    "crates/gents/src/lifecycle/queue/coalescing.rs",
    "crates/gents/src/lifecycle/queue/draining.rs",
    "crates/gents/src/lifecycle/queue/edit_submission.rs",
    "crates/gents/src/lifecycle/queue/enqueue.rs",
    "crates/gents/src/lifecycle/queue/folding.rs",
    "crates/gents/src/lifecycle/queue/goal_continuation.rs",
    "crates/gents/src/lifecycle/queue/intake.rs",
    "crates/gents/src/lifecycle/queue/management.rs",
    "crates/gents/src/lifecycle/queue/mutation.rs",
    "crates/gents/src/lifecycle/queue/position.rs",
    "crates/gents/src/lifecycle/queue/submission.rs",
    "crates/gents/src/lifecycle/queue/tests.rs",
    "crates/gents/src/lifecycle/recovery.rs",
    "crates/gents/src/lifecycle/terminal_tools.rs",
    "crates/gents/src/lifecycle/transition.rs",
    "crates/gents/src/llm/backend_client.rs",
    "crates/gents/src/mailbox.rs",
    "crates/gents/src/mailbox/domain.rs",
    "crates/gents/src/mailbox/notification.rs",
    "crates/gents/src/mailbox/reply_tests.rs",
    "crates/gents/src/mailbox/reply.rs",
    "crates/gents/src/managed_exec/ownership.rs",
    "crates/gents/src/mcp_pool.rs",
    "crates/gents/src/mcp_pool/tests.rs",
    "crates/gents/src/meta_tools.rs",
    "crates/gents/src/meta_tools/call.rs",
    "crates/gents/src/meta_tools/shared.rs",
    "crates/gents/src/migration.rs",
    "crates/gents/src/node_readiness_publisher.rs",
    "crates/gents/src/node_readiness_publisher/tests.rs",
    "crates/gents/src/oauth_credential.rs",
    "crates/gents/src/oauth_http.rs",
    "crates/gents/src/oneshot.rs",
    "crates/gents/src/optimization/driver.rs",
    "crates/gents/src/optimization/driver/matrix.rs",
    "crates/gents/src/optimization/gate.rs",
    "crates/gents/src/optimization/job.rs",
    "crates/gents/src/optimization/promote.rs",
    "crates/gents/src/optimization/proposer.rs",
    "crates/gents/src/optimization/references.rs",
    "crates/gents/src/optimization/show.rs",
    "crates/gents/src/optimization/subject.rs",
    "crates/gents/src/optimization/target.rs",
    "crates/gents/src/p2p_observability.rs",
    "crates/gents/src/p2p_tool.rs",
    "crates/gents/src/pack_archive.rs",
    "crates/gents/src/pack_archive/format.rs",
    "crates/gents/src/pack_registry.rs",
    "crates/gents/src/pack_resolve.rs",
    "crates/gents/src/pack_store.rs",
    "crates/gents/src/pack.rs",
    "crates/gents/src/pack/authoring/build.rs",
    "crates/gents/src/pack/authoring/check.rs",
    "crates/gents/src/pack/authoring/inspect.rs",
    "crates/gents/src/pack/authoring/names.rs",
    "crates/gents/src/pack/authoring/scaffold.rs",
    "crates/gents/src/pack/authoring/test.rs",
    "crates/gents/src/pack/home_install.rs",
    "crates/gents/src/pack/inference.rs",
    "crates/gents/src/pack/installation.rs",
    "crates/gents/src/pack/installation/dependencies.rs",
    "crates/gents/src/pack/installation/documents.rs",
    "crates/gents/src/pack/installation/graph.rs",
    "crates/gents/src/pack/loader.rs",
    "crates/gents/src/pack/loader/tests.rs",
    "crates/gents/src/pack/local.rs",
    "crates/gents/src/pack/materialize.rs",
    "crates/gents/src/pack/plugin_lifecycle.rs",
    "crates/gents/src/pack/provenance.rs",
    "crates/gents/src/pack/publication_contract_tests.rs",
    "crates/gents/src/periodic_recovery.rs",
    "crates/gents/src/plugin_job/ancestry.rs",
    "crates/gents/src/plugin_job/control.rs",
    "crates/gents/src/plugin_job/dispatch.rs",
    "crates/gents/src/plugin_job/driver.rs",
    "crates/gents/src/plugin_job/effects.rs",
    "crates/gents/src/plugin_job/input.rs",
    "crates/gents/src/plugin_job/origin.rs",
    "crates/gents/src/plugin_job/records.rs",
    "crates/gents/src/plugin_job/recovery.rs",
    "crates/gents/src/plugin_job/settings.rs",
    "crates/gents/src/plugin_job/view.rs",
    "crates/gents/src/plugin.rs",
    "crates/gents/src/plugin/allowed_tests.rs",
    "crates/gents/src/plugin/allowed.rs",
    "crates/gents/src/plugin/allowed/protected.rs",
    "crates/gents/src/plugin/approval.rs",
    "crates/gents/src/plugin/approval/question.rs",
    "crates/gents/src/plugin/approval/workspace.rs",
    "crates/gents/src/plugin/approval/workspace/command.rs",
    "crates/gents/src/plugin/approval/workspace/context.rs",
    "crates/gents/src/plugin/approval/workspace/file.rs",
    "crates/gents/src/plugin/approval/workspace/review_task.rs",
    "crates/gents/src/plugin/authority.rs",
    "crates/gents/src/plugin/bound.rs",
    "crates/gents/src/plugin/catalog.rs",
    "crates/gents/src/plugin/data.rs",
    "crates/gents/src/plugin/executor.rs",
    "crates/gents/src/plugin/http_calls.rs",
    "crates/gents/src/plugin/install.rs",
    "crates/gents/src/plugin/job_calls.rs",
    "crates/gents/src/plugin/model_calls_tests.rs",
    "crates/gents/src/plugin/model_calls.rs",
    "crates/gents/src/plugin/mounts.rs",
    "crates/gents/src/plugin/rounds.rs",
    "crates/gents/src/plugin/service.rs",
    "crates/gents/src/plugin/store.rs",
    "crates/gents/src/plugin/tests/executor.rs",
    "crates/gents/src/plugin/tool.rs",
    "crates/gents/src/provider_context_reduction.rs",
    "crates/gents/src/provider_input/tests.rs",
    "crates/gents/src/registry.rs",
    "crates/gents/src/rendered_request/commits.rs",
    "crates/gents/src/rendered_request/mod.rs",
    "crates/gents/src/rendered_request/sink.rs",
    "crates/gents/src/request_admission.rs",
    "crates/gents/src/request_admission/workspace_cleanup_tests.rs",
    "crates/gents/src/request_binding.rs",
    "crates/gents/src/run_timeline_fetch.rs",
    "crates/gents/src/run_timeline_fetch/context_loaders.rs",
    "crates/gents/src/run_timeline_fetch/event_loaders.rs",
    "crates/gents/src/run_timeline_fetch/query_helpers.rs",
    "crates/gents/src/run_timeline_fetch/request_loaders.rs",
    "crates/gents/src/run_timeline_fetch/validation.rs",
    "crates/gents/src/runtime_snapshot.rs",
    "crates/gents/src/runtime_status.rs",
    "crates/gents/src/runtime_trace.rs",
    "crates/gents/src/schedule_cron.rs",
    "crates/gents/src/schema_tool/help.rs",
    "crates/gents/src/schema_tool/mod.rs",
    "crates/gents/src/schema.rs",
    "crates/gents/src/self_config/agent_management.rs",
    "crates/gents/src/self_config/agent.rs",
    "crates/gents/src/self_config/command.rs",
    "crates/gents/src/self_config/command/authoring.rs",
    "crates/gents/src/self_config/command/crud.rs",
    "crates/gents/src/self_config/command/datastore.rs",
    "crates/gents/src/self_config/command/discovery.rs",
    "crates/gents/src/self_config/command/skill.rs",
    "crates/gents/src/self_config/command/validate.rs",
    "crates/gents/src/self_config/mod.rs",
    "crates/gents/src/self_config/ops.rs",
    "crates/gents/src/self_config/pack_plugins.rs",
    "crates/gents/src/self_config/read.rs",
    "crates/gents/src/self_config/tests.rs",
    "crates/gents/src/session_message.rs",
    "crates/gents/src/session_message/discovery.rs",
    "crates/gents/src/session_message/inspection.rs",
    "crates/gents/src/session_origin.rs",
    "crates/gents/src/session.rs",
    "crates/gents/src/session/canonical_rows.rs",
    "crates/gents/src/session/compaction_entries.rs",
    "crates/gents/src/session/control.rs",
    "crates/gents/src/session/fork.rs",
    "crates/gents/src/session/history.rs",
    "crates/gents/src/session/observations.rs",
    "crates/gents/src/session/output_replay_tests.rs",
    "crates/gents/src/session/output.rs",
    "crates/gents/src/session/query.rs",
    "crates/gents/src/session/request_output.rs",
    "crates/gents/src/session/sessions.rs",
    "crates/gents/src/skills/import.rs",
    "crates/gents/src/startup_recovery.rs",
    "crates/gents/src/storage_backend.rs",
    "crates/gents/src/store_key.rs",
    "crates/gents/src/store_key/upgrade.rs",
    "crates/gents/src/store_key/upgrade/tests.rs",
    "crates/gents/src/streaming.rs",
    "crates/gents/src/streaming/auxiliary_tests.rs",
    "crates/gents/src/streaming/auxiliary.rs",
    "crates/gents/src/streaming/canonical_tests.rs",
    "crates/gents/src/streaming/canonical.rs",
    "crates/gents/src/task_hooks.rs",
    "crates/gents/src/task_hooks/records.rs",
    "crates/gents/src/tool_call_lifecycle.rs",
    "crates/gents/src/tool_call_lifecycle/admission_fixture.rs",
    "crates/gents/src/tool_call_lifecycle/completion_owner_conformance.rs",
    "crates/gents/src/tool_call_lifecycle/delivery.rs",
    "crates/gents/src/tool_call_lifecycle/file_review_access.rs",
    "crates/gents/src/tool_call_lifecycle/plugin_effect.rs",
    "crates/gents/src/tool_call_lifecycle/query.rs",
    "crates/gents/src/tool_call_lifecycle/query/accepted.rs",
    "crates/gents/src/tool_call_lifecycle/query/result.rs",
    "crates/gents/src/tool_call_lifecycle/recovery.rs",
    "crates/gents/src/tool_call_lifecycle/transition.rs",
    "crates/gents/src/tool_call_lifecycle/transition/cancel.rs",
    "crates/gents/src/tool_call_lifecycle/transition/mode_policy.rs",
    "crates/gents/src/tool_call_lifecycle/transition/native.rs",
    "crates/gents/src/tool_call_lifecycle/transition/plugin_job.rs",
    "crates/gents/src/tool_call_lifecycle/transition/session_message.rs",
    "crates/gents/src/tool_control.rs",
    "crates/gents/src/tool_surface/behavior_config.rs",
    "crates/gents/src/tool_surface/build.rs",
    "crates/gents/src/tool_surface/mod.rs",
    "crates/gents/src/tool_surface/output_budget.rs",
    "crates/gents/src/tool_surface/root_admission.rs",
    "crates/gents/src/tool_surface/selection.rs",
    "crates/gents/src/toolset.rs",
    "crates/gents/src/toolset/bash_tools.rs",
    "crates/gents/src/toolset/cli_tool.rs",
    "crates/gents/src/toolset/context_budget.rs",
    "crates/gents/src/toolset/file_tools.rs",
    "crates/gents/src/toolset/file_tools/change_capture.rs",
    "crates/gents/src/toolset/goal.rs",
    "crates/gents/src/toolset/lsp/actions.rs",
    "crates/gents/src/toolset/lsp/mod.rs",
    "crates/gents/src/toolset/memory.rs",
    "crates/gents/src/toolset/native_runner.rs",
    "crates/gents/src/toolset/session_history.rs",
    "crates/gents/src/toolset/session_history/context_details.rs",
    "crates/gents/src/toolset/session_history/discovery.rs",
    "crates/gents/src/toolset/session_message.rs",
    "crates/gents/src/toolset/shared/command.rs",
    "crates/gents/src/toolset/shared/context.rs",
    "crates/gents/src/toolset/shared/context/tests.rs",
    "crates/gents/src/toolset/title_audit_usage.rs",
    "crates/gents/src/trace_export.rs",
    "crates/gents/src/trigger_engine/durable.rs",
    "crates/gents/src/trigger_engine/durable/recovery.rs",
    "crates/gents/src/trigger_engine/durable/retained_tests.rs",
    "crates/gents/src/trigger_engine/event_delivery.rs",
    "crates/gents/src/trigger_engine/event_delivery/source_scan.rs",
    "crates/gents/src/trigger_engine/event_source.rs",
    "crates/gents/src/trigger_engine/event_source/durable.rs",
    "crates/gents/src/trigger_engine/goal_source.rs",
    "crates/gents/src/trigger_engine/manual_source.rs",
    "crates/gents/src/trigger_engine/mod.rs",
    "crates/gents/src/trigger_engine/production_materializer.rs",
    "crates/gents/src/trigger_engine/subscription_source.rs",
    "crates/gents/src/trigger_engine/tests/durable_contract.rs",
    "crates/gents/src/trigger_engine/tests/event_source/unacknowledged.rs",
    "crates/gents/src/trigger_engine/tests/mod.rs",
    "crates/gents/src/trigger_engine/tests/production_materializer.rs",
    "crates/gents/src/trigger_engine/tests/schedule_source.rs",
    "crates/gents/src/usage_observation.rs",
    "crates/gents/src/ux_data.rs",
    "crates/gents/src/ux_plugin.rs",
    "crates/gents/src/watcher.rs",
    "crates/gents/src/watcher/query.rs",
    "crates/gents/src/workspace/artifact.rs",
    "crates/gents/src/workspace/documents.rs",
    "crates/gents/src/workspace/executor.rs",
    "crates/gents/src/workspace/overlay_tests.rs",
    "crates/gents/src/workspace/overlay.rs",
    "crates/gents/src/workspace/quickstart.rs",
    "crates/gents/src/workspace/runtime.rs",
    "crates/gents/src/workspace/tests/operator_base_freeze.rs",
    "crates/gents/src/xai_grok_oauth.rs",
    "crates/gents/tests/concurrent_writer_probe.rs",
    "crates/gents/tests/conformance/event_delivery.rs",
    "crates/gents/tests/conformance/identity.rs",
    "crates/gents/tests/conformance/p2p_replication.rs",
    "crates/gents/tests/conformance/self_config.rs",
    "crates/gents/tests/defradb_genesis_identity.rs",
    "crates/gents/tests/defradb_transaction_atomicity.rs",
    "crates/gents/tests/e2e_live/backend_auth_live.rs",
    "crates/gents/tests/e2e_live/eth_tool_write_live.rs",
    "crates/gents/tests/e2e_live/interrupt_live.rs",
    "crates/gents/tests/e2e_live/lsp_live.rs",
    "crates/gents/tests/e2e_live/p2p_admission_concurrent_live.rs",
    "crates/gents/tests/e2e_live/seed_live.rs",
    "crates/gents/tests/e2e_live/session_message_live.rs",
    "crates/gents/tests/e2e_live/steering_live.rs",
    "crates/gents/tests/e2e_runtime/adapter_projection_external_fixtures.rs",
    "crates/gents/tests/e2e_runtime/projection_acp_policy_lifecycle.rs",
    "crates/gents/tests/e2e_runtime/provider_fixture_redaction.rs",
    "crates/gents/tests/e2e_runtime/provider_fixture_replay.rs",
    "crates/gents/tests/e2e_runtime/queue_management.rs",
    "crates/gents/tests/e2e_triggers/workspace_change_unit_cycle.rs",
    "crates/gents/tests/e2e_triggers/write_tool_trigger_e2e.rs",
    "crates/gents/tests/misc/backend_auth_startup.rs",
    "crates/gents/tests/misc/goal_controller.rs",
    "crates/gents/tests/misc/node_access_control.rs",
    "crates/gents/tests/misc/oneshot_admission_exemption.rs",
    "crates/gents/tests/misc/startup_readiness_barrier.rs",
    "crates/gents/tests/onboarding_evals/p2p.rs",
    "crates/gents/tests/onboarding_evals/sessions.rs",
    "crates/gents/tests/support/accepted_turn.rs",
    "crates/gents/tests/support/fixtures.rs",
    "crates/gents/tests/support/identity_stubs.rs",
    "crates/gents/tests/support/interrupt.rs",
    "crates/gents/tests/support/live_inference.rs",
    "crates/gents/tests/support/mock_endpoint.rs",
    "crates/gents/tests/support/mod.rs",
    "crates/gents/tests/support/pairing_conformance/runner.rs",
    "crates/gents/tests/support/pairing_conformance/scenario.rs",
    "crates/gents/tests/support/snapshots.rs",
    "crates/gents/tests/support/streaming_backend.rs",
    "crates/gents/tests/trigger_delivery_live.rs",
    "crates/gents/tests/trigger_delivery_live/process.rs",
];

/// Workspace members whose manifests still depend on `anyhow`; "Cargo.toml"
/// is the workspace dependency table. The list only shrinks.
#[rustfmt::skip]
const ANYHOW_MANIFEST_ALLOWLIST: &[&str] = &[
    "Cargo.toml",
    "apps/gents-desktop/src-tauri",
    "crates/gents",
    "crates/gents-cli",
    "crates/gents-client-host",
    "crates/gents-desktop-bridge",
    "crates/gents-desktop-core",
    "crates/gents-lean-contract",
    "crates/gents-loop",
];

#[derive(Default)]
struct AnyhowVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for AnyhowVisitor {
    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path
            .segments
            .iter()
            .any(|segment| segment.ident == "anyhow")
        {
            self.sites += 1;
        }
        visit::visit_path(self, path);
    }

    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        let named = match tree {
            syn::UseTree::Path(syn::UsePath { ident, .. })
            | syn::UseTree::Name(syn::UseName { ident })
            | syn::UseTree::Rename(syn::UseRename { ident, .. }) => ident == "anyhow",
            _ => false,
        };
        if named {
            self.sites += 1;
        }
        visit::visit_use_tree(self, tree);
    }

    fn visit_item_extern_crate(&mut self, item: &'ast syn::ItemExternCrate) {
        if item.ident == "anyhow" {
            self.sites += 1;
        }
        visit::visit_item_extern_crate(self, item);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let tokens = syn::buffer::TokenBuffer::new2(invocation.tokens.clone());
        if macro_names_anyhow(tokens.begin()) {
            self.sites += 1;
        }
        visit::visit_macro(self, invocation);
    }
}

fn macro_names_anyhow(mut cursor: syn::buffer::Cursor) -> bool {
    while !cursor.eof() {
        if let Some((inside, _, _, next)) = cursor.any_group() {
            if macro_names_anyhow(inside) {
                return true;
            }
            cursor = next;
        } else if let Some((ident, next)) = cursor.ident() {
            let invoked = next.punct().is_some_and(|(punct, after)| {
                punct.as_char() == '!'
                    || (punct.as_char() == ':'
                        && after.punct().is_some_and(|(p, _)| p.as_char() == ':'))
            });
            if ident == "anyhow" && invoked {
                return true;
            }
            cursor = next;
        } else if let Some((_, next)) = cursor.token_tree() {
            cursor = next;
        } else {
            break;
        }
    }
    false
}

fn anyhow_sites(syntax: &syn::File) -> usize {
    let mut visitor = AnyhowVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

fn ratchet_violations(
    actual: &BTreeSet<String>,
    allowlist: &[&str],
    added: &str,
    removed: &str,
) -> Vec<String> {
    let allowed = allowlist
        .iter()
        .map(|path| path.to_string())
        .collect::<BTreeSet<_>>();
    let mut violations = actual
        .difference(&allowed)
        .map(|path| format!("{path}: {added}"))
        .collect::<Vec<_>>();
    violations.extend(
        allowed
            .difference(actual)
            .map(|path| format!("{path}: {removed}")),
    );
    violations
}

/// Errors are typed `thiserror` enums owned by the module that originates
/// them, matching DefraDB; no file may start naming `anyhow`.
#[test]
fn errors_use_thiserror() {
    let root = repo_root();
    let mut sources = Vec::new();
    for directory in ["crates", "apps"] {
        all_rust_sources(&root.join(directory), &mut sources);
    }
    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            (anyhow_sites(&syntax) > 0).then(|| {
                path.strip_prefix(&root)
                    .expect("source under repo root")
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect::<BTreeSet<_>>();
    let violations = ratchet_violations(
        &actual,
        ANYHOW_ALLOWLIST,
        "names anyhow; return a module-owned thiserror enum",
        "no longer names anyhow; remove its allowlist entry",
    );
    assert!(
        violations.is_empty(),
        "thiserror ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn anyhow_dependencies_only_shrink() {
    let root = repo_root();
    let workspace = toml_file(&root.join("Cargo.toml"));
    let names_anyhow = |table: Option<&toml::Value>| {
        table.and_then(toml::Value::as_table).is_some_and(|table| {
            table.iter().any(|(key, specification)| {
                key == "anyhow"
                    || specification.get("package").and_then(toml::Value::as_str) == Some("anyhow")
            })
        })
    };
    let mut actual = BTreeSet::new();
    if names_anyhow(workspace["workspace"].get("dependencies")) {
        actual.insert("Cargo.toml".to_string());
    }
    for member in workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members")
    {
        let member = member.as_str().expect("member path");
        let manifest = toml_file(&root.join(member).join("Cargo.toml"));
        let mut sections = vec![&manifest];
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            sections.extend(targets.values().filter_map(toml::Value::as_table));
        }
        let depends = sections.into_iter().any(|section| {
            ["dependencies", "dev-dependencies", "build-dependencies"]
                .into_iter()
                .any(|kind| names_anyhow(section.get(kind)))
        });
        if depends {
            actual.insert(member.to_string());
        }
    }
    let violations = ratchet_violations(
        &actual,
        ANYHOW_MANIFEST_ALLOWLIST,
        "depends on anyhow; use thiserror",
        "no longer depends on anyhow; remove its allowlist entry",
    );
    assert!(
        violations.is_empty(),
        "anyhow dependency ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn thiserror_fence_counts_anyhow_names() {
    for (source, sites) in [
        ("use anyhow::Result;", 1),
        ("use anyhow::{bail, Context};", 1),
        ("fn f() -> anyhow::Result<()> { Ok(()) }", 1),
        ("fn f() { anyhow::bail!(\"x\"); }", 1),
        ("fn f() { let e = anyhow!(\"x\"); }", 1),
        (
            "fn f() { assert!(matches!(g(), Err(e) if e.is::<anyhow::Error>())); }",
            1,
        ),
        ("extern crate anyhow;", 1),
        (
            "#[derive(thiserror::Error)] enum E { #[error(\"anyhow\")] A }",
            0,
        ),
        ("fn f() { let anyhow = 1; }", 0),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(anyhow_sites(&syntax), sites, "{source}");
    }
}
