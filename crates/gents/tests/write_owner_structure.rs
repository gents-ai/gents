//! AST fences for the canonical DefraDB write (#1418) and read (#1670) owners.

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

/// Production files that still call `EmbeddedNode` GraphQL execution directly,
/// with their current site counts. This is a syntactic ratchet over direct node
/// access, not complete access enforcement: receivers are recognized from
/// declared `EmbeddedNode` types, the `node` naming convention, and local
/// aliases of those. A new file or a higher count fails, and so does a lower
/// count until the entry is lowered or removed. Migrate sites to the
/// `config_client`/`graphql` owners; never raise.
#[rustfmt::skip]
const NODE_EXECUTE_ALLOWLIST: &[(&str, usize)] = &[
    ("apps/gents-desktop/src-tauri/src/bin/bridge_runner/http/routes.rs", 1),
    ("apps/gents-desktop/src-tauri/src/runner/live_fixture/agent.rs", 1),
    ("crates/gents-cli/src/commands/grok_shim/acp.rs", 1),
    ("crates/gents-cli/src/commands/grok_shim/binding.rs", 1),
    ("crates/gents-cli/src/commands/grok_shim/projection/subagents.rs", 4),
    ("crates/gents-cli/src/commands/grok_shim/projection/subagents/control.rs", 4),
    ("crates/gents-cli/src/commands/grok_shim/projection/tools.rs", 1),
    ("crates/gents-cli/src/commands/grok_shim/sessions.rs", 3),
    ("crates/gents-cli/src/commands/grok_shim/task_control.rs", 2),
    ("crates/gents-cli/src/commands/grok_shim/turn.rs", 5),
    ("crates/gents-cli/src/commands/grok_shim/usage.rs", 2),
    ("crates/gents-desktop-bridge/src/commands/mcp_health.rs", 2),
    ("crates/gents-desktop-bridge/src/commands/task.rs", 1),
    ("crates/gents-desktop-bridge/src/snapshot/session/request_context.rs", 1),
    ("crates/gents-desktop-bridge/src/tauri_commands/operations.rs", 2),
    ("crates/gents-desktop-core/src/client/core/writes.rs", 3),
    ("crates/gents-desktop-core/src/client/mutations/chat/request.rs", 3),
    ("crates/gents-desktop-core/src/client/mutations/manage/task.rs", 3),
    ("crates/gents-migration/src/materialize.rs", 1),
    ("crates/gents/src/admission/recovery.rs", 2),
    ("crates/gents/src/backend_registry.rs", 1),
    ("crates/gents/src/background_completion/datetime_fields.rs", 1),
    ("crates/gents/src/background_completion/notification_delivery.rs", 2),
    ("crates/gents/src/background_completion/queries.rs", 3),
    ("crates/gents/src/background_completion/reconciliation.rs", 4),
    ("crates/gents/src/background_completion/side_effects.rs", 1),
    ("crates/gents/src/background_tools.rs", 9),
    ("crates/gents/src/background_tools/subagent_control.rs", 1),
    ("crates/gents/src/callback/scan.rs", 3),
    ("crates/gents/src/completion_factory.rs", 1),
    ("crates/gents/src/eth/submit.rs", 2),
    ("crates/gents/src/goal/operator_resume/support.rs", 2),
    ("crates/gents/src/graph_pipeline/run.rs", 2),
    ("crates/gents/src/health_checker.rs", 1),
    ("crates/gents/src/hook.rs", 1),
    ("crates/gents/src/hook/persistence/helpers.rs", 1),
    ("crates/gents/src/hook/persistence/subagent_bridge.rs", 1),
    ("crates/gents/src/interrupt.rs", 4),
    ("crates/gents/src/lifecycle/background_wake_recovery.rs", 1),
    ("crates/gents/src/lifecycle/claim.rs", 1),
    ("crates/gents/src/lifecycle/materialize.rs", 1),
    ("crates/gents/src/lifecycle/query.rs", 2),
    ("crates/gents/src/lifecycle/queue/coalescing.rs", 1),
    ("crates/gents/src/lifecycle/queue/draining.rs", 1),
    ("crates/gents/src/lifecycle/recovery.rs", 2),
    ("crates/gents/src/oauth_credential.rs", 4),
    ("crates/gents/src/provider_context_reduction.rs", 4),
    ("crates/gents/src/registry.rs", 1),
    ("crates/gents/src/rendered_request/commits.rs", 1),
    ("crates/gents/src/rendered_request/mod.rs", 1),
    ("crates/gents/src/request_admission.rs", 5),
    ("crates/gents/src/self_config/mod.rs", 2),
    ("crates/gents/src/session/observations.rs", 1),
    ("crates/gents/src/session/query.rs", 1),
    ("crates/gents/src/tool_call_lifecycle/query.rs", 2),
    ("crates/gents/src/tool_call_lifecycle/recovery.rs", 9),
    ("crates/gents/src/tool_call_lifecycle/subagent_request.rs", 2),
    ("crates/gents/src/tool_surface/root_admission.rs", 1),
    ("crates/gents/src/toolset/context_budget.rs", 2),
    ("crates/gents/src/toolset/memory.rs", 1),
    ("crates/gents/src/toolset/session_history.rs", 9),
    ("crates/gents/src/toolset/session_history/context_details.rs", 2),
    ("crates/gents/src/watcher/query.rs", 3),
];

const NODE_ONLY_METHODS: &[&str] = &[
    "execute_request",
    "execute_request_with_retry",
    "execute_with_retry",
    "runner",
];

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
    "execute_with_retry",
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
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(root.join("crates"))
        .expect("read crates")
        .flatten()
    {
        for directory in ["src", "examples"] {
            let path = entry.path().join(directory);
            if path.is_dir() {
                rust_sources(&path, &mut sources);
            }
        }
    }
    let apps = root.join("apps");
    if apps.is_dir() {
        rust_sources(&apps, &mut sources);
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
        "production DefraDB writes bypass ConfigAccess::write/transact:\n{}",
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

/// `#[path]` resolves against the file's directory at the top level and
/// against the inline module directory inside `mod name { ... }`.
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

/// Names whose declared type is `EmbeddedNode` (possibly behind references or
/// smart pointers), plus the workspace convention of naming node handles `node`.
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
        "fn f(x: &X, q: &str, p: P) { x.execute_with_retry(q, p); }",
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
