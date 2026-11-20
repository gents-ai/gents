use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
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

/// Whether an item compiles only into tests: a `#[cfg(..)]` that requires
/// `test`, the `test-fixtures` feature, the `test-home-guard` feature or the
/// `native-e2e` feature, none of which reaches a shipped build
/// (`gents_test_fixtures_stay_dev_only`, `service_isolation_stays_test_only`,
/// `native_e2e_stays_test_only`).
fn cfg_test(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Meta>()
                .is_ok_and(|predicate| predicate_requires_test(&predicate))
    })
}

fn predicate_requires_test(predicate: &syn::Meta) -> bool {
    let operands = |list: &syn::MetaList| {
        list.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        )
        .map(|operands| operands.into_iter().collect::<Vec<_>>())
        .unwrap_or_default()
    };
    match predicate {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::NameValue(pair) => {
            pair.path.is_ident("feature")
                && matches!(&pair.value, Expr::Lit(syn::ExprLit { lit: syn::Lit::Str(name), .. }) if TEST_FEATURES.contains(&name.value().as_str()) || name.value() == NATIVE_E2E)
        }
        syn::Meta::List(list) if list.path.is_ident("all") => {
            operands(list).iter().any(predicate_requires_test)
        }
        syn::Meta::List(list) if list.path.is_ident("any") => {
            let operands = operands(list);
            !operands.is_empty() && operands.iter().all(predicate_requires_test)
        }
        syn::Meta::List(_) => false,
    }
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

const GENTS_TEST_FIXTURES: &str = "test-fixtures";

/// `cfg_test` treats `test-fixtures` items as test code, which holds only
/// while no production build can enable the feature.
fn test_fixtures_leaks(manifest: &toml::Table, inherited: &toml::Table) -> Vec<String> {
    let mut tables = vec![(
        "workspace.dependencies",
        manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies")),
    )];
    for kind in ["dependencies", "build-dependencies"] {
        tables.push((kind, manifest.get(kind)));
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            tables.extend(targets.values().map(|target| (kind, target.get(kind))));
        }
    }
    let mut leaks = Vec::new();
    for (kind, table) in tables {
        for (key, specification) in table.and_then(toml::Value::as_table).into_iter().flatten() {
            if package_name(key, specification, inherited) == "gents"
                && specification
                    .get("features")
                    .and_then(toml::Value::as_array)
                    .is_some_and(|features| {
                        features
                            .iter()
                            .any(|feature| feature.as_str() == Some(GENTS_TEST_FIXTURES))
                    })
            {
                leaks.push(format!("[{kind}] {key}"));
            }
        }
    }
    let own = manifest
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml::Value::as_str)
        == Some("gents");
    for (feature, enabled) in manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
    {
        if enabled
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .any(|entry| {
                matches!(entry, "gents/test-fixtures" | "gents?/test-fixtures")
                    || (own && entry == GENTS_TEST_FIXTURES)
            })
        {
            leaks.push(format!("[features] {feature}"));
        }
    }
    leaks
}

#[test]
fn gents_test_fixtures_stay_dev_only() {
    let root = repo_root();
    let workspace = toml_file(&root.join("Cargo.toml"));
    let inherited = workspace["workspace"]
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let mut leaks = test_fixtures_leaks(&workspace, &inherited)
        .into_iter()
        .map(|site| format!("Cargo.toml {site}"))
        .collect::<Vec<_>>();
    for member in workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members")
    {
        let member = member.as_str().expect("member path");
        let manifest = toml_file(&root.join(member).join("Cargo.toml"));
        leaks.extend(
            test_fixtures_leaks(&manifest, &inherited)
                .into_iter()
                .map(|site| format!("{member}/Cargo.toml {site}")),
        );
    }
    assert!(
        leaks.is_empty(),
        "the gents `test-fixtures` feature is enabled only from [dev-dependencies]:\n{}",
        leaks.join("\n")
    );
}

#[test]
fn test_fixtures_fence_flags_production_and_default_enablement() {
    let leaks = |manifest: &str| {
        let manifest: toml::Table = manifest.parse().expect("parse manifest");
        let inherited = manifest
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies"))
            .and_then(toml::Value::as_table)
            .cloned()
            .unwrap_or_default();
        test_fixtures_leaks(&manifest, &inherited)
    };
    for (manifest, expected) in [
        (
            r#"[dependencies]
            gents = { path = "../gents", features = ["test-fixtures"] }"#,
            "[dependencies] gents",
        ),
        (
            r#"[build-dependencies]
            core = { package = "gents", path = "../gents", features = ["test-fixtures"] }"#,
            "[build-dependencies] core",
        ),
        (
            r#"[target.'cfg(unix)'.dependencies]
            gents = { path = "../gents", features = ["test-fixtures"] }"#,
            "[dependencies] gents",
        ),
        (
            r#"[workspace.dependencies]
            gents = { path = "crates/gents", features = ["test-fixtures"] }"#,
            "[workspace.dependencies] gents",
        ),
        (
            r#"[features]
            fixtures = ["gents/test-fixtures"]"#,
            "[features] fixtures",
        ),
        (
            r#"[package]
            name = "gents"
            [features]
            default = ["test-fixtures"]
            test-fixtures = []"#,
            "[features] default",
        ),
    ] {
        assert_eq!(leaks(manifest), [expected], "{manifest}");
    }
    for manifest in [
        r#"[dev-dependencies]
        gents = { path = "../gents", features = ["test-fixtures"] }"#,
        r#"[dependencies]
        gents = { path = "../gents", features = ["live-e2e"] }
        other = { path = "../other", features = ["test-fixtures"] }"#,
        r#"[package]
        name = "gents-desktop-core"
        [features]
        default = ["test-fixtures"]
        test-fixtures = []"#,
        r#"[package]
        name = "gents"
        [features]
        test-home-guard = []
        test-fixtures = ["test-home-guard"]"#,
    ] {
        assert!(leaks(manifest).is_empty(), "{manifest}");
    }
}

/// The feature end-to-end launchers pass on the command line to build the
/// desktop, bridge and CLI with their native test harness hooks; release
/// builds never pass it (`scripts/check-native-e2e-capabilities.sh` keeps its
/// capability out of the shipped app).
const NATIVE_E2E: &str = "native-e2e";

/// `cfg_test` treats `native-e2e` items as test code, which holds only while
/// no manifest enables the feature outside `[dev-dependencies]` except a
/// crate's own `native-e2e` feature forwarding it to a dependency.
fn native_e2e_leaks(manifest: &toml::Table) -> Vec<String> {
    let mut tables = Vec::new();
    for kind in ["dependencies", "build-dependencies"] {
        tables.push((kind, manifest.get(kind)));
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            tables.extend(targets.values().map(|target| (kind, target.get(kind))));
        }
    }
    let mut leaks = Vec::new();
    for (kind, table) in tables {
        for (key, specification) in table.and_then(toml::Value::as_table).into_iter().flatten() {
            if specification
                .get("features")
                .and_then(toml::Value::as_array)
                .is_some_and(|features| {
                    features
                        .iter()
                        .any(|feature| feature.as_str() == Some(NATIVE_E2E))
                })
            {
                leaks.push(format!("[{kind}] {key}"));
            }
        }
    }
    for (feature, enabled) in manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .into_iter()
        .flatten()
    {
        if feature != NATIVE_E2E
            && enabled
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .any(|entry| entry.rsplit('/').next() == Some(NATIVE_E2E))
        {
            leaks.push(format!("[features] {feature}"));
        }
    }
    leaks
}

#[test]
fn native_e2e_stays_test_only() {
    let root = repo_root();
    let workspace = toml_file(&root.join("Cargo.toml"));
    let mut leaks = Vec::new();
    for member in workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members")
    {
        let member = member.as_str().expect("member path");
        let manifest = toml_file(&root.join(member).join("Cargo.toml"));
        leaks.extend(
            native_e2e_leaks(&manifest)
                .into_iter()
                .map(|site| format!("{member}/Cargo.toml {site}")),
        );
    }
    assert!(
        leaks.is_empty(),
        "the `native-e2e` feature is enabled only from the command line, [dev-dependencies] or a crate's own `native-e2e` feature:\n{}",
        leaks.join("\n")
    );
}

#[test]
fn native_e2e_fence_flags_production_and_default_enablement() {
    let leaks = |manifest: &str| native_e2e_leaks(&manifest.parse().expect("parse manifest"));
    for (manifest, expected) in [
        (
            r#"[dependencies]
            bridge = { path = "../bridge", features = ["native-e2e"] }"#,
            "[dependencies] bridge",
        ),
        (
            r#"[target.'cfg(unix)'.dependencies]
            bridge = { path = "../bridge", features = ["native-e2e"] }"#,
            "[dependencies] bridge",
        ),
        (
            r#"[features]
            default = ["native-e2e"]
            native-e2e = []"#,
            "[features] default",
        ),
        (
            r#"[features]
            desktop = ["bridge?/native-e2e"]"#,
            "[features] desktop",
        ),
    ] {
        assert_eq!(leaks(manifest), [expected], "{manifest}");
    }
    for manifest in [
        r#"[dev-dependencies]
        bridge = { path = "../bridge", features = ["native-e2e"] }"#,
        r#"[features]
        native-e2e = ["tauri-host", "bridge/native-e2e"]"#,
        r#"[dependencies]
        bridge = { path = "../bridge", features = ["native-e2e-ui"] }"#,
    ] {
        assert!(leaks(manifest).is_empty(), "{manifest}");
    }
}

/// The feature that cuts a test build off the machine's service manager
/// (`gents_service::native_service::isolation`). The per-user service label
/// is not per `HOME`, so a `gents` a test spawns with a temporary `HOME`
/// otherwise addresses the user's own node: on 2026-11-13 test binaries
/// stopped the maintainer's nodes through it.
const SERVICE_ISOLATION: (&str, &str) = ("gents-service", "test-home-guard");

/// The features only test builds enable: `cfg_test` treats what they gate as
/// test code, and `Workspace::production` turns them on only where a
/// compiled member's dependency entry or enabled feature does.
const TEST_FEATURES: [&str; 2] = [GENTS_TEST_FIXTURES, SERVICE_ISOLATION.1];

struct Workspace {
    inherited: toml::Table,
    members: BTreeMap<String, (String, toml::Table)>,
}

impl Workspace {
    fn load(root: &Path) -> Self {
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
            .map(|member| {
                let member = member.as_str().expect("member path").to_owned();
                let manifest = toml_file(&root.join(&member).join("Cargo.toml"));
                let name = manifest["package"]["name"]
                    .as_str()
                    .expect("package name")
                    .to_owned();
                (name, (member, manifest))
            })
            .collect();
        Self { inherited, members }
    }

    /// Every dependency entry of `manifest` in the tables of `kind`, with the
    /// package it names, the features it enables (inherited ones included)
    /// and whether it is optional.
    fn entries(
        &self,
        manifest: &toml::Table,
        kind: &str,
    ) -> Vec<(String, String, Vec<String>, bool)> {
        let mut tables = vec![manifest.get(kind)];
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            tables.extend(targets.values().map(|target| target.get(kind)));
        }
        if kind == "workspace.dependencies" {
            tables = vec![manifest
                .get("workspace")
                .and_then(|workspace| workspace.get("dependencies"))];
        }
        let features = |value: Option<&toml::Value>| {
            value
                .and_then(|value| value.get("features"))
                .and_then(toml::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        tables
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_table)
            .flat_map(|table| table.iter())
            .map(|(key, specification)| {
                let mut enabled = features(Some(specification));
                if specification
                    .get("workspace")
                    .and_then(toml::Value::as_bool)
                    == Some(true)
                {
                    enabled.extend(features(self.inherited.get(key)));
                }
                (
                    key.clone(),
                    package_name(key, specification, &self.inherited),
                    enabled,
                    specification.get("optional").and_then(toml::Value::as_bool) == Some(true),
                )
            })
            .collect()
    }

    /// The `[dependencies]` and `[build-dependencies]` entries of `manifest`,
    /// target-specific ones included: what a build of the member compiles.
    fn build_entries(&self, manifest: &toml::Table) -> Vec<(String, String, Vec<String>, bool)> {
        ["dependencies", "build-dependencies"]
            .into_iter()
            .flat_map(|kind| self.entries(manifest, kind))
            .collect()
    }

    /// The features of `manifest` as Cargo defines them: each `[features]`
    /// key, and the implicit feature `x = ["dep:x"]` of every optional
    /// dependency `x` that no `dep:x` entry names.
    fn features(&self, manifest: &toml::Table) -> BTreeMap<String, Vec<String>> {
        let mut features = manifest
            .get("features")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flatten()
            .map(|(name, entries)| {
                let entries = entries
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                (name.clone(), entries)
            })
            .collect::<BTreeMap<_, _>>();
        let explicit = features
            .values()
            .flatten()
            .filter_map(|entry| entry.strip_prefix("dep:"))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        for (key, .., optional) in self.build_entries(manifest) {
            if optional && !explicit.contains(&key) {
                features
                    .entry(key.clone())
                    .or_insert_with(|| vec![format!("dep:{key}")]);
            }
        }
        features
    }

    /// The members a build starting from `start` compiles, each with the path
    /// that reaches it, and the (package, feature) pairs it enables. A
    /// compiled member compiles every entry of `build_entries` that is not
    /// optional, with the features the entry names; an enabled feature turns
    /// on what it lists: another feature of the member (`f`), an optional
    /// dependency (`dep:x`), or a dependency with one of its features (`x/f`,
    /// and `x?/f`, counted as turning `x` on). With `every_feature`, every
    /// feature of a compiled member except the test features counts as
    /// enabled.
    fn resolve(
        &self,
        start: Vec<(String, Option<String>)>,
        every_feature: bool,
    ) -> (BTreeMap<String, String>, BTreeSet<(String, String)>) {
        let mut pending = start
            .into_iter()
            .map(|(package, feature)| (package, feature, String::new()))
            .collect::<Vec<_>>();
        let compile = |pending: &mut Vec<_>, parent: &str, target: &str, features: Vec<String>| {
            pending.push((target.to_owned(), None, parent.to_owned()));
            pending.extend(
                features
                    .into_iter()
                    .map(|feature| (target.to_owned(), Some(feature), parent.to_owned())),
            );
        };
        let mut compiled = BTreeMap::new();
        let mut enabled = BTreeSet::new();
        while let Some((package, feature, parent)) = pending.pop() {
            let newly_enabled = feature
                .as_ref()
                .is_some_and(|feature| enabled.insert((package.clone(), feature.clone())));
            let fresh = !compiled.contains_key(&package);
            let Some((_, manifest)) = self
                .members
                .get(&package)
                .filter(|_| fresh || newly_enabled)
            else {
                continue;
            };
            let step = |label: &str| {
                if parent.is_empty() {
                    label.to_owned()
                } else {
                    format!("{parent} > {label}")
                }
            };
            let features = self.features(manifest);
            let dependencies = self.build_entries(manifest);
            if fresh {
                let path = step(&package);
                if every_feature {
                    for name in features.keys() {
                        if !TEST_FEATURES.contains(&name.as_str()) {
                            pending.push((package.clone(), Some(name.clone()), path.clone()));
                        }
                    }
                }
                for (_, target, with, optional) in &dependencies {
                    if !optional {
                        compile(&mut pending, &path, target, with.clone());
                    }
                }
                compiled.insert(package.clone(), path);
            }
            let Some(feature) = feature.filter(|_| newly_enabled) else {
                continue;
            };
            let path = step(&format!("{package}/{feature}"));
            for entry in features.get(&feature).into_iter().flatten() {
                let (key, sub) = match entry.split_once('/') {
                    Some((key, sub)) => (key.trim_end_matches('?'), Some(sub.to_owned())),
                    None => match entry.strip_prefix("dep:") {
                        Some(key) => (key, None),
                        None => {
                            pending.push((package.clone(), Some(entry.clone()), path.clone()));
                            continue;
                        }
                    },
                };
                for (_, target, with, _) in dependencies.iter().filter(|(name, ..)| name == key) {
                    let with = with.iter().cloned().chain(sub.clone()).collect();
                    compile(&mut pending, &path, target, with);
                }
            }
        }
        (compiled, enabled)
    }

    /// Whether a build of `package` with `features` enables the service
    /// isolation (`resolve`).
    fn isolates(&self, package: &str, features: &[String]) -> bool {
        let start = std::iter::once(None)
            .chain(features.iter().cloned().map(Some))
            .map(|feature| (package.to_owned(), feature))
            .collect();
        self.resolve(start, false).1.contains(&(
            SERVICE_ISOLATION.0.to_owned(),
            SERVICE_ISOLATION.1.to_owned(),
        ))
    }

    /// Workspace members `package` builds against, transitively.
    fn reaches(&self, package: &str, target: &str) -> bool {
        let mut pending = vec![package.to_owned()];
        let mut seen = BTreeSet::new();
        while let Some(next) = pending.pop() {
            if next == target {
                return true;
            }
            if !seen.insert(next.clone()) {
                continue;
            }
            if let Some((_, manifest)) = self.members.get(&next) {
                pending.extend(dependency_names(manifest, &self.inherited));
            }
        }
        false
    }

    /// Members a production build compiles, each with the path that reaches
    /// it. A production build starts at a member no other member names (an
    /// application, a binary, a plugin; naming itself, as a `[dev-dependencies]`
    /// entry enabling its own test features does, does not count) and enables
    /// every feature of what it compiles except the test features (`resolve`).
    fn production(&self) -> BTreeMap<String, String> {
        let named = self
            .members
            .iter()
            .flat_map(|(name, (_, manifest))| {
                ["dependencies", "build-dependencies", "dev-dependencies"]
                    .into_iter()
                    .flat_map(|kind| self.entries(manifest, kind))
                    .map(|(_, package, ..)| package)
                    .filter(move |package| package != name)
            })
            .collect::<BTreeSet<_>>();
        let tops = self
            .members
            .keys()
            .filter(|name| !named.contains(*name))
            .map(|name| (name.clone(), None))
            .collect();
        self.resolve(tops, true).0
    }

    /// Members no production build compiles (`production`).
    fn test_only(&self) -> BTreeSet<String> {
        let production = self.production();
        self.members
            .keys()
            .filter(|name| !production.contains_key(*name))
            .cloned()
            .collect()
    }
}

/// Sites that would enable the service isolation in a production build: a
/// normal, build or workspace dependency enabling it, or a feature other than
/// a test feature (`test-home-guard`, `test-fixtures`) that does.
fn service_isolation_leaks(workspace: &Workspace) -> Vec<String> {
    let mut leaks = Vec::new();
    for (name, (member, manifest)) in &workspace.members {
        for kind in [
            "dependencies",
            "build-dependencies",
            "workspace.dependencies",
        ] {
            for (key, package, features, _) in workspace.entries(manifest, kind) {
                if workspace.isolates(&package, &features) {
                    leaks.push(format!("{member}/Cargo.toml [{kind}] {key}"));
                }
            }
        }
        for feature in manifest
            .get("features")
            .and_then(toml::Value::as_table)
            .into_iter()
            .flat_map(|table| table.keys())
        {
            let test_feature = TEST_FEATURES.contains(&feature.as_str());
            if !test_feature && workspace.isolates(name, std::slice::from_ref(feature)) {
                leaks.push(format!("{member}/Cargo.toml [features] {feature}"));
            }
        }
    }
    leaks
}

#[test]
fn service_isolation_stays_test_only() {
    let workspace = Workspace::load(&repo_root());
    assert!(
        workspace.isolates("gents-service", &["test-home-guard".to_owned()]),
        "the service isolation feature is gents-service's test-home-guard"
    );
    let leaks = service_isolation_leaks(&workspace);
    assert!(
        leaks.is_empty(),
        "a production build would never reach the service manager; enable gents-service's `test-home-guard` only from [dev-dependencies] or a test feature:\n{}",
        leaks.join("\n")
    );
}

/// Every package whose tests can reach the service manager, by building
/// against gents-service or by spawning the `gents` binary, builds it with
/// the isolation through its dev-dependencies, so that the `gents` its tests
/// spawn is cut off from the machine's service manager too.
#[test]
fn tests_reaching_the_service_manager_are_isolated() {
    let root = repo_root();
    let workspace = Workspace::load(&root);
    let mut unisolated = Vec::new();
    for (name, (member, manifest)) in &workspace.members {
        let mut sources = Vec::new();
        for directory in ["src", "tests", "benches", "examples"] {
            let directory = root.join(member).join(directory);
            if directory.is_dir() {
                all_rust_sources(&directory, &mut sources);
            }
        }
        let spawns_gents = sources.iter().any(|path| {
            std::fs::read_to_string(path).is_ok_and(|text| text.contains("CARGO_BIN_EXE_gents\""))
        });
        if !spawns_gents && !workspace.reaches(name, SERVICE_ISOLATION.0) {
            continue;
        }
        let isolated = workspace
            .entries(manifest, "dev-dependencies")
            .iter()
            .any(|(_, package, features, _)| workspace.isolates(package, features));
        if !isolated {
            unisolated.push(format!(
                "{member}: its tests reach the service manager; add `{} = {{ workspace = true, features = [\"{}\"] }}` (or a test feature enabling it) to its [dev-dependencies]",
                SERVICE_ISOLATION.0, SERVICE_ISOLATION.1
            ));
        }
    }
    assert!(unisolated.is_empty(), "{}", unisolated.join("\n"));
}

fn snippet_workspace(manifests: &[(&str, &str)]) -> Workspace {
    Workspace {
        inherited: toml::Table::new(),
        members: manifests
            .iter()
            .map(|(name, manifest)| {
                let manifest: toml::Table = format!("[package]\nname = \"{name}\"\n{manifest}")
                    .parse()
                    .expect("parse manifest");
                ((*name).to_owned(), ((*name).to_owned(), manifest))
            })
            .collect(),
    }
}

#[test]
fn service_isolation_fence_flags_production_enablement() {
    let service = ("gents-service", "[features]\ntest-home-guard = []");
    let leaking = snippet_workspace(&[
        service,
        (
            "host",
            "[dependencies]\ngents-service = { path = \"x\" }\n[features]\nfixtures = [\"gents-service/test-home-guard\"]\ntest-fixtures = [\"gents-service/test-home-guard\"]",
        ),
        (
            "cli",
            "[dependencies]\nhost = { path = \"h\", features = [\"test-fixtures\"] }",
        ),
    ]);
    assert_eq!(
        service_isolation_leaks(&leaking),
        [
            "cli/Cargo.toml [dependencies] host",
            "host/Cargo.toml [features] fixtures"
        ]
    );
    let fenced = snippet_workspace(&[
        service,
        (
            "host",
            "[dependencies]\ngents-service = { path = \"x\" }\n[features]\ntest-fixtures = [\"gents-service/test-home-guard\"]",
        ),
        (
            "cli",
            "[dependencies]\nhost = { path = \"h\" }\n[dev-dependencies]\nhost = { path = \"h\", features = [\"test-fixtures\"] }",
        ),
    ]);
    assert!(service_isolation_leaks(&fenced).is_empty());
    assert!(fenced.isolates("host", &["test-fixtures".to_owned()]));
    assert!(fenced.reaches("cli", "gents-service"));
}

/// The members no production build compiles (`Workspace::test_only`), whose
/// sources `runtime_environment_reads_only_shrink` skips. Pinned so that one
/// becoming reachable from a production build, or a new one becoming
/// test-only, is a visible change.
const TEST_ONLY_CRATES: &[&str] = &[
    "gents-callback-fixture-create-workspace",
    "gents-lean-contract",
    "gents-lean-vocab",
    "gents-workspace-hack",
];

#[test]
fn test_only_crates_stay_off_production_builds() {
    let workspace = Workspace::load(&repo_root());
    let production = workspace.production();
    let mut violations = TEST_ONLY_CRATES
        .iter()
        .filter_map(|name| match production.get(*name) {
            Some(path) => Some(format!(
                "{name} is compiled into a production build ({path}); name it only from [dev-dependencies] or a test feature"
            )),
            None if !workspace.members.contains_key(*name) => Some(format!(
                "{name} is not a workspace member; remove it from TEST_ONLY_CRATES"
            )),
            None => None,
        })
        .collect::<Vec<_>>();
    violations.extend(
        workspace
            .test_only()
            .into_iter()
            .filter(|name| !TEST_ONLY_CRATES.contains(&name.as_str()))
            .map(|name| {
                format!("{name}: no production build compiles it; name it in TEST_ONLY_CRATES")
            }),
    );
    assert!(
        violations.is_empty(),
        "test-only crates ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn test_only_fence_follows_production_dependencies_and_features() {
    const APP: &str = "[dependencies]\ncore = { path = \"c\" }\n[dev-dependencies]\ncore = { path = \"c\", features = [\"test-fixtures\"] }";
    const CORE: &str = "[dependencies]\ndecoder = { path = \"d\", optional = true }\n[features]\ndefault = []\ntest-fixtures = [\"dep:decoder\"]";
    let test_only = |app: &str, core: &str| {
        snippet_workspace(&[
            ("app", app),
            ("core", core),
            (
                "decoder",
                "[dependencies]\nhelper = { path = \"h\" }\n[dev-dependencies]\nfixture = { path = \"f\" }",
            ),
            ("helper", ""),
            ("fixture", ""),
        ])
        .test_only()
    };
    for app in [APP.to_owned(), format!("{APP}\napp = {{ path = \".\" }}")] {
        assert_eq!(
            test_only(&app, CORE),
            BTreeSet::from(["decoder", "fixture", "helper"].map(str::to_owned)),
            "{app}"
        );
    }
    let implicit = CORE.replace("dep:decoder", "decoder");
    for (app, core) in [
        (
            APP.to_owned(),
            format!("{CORE}\nlive = [\"test-fixtures\"]"),
        ),
        (APP.to_owned(), format!("{CORE}\nlive = [\"dep:decoder\"]")),
        (APP.to_owned(), implicit.clone()),
        (
            APP.replacen(
                "{ path = \"c\" }",
                "{ path = \"c\", features = [\"decoder\"] }",
                1,
            ),
            implicit,
        ),
        (APP.to_owned(), format!("{CORE}\nlive = [\"decoder/x\"]")),
        (
            APP.to_owned(),
            format!(
                "{CORE}\n[target.'cfg(unix)'.build-dependencies]\ndecoder = {{ path = \"d\" }}"
            ),
        ),
        (APP.to_owned(), CORE.replace(", optional = true", "")),
        (
            APP.replacen(
                "{ path = \"c\" }",
                "{ path = \"c\", features = [\"test-fixtures\"] }",
                1,
            ),
            CORE.to_owned(),
        ),
        (
            format!("{APP}\n[features]\nx = [\"core/test-fixtures\"]"),
            CORE.to_owned(),
        ),
    ] {
        assert_eq!(
            test_only(&app, &core),
            BTreeSet::from(["fixture".to_owned()]),
            "{app}\n{core}"
        );
    }
}

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
        ("fn f(n: &N) { lookup_oauth_credential(n, \"did\", \"p\"); }", 1, 0),
        ("fn f(a: &A) { gents::inference::auth::credential::oauth_credential_query(\"d\", \"p\"); }", 1, 0),
        ("async fn load_oauth_credential() {}", 1, 0),
        ("fn f(v: &V) { v.has_enabled_oauth_credential(\"p\"); }", 1, 0),
        (r#"fn f(d: &str) -> String { format!("query {{ OAuthCredential(filter: {{ node_did: {{ _eq: \"{d}\" }} }}) {{ _docID }} }}") }"#, 1, 0),
        ("fn f(a: &A) { list_oauth_credentials_on(a, \"d\"); }", 0, 1),
        ("fn f(v: &V) { v.oauth_credentials.values().find(|r| r.enabled); }", 0, 1),
        ("fn f(n: &N) { lookup_oauth_credential_by_doc_id(n, \"id\"); load_oauth_credential_for_discovery(); }", 0, 1),
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
    "crates/gents-loop/src/rig_compat",
    "crates/gents-loop/src/provider_input",
    "crates/gents-loop/src/loop_stream",
    "crates/gents-inference/src/llm/rig_compat.rs",
    "crates/gents/src/turn/provider_input",
    "crates/gents/src/runtime/agent/loop_stream",
];

/// Files outside the owner modules that still name `rig`. A layer over the
/// provider exchange implements `rig_compat::http::Transport`, and a model
/// this workspace implements itself `rig_compat::model::Model`; `rig_compat`
/// lifts both into Rig's traits. This list stays empty.
const RIG_PATH_ALLOWLIST: &[&str] = &[];

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

#[test]
fn item_cfg_test_excludes_items_whose_cfg_requires_test() {
    let syntax = syn::parse_file(
        r#"
        #[cfg(test)] fn a() {}
        #[cfg(any(test, feature = "test-fixtures"))] fn b() {}
        #[cfg(all(test, unix))] mod c {}
        #[cfg(all(unix, all(target_os = "macos", test)))] fn d() {}
        #[cfg(unix)] fn e() {}
        #[cfg(any(test, unix))] fn f() {}
        #[cfg(not(test))] fn g() {}
        #[cfg(all(unix, not(test)))] fn h() {}
        #[cfg(feature = "test-fixtures")] fn i() {}
        #[cfg(all(feature = "test-fixtures", unix))] fn j() {}
        #[cfg(any(feature = "test-fixtures", feature = "other"))] fn k() {}
        #[cfg(any(test, feature = "test-home-guard"))] fn l() {}
        fn m() {}
        #[cfg(all(feature = "native-e2e", debug_assertions))] fn n() {}
        #[cfg(any(feature = "native-e2e", feature = "other"))] fn o() {}
        "#,
    )
    .expect("parse snippet");
    let excluded = syntax.items.iter().map(item_cfg_test).collect::<Vec<_>>();
    assert_eq!(
        excluded,
        [
            true, true, true, true, false, false, false, false, true, true, false, true, false,
            true, false
        ]
    );
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
/// A third-party parameter typed as std's map takes a converted value
/// (`into_iter().collect()`, `.into()`), so no file names the std type.
#[test]
fn hash_maps_use_rapidhash() {
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
            (std_hash_sites(&syntax) > 0).then(|| {
                format!(
                    "{}: names a std hash map, set or hasher; use rapidhash",
                    path.strip_prefix(&root)
                        .expect("source under repo root")
                        .display()
                )
            })
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "rapidhash rule failed:\n{}",
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

/// Task-local statics that hold no request context, by file and name. The
/// rule is about what a request carries, so these are not counted, provided
/// the static's own docstring says what it holds instead.
const AMBIENT_NOT_REQUEST_CONTEXT: &[(&str, &str)] = &[
    ("crates/gents-cli/src/request_helpers.rs", "CAPTURED_REPORT"),
    (
        "crates/gents-store/src/config_client/txn/native.rs",
        "ACTIVE_EMBEDDED_TRANSACTION",
    ),
];

struct AmbientStatic {
    name: String,
    documented: bool,
}

/// Type names that make a `static` mutable or lazily initialised state
/// shared by the whole process; any `Atomic*` name counts too.
const PROCESS_STATE_TYPES: &[&str] = &[
    "Mutex",
    "RwLock",
    "OnceLock",
    "OnceCell",
    "Once",
    "LazyLock",
    "LazyCell",
    "Lazy",
    "Cell",
    "RefCell",
    "UnsafeCell",
    "Semaphore",
    "Notify",
    "ArcSwap",
    "ArcSwapOption",
    "DashMap",
];

fn names_process_state(ty: &syn::Type) -> bool {
    struct TypeNames(bool);
    impl<'ast> Visit<'ast> for TypeNames {
        fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
            let name = segment.ident.to_string();
            self.0 |= PROCESS_STATE_TYPES.contains(&name.as_str()) || name.starts_with("Atomic");
            visit::visit_path_segment(self, segment);
        }
    }
    let mut names = TypeNames(false);
    names.visit_type(ty);
    names.0
}

/// The production statics of one file: `thread_local!`/`task_local!` statics,
/// which hold per-thread or per-task context, and process-wide statics (a
/// `static` item whose type is interior-mutable or lazily initialised, every
/// `static mut`, and every `lazy_static!` static).
#[derive(Default)]
struct StaticVisitor {
    ambient: Vec<AmbientStatic>,
    process_wide: Vec<String>,
}

impl<'ast> Visit<'ast> for StaticVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        if matches!(item.mutability, syn::StaticMutability::Mut(_)) || names_process_state(&item.ty)
        {
            self.process_wide.push(item.ident.to_string());
        }
        visit::visit_item_static(self, item);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let Some(name) = invocation
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
        else {
            return;
        };
        let local = name == "thread_local" || name == "task_local";
        if local || name == "lazy_static" {
            let tokens = invocation.tokens.clone().into_iter().collect::<Vec<_>>();
            let mut documented = false;
            for (index, token) in tokens.iter().enumerate() {
                match token {
                    proc_macro2::TokenTree::Punct(punct) if punct.as_char() == ';' => {
                        documented = false;
                    }
                    proc_macro2::TokenTree::Group(group) => {
                        documented |= matches!(
                            group.stream().into_iter().next(),
                            Some(proc_macro2::TokenTree::Ident(ident)) if ident == "doc"
                        );
                    }
                    proc_macro2::TokenTree::Ident(ident) if ident == "static" => {
                        let lifetime = index.checked_sub(1).is_some_and(|previous| {
                            matches!(&tokens[previous], proc_macro2::TokenTree::Punct(punct) if punct.as_char() == '\'')
                        });
                        if lifetime {
                            continue;
                        }
                        let name = tokens[index + 1..]
                            .iter()
                            .map(ToString::to_string)
                            .find(|word| word != "ref" && word != "mut")
                            .unwrap_or_default();
                        if local {
                            self.ambient.push(AmbientStatic { name, documented });
                        } else {
                            self.process_wide.push(name);
                        }
                    }
                    _ => {}
                }
            }
        }
        visit::visit_macro(self, invocation);
    }
}

fn production_statics(syntax: &syn::File) -> StaticVisitor {
    let mut visitor = StaticVisitor::default();
    visitor.visit_file(syntax);
    visitor
}

fn ambient_context_sites(relative: &str, syntax: &syn::File) -> usize {
    production_statics(syntax)
        .ambient
        .iter()
        .filter(|found| {
            !(found.documented
                && AMBIENT_NOT_REQUEST_CONTEXT
                    .iter()
                    .any(|(path, name)| *path == relative && *name == found.name))
        })
        .count()
}

fn process_static_sites(syntax: &syn::File) -> usize {
    production_statics(syntax).process_wide.len()
}

/// Request context (identity, signing, ACP bypass, transaction handles, the
/// requester or session, trace or request IDs) travels as explicit arguments
/// or fields. `tokio::task_local!` loses it across `tokio::spawn`, and Clippy's
/// `disallowed_macros` cannot be allowed at an item-position macro site, so
/// this test owns the ban: no production site remains and none is admitted.
#[test]
fn ambient_request_context_only_shrinks() {
    let root = repo_root();
    let violations = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter_map(|(relative, syntax)| {
            let sites = ambient_context_sites(&relative, &syntax);
            (sites > 0).then(|| {
                format!(
                    "{relative}: {sites} thread_local!/task_local! static(s); pass request context as an argument or field"
                )
            })
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "ambient request context is banned:\n{}",
        violations.join("\n")
    );
}

/// A request a client endpoint authorizes is sent through
/// `GraphqlEndpoint::send_resolving`, so a refusal of its credential reads the
/// home's status again and is sent once more (`Identity.HeldAudience.resend`),
/// or inside a transaction through `authorize_in_transaction`, whose
/// transaction recovers as a unit (`Identity.HeldAudience.resendUnit`).
/// Entries name why each remaining direct `authorize` stays; the list only
/// shrinks.
#[rustfmt::skip]
const UNRECOVERED_AUTHORIZE_ALLOWLIST: &[(&str, usize, &str)] = &[
    ("crates/gents-store/src/config_client/client_authentication.rs", 1, "correct by design: the body of the authorizer authorize_runtime, whose callers are counted"),
    ("crates/gents-store/src/config_client/graphql.rs", 2, "correct by design: a transaction's commit and discard are never sent alone again; the transaction reruns as a unit and a refused commit is told (Identity.HeldAudience.resendUnit, a_refused_commit_is_never_run_again)"),
    ("crates/gents-store/src/config_client/mod.rs", 1, "correct by design: the body of the authorizer authorize_in_transaction, whose callers recover as a unit (resendUnit)"),
];

/// What authorizes a request as the endpoint's principal.
const AUTHORIZERS: &[&str] = &["authorize", "authorize_runtime"];

/// What sends the request a closure builds through `send_resolving`.
const RESOLVING_SENDERS: &[&str] = &[
    "send_resolving",
    "send_counting_resends",
    "endpoint_get_json",
    "endpoint_send",
];

#[derive(Default)]
struct UnrecoveredAuthorizeVisitor {
    resolving: usize,
    sites: usize,
}

impl<'ast> Visit<'ast> for UnrecoveredAuthorizeVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if !self.visit_resolving_call(call) {
            visit::visit_expr_call(self, call);
        }
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if AUTHORIZERS.contains(&call.method.to_string().as_str())
            && call.args.len() == 1
            && self.resolving == 0
        {
            self.sites += 1;
        }
        if RESOLVING_SENDERS.contains(&call.method.to_string().as_str()) {
            self.visit_expr(&call.receiver);
            self.resolving += 1;
            for argument in &call.args {
                self.visit_expr(argument);
            }
            self.resolving -= 1;
        } else {
            visit::visit_expr_method_call(self, call);
        }
    }
}

impl UnrecoveredAuthorizeVisitor {
    fn visit_resolving_call(&mut self, call: &ExprCall) -> bool {
        if !path_name(call).is_some_and(|name| {
            RESOLVING_SENDERS
                .iter()
                .any(|sender| name.ends_with(sender))
        }) {
            return false;
        }
        self.resolving += 1;
        for argument in &call.args {
            self.visit_expr(argument);
        }
        self.resolving -= 1;
        true
    }
}

fn unrecovered_authorize_sites(syntax: &syn::File) -> usize {
    let mut visitor = UnrecoveredAuthorizeVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

/// Compares per-file site counts with a `(path, count, reason)` allowlist
/// that only shrinks: a file missing from the list, a count above its entry
/// and a count below it (a stale entry) each fail, and the failure lists the
/// current sites to copy from.
fn shrinking_allowlist(
    ratchet: &str,
    allowlist: &[(&str, usize, &str)],
    actual: &BTreeMap<String, usize>,
    sites: &str,
    remedy: &str,
) {
    let mut allowed = BTreeMap::new();
    let mut violations = Vec::new();
    for (path, count, _) in allowlist {
        if allowed.insert(path.to_string(), *count).is_some() {
            violations.push(format!("{path}: listed twice"));
        }
    }
    for path in allowed.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let found = actual.get(path).copied().unwrap_or_default();
        match allowed.get(path).copied() {
            None => violations.push(format!("{path}: {found} new {sites}; {remedy}")),
            Some(limit) if found > limit => violations.push(format!(
                "{path}: {found} {sites} exceed the allowlisted {limit}"
            )),
            Some(limit) if found < limit => violations.push(format!(
                "{path}: {found} {sites}; lower the allowlist entry from {limit}"
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
        "{ratchet} ratchet failed:\n{}\n\ncurrent sites:\n{current}",
        violations.join("\n")
    );
}

fn production_site_counts(count: impl Fn(&str, &syn::File) -> usize) -> BTreeMap<String, usize> {
    let root = repo_root();
    parse_production(&root, production_sources(&root))
        .into_iter()
        .map(|(relative, syntax)| {
            let sites = count(&relative, &syntax);
            (relative, sites)
        })
        .filter(|(_, sites)| *sites > 0)
        .collect()
}

#[test]
fn client_requests_recover_a_refused_credential() {
    shrinking_allowlist(
        "refused credential recovery",
        UNRECOVERED_AUTHORIZE_ALLOWLIST,
        &production_site_counts(|_, syntax| unrecovered_authorize_sites(syntax)),
        "request(s) authorized outside `send_resolving`",
        "send through it so a refused credential recovers",
    );
}

#[test]
fn credential_recovery_fence_counts_authorize_outside_send_resolving() {
    for (source, sites) in [
        ("fn f() { e.authorize(c.get(u))?.send(); }", 1),
        ("fn f() { e.send_resolving(|e| e.authorize(c.get(u))); }", 0),
        (
            "fn f() { e.send_resolving(|e| Ok(e.authorize(c.get(u))?.json(&b))); }",
            0,
        ),
        ("fn f() { e.authorize_in_transaction(c.get(u), id); }", 0),
        ("fn f() { e.authorize_runtime(c.get(u)).await?.send(); }", 1),
        (
            "fn f() { e.send_resolving(|e| e.authorize_runtime(c.get(u))); }",
            0,
        ),
        (
            "fn f() { http_client::endpoint_get_json(&e, |e| e.authorize(c.get(u))); }",
            0,
        ),
        (
            "fn f() { endpoint_send(&e, |e| e.authorize(c.post(u))); }",
            0,
        ),
        ("fn f() { other(&e, |e| e.authorize(c.post(u))); }", 1),
        (
            "fn f() { e.authorize(c.get(u)).and(e.send_resolving(|e| e.authorize(r))); }",
            1,
        ),
        ("#[cfg(test)] mod tests { fn f() { e.authorize(r); } }", 0),
        (
            "impl E { #[cfg(test)] fn f(&self) { self.authorize(r); } }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(unrecovered_authorize_sites(&syntax), sites, "{source}");
    }
}

/// A client keeps a delegation audience or clock only from a status its
/// home's node key signed for the read (`Identity.HeldAudience.reportedOfHome`):
/// what a status's unsigned fields report is read into a report only where
/// the home signs it, and a client takes a report only from the signed
/// statement (`home_status::authenticate`). Entries name each reader; the
/// list only shrinks.
#[rustfmt::skip]
const STATUS_REPORT_READERS: &[(&str, usize, &str)] = &[
    ("crates/gents-protocol/src/home_attestation.rs", 2, "correct by design: the home reading its own status into the statement it signs (HomeStatement::of_status), and the report of a statement whose signature was checked (HomeStatement::reported)"),
];

/// What reads a report from a status, and the report itself.
const STATUS_REPORT_CALLS: &[&str] = &[
    "ReportedHome::observe",
    "ReportedDelegationAudience::observe",
];

#[derive(Default)]
struct StatusReportVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for StatusReportVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if path_name(call).is_some_and(|name| {
            STATUS_REPORT_CALLS
                .iter()
                .any(|reader| name.ends_with(reader))
        }) {
            self.sites += 1;
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_struct(&mut self, report: &'ast syn::ExprStruct) {
        if report
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "ReportedHome")
        {
            self.sites += 1;
        }
        visit::visit_expr_struct(self, report);
    }
}

fn status_report_sites(syntax: &syn::File) -> usize {
    let mut visitor = StatusReportVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn a_client_keeps_only_what_its_home_signed() {
    let root = repo_root();
    let actual = parse_production(&root, production_sources(&root))
        .into_iter()
        .map(|(relative, syntax)| (relative, status_report_sites(&syntax)))
        .filter(|(_, sites)| *sites > 0)
        .collect::<BTreeMap<_, _>>();
    let allowed = STATUS_REPORT_READERS
        .iter()
        .map(|(path, count, _)| (path.to_string(), *count))
        .collect::<BTreeMap<_, _>>();
    let mut violations = Vec::new();
    for path in allowed.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let found = actual.get(path).copied().unwrap_or_default();
        match allowed.get(path).copied() {
            None => violations.push(format!(
                "{path}: {found} new read(s) of a status's unsigned report; keep only what `home_status::authenticate` returns"
            )),
            Some(limit) if found > limit => violations.push(format!(
                "{path}: {found} reads of a status's unsigned report exceed the allowlisted {limit}"
            )),
            Some(limit) if found < limit => violations.push(format!(
                "{path}: {found} reads of a status's unsigned report; lower the allowlist entry from {limit}"
            )),
            Some(_) => {}
        }
    }
    assert!(
        violations.is_empty(),
        "signed status ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn signed_status_fence_counts_unsigned_report_readers() {
    for (source, sites) in [
        ("fn f() { ReportedHome::observe(&status); }", 1),
        (
            "fn f() { gents_protocol::delegation_audience::ReportedDelegationAudience::observe(&s); }",
            1,
        ),
        ("fn f() { ReportedHome { audience, provider_time }; }", 1),
        ("fn f() { home_status::authenticate(&status, node, &nonce); }", 0),
        ("#[cfg(test)] mod tests { fn f() { ReportedHome::observe(&s); } }", 0),
        (
            "impl E { #[cfg(test)] fn f(&self) { ReportedHome { audience, provider_time }; } }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(status_report_sites(&syntax), sites, "{source}");
    }
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
    assert_eq!(ambient_context_sites("src/lib.rs", &syntax), 3);
}

#[test]
fn ambient_context_skips_only_named_documented_statics() {
    let (path, name) = AMBIENT_NOT_REQUEST_CONTEXT[0];
    for (statics, listed, elsewhere) in [
        (format!("/// Why.\n static {name}: u8;"), 0, 1),
        (format!("static {name}: u8;"), 1, 1),
        (
            format!("/// Why.\n static {name}: u8; static OTHER: u8;"),
            1,
            2,
        ),
    ] {
        let source = format!("tokio::task_local! {{ {statics} }}");
        let syntax = syn::parse_file(&source).expect("parse snippet");
        assert_eq!(ambient_context_sites(path, &syntax), listed, "{source}");
        assert_eq!(
            ambient_context_sites("src/lib.rs", &syntax),
            elsewhere,
            "{source}"
        );
    }
}

/// Behaviour comes from configuration documents and command-line flags, never
/// from the process environment: a variable is invisible to the store, to
/// other clients and to the model, and differs between the shell, the service
/// unit and the desktop that start the same runtime. This counts each
/// production `std::env::{var, var_os, vars, vars_os}` named as a path,
/// called or passed as a function (also through `use std::env::var`, and
/// inside macro arguments). It does not count `env!`/`option_env!`, which
/// Cargo resolves at compile time into a fixed property of the build, nor the
/// platform locators `home_dir`, `temp_dir`, `current_dir`, `current_exe` and
/// `args`, nor build scripts, which Cargo runs with its own environment, nor a
/// member no production build compiles (`Workspace::test_only`).
/// Entries name what replaces each remaining read (a flag, a document field,
/// a test fixture) or why it is legitimately external; the list only shrinks
/// (#1764).
#[rustfmt::skip]
const ENVIRONMENT_READ_ALLOWLIST: &[(&str, usize, &str)] = &[
    ("apps/gents-desktop/src-tauri/src/bridge/browser.rs", 1, "HOME: expands a leading ~ in a user-typed path; legitimately external (platform discovery)"),
    ("apps/gents-desktop/src-tauri/src/bridge/links.rs", 1, "APPIMAGE: AppImage probe deciding how links open from a packaged Linux build; legitimately external"),
    ("apps/gents-desktop/src-tauri/src/bridge/product_telemetry.rs", 2, "GENTS_UMAMI_ENDPOINT and GENTS_UMAMI_WEBSITE_ID: a fixed property of the release build; becomes option_env! or a desktop setting"),
    ("apps/gents-desktop/src-tauri/src/runner/live_fixture/backend.rs", 2, "the API key variable a live-fixture backend names: the runner checks it is set before seeding a backend the runtime will read it from by name; stays"),
    ("crates/gents-chatgpt-login/src/lib.rs", 1, "CODEX_CA_CERT and SSL_CERT_FILE: standard custom CA bundle convention; legitimately external"),
    ("crates/gents-cli/src/commands/chat/streaming.rs", 1, "NO_COLOR: standard convention disabling terminal colour; legitimately external"),
    ("crates/gents-cli/src/commands/eval/frame.rs", 1, "NO_COLOR: standard convention disabling terminal colour; legitimately external"),
    ("crates/gents-cli/src/commands/init.rs", 1, "HOME: platform discovery for the default tool root before any configuration exists; legitimately external"),
    ("crates/gents-cli/src/commands/job/promote.rs", 1, "JAVY: tool discovery for the javy compiler, like PATH; becomes a --javy flag"),
    ("crates/gents-cli/src/commands/pack/registry.rs", 1, "GENTS_REGISTRY_TOKEN: registry credential kept out of argv and shell history; legitimately external"),
    ("crates/gents-cli/src/commands/pack/scenario/load.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-cli/src/commands/pack/scenario/setup.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-cli/src/commands/pack/wire.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-cli/src/desired_state/load.rs", 1, "desired-state ${VAR} interpolation: the manifest names operator-supplied variables; legitimately external"),
    ("crates/gents-desktop-bridge/src/host_browser.rs", 4, "BROWSER, PATH, APPDIR and APPIMAGE: platform browser discovery and the AppImage probe; legitimately external"),
    ("crates/gents-desktop-bridge/src/package_tools.rs", 6, "PATH, APPDIR, APPIMAGE and the launcher's variables: restores the host environment for host programs launched from an AppImage; legitimately external"),
    ("crates/gents-desktop-bridge/src/tauri_commands/service_executable.rs", 4, "GENTS_BIN, PATH, APPDIR and APPIMAGE: discovery of the gents executable and the AppImage probe; GENTS_BIN becomes a desktop setting, the rest are legitimately external"),
    ("crates/gents-desktop/src/main.rs", 2, "GENTS_DESKTOP_TAURI_BIN and PATH: discovery of the desktop executable; GENTS_DESKTOP_TAURI_BIN becomes a flag"),
    ("crates/gents-documents/src/inference_backend.rs", 1, "the variable an InferenceBackend document names as its API key source (auth kind environment); the document decides, legitimately external"),
    ("crates/gents-eval/src/optimization/driver.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-eval/src/runner/embedded/executor/install.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-eval/src/runner/freeze/pack.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-host/src/serve.rs", 2, "the secret named by the metrics token flag (legitimately external) and HOME for the default working root (platform discovery)"),
    ("crates/gents-inference/src/clients/claude_messages/transport.rs", 2, "HTTPS_PROXY/https_proxy and NO_PROXY: standard proxy convention; legitimately external"),
    ("crates/gents-pack/src/operations/pack.rs", 1, "pack variables whose declared source is the installer's own environment (Lean Pack.Operations.planEnvironment); legitimately external"),
    ("crates/gents-pack/src/pack/interpolate.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-pack/src/plugin/host_imports.rs", 2, "passes the variables an installed plugin's consented env access names into its sandbox; the consent document decides, legitimately external"),
    ("crates/gents-service/src/native_service.rs", 7, "HOME, XDG_CONFIG_HOME, PATH, APPDIR and APPIMAGE: platform discovery of the service unit paths and the AppImage probe; legitimately external"),
    ("crates/gents-service/src/telemetry.rs", 1, "OTEL_EXPORTER_OTLP_* and OTEL_SERVICE_NAME: standard OpenTelemetry convention; legitimately external"),
    ("crates/gents-session/src/workspace/adapter.rs", 1, "inherits the process environment for a managed git child, minus the git variables it overrides; legitimately external"),
    ("crates/gents-tools/src/ux_preview.rs", 1, "GENTS_UX_PREVIEW_ASSETS: preview renderer assets outside an installed layout; becomes a flag"),
    ("crates/gents-tools/src/ux_preview/browser.rs", 1, "PATH: platform discovery of a system browser; legitimately external"),
    ("crates/gents/examples/serve_default_agent.rs", 1, "example program configured by its environment; not shipped"),
    ("crates/gents/src/client_runtime/mod.rs", 1, "HOME: platform discovery of the user home before configuration exists; legitimately external"),
];

const ENVIRONMENT_READS: &[&str] = &["var", "var_os", "vars", "vars_os"];

#[derive(Default)]
struct EnvironmentReadVisitor {
    qualified: usize,
    bare: Vec<String>,
    imported: BTreeSet<String>,
}

fn qualified_environment_read<'a>(mut segments: impl DoubleEndedIterator<Item = &'a str>) -> bool {
    let function = segments.next_back().unwrap_or_default();
    ENVIRONMENT_READS.contains(&function) && segments.next_back() == Some("env")
}

impl EnvironmentReadVisitor {
    fn imports(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree) {
        let from_env = prefix.last().is_some_and(|last| last == "env");
        match tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                self.imports(prefix, &path.tree);
                prefix.pop();
            }
            syn::UseTree::Name(name) if from_env => {
                let name = name.ident.to_string();
                if ENVIRONMENT_READS.contains(&name.as_str()) {
                    self.imported.insert(name);
                }
            }
            syn::UseTree::Rename(rename)
                if from_env && ENVIRONMENT_READS.contains(&rename.ident.to_string().as_str()) =>
            {
                self.imported.insert(rename.rename.to_string());
            }
            syn::UseTree::Glob(_) if from_env => {
                self.imported
                    .extend(ENVIRONMENT_READS.iter().map(ToString::to_string));
            }
            syn::UseTree::Group(group) => {
                for tree in &group.items {
                    self.imports(prefix, tree);
                }
            }
            _ => {}
        }
    }

    fn macro_tokens(&mut self, stream: proc_macro2::TokenStream) {
        let tokens = stream.into_iter().collect::<Vec<_>>();
        for (index, token) in tokens.iter().enumerate() {
            match token {
                proc_macro2::TokenTree::Group(group) => self.macro_tokens(group.stream()),
                proc_macro2::TokenTree::Ident(function)
                    if ENVIRONMENT_READS.contains(&function.to_string().as_str())
                        && index >= 3
                        && matches!(&tokens[index - 1], proc_macro2::TokenTree::Punct(punct) if punct.as_char() == ':')
                        && matches!(&tokens[index - 2], proc_macro2::TokenTree::Punct(punct) if punct.as_char() == ':')
                        && matches!(&tokens[index - 3], proc_macro2::TokenTree::Ident(module) if module == "env") =>
                {
                    self.qualified += 1;
                }
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for EnvironmentReadVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.imports(&mut Vec::new(), &item.tree);
    }

    fn visit_expr_path(&mut self, expression: &'ast syn::ExprPath) {
        let segments = expression
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        if qualified_environment_read(segments.iter().map(String::as_str)) {
            self.qualified += 1;
        } else if let [name] = segments.as_slice() {
            self.bare.push(name.clone());
        }
        visit::visit_expr_path(self, expression);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        self.macro_tokens(invocation.tokens.clone());
        visit::visit_macro(self, invocation);
    }
}

fn environment_read_sites(syntax: &syn::File) -> usize {
    let mut visitor = EnvironmentReadVisitor::default();
    visitor.visit_file(syntax);
    visitor.qualified
        + visitor
            .bare
            .iter()
            .filter(|name| visitor.imported.contains(*name))
            .count()
}

#[test]
fn runtime_environment_reads_only_shrink() {
    let workspace = Workspace::load(&repo_root());
    let test_only = workspace
        .test_only()
        .iter()
        .map(|name| format!("{}/", workspace.members[name].0))
        .collect::<Vec<_>>();
    shrinking_allowlist(
        "runtime environment read",
        ENVIRONMENT_READ_ALLOWLIST,
        &production_site_counts(|relative, syntax| {
            if relative.ends_with("build.rs")
                || test_only
                    .iter()
                    .any(|member| relative.starts_with(member.as_str()))
            {
                0
            } else {
                environment_read_sites(syntax)
            }
        }),
        "environment read(s)",
        "take the value from a flag or a configuration document",
    );
}

#[test]
fn environment_reads_count_runtime_reads_outside_test_items() {
    for (source, sites) in [
        ("fn f() { let _ = std::env::var(\"A\"); }", 1),
        (
            "fn f() { let _ = env::var_os(\"A\"); for _ in std::env::vars() {} }",
            2,
        ),
        ("fn f() { let _ = ::std::env::vars_os(); }", 1),
        ("fn f() { let _ = names.iter().map(std::env::var); }", 1),
        ("use std::env::var; fn f() { let _ = var(\"A\"); }", 1),
        (
            "fn f() { let _ = var(\"A\"); } use std::env::{self, var_os as read};",
            0,
        ),
        (
            "fn f() { let _ = read(\"A\"); } use std::env::{self, var_os as read};",
            1,
        ),
        (
            "fn f() { tracing::info!(\"{:?}\", std::env::var(\"A\")); }",
            1,
        ),
        ("fn f() { let _ = var(\"A\"); }", 0),
        ("fn f() { let _ = (env!(\"A\"), option_env!(\"B\")); }", 0),
        (
            "fn f() { let _ = (std::env::home_dir(), std::env::temp_dir()); }",
            0,
        ),
        ("fn f() { std::env::set_var(\"A\", \"1\"); }", 0),
        ("#[cfg(test)] fn f() { let _ = std::env::var(\"A\"); }", 0),
        (
            "#[cfg(all(test, unix))] mod t { fn f() { let _ = std::env::var(\"A\"); } }",
            0,
        ),
        (
            "impl S { #[cfg(test)] fn f() { let _ = std::env::var(\"A\"); } }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(environment_read_sites(&syntax), sites, "{source}");
    }
}

/// Configuration and request context travel as arguments or as fields on the
/// owner that reaches the reader, never in process-wide mutable state: a
/// static is shared by every node and Agent in the process, so the last
/// writer wins for all of them, and a test that sets one leaks into every
/// other test in its binary. This counts each production `static` item whose
/// type is interior-mutable or lazily initialised (`PROCESS_STATE_TYPES`,
/// `Atomic*`), every `static mut` and every `lazy_static!` static; a plain
/// constant `static` is not counted. `thread_local!` and `task_local!` belong
/// to `ambient_request_context_only_shrinks`, which admits none. Entries name
/// why each remaining static is a genuine process singleton (an immutable
/// value built once, a process-global resource) or what owner replaces it;
/// the list only shrinks (#1764).
#[rustfmt::skip]
const PROCESS_STATIC_ALLOWLIST: &[(&str, usize, &str)] = &[
    ("apps/gents-desktop/src-tauri/src/bridge/browser.rs", 2, "the desktop window's one embedded browser and its applied-generation counter: a process-global UI resource; stays"),
    ("apps/gents-desktop/src-tauri/src/bridge/notifications.rs", 1, "count of notification permission waits in the one desktop process; stays"),
    ("apps/gents-desktop/src-tauri/src/bridge/windows.rs", 1, "next window label of the one desktop process; stays"),
    ("crates/gents-automation/src/lifecycle_hooks.rs", 2, "the bundled hook registry and composition built once: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-desktop-bridge/src/runtime_setup.rs", 1, "the desktop's one Tokio runtime, built at its entry point; stays"),
    ("crates/gents-desktop-bridge/src/tauri_commands/service_executable.rs", 2, "lock and attempt counter serialising installs of the one service executable a host has; process-global resource, stays"),
    ("crates/gents-eval/src/authoring/dossier.rs", 1, "runtime collection names derived from the schema: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-host/src/http/version.rs", 1, "the build's version response: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-host/src/p2p_status.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-inference/src/admission/config.rs", 1, "per-process HMAC key for credential fingerprints, never persisted by design; stays"),
    ("crates/gents-inference/src/auth/credential/bearer.rs", 1, "weak registry of live OAuth bearers by credential, reused only on the node holding it, so every in-process user of that node's credential (agent clients, usage polls, health probes, model discovery) shares one refresh; they share no owner but the DefraDB node, so it moves onto a per-node inference owner once one exists"),
    ("crates/gents-inference/src/clients/claude_messages/transport.rs", 1, "shared HTTP clients built once from the proxy environment, one keep-alive pool per proxy decision; stays"),
    ("crates/gents-lean-contract/src/lib.rs", 2, "the Lean process output cache and load lock in a crate that links only into tests; stays"),
    ("crates/gents-lean-vocab/src/lib.rs", 1, "the Lean contract snapshot in a crate that links only into tests: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-pack/src/pack/registry.rs", 1, "one registry download at a time per process, an external premise of the registry's throttle; stays"),
    ("crates/gents-pack/src/plugin/allowed.rs", 1, "embedded release digests: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-pack/src/plugin/allowed/protected.rs", 1, "protected plugin paths shared by every home in the process; moves onto the home's owner"),
    ("crates/gents-pack/src/plugin/script_engine.rs", 1, "the script engine, compiled once: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-pack/src/plugin/ux/lint.rs", 10, "UX lint rules: compiled regular expressions, immutable once built; stay"),
    ("crates/gents-pack/src/plugin/wasi_runtime.rs", 1, "the WASI async runtime, a process-global resource built once; stays"),
    ("crates/gents-protocol/src/failure_text.rs", 5, "secret redaction patterns: compiled regular expressions, immutable once built; stay"),
    ("crates/gents-protocol/src/graphql/transport.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-session/src/exec/process/registry.rs", 2, "registry and id counter of managed child processes, which belong to the process; stays"),
    ("crates/gents-session/src/request/admission.rs", 1, "the default request input: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-session/src/session/output/replay_capture.rs", 1, "log rate limiter for replay capture; stays"),
    ("crates/gents-store/src/config_client/common.rs", 1, "monotonic recreate-identity clock, process-local by design; stays"),
    ("crates/gents-store/src/config_client/graphql.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-store/src/config_client/runtime_endpoints.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-store/src/config_client/subscription.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
    ("crates/gents-store/src/identity/request_receipt.rs", 1, "the default request input: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-store/src/node_extension.rs", 1, "the node extension table, the one owner of per-node attachments and per-node state keyed by node address; stays"),
    ("crates/gents-store/src/schema/catalog.rs", 4, "baseline schemas and the runtime catalog: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-store/src/schema/catalog/install.rs", 1, "lock serialising schema ensure across the desktop and runtime paths of one process; stays"),
    ("crates/gents-store/src/schema/journal.rs", 1, "journal baseline collections: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-store/src/service.rs", 1, "the build identifier: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-toolcalls/src/call/lifecycle/recovery.rs", 1, "rate limit of the unresolved-parent warning; stays"),
    ("crates/gents-toolcalls/src/plugin/executor.rs", 1, "a built-in plugin declaration: an immutable value built once on first use, the same for every node and Agent; stays"),
    ("crates/gents-tools/src/builtin/self_config/configuration_discovery/text.rs", 5, "secret redaction patterns: compiled regular expressions, immutable once built; stay"),
    ("crates/gents-tools/src/native/command_observability.rs", 2, "process-wide command policy counters for metrics; stay"),
    ("crates/gents/src/client_runtime/mod.rs", 1, "the shared reqwest client, a connection pool built once; stays"),
];

#[test]
fn process_wide_statics_only_shrink() {
    shrinking_allowlist(
        "process-wide static",
        PROCESS_STATIC_ALLOWLIST,
        &production_site_counts(|_, syntax| process_static_sites(syntax)),
        "process-wide static(s)",
        "pass the value as an argument or hold it on its owner",
    );
}

#[test]
fn process_statics_count_interior_mutable_statics_outside_test_items() {
    let syntax = syn::parse_file(
        r##"
        static A: std::sync::Mutex<u8> = std::sync::Mutex::new(0);
        static B: AtomicU64 = AtomicU64::new(0);
        static mut C: u8 = 0;
        fn f() { static D: OnceLock<Arc<u8>> = OnceLock::new(); }
        static E: std::sync::LazyLock<Option<RwLock<u8>>> = std::sync::LazyLock::new(|| None);
        lazy_static::lazy_static! { static ref F: u8 = 0; }
        static G: &[&str] = &["Mutex"];
        static H: u64 = 0;
        const I: std::sync::Mutex<u8> = std::sync::Mutex::new(0);
        fn g() { let _ = r#"static J: RwLock<u8> = RwLock::new(0);"#; }
        thread_local! { static K: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }
        #[cfg(test)]
        static L: OnceLock<u8> = OnceLock::new();
        #[cfg(all(test, unix))]
        mod tests { static M: AtomicBool = AtomicBool::new(false); }
        impl S { #[cfg(test)] fn f() { static N: OnceLock<u8> = OnceLock::new(); } }
        "##,
    )
    .expect("parse snippet");
    assert_eq!(process_static_sites(&syntax), 6);
    assert_eq!(ambient_context_sites("src/lib.rs", &syntax), 1);
}

/// Functions that create a file in-process (`fs::write`, `fs::copy`,
/// `File::create`, an `OpenOptions` open with `create`) and then give that
/// path an exec bit, or that `fs::copy` an executable (a source naming a
/// binary, which the copy keeps executable). Linux execve refuses a file any
/// process holds open for writing (ETXTBSY), and a concurrent spawn copies
/// this process's write descriptor into its child until that child execs,
/// even with O_CLOEXEC (`tla/ScriptFixtureExec.tla`). A static script is
/// checked in; a generated one goes through
/// `gents_store::test_support::write_executable_script` and a copied one
/// through `gents_store::test_support::copy_executable`. No site
/// remains and none is admitted.
#[derive(Default)]
struct ExecutableWriteScan {
    written: BTreeSet<String>,
    permitted: Vec<String>,
    executable_mode: bool,
    executable_copy: bool,
}

/// A copy source that names a binary: Cargo's `CARGO_BIN_EXE_*`,
/// `current_exe`, or a `*_bin`/`*_exe`/`executable*` binding or helper.
static EXECUTABLE_SOURCE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"\b(CARGO_BIN_EXE\w*|current_exe|\w+_bin|\w+_exe|\w*executable\w*)\b")
        .expect("executable source pattern")
});

fn method_chain_calls(expression: &Expr, names: &[&str]) -> bool {
    match expression {
        Expr::MethodCall(call) => {
            names.iter().any(|name| call.method == name)
                || method_chain_calls(&call.receiver, names)
        }
        _ => false,
    }
}

fn path_argument_key(expression: &Expr) -> String {
    match expression {
        Expr::Reference(reference) => path_argument_key(&reference.expr),
        Expr::Paren(inner) => path_argument_key(&inner.expr),
        other => quote::ToTokens::to_token_stream(other).to_string(),
    }
}

fn executable_literal(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Lit(syn::ExprLit { lit: syn::Lit::Int(literal), .. })
            if literal.base10_parse::<u32>().is_ok_and(|mode| mode & 0o111 != 0)
    )
}

impl<'ast> Visit<'ast> for ExecutableWriteScan {
    fn visit_item_fn(&mut self, _: &'ast ItemFn) {}

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(path) = call.func.as_ref() {
            let segments = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            let first = call.args.first();
            match segments.as_slice() {
                [.., fs, write] if fs == "fs" && write == "write" => {
                    self.written.extend(first.map(path_argument_key));
                }
                [.., fs, copy] if fs == "fs" && copy == "copy" => {
                    self.written
                        .extend(call.args.iter().nth(1).map(path_argument_key));
                    self.executable_copy |= first.is_some_and(|source| {
                        EXECUTABLE_SOURCE.is_match(&path_argument_key(source))
                    });
                }
                [.., file, create]
                    if file == "File" && (create == "create" || create == "create_new") =>
                {
                    self.written.extend(first.map(path_argument_key));
                }
                [.., set] if set == "set_permissions" => {
                    self.permitted.extend(first.map(path_argument_key));
                }
                [.., from_mode] if from_mode == "from_mode" => {
                    self.executable_mode |= first.is_some_and(executable_literal);
                }
                _ => {}
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if call.method == "set_mode" {
            self.executable_mode |= call.args.first().is_some_and(executable_literal);
        }
        if call.method == "open" && method_chain_calls(&call.receiver, &["create", "create_new"]) {
            self.written
                .extend(call.args.first().map(path_argument_key));
        }
        visit::visit_expr_method_call(self, call);
    }
}

#[derive(Default)]
struct ExecutableWriteVisitor {
    sites: usize,
}

impl ExecutableWriteVisitor {
    fn scan(&mut self, block: &syn::Block) {
        let mut scan = ExecutableWriteScan::default();
        scan.visit_block(block);
        if scan.executable_copy
            || scan.executable_mode
                && scan
                    .permitted
                    .iter()
                    .any(|path| scan.written.contains(path))
        {
            self.sites += 1;
        }
    }
}

impl<'ast> Visit<'ast> for ExecutableWriteVisitor {
    fn visit_item_fn(&mut self, function: &'ast ItemFn) {
        self.scan(&function.block);
        visit::visit_item_fn(self, function);
    }

    fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
        self.scan(&function.block);
        visit::visit_impl_item_fn(self, function);
    }
}

fn executable_write_sites(syntax: &syn::File) -> usize {
    let mut visitor = ExecutableWriteVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn executables_are_never_written_in_process() {
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
            let sites = executable_write_sites(&syntax);
            (sites > 0).then(|| {
                let relative = path
                    .strip_prefix(&root)
                    .expect("source under repo root")
                    .to_string_lossy()
                    .into_owned();
                format!(
                    "{relative}: {sites} function(s) write a file and then make it executable; check in a static script or use gents_store::test_support::write_executable_script"
                )
            })
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "executable write ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn executable_write_fence_pairs_a_write_with_an_exec_bit_on_its_path() {
    for (source, sites) in [
        (
            "fn f() { std::fs::write(&p, b\"x\").unwrap(); std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); }",
            1,
        ),
        (
            "fn f() { fs::write(&p, b\"x\")?; let mut m = fs::metadata(&p)?.permissions(); m.set_mode(0o700); fs::set_permissions(p, m)?; }",
            1,
        ),
        (
            "fn f() { let stub = |body: &str| { std::fs::write(&b, body).unwrap(); std::fs::set_permissions(&b, Permissions::from_mode(0o751)).unwrap(); }; }",
            1,
        ),
        (
            "impl S { fn f(&self) { fs::write(&p, b\"x\")?; fs::set_permissions(&p, Permissions::from_mode(0o755))?; } }",
            1,
        ),
        (
            "fn f() { std::fs::write(&p, b\"x\").unwrap(); std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap(); }",
            0,
        ),
        (
            "fn f() { std::fs::write(&p.join(\"a\"), b\"x\").unwrap(); std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap(); }",
            0,
        ),
        (
            "fn f() { write_executable_script(&p, \"#!/bin/sh\\n\").unwrap(); }",
            0,
        ),
        (
            "fn f() { fs::copy(source, target)?; fs::set_permissions(target, fs::Permissions::from_mode(0o700))?; }",
            1,
        ),
        ("fn f() { fs::copy(cli_bin(), &copy)?; }", 1),
        ("fn f() { std::fs::copy(std::env::current_exe()?, &copy)?; }", 1),
        ("fn f() { std::fs::copy(entry.path(), &target)?; }", 0),
        (
            "fn f() { let f = File::create(&p)?; drop(f); fs::set_permissions(&p, Permissions::from_mode(0o755))?; }",
            1,
        ),
        (
            "fn f() { let f = OpenOptions::new().write(true).create(true).open(&p)?; fs::set_permissions(&p, Permissions::from_mode(0o755))?; }",
            1,
        ),
        (
            "fn f() { let f = OpenOptions::new().write(true).open(&p)?; fs::set_permissions(&p, Permissions::from_mode(0o755))?; }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(executable_write_sites(&syntax), sites, "{source}");
    }
}

/// Owner and library interfaces: the seams where callers decide behavior by
/// which failure occurred. They return module-owned `thiserror` enums and
/// classifiers match those enums; application code may use `anyhow`.
const OWNER_MODULES: &[&str] = &[
    "crates/gents-tools/src/ux_data/create_schema.rs",
    "crates/gents-claude/src",
    "crates/gents-protocol/src",
    "crates/gents-loop/src",
    "crates/gents/src/operations",
    "crates/gents-store/src/config_client/common.rs",
    "crates/gents-store/src/config_client/error.rs",
    "crates/gents-store/src/config_client/graphql.rs",
    "crates/gents-store/src/config_client/mod.rs",
    "crates/gents-documents/src/patch.rs",
    "crates/gents-store/src/config_client/retry.rs",
    "crates/gents-store/src/config_client/schema_contract.rs",
    "crates/gents-store/src/config_client/schema_install.rs",
    "crates/gents-store/src/config_client/schema_management.rs",
    "crates/gents-store/src/config_client/txn",
    "crates/gents-store/src/config_client/write_telemetry.rs",
    "crates/gents-store/src/graphql",
    "crates/gents-store/src/local/home",
    "crates/gents-store/src/identity",
    "crates/gents-store/src/schema/catalog",
    "crates/gents-store/src/local/storage_backend",
    "crates/gents-store/src/local/store_key",
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

/// Production sites that classify an error by its concrete type and are not
/// the owner asking for its own type: a downcast (`downcast*`, `is::<T>`) or a
/// `find_source` call whose type the function's module does not declare and
/// export, or that sits in a function that is not handed an error. Each is
/// debt: the producer returns an owner enum whose variants the caller matches,
/// or the caller asks the owner's classifier. A third-party type is asked
/// for in an adapter classifier instead. This list only shrinks.
#[rustfmt::skip]
const DOWNCAST_CLASSIFIER_ALLOWLIST: &[(&str, usize)] = &[
];

/// Owner classifiers: the functions through which an owner asks an erased
/// error for its own error type. A site is one when its function is
/// synchronous and takes an error, and the type asked for is an error type
/// that is declared and exported (any `pub`) in the same file, or, for a file
/// in `OWNER_MODULES`, in an `OWNER_MODULES` file of the same crate. The chain
/// walker alone may ask for its own type parameter. These sites are not debt;
/// a new entry is a reviewed addition.
#[rustfmt::skip]
const OWNER_CLASSIFIER_REGISTRY: &[(&str, &[&str])] = &[
    ("crates/gents-client-host/src/editor_store.rs", &["HistoryUnlockRequired::occurs_in"]),
    ("crates/gents-desktop-bridge/src/client_runtime/authored_delivery.rs", &["SavedForDelivery::in_error"]),
    ("crates/gents-store/src/client_error.rs", &["BridgeError::carried_by", "BridgeError::classify_transport_error"]),
    ("crates/gents-documents/src/desired_state/plan.rs", &["stale_expectation"]),
    ("crates/gents-documents/src/desired_state/validate.rs", &["OutcomeSourceSchemaError::in_error"]),
    ("crates/gents-documents/src/references.rs", &["MissingReference::in_error"]),
    ("crates/gents-eval/src/optimization/driver.rs", &["job_refused"]),
    ("crates/gents-eval/src/optimization/job.rs", &["journal_conflict"]),
    ("crates/gents-eval/src/optimization/promote.rs", &["promote_refused"]),
    ("crates/gents-eval/src/report/mod.rs", &["report_refused"]),
    ("crates/gents-eval/src/runner/freeze.rs", &["freeze_refused"]),
    ("crates/gents-eval/src/runner/mod.rs", &["provider_down"]),
    ("crates/gents-inference/src/backends/provider.rs", &["ModelDiscoveryHttpError::in_chain"]),
    ("crates/gents-inference/src/backends/registry.rs", &["ActorBackendDiscoveryError::unavailable_in"]),
    ("crates/gents-loop/src/loop_stream/aggregate_budget.rs", &["aggregate_token_budget_exhaustion_message"]),
    ("crates/gents-loop/src/loop_stream/contract.rs", &["ReplayEvidenceViolation::in_chain"]),
    ("crates/gents-p2p/src/p2p_reconcile/enrollment_store.rs", &["EnrollmentDecisionRefusal::in_chain"]),
    ("crates/gents-p2p/src/p2p_reconcile/trait_def.rs", &["RemoteP2pAdminError::in_error"]),
    ("crates/gents-pack/src/pack/registry.rs", &["RegistryError::in_chain"]),
    ("crates/gents-pack/src/plugin.rs", &["GuestTrap::in_chain", "GuestTrap::raised_by"]),
    ("crates/gents-pack/src/plugin/http_calls.rs", &["InternalAddress::in_chain"]),
    ("crates/gents-pack/src/plugin/store.rs", &["PublishedPluginReadError::error_chain_is_busy"]),
    ("crates/gents-protocol/src/database_error.rs", &["is_storage_full"]),
    ("crates/gents-protocol/src/error_chain.rs", &["exactly"]),
    ("crates/gents-protocol/src/graphql/transport.rs", &["graphql_error_is_retryable"]),
    ("crates/gents-protocol/src/operations/install_repair.rs", &["IncompatibleStore::of"]),
    ("crates/gents-protocol/src/output.rs", &["ReconstructionError::in_chain"]),
    ("crates/gents-protocol/src/output/recovery.rs", &["RecoveryPlanError::in_chain"]),
    ("crates/gents-service/src/enrollment_admin.rs", &["EnrollmentDecisionRejected::in_chain"]),
    ("crates/gents-service/src/native_service.rs", &["BackgroundApprovalPending::take", "UnsupportedNativeServicePlatform::occurs_in"]),
    ("crates/gents-service/src/runtime_control.rs", &["ControlFailure::reason_in"]),
    ("crates/gents-session/src/request/admission.rs", &["classify_context_error"]),
    ("crates/gents-session/src/request/lifecycle.rs", &["is_claim_admission_error"]),
    ("crates/gents-session/src/request/lifecycle/execution_lease.rs", &["ExecutionOwnershipLost::occurs_in"]),
    ("crates/gents-session/src/request/lifecycle/queue/folding.rs", &["PendingInputChanged::occurs_in"]),
    ("crates/gents-session/src/request/seam_error.rs", &["RuntimeSeamError::recover"]),
    ("crates/gents-session/src/session/compaction_entries.rs", &["StaleCompactionGeneration::occurs_in"]),
    ("crates/gents-session/src/session/output.rs", &["CanonicalOutputReadError::in_error"]),
    ("crates/gents-store/src/config_client/retry.rs", &["access_owner", "rerun_request_in"]),
    ("crates/gents-store/src/config_client/schema_install.rs", &["install_error_in", "schema_install_mismatch"]),
    ("crates/gents-store/src/config_client/user_write.rs", &["UserWriteRefusal::in_chain"]),
    ("crates/gents-store/src/identity/error.rs", &["identity_key_unavailable"]),
    ("crates/gents-store/src/local/backend_protection.rs", &["BackendProtectionError::in_chain"]),
    ("crates/gents-store/src/local/document_acp.rs", &["DocumentAcpError::in_chain"]),
    ("crates/gents-store/src/local/home.rs", &["store_lock_held"]),
    ("crates/gents-store/src/local/storage_backend.rs", &["find_incompatible", "find_insecure_key"]),
    ("crates/gents-store/src/local/store_key.rs", &["store_key_unavailable"]),
    ("crates/gents-store/src/schema/catalog/error.rs", &["SchemaError::in_chain"]),
    ("crates/gents-toolcalls/src/background/job/records.rs", &["RecordError::in_error"]),
    ("crates/gents-toolcalls/src/call/hook/persistence/helpers.rs", &["BackgroundSlotRefusal::in_failure"]),
    ("crates/gents-tools/src/builtin/self_config/command.rs", &["CommandGuidance::in_error"]),
    ("crates/gents-tools/src/builtin/self_config/command/crud.rs", &["BatchFailure::in_error"]),
    ("crates/gents-tools/src/builtin/self_config/command/guidance.rs", &["CatalogRefusal::in_error"]),
    ("crates/gents-tools/src/builtin/self_config/ops.rs", &["MissingAgent::in_error", "MissingConfigDocument::in_error"]),
    ("crates/gents-tools/src/guided_failure.rs", &["GuidedFailure::or_help"]),
    ("crates/gents-tools/src/mcp/pool/auth_refusal.rs", &["auth_refusal_of"]),
    ("crates/gents/src/eval/documents.rs", &["already_completed", "already_invalidated", "feedback_off_train"]),
    ("crates/gents/src/runtime/agent/daemon.rs", &["ShutdownDrainFailure::occurs_in"]),
];

/// The one generic chain walker; every other generic downcast hides the type
/// it asks for and counts.
const CHAIN_WALKER: &str = "crates/gents-protocol/src/error_chain.rs";

/// Production decisions on an error's text outside an adapter-boundary
/// classifier. Each is debt: the fact becomes an owner enum variant that
/// callers match, or the decision moves into an adapter classifier whose
/// docstring names the external premise. This list only shrinks.
#[rustfmt::skip]
const ERROR_TEXT_CLASSIFIER_ALLOWLIST: &[(&str, usize)] = &[
];

/// Adapter-boundary classifiers: functions that decide on error text, or ask
/// an erased error for a third-party type, because third-party or persisted
/// text or a third-party type is the only signal, which their docstring
/// states on a line starting `External premise:`. A type is third-party when
/// it is written out, is no type parameter and no `crate`, `self`, `super`
/// or `gents_*` path, and either no workspace source declares its name or its
/// path starts at an external crate. Their decisions are not debt; a new
/// entry is a reviewed addition.
#[rustfmt::skip]
const ADAPTER_CLASSIFIER_REGISTRY: &[(&str, &[&str])] = &[
    ("crates/gents-cli/src/commands/eval/watch.rs", &["store_locked"]),
    ("crates/gents-desktop-bridge/src/host/local/windows.rs", &["pipe_name_reserved"]),
    ("crates/gents-observe/src/client_inference.rs", &["classify_discovery_failure"]),
    ("crates/gents-eval/src/report/store.rs", &["unresolvable"]),
    ("crates/gents-eval/src/runner/embedded/executor/outcome.rs", &["provider_reason_from_failure"]),
    ("crates/gents-loop/src/error.rs", &["ProviderReport::classify_failure_text"]),
    ("crates/gents-loop/src/tool.rs", &["schema_args_error"]),
    ("crates/gents-loop/src/tool_call_lifecycle/runtime.rs", &["classify_error_text"]),
    ("crates/gents-observe/src/observe/trace_export.rs", &["looks_like_deadline_or_inference_failure"]),
    ("crates/gents-pack/src/plugin/store.rs", &["is_not_found"]),
    ("crates/gents-protocol/src/graphql/response.rs", &["is_collection_missing_error_message"]),
    ("crates/gents-protocol/src/graphql/transport.rs", &["graphql_transport_source"]),
    ("crates/gents-protocol/src/node_readiness.rs", &["is_behavior_unavailable_rejection"]),
    ("crates/gents-service/src/native_service.rs", &["ensure_launchd_result"]),
    ("crates/gents-session/src/workspace/action_plan.rs", &["parse_action_plan_json"]),
    ("crates/gents-session/src/workspace/adapter.rs", &["git_blob_missing"]),
    ("crates/gents-store/src/config_client/retry.rs", &["ConfigClientError::is_missing_or_denied_write", "is_transaction_conflict_text"]),
    ("crates/gents-store/src/graphql.rs", &["is_storage_full"]),
    ("crates/gents-tools/src/builtin/schema_help/mod.rs", &["sdl_help_call"]),
    ("crates/gents-tools/src/builtin/self_config/command/patch.rs", &["serde_found_string"]),
    ("crates/gents-tools/src/mcp/health.rs", &["classify_last_error"]),
    ("crates/gents-tools/src/mcp/pool/auth_refusal.rs", &["classify"]),
];

const EXTERNAL_PREMISE: &str = "External premise:";

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

/// An expression that is an error: one named as an error, or a binding in
/// `bound` that its function received as an error under any name.
fn expression_is_error(expression: &Expr, bound: &[String]) -> bool {
    match expression {
        Expr::Path(path) => path.path.segments.last().is_some_and(|segment| {
            let name = segment.ident.to_string();
            names_error(&name) || (path.path.segments.len() == 1 && bound.contains(&name))
        }),
        // A `cause` field states why a request or event happened; a `cause`
        // binding is an error walked from a source chain.
        Expr::Field(field) => match &field.member {
            syn::Member::Named(name) => {
                let name = name.to_string();
                name != "cause" && names_error(&name)
            }
            syn::Member::Unnamed(_) => expression_is_error(&field.base, bound),
        },
        Expr::Reference(reference) => expression_is_error(&reference.expr, bound),
        Expr::Paren(paren) => expression_is_error(&paren.expr, bound),
        Expr::Unary(unary) => expression_is_error(&unary.expr, bound),
        Expr::MethodCall(call) => {
            let method = call.method.to_string();
            names_error(&method)
                || (ERROR_VIEWS.contains(&method.as_str())
                    && expression_is_error(&call.receiver, bound))
        }
        Expr::Call(call) => {
            path_name(call).is_some_and(|name| name.ends_with("ErrorChain"))
                || call
                    .args
                    .iter()
                    .any(|argument| expression_is_error(argument, bound))
        }
        _ => false,
    }
}

fn macro_renders_error(invocation: &syn::Macro, bound: &[String]) -> bool {
    invocation.path.is_ident("format")
        && invocation
            .tokens
            .clone()
            .into_iter()
            .any(|token| match token {
                proc_macro2::TokenTree::Ident(ident) => {
                    let ident = ident.to_string();
                    names_error(&ident) || bound.contains(&ident)
                }
                proc_macro2::TokenTree::Literal(literal) => {
                    let text = literal.to_string();
                    ["{e:", "{e}", "{err", "{error"]
                        .iter()
                        .any(|start| text.contains(start))
                        || bound.iter().any(|name| {
                            text.contains(&format!("{{{name}}}"))
                                || text.contains(&format!("{{{name}:"))
                        })
                }
                _ => false,
            })
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

/// A docstring names its external premise on a line that starts
/// `External premise:` and goes on to state it.
fn names_external_premise(attributes: &[syn::Attribute]) -> bool {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("doc"))
        .filter_map(|attribute| match &attribute.meta {
            syn::Meta::NameValue(pair) => match &pair.value {
                Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(text),
                    ..
                }) => Some(text.value()),
                _ => None,
            },
            _ => None,
        })
        .any(|text| {
            text.lines().any(|line| {
                line.trim_start()
                    .strip_prefix(EXTERNAL_PREMISE)
                    .is_some_and(|premise| !premise.trim().is_empty())
            })
        })
}

/// A type that is an error by its name or trait: `anyhow::Error`, an
/// `...Error`, `dyn Error`, `impl Error`, or a reference or box of one.
fn names_error_type(ty: &syn::Type) -> bool {
    let error_bound = |bound: &syn::TypeParamBound| {
        matches!(bound, syn::TypeParamBound::Trait(bound)
            if bound.path.segments.last().is_some_and(|segment| segment.ident == "Error"))
    };
    match ty {
        syn::Type::Paren(paren) => names_error_type(&paren.elem),
        syn::Type::Reference(reference) => names_error_type(&reference.elem),
        syn::Type::Path(path) => path.path.segments.last().is_some_and(|segment| {
            let boxed = segment.ident == "Box"
                && matches!(&segment.arguments, syn::PathArguments::AngleBracketed(arguments)
                if arguments.args.iter().any(|argument| {
                    matches!(argument, syn::GenericArgument::Type(inner) if names_error_type(inner))
                }));
            boxed || segment.ident.to_string().ends_with("Error")
        }),
        syn::Type::TraitObject(object) => object.bounds.iter().any(error_bound),
        syn::Type::ImplTrait(object) => object.bounds.iter().any(error_bound),
        _ => false,
    }
}

/// The function a classifier site sits in, as the two classifier ratchets
/// judge it: by its signature and its docstring, never by its name.
struct FnFrame {
    name: String,
    /// Synchronous, with a parameter typed as an error.
    asks_error: bool,
    type_parameters: Vec<String>,
    /// Its docstring names the external premise.
    premise: bool,
}

fn fn_frame(
    owner: Option<&String>,
    attributes: &[syn::Attribute],
    signature: &syn::Signature,
) -> FnFrame {
    let takes_error = signature
        .inputs
        .iter()
        .any(|input| matches!(input, syn::FnArg::Typed(typed) if names_error_type(&typed.ty)));
    FnFrame {
        name: match owner {
            Some(owner) => format!("{owner}::{}", signature.ident),
            None => signature.ident.to_string(),
        },
        asks_error: takes_error && signature.asyncness.is_none(),
        type_parameters: signature
            .generics
            .type_params()
            .map(|parameter| parameter.ident.to_string())
            .collect(),
        premise: names_external_premise(attributes),
    }
}

/// The type a downcast to `Box<T>` asks for is `T`: a boxed `#[source]` sits
/// on the chain as the box, so asking for the box is asking for its owner.
fn unboxed(ty: &syn::Type) -> &syn::Type {
    let syn::Type::Path(path) = ty else {
        return ty;
    };
    let Some(segment) = path.path.segments.last() else {
        return ty;
    };
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return ty;
    };
    match (segment.ident == "Box", arguments.args.first()) {
        (true, Some(syn::GenericArgument::Type(inner))) if arguments.args.len() == 1 => {
            unboxed(inner)
        }
        _ => ty,
    }
}

fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default(),
        syn::Type::Paren(paren) => type_name(&paren.elem),
        syn::Type::Reference(reference) => type_name(&reference.elem),
        _ => String::new(),
    }
}

fn pattern_names(pattern: &syn::Pat, names: &mut Vec<String>) {
    match pattern {
        syn::Pat::Ident(binding) => {
            names.push(binding.ident.to_string());
            if let Some((_, inner)) = &binding.subpat {
                pattern_names(inner, names);
            }
        }
        syn::Pat::Type(typed) => pattern_names(&typed.pat, names),
        syn::Pat::Reference(reference) => pattern_names(&reference.pat, names),
        syn::Pat::Paren(paren) => pattern_names(&paren.pat, names),
        syn::Pat::Tuple(tuple) => tuple
            .elems
            .iter()
            .for_each(|inner| pattern_names(inner, names)),
        syn::Pat::TupleStruct(tuple) => tuple
            .elems
            .iter()
            .for_each(|inner| pattern_names(inner, names)),
        syn::Pat::Or(cases) => cases
            .cases
            .iter()
            .for_each(|inner| pattern_names(inner, names)),
        _ => {}
    }
}

fn first_type(arguments: &syn::AngleBracketedGenericArguments) -> Option<&syn::Type> {
    arguments.args.iter().find_map(|argument| match argument {
        syn::GenericArgument::Type(ty) => Some(ty),
        _ => None,
    })
}

/// The Rust expressions a macro invocation holds. Macro input is not parsed
/// by `syn`, so a site inside `matches!`, `assert!` or `tokio::select!` would
/// otherwise never count. The input is read as an expression list, then as
/// statements; where both fail it is split at `,`, `;`, `=>` and `if`, and a
/// piece that is no expression at its lone `:` (a `json!` key), and each
/// piece is read alone. Pieces that are still not expressions give up their
/// groups to the same reading and their remaining tokens to `loose`.
fn macro_expressions(
    tokens: proc_macro2::TokenStream,
    parsed: &mut Vec<Expr>,
    loose: &mut Vec<proc_macro2::TokenTree>,
) {
    use proc_macro2::{Spacing, TokenTree};
    use syn::parse::Parser;
    let list = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
    if let Ok(expressions) = list.parse2(tokens.clone()) {
        parsed.extend(expressions);
        return;
    }
    if let Ok(stmts) = syn::Block::parse_within.parse2(tokens.clone()) {
        parsed.push(Expr::Block(syn::ExprBlock {
            attrs: Vec::new(),
            label: None,
            block: syn::Block {
                brace_token: syn::token::Brace::default(),
                stmts,
            },
        }));
        return;
    }
    let tokens = tokens.into_iter().collect::<Vec<_>>();
    let mut pieces = vec![Vec::new()];
    let mut index = 0;
    while index < tokens.len() {
        let arrow = matches!(
            (&tokens[index], tokens.get(index + 1)),
            (TokenTree::Punct(equals), Some(TokenTree::Punct(greater)))
                if equals.as_char() == '='
                    && equals.spacing() == Spacing::Joint
                    && greater.as_char() == '>'
        );
        // A `,` before `>` ends a generic argument list, not an expression.
        let closes_generics = matches!(
            tokens.get(index + 1),
            Some(TokenTree::Punct(next)) if next.as_char() == '>'
        );
        let boundary = arrow
            || match &tokens[index] {
                TokenTree::Punct(punct) => match punct.as_char() {
                    ',' => !closes_generics,
                    ';' => true,
                    _ => false,
                },
                TokenTree::Ident(ident) => ident == "if",
                _ => false,
            };
        if boundary {
            pieces.push(Vec::new());
            index += 1 + usize::from(arrow);
        } else {
            if let Some(piece) = pieces.last_mut() {
                piece.push(tokens[index].clone());
            }
            index += 1;
        }
    }
    for piece in pieces {
        if piece.is_empty() {
            continue;
        }
        if let Ok(expression) = syn::parse2::<Expr>(piece.iter().cloned().collect()) {
            parsed.push(expression);
            continue;
        }
        let colon = |token: Option<&TokenTree>| matches!(token, Some(TokenTree::Punct(punct)) if punct.as_char() == ':');
        let mut keyed = vec![Vec::new()];
        for (index, token) in piece.iter().enumerate() {
            let lone = colon(Some(token))
                && !colon(piece.get(index + 1))
                && !(index > 0 && colon(piece.get(index - 1)));
            if lone {
                keyed.push(Vec::new());
            } else if let Some(part) = keyed.last_mut() {
                part.push(token.clone());
            }
        }
        for part in keyed {
            match syn::parse2::<Expr>(part.iter().cloned().collect()) {
                Ok(expression) => parsed.push(expression),
                Err(_) => {
                    for token in part {
                        match token {
                            TokenTree::Group(group) => {
                                macro_expressions(group.stream(), parsed, loose)
                            }
                            other => loose.push(other),
                        }
                    }
                }
            }
        }
    }
}

/// Error types by the file that declares them: a `struct` or `enum` that
/// derives `Error` or has an `impl Error for` it in the same file.
#[derive(Default)]
struct DeclaredErrors {
    /// Declared with any `pub`, by file.
    exported: BTreeMap<String, BTreeSet<String>>,
    /// Every declared error type's name, private ones included.
    names: BTreeSet<String>,
    /// Every type name a workspace source declares, test code included.
    workspace: BTreeSet<String>,
    /// `std`, `core`, `alloc` and the crates the workspace depends on.
    external: BTreeSet<String>,
}

#[derive(Default)]
struct TypeNames(BTreeSet<String>);

impl<'ast> Visit<'ast> for TypeNames {
    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.0.insert(item.ident.to_string());
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.0.insert(item.ident.to_string());
    }

    fn visit_item_union(&mut self, item: &'ast syn::ItemUnion) {
        self.0.insert(item.ident.to_string());
    }

    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        self.0.insert(item.ident.to_string());
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        self.0.insert(item.ident.to_string());
        visit::visit_item_trait(self, item);
    }
}

/// The crate names a path may start at that are not this workspace's.
fn external_crates(root: &Path) -> BTreeSet<String> {
    let workspace = toml_file(&root.join("Cargo.toml"));
    let mut tables = vec![workspace["workspace"].get("dependencies").cloned()];
    for member in workspace["workspace"]["members"]
        .as_array()
        .expect("workspace members")
    {
        let manifest = toml_file(
            &root
                .join(member.as_str().expect("member path"))
                .join("Cargo.toml"),
        );
        tables.push(manifest.get("dependencies").cloned());
        if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
            tables.extend(
                targets
                    .values()
                    .map(|target| target.get("dependencies").cloned()),
            );
        }
    }
    tables
        .into_iter()
        .flatten()
        .filter_map(|table| table.as_table().cloned())
        .flat_map(|table| table.into_iter().map(|(key, _)| key.replace('-', "_")))
        .chain(["std", "core", "alloc"].map(str::to_string))
        .filter(|name| !name.starts_with("gents"))
        .collect()
}

#[derive(Default)]
struct ErrorDeclarations {
    /// Name, exported, derives `Error`.
    types: Vec<(String, bool, bool)>,
    implemented: BTreeSet<String>,
}

fn derives_error(attributes: &[syn::Attribute]) -> bool {
    attributes
        .iter()
        .filter(|attribute| attribute.path().is_ident("derive"))
        .any(|attribute| {
            let mut found = false;
            let _ = attribute.parse_nested_meta(|meta| {
                found |= meta
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "Error");
                Ok(())
            });
            found
        })
}

impl<'ast> Visit<'ast> for ErrorDeclarations {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
        self.types.push((
            item.ident.to_string(),
            !matches!(item.vis, syn::Visibility::Inherited),
            derives_error(&item.attrs),
        ));
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        self.types.push((
            item.ident.to_string(),
            !matches!(item.vis, syn::Visibility::Inherited),
            derives_error(&item.attrs),
        ));
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        if let Some((_, path, _)) = &item.trait_ {
            if path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "Error")
            {
                self.implemented.insert(type_name(&item.self_ty));
            }
        }
        visit::visit_item_impl(self, item);
    }
}

fn crate_of(path: &str) -> &str {
    let end = path
        .match_indices('/')
        .nth(1)
        .map_or(path.len(), |(index, _)| index);
    &path[..end]
}

fn in_owner_module(path: &str) -> bool {
    OWNER_MODULES.iter().any(|owner| owned_by(path, owner))
}

impl DeclaredErrors {
    fn of(sources: &[(String, syn::File)], external: &BTreeSet<String>) -> Self {
        let mut declared = Self {
            external: external.clone(),
            ..Self::default()
        };
        for (path, syntax) in sources {
            let mut names = TypeNames::default();
            names.visit_file(syntax);
            declared.workspace.extend(names.0);
            let mut declarations = ErrorDeclarations::default();
            declarations.visit_file(syntax);
            for (name, exported, derives) in declarations.types {
                if !derives && !declarations.implemented.contains(&name) {
                    continue;
                }
                declared.names.insert(name.clone());
                if exported {
                    declared
                        .exported
                        .entry(path.clone())
                        .or_default()
                        .insert(name);
                }
            }
        }
        declared
    }

    /// The error types `path` owns: those its file exports, and for a file
    /// in `OWNER_MODULES` those the `OWNER_MODULES` files of its crate export.
    fn own(&self, path: &str) -> BTreeSet<String> {
        let owner = in_owner_module(path);
        self.exported
            .iter()
            .filter(|(file, _)| {
                file.as_str() == path
                    || (owner && crate_of(file) == crate_of(path) && in_owner_module(file))
            })
            .flat_map(|(_, names)| names.iter().cloned())
            .collect()
    }
}

/// The sites of one file: those that count, described for the failure
/// message, and the functions that hold a compliant one.
#[derive(Default)]
struct SiteFindings {
    debt: Vec<String>,
    compliant: BTreeMap<String, usize>,
}

struct ClassifierVisitor<'declared> {
    declared: &'declared DeclaredErrors,
    own: BTreeSet<String>,
    walker: bool,
    owners: Vec<String>,
    frames: Vec<FnFrame>,
    /// Bindings the function received as errors, whatever they are named.
    errors: Vec<String>,
    /// Bindings that hold an error's rendering.
    renderings: Vec<String>,
    downcasts: SiteFindings,
    texts: SiteFindings,
}

impl ClassifierVisitor<'_> {
    fn function_name(&self) -> String {
        self.frames
            .last()
            .map_or_else(|| "(no function)".to_string(), |frame| frame.name.clone())
    }

    /// The type a downcast asks for, with `Self` read as the `impl` type.
    fn asked_name(&self, asked: &syn::Type) -> String {
        match type_name(asked).as_str() {
            "Self" => self.owners.last().cloned().unwrap_or_default(),
            name => name.to_string(),
        }
    }

    /// Whether the owner is asking for its own type. A path into another
    /// workspace crate is never the owner's, whatever its last segment.
    fn owns(&self, asked: &syn::Type, frame: &FnFrame) -> bool {
        let path = match asked {
            syn::Type::Path(path) if path.qself.is_none() => &path.path,
            _ => return false,
        };
        let single = path.segments.len() == 1;
        let foreign = !single
            && path
                .segments
                .first()
                .is_some_and(|segment| segment.ident.to_string().starts_with("gents_"));
        if foreign || !frame.asks_error {
            return false;
        }
        let name = self.asked_name(asked);
        self.own.contains(&name) || (self.walker && single && frame.type_parameters.contains(&name))
    }

    /// Whether the type asked for is a third-party one, which no owner here
    /// can classify. A workspace type never is: not by a `crate`, `self`,
    /// `super` or `gents_*` path, not as `Self` or a type parameter, and not
    /// by a name a workspace source declares unless its path starts at an
    /// external crate.
    fn third_party(&self, asked: &syn::Type, frame: &FnFrame) -> bool {
        let path = match asked {
            syn::Type::Path(path) if path.qself.is_none() => &path.path,
            _ => return false,
        };
        let (Some(first), Some(last)) = (path.segments.first(), path.segments.last()) else {
            return false;
        };
        let (first, last) = (first.ident.to_string(), last.ident.to_string());
        let single = path.segments.len() == 1;
        if matches!(first.as_str(), "crate" | "self" | "super" | "Self")
            || first.starts_with("gents_")
            || (single && frame.type_parameters.contains(&last))
        {
            return false;
        }
        (!single && self.declared.external.contains(&first))
            || !self.declared.workspace.contains(&last)
    }

    fn downcast_site(&mut self, line: usize, asked: Option<&syn::Type>) {
        let asked = asked.map(unboxed);
        let function = self.function_name();
        let (owner, adapter) = match (self.frames.last(), asked) {
            (Some(frame), Some(asked)) => (
                self.owns(asked, frame),
                frame.premise && self.third_party(asked, frame),
            ),
            _ => (false, false),
        };
        if owner {
            *self.downcasts.compliant.entry(function).or_default() += 1;
        } else if adapter {
            *self.texts.compliant.entry(function).or_default() += 1;
        } else {
            let asked = asked.map_or_else(|| "an inferred type".to_string(), type_name);
            self.downcasts
                .debt
                .push(format!("{line}: {asked} in {function}"));
        }
    }

    fn text_site(&mut self, line: usize) {
        let function = self.function_name();
        if self.frames.last().is_some_and(|frame| frame.premise) {
            *self.texts.compliant.entry(function).or_default() += 1;
        } else {
            self.texts.debt.push(format!("{line}: in {function}"));
        }
    }

    /// A downcast asks about an error when its receiver is one or the type
    /// it asks for is an error type, so renaming the receiver hides nothing.
    fn asks_error_type(&self, asked: Option<&syn::Type>) -> bool {
        asked.is_some_and(|asked| {
            let name = self.asked_name(unboxed(asked));
            name.ends_with("Error") || self.declared.names.contains(&name)
        })
    }

    /// Text derived from an error: its `to_string()` or `format!` rendering,
    /// a local bound to one, or an identifier named as error text.
    fn error_text(&self, expression: &Expr, named: bool) -> bool {
        match expression {
            Expr::Reference(reference) => self.error_text(&reference.expr, named),
            Expr::Paren(paren) => self.error_text(&paren.expr, named),
            Expr::Unary(unary) => self.error_text(&unary.expr, named),
            Expr::Macro(invocation) => macro_renders_error(&invocation.mac, &self.errors),
            Expr::Path(path) => path.path.get_ident().is_some_and(|ident| {
                let ident = ident.to_string();
                self.renderings.contains(&ident) || (named && names_error(&ident))
            }),
            Expr::Field(_) => named && expression_is_error(expression, &[]),
            Expr::MethodCall(call) => {
                let method = call.method.to_string();
                if TEXT_ADAPTERS.contains(&method.as_str()) {
                    self.error_text(&call.receiver, named)
                } else {
                    method == "to_string" && expression_is_error(&call.receiver, &self.errors)
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

    fn function(
        &mut self,
        attributes: &[syn::Attribute],
        signature: &syn::Signature,
        visit: impl FnOnce(&mut Self),
    ) {
        if is_test_fn(attributes) {
            return;
        }
        let (errors, renderings) = (self.errors.len(), self.renderings.len());
        self.frames
            .push(fn_frame(self.owners.last(), attributes, signature));
        for input in &signature.inputs {
            if let syn::FnArg::Typed(typed) = input {
                if names_error_type(&typed.ty) {
                    pattern_names(&typed.pat, &mut self.errors);
                }
            }
        }
        self.classifier_parameters(signature);
        visit(self);
        self.frames.pop();
        self.errors.truncate(errors);
        self.renderings.truncate(renderings);
    }

    fn owned(&mut self, owner: String, visit: impl FnOnce(&mut Self)) {
        self.owners.push(owner);
        visit(self);
        self.owners.pop();
    }
}

impl<'ast> Visit<'ast> for ClassifierVisitor<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        self.owned(type_name(&item.self_ty), |visitor| {
            visit::visit_item_impl(visitor, item)
        });
    }

    fn visit_item_trait(&mut self, item: &'ast syn::ItemTrait) {
        self.owned(item.ident.to_string(), |visitor| {
            visit::visit_item_trait(visitor, item)
        });
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.function(&item.attrs, &item.sig, |visitor| {
            visit::visit_item_fn(visitor, item)
        });
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.function(&item.attrs, &item.sig, |visitor| {
            visit::visit_impl_item_fn(visitor, item)
        });
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        self.function(&item.attrs, &item.sig, |visitor| {
            visit::visit_trait_item_fn(visitor, item)
        });
    }

    /// The payload of an `Err(..)` pattern is an error under any name.
    fn visit_pat_tuple_struct(&mut self, pattern: &'ast syn::PatTupleStruct) {
        if pattern
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "Err")
        {
            for inner in &pattern.elems {
                pattern_names(inner, &mut self.errors);
            }
        }
        visit::visit_pat_tuple_struct(self, pattern);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let mut parsed = Vec::new();
        let mut loose = Vec::new();
        macro_expressions(invocation.tokens.clone(), &mut parsed, &mut loose);
        for expression in &parsed {
            Visit::visit_expr(self, expression);
        }
        for token in loose {
            if let proc_macro2::TokenTree::Ident(ident) = token {
                if matches!(
                    ident.to_string().as_str(),
                    "downcast" | "downcast_ref" | "downcast_mut" | "find_source" | "as_owner"
                ) {
                    self.downcast_site(ident.span().start().line, None);
                }
            }
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

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            if let Some(segment) = function.path.segments.last() {
                if segment.ident == "find_source" || segment.ident == "as_owner" {
                    let asked = match &segment.arguments {
                        syn::PathArguments::AngleBracketed(arguments) => first_type(arguments),
                        _ => None,
                    };
                    self.downcast_site(segment.ident.span().start().line, asked);
                }
            }
        }
        visit::visit_expr_call(self, call);
    }

    /// Only an error classifies a failure; `Any` maps, deferred write
    /// payloads and panic payloads downcast data, not errors.
    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        let method = call.method.to_string();
        let line = call.method.span().start().line;
        let asked = call.turbofish.as_ref().and_then(first_type);
        let downcast = matches!(
            method.as_str(),
            "downcast" | "downcast_ref" | "downcast_mut"
        ) || (method == "is" && call.turbofish.is_some());
        if downcast
            && (expression_is_error(&call.receiver, &self.errors) || self.asks_error_type(asked))
        {
            self.downcast_site(line, asked);
        }
        if TEXT_TESTS.contains(&method.as_str()) && self.error_text(&call.receiver, true) {
            self.text_site(line);
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_binary(&mut self, binary: &'ast syn::ExprBinary) {
        // Comparing two observed values is identity, not classification; a
        // decision compares error text with a fixed wording.
        let text_literal = |literal: &syn::ExprLit| {
            matches!(
                literal.lit,
                syn::Lit::Str(_) | syn::Lit::ByteStr(_) | syn::Lit::Char(_) | syn::Lit::Byte(_)
            )
        };
        let wording = |expression: &Expr| match expression {
            Expr::Lit(literal) => text_literal(literal),
            Expr::Macro(_) => true,
            Expr::Reference(reference) => {
                matches!(reference.expr.as_ref(), Expr::Lit(literal) if text_literal(literal))
            }
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
            self.text_site(binary.op.span().start().line);
        }
        visit::visit_expr_binary(self, binary);
    }
}

/// The downcast sites and the error-text decisions of one file.
fn classifier_sites(
    path: &str,
    syntax: &syn::File,
    declared: &DeclaredErrors,
) -> (SiteFindings, SiteFindings) {
    let mut visitor = ClassifierVisitor {
        declared,
        own: declared.own(path),
        walker: path == CHAIN_WALKER,
        owners: Vec::new(),
        frames: Vec::new(),
        errors: Vec::new(),
        renderings: Vec::new(),
        downcasts: SiteFindings::default(),
        texts: SiteFindings::default(),
    };
    visitor.visit_file(syntax);
    (visitor.downcasts, visitor.texts)
}

/// What a classifier ratchet found: the sites that still count, by file, and
/// the functions whose sites are the compliant form.
#[derive(Default)]
struct ClassifierFindings {
    debt: BTreeMap<String, Vec<String>>,
    compliant: BTreeMap<(String, String), usize>,
}

impl ClassifierFindings {
    fn record(&mut self, path: &str, sites: SiteFindings) {
        if !sites.debt.is_empty() {
            self.debt.insert(path.to_string(), sites.debt);
        }
        self.compliant.extend(
            sites
                .compliant
                .into_iter()
                .map(|(function, sites)| ((path.to_string(), function), sites)),
        );
    }

    fn sites(&self) -> usize {
        self.debt.values().map(Vec::len).sum()
    }

    fn functions(&self) -> Vec<&str> {
        self.compliant
            .keys()
            .map(|(_, function)| function.as_str())
            .collect()
    }

    /// The counted sites must equal the allowlist, which only shrinks, and the
    /// compliant functions must equal the registry, which is reviewed.
    fn failures(
        &self,
        allowlist: &[(&str, usize)],
        registry: &[(&str, &[&str])],
        advice: &str,
    ) -> Vec<String> {
        let allowed = allowlist
            .iter()
            .map(|(path, count)| (path.to_string(), *count))
            .collect::<BTreeMap<_, _>>();
        let mut failures = Vec::new();
        for path in allowed
            .keys()
            .chain(self.debt.keys())
            .collect::<BTreeSet<_>>()
        {
            let sites = self.debt.get(path).cloned().unwrap_or_default();
            let found = sites.len();
            let listed = sites.join("\n    ");
            match allowed.get(path).copied() {
                None => failures.push(format!(
                    "{path}: {found} new site(s); {advice}\n    {listed}"
                )),
                Some(limit) if found > limit => failures.push(format!(
                    "{path}: {found} sites exceed the allowlisted {limit}; {advice}\n    {listed}"
                )),
                Some(limit) if found < limit => failures.push(format!(
                    "{path}: {found} sites; lower the allowlist entry from {limit}"
                )),
                Some(_) => {}
            }
        }
        let registered = registry
            .iter()
            .flat_map(|(path, functions)| {
                functions
                    .iter()
                    .map(|function| (path.to_string(), function.to_string()))
            })
            .collect::<BTreeSet<_>>();
        let compliant = self.compliant.keys().cloned().collect::<BTreeSet<_>>();
        for (path, function) in compliant.difference(&registered) {
            failures.push(format!(
                "{path}: `{function}` is a compliant classifier that is not registered; add it to the registry for review"
            ));
        }
        for (path, function) in registered.difference(&compliant) {
            failures.push(format!(
                "{path}: `{function}` is registered but holds no compliant site; remove it from the registry"
            ));
        }
        failures
    }

    fn current(&self) -> String {
        let debt = self
            .debt
            .iter()
            .map(|(path, sites)| {
                let what = sites
                    .iter()
                    .map(|site| {
                        site.split_once(": ")
                            .map_or(site.as_str(), |(_, what)| what)
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join("; ");
                format!("    // {what}\n    (\"{path}\", {}),", sites.len())
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut registry = BTreeMap::<&str, Vec<&str>>::new();
        for (path, function) in self.compliant.keys() {
            registry.entry(path).or_default().push(function);
        }
        let registry = registry
            .iter()
            .map(|(path, functions)| format!("    (\"{path}\", &{functions:?}),"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "current allowlist ({} sites in {} files):\n{debt}\n\ncurrent registry ({} sites in {} functions):\n{registry}",
            self.sites(),
            self.debt.len(),
            self.compliant.values().sum::<usize>(),
            self.compliant.len()
        )
    }
}

fn classifier_findings(
    sources: &[(String, syn::File)],
    external: &BTreeSet<String>,
) -> (ClassifierFindings, ClassifierFindings) {
    let declared = DeclaredErrors::of(sources, external);
    let mut downcasts = ClassifierFindings::default();
    let mut texts = ClassifierFindings::default();
    for (path, syntax) in sources {
        let (downcast, text) = classifier_sites(path, syntax, &declared);
        downcasts.record(path, downcast);
        texts.record(path, text);
    }
    (downcasts, texts)
}

fn production_classifier_findings() -> (ClassifierFindings, ClassifierFindings) {
    let root = repo_root();
    classifier_findings(
        &parse_production(&root, production_sources(&root)),
        &external_crates(&root),
    )
}

#[test]
fn downcast_classifiers_only_shrink() {
    let (downcasts, _) = production_classifier_findings();
    let failures = downcasts.failures(
        DOWNCAST_CLASSIFIER_ALLOWLIST,
        OWNER_CLASSIFIER_REGISTRY,
        "return an owner enum and match its variants, or ask the owner's classifier",
    );
    assert!(
        failures.is_empty(),
        "downcast classifier ratchet failed:\n{}\n\n{}",
        failures.join("\n"),
        downcasts.current()
    );
}

#[test]
fn error_text_classification_only_shrinks() {
    let (_, texts) = production_classifier_findings();
    let failures = texts.failures(
        ERROR_TEXT_CLASSIFIER_ALLOWLIST,
        ADAPTER_CLASSIFIER_REGISTRY,
        "expose the fact as an owner enum variant and match it, or decide in an adapter classifier whose docstring states its `External premise:`",
    );
    assert!(
        failures.is_empty(),
        "error text ratchet failed:\n{}\n\n{}",
        failures.join("\n"),
        texts.current()
    );
}

/// A file that is no owner module, and two owner modules of one crate.
const APPLICATION_FILE: &str = "crates/gents-cli/src/snippet.rs";
const OWNER_FILE: &str = "crates/gents-protocol/src/snippet.rs";
const OWNER_SIBLING: &str = "crates/gents-protocol/src/sibling.rs";
const OTHER_OWNER: &str = "crates/gents-loop/src/snippet.rs";

fn snippet_findings(files: &[(&str, &str)]) -> (ClassifierFindings, ClassifierFindings) {
    let sources = files
        .iter()
        .map(|(path, source)| {
            let syntax = syn::parse_file(source)
                .unwrap_or_else(|error| panic!("parse snippet {source}: {error}"));
            (path.to_string(), syntax)
        })
        .collect::<Vec<_>>();
    let external: BTreeSet<String> = ["std", "serde_json"].map(str::to_string).into();
    classifier_findings(&sources, &external)
}

fn downcast_sites(source: &str) -> usize {
    snippet_findings(&[(APPLICATION_FILE, source)]).0.sites()
}

fn error_text_sites(source: &str) -> usize {
    snippet_findings(&[(APPLICATION_FILE, source)]).1.sites()
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
        // Renaming the receiver hides nothing: an error-typed parameter, an
        // `Err(..)` payload and a downcast to an error type all count.
        ("fn f(value: &anyhow::Error) -> bool { value.downcast_ref::<E>().is_some() }", 1),
        ("fn f(r: Result<(), anyhow::Error>) { if let Err(value) = r { value.is::<E>(); } }", 1),
        ("fn f(x: &X) -> bool { x.inner.downcast_ref::<ConflictError>().is_some() }", 1),
        ("#[derive(Debug, thiserror::Error)] struct Marker; fn f(x: &X) -> bool { x.inner.is::<Marker>() }", 1),
        ("fn f(value: &anyhow::Error) -> Option<&E> { value.downcast_ref() }", 1),
        // Nor does a macro.
        ("fn f(e: &anyhow::Error) -> bool { matches!(e.downcast_ref::<E>(), Some(_)) }", 1),
        ("fn f(r: R) -> bool { matches!(r, Err(e) if e.is::<E>()) }", 1),
        ("fn f(e: &anyhow::Error) { assert!(e.downcast_ref::<E>().is_some(), \"typed\"); }", 1),
        ("async fn f() { tokio::select! { r = a() => { if let Err(e) = r { e.is::<E>(); } } _ = b() => {} } }", 1),
        ("macro_rules! typed { ($e:expr) => { $e.downcast_ref::<E>() }; }", 1),
        ("fn f(e: &anyhow::Error) -> Value { json!({ \"code\": find_source::<E>(e.as_ref()).map(E::code) }) }", 1),
        ("fn f(e: &anyhow::Error) -> Value { json!({ \"a\": 1, \"code\": find_source::<E,>(e.as_ref()).map(E::code) }) }", 1),
    ] {
        assert_eq!(downcast_sites(source), sites, "{source}");
    }
}

const OWN_ERROR: &str =
    "#[derive(Debug, thiserror::Error)] pub enum OwnError { #[error(\"x\")] X }";

#[test]
fn downcast_fence_exempts_only_the_owner_asking_for_its_own_type() {
    let classifier = "pub fn own_error(error: &(dyn std::error::Error + 'static)) -> Option<&OwnError> { find_map_source(error, |cause| cause.downcast_ref::<OwnError>()) }";
    let declared_here = format!("{OWN_ERROR} {classifier}");
    let check = |files: &[(&str, &str)]| {
        let (downcasts, _) = snippet_findings(files);
        (
            downcasts.sites(),
            downcasts
                .functions()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        )
    };
    let compliant = |function: &str| (0, vec![function.to_string()]);
    let counted = |sites: usize| (sites, Vec::new());

    // The module that declares and exports the type, owner module or not.
    assert_eq!(
        check(&[(OWNER_FILE, declared_here.as_str())]),
        compliant("own_error")
    );
    assert_eq!(
        check(&[(APPLICATION_FILE, declared_here.as_str())]),
        compliant("own_error")
    );
    // Another owner module of the same crate.
    assert_eq!(
        check(&[(OWNER_FILE, OWN_ERROR), (OWNER_SIBLING, classifier)]),
        compliant("own_error")
    );
    // The same downcast outside the owner: an application file, a file of the
    // same crate that is no owner module, and an owner module of another crate.
    assert_eq!(
        check(&[(OWNER_FILE, OWN_ERROR), (APPLICATION_FILE, classifier)]),
        counted(1)
    );
    assert_eq!(
        check(&[
            (APPLICATION_FILE, OWN_ERROR),
            ("crates/gents-cli/src/other.rs", classifier)
        ]),
        counted(1)
    );
    assert_eq!(
        check(&[(OWNER_FILE, OWN_ERROR), (OTHER_OWNER, classifier)]),
        counted(1)
    );
    let named_crate = "pub fn own_error(error: &anyhow::Error) -> bool { error.is::<gents_protocol::snippet::OwnError>() }";
    let named_crate = format!("{OWN_ERROR} {named_crate}");
    assert_eq!(check(&[(OTHER_OWNER, named_crate.as_str())]), counted(1));

    for (source, expected) in [
        // A method asking for `Self`.
        (
            "#[derive(Debug, thiserror::Error)] pub enum OwnError { #[error(\"x\")] X } impl OwnError { pub fn recover(error: anyhow::Error) -> Self { error.downcast::<Self>().unwrap_or(Self::X) } }",
            compliant("OwnError::recover"),
        ),
        // A hand-written `Error` impl declares an error type too.
        (
            "#[derive(Debug)] pub struct Refused; impl std::error::Error for Refused {} pub fn refused(error: &anyhow::Error) -> bool { error.is::<Refused>() }",
            compliant("refused"),
        ),
        // A private marker carried through `anyhow` is debt wherever it is asked for.
        (
            "#[derive(Debug, thiserror::Error)] struct Marker; fn marked(error: &anyhow::Error) -> bool { error.is::<Marker>() }",
            counted(1),
        ),
        // A function that is not handed an error is not a classifier.
        (
            "#[derive(Debug, thiserror::Error)] pub struct OwnError; fn run(job: &Job) -> bool { match job.run() { Err(error) => error.is::<OwnError>(), Ok(()) => false } }",
            counted(1),
        ),
        // Nor is an asynchronous one.
        (
            "#[derive(Debug, thiserror::Error)] pub struct OwnError; async fn settle(error: &anyhow::Error) -> bool { error.is::<OwnError>() }",
            counted(1),
        ),
        // A type the module only imports, an inferred type, a third-party type.
        (
            "use other::OwnError; fn own_error(error: &anyhow::Error) -> bool { error.is::<OwnError>() }",
            counted(1),
        ),
        (
            "#[derive(Debug, thiserror::Error)] pub struct OwnError; fn own_error(error: &anyhow::Error) -> Option<&OwnError> { error.downcast_ref() }",
            counted(1),
        ),
        (
            "fn io(error: &anyhow::Error) -> bool { error.is::<std::io::Error>() }",
            counted(1),
        ),
        // A generic helper hides the type it asks for.
        (
            "fn find<T>(error: &(dyn Error + 'static)) -> Option<&T> where T: Error + 'static { error.downcast_ref::<T>() }",
            counted(1),
        ),
        // Wrapping the compliant form in a macro changes nothing either way.
        (
            "#[derive(Debug, thiserror::Error)] pub struct OwnError; fn own_error(error: &anyhow::Error) -> bool { matches!(error.downcast_ref::<OwnError>(), Some(_)) }",
            compliant("own_error"),
        ),
    ] {
        assert_eq!(check(&[(APPLICATION_FILE, source)]), expected, "{source}");
    }

    // Asking for another module's type beside its own counts that site alone.
    let mixed = "#[derive(Debug, thiserror::Error)] pub struct OwnError; fn own_error(error: &anyhow::Error) -> bool { error.is::<OwnError>() || error.is::<other::TheirError>() }";
    assert_eq!(
        check(&[(APPLICATION_FILE, mixed)]),
        (1, vec!["own_error".to_string()])
    );

    // Only the chain walker may ask for its own type parameter.
    let walker = "pub fn find_source<'a, T>(error: &'a (dyn Error + 'static)) -> Option<&'a T> where T: Error + 'static { find_map_source(error, |cause| cause.downcast_ref::<T>()) }";
    assert_eq!(check(&[(CHAIN_WALKER, walker)]), compliant("find_source"));
    assert_eq!(check(&[(OWNER_FILE, walker)]), counted(1));
}

#[test]
fn downcast_fence_exempts_third_party_types_only_in_adapter_classifiers() {
    const PREMISE: &str =
        "/// External premise: serde reports a decode failure only as its own error type.\n";
    let check = |files: &[(&str, &str)]| {
        let (downcasts, adapters) = snippet_findings(files);
        (
            downcasts.sites(),
            adapters
                .functions()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        )
    };
    let adapter = |function: &str| (0, vec![function.to_string()]);
    let counted = (1, Vec::new());
    let documented = |body: &str| format!("{PREMISE}{body}");

    for (body, expected) in [
        // A type no workspace source declares, by an external crate's path
        // or by an imported name, in a function of any shape.
        (
            "fn decode_failed(error: &anyhow::Error) -> bool { error.chain().any(|cause| cause.is::<serde_json::Error>()) }",
            adapter("decode_failed"),
        ),
        (
            "fn missing(error: &anyhow::Error) -> bool { find_source::<std::io::Error>(error.as_ref()).is_some() }",
            adapter("missing"),
        ),
        (
            "fn refused(error: &Init) -> bool { error.inner.downcast_ref::<StreamableHttpError<C::Error>>().is_some() }",
            adapter("refused"),
        ),
        // A workspace type in an adapter still counts: by its declaration,
        // by a workspace path, as `Self`, and as a type parameter.
        (
            "#[derive(Debug, thiserror::Error)] struct Marker; fn marked(error: &anyhow::Error) -> bool { error.is::<Marker>() }",
            counted.clone(),
        ),
        (
            "fn theirs(error: &anyhow::Error) -> bool { error.is::<gents_store::StoreError>() }",
            counted.clone(),
        ),
        (
            "fn ours(error: &anyhow::Error) -> bool { error.is::<crate::other::OtherError>() }",
            counted.clone(),
        ),
        (
            "fn any<E>(error: &anyhow::Error) -> bool where E: std::error::Error + 'static { error.is::<E>() }",
            counted.clone(),
        ),
        (
            "fn inferred(error: &anyhow::Error) -> Option<&std::io::Error> { error.downcast_ref() }",
            counted.clone(),
        ),
    ] {
        let source = documented(body);
        assert_eq!(check(&[(APPLICATION_FILE, source.as_str())]), expected, "{body}");
        // The same downcast in a function without the premise counts.
        assert_eq!(check(&[(APPLICATION_FILE, body)]), counted.clone(), "{body}");
    }

    // A name another workspace file declares is a workspace type, even in
    // test code, unless the path starts at an external crate.
    let declares = "#[cfg(test)] mod tests { pub struct Error; }";
    let short = documented("fn io(error: &anyhow::Error) -> bool { error.is::<io::Error>() }");
    let full = documented("fn io(error: &anyhow::Error) -> bool { error.is::<std::io::Error>() }");
    assert_eq!(
        check(&[(OWNER_FILE, declares), (APPLICATION_FILE, short.as_str())]),
        counted.clone()
    );
    assert_eq!(
        check(&[(OWNER_FILE, declares), (APPLICATION_FILE, full.as_str())]),
        adapter("io")
    );

    // The adapter is registered like any other: undocumented it is debt,
    // documented and unregistered it fails the registry check.
    let (downcasts, adapters) = snippet_findings(&[(APPLICATION_FILE, full.as_str())]);
    assert!(downcasts.failures(&[], &[], "advice").is_empty());
    assert_eq!(adapters.failures(&[], &[], "advice").len(), 1);
    assert!(adapters
        .failures(&[], &[(APPLICATION_FILE, &["io"])], "advice")
        .is_empty());
}

#[test]
fn classifier_ratchet_checks_the_allowlist_and_the_registry() {
    let source = format!(
        "{OWN_ERROR} fn own_error(error: &anyhow::Error) -> bool {{ error.is::<OwnError>() }} fn other(error: &anyhow::Error) -> bool {{ error.is::<std::io::Error>() }}"
    );
    let (downcasts, _) = snippet_findings(&[(APPLICATION_FILE, source.as_str())]);
    let failures = |allowlist: &[(&str, usize)], registry: &[(&str, &[&str])]| {
        downcasts.failures(allowlist, registry, "advice").len()
    };
    assert_eq!(
        failures(
            &[(APPLICATION_FILE, 1)],
            &[(APPLICATION_FILE, &["own_error"])]
        ),
        0
    );
    // An unlisted site, an entry that is too high or too low.
    assert_eq!(failures(&[], &[(APPLICATION_FILE, &["own_error"])]), 1);
    assert_eq!(
        failures(
            &[(APPLICATION_FILE, 2)],
            &[(APPLICATION_FILE, &["own_error"])]
        ),
        1
    );
    assert_eq!(
        failures(
            &[(APPLICATION_FILE, 0)],
            &[(APPLICATION_FILE, &["own_error"])]
        ),
        1
    );
    // A compliant classifier that is not registered, and a stale registration.
    assert_eq!(failures(&[(APPLICATION_FILE, 1)], &[]), 1);
    assert_eq!(
        failures(
            &[(APPLICATION_FILE, 1)],
            &[(APPLICATION_FILE, &["own_error", "other"])]
        ),
        1
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
            r#"fn f(activation_error_count: usize) -> bool { activation_error_count == 0 }"#,
            0,
        ),
        (r#"fn f(error: &str) -> bool { error == "unavailable" }"#, 1),
        (
            r#"fn f(error: &str) -> bool { error == &"unavailable" }"#,
            1,
        ),
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
        // Renaming the error hides nothing: an error-typed parameter and an
        // `Err(..)` payload render error text under any name.
        (
            r#"fn f(value: &anyhow::Error) -> bool { value.to_string().contains("x") }"#,
            1,
        ),
        (
            r#"fn f(r: R) -> bool { match r { Err(value) => value.to_string().contains("x"), Ok(()) => false } }"#,
            1,
        ),
        (
            r#"fn f(value: &anyhow::Error) -> bool { let text = format!("{value:#}"); text.contains("x") }"#,
            1,
        ),
        // Nor does a macro.
        (
            r#"fn f(e: &E) -> bool { matches!(e.to_string().contains("x"), true) }"#,
            1,
        ),
        (
            r#"fn f(e: &E) { assert!(e.to_string().contains("x"), "worded"); }"#,
            1,
        ),
        (
            r#"async fn f() { tokio::select! { r = a() => { if let Err(e) = r { let _ = e.to_string() == "x"; } } _ = b() => {} } }"#,
            1,
        ),
    ] {
        assert_eq!(error_text_sites(source), sites, "{source}");
    }
}

#[test]
fn error_text_fence_exempts_only_adapter_classifiers_that_name_their_premise() {
    let decision = r#"fn unknown_variant(error: &serde_json::Error) -> bool { error.to_string().contains("unknown variant") }"#;
    let check = |docstring: &str| {
        let source = format!("{docstring}\n{decision}");
        let (_, texts) = snippet_findings(&[(APPLICATION_FILE, source.as_str())]);
        (
            texts.sites(),
            texts
                .functions()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        )
    };
    let compliant = || (0, vec!["unknown_variant".to_string()]);
    let counted = || (1, Vec::new());

    assert_eq!(
        check(
            "/// External premise: `serde_json::Error` names an unknown tag only in its message."
        ),
        compliant()
    );
    assert_eq!(
        check("/// Whether the tag was unknown.\n///\n/// External premise: serde says so only in its message."),
        compliant()
    );
    // The same decision without the docstring, and with docstrings that do
    // not state the premise in the required form.
    assert_eq!(check(""), counted());
    assert_eq!(check("/// Whether serde met an unknown tag."), counted());
    assert_eq!(check("/// External premise:"), counted());
    assert_eq!(
        check("/// On the external premise that serde says so only in text."),
        counted()
    );
    assert_eq!(
        check("/// See the External premise: of the caller."),
        counted()
    );
    assert_eq!(
        check("// External premise: an ordinary comment is not a docstring."),
        counted()
    );

    for (source, expected) in [
        // A closure belongs to its function; a nested function and a caller do not.
        (
            "/// External premise: git says so only on stderr.\nfn missing(stderr: &str) -> bool { stderr.lines().any(|message| message.contains(\"exists on disk\")) }",
            (0, vec!["missing".to_string()]),
        ),
        (
            "/// External premise: git says so only on stderr.\nfn outer(stderr: &str) -> bool { fn inner(stderr: &str) -> bool { stderr.contains(\"x\") } inner(stderr) }",
            (1, Vec::new()),
        ),
        (
            "/// External premise: git says so only on stderr.\nfn missing(stderr: &str) -> bool { stderr.contains(\"x\") } fn caller(e: &E) -> bool { missing(&e.to_string()) || e.to_string().contains(\"y\") }",
            (1, vec!["missing".to_string()]),
        ),
        // The premise belongs to the function, not to its `impl`.
        (
            "/// External premise: git says so only on stderr.\nimpl Git { fn missing(stderr: &str) -> bool { stderr.contains(\"x\") } }",
            (1, Vec::new()),
        ),
        (
            "impl Git {\n/// External premise: git says so only on stderr.\nfn missing(stderr: &str) -> bool { stderr.contains(\"x\") } }",
            (0, vec!["Git::missing".to_string()]),
        ),
        // Wrapping the compliant form in a macro changes nothing either way.
        (
            "/// External premise: git says so only on stderr.\nfn missing(stderr: &str) -> bool { matches!(stderr.contains(\"x\"), true) }",
            (0, vec!["missing".to_string()]),
        ),
    ] {
        let (_, texts) = snippet_findings(&[(APPLICATION_FILE, source)]);
        let found = (
            texts.sites(),
            texts
                .functions()
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        );
        assert_eq!(found, expected, "{source}");
    }
}

/// Workspace crates by layer, bottom to top. A crate names only crates in layers
/// below its own in `[dependencies]`, `[build-dependencies]` and
/// `[dev-dependencies]`, so Cargo itself rejects an upward or peer reference in
/// source; this test fixes the direction the manifests may declare. Two crates
/// in one layer are peers and name neither. The Lean decoders are test-only
/// leaves that a lower crate may dev-depend on although they name `gents-protocol`.
const CRATE_LAYERS: &[&[&str]] = &[
    &["gents-schemas"],
    &["gents-protocol"],
    &["gents-lean-vocab"],
    &["gents-loop"],
    &["gents-store"],
    &["gents-documents"],
    &["gents-inference", "gents-p2p"],
    &["gents-pack"],
    &["gents-session"],
    &["gents-tools"],
    &["gents-toolcalls"],
    &["gents-automation"],
    &["gents-observe"],
    &["gents"],
];

const TEST_ONLY_DECODERS: &[&str] = &["gents-lean-vocab", "gents-lean-contract"];

/// Crates that operate or expose the runtime; no layered crate may name them.
const CLIENT_CRATES: &[&str] = &[
    "gents-cli",
    "gents-eval",
    "gents-desktop-core",
    "gents-desktop",
    "gents-desktop-bridge",
    "gents-client-host",
    "gents-media-host",
];

fn crate_layer(name: &str) -> Option<usize> {
    CRATE_LAYERS.iter().position(|layer| layer.contains(&name))
}

/// Package names a manifest declares in the given dependency table kinds,
/// including target-specific tables.
fn declared_dependencies(
    manifest: &toml::Table,
    inherited: &toml::Table,
    kind: &str,
) -> BTreeSet<String> {
    let mut tables = vec![manifest.get(kind)];
    if let Some(targets) = manifest.get("target").and_then(toml::Value::as_table) {
        tables.extend(targets.values().map(|target| target.get(kind)));
    }
    tables
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_table)
        .flat_map(|table| table.iter())
        .map(|(key, specification)| package_name(key, specification, inherited))
        .collect()
}

fn crate_dependency_violations(
    manifests: &BTreeMap<String, toml::Table>,
    inherited: &toml::Table,
) -> Vec<String> {
    let mut violations = Vec::new();
    for (name, manifest) in manifests {
        let Some(own) = crate_layer(name) else {
            continue;
        };
        for kind in ["dependencies", "build-dependencies", "dev-dependencies"] {
            for dependency in declared_dependencies(manifest, inherited, kind) {
                if dependency == *name {
                    continue;
                }
                if CLIENT_CRATES.contains(&dependency.as_str()) {
                    violations.push(format!("{name} [{kind}] names client crate {dependency}"));
                    continue;
                }
                if kind == "dev-dependencies" && TEST_ONLY_DECODERS.contains(&dependency.as_str()) {
                    continue;
                }
                if let Some(theirs) = crate_layer(&dependency) {
                    if theirs >= own {
                        violations.push(format!(
                            "{name} [{kind}] names {dependency}, which is not in a lower layer"
                        ));
                    }
                }
            }
        }
    }
    violations
}

#[test]
fn crate_dependencies_follow_the_layers() {
    let root = repo_root();
    let inherited = toml_file(&root.join("Cargo.toml"))["workspace"]
        .get("dependencies")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();
    let mut manifests = BTreeMap::new();
    for layer in CRATE_LAYERS {
        for name in *layer {
            let directory = if *name == "gents" { "gents" } else { name };
            manifests.insert(
                (*name).to_string(),
                toml_file(&root.join("crates").join(directory).join("Cargo.toml")),
            );
        }
    }
    let violations = crate_dependency_violations(&manifests, &inherited);
    assert!(
        violations.is_empty(),
        "crate layering ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn crate_layer_fence_flags_upward_peer_and_client_references() {
    let manifest = |dependencies: &str| -> toml::Table {
        format!("[package]\nname = \"x\"\n{dependencies}")
            .parse()
            .expect("parse snippet")
    };
    let inherited = toml::Table::new();
    let check = |name: &str, dependencies: &str| {
        crate_dependency_violations(
            &BTreeMap::from([(name.to_string(), manifest(dependencies))]),
            &inherited,
        )
        .len()
    };
    assert_eq!(
        check("gents-store", "[dependencies]\ngents-protocol = \"1\"\n"),
        0
    );
    assert_eq!(
        check("gents-store", "[dependencies]\ngents-documents = \"1\"\n"),
        1
    );
    assert_eq!(
        check("gents-inference", "[dependencies]\ngents-p2p = \"1\"\n"),
        1
    );
    assert_eq!(
        check(
            "gents-tools",
            "[dev-dependencies]\ngents-toolcalls = \"1\"\n"
        ),
        1
    );
    assert_eq!(
        check(
            "gents-store",
            "[dev-dependencies]\ngents-lean-vocab = \"1\"\n"
        ),
        0
    );
    assert_eq!(check("gents", "[dev-dependencies]\ngents = \"1\"\n"), 0);
    assert_eq!(
        check("gents-observe", "[dependencies]\ngents-cli = \"1\"\n"),
        1
    );
    assert_eq!(
        check(
            "gents-session",
            "[target.'cfg(unix)'.dependencies]\ngents-tools = \"1\"\n"
        ),
        1
    );
}

/// Errors a classifier finds on an erased error's `source()` chain: the access
/// owners, the GraphQL owners asked for retryability, and erased errors, whose
/// top may be either. A variant that holds one directly is sourced
/// (`AccessClassification.Chain.exposed`); the owners that answer for the
/// error they hold by type are exempt for it.
const SOURCED_HOLDINGS: &[&str] = &[
    "ConfigClientError",
    "GraphqlResponseError",
    "GraphqlError",
    "GraphqlFailure",
    "anyhow::Error",
    "dyn Error",
];

const OWNERS_ASKED_FOR_WHAT_THEY_HOLD: &[(&str, &str)] = &[
    ("ConfigClientError", "GraphqlResponseError"),
    ("GraphqlResponseError", "ConfigClientError"),
    ("GraphqlError", "GraphqlFailure"),
];

/// Each crate's `AccessClassification` family enumerating its variants that
/// hold an access error or the store's `ViewError`.
const ACCESS_HOLDER_FAMILIES: &[(&str, &str)] = &[
    (
        "gents-automation",
        "access_classification_automation_wrapped",
    ),
    (
        "gents-desktop-bridge",
        "access_classification_desktop_bridge_wrapped",
    ),
    ("gents-store", "access_classification_store_wrapped"),
    ("gents-inference", "access_classification_inference_wrapped"),
    ("gents-session", "access_classification_session_wrapped"),
    ("gents-p2p", "access_classification_p2p_wrapped"),
    ("gents-service", "access_classification_service_wrapped"),
    ("gents-tools", "access_classification_tools_wrapped"),
    ("gents-eval", "access_classification_eval_wrapped"),
    ("gents-toolcalls", "access_classification_toolcalls_wrapped"),
    (
        "gents-desktop-core",
        "access_classification_desktop_core_wrapped",
    ),
];

/// Each crate's family enumerating its variants, other than the GraphQL
/// owners', that hold a GraphQL error or failure.
const RETRY_HOLDER_FAMILIES: &[(&str, &str)] = &[
    ("gents-store", "access_classification_store_retry_wrapped"),
    (
        "gents-protocol",
        "access_classification_protocol_retry_wrapped",
    ),
];

#[derive(Default)]
struct ErrorHolderVisitor {
    holders: Vec<ErrorHolder>,
}

struct ErrorHolder {
    owner: String,
    variant: String,
    held: BTreeSet<&'static str>,
    transparent: bool,
}

impl<'ast> Visit<'ast> for ErrorHolderVisitor {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, _: &'ast ItemFn) {}

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        if !derives_error(&item.attrs) {
            return;
        }
        for variant in &item.variants {
            let held = held_errors(&variant.fields);
            if held.is_empty() {
                continue;
            }
            self.holders.push(ErrorHolder {
                owner: item.ident.to_string(),
                variant: variant.ident.to_string(),
                held,
                transparent: variant.attrs.iter().any(|attribute| {
                    attribute.path().is_ident("error")
                        && attribute
                            .meta
                            .require_list()
                            .is_ok_and(|list| list.tokens.to_string() == "transparent")
                }),
            });
        }
    }
}

fn held_errors(fields: &syn::Fields) -> BTreeSet<&'static str> {
    struct Finder(BTreeSet<&'static str>);
    impl<'ast> Visit<'ast> for Finder {
        fn visit_path(&mut self, path: &'ast syn::Path) {
            let segments = path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>();
            if let Some(last) = segments.last() {
                for name in [
                    "ConfigClientError",
                    "GraphqlResponseError",
                    "GraphqlError",
                    "GraphqlFailure",
                    "ViewError",
                ] {
                    if last == name {
                        self.0.insert(name);
                    }
                }
            }
            if segments.ends_with(&["anyhow".to_string(), "Error".to_string()]) {
                self.0.insert("anyhow::Error");
            }
            visit::visit_path(self, path);
        }

        fn visit_type_trait_object(&mut self, object: &'ast syn::TypeTraitObject) {
            if object.bounds.iter().any(|bound| {
                matches!(bound, syn::TypeParamBound::Trait(bound)
                    if bound.path.segments.last().is_some_and(|segment| segment.ident == "Error"))
            }) {
                self.0.insert("dyn Error");
            }
            visit::visit_type_trait_object(self, object);
        }
    }
    let mut finder = Finder(BTreeSet::new());
    for field in fields {
        finder.visit_type(&field.ty);
    }
    finder.0
}

/// `Conformance.wireName` of `<owner><variant>`.
fn holder_wire_name(owner: &str, variant: &str) -> String {
    let characters = owner.chars().chain(variant.chars()).collect::<Vec<_>>();
    let mut wire = String::new();
    for (index, character) in characters.iter().enumerate() {
        let previous = index.checked_sub(1).map(|index| characters[index]);
        let next = characters.get(index + 1);
        let starts_word = character.is_ascii_uppercase()
            && previous.is_some_and(|previous| {
                previous.is_ascii_lowercase()
                    || previous.is_ascii_digit()
                    || (previous.is_ascii_uppercase() && next.is_some_and(char::is_ascii_lowercase))
            });
        if starts_word {
            wire.push('_');
        }
        wire.push(character.to_ascii_lowercase());
    }
    wire
}

fn modeled_wrappers(families: &serde_json::Value, key: &str) -> BTreeSet<String> {
    modeled_field(families, key, "wrapper")
}

fn modeled_field(families: &serde_json::Value, key: &str, name: &str) -> BTreeSet<String> {
    families["functions"][key]["input"]["fields"]
        .as_array()
        .unwrap_or_else(|| panic!("family {key} is generated"))
        .iter()
        .find(|field| field[0] == name)
        .and_then(|field| field[1]["values"].as_array())
        .unwrap_or_else(|| panic!("family {key} has a wrapper enum"))
        .iter()
        .map(|value| value.as_str().expect("wrapper wire name").to_string())
        .collect()
}

#[test]
fn access_error_holders_are_sourced() {
    let root = repo_root();
    let families: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("crates/gents-lean-vocab/families.schema.json"))
            .expect("family schema"),
    )
    .expect("family schema JSON");
    let mut violations = Vec::new();
    let mut access = BTreeMap::<String, BTreeSet<String>>::new();
    let mut retry = BTreeMap::<String, BTreeSet<String>>::new();
    for (relative, syntax) in parse_production(&root, production_sources(&root)) {
        let mut visitor = ErrorHolderVisitor::default();
        visitor.visit_file(&syntax);
        let package = relative
            .split('/')
            .nth(1)
            .expect("source is in a crate")
            .to_string();
        for holder in visitor.holders {
            let exempt = |held: &str| {
                holder.owner == held
                    || OWNERS_ASKED_FOR_WHAT_THEY_HOLD
                        .iter()
                        .any(|(owner, asked)| *owner == holder.owner && *asked == held)
            };
            let must_source = holder
                .held
                .iter()
                .copied()
                .filter(|held| SOURCED_HOLDINGS.contains(held) && !exempt(held))
                .collect::<Vec<_>>();
            if holder.transparent && !must_source.is_empty() {
                violations.push(format!(
                    "{relative}: {}::{} holds {} transparently; hold it as `#[source]` under \
                     `#[error(fmt = gents_protocol::error_chain::held)]`",
                    holder.owner,
                    holder.variant,
                    must_source.join(", ")
                ));
            }
            let wire = holder_wire_name(&holder.owner, &holder.variant);
            if ["ConfigClientError", "GraphqlResponseError", "ViewError"]
                .into_iter()
                .any(|held| holder.held.contains(&held) && !exempt(held))
            {
                access
                    .entry(package.clone())
                    .or_default()
                    .insert(wire.clone());
            }
            if ["GraphqlError", "GraphqlFailure"]
                .into_iter()
                .any(|held| holder.held.contains(&held) && !exempt(held))
            {
                retry.entry(package.clone()).or_default().insert(wire);
            }
        }
    }
    for (kind, declared, families_by_crate) in [
        (
            "an access error or a store view error",
            &access,
            ACCESS_HOLDER_FAMILIES,
        ),
        ("a GraphQL error or failure", &retry, RETRY_HOLDER_FAMILIES),
    ] {
        let packages = declared
            .keys()
            .map(String::as_str)
            .chain(families_by_crate.iter().map(|(package, _)| *package))
            .collect::<BTreeSet<_>>();
        for package in packages {
            let found = declared.get(package).cloned().unwrap_or_default();
            match families_by_crate
                .iter()
                .find(|(owner, _)| *owner == package)
            {
                None => violations.push(format!(
                    "{package}: variants {found:?} hold {kind} and the crate has no \
                     `AccessClassification` family; model them and replay it in the crate"
                )),
                Some((_, key)) => {
                    let modeled = modeled_wrappers(&families, key);
                    if found != modeled {
                        violations.push(format!(
                            "{package}: variants holding {kind} {found:?} differ from family \
                             {key} {modeled:?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "access classification guard failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn error_holder_fence_reads_holdings_and_wire_names() {
    let syntax = syn::parse_file(concat!(
        "#[derive(Debug, thiserror::Error)] enum ReadError {",
        "  #[error(transparent)] Execute(#[from] ConfigClientError),",
        "  #[error(\"{0}\")] Boxed(#[source] Box<crate::config_client::ConfigClientError>),",
        "  #[error(transparent)] Erased(Box<dyn std::error::Error + Send + Sync>),",
        "  #[error(transparent)] Other(#[from] anyhow::Error),",
        "  #[error(\"plain\")] Plain(String),",
        "}",
        "#[derive(Debug)] enum NotAnError { Execute(ConfigClientError) }",
        "#[cfg(test)] mod tests { #[derive(Debug, Error)] enum E { X(GraphqlFailure) } }",
    ))
    .expect("parse snippet");
    let mut visitor = ErrorHolderVisitor::default();
    visitor.visit_file(&syntax);
    let holders = visitor
        .holders
        .iter()
        .map(|holder| {
            (
                holder.variant.as_str(),
                holder.held.iter().copied().collect::<Vec<_>>(),
                holder.transparent,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        holders,
        [
            ("Execute", vec!["ConfigClientError"], true),
            ("Boxed", vec!["ConfigClientError"], false),
            ("Erased", vec!["dyn Error"], true),
            ("Other", vec!["anyhow::Error"], true),
        ]
    );
    assert_eq!(
        holder_wire_name("AgentSelfConfigPermissionError", "Access"),
        "agent_self_config_permission_error_access"
    );
    assert_eq!(
        holder_wire_name("MCPHealthError", "Probe"),
        "mcp_health_error_probe"
    );
}

/// Integration-test targets that link on their own beside their crate's one
/// integration binary, each with the process-wide state that keeps it apart.
/// Every other integration test is a module of that binary: each extra target
/// relinks the crate's whole dependency graph. This allowlist only shrinks.
const SEPARATE_TEST_TARGETS: &[(&str, &str)] = &[
    // Its scenarios serialise on the process-global embedded P2P transport.
    ("gents", "e2e_triggers"),
    // A source scanner that links no runtime crate; run alone while editing ratchets.
    ("gents", "write_owner_structure"),
    // Installs a global allocator.
    ("gents-cli", "heap_profile"),
    // The Windows editor job builds and runs it without the CLI suite.
    ("gents-cli", "port_allocation"),
    // Sets GENTS_PLUGIN_BUILDER for the process.
    ("gents-cli", "plugin_build_sandbox"),
    // Sets HOME and the XDG directories for the process.
    ("gents-desktop-bridge", "connect_this_desktop"),
    ("gents-desktop-bridge", "home_isolation"),
    // Installs the global tracing subscriber and drives its clock.
    ("gents-session", "log_rate_filter"),
];

/// Integration-test roots on disk: `tests/*.rs` and `tests/*/main.rs`.
fn integration_test_roots(tests: &Path) -> Vec<(String, String)> {
    let mut roots = Vec::new();
    for entry in std::fs::read_dir(tests).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
            roots.push((name.trim_end_matches(".rs").to_owned(), name));
        } else if path.join("main.rs").is_file() {
            roots.push((name.clone(), format!("{name}/main.rs")));
        }
    }
    roots.sort();
    roots
}

/// Test targets a member links and the `tests/` files no target compiles.
fn integration_test_layout(
    root: &Path,
    member: &str,
    manifest: &toml::Table,
) -> (Vec<String>, Vec<String>) {
    let tests = root.join(member).join("tests");
    let declared: Vec<(String, String)> = manifest
        .get("test")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .map(|target| {
            let name = target["name"].as_str().expect("[[test]] name").to_owned();
            let path = target
                .get("path")
                .and_then(toml::Value::as_str)
                .map(|path| path.trim_start_matches("tests/").to_owned())
                .unwrap_or_else(|| format!("{name}.rs"));
            (name, path)
        })
        .collect();
    let discovered = manifest
        .get("package")
        .and_then(|package| package.get("autotests"))
        .and_then(toml::Value::as_bool)
        != Some(false);
    let mut targets: BTreeSet<String> = declared.iter().map(|(name, _)| name.clone()).collect();
    let mut compiled: BTreeSet<String> = declared.iter().map(|(_, path)| path.clone()).collect();
    let module_path = regex::Regex::new(r#"#\[path\s*=\s*"([^"]+)"\]"#).unwrap();
    for (_, path) in &declared {
        if let Ok(source) = std::fs::read_to_string(tests.join(path)) {
            compiled.extend(
                module_path
                    .captures_iter(&source)
                    .map(|capture| capture[1].to_owned()),
            );
        }
    }
    let mut uncompiled = Vec::new();
    for (name, path) in integration_test_roots(&tests) {
        if discovered {
            targets.insert(name);
        } else if !compiled.contains(&path) {
            uncompiled.push(format!("{member}/tests/{path}"));
        }
    }
    (targets.into_iter().collect(), uncompiled)
}

#[test]
fn integration_tests_share_one_binary_per_crate() {
    let root = repo_root();
    let workspace = Workspace::load(&root);
    let mut findings = Vec::new();
    let mut separate_seen = BTreeSet::new();
    for (name, (member, manifest)) in &workspace.members {
        let (targets, uncompiled) = integration_test_layout(&root, member, manifest);
        for file in uncompiled {
            findings.push(format!(
                "{file} is not compiled: with `autotests = false` add it as a `#[path]` module of tests/integration.rs"
            ));
        }
        let mut shared = Vec::new();
        for target in targets {
            match SEPARATE_TEST_TARGETS
                .iter()
                .find(|(package, separate)| package == name && *separate == target)
            {
                Some(entry) => {
                    separate_seen.insert(*entry);
                }
                None => shared.push(target),
            }
        }
        if shared.len() > 1 {
            findings.push(format!(
                "{member}: integration-test targets {shared:?} each relink the crate's dependency graph; make them modules of one tests/integration.rs"
            ));
        }
    }
    for entry in SEPARATE_TEST_TARGETS {
        if !separate_seen.contains(entry) {
            findings.push(format!(
                "{entry:?} no longer exists; remove it from SEPARATE_TEST_TARGETS"
            ));
        }
    }
    assert!(findings.is_empty(), "{}", findings.join("\n"));
}

#[test]
fn integration_test_layout_flags_extra_and_uncompiled_targets() {
    let root = tempfile::tempdir().unwrap();
    let tests = root.path().join("member/tests");
    std::fs::create_dir_all(tests.join("suite")).unwrap();
    for file in ["integration.rs", "folded.rs", "stray.rs", "suite/main.rs"] {
        std::fs::write(tests.join(file), "").unwrap();
    }
    std::fs::write(
        tests.join("integration.rs"),
        "#[path = \"folded.rs\"]\nmod folded;\n",
    )
    .unwrap();
    let manifest = |text: &str| text.parse::<toml::Table>().unwrap();
    let discovered = integration_test_layout(
        root.path(),
        "member",
        &manifest("[package]\nname = \"m\"\n"),
    );
    assert_eq!(discovered.0, ["folded", "integration", "stray", "suite"]);
    assert!(discovered.1.is_empty());
    let folded = integration_test_layout(
        root.path(),
        "member",
        &manifest("[package]\nname = \"m\"\nautotests = false\n\n[[test]]\nname = \"integration\"\npath = \"tests/integration.rs\"\n"),
    );
    assert_eq!(folded.0, ["integration"]);
    assert_eq!(
        folded.1,
        ["member/tests/stray.rs", "member/tests/suite/main.rs"]
    );
}

#[derive(Default)]
struct BoxedPassthroughVisitor {
    sites: usize,
    /// `<owner>Passthrough` wire names of the variants holding `PassedThrough`.
    passed: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for BoxedPassthroughVisitor {
    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        for variant in &item.variants {
            let passed = variant.fields.iter().any(|field| {
                matches!(&field.ty, syn::Type::Path(path)
                    if path.path.segments.last().is_some_and(|segment| segment.ident == "PassedThrough"))
            });
            if variant.ident == "Passthrough" && passed {
                self.passed
                    .insert(holder_wire_name(&item.ident.to_string(), "Passthrough"));
            }
        }
        visit::visit_item_enum(self, item);
    }

    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        let boxed = variant
            .fields
            .iter()
            .any(|field| boxes_a_trait_object(&field.ty));
        if variant.ident == "Passthrough" && boxed {
            self.sites += 1;
        }
        visit::visit_variant(self, variant);
    }
}

fn boxes_a_trait_object(ty: &syn::Type) -> bool {
    let syn::Type::Path(path) = ty else {
        return false;
    };
    let Some(last) = path.path.segments.last() else {
        return false;
    };
    let syn::PathArguments::AngleBracketed(arguments) = &last.arguments else {
        return false;
    };
    last.ident == "Box"
        && arguments.args.iter().any(|argument| {
            matches!(
                argument,
                syn::GenericArgument::Type(syn::Type::TraitObject(_))
            )
        })
}

fn boxed_passthrough_sites(syntax: &syn::File) -> usize {
    let mut visitor = BoxedPassthroughVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

fn passed_through_owners(syntax: &syn::File) -> BTreeSet<String> {
    let mut visitor = BoxedPassthroughVisitor::default();
    visitor.visit_file(syntax);
    visitor.passed
}

/// An operation error's `Passthrough` variant holds
/// `gents_protocol::error_chain::PassedThrough`, never `Box<dyn Error>`: an
/// anyhow error converted into that box forwards `source()` past its top error,
/// so a store conflict or refusal on top is invisible to every classifier
/// (Lean `AccessClassification.Chain.hidden_passthrough_is_not_found`). The
/// owners holding `PassedThrough` are `AccessClassification.PassthroughWrapper`,
/// whose families replay them. The allowlist is empty and only shrinks.
#[test]
fn passthrough_variants_keep_their_source() {
    let root = repo_root();
    let mut sources = Vec::new();
    for directory in ["crates", "apps"] {
        all_rust_sources(&root.join(directory), &mut sources);
    }
    let mut passed = BTreeSet::new();
    let mut violations = sources
        .into_iter()
        .filter_map(|path| {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            passed.extend(passed_through_owners(&syntax));
            let sites = boxed_passthrough_sites(&syntax);
            (sites > 0).then(|| {
                let relative = path
                    .strip_prefix(&root)
                    .expect("source under repo root")
                    .to_string_lossy()
                    .into_owned();
                format!("{relative}: {sites} Passthrough variants hold Box<dyn Error>; hold error_chain::PassedThrough")
            })
        })
        .collect::<BTreeSet<_>>();
    let families: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("crates/gents-lean-vocab/families.schema.json"))
            .expect("family schema"),
    )
    .expect("family schema JSON");
    for key in [
        "access_classification_passthrough_wrapped",
        "access_classification_passthrough_printed",
    ] {
        let modeled = modeled_field(&families, key, "passthrough_wrapper");
        if passed != modeled {
            violations.insert(format!(
                "Passthrough variants holding PassedThrough {passed:?} differ from family {key} \
                 {modeled:?}"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "passthrough ratchet failed:\n{}",
        violations.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn passthrough_fence_flags_boxed_variants_only() {
    for (source, sites) in [
        (
            "enum E { #[error(transparent)] Passthrough(Box<dyn Error + Send + Sync>) }",
            1,
        ),
        (
            "enum E { Passthrough(Box<dyn std::error::Error + Send + Sync>) }",
            1,
        ),
        (
            "enum E { Passthrough(crate::error_chain::PassedThrough) }",
            0,
        ),
        ("enum E { Other(Box<dyn Error + Send + Sync>) }", 0),
        (
            "mod m { enum E { Passthrough { source: Box<dyn Error> } } }",
            1,
        ),
    ] {
        let syntax = syn::parse_file(source).unwrap();
        assert_eq!(boxed_passthrough_sites(&syntax), sites, "{source}");
    }
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

/// `gents_lean_contract` entry points that build or run the Lean model with
/// `lake`, synchronously, for as long as the machine takes.
const LEAN_GENERATOR_RUNS: &[&str] = &[
    "load_contract_json",
    "load_contract_snapshot",
    "load_contract_stdout",
    "run_contract_generator",
    "run_lake_build",
    "run_prepared_generator",
    "run_prepared_generator_with_input",
];

/// Async bodies that still run a Lean generator on their runtime thread. On a
/// `#[tokio::test]` runtime that thread is the only worker: a body holding an
/// execution lease starves its renewal for the whole run
/// (`LeaseRenewalStarvation.tla`, `SyncInsideLease`). New code awaits
/// `gents_lean_vocab::snapshot()`, which loads off the runtime, or runs the
/// generator inside `spawn_blocking`. This allowlist only shrinks.
#[rustfmt::skip]
const LEAN_GENERATOR_ON_RUNTIME_ALLOWLIST: &[(&str, usize)] = &[];

struct LeanGeneratorVisitor {
    imports_owner: bool,
    in_async: bool,
    sites: usize,
}

impl LeanGeneratorVisitor {
    fn off_runtime<F>(&mut self, visit_arguments: F)
    where
        F: FnOnce(&mut Self),
    {
        let in_async = std::mem::replace(&mut self.in_async, false);
        visit_arguments(self);
        self.in_async = in_async;
    }

    fn with_async<F>(&mut self, asynchronous: bool, visit_body: F)
    where
        F: FnOnce(&mut Self),
    {
        let in_async = std::mem::replace(&mut self.in_async, asynchronous);
        visit_body(self);
        self.in_async = in_async;
    }
}

impl<'ast> Visit<'ast> for LeanGeneratorVisitor {
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.with_async(item.sig.asyncness.is_some(), |visitor| {
            visit::visit_item_fn(visitor, item);
        });
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.with_async(item.sig.asyncness.is_some(), |visitor| {
            visit::visit_impl_item_fn(visitor, item);
        });
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        self.with_async(item.sig.asyncness.is_some(), |visitor| {
            visit::visit_trait_item_fn(visitor, item);
        });
    }

    fn visit_expr_async(&mut self, block: &'ast syn::ExprAsync) {
        self.with_async(true, |visitor| visit::visit_expr_async(visitor, block));
    }

    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        let asynchronous = self.in_async || closure.asyncness.is_some();
        self.with_async(asynchronous, |visitor| {
            visit::visit_expr_closure(visitor, closure);
        });
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        let Expr::Path(function) = call.func.as_ref() else {
            visit::visit_expr_call(self, call);
            return;
        };
        let segments = &function.path.segments;
        let last = segments.last().map(|segment| segment.ident.to_string());
        let leaves_runtime = last.as_deref() == Some("spawn_blocking")
            || (last.as_deref() == Some("spawn")
                && segments.iter().any(|segment| segment.ident == "thread"));
        if leaves_runtime {
            self.visit_expr(&call.func);
            self.off_runtime(|visitor| {
                for argument in &call.args {
                    visitor.visit_expr(argument);
                }
            });
            return;
        }
        let names_owner = segments
            .iter()
            .any(|segment| segment.ident == "gents_lean_contract")
            || (self.imports_owner && segments.len() == 1);
        if self.in_async
            && names_owner
            && last
                .as_deref()
                .is_some_and(|name| LEAN_GENERATOR_RUNS.contains(&name))
        {
            self.sites += 1;
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if call.method == "spawn_blocking" {
            self.visit_expr(&call.receiver);
            self.off_runtime(|visitor| {
                for argument in &call.args {
                    visitor.visit_expr(argument);
                }
            });
            return;
        }
        visit::visit_expr_method_call(self, call);
    }
}

fn lean_generator_runtime_sites(source: &str, syntax: &syn::File) -> usize {
    let mut visitor = LeanGeneratorVisitor {
        imports_owner: source.contains("use gents_lean_contract::"),
        in_async: false,
        sites: 0,
    };
    visitor.visit_file(syntax);
    visitor.sites
}

#[test]
fn lean_generator_runs_stay_off_async_runtime_threads() {
    let root = repo_root();
    let mut sources = Vec::new();
    for directory in ["crates", "apps"] {
        all_rust_sources(&root.join(directory), &mut sources);
    }
    let actual = sources
        .into_iter()
        .filter_map(|path| {
            let relative = path
                .strip_prefix(&root)
                .expect("source under repo root")
                .to_string_lossy()
                .into_owned();
            if relative.starts_with("crates/gents-lean-contract/src/") {
                return None;
            }
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            if !source.contains("gents_lean_contract") {
                return None;
            }
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let sites = lean_generator_runtime_sites(&source, &syntax);
            (sites > 0).then_some((relative, sites))
        })
        .collect::<BTreeMap<_, _>>();
    let allowed = LEAN_GENERATOR_ON_RUNTIME_ALLOWLIST
        .iter()
        .map(|(path, count)| (path.to_string(), *count))
        .collect::<BTreeMap<_, _>>();
    let mut violations = Vec::new();
    for path in allowed.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let found = actual.get(path).copied().unwrap_or_default();
        match allowed.get(path).copied() {
            None => violations.push(format!(
                "{path}: {found} Lean generator run(s) on an async runtime thread; await gents_lean_vocab::snapshot() or run it inside spawn_blocking"
            )),
            Some(limit) if found > limit => violations.push(format!(
                "{path}: {found} Lean generator runs on an async runtime thread exceed the allowlisted {limit}"
            )),
            Some(limit) if found < limit => violations.push(format!(
                "{path}: {found} Lean generator runs on an async runtime thread; lower the allowlist entry from {limit}"
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
        "Lean generator runtime ratchet failed:\n{}\n\ncurrent sites:\n{current}",
        violations.join("\n")
    );
}

#[test]
fn lean_generator_fence_counts_async_bodies_only() {
    for (source, sites) in [
        (
            "async fn f() { gents_lean_contract::run_prepared_generator(d, t, f); }",
            1,
        ),
        (
            "fn f() { gents_lean_contract::run_prepared_generator(d, t, f); }",
            0,
        ),
        (
            "fn f() { let _ = async { gents_lean_contract::load_contract_json() }; }",
            1,
        ),
        (
            "async fn f() { tokio::task::spawn_blocking(|| gents_lean_contract::load_contract_json()).await; }",
            0,
        ),
        (
            "async fn f() { std::thread::spawn(move || gents_lean_contract::run_lake_build(d)); }",
            0,
        ),
        (
            "async fn f() { fn g() { gents_lean_contract::run_lake_build(d); } }",
            0,
        ),
        (
            "async fn f() { let g = || gents_lean_contract::run_lake_build(d); g(); }",
            1,
        ),
        ("async fn f() { gents_lean_contract::proofs_dir(); }", 0),
        (
            "use gents_lean_contract::run_lake_build;\nasync fn f() { run_lake_build(d); }",
            1,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(lean_generator_runtime_sites(source, &syntax), sites, "{source}");
    }
}

/// Calls that start or extend a request's execution lease and its renewal task
/// on the calling runtime, directly or through a test fixture.
const LEASE_STARTS: &[&str] = &[
    "begin_owned_execution",
    "bind_interruptible_request",
    "claim_at",
    "claim_with_identity",
    "fixture_with_ids",
];

/// Counts synchronous `gents_lean_vocab` reads that an async body makes after
/// starting a lease without having awaited `gents_lean_vocab::snapshot()` or
/// read the model before the lease.
/// Such a read can be the process's first, which runs Lean on the runtime
/// thread for minutes and starves the lease renewal (#3704, #3891, #3892).
struct LeanReadAfterLeaseVisitor {
    starts: BTreeSet<String>,
    events: Vec<(usize, usize, LeanEvent)>,
    sites: usize,
}

/// The names of one file's calls that start a lease: [`LEASE_STARTS`] and,
/// to a fixed point, every function of the file whose body calls one, so a
/// file-local fixture that claims counts at its call site (`step0` over
/// `job_chain` in `plugin_job_dispatch.rs`).
fn lease_starts_in_file(syntax: &syn::File) -> BTreeSet<String> {
    #[derive(Default)]
    struct Callees {
        current: Option<String>,
        functions: Vec<(String, BTreeSet<String>)>,
    }
    impl Callees {
        fn enter(&mut self, name: String) -> Option<String> {
            self.functions.push((name.clone(), BTreeSet::new()));
            self.current.replace(name)
        }
        fn called(&mut self, name: String) {
            if self.current.is_some() {
                if let Some((_, calls)) = self.functions.last_mut() {
                    calls.insert(name);
                }
            }
        }
    }
    impl<'ast> Visit<'ast> for Callees {
        fn visit_item_fn(&mut self, item: &'ast ItemFn) {
            let outer = self.enter(item.sig.ident.to_string());
            visit::visit_item_fn(self, item);
            self.current = outer;
        }
        fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
            let outer = self.enter(item.sig.ident.to_string());
            visit::visit_impl_item_fn(self, item);
            self.current = outer;
        }
        fn visit_expr_call(&mut self, call: &'ast ExprCall) {
            if let Expr::Path(function) = call.func.as_ref() {
                if let Some(last) = function.path.segments.last() {
                    self.called(last.ident.to_string());
                }
            }
            visit::visit_expr_call(self, call);
        }
        fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
            self.called(call.method.to_string());
            visit::visit_expr_method_call(self, call);
        }
        fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
            let (mut parsed, mut loose) = (Vec::new(), Vec::new());
            macro_expressions(invocation.tokens.clone(), &mut parsed, &mut loose);
            for argument in &parsed {
                self.visit_expr(argument);
            }
        }
    }
    let mut callees = Callees::default();
    callees.visit_file(syntax);
    let mut starts = LEASE_STARTS
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    loop {
        let before = starts.len();
        for (name, calls) in &callees.functions {
            if calls.iter().any(|call| starts.contains(call)) {
                starts.insert(name.clone());
            }
        }
        if starts.len() == before {
            return starts;
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum LeanEvent {
    LeaseStart,
    SnapshotAwait,
    SyncRead,
}

impl LeanReadAfterLeaseVisitor {
    fn record(&mut self, span: proc_macro2::Span, event: LeanEvent) {
        let start = span.start();
        self.events.push((start.line, start.column, event));
    }

    fn settle(&mut self) {
        self.events
            .sort_by_key(|(line, column, _)| (*line, *column));
        let (mut leased, mut loaded) = (false, false);
        for (_, _, event) in self.events.drain(..) {
            match event {
                LeanEvent::LeaseStart => leased = true,
                LeanEvent::SnapshotAwait => loaded = true,
                LeanEvent::SyncRead if leased && !loaded => self.sites += 1,
                // A read before any lease loaded the snapshot already.
                LeanEvent::SyncRead => loaded = true,
            }
        }
    }
}

impl<'ast> Visit<'ast> for LeanReadAfterLeaseVisitor {
    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        let outer = std::mem::take(&mut self.events);
        visit::visit_item_fn(self, item);
        if item.sig.asyncness.is_some() {
            self.settle();
        }
        self.events = outer;
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Expr::Path(function) = call.func.as_ref() {
            let segments = &function.path.segments;
            let last = segments.last().map(|segment| segment.ident.to_string());
            if segments
                .iter()
                .any(|segment| segment.ident == "gents_lean_vocab")
            {
                let event = if last.as_deref() == Some("snapshot") {
                    LeanEvent::SnapshotAwait
                } else {
                    LeanEvent::SyncRead
                };
                self.record(call.span(), event);
            } else if last.as_ref().is_some_and(|name| self.starts.contains(name)) {
                self.record(call.span(), LeanEvent::LeaseStart);
            }
        }
        visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast ExprMethodCall) {
        if self.starts.contains(&call.method.to_string()) {
            self.record(call.method.span(), LeanEvent::LeaseStart);
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, invocation: &'ast syn::Macro) {
        let (mut parsed, mut loose) = (Vec::new(), Vec::new());
        macro_expressions(invocation.tokens.clone(), &mut parsed, &mut loose);
        for argument in &parsed {
            self.visit_expr(argument);
        }
    }
}

fn lean_reads_after_lease(syntax: &syn::File) -> usize {
    let mut visitor = LeanReadAfterLeaseVisitor {
        starts: lease_starts_in_file(syntax),
        events: Vec::new(),
        sites: 0,
    };
    visitor.visit_file(syntax);
    visitor.sites
}

/// An async test that holds an execution lease awaits
/// `gents_lean_vocab::snapshot()` before any synchronous model read, so the
/// first load runs off its runtime (`LeaseRenewalStarvation.tla`,
/// `OffRuntime`). Syntactic: fixtures are matched by name, a file's own
/// functions that reach a lease start count as one, and macro input is read
/// as [`macro_expressions`] reads it.
#[test]
fn lease_holders_await_the_lean_snapshot() {
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
            if !source.contains("gents_lean_vocab") {
                return None;
            }
            let syntax = syn::parse_file(&source)
                .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
            let sites = lean_reads_after_lease(&syntax);
            (sites > 0).then(|| {
                format!(
                    "{}: {sites} synchronous gents_lean_vocab read(s) after a lease starts; await gents_lean_vocab::snapshot() first",
                    path.strip_prefix(&root).expect("source under repo root").display()
                )
            })
        })
        .collect::<Vec<_>>();
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn lean_read_fence_counts_reads_after_a_lease_without_the_await() {
    for (source, sites) in [
        (
            "async fn f() { l.claim_with_identity().await; gents_lean_vocab::lean_contract_snapshot(); }",
            1,
        ),
        (
            "async fn f() { gents_lean_vocab::lean_contract_snapshot(); l.claim_with_identity().await; }",
            0,
        ),
        (
            "async fn f() { fixture_with_ids(\"x\").await; gents_lean_vocab::snapshot().await; gents_lean_vocab::families::k::cases(); }",
            0,
        ),
        (
            "async fn f() { fixture_with_ids(\"x\").await; for c in gents_lean_vocab::families::k::cases() {} }",
            1,
        ),
        (
            "fn f() { l.claim_with_identity(); gents_lean_vocab::lean_contract_snapshot(); }",
            0,
        ),
        (
            "async fn chain() { l.claim_with_identity().await; }\nasync fn step0() { chain().await; }\nasync fn f() { step0().await; for c in gents_lean_vocab::families::k::cases() {} }",
            1,
        ),
        (
            "async fn step0() { l.claim_with_identity().await; }\nasync fn f() { gents_lean_vocab::snapshot().await; step0().await; gents_lean_vocab::families::k::cases(); }",
            0,
        ),
        (
            "async fn chain() { assert_eq!(l.claim_with_identity().await, Claimed); }\nasync fn f() { chain().await; assert!(gents_lean_vocab::families::k::cases().is_empty()); }",
            1,
        ),
        (
            "async fn f() { gents_lean_vocab::lean_contract_snapshot(); l.claim_with_identity().await; gents_lean_vocab::lean_contract_snapshot(); }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(lean_reads_after_lease(&syntax), sites, "{source}");
    }
}

const CLIENT_ENDPOINT_CRATES: &[&str] = &[
    "crates/gents-desktop-",
    "crates/gents-client-host/",
    "apps/",
];

#[derive(Default)]
struct EndpointConstructionVisitor {
    sites: usize,
}

impl<'ast> Visit<'ast> for EndpointConstructionVisitor {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !item_cfg_test(item) {
            visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !cfg_test(&item.attrs) {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if path_name(call).is_some_and(|name| {
            [
                "GraphqlEndpoint::as_principal",
                "GraphqlEndpoint::anonymous",
            ]
            .iter()
            .any(|constructor| name.ends_with(constructor))
        }) {
            self.sites += 1;
        }
        visit::visit_expr_call(self, call);
    }
}

const HOME_CHOICE_CHECKS: &[&str] = &[
    "document_acp::ensure_home_opens",
    "document_acp::ensure_admissible",
];

const HOME_STORE_CLAIMS: &[&str] = &[
    "lock_store",
    "lock_store_at",
    "lock_home_store",
    "try_store",
    "acquire_store",
    "lock_init_store",
    "reserve_runtime_startup",
    "open_home_store_key",
    "open_home_store_key_with_custody",
    "ProtectedBackendStore::open",
    "FreshBackendStore::observe",
];

#[derive(Clone, Copy, PartialEq)]
enum HomeEvent {
    Check,
    Claim,
}

type HomeEvents = Vec<(usize, usize, HomeEvent)>;

fn names_one_of(name: &str, entries: &[&str]) -> bool {
    entries
        .iter()
        .any(|entry| name == *entry || name.ends_with(&format!("::{entry}")))
}

/// The production functions of one file with their choice checks and store
/// claims in source order. A function of the same file that checks the
/// choice counts as a check where it is called by its bare name.
struct HomeChoiceVisitor<'a> {
    wrappers: &'a BTreeSet<String>,
    functions: Vec<(String, HomeEvents)>,
    events: HomeEvents,
}

impl HomeChoiceVisitor<'_> {
    fn function(
        &mut self,
        name: String,
        attributes: &[syn::Attribute],
        visit: impl FnOnce(&mut Self),
    ) {
        if cfg_test(attributes) {
            return;
        }
        let outer = std::mem::take(&mut self.events);
        visit(self);
        let mut events = std::mem::replace(&mut self.events, outer);
        events.sort_by_key(|(line, column, _)| (*line, *column));
        self.functions.push((name, events));
    }
}

impl<'ast> Visit<'ast> for HomeChoiceVisitor<'_> {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !cfg_test(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        self.function(item.sig.ident.to_string(), &item.attrs, |visitor| {
            visit::visit_item_fn(visitor, item);
        });
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        self.function(item.sig.ident.to_string(), &item.attrs, |visitor| {
            visit::visit_impl_item_fn(visitor, item);
        });
    }

    fn visit_expr_call(&mut self, call: &'ast ExprCall) {
        if let Some(name) = path_name(call) {
            let event = if names_one_of(&name, HOME_CHOICE_CHECKS) || self.wrappers.contains(&name)
            {
                Some(HomeEvent::Check)
            } else if names_one_of(&name, HOME_STORE_CLAIMS) {
                Some(HomeEvent::Claim)
            } else {
                None
            };
            if let Some(event) = event {
                let start = call.span().start();
                self.events.push((start.line, start.column, event));
            }
        }
        visit::visit_expr_call(self, call);
    }
}

fn endpoint_construction_sites(syntax: &syn::File) -> usize {
    let mut visitor = EndpointConstructionVisitor::default();
    visitor.visit_file(syntax);
    visitor.sites
}

/// A client builds every endpoint to a runtime through `RuntimeEndpoints`, so
/// the audience the runtime reports reaches the one minting point
/// (`Identity.HeldAudience.recordCredential`).
#[test]
fn client_endpoints_are_built_by_their_owner() {
    let root = repo_root();
    let violations = parse_production(&root, production_sources(&root))
        .into_iter()
        .filter(|(relative, _)| {
            CLIENT_ENDPOINT_CRATES
                .iter()
                .any(|prefix| relative.starts_with(prefix))
        })
        .filter(|(_, syntax)| endpoint_construction_sites(syntax) > 0)
        .map(|(relative, _)| {
            format!("{relative}: builds a GraphqlEndpoint; use gents_store::config_client::runtime_endpoints::RuntimeEndpoints")
        })
        .collect::<Vec<_>>();
    assert!(
        violations.is_empty(),
        "client endpoint ratchet failed:\n{}",
        violations.join("\n")
    );
}

#[test]
fn client_endpoint_fence_counts_constructors_outside_tests() {
    for (source, sites) in [
        ("fn f() { GraphqlEndpoint::as_principal(u, d); }", 1),
        (
            "fn f() { gents_store::config_client::GraphqlEndpoint::anonymous(u); }",
            1,
        ),
        ("fn f() { RuntimeEndpoints::new(c).endpoint(n, u); }", 0),
        (
            "#[cfg(test)] mod tests { fn f() { GraphqlEndpoint::anonymous(u); } }",
            0,
        ),
        (
            "impl S { #[cfg(test)] fn f() { GraphqlEndpoint::anonymous(u); } }",
            0,
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(endpoint_construction_sites(&syntax), sites, "{source}");
    }
}

fn home_choice_functions(
    syntax: &syn::File,
    wrappers: &BTreeSet<String>,
) -> Vec<(String, HomeEvents)> {
    let mut visitor = HomeChoiceVisitor {
        wrappers,
        functions: Vec::new(),
        events: Vec::new(),
    };
    visitor.visit_file(syntax);
    visitor.functions
}

fn checks_the_choice(events: &[(usize, usize, HomeEvent)]) -> bool {
    events
        .iter()
        .any(|(_, _, event)| *event == HomeEvent::Check)
}

fn stores_claimed_before_the_choice(syntax: &syn::File) -> Vec<String> {
    let mut wrappers = BTreeSet::new();
    let functions = loop {
        let functions = home_choice_functions(syntax, &wrappers);
        let checking = functions
            .iter()
            .filter(|(_, events)| checks_the_choice(events))
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        if checking == wrappers {
            break functions;
        }
        wrappers = checking;
    };
    functions
        .into_iter()
        .filter(|(_, events)| {
            checks_the_choice(events)
                && events
                    .iter()
                    .take_while(|(_, _, event)| *event != HomeEvent::Check)
                    .any(|(_, _, event)| *event == HomeEvent::Claim)
        })
        .map(|(name, _)| name)
        .collect()
}

/// A function that checks a home's document-ACP choice does so before it
/// claims or opens the home's store, so a refused home is left untouched
/// (Lean `DocumentAcpChoice.enter`). Syntactic: claims are matched by name,
/// and a function that never checks the choice is not held to it.
#[test]
fn a_homes_choice_is_checked_before_its_store_is_claimed() {
    let root = repo_root();
    let violations = parse_production(&root, production_sources(&root))
        .into_iter()
        .flat_map(|(relative, syntax)| {
            stores_claimed_before_the_choice(&syntax)
                .into_iter()
                .map(move |name| format!("{relative}: `{name}` claims the home's store before checking its document-ACP choice"))
        })
        .collect::<Vec<_>>();
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn home_choice_fence_flags_a_claim_before_the_check() {
    for (source, flagged) in [
        (
            "async fn f() { let k = store_key::open_home_store_key(h, d).await?; document_acp::ensure_home_opens(&c)?; }",
            vec!["f"],
        ),
        (
            "fn f() { let l = home::lock_store(h, d)?; gents_store::local::document_acp::ensure_admissible(r, q, s)?; }",
            vec!["f"],
        ),
        (
            "fn f() { document_acp::ensure_home_opens(&c)?; let l = home::lock_store(h, d)?; ProtectedBackendStore::open(d, k, l); }",
            vec![],
        ),
        (
            "fn admit() { document_acp::ensure_admissible(r, q, s)?; } fn f() { let l = home::acquire_store(h, d, w)?; admit()?; }",
            vec!["f"],
        ),
        (
            "fn admit() { document_acp::ensure_admissible(r, q, s)?; } fn f() { admit()?; let l = home::acquire_store(h, d, w)?; }",
            vec![],
        ),
        ("fn f() { let l = home::lock_store(h, d)?; }", vec![]),
        (
            "#[cfg(test)] fn f() { let l = home::lock_store(h, d)?; document_acp::ensure_home_opens(&c)?; }",
            vec![],
        ),
        (
            "fn f() { patch::ensure_admissible(t, p)?; let l = home::lock_store(h, d)?; document_acp::ensure_home_opens(&c)?; }",
            vec!["f"],
        ),
    ] {
        let syntax = syn::parse_file(source).expect("parse snippet");
        assert_eq!(stores_claimed_before_the_choice(&syntax), flagged, "{source}");
    }
}
