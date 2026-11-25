use super::support::{cfg_test, item_cfg_test, named_exceptions, repo_root, Workspace};
use quote::ToTokens;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use syn::visit::{self, Visit};

/// The standard fixtures and the names their duplicates carried. Each has one
/// definition in the workspace: a second is a copy a caller should replace
/// with the standard one.
const FIXTURE_NAMES: &[&str] = &[
    "TestStore",
    "SignedHome",
    "ClaimedRequest",
    "AgentHome",
    "ScriptedBackend",
    "HeldCall",
    "TestDb",
    "test_db",
    "spawn_runtime",
    "node_builder",
    "managed_agent",
    "ManagedAgentFixture",
    "test_identity",
];

/// Names defined more than once, with the count of definitions; each only
/// shrinks as its callers move to the standard fixture.
#[rustfmt::skip]
const FIXTURE_DUPLICATES: &[(&str, usize, &str)] = &[
    ("ManagedAgentFixture", 2, "root tests/support and the automation pushdown tests each install a managed Agent; both move to SignedHome::create_agent"),
    ("managed_agent", 4, "root tests/support and automation pushdown copies of Agent installation; callers move to SignedHome::create_agent"),
    ("node_builder", 3, "raw-node builders in gents-store, gents-host and gents-desktop-bridge; callers move to TestStore"),
    ("spawn_runtime", 2, "two root copies of the runtime boot; they move to the running-runtime fixture"),
    ("test_identity", 7, "key-file identities written per crate; callers move to gents_store::test_support::test_identity"),
];

/// `src` files that compile a `tests/` file into the library.
#[rustfmt::skip]
const TESTS_INCLUDED_INTO_SRC: &[(&str, usize, &str)] = &[
    ("crates/gents-session/src/session/provider_context_reduction.rs", 1, "a durable-reduction conformance consumer kept in the root tests directory; it moves beside its owner in gents-session"),
];

/// Test-only implementations of production traits outside a `src/test_support`
/// module, per crate.
#[rustfmt::skip]
const TEST_TRAIT_IMPLEMENTATIONS: &[(&str, usize, &str)] = &[
    ("gents", 43, "doubles of owner traits written beside their tests; drive the production owner or move the double into the crate's src/test_support"),
    ("gents-cli", 2, "doubles of owner traits written beside their tests; drive the production owner or move the double into the crate's src/test_support"),
    ("gents-desktop-bridge", 4, "doubles of owner traits written beside their tests; drive the production owner or move the double into the crate's src/test_support"),
    ("gents-pack", 5, "doubles of owner traits written beside their tests; drive the production owner or move the double into the crate's src/test_support"),
    ("gents-service", 7, "doubles of owner traits written beside their tests; drive the production owner or move the double into the crate's src/test_support"),
    ("gents-session", 8, "doubles of owner traits written beside their tests; drive the production owner or move the double into the crate's src/test_support"),
    ("gents-tools", 7, "doubles of owner traits written beside their tests; drive the production owner or move the double into the crate's src/test_support"),
];

/// `sleep(` calls in test code, per crate.
#[rustfmt::skip]
const TEST_SLEEPS: &[(&str, usize, &str)] = &[
    ("gents", 6, "sleeps that stand in for an event the test could wait on; each converted wait lowers the count"),
    ("gents-automation", 21, "sleeps that stand in for an event the test could wait on; each converted wait lowers the count"),
    ("gents-cli", 4, "external tiny_http port release, TCP bind readiness, startup-gate HTTP readiness, and a 2 s P2P lsof observation window"),
    ("gents-client-host", 29, "sleeps that stand in for an event the test could wait on; each converted wait lowers the count"),
    ("gents-desktop-bridge", 15, "sleeps that stand in for an event the test could wait on; each converted wait lowers the count"),
    ("gents-fs-runner", 2, "cross-process file-gate polling and wall-budget pause in the synchronous fs-runner"),
    ("gents-inference", 6, "sleeps that stand in for an event the test could wait on; each converted wait lowers the count"),
    ("gents-lean-contract", 1, "cross-process file-lock contention hold"),
    ("gents-lean-vocab", 1, "the sleep-call token matcher literal in fixtures.rs"),
    ("gents-loop", 9, "virtual-time start_paused timers, Utc::now() deadline regression polls, and ScriptedProvider Delay/Repeat steps"),
    ("gents-p2p", 8, "sleeps that stand in for an event the test could wait on; each converted wait lowers the count"),
    ("gents-session", 6, "sleeps that stand in for an event the test could wait on; each converted wait lowers the count"),
    ("gents-store", 8, "bounded child/stdout polling, cross-process marker files, JWT real-clock second/expiry boundaries, and paused virtual-clock tests"),
    ("gents-tools", 44, "sleeps that stand in for an event the test could wait on; each converted wait lowers the count"),
];

struct Source {
    package: String,
    relative: String,
    syntax: syn::File,
    production: bool,
}

fn workspace_sources() -> Vec<Source> {
    let root = repo_root();
    let workspace = Workspace::load(&root);
    let mut sources = Vec::new();
    for (package, (member, _)) in &workspace.members {
        let member = root.join(member);
        let production = production_files(&member);
        let mut files = Vec::new();
        for directory in ["src", "tests"] {
            package_files(&member.join(directory), &mut files);
        }
        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(syntax) = syn::parse_file(&text) else {
                continue;
            };
            sources.push(Source {
                package: package.clone(),
                relative: path
                    .strip_prefix(&root)
                    .expect("source under the repository")
                    .to_string_lossy()
                    .replace('\\', "/"),
                syntax,
                production: production.contains(&path),
            });
        }
    }
    sources
}

fn package_files(directory: &Path, output: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(directory).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !path.join("Cargo.toml").is_file()
                && path
                    .file_name()
                    .is_none_or(|name| name != "target" && name != "node_modules")
            {
                package_files(&path, output);
            }
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            output.push(path);
        }
    }
}

/// The files a member's library, binaries and build script compile outside
/// any test-only `cfg`.
fn production_files(member: &Path) -> BTreeSet<PathBuf> {
    let mut roots = vec![
        member.join("src/lib.rs"),
        member.join("src/main.rs"),
        member.join("build.rs"),
    ];
    for entry in std::fs::read_dir(member.join("src/bin"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        roots.push(if path.is_dir() {
            path.join("main.rs")
        } else {
            path
        });
    }
    let mut files = BTreeSet::new();
    for root in roots.into_iter().filter(|root| root.is_file()) {
        let directory = root.parent().expect("root in a directory").to_path_buf();
        follow_module(&root, &directory, &mut files);
    }
    files
}

fn follow_module(file: &Path, children: &Path, files: &mut BTreeSet<PathBuf>) {
    if !files.insert(file.to_path_buf()) {
        return;
    }
    let Ok(syntax) = std::fs::read_to_string(file)
        .map_err(|_| ())
        .and_then(|text| syn::parse_file(&text).map_err(|_| ()))
    else {
        return;
    };
    let here = file.parent().expect("file in a directory");
    follow_items(&syntax.items, here, children, files);
}

fn follow_items(items: &[syn::Item], here: &Path, children: &Path, files: &mut BTreeSet<PathBuf>) {
    for item in items {
        let syn::Item::Mod(module) = item else {
            continue;
        };
        if cfg_test(&module.attrs) {
            continue;
        }
        let named = module_path(&module.attrs);
        let name = module.ident.to_string();
        match &module.content {
            Some((_, inner)) => {
                let nested = children.join(named.as_deref().unwrap_or(&name));
                follow_items(inner, &nested, &nested, files);
            }
            None => {
                let target = match &named {
                    Some(named) => here.join(named),
                    None if children.join(format!("{name}.rs")).is_file() => {
                        children.join(format!("{name}.rs"))
                    }
                    None => children.join(&name).join("mod.rs"),
                };
                let nested = if target.file_name().is_some_and(|file| file == "mod.rs") {
                    target
                        .parent()
                        .expect("mod.rs in a directory")
                        .to_path_buf()
                } else {
                    target.with_extension("")
                };
                follow_module(&target, &nested, files);
            }
        }
    }
}

fn module_path(attributes: &[syn::Attribute]) -> Option<String> {
    attributes.iter().find_map(|attribute| {
        let syn::Meta::NameValue(pair) = &attribute.meta else {
            return None;
        };
        if !pair.path.is_ident("path") {
            return None;
        }
        match &pair.value {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(path),
                ..
            }) => Some(path.value()),
            _ => None,
        }
    })
}

fn in_test_support(relative: &str) -> bool {
    relative.contains("/src/test_support/") || relative.ends_with("/src/test_support.rs")
}

/// Visits the items of a source that compile only into tests: every item of
/// a test file, and the test-only items of a production file.
struct TestItems<'a, V> {
    test_file: bool,
    within_test: bool,
    inner: &'a mut V,
}

trait TestItemVisitor {
    fn test_item(&mut self, item: &syn::Item);
}

impl<V> Visit<'_> for TestItems<'_, V>
where
    V: TestItemVisitor,
{
    fn visit_item(&mut self, item: &syn::Item) {
        let entered = !self.within_test && item_cfg_test(item);
        if entered {
            self.within_test = true;
        }
        if self.test_file || self.within_test {
            self.inner.test_item(item);
        }
        visit::visit_item(self, item);
        if entered {
            self.within_test = false;
        }
    }
}

fn visit_test_items<V>(source: &Source, inner: &mut V)
where
    V: TestItemVisitor,
{
    let mut items = TestItems {
        test_file: !source.production,
        within_test: false,
        inner,
    };
    items.visit_file(&source.syntax);
}

#[derive(Default)]
struct Definitions {
    names: BTreeMap<String, Vec<String>>,
    relative: String,
}

impl Visit<'_> for Definitions {
    fn visit_item(&mut self, item: &syn::Item) {
        let name = match item {
            syn::Item::Fn(item) => Some(&item.sig.ident),
            syn::Item::Struct(item) => Some(&item.ident),
            syn::Item::Enum(item) => Some(&item.ident),
            syn::Item::Type(item) => Some(&item.ident),
            _ => None,
        };
        if let Some(name) = name.map(ToString::to_string) {
            if FIXTURE_NAMES.contains(&name.as_str()) {
                self.names
                    .entry(name)
                    .or_default()
                    .push(self.relative.clone());
            }
        }
        visit::visit_item(self, item);
    }
}

#[test]
fn each_fixture_has_one_definition() {
    let mut definitions = Definitions::default();
    for source in workspace_sources() {
        definitions.relative = source.relative;
        definitions.visit_file(&source.syntax);
    }
    let duplicated: BTreeMap<String, usize> = definitions
        .names
        .iter()
        .filter(|(_, sites)| sites.len() > 1)
        .map(|(name, sites)| (name.clone(), sites.len()))
        .collect();
    named_exceptions(
        "one definition per fixture",
        FIXTURE_DUPLICATES,
        &duplicated,
        "definitions",
        &format!(
            "use the standard fixture and delete the copy; definitions: {:?}",
            definitions
                .names
                .iter()
                .filter(|(_, sites)| sites.len() > 1)
                .collect::<BTreeMap<_, _>>()
        ),
    );
}

#[test]
fn src_includes_no_tests_file() {
    let mut includes = BTreeMap::new();
    for source in workspace_sources() {
        if !source.relative.contains("/src/") {
            continue;
        }
        let mut found = 0;
        let mut modules = PathIncludes { found: &mut found };
        modules.visit_file(&source.syntax);
        if found > 0 {
            includes.insert(source.relative, found);
        }
    }
    named_exceptions(
        "src includes no tests/ file",
        TESTS_INCLUDED_INTO_SRC,
        &includes,
        "#[path] modules into tests/",
        "move the fixture into the lowest owning crate's src/test_support",
    );
}

struct PathIncludes<'a> {
    found: &'a mut usize,
}

impl Visit<'_> for PathIncludes<'_> {
    fn visit_item_mod(&mut self, module: &syn::ItemMod) {
        if module_path(&module.attrs).is_some_and(|path| {
            let mut left_src = false;
            Path::new(&path).components().any(|component| {
                left_src |= component == std::path::Component::ParentDir;
                left_src && component.as_os_str() == "tests"
            })
        }) {
            *self.found += 1;
        }
        visit::visit_item_mod(self, module);
    }
}

fn production_traits(sources: &[Source]) -> BTreeSet<String> {
    struct Traits<'a>(&'a mut BTreeSet<String>);
    impl Visit<'_> for Traits<'_> {
        fn visit_item(&mut self, item: &syn::Item) {
            if item_cfg_test(item) {
                return;
            }
            if let syn::Item::Trait(item) = item {
                self.0.insert(item.ident.to_string());
            }
            visit::visit_item(self, item);
        }
    }
    let mut traits = BTreeSet::new();
    for source in sources
        .iter()
        .filter(|source| source.production && !in_test_support(&source.relative))
    {
        Traits(&mut traits).visit_file(&source.syntax);
    }
    traits
}

struct TraitImplementations<'a> {
    traits: &'a BTreeSet<String>,
    found: usize,
}

impl TestItemVisitor for TraitImplementations<'_> {
    fn test_item(&mut self, item: &syn::Item) {
        if let syn::Item::Impl(syn::ItemImpl {
            trait_: Some((_, path, _)),
            ..
        }) = item
        {
            if path
                .segments
                .last()
                .is_some_and(|segment| self.traits.contains(&segment.ident.to_string()))
            {
                self.found += 1;
            }
        }
    }
}

#[test]
fn production_traits_have_test_implementations_only_in_test_support() {
    let sources = workspace_sources();
    let traits = production_traits(&sources);
    let mut counts = BTreeMap::new();
    let mut files = BTreeMap::<String, Vec<String>>::new();
    for source in sources
        .iter()
        .filter(|source| !in_test_support(&source.relative))
    {
        let mut implementations = TraitImplementations {
            traits: &traits,
            found: 0,
        };
        visit_test_items(source, &mut implementations);
        if implementations.found > 0 {
            *counts.entry(source.package.clone()).or_default() += implementations.found;
            files
                .entry(source.package.clone())
                .or_default()
                .push(format!("{} ({})", source.relative, implementations.found));
        }
    }
    named_exceptions(
        "test implementations of production traits live in src/test_support",
        TEST_TRAIT_IMPLEMENTATIONS,
        &counts,
        "test-only implementations of production traits outside src/test_support",
        &format!(
            "drive the production owner, or move the double into the crate's src/test_support; sites: {files:?}"
        ),
    );
}

/// The token text of a source's test code: a whole test file, or each
/// outermost test-only item of a production file.
fn test_code(source: &Source) -> Vec<String> {
    fn items(within: &[syn::Item], output: &mut Vec<String>) {
        for item in within {
            if item_cfg_test(item) {
                output.push(item.to_token_stream().to_string());
            } else if let syn::Item::Mod(syn::ItemMod {
                content: Some((_, inner)),
                ..
            }) = item
            {
                items(inner, output);
            }
        }
    }
    if !source.production {
        return vec![source.syntax.to_token_stream().to_string()];
    }
    let mut output = Vec::new();
    items(&source.syntax.items, &mut output);
    output
}

#[test]
fn test_sleeps_only_shrink() {
    let mut counts = BTreeMap::new();
    for source in workspace_sources() {
        let found: usize = test_code(&source)
            .iter()
            .map(|tokens| tokens.matches("sleep (").count())
            .sum();
        if found > 0 {
            *counts.entry(source.package.clone()).or_default() += found;
        }
    }
    named_exceptions(
        "sleeps in test code only shrink",
        TEST_SLEEPS,
        &counts,
        "sleep( calls in test code",
        "wait on the event the test observes (`read_when_changed`, `rows_when`, a held call's `entered()`), never a sleep",
    );
}
