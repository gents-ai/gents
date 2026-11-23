use super::support::{
    cfg_test, item_cfg_test, named_exceptions, production_site_counts, repo_root, Workspace,
};
use std::collections::BTreeSet;
use syn::visit::{self, Visit};

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
/// Each exception names why its reads are legitimately external.
#[rustfmt::skip]
const ENVIRONMENT_READ_EXCEPTIONS: &[(&str, usize, &str)] = &[
    ("apps/gents-desktop/src-tauri/src/bridge/browser.rs", 1, "HOME: expands a leading ~ in a user-typed path; legitimately external (platform discovery)"),
    ("apps/gents-desktop/src-tauri/src/bridge/links.rs", 1, "APPIMAGE: AppImage probe deciding how links open from a packaged Linux build; legitimately external"),
    ("apps/gents-desktop/src-tauri/src/runner/live_fixture/backend.rs", 1, "the API key variable a live-fixture backend names: the runner checks it is set before seeding a backend the runtime will read it from by name; legitimately external"),
    ("crates/gents-chatgpt-login/src/lib.rs", 1, "CODEX_CA_CERT and SSL_CERT_FILE: standard custom CA bundle convention; legitimately external"),
    ("crates/gents-cli/src/commands/chat/streaming.rs", 1, "NO_COLOR: standard convention disabling terminal colour; legitimately external"),
    ("crates/gents-cli/src/commands/eval/frame.rs", 1, "NO_COLOR: standard convention disabling terminal colour; legitimately external"),
    ("crates/gents-cli/src/commands/init.rs", 1, "HOME: platform discovery for the default tool root before any configuration exists; legitimately external"),
    ("crates/gents-cli/src/commands/pack/registry.rs", 1, "GENTS_REGISTRY_TOKEN: registry credential kept out of argv and shell history; legitimately external"),
    ("crates/gents-cli/src/commands/pack/scenario/load.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-cli/src/commands/pack/scenario/setup.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-cli/src/desired_state/load.rs", 1, "desired-state ${VAR} interpolation: the manifest names operator-supplied variables; legitimately external"),
    ("crates/gents-desktop-bridge/src/host_browser.rs", 4, "BROWSER, PATH, APPDIR and APPIMAGE: platform browser discovery and the AppImage probe; legitimately external"),
    ("crates/gents-desktop-bridge/src/package_tools.rs", 6, "PATH, APPDIR, APPIMAGE and the launcher's variables: restores the host environment for host programs launched from an AppImage; legitimately external"),
    ("crates/gents-desktop-bridge/src/tauri_commands/service_executable.rs", 3, "PATH, APPDIR and APPIMAGE: discovery of the gents executable and the AppImage probe; legitimately external"),
    ("crates/gents-desktop/src/main.rs", 1, "PATH: discovery of the desktop executable; legitimately external"),
    ("crates/gents-documents/src/inference_backend.rs", 1, "the variable an InferenceBackend document names as its API key source (auth kind environment); the document decides, legitimately external"),
    ("crates/gents-eval/src/optimization/driver.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-eval/src/runner/embedded/executor/install.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-eval/src/runner/freeze/pack.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-host/src/serve.rs", 2, "the secret named by the metrics token flag (legitimately external) and HOME for the default working root (platform discovery)"),
    ("crates/gents-inference/src/clients/claude_messages/transport.rs", 2, "HTTPS_PROXY/https_proxy and NO_PROXY: standard proxy convention; legitimately external"),
    ("crates/gents-pack/src/operations/pack.rs", 1, "pack variables whose declared source is the installer's own environment (Lean Pack.Operations.planEnvironment); legitimately external"),
    ("crates/gents-pack/src/pack/installation/host_roots.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-pack/src/pack/interpolate.rs", 1, "pack manifest ${VAR} interpolation: the pack author names operator-supplied variables; legitimately external"),
    ("crates/gents-pack/src/plugin/host_imports.rs", 2, "passes the variables an installed plugin's consented env access names into its sandbox; the consent document decides, legitimately external"),
    ("crates/gents-service/src/native_service.rs", 7, "HOME, XDG_CONFIG_HOME, PATH, APPDIR and APPIMAGE: platform discovery of the service unit paths and the AppImage probe; legitimately external"),
    ("crates/gents-service/src/telemetry.rs", 1, "OTEL_EXPORTER_OTLP_* and OTEL_SERVICE_NAME: standard OpenTelemetry convention; legitimately external"),
    ("crates/gents-session/src/workspace/adapter.rs", 1, "inherits the process environment for a managed git child, minus the git variables it overrides; legitimately external"),
    ("crates/gents-tools/src/ux_preview/browser.rs", 1, "PATH: platform discovery of a system browser; legitimately external"),
    ("crates/gents/examples/serve_default_agent.rs", 1, "a developer example configured from the shell that runs it, never shipped; legitimately external"),
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
fn runtime_environment_reads_are_named_exceptions() {
    let workspace = Workspace::load(&repo_root());
    let test_only = workspace
        .test_only()
        .iter()
        .map(|name| format!("{}/", workspace.members[name].0))
        .collect::<Vec<_>>();
    named_exceptions(
        "runtime environment read",
        ENVIRONMENT_READ_EXCEPTIONS,
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
