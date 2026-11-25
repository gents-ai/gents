use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use syn::{Expr, ExprCall};

pub(crate) fn repo_root() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("runtime CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("gents-lean-vocab manifest is under <repo>/crates/gents-lean-vocab")
        .to_path_buf()
}

pub(crate) fn rust_sources(root: &Path, output: &mut Vec<PathBuf>) {
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

pub(crate) fn production_sources(root: &Path) -> Vec<PathBuf> {
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
/// (`test_fixtures_stay_dev_only`, `service_isolation_stays_test_only`,
/// `native_e2e_stays_test_only`).
pub(crate) fn cfg_test(attributes: &[syn::Attribute]) -> bool {
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

pub(crate) fn path_name(call: &ExprCall) -> Option<String> {
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

pub(crate) fn toml_file(path: &Path) -> toml::Table {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .parse()
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

pub(crate) fn package_name(
    key: &str,
    specification: &toml::Value,
    inherited: &toml::Table,
) -> String {
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

pub(crate) fn dependency_names(
    manifest: &toml::Table,
    inherited: &toml::Table,
) -> BTreeSet<String> {
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

pub(crate) fn parse_production(root: &Path, sources: Vec<PathBuf>) -> Vec<(String, syn::File)> {
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

pub(crate) const TEST_FIXTURES: &str = "test-fixtures";

/// The feature end-to-end launchers pass on the command line to build the
/// desktop, bridge and CLI with their native test harness hooks; release
/// builds never pass it (`scripts/check-native-e2e-capabilities.sh` keeps its
/// capability out of the shipped app).
pub(crate) const NATIVE_E2E: &str = "native-e2e";

/// The feature that cuts a test build off the machine's service manager
/// (`gents_service::native_service::isolation`). The per-user service label
/// is not per `HOME`, so a `gents` a test spawns with a temporary `HOME`
/// otherwise addresses the user's own node: on 2026-11-13 test binaries
/// stopped the maintainer's nodes through it.
pub(crate) const SERVICE_ISOLATION: (&str, &str) = ("gents-service", "test-home-guard");

/// The features only test builds enable: `cfg_test` treats what they gate as
/// test code, and `Workspace::production` turns them on only where a
/// compiled member's dependency entry or enabled feature does.
pub(crate) const TEST_FEATURES: [&str; 2] = [TEST_FIXTURES, SERVICE_ISOLATION.1];

pub(crate) struct Workspace {
    pub(crate) inherited: toml::Table,
    pub(crate) members: BTreeMap<String, (String, toml::Table)>,
}

impl Workspace {
    pub(crate) fn load(root: &Path) -> Self {
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
    pub(crate) fn entries(
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
    pub(crate) fn isolates(&self, package: &str, features: &[String]) -> bool {
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
    pub(crate) fn reaches(&self, package: &str, target: &str) -> bool {
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
    pub(crate) fn production(&self) -> BTreeMap<String, String> {
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
    pub(crate) fn test_only(&self) -> BTreeSet<String> {
        let production = self.production();
        self.members
            .keys()
            .filter(|name| !production.contains_key(*name))
            .cloned()
            .collect()
    }
}

pub(crate) fn macro_strings(mut cursor: syn::buffer::Cursor, strings: &mut String) {
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

pub(crate) fn owned_by(relative: &str, owner: &str) -> bool {
    relative == owner
        || relative
            .strip_prefix(owner)
            .is_some_and(|rest| rest == ".rs" || rest.starts_with('/'))
}

pub(crate) fn item_cfg_test(item: &syn::Item) -> bool {
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

pub(crate) fn all_rust_sources(root: &Path, output: &mut Vec<PathBuf>) {
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

/// The failures of a rule whose exceptions are `(path, count, reason)`: a
/// file's sites must equal its exception's count exactly, a file without an
/// exception has none, and every exception carries its reason.
pub(crate) fn exception_failures(
    exceptions: &[(&str, usize, &str)],
    actual: &BTreeMap<String, usize>,
    sites: &str,
    remedy: &str,
) -> Vec<String> {
    let mut named = BTreeMap::new();
    let mut failures = Vec::new();
    for (path, count, reason) in exceptions {
        if named.insert(path.to_string(), *count).is_some() {
            failures.push(format!("{path}: listed twice"));
        }
        if reason.trim().is_empty() {
            failures.push(format!("{path}: the exception names no reason"));
        }
    }
    for path in named.keys().chain(actual.keys()).collect::<BTreeSet<_>>() {
        let found = actual.get(path).copied().unwrap_or_default();
        match named.get(path).copied() {
            None => failures.push(format!("{path}: {found} {sites}; {remedy}")),
            Some(count) if found != count => failures.push(format!(
                "{path}: {found} {sites}, its exception names {count}; {remedy}, or correct the exception"
            )),
            Some(_) => {}
        }
    }
    failures
}

/// Asserts a rule with named exceptions (`exception_failures`); the failure
/// lists the current sites with their reasons to copy from.
pub(crate) fn named_exceptions(
    rule: &str,
    exceptions: &[(&str, usize, &str)],
    actual: &BTreeMap<String, usize>,
    sites: &str,
    remedy: &str,
) {
    let failures = exception_failures(exceptions, actual, sites, remedy);
    let current = actual
        .iter()
        .map(|(path, count)| {
            let reason = exceptions
                .iter()
                .find(|(named, _, _)| named == path)
                .map_or("<reason>", |(_, _, reason)| reason);
            format!("    (\"{path}\", {count}, \"{reason}\"),")
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        failures.is_empty(),
        "{rule} rule failed:\n{}\n\ncurrent sites:\n{current}",
        failures.join("\n")
    );
}

/// Asserts a rule that admits no site.
pub(crate) fn no_sites(rule: &str, actual: &BTreeMap<String, usize>, sites: &str, remedy: &str) {
    let failures = exception_failures(&[], actual, sites, remedy);
    assert!(
        failures.is_empty(),
        "{rule} rule failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn named_exceptions_need_a_reason_and_an_exact_count() {
    let actual = BTreeMap::from([("a.rs".to_owned(), 2)]);
    let failures = |exceptions: &[(&str, usize, &str)]| {
        exception_failures(exceptions, &actual, "site(s)", "remedy").len()
    };
    assert_eq!(failures(&[("a.rs", 2, "why")]), 0);
    assert_eq!(failures(&[]), 1);
    assert_eq!(failures(&[("a.rs", 1, "why")]), 1);
    assert_eq!(failures(&[("a.rs", 3, "why")]), 1);
    assert_eq!(failures(&[("a.rs", 2, " ")]), 1);
    assert_eq!(failures(&[("a.rs", 2, "why"), ("b.rs", 1, "why")]), 1);
    assert_eq!(failures(&[("a.rs", 2, "why"), ("a.rs", 2, "why")]), 1);
}

pub(crate) fn production_site_counts(
    count: impl Fn(&str, &syn::File) -> usize,
) -> BTreeMap<String, usize> {
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

/// The Rust expressions a macro invocation holds. Macro input is not parsed
/// by `syn`, so a site inside `matches!`, `assert!` or `tokio::select!` would
/// otherwise never count. The input is read as an expression list, then as
/// statements; where both fail it is split at `,`, `;`, `=>` and `if`, and a
/// piece that is no expression at its lone `:` (a `json!` key), and each
/// piece is read alone. Pieces that are still not expressions give up their
/// groups to the same reading and their remaining tokens to `loose`.
pub(crate) fn macro_expressions(
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

pub(crate) fn derives_error(attributes: &[syn::Attribute]) -> bool {
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
