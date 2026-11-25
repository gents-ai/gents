use super::support::{
    all_rust_sources, cfg_test, derives_error, parse_production, production_sources, repo_root,
};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use syn::visit::{self, Visit};
use syn::{ItemFn, ItemMod};

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
/// whose families replay them. No site is admitted.
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
        "passthrough rule failed:\n{}",
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
