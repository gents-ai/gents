//! The conformance rule: every family the Lean snapshot emits is replayed by
//! exactly one native test. The scan finds that test from the family's
//! snapshot key, in a Rust file some target compiles (`module_tree`) or a
//! TypeScript file, so a consumed family needs no ledger entry. A family no test
//! replays is refused unless the coverage ledger
//! (`Proofs/Conformance/CoverageLedger.lean`) lists it as a boundary or as
//! open; a ledger entry naming a consumed or unemitted family is refused.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde_json::Value;
use syn::visit::{self, Visit};

const CONTRACT_METADATA: &[&str] = &["coverage_ledger", "feature_surface_requirements"];

const DERIVED_FAMILIES: &str = "families";
const DERIVED_VOCABULARIES: &str = "family_vocabularies";
const CONSUMER_FUNCTIONS: &[&str] = &["replay", "cases", "replay_vocabulary"];

const SOURCE_ROOTS: &[&str] = &["crates", "apps", "packages"];
const SKIPPED_DIRECTORIES: &[&str] = &["target", "node_modules", "proofs", "dist", "gen"];

fn source_files(directory: &Path, files: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("source directory entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if !SKIPPED_DIRECTORIES.contains(&name) && !name.starts_with('.') {
                source_files(&path, files);
            }
        } else if matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("rs" | "ts" | "tsx")
        ) {
            files.push(path);
        }
    }
}

pub fn emitted_families(snapshot: &Value) -> BTreeSet<String> {
    let snapshot = snapshot.as_object().expect("snapshot object");
    let unexpected: Vec<&str> = snapshot
        .iter()
        .filter(|(key, value)| {
            (value.is_array() || value.is_object())
                && !CONTRACT_METADATA.contains(&key.as_str())
                && ![DERIVED_FAMILIES, DERIVED_VOCABULARIES].contains(&key.as_str())
        })
        .map(|(key, _)| key.as_str())
        .collect();
    assert!(
        unexpected.is_empty(),
        "unexpected top-level snapshot collections: {unexpected:?}"
    );
    let mut families = BTreeSet::new();
    for derived in [DERIVED_FAMILIES, DERIVED_VOCABULARIES] {
        let derived = snapshot[derived].as_object().expect("derived families");
        families.extend(
            derived
                .keys()
                .map(|key| format!("{DERIVED_FAMILIES}/{key}")),
        );
    }
    families
}

/// The emitted family a ledger name denotes.
pub fn ledger_family(name: &str, families: &BTreeSet<String>) -> Option<String> {
    let family = format!("{DERIVED_FAMILIES}/{name}");
    families.contains(&family).then_some(family)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ImportTarget {
    FamiliesMod,
    FamilyMod(String),
    FamilyFn(String),
}

fn cfg_eval(expr: &syn::Expr) -> Option<bool> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Bool(value),
            ..
        }) => Some(value.value),
        syn::Expr::Call(call) => {
            let syn::Expr::Path(path) = call.func.as_ref() else {
                return None;
            };
            let ident = path.path.get_ident()?.to_string();
            match ident.as_str() {
                "not" if call.args.len() == 1 => cfg_eval(&call.args[0]).map(|value| !value),
                "all" => {
                    let mut all_true = true;
                    for arg in &call.args {
                        match cfg_eval(arg) {
                            Some(false) => return Some(false),
                            Some(true) => {}
                            None => all_true = false,
                        }
                    }
                    all_true.then_some(true)
                }
                "any" => {
                    let mut all_false = true;
                    for arg in &call.args {
                        match cfg_eval(arg) {
                            Some(true) => return Some(true),
                            Some(false) => {}
                            None => all_false = false,
                        }
                    }
                    all_false.then_some(false)
                }
                _ => None,
            }
        }
        _ => None,
    }
}

fn has_cfg_false(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Expr>()
                .ok()
                .and_then(|expr| cfg_eval(&expr))
                == Some(false)
    })
}

fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

fn impl_item_attrs(item: &syn::ImplItem) -> &[syn::Attribute] {
    match item {
        syn::ImplItem::Const(item) => &item.attrs,
        syn::ImplItem::Fn(item) => &item.attrs,
        syn::ImplItem::Type(item) => &item.attrs,
        syn::ImplItem::Macro(item) => &item.attrs,
        _ => &[],
    }
}

fn stmt_attrs(stmt: &syn::Stmt) -> &[syn::Attribute] {
    match stmt {
        syn::Stmt::Local(local) => &local.attrs,
        syn::Stmt::Item(item) => item_attrs(item),
        syn::Stmt::Expr(expr, _) => expr_attrs(expr),
        syn::Stmt::Macro(mac) => &mac.attrs,
    }
}

fn expr_attrs(expr: &syn::Expr) -> &[syn::Attribute] {
    match expr {
        syn::Expr::Array(expr) => &expr.attrs,
        syn::Expr::Assign(expr) => &expr.attrs,
        syn::Expr::Async(expr) => &expr.attrs,
        syn::Expr::Await(expr) => &expr.attrs,
        syn::Expr::Binary(expr) => &expr.attrs,
        syn::Expr::Block(expr) => &expr.attrs,
        syn::Expr::Break(expr) => &expr.attrs,
        syn::Expr::Call(expr) => &expr.attrs,
        syn::Expr::Cast(expr) => &expr.attrs,
        syn::Expr::Closure(expr) => &expr.attrs,
        syn::Expr::Const(expr) => &expr.attrs,
        syn::Expr::Continue(expr) => &expr.attrs,
        syn::Expr::Field(expr) => &expr.attrs,
        syn::Expr::ForLoop(expr) => &expr.attrs,
        syn::Expr::Group(expr) => &expr.attrs,
        syn::Expr::If(expr) => &expr.attrs,
        syn::Expr::Index(expr) => &expr.attrs,
        syn::Expr::Let(expr) => &expr.attrs,
        syn::Expr::Lit(expr) => &expr.attrs,
        syn::Expr::Loop(expr) => &expr.attrs,
        syn::Expr::Macro(expr) => &expr.attrs,
        syn::Expr::Match(expr) => &expr.attrs,
        syn::Expr::MethodCall(expr) => &expr.attrs,
        syn::Expr::Paren(expr) => &expr.attrs,
        syn::Expr::Path(expr) => &expr.attrs,
        syn::Expr::Range(expr) => &expr.attrs,
        syn::Expr::RawAddr(expr) => &expr.attrs,
        syn::Expr::Reference(expr) => &expr.attrs,
        syn::Expr::Repeat(expr) => &expr.attrs,
        syn::Expr::Return(expr) => &expr.attrs,
        syn::Expr::Struct(expr) => &expr.attrs,
        syn::Expr::Try(expr) => &expr.attrs,
        syn::Expr::TryBlock(expr) => &expr.attrs,
        syn::Expr::Tuple(expr) => &expr.attrs,
        syn::Expr::Unary(expr) => &expr.attrs,
        syn::Expr::Unsafe(expr) => &expr.attrs,
        syn::Expr::While(expr) => &expr.attrs,
        syn::Expr::Yield(expr) => &expr.attrs,
        _ => &[],
    }
}

fn flatten_use_tree(
    tree: &syn::UseTree,
    prefix: &mut Vec<String>,
    out: &mut Vec<(Vec<String>, Option<String>)>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            flatten_use_tree(&path.tree, prefix, out);
            prefix.pop();
        }
        syn::UseTree::Name(name) => {
            let ident = name.ident.to_string();
            if ident == "self" {
                if let Some(last) = prefix.last().cloned() {
                    out.push((prefix.clone(), Some(last)));
                }
            } else {
                let mut full = prefix.clone();
                full.push(ident.clone());
                out.push((full, Some(ident)));
            }
        }
        syn::UseTree::Rename(rename) => {
            let ident = rename.ident.to_string();
            let local = rename.rename.to_string();
            if ident == "self" {
                out.push((prefix.clone(), Some(local)));
            } else {
                let mut full = prefix.clone();
                full.push(ident);
                out.push((full, Some(local)));
            }
        }
        syn::UseTree::Group(group) => {
            for item in &group.items {
                flatten_use_tree(item, prefix, out);
            }
        }
        syn::UseTree::Glob(_) => {
            out.push((prefix.clone(), None));
        }
    }
}

struct RustConsumerScanner<'a> {
    known_keys: &'a BTreeSet<String>,
    scopes: Vec<BTreeMap<String, ImportTarget>>,
    counts: BTreeMap<String, usize>,
}

impl<'a> RustConsumerScanner<'a> {
    fn new(known_keys: &'a BTreeSet<String>) -> Self {
        Self {
            known_keys,
            scopes: Vec::new(),
            counts: BTreeMap::new(),
        }
    }

    fn lookup(&self, name: &str) -> Option<&ImportTarget> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    fn classify_use_path(&self, path: &[String]) -> Option<ImportTarget> {
        let first = path.first()?;
        if let Some(target) = self.lookup(first) {
            match (target, path.len()) {
                (ImportTarget::FamiliesMod, 1) => return Some(ImportTarget::FamiliesMod),
                (ImportTarget::FamiliesMod, 2) if self.known_keys.contains(&path[1]) => {
                    return Some(ImportTarget::FamilyMod(path[1].clone()));
                }
                (ImportTarget::FamiliesMod, 3)
                    if self.known_keys.contains(&path[1])
                        && CONSUMER_FUNCTIONS.contains(&path[2].as_str()) =>
                {
                    return Some(ImportTarget::FamilyFn(path[1].clone()));
                }
                (ImportTarget::FamilyMod(key), 1) => {
                    return Some(ImportTarget::FamilyMod(key.clone()))
                }
                (ImportTarget::FamilyMod(key), 2)
                    if CONSUMER_FUNCTIONS.contains(&path[1].as_str()) =>
                {
                    return Some(ImportTarget::FamilyFn(key.clone()));
                }
                (ImportTarget::FamilyFn(key), 1) => {
                    return Some(ImportTarget::FamilyFn(key.clone()))
                }
                _ => {}
            }
        }
        let families_index = path
            .iter()
            .position(|segment| segment == DERIVED_FAMILIES)?;
        match &path[families_index + 1..] {
            [] => Some(ImportTarget::FamiliesMod),
            [key] if self.known_keys.contains(key) => Some(ImportTarget::FamilyMod(key.clone())),
            [key, func]
                if self.known_keys.contains(key) && CONSUMER_FUNCTIONS.contains(&func.as_str()) =>
            {
                Some(ImportTarget::FamilyFn(key.clone()))
            }
            _ => None,
        }
    }

    fn collect_uses<'b>(&mut self, uses: impl Iterator<Item = &'b syn::ItemUse>) {
        let mut entries = Vec::new();
        for item in uses {
            if has_cfg_false(&item.attrs) {
                continue;
            }
            let mut prefix = Vec::new();
            flatten_use_tree(&item.tree, &mut prefix, &mut entries);
        }
        // Two passes so `use gents_lean_vocab::families; use families::foo as f;`
        // resolves regardless of declaration order within the same scope.
        for _ in 0..2 {
            for (path, local) in &entries {
                let Some(target) = self.classify_use_path(path) else {
                    continue;
                };
                let scope = self.scopes.last_mut().expect("active scope");
                match local {
                    Some(name) => {
                        scope.insert(name.clone(), target);
                    }
                    None => {
                        if let ImportTarget::FamilyMod(key) = target {
                            for func in CONSUMER_FUNCTIONS {
                                scope.insert(
                                    (*func).to_owned(),
                                    ImportTarget::FamilyFn(key.clone()),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    fn resolve_consumer_path(&self, expr_path: &syn::ExprPath) -> Option<String> {
        let segments: Vec<String> = expr_path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect();
        let leading_colon = expr_path.path.leading_colon.is_some();
        match segments.as_slice() {
            [func] if !leading_colon => match self.lookup(func) {
                Some(ImportTarget::FamilyFn(key)) => Some(key.clone()),
                _ => None,
            },
            [module, func] if !leading_colon && CONSUMER_FUNCTIONS.contains(&func.as_str()) => {
                match self.lookup(module) {
                    Some(ImportTarget::FamilyMod(key)) => Some(key.clone()),
                    _ => None,
                }
            }
            [.., families_mod, key, func]
                if CONSUMER_FUNCTIONS.contains(&func.as_str()) && self.known_keys.contains(key) =>
            {
                if families_mod == DERIVED_FAMILIES {
                    return Some(key.clone());
                }
                if segments.len() == 3
                    && !leading_colon
                    && matches!(self.lookup(families_mod), Some(ImportTarget::FamiliesMod))
                {
                    return Some(key.clone());
                }
                None
            }
            _ => None,
        }
    }

    fn record_path(&mut self, expr: &syn::Expr) {
        if let syn::Expr::Path(expr_path) = expr {
            if let Some(key) = self.resolve_consumer_path(expr_path) {
                *self.counts.entry(key).or_default() += 1;
            }
        }
    }
}

impl<'ast> Visit<'ast> for RustConsumerScanner<'_> {
    fn visit_file(&mut self, file: &'ast syn::File) {
        if has_cfg_false(&file.attrs) {
            return;
        }
        self.scopes.push(BTreeMap::new());
        self.collect_uses(file.items.iter().filter_map(|item| match item {
            syn::Item::Use(item_use) => Some(item_use),
            _ => None,
        }));
        visit::visit_file(self, file);
        self.scopes.pop();
    }

    fn visit_item(&mut self, item: &'ast syn::Item) {
        if has_cfg_false(item_attrs(item)) {
            return;
        }
        visit::visit_item(self, item);
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        if has_cfg_false(impl_item_attrs(item)) {
            return;
        }
        visit::visit_impl_item(self, item);
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if has_cfg_false(&module.attrs) {
            return;
        }
        let Some((_, items)) = &module.content else {
            return;
        };
        self.scopes.push(BTreeMap::new());
        self.collect_uses(items.iter().filter_map(|item| match item {
            syn::Item::Use(item_use) => Some(item_use),
            _ => None,
        }));
        visit::visit_item_mod(self, module);
        self.scopes.pop();
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.scopes.push(BTreeMap::new());
        self.collect_uses(block.stmts.iter().filter_map(|stmt| match stmt {
            syn::Stmt::Item(syn::Item::Use(item_use)) => Some(item_use),
            _ => None,
        }));
        visit::visit_block(self, block);
        self.scopes.pop();
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if has_cfg_false(stmt_attrs(stmt)) {
            return;
        }
        visit::visit_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if has_cfg_false(expr_attrs(expr)) {
            return;
        }
        visit::visit_expr(self, expr);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if has_cfg_false(&call.attrs) {
            return;
        }
        self.record_path(&call.func);
        for arg in &call.args {
            if !has_cfg_false(expr_attrs(arg)) {
                self.record_path(arg);
            }
        }
        visit::visit_expr_call(self, call);
    }
}

fn rust_consumer_counts(source: &str, known_keys: &BTreeSet<String>) -> BTreeMap<String, usize> {
    let syntax = syn::parse_file(source).expect("parse Rust source for conformance scan");
    let mut scanner = RustConsumerScanner::new(known_keys);
    scanner.visit_file(&syntax);
    scanner.counts
}

fn strip_ts_comments(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let byte = bytes[i];
        if let Some(delim) = quote {
            out.push(byte as char);
            if byte == b'\\' && i + 1 < bytes.len() {
                i += 1;
                out.push(bytes[i] as char);
            } else if byte == delim {
                quote = None;
            }
            i += 1;
            continue;
        }
        if matches!(byte, b'"' | b'\'' | b'`') {
            quote = Some(byte);
            out.push(byte as char);
            i += 1;
            continue;
        }
        if byte == b'/' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'/' {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if bytes[i + 1] == b'*' {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
                out.push(' ');
                continue;
            }
        }
        let ch = source[i..].chars().next().expect("valid UTF-8");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn ts_consumer_counts(source: &str, known_keys: &BTreeSet<String>) -> BTreeMap<String, usize> {
    let stripped = strip_ts_comments(source);
    let mut counts = BTreeMap::new();
    if !stripped.contains("families::") {
        return counts;
    }
    for key in known_keys {
        let double_quoted = format!("\"families::{key}::cases()\"");
        let single_quoted = format!("'families::{key}::cases()'");
        let found =
            stripped.matches(&double_quoted).count() + stripped.matches(&single_quoted).count();
        if found > 0 {
            counts.insert(key.clone(), found);
        }
    }
    counts
}

pub fn consumer_sites(
    workspace: &Path,
    families: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let prefix = format!("{DERIVED_FAMILIES}/");
    let known_keys: BTreeSet<String> = families
        .iter()
        .filter_map(|family| family.strip_prefix(&prefix).map(str::to_owned))
        .collect();
    let mut sites: BTreeMap<String, BTreeSet<String>> = families
        .iter()
        .map(|family| (family.clone(), BTreeSet::new()))
        .collect();
    let compiled = crate::module_tree::module_tree(workspace).compiled;
    let mut files = Vec::new();
    for root in SOURCE_ROOTS {
        source_files(&workspace.join(root), &mut files);
    }
    let runs = |path: &Path| {
        path.extension().is_none_or(|extension| extension != "rs") || compiled.contains(path)
    };
    for path in files.into_iter().filter(|path| runs(path)) {
        let relative = path
            .strip_prefix(workspace)
            .expect("workspace file")
            .display()
            .to_string();
        if relative.starts_with("crates/gents-lean-contract/src")
            || relative == "crates/gents-lean-vocab/src/consumer_scan.rs"
        {
            continue;
        }
        let source = fs::read_to_string(&path).expect("workspace source");
        let is_rust = path.extension().is_some_and(|extension| extension == "rs");
        let counts = if is_rust {
            if !source.contains("families") {
                continue;
            }
            rust_consumer_counts(&source, &known_keys)
        } else {
            if !source.contains("families::") {
                continue;
            }
            ts_consumer_counts(&source, &known_keys)
        };
        for (key, calls) in counts {
            let family = format!("{DERIVED_FAMILIES}/{key}");
            if let Some(found) = sites.get_mut(&family) {
                found.extend((1..=calls).map(|call| format!("{relative}#{call}")));
            }
        }
    }
    sites
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    const PREMISE_PREFIX: &str = "boundary.";

    /// Each name is an emitted family that no native test replays. The list
    /// only shrinks: a family leaves it when its consumer is written or Lean
    /// stops emitting it, and a new family arrives with its consumer.
    const OPEN: &[&str] = &[];

    struct Scan {
        families: BTreeSet<String>,
        sites: BTreeMap<String, BTreeSet<String>>,
    }

    impl Scan {
        fn consumed(&self, family: &str) -> bool {
            self.sites
                .get(family)
                .is_some_and(|found| !found.is_empty())
        }
    }

    fn scan() -> &'static Scan {
        static SCAN: OnceLock<Scan> = OnceLock::new();
        SCAN.get_or_init(|| {
            let json = gents_lean_contract::load_contract_json().expect("Lean contract JSON");
            let snapshot: Value = serde_json::from_str(&json).expect("snapshot JSON");
            let families = emitted_families(&snapshot);
            let sites = consumer_sites(&workspace(), &families);
            Scan { families, sites }
        })
    }

    fn workspace() -> std::path::PathBuf {
        std::path::PathBuf::from(
            std::env::var_os("CARGO_MANIFEST_DIR").expect("runtime CARGO_MANIFEST_DIR"),
        )
        .join("../..")
        .canonicalize()
        .expect("workspace root")
    }

    fn is_unreached(family: &str) -> bool {
        family.ends_with("_unreached") || family.ends_with("Unreached")
    }

    #[test]
    fn commented_out_call_is_not_counted() {
        let keys = BTreeSet::from(["foo".to_owned()]);
        let source = r#"
            // families::foo::replay(|_| ());
            /* gents_lean_vocab::families::foo::cases() */
            const DOC: &str = "families::foo::replay()";
        "#;
        assert!(rust_consumer_counts(source, &keys).is_empty());
    }

    #[test]
    fn unused_family_alias_is_not_counted() {
        let keys = BTreeSet::from(["foo".to_owned()]);
        let source = r#"
            use gents_lean_vocab::families::foo as family;
            fn only_uses_types(_: family::Input) {}
        "#;
        assert!(rust_consumer_counts(source, &keys).is_empty());
    }

    #[test]
    fn cfg_false_call_is_not_counted() {
        let keys = BTreeSet::from(["foo".to_owned(), "bar".to_owned(), "baz".to_owned()]);
        let source = r#"
            #[cfg(false)]
            fn dead_fn() {
                gents_lean_vocab::families::foo::replay(|_| ());
            }

            #[cfg(all(test, false))]
            mod dead_mod {
                use gents_lean_vocab::families::bar as family;
                fn inner() {
                    family::cases();
                }
            }

            fn live_fn() {
                #[cfg(false)]
                gents_lean_vocab::families::baz::cases();
            }
        "#;
        assert!(rust_consumer_counts(source, &keys).is_empty());
    }

    #[test]
    fn aliased_and_grouped_calls_are_counted() {
        let keys = BTreeSet::from(["foo".to_owned(), "bar".to_owned(), "baz".to_owned()]);
        let source = r#"
            use gents_lean_vocab::families::{foo as family, bar::{cases as bar_cases}};

            fn first() {
                family::replay::<String>(|_| ());
            }

            fn second() {
                let _ = bar_cases();
            }

            fn third() {
                use gents_lean_vocab::families::baz as family;
                let _ = spawn_blocking(family::cases::<String>);
            }
        "#;
        assert_eq!(
            rust_consumer_counts(source, &keys),
            BTreeMap::from([
                ("bar".to_owned(), 1),
                ("baz".to_owned(), 1),
                ("foo".to_owned(), 1),
            ])
        );
    }

    #[test]
    fn ts_comments_are_stripped_before_matching() {
        let keys = BTreeSet::from(["live_family".to_owned(), "dead_family".to_owned()]);
        let source = r#"
            // "families::dead_family::cases()"
            /* "families::dead_family::cases()" */
            const url = "https://example.com";
            const cases = load("families::live_family::cases()");
        "#;
        assert_eq!(
            ts_consumer_counts(source, &keys),
            BTreeMap::from([("live_family".to_owned(), 1)])
        );
    }

    #[test]
    fn every_emitted_family_has_one_consumer_or_a_ledger_entry() {
        let scan = scan();
        let ledger = &crate::lean_contract_snapshot().coverage_ledger;
        let names = ledger
            .boundaries
            .iter()
            .map(|boundary| boundary.name.as_str())
            .filter(|name| !name.starts_with(PREMISE_PREFIX))
            .chain(ledger.open.iter().map(String::as_str));

        let mut listed = BTreeSet::new();
        let mut refused = Vec::new();
        for name in names {
            match ledger_family(name, &scan.families) {
                Some(family) if !scan.consumed(&family) => {
                    listed.insert(family);
                }
                _ => refused.push(name),
            }
        }
        assert!(
            refused.is_empty(),
            "coverage ledger entries name families that a test replays or the snapshot \
             does not emit; delete them from Proofs/Conformance/CoverageLedger.lean: {refused:?}"
        );

        let premises_naming_families: Vec<&str> = ledger
            .boundaries
            .iter()
            .map(|boundary| boundary.name.as_str())
            .filter(|name| name.starts_with(PREMISE_PREFIX))
            .filter(|name| ledger_family(name, &scan.families).is_some())
            .collect();
        assert!(
            premises_naming_families.is_empty(),
            "a boundary named `{PREMISE_PREFIX}*` is a premise, not a family: {premises_naming_families:?}"
        );

        let unlisted: Vec<&str> = scan
            .families
            .iter()
            .filter(|family| !scan.consumed(family) && !listed.contains(*family))
            .map(String::as_str)
            .collect();
        assert!(
            unlisted.is_empty(),
            "no native test replays these emitted families; write each family's consumer \
             in the same change as the family: {unlisted:?}"
        );

        let prefix = format!("{DERIVED_FAMILIES}/");
        let replayed_twice: Vec<_> = scan
            .sites
            .iter()
            .filter(|(family, found)| family.starts_with(&prefix) && found.len() > 1)
            .collect();
        assert!(
            replayed_twice.is_empty(),
            "a derived family is replayed by exactly one test: {replayed_twice:?}"
        );
    }

    #[test]
    fn open_only_shrinks() {
        let ledger = &crate::lean_contract_snapshot().coverage_ledger;
        let open: BTreeSet<&str> = ledger.open.iter().map(String::as_str).collect();
        let pinned: BTreeSet<&str> = OPEN.iter().copied().collect();
        let added: Vec<_> = open.difference(&pinned).collect();
        assert!(
            added.is_empty(),
            "the coverage ledger's open list only shrinks; write these families' consumers \
             in the same change as the families: {added:?}"
        );
        let removed: Vec<_> = pinned.difference(&open).collect();
        assert!(
            removed.is_empty(),
            "these families left the coverage ledger's open list; delete them from OPEN: {removed:?}"
        );
    }

    #[test]
    fn no_new_family_splits_off_its_unreached_cases() {
        let scan = scan();
        let ledger = &crate::lean_contract_snapshot().coverage_ledger;
        let allowed: BTreeSet<String> = OPEN
            .iter()
            .copied()
            .chain(
                ledger
                    .boundaries
                    .iter()
                    .map(|boundary| boundary.name.as_str()),
            )
            .filter_map(|name| ledger_family(name, &scan.families))
            .collect();
        let prefix = format!("{DERIVED_FAMILIES}/");
        let split: Vec<&str> = scan
            .families
            .iter()
            .filter(|family| is_unreached(family.strip_prefix(&prefix).unwrap_or(family.as_str())))
            .filter(|family| !allowed.contains(*family))
            .map(String::as_str)
            .collect();
        assert!(
            split.is_empty(),
            "an owner emits one family over the cases production reaches, never an \
             `_unreached` complement; fold these cases into the replayed family or keep \
             them as Lean theorems: {split:?}"
        );
    }

    #[test]
    fn feature_surfaces_name_consumed_families() {
        let scan = scan();
        let unproven: Vec<_> = crate::lean_feature_surface_requirements()
            .iter()
            .flat_map(|requirement| {
                requirement
                    .required
                    .iter()
                    .map(move |evidence| (requirement.feature.as_str(), evidence))
            })
            .filter(|(_, evidence)| {
                !ledger_family(&evidence.family, &scan.families)
                    .is_some_and(|family| scan.consumed(&family))
            })
            .map(|(feature, evidence)| (feature, evidence.surface, evidence.family.as_str()))
            .collect();
        assert!(
            unproven.is_empty(),
            "a required feature surface names a family a native test replays: {unproven:?}"
        );
    }
}
