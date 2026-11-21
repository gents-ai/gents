use super::owner_errors::OWNER_MODULES;
use super::support::{
    cfg_test, derives_error, item_cfg_test, macro_expressions, owned_by, parse_production,
    path_name, production_sources, repo_root, toml_file,
};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use syn::{Expr, ExprCall, ExprMethodCall, ItemFn, ItemImpl};

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
        (
            "fn f(e: &anyhow::Error) -> bool { e.downcast_ref::<E>().is_some() }",
            1,
        ),
        ("fn f(e: anyhow::Error) { let _ = e.downcast::<E>(); }", 1),
        ("fn f(e: &anyhow::Error) -> bool { e.is::<E>() }", 1),
        (
            "fn f(e: &dyn Error) { find_source::<E>(e); gents_protocol::error_chain::find_source::<F>(e); }",
            2,
        ),
        ("fn f(e: &Error) -> bool { e.is_conflict() || e.is(x) }", 0),
        (
            "fn f(e: &dyn Error) -> bool { find_map_source(e, |cause| cause.downcast_ref::<E>()).is_some() }",
            1,
        ),
        (
            "fn f(m: &Map) -> Option<&T> { m.get(&id)?.downcast_ref() }",
            0,
        ),
        (
            "fn f(payload: &dyn Any) -> bool { payload.downcast_ref::<String>().is_some() }",
            0,
        ),
        (
            "#[cfg(test)] mod tests { fn f(e: &anyhow::Error) { e.downcast_ref::<E>(); } }",
            0,
        ),
        ("#[test] fn f() { error().downcast_ref::<E>(); }", 0),
        (
            "#[tokio::test] async fn f() { error().downcast_ref::<E>(); }",
            0,
        ),
        // Renaming the receiver hides nothing: an error-typed parameter, an
        // `Err(..)` payload and a downcast to an error type all count.
        (
            "fn f(value: &anyhow::Error) -> bool { value.downcast_ref::<E>().is_some() }",
            1,
        ),
        (
            "fn f(r: Result<(), anyhow::Error>) { if let Err(value) = r { value.is::<E>(); } }",
            1,
        ),
        (
            "fn f(x: &X) -> bool { x.inner.downcast_ref::<ConflictError>().is_some() }",
            1,
        ),
        (
            "#[derive(Debug, thiserror::Error)] struct Marker; fn f(x: &X) -> bool { x.inner.is::<Marker>() }",
            1,
        ),
        (
            "fn f(value: &anyhow::Error) -> Option<&E> { value.downcast_ref() }",
            1,
        ),
        // Nor does a macro.
        (
            "fn f(e: &anyhow::Error) -> bool { matches!(e.downcast_ref::<E>(), Some(_)) }",
            1,
        ),
        (
            "fn f(r: R) -> bool { matches!(r, Err(e) if e.is::<E>()) }",
            1,
        ),
        (
            "fn f(e: &anyhow::Error) { assert!(e.downcast_ref::<E>().is_some(), \"typed\"); }",
            1,
        ),
        (
            "async fn f() { tokio::select! { r = a() => { if let Err(e) = r { e.is::<E>(); } } _ = b() => {} } }",
            1,
        ),
        (
            "macro_rules! typed { ($e:expr) => { $e.downcast_ref::<E>() }; }",
            1,
        ),
        (
            "fn f(e: &anyhow::Error) -> Value { json!({ \"code\": find_source::<E>(e.as_ref()).map(E::code) }) }",
            1,
        ),
        (
            "fn f(e: &anyhow::Error) -> Value { json!({ \"a\": 1, \"code\": find_source::<E,>(e.as_ref()).map(E::code) }) }",
            1,
        ),
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
        assert_eq!(
            check(&[(APPLICATION_FILE, source.as_str())]),
            expected,
            "{body}"
        );
        // The same downcast in a function without the premise counts.
        assert_eq!(
            check(&[(APPLICATION_FILE, body)]),
            counted.clone(),
            "{body}"
        );
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
        check(
            "/// Whether the tag was unknown.\n///\n/// External premise: serde says so only in its message."
        ),
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
