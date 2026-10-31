use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, ExprMethodCall, ItemConst, ItemFn, ItemImpl, ItemMod};

const OWNER_FILES: &[&str] = &[
    "crates/gents/src/store/config_client/graphql.rs",
    "crates/gents/src/store/config_client/retry.rs",
    "crates/gents/src/store/config_client/txn/auto_commit.rs",
    "crates/gents/src/store/config_client/txn/execute.rs",
    "crates/gents/src/store/config_client/txn/transact_entry.rs",
    "crates/gents-protocol/src/graphql/transport.rs",
];

const READ_OWNER_FILES: &[&str] = &[
    "crates/gents/src/store/config_client/txn/auto_commit.rs",
    "crates/gents/src/store/graphql.rs",
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

const OAUTH_CREDENTIAL_OWNER: &str = "crates/gents/src/inference/auth/credential.rs";

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
    ("crates/gents/src/runtime/agent/document_view/load.rs", 1),
    ("crates/gents/src/runtime/agent/document_view/mod.rs", 1),
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
                "{path}: {found} OAuthCredential row read(s), allowlisted {limit}; pick through inference::auth::credential::resolve_oauth_credential"
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
        ("fn f(a: &A) { gents::inference::auth::credential::oauth_credential_query(\"d\", \"p\"); }", 1, 0),
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
    "crates/gents/src/inference/llm/rig_compat.rs",
    "crates/gents/src/turn/provider_input",
    "crates/gents/src/runtime/agent/loop_stream",
];

#[rustfmt::skip]
const RIG_PATH_ALLOWLIST: &[&str] = &[
    "crates/gents-loop/src/claude_messages_body.rs",
    "crates/gents-loop/src/provider_stream.rs",
    "crates/gents-loop/src/provider_usage.rs",
    "crates/gents-loop/src/rendered_request/transport.rs",
    "crates/gents/src/inference/clients/chatgpt_codex.rs",
    "crates/gents/src/inference/clients/claude_messages.rs",
    "crates/gents/src/inference/clients/claude_messages/sse_body.rs",
    "crates/gents/src/inference/clients/claude_messages/sse_state.rs",
    "crates/gents/src/inference/clients/claude_messages/transport.rs",
    "crates/gents/src/inference/clients/claude_subscription.rs",
    "crates/gents/src/inference/clients/inference_http.rs",
    "crates/gents/src/inference/llm/backend_client.rs",
    "crates/gents/src/inference/auth/http.rs",
    "crates/gents/src/inference/clients/provider_http.rs",
    "crates/gents/src/inference/usage_observation.rs",
    "crates/gents/src/inference/clients/xai_grok.rs",
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
    "crates/gents/src/tools/mcp/pool.rs",
    "crates/gents/src/turn/provider_input/tests.rs",
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
    ("crates/gents/src/inference/admission/client.rs", 1, "request context read inside Rig's CompletionModel and HTTP client; thread through a per-call provider client"),
    ("crates/gents/src/store/config_client/txn/native.rs", 1, "re-entrant write fence inside a transaction callback; the handle itself is passed explicitly"),
    ("crates/gents/src/tools/exec/ownership.rs", 1, "request context for tool calls; thread through a ToolDyn::call context argument"),
    ("crates/gents/src/plugin/approval.rs", 1, "request context for tool calls; thread through a ToolDyn::call context argument"),
    ("crates/gents/src/plugin/approval/workspace/context.rs", 1, "request context for tool calls; thread through a ToolDyn::call context argument"),
    ("crates/gents/src/tools/call/lifecycle/runtime.rs", 1, "request context for tool calls; thread through a ToolDyn::call context argument"),
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

/// Owner and library interfaces: the seams where callers decide behavior by
/// which failure occurred. They return module-owned `thiserror` enums and
/// classifiers match those enums; application code may use `anyhow`.
const OWNER_MODULES: &[&str] = &[
    "crates/gents-protocol/src",
    "crates/gents-loop/src",
    "crates/gents-migration/src",
    "crates/gents/src/store/config_client/common.rs",
    "crates/gents/src/store/config_client/error.rs",
    "crates/gents/src/store/config_client/graphql.rs",
    "crates/gents/src/store/config_client/mod.rs",
    "crates/gents/src/store/config_client/patch.rs",
    "crates/gents/src/store/config_client/retry.rs",
    "crates/gents/src/store/config_client/schema_contract.rs",
    "crates/gents/src/store/config_client/schema_install.rs",
    "crates/gents/src/store/config_client/schema_management.rs",
    "crates/gents/src/store/config_client/txn",
    "crates/gents/src/store/config_client/write_telemetry.rs",
    "crates/gents/src/store/graphql",
    "crates/gents/src/store/local/home",
    "crates/gents/src/store/identity",
    "crates/gents/src/store/schema/migration",
    "crates/gents/src/store/local/storage_backend",
    "crates/gents/src/store/local/store_key",
];

#[derive(Default)]
struct AnyhowVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for AnyhowVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

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
        if tokens_name_anyhow(tokens.begin()) {
            self.sites += 1;
        }
        visit::visit_macro(self, invocation);
    }

    fn visit_meta_list(&mut self, list: &'ast syn::MetaList) {
        let tokens = syn::buffer::TokenBuffer::new2(list.tokens.clone());
        if tokens_name_anyhow(tokens.begin()) {
            self.sites += 1;
        }
        visit::visit_meta_list(self, list);
    }
}

/// Macro and attribute tokens are not parsed, so any `anyhow` identifier in
/// them counts: `use anyhow as e;` inside a macro would otherwise hide every
/// later use behind the alias.
fn tokens_name_anyhow(mut cursor: syn::buffer::Cursor) -> bool {
    while !cursor.eof() {
        if let Some((inside, _, _, next)) = cursor.any_group() {
            if tokens_name_anyhow(inside) {
                return true;
            }
            cursor = next;
        } else if let Some((ident, next)) = cursor.ident() {
            if ident == "anyhow" {
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

#[test]
fn owner_interfaces_use_thiserror() {
    let root = repo_root();
    let missing = OWNER_MODULES
        .iter()
        .filter(|owner| {
            let path = root.join(owner);
            !path.exists() && !path.with_extension("rs").is_file()
        })
        .collect::<Vec<_>>();
    assert!(missing.is_empty(), "owner modules not found: {missing:?}");
    let violations = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter(|(relative, _)| OWNER_MODULES.iter().any(|owner| owned_by(relative, owner)))
        .filter(|(_, syntax)| anyhow_sites(syntax) > 0)
        .map(|(relative, _)| {
            format!("{relative}: names anyhow; return a module-owned thiserror enum")
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "owner interface ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn owner_interface_fence_counts_anyhow_names_outside_tests() {
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
        ("impl T for anyhow::Error {}", 1),
        (
            "#[derive(thiserror::Error)] enum E { #[error(\"anyhow\")] A }",
            0,
        ),
        ("fn f() { let anyhow = 1; }", 0),
        ("macro_rules! m { () => { use anyhow as eh; } }", 1),
        (
            "fn f() { tokio::select! { _ = g() => { use anyhow as eh; } } }",
            1,
        ),
        ("#[attr(anyhow::Error)] fn f() {}", 1),
        ("#[cfg(test)] mod tests { use anyhow::Result; }", 0),
        ("#[cfg(test)] fn f() -> anyhow::Result<()> { Ok(()) }", 0),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(anyhow_sites(&syntax), sites, "{source}");
    }
}

/// Production files that classify an error by its concrete type through a
/// downcast (`downcast*`, `is::<T>`) or `find_source`. Owner interfaces return
/// thiserror enums whose classifiers match their variants; a downcast belongs
/// only where an application (`anyhow`) or erased error reaches an owner
/// boundary, which then asks the owner enum it finds.
#[rustfmt::skip]
const DOWNCAST_CLASSIFIER_ALLOWLIST: &[(&str, usize)] = &[
    ("crates/gents-cli/src/commands/config/backend.rs", 2),
    ("crates/gents-cli/src/commands/eval/manage.rs", 1),
    ("crates/gents-cli/src/commands/grok_shim/acp/errors.rs", 2),
    ("crates/gents-cli/src/commands/grok_shim/job_runs.rs", 2),
    ("crates/gents-cli/src/commands/grok_shim/workflow_command.rs", 1),
    ("crates/gents-cli/src/commands/prepare_runtime.rs", 1),
    ("crates/gents-cli/src/http/file_inventory.rs", 1),
    ("crates/gents-client-host/src/editor_store.rs", 1),
    ("crates/gents-desktop-bridge/src/commands/chat_resolve.rs", 3),
    ("crates/gents-desktop-bridge/src/commands/session.rs", 2),
    ("crates/gents-desktop-bridge/src/host/local/windows.rs", 1),
    ("crates/gents-desktop-bridge/src/inference_setup.rs", 2),
    ("crates/gents-desktop-bridge/src/local_pairing.rs", 1),
    ("crates/gents-desktop-bridge/src/tauri_commands/managed_server/launch.rs", 4),
    ("crates/gents-desktop-bridge/src/tauri_commands/managed_server/native.rs", 1),
    ("crates/gents-desktop-bridge/src/tauri_commands/managed_server/readiness.rs", 3),
    ("crates/gents-desktop-core/src/client/core/route_manager.rs", 2),
    ("crates/gents-desktop-core/src/client/core/sync_state.rs", 1),
    ("crates/gents-desktop-core/src/client/core/writes/pending_submissions.rs", 2),
    ("crates/gents-desktop-core/src/client/mutations/chat/request.rs", 1),
    ("crates/gents-desktop-core/src/client/mutations/chat/request/receipt/delivery.rs", 1),
    ("crates/gents-desktop-core/src/error.rs", 1),
    ("crates/gents-loop/src/loop_stream/aggregate_budget.rs", 4),
    ("crates/gents-protocol/src/database_error.rs", 1),
    ("crates/gents-protocol/src/error_chain.rs", 1),
    ("crates/gents-protocol/src/graphql/transport.rs", 3),
    ("crates/gents/src/runtime/agent/daemon.rs", 1),
    ("crates/gents/src/runtime/agent/daemon/owned_work.rs", 1),
    ("crates/gents/src/runtime/agent/stream_processor.rs", 1),
    ("crates/gents/src/runtime/agent/daemon/request.rs", 1),
    ("crates/gents/src/runtime/agent/document_view/snapshot.rs", 1),
    ("crates/gents/src/runtime/agent/p2p_reconcile/session_hydration_reconcile/inventory.rs", 1),
    ("crates/gents/src/runtime/agent/reconcile.rs", 1),
    ("crates/gents/src/plugin/application_write/mod.rs", 1),
    ("crates/gents/src/tools/background/status/final_output.rs", 1),
    ("crates/gents/src/store/config_client/desired_state/plan.rs", 1),
    ("crates/gents/src/store/config_client/retry.rs", 2),
    ("crates/gents/src/store/config_client/schema_install.rs", 2),
    ("crates/gents/src/tools/builtin/defra_query/mod.rs", 1),
    ("crates/gents/src/eval/documents.rs", 3),
    ("crates/gents/src/eval/report/mod.rs", 1),
    ("crates/gents/src/eval/report/store.rs", 1),
    ("crates/gents/src/eval/runner/freeze.rs", 1),
    ("crates/gents/src/eval/runner/mod.rs", 1),
    ("crates/gents/src/file_inventory/operator.rs", 1),
    ("crates/gents/src/goal/creation.rs", 2),
    ("crates/gents/src/store/graphql.rs", 1),
    ("crates/gents/src/store/local/home.rs", 3),
    ("crates/gents/src/tools/call/hook/persistence/background_tools.rs", 1),
    ("crates/gents/src/tools/call/hook/persistence/session_message.rs", 1),
    ("crates/gents/src/store/identity/error.rs", 1),
    ("crates/gents/src/request/lifecycle.rs", 1),
    ("crates/gents/src/request/lifecycle/canonical_recovery.rs", 2),
    ("crates/gents/src/request/lifecycle/execution_lease.rs", 2),
    ("crates/gents/src/request/lifecycle/execution_renewal.rs", 1),
    ("crates/gents/src/request/lifecycle/queue/position.rs", 1),
    ("crates/gents/src/runtime/node_readiness_publisher.rs", 1),
    ("crates/gents/src/eval/optimization/driver.rs", 2),
    ("crates/gents/src/eval/optimization/job.rs", 1),
    ("crates/gents/src/eval/optimization/promote.rs", 2),
    ("crates/gents/src/tools/builtin/p2p.rs", 1),
    ("crates/gents/src/pack/resolve.rs", 1),
    ("crates/gents/src/plugin/allowed.rs", 2),
    ("crates/gents/src/plugin/approval.rs", 2),
    ("crates/gents/src/plugin/executor.rs", 2),
    ("crates/gents/src/plugin/http_calls.rs", 1),
    ("crates/gents/src/plugin/install.rs", 1),
    ("crates/gents/src/plugin/store.rs", 1),
    ("crates/gents/src/tools/background/job/driver.rs", 1),
    ("crates/gents/src/tools/background/job/driver/step.rs", 1),
    ("crates/gents/src/turn/provider_input/replay.rs", 1),
    ("crates/gents/src/inference/rendered_request/mod.rs", 2),
    ("crates/gents/src/request/admission.rs", 2),
    ("crates/gents/src/tools/builtin/schema_help/mod.rs", 2),
    ("crates/gents/src/tools/builtin/self_config/command/guidance.rs", 5),
    ("crates/gents/src/tools/builtin/self_config/command/validate.rs", 1),
    ("crates/gents/src/session/fork.rs", 1),
    ("crates/gents/src/session/output/replay_candidates.rs", 1),
    ("crates/gents/src/session/output/replay_capture.rs", 1),
    ("crates/gents/src/session/output/replay_request.rs", 1),
    ("crates/gents/src/session/request_output.rs", 1),
    ("crates/gents/src/store/local/storage_backend.rs", 9),
    ("crates/gents/src/store/local/store_key.rs", 2),
    ("crates/gents/src/request/streaming.rs", 1),
    ("crates/gents/src/tools/call/lifecycle/delivery/terminalize.rs", 1),
    ("crates/gents/src/triggers/engine/mod.rs", 2),
];

#[derive(Default)]
struct DowncastVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for DowncastVisitor {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        let test = cfg_test(&item.attrs)
            || item.attrs.iter().any(|attribute| {
                attribute
                    .path()
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "test")
            });
        if !test {
            visit::visit_item_fn(self, item);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if path_name(call).is_some_and(|name| name.rsplit("::").next() == Some("find_source")) {
            self.sites += 1;
        }
        visit::visit_expr_call(self, call);
    }

    /// Only an error receiver classifies a failure; `Any` maps, deferred
    /// write payloads and panic payloads downcast data, not errors.
    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        let method = call.method.to_string();
        if (matches!(
            method.as_str(),
            "downcast" | "downcast_ref" | "downcast_mut"
        ) || (method == "is" && call.turbofish.is_some()))
            && expression_names_error(&call.receiver)
        {
            self.sites += 1;
        }
        visit::visit_expr_method_call(self, call);
    }
}

fn downcast_sites(syntax: &syn::File) -> usize {
    let mut visitor = DowncastVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn downcast_classifiers_only_shrink() {
    let root = repo_root();
    let actual = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter_map(|(relative, syntax)| {
            let sites = downcast_sites(&syntax);
            (sites > 0).then_some((relative, sites))
        })
        .collect::<BTreeMap<_, _>>();
    let allowed = DOWNCAST_CLASSIFIER_ALLOWLIST
        .iter()
        .map(|(path, count)| (path.to_string(), *count))
        .collect::<BTreeMap<_, _>>();
    let mut violations = Vec::new();
    for path in allowed.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let found = actual.get(path).copied().unwrap_or_default();
        match allowed.get(path).copied() {
            None => violations.push(format!(
                "{path}: {found} new downcast classifier site(s); return an owner enum and match its variants"
            )),
            Some(limit) if found > limit => violations.push(format!(
                "{path}: {found} downcast classifier sites exceed the allowlisted {limit}"
            )),
            Some(limit) if found < limit => violations.push(format!(
                "{path}: {found} downcast classifier sites; lower the allowlist entry from {limit}"
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
        "downcast classifier ratchet failed:\n{}\n\ncurrent sites:\n{current}",
        violations.join("\n")
    );
}

#[test]
fn downcast_fence_counts_type_classification_outside_tests() {
    for (source, sites) in [
        ("fn f(e: &anyhow::Error) -> bool { e.downcast_ref::<E>().is_some() }", 1),
        ("fn f(e: anyhow::Error) { let _ = e.downcast::<E>(); }", 1),
        ("fn f(e: &anyhow::Error) -> bool { e.is::<E>() }", 1),
        ("fn f(e: &dyn Error) { find_source::<E>(e); gents_protocol::error_chain::find_source::<F>(e); }", 2),
        ("fn f(e: &Error) -> bool { e.is_conflict() || e.is(x) }", 0),
        ("fn f(e: &dyn Error) -> bool { find_map_source(e, |cause| cause.downcast_ref::<E>()).is_some() }", 1),
        ("fn f(m: &Map) -> Option<&T> { m.get(&id)?.downcast_ref() }", 0),
        ("fn f(payload: &dyn Any) -> bool { payload.downcast_ref::<String>().is_some() }", 0),
        ("#[cfg(test)] mod tests { fn f(e: &anyhow::Error) { e.downcast_ref::<E>(); } }", 0),
        ("#[test] fn f() { error().downcast_ref::<E>(); }", 0),
        ("#[tokio::test] async fn f() { error().downcast_ref::<E>(); }", 0),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(downcast_sites(&syntax), sites, "{source}");
    }
}

/// Production files that still decide on an error's text. Each entry is an
/// adapter-boundary classifier whose external premise its docstring names
/// (third-party or persisted text that is the only signal), or display-only
/// composition; owner facts are variants that callers match instead.
#[rustfmt::skip]
const ERROR_TEXT_CLASSIFIER_ALLOWLIST: &[(&str, usize)] = &[
    // regolith store-lock messages through the node builder
    ("crates/gents-cli/src/commands/eval/watch.rs", 1),
    // launchctl print output
    ("crates/gents-cli/src/native_service.rs", 1),
    // not yet typed: pairing closures flatten failures to strings
    ("crates/gents-desktop-bridge/src/local_pairing.rs", 1),
    // DefraDB duplicate-document rejection wording (no structured code)
    ("crates/gents-desktop-core/src/client/mutations/chat/request/receipt.rs", 2),
    // provider text through Rig's ProviderError(String) (ProviderReport)
    ("crates/gents-loop/src/error.rs", 21),
    // serde_json schema-mismatch message prefixes
    ("crates/gents-loop/src/tool.rs", 3),
    // tool errors erased by Rig's ToolCallError
    ("crates/gents-loop/src/tool_call_lifecycle/runtime.rs", 8),
    // DefraDB schema validator messages
    ("crates/gents-migration/src/engine.rs", 1),
    // DefraDB missing-collection messages
    ("crates/gents-protocol/src/graphql/response.rs", 4),
    // replicated AgentRequest failure_reason
    ("crates/gents-protocol/src/node_readiness.rs", 1),
    // DefraDB HTTP 409 commit body
    ("crates/gents/src/store/config_client/retry.rs", 3),
    // JSON-RPC node pruned-history wording
    // persisted AgentRequest failure_reason
    ("crates/gents/src/eval/runner/embedded/executor/outcome.rs", 2),
    // MCP probe transport/OS error text (metric label)
    ("crates/gents/src/tools/mcp/health.rs", 10),
    // display only: appends a next-call hint when absent
    ("crates/gents/src/tools/builtin/p2p.rs", 1),
    // DefraDB SDL parser messages (help page choice)
    ("crates/gents/src/tools/builtin/schema_help/mod.rs", 6),
    // serde `invalid type: string` message
    ("crates/gents/src/tools/builtin/self_config/command/patch.rs", 1),
    // display only: appends a next-call hint when absent
    ("crates/gents/src/tools/native/session_history.rs", 1),
    // persisted AgentRequest failure_reason
    ("crates/gents/src/observe/trace_export.rs", 6),
    // serde `unknown variant` message
    ("crates/gents/src/tools/workspace/action_plan.rs", 1),
    // git show stderr
    ("crates/gents/src/tools/workspace/adapter.rs", 2),
];

const TEXT_TESTS: &[&str] = &["contains", "starts_with", "ends_with", "eq", "ne"];

const TEXT_ADAPTERS: &[&str] = &[
    "as_str",
    "as_deref",
    "as_ref",
    "to_lowercase",
    "to_ascii_lowercase",
    "to_uppercase",
    "trim",
    "to_owned",
];

/// Methods that view an error as another error rather than one of its fields.
const ERROR_VIEWS: &[&str] = &[
    "as_ref",
    "root_cause",
    "source",
    "inner",
    "unwrap_err",
    "as_deref",
    "clone",
];

/// Identifier words that name an error or its rendering.
const ERROR_WORDS: &[&str] = &[
    "e", "err", "error", "errors", "stderr", "failure", "message", "msg", "cause",
];

fn names_error(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let words = name.split('_').collect::<Vec<_>>();
    match words.as_slice() {
        [word] => ERROR_WORDS.contains(word),
        _ => words.iter().any(|word| {
            matches!(
                *word,
                "e" | "err" | "error" | "errors" | "stderr" | "failure" | "cause"
            )
        }),
    }
}

fn expression_names_error(expression: &Expr) -> bool {
    match expression {
        Expr::Path(path) => path
            .path
            .segments
            .last()
            .is_some_and(|segment| names_error(&segment.ident.to_string())),
        // A `cause` field states why a request or event happened; a `cause`
        // binding is an error walked from a source chain.
        Expr::Field(field) => match &field.member {
            syn::Member::Named(name) => {
                let name = name.to_string();
                name != "cause" && names_error(&name)
            }
            syn::Member::Unnamed(_) => expression_names_error(&field.base),
        },
        Expr::Reference(reference) => expression_names_error(&reference.expr),
        Expr::Paren(paren) => expression_names_error(&paren.expr),
        Expr::Unary(unary) => expression_names_error(&unary.expr),
        Expr::MethodCall(call) => {
            let method = call.method.to_string();
            names_error(&method)
                || (ERROR_VIEWS.contains(&method.as_str())
                    && expression_names_error(&call.receiver))
        }
        Expr::Call(call) => {
            path_name(call).is_some_and(|name| name.ends_with("ErrorChain"))
                || call.args.iter().any(expression_names_error)
        }
        _ => false,
    }
}

fn macro_renders_error(invocation: &syn::Macro) -> bool {
    invocation.path.is_ident("format")
        && invocation
            .tokens
            .clone()
            .into_iter()
            .any(|token| match token {
                proc_macro2::TokenTree::Ident(ident) => names_error(&ident.to_string()),
                proc_macro2::TokenTree::Literal(literal) => {
                    let text = literal.to_string();
                    ["{e:", "{e}", "{err", "{error"]
                        .iter()
                        .any(|start| text.contains(start))
                }
                _ => false,
            })
}

#[derive(Default)]
struct ErrorTextVisitor {
    sites: Vec<usize>,
    renderings: Vec<String>,
}

impl ErrorTextVisitor {
    /// Text derived from an error: its `to_string()` or `format!` rendering,
    /// a local bound to one, or an identifier named as error text.
    fn error_text(&self, expression: &Expr, named: bool) -> bool {
        match expression {
            Expr::Reference(reference) => self.error_text(&reference.expr, named),
            Expr::Paren(paren) => self.error_text(&paren.expr, named),
            Expr::Unary(unary) => self.error_text(&unary.expr, named),
            Expr::Macro(invocation) => macro_renders_error(&invocation.mac),
            Expr::Path(path) => path.path.get_ident().is_some_and(|ident| {
                let ident = ident.to_string();
                self.renderings.contains(&ident) || (named && names_error(&ident))
            }),
            Expr::Field(_) => named && expression_names_error(expression),
            Expr::MethodCall(call) => {
                let method = call.method.to_string();
                if TEXT_ADAPTERS.contains(&method.as_str()) {
                    self.error_text(&call.receiver, named)
                } else {
                    method == "to_string" && expression_names_error(&call.receiver)
                }
            }
            _ => false,
        }
    }

    /// A function named for a failure reads its text parameters as error text.
    fn classifier_parameters(&mut self, signature: &syn::Signature) {
        let name = signature.ident.to_string();
        let predicate = ["is_", "classify", "looks_like"]
            .iter()
            .any(|start| name.starts_with(start));
        let classifier = predicate
            && name
                .split('_')
                .any(|word| matches!(word, "error" | "err" | "failure" | "conflict" | "rejection"));
        if !classifier {
            return;
        }
        for input in &signature.inputs {
            if let syn::FnArg::Typed(typed) = input {
                let text = matches!(typed.ty.as_ref(), syn::Type::Reference(reference)
                    if matches!(reference.elem.as_ref(), syn::Type::Path(path) if path.path.is_ident("str")));
                if let (true, syn::Pat::Ident(binding)) = (text, typed.pat.as_ref()) {
                    self.renderings.push(binding.ident.to_string());
                }
            }
        }
    }

    fn scoped(&mut self, visit: impl FnOnce(&mut Self)) {
        let depth = self.renderings.len();
        visit(self);
        self.renderings.truncate(depth);
    }
}

fn is_test_fn(attributes: &[syn::Attribute]) -> bool {
    cfg_test(attributes)
        || attributes.iter().any(|attribute| {
            attribute
                .path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
        })
}

impl<'ast> Visit<'ast> for ErrorTextVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if !is_test_fn(&item.attrs) {
            self.scoped(|visitor| {
                visitor.classifier_parameters(&item.sig);
                visit::visit_item_fn(visitor, item);
            });
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !is_test_fn(&item.attrs) {
            self.scoped(|visitor| {
                visitor.classifier_parameters(&item.sig);
                visit::visit_impl_item_fn(visitor, item);
            });
        }
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        visit::visit_local(self, local);
        if let (syn::Pat::Ident(binding), Some(init)) = (&local.pat, &local.init) {
            if self.error_text(&init.expr, true) {
                self.renderings.push(binding.ident.to_string());
            }
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if TEXT_TESTS.contains(&call.method.to_string().as_str())
            && self.error_text(&call.receiver, true)
        {
            self.sites.push(call.method.span().start().line);
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_binary(&mut self, binary: &'ast syn::ExprBinary) {
        // Comparing two observed values is identity, not classification; a
        // decision compares error text with a fixed wording.
        let wording = |expression: &Expr| match expression {
            Expr::Lit(_) | Expr::Macro(_) => true,
            Expr::Reference(reference) => matches!(reference.expr.as_ref(), Expr::Lit(_)),
            Expr::Path(path) => path.path.segments.last().is_some_and(|segment| {
                let name = segment.ident.to_string();
                name.chars()
                    .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
            }),
            _ => false,
        };
        if matches!(binary.op, syn::BinOp::Eq(_) | syn::BinOp::Ne(_))
            && ((wording(&binary.right) && self.error_text(&binary.left, true))
                || (wording(&binary.left) && self.error_text(&binary.right, true)))
        {
            self.sites.push(binary.op.span().start().line);
        }
        visit::visit_expr_binary(self, binary);
    }
}

fn error_text_sites(syntax: &syn::File) -> Vec<usize> {
    let mut visitor = ErrorTextVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn error_text_classification_only_shrinks() {
    let root = repo_root();
    let actual = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter_map(|(relative, syntax)| {
            let sites = error_text_sites(&syntax);
            (!sites.is_empty()).then_some((relative, sites))
        })
        .collect::<BTreeMap<_, _>>();
    let allowed = ERROR_TEXT_CLASSIFIER_ALLOWLIST
        .iter()
        .map(|(path, count)| (path.to_string(), *count))
        .collect::<BTreeMap<_, _>>();
    let mut violations = Vec::new();
    for path in allowed.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let lines = actual.get(path).cloned().unwrap_or_default();
        let found = lines.len();
        match allowed.get(path).copied() {
            None => violations.push(format!(
                "{path}:{lines:?}: {found} new decision(s) on error text; expose the fact as an owner enum variant and match it"
            )),
            Some(limit) if found > limit => violations.push(format!(
                "{path}:{lines:?}: {found} error-text decisions exceed the allowlisted {limit}"
            )),
            Some(limit) if found < limit => violations.push(format!(
                "{path}: {found} error-text decisions; lower the allowlist entry from {limit}"
            )),
            Some(_) => {}
        }
    }
    let current = actual
        .iter()
        .map(|(path, lines)| format!("    (\"{path}\", {}),", lines.len()))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        violations.is_empty(),
        "error text ratchet failed:\n{}\n\ncurrent sites:\n{current}",
        violations.join("\n")
    );
}

#[test]
fn error_text_fence_counts_decisions_on_error_renderings() {
    for (source, sites) in [
        (r#"fn f(e: &E) -> bool { e.to_string().contains("x") }"#, 1),
        (
            r#"fn f(e: &E) -> bool { format!("{e:#}").contains("x") }"#,
            1,
        ),
        (
            r#"fn f(e: &E) -> bool { ErrorChain(e).to_string().starts_with("x") }"#,
            1,
        ),
        (
            r#"fn f(e: &E) -> bool { let text = e.to_string(); text.contains("x") }"#,
            1,
        ),
        (
            r#"fn f(error: &E) -> bool { error.to_string().to_lowercase() == "x" }"#,
            1,
        ),
        (
            r#"fn f(message: &str) -> bool { message.contains("x") }"#,
            1,
        ),
        (
            r#"fn is_retryable_error(text: &str) -> bool { text.contains("x") }"#,
            1,
        ),
        (
            r#"fn is_failure(reason: &str) -> bool { reason == MESSAGE }"#,
            1,
        ),
        (
            r#"fn hook_failure_reason(id: &str, a: &A) -> bool { a.id == id }"#,
            0,
        ),
        (
            r#"fn f(e: &E, seen: &str) -> bool { e.to_string() != seen }"#,
            0,
        ),
        (
            r#"fn f(e: &E) -> bool { std::iter::successors(Some(e), |c| c.source()).any(|cause| cause.to_string().contains("x")) }"#,
            1,
        ),
        (r#"fn f(r: &R) -> bool { r.cause != "userCancelled" }"#, 0),
        (r#"fn f(text: &str) -> bool { text.contains("x") }"#, 0),
        (r#"fn f(e: &E) -> bool { e.path().to_string() == "." }"#, 0),
        (r#"fn f(e: &E) -> bool { matches!(e, E::Conflict) }"#, 0),
        (
            r#"fn f(a: &R, b: &R) -> bool { a.last_error != b.last_error }"#,
            0,
        ),
        (
            r#"#[cfg(test)] mod tests { fn f(e: &E) { e.to_string().contains("x"); } }"#,
            0,
        ),
        (
            r#"#[test] fn f() { error().to_string().contains("x"); }"#,
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(error_text_sites(&syntax).len(), sites, "{source}");
    }
}

/// Layers of `crates/gents/src`, bottom to top. A module may name its own
/// layer and those below it; the peer tier names none of its siblings.
/// Root-level modules outside these directories are not ranked.
const LAYERS: &[&[&str]] = &[
    &["support"],
    &["store"],
    &["documents"],
    &["inference"],
    &[
        "request", "session", "goal", "triggers", "tools", "plugin", "pack",
    ],
    &["observe"],
    &["turn"],
    &["runtime"],
    &["eval"],
];

const PEER_TIER: usize = 4;

/// Upward and peer-crossing `crate::` edges as (from, to, references). The
/// list only shrinks.
#[rustfmt::skip]
const LAYER_EDGE_ALLOWLIST: &[(&str, &str, usize)] = &[
    ("documents", "goal", 4),
    ("documents", "inference", 10),
    ("documents", "observe", 1),
    ("documents", "pack", 3),
    ("documents", "plugin", 4),
    ("documents", "runtime", 1),
    ("documents", "session", 3),
    ("documents", "tools", 26),
    ("documents", "triggers", 5),
    ("goal", "request", 33),
    ("goal", "runtime", 1),
    ("goal", "session", 13),
    ("goal", "tools", 3),
    ("goal", "triggers", 6),
    ("inference", "observe", 3),
    ("inference", "request", 3),
    ("inference", "runtime", 3),
    ("inference", "tools", 1),
    ("inference", "turn", 1),
    ("observe", "runtime", 2),
    ("pack", "plugin", 31),
    ("pack", "runtime", 1),
    ("pack", "tools", 3),
    ("plugin", "pack", 40),
    ("plugin", "runtime", 2),
    ("plugin", "session", 1),
    ("plugin", "tools", 29),
    ("plugin", "triggers", 2),
    ("request", "goal", 13),
    ("request", "runtime", 7),
    ("request", "session", 44),
    ("request", "tools", 32),
    ("request", "triggers", 32),
    ("request", "turn", 1),
    ("session", "goal", 11),
    ("session", "observe", 1),
    ("session", "request", 53),
    ("session", "tools", 36),
    ("session", "turn", 8),
    ("store", "documents", 46),
    ("store", "inference", 11),
    ("store", "runtime", 2),
    ("store", "tools", 13),
    ("store", "triggers", 5),
    ("tools", "goal", 37),
    ("tools", "observe", 4),
    ("tools", "pack", 88),
    ("tools", "plugin", 77),
    ("tools", "request", 79),
    ("tools", "runtime", 20),
    ("tools", "session", 139),
    ("tools", "turn", 1),
    ("triggers", "goal", 28),
    ("triggers", "plugin", 16),
    ("triggers", "request", 18),
    ("triggers", "runtime", 45),
    ("triggers", "session", 3),
    ("triggers", "tools", 24),
    ("turn", "runtime", 2),
];

fn layer_rank(top: &str) -> Option<usize> {
    LAYERS.iter().position(|layer| layer.contains(&top))
}

fn layer_edge_violates(from: &str, to: &str) -> bool {
    match (layer_rank(from), layer_rank(to)) {
        (Some(lower), Some(higher)) if lower < higher => true,
        (Some(PEER_TIER), Some(PEER_TIER)) => from != to,
        _ => false,
    }
}

fn source_top(relative: &str) -> Option<&str> {
    let rest = relative.strip_prefix("crates/gents/src/")?;
    let first = rest.split('/').next()?;
    Some(first.strip_suffix(".rs").unwrap_or(first))
}

#[derive(Default)]
struct CrateTopVisitor {
    targets: Vec<String>,
}

impl CrateTopVisitor {
    fn use_targets(&mut self, tree: &syn::UseTree, under_crate: bool) {
        match tree {
            syn::UseTree::Path(path) if under_crate => self.targets.push(path.ident.to_string()),
            syn::UseTree::Path(path) if path.ident == "crate" => self.use_targets(&path.tree, true),
            syn::UseTree::Name(name) if under_crate => self.targets.push(name.ident.to_string()),
            syn::UseTree::Rename(rename) if under_crate => {
                self.targets.push(rename.ident.to_string())
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.use_targets(item, under_crate);
                }
            }
            _ => {}
        }
    }

    fn macro_targets(&mut self, stream: proc_macro2::TokenStream) {
        let tokens = stream.into_iter().collect::<Vec<_>>();
        for (index, token) in tokens.iter().enumerate() {
            match token {
                proc_macro2::TokenTree::Group(group) => self.macro_targets(group.stream()),
                proc_macro2::TokenTree::Ident(ident) if ident == "crate" => {
                    let colons = tokens.get(index + 1..index + 3).is_some_and(|pair| {
                        pair.iter().all(|token| {
                            matches!(token, proc_macro2::TokenTree::Punct(punct) if punct.as_char() == ':')
                        })
                    });
                    if let (true, Some(proc_macro2::TokenTree::Ident(target))) =
                        (colons, tokens.get(index + 3))
                    {
                        self.targets.push(target.to_string());
                    }
                }
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for CrateTopVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.use_targets(&item.tree, false);
        visit::visit_item_use(self, item);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path.segments.len() > 1 && path.segments[0].ident == "crate" {
            self.targets.push(path.segments[1].ident.to_string());
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        self.macro_targets(invocation.tokens.clone());
        visit::visit_macro(self, invocation);
    }
}

fn layer_edges(from: &str, syntax: &syn::File) -> BTreeMap<(String, String), usize> {
    let mut visitor = CrateTopVisitor::default();
    visitor.visit_file(syntax);
    let mut edges = BTreeMap::new();
    for to in visitor.targets {
        if layer_edge_violates(from, &to) {
            *edges.entry((from.to_string(), to)).or_default() += 1;
        }
    }
    edges
}

#[test]
fn layer_edges_only_shrink() {
    let root = repo_root();
    let mut actual = BTreeMap::<(String, String), usize>::new();
    for (relative, syntax) in parse_production(&root, production_sources(&root)) {
        let Some(from) = source_top(&relative) else {
            continue;
        };
        for (edge, count) in layer_edges(from, &syntax) {
            *actual.entry(edge).or_default() += count;
        }
    }
    let allowed = LAYER_EDGE_ALLOWLIST
        .iter()
        .map(|(from, to, count)| ((from.to_string(), to.to_string()), *count))
        .collect::<BTreeMap<_, _>>();
    let mut violations = Vec::new();
    for edge in allowed.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let (from, to) = edge;
        let found = actual.get(edge).copied().unwrap_or_default();
        match allowed.get(edge).copied() {
            None => violations.push(format!(
                "{from} -> {to}: {found} new upward or peer reference(s); move the shared item down to a layer both may name"
            )),
            Some(limit) if found > limit => violations.push(format!(
                "{from} -> {to}: {found} references exceed the allowlisted {limit}"
            )),
            Some(limit) if found < limit => violations.push(format!(
                "{from} -> {to}: {found} references; lower the allowlist entry from {limit}"
            )),
            Some(_) => {}
        }
    }
    let current = actual
        .iter()
        .map(|((from, to), count)| format!("    (\"{from}\", \"{to}\", {count}),"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        violations.is_empty(),
        "layer edge ratchet failed:\n{}\n\ncurrent edges:\n{current}",
        violations.join("\n")
    );
}

#[test]
fn layer_edges_count_upward_and_peer_references_outside_tests() {
    let syntax = syn::parse_file(
        r#"
        use crate::{inference::Model, store::Key, tools::{a, b}};
        use crate::session as chat;
        fn f() -> crate::eval::Score { crate::request::admit(); crate::store::read(); }
        fn g() { tracing::info!(target: "x", "{}", crate::goal::NAME); }
        #[cfg(test)]
        mod tests { use crate::eval::Case; }
        "#,
    )
    .expect("parse snippet");
    let edges = layer_edges("request", &syntax)
        .into_iter()
        .map(|((_, to), count)| (to, count))
        .collect::<Vec<_>>();
    assert_eq!(
        edges,
        [
            ("eval".to_string(), 1),
            ("goal".to_string(), 1),
            ("session".to_string(), 1),
            ("tools".to_string(), 1),
        ]
    );
    assert!(!layer_edge_violates("eval", "store"));
    assert!(!layer_edge_violates("request", "agent"));
}
