//! The Rust sources some target of the workspace compiles. Every target root
//! a member's manifest declares or Cargo discovers (library, binaries, tests,
//! examples, benches, build script) is followed through its `mod`
//! declarations, `#[path]` attributes and `include!` calls, whatever their
//! `cfg`. A file named by `include_str!` or `include_bytes!` is data.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use syn::visit::Visit;

#[derive(Debug, Default)]
pub struct ModuleTree {
    pub compiled: BTreeSet<PathBuf>,
    pub data: BTreeSet<PathBuf>,
}

/// Target kinds with the manifest key that turns their discovery off and the
/// directory Cargo discovers them in.
const DISCOVERED: &[(&str, &str, &str)] = &[
    ("bin", "autobins", "src/bin"),
    ("test", "autotests", "tests"),
    ("example", "autoexamples", "examples"),
    ("bench", "autobenches", "benches"),
];

/// The workspace members' directories, from the root manifest.
pub fn workspace_members(workspace: &Path) -> Vec<PathBuf> {
    let manifest = manifest(&workspace.join("Cargo.toml"));
    manifest["workspace"]["members"]
        .as_array()
        .expect("workspace members")
        .iter()
        .map(|member| workspace.join(member.as_str().expect("member path")))
        .collect()
}

pub fn module_tree(workspace: &Path) -> ModuleTree {
    collect_modules(workspace, false)
}

#[cfg(test)]
pub(crate) fn enabled_module_tree(workspace: &Path) -> ModuleTree {
    collect_modules(workspace, true)
}

fn collect_modules(workspace: &Path, enabled_only: bool) -> ModuleTree {
    let mut tree = ModuleTree::default();
    for member in workspace_members(workspace) {
        for root in target_roots(&member) {
            tree.visit_file(&root, module_directory(&root, true), enabled_only);
        }
    }
    tree
}

fn manifest(path: &Path) -> toml::Table {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .parse()
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn target_roots(member: &Path) -> BTreeSet<PathBuf> {
    let manifest = manifest(&member.join("Cargo.toml"));
    let package = manifest.get("package").and_then(toml::Value::as_table);
    let mut candidates = vec![
        manifest
            .get("lib")
            .and_then(|lib| lib.get("path"))
            .and_then(toml::Value::as_str)
            .unwrap_or("src/lib.rs")
            .to_owned(),
        package
            .and_then(|package| package.get("build"))
            .and_then(toml::Value::as_str)
            .unwrap_or("build.rs")
            .to_owned(),
    ];
    for (kind, switch, directory) in DISCOVERED {
        for target in manifest
            .get(*kind)
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            match target.get("path").and_then(toml::Value::as_str) {
                Some(path) => candidates.push(path.to_owned()),
                None => {
                    let name = target["name"].as_str().expect("target name");
                    candidates.push(format!("{directory}/{name}.rs"));
                    candidates.push(format!("{directory}/{name}/main.rs"));
                    if *kind == "bin" {
                        candidates.push("src/main.rs".to_owned());
                    }
                }
            }
        }
        let discovered = package
            .and_then(|package| package.get(*switch))
            .and_then(toml::Value::as_bool)
            != Some(false);
        if discovered {
            if *kind == "bin" {
                candidates.push("src/main.rs".to_owned());
            }
            candidates.extend(discovered_roots(&member.join(directory), member));
        }
    }
    candidates
        .into_iter()
        .map(|candidate| member.join(candidate))
        .filter(|path| path.is_file())
        .collect()
}

/// `<directory>/*.rs` and `<directory>/*/main.rs`, relative to `member`.
fn discovered_roots(directory: &Path, member: &Path) -> Vec<String> {
    let mut roots = Vec::new();
    for entry in fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        let root = if path.is_dir() {
            path.join("main.rs")
        } else {
            path
        };
        if root.is_file() && root.extension().is_some_and(|extension| extension == "rs") {
            let relative = root.strip_prefix(member).expect("root under member");
            roots.push(relative.to_string_lossy().into_owned());
        }
    }
    roots
}

fn parent(file: &Path) -> PathBuf {
    file.parent().expect("source has a parent").to_path_buf()
}

/// The directory a module file's undecorated `mod name;` declarations resolve
/// in: a target root, a `mod.rs` or a `#[path]` file owns its own directory,
/// any other file the directory named after it.
fn module_directory(file: &Path, owns_directory: bool) -> PathBuf {
    match file.file_stem().and_then(|stem| stem.to_str()) {
        Some(stem) if !owns_directory && stem != "mod" => parent(file).join(stem),
        _ => parent(file),
    }
}

fn path_attribute(attributes: &[syn::Attribute]) -> Option<String> {
    attributes.iter().find_map(|attribute| {
        let syn::Meta::NameValue(pair) = &attribute.meta else {
            return None;
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(value),
            ..
        }) = &pair.value
        else {
            return None;
        };
        pair.path.is_ident("path").then(|| value.value())
    })
}

/// The literal path of `include!`, `include_str!` or `include_bytes!`.
fn included(mac: &syn::Macro) -> Option<(String, String)> {
    let name = mac.path.get_ident()?.to_string();
    if !matches!(name.as_str(), "include" | "include_str" | "include_bytes") {
        return None;
    }
    let path = syn::parse2::<syn::LitStr>(mac.tokens.clone()).ok()?;
    Some((name, path.value()))
}

impl ModuleTree {
    fn visit_file(&mut self, file: &Path, directory: PathBuf, enabled_only: bool) {
        let file = normalize(file);
        if !self.compiled.insert(file.clone()) {
            return;
        }
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        let syntax = syn::parse_file(&source)
            .unwrap_or_else(|error| panic!("parse {}: {error}", file.display()));
        if enabled_only && crate::consumer_scan::is_disabled(&syntax.attrs) {
            self.compiled.remove(&file);
            return;
        }
        let base = parent(&file);
        self.visit_items(&file, &base, &directory, &syntax.items, enabled_only);
        let mut macros = Macros {
            enabled_only,
            ..Default::default()
        };
        macros.visit_file(&syntax);
        for (name, path) in macros.found {
            let target = normalize(&base.join(path));
            if name != "include" {
                self.data.insert(target);
            } else if target.is_file() {
                self.visit_file(&target, directory.clone(), enabled_only);
            }
        }
    }

    fn visit_items(
        &mut self,
        file: &Path,
        path_base: &Path,
        directory: &Path,
        items: &[syn::Item],
        enabled_only: bool,
    ) {
        for item in items {
            let syn::Item::Mod(module) = item else {
                continue;
            };
            if enabled_only && crate::consumer_scan::is_disabled(&module.attrs) {
                continue;
            }
            let name = module.ident.to_string();
            let explicit = path_attribute(&module.attrs).map(|path| path_base.join(path));
            match &module.content {
                Some((_, nested)) => {
                    let inner = explicit.unwrap_or_else(|| directory.join(&name));
                    self.visit_items(file, &inner, &inner, nested, enabled_only);
                }
                None => {
                    let target = explicit.clone().or_else(|| {
                        [
                            directory.join(format!("{name}.rs")),
                            directory.join(&name).join("mod.rs"),
                        ]
                        .into_iter()
                        .find(|candidate| candidate.is_file())
                    });
                    match target {
                        Some(target) if target.is_file() => {
                            let directory = module_directory(&target, explicit.is_some());
                            self.visit_file(&target, directory, enabled_only);
                        }
                        _ => panic!("{}: `mod {name};` names no file", file.display()),
                    }
                }
            }
        }
    }
}

#[derive(Default)]
struct Macros {
    found: Vec<(String, String)>,
    enabled_only: bool,
    tests_only: bool,
    in_test: bool,
}

impl<'ast> Visit<'ast> for Macros {
    fn visit_file(&mut self, file: &'ast syn::File) {
        if !self.enabled_only || !crate::consumer_scan::is_disabled(&file.attrs) {
            syn::visit::visit_file(self, file);
        }
    }

    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !self.enabled_only
            || !crate::consumer_scan::is_disabled(crate::consumer_scan::item_attrs(item))
        {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        if !self.enabled_only
            || !crate::consumer_scan::is_disabled(crate::consumer_scan::stmt_attrs(stmt))
        {
            syn::visit::visit_stmt(self, stmt);
        }
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        if !self.enabled_only
            || !crate::consumer_scan::is_disabled(crate::consumer_scan::expr_attrs(expr))
        {
            syn::visit::visit_expr(self, expr);
        }
    }

    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        let outer = self.in_test;
        self.in_test = function.attrs.iter().any(|attr| {
            attr.path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
        });
        if !self.tests_only || self.in_test {
            syn::visit::visit_item_fn(self, function);
        }
        self.in_test = outer;
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if !self.tests_only || self.in_test {
            self.found.extend(included(mac));
        }
    }
}

#[cfg(test)]
pub(crate) fn test_string_includes(file: &syn::File) -> Vec<String> {
    let mut visitor = Macros {
        tests_only: true,
        enabled_only: true,
        ..Default::default()
    };
    visitor.visit_file(file);
    visitor
        .found
        .into_iter()
        .filter_map(|(kind, path)| (kind == "include_str").then_some(path))
        .collect()
}

/// Removes `.` and `..` components without touching the filesystem, so a file
/// reached through two spellings is one entry.
fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normal.pop();
            }
            other => normal.push(other),
        }
    }
    normal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modules_resolve_by_rust_module_rules() {
        let root = std::env::temp_dir().join(format!("module-tree-{}", std::process::id()));
        let file = |path: &str, text: &str| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        file(
            "src/lib.rs",
            r#"mod feature; #[cfg(test)] #[path = "elsewhere/fixture.rs"] mod fixture;
               mod inline { mod nested; } #[cfg(false)] mod inert;"#,
        );
        file(
            "src/feature.rs",
            r#"#[cfg(test)] mod tests; const T: &str = include_str!("feature/data.rs");"#,
        );
        file("src/feature/tests.rs", r#"include!("cases.rs");"#);
        file("src/feature/cases.rs", "fn case() {}");
        file("src/feature/data.rs", "not rust");
        file("src/elsewhere/fixture.rs", "mod sibling;");
        file("src/elsewhere/sibling.rs", "");
        file("src/inline/nested.rs", "");
        file("src/orphan.rs", "");
        file("src/inert.rs", "");
        let mut tree = ModuleTree::default();
        let lib = root.join("src/lib.rs");
        tree.visit_file(&lib, module_directory(&lib, true), false);
        let compiled: BTreeSet<String> = tree
            .compiled
            .iter()
            .map(|path| path.strip_prefix(&root).unwrap().display().to_string())
            .collect();
        let expected = [
            "src/elsewhere/fixture.rs",
            "src/elsewhere/sibling.rs",
            "src/feature.rs",
            "src/feature/cases.rs",
            "src/feature/tests.rs",
            "src/inline/nested.rs",
            "src/inert.rs",
            "src/lib.rs",
        ];
        assert_eq!(compiled, expected.into_iter().map(str::to_owned).collect());
        let mut active = ModuleTree::default();
        active.visit_file(&lib, module_directory(&lib, true), true);
        assert!(!active.compiled.contains(&root.join("src/inert.rs")));
        assert!(tree.data.contains(&root.join("src/feature/data.rs")));
        fs::remove_dir_all(&root).unwrap();
    }
}
