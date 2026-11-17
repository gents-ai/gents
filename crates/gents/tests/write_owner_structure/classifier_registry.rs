//! Function-scoped registrations for reviewed, compliant classifiers added
//! after the file-count ratchets. Existing debt lists remain unchanged.
use super::*;
use quote::ToTokens;

#[derive(Clone, Copy)]
enum Kind {
    Owner,
    PersistedText,
}

struct Registration {
    path: &'static str,
    function: &'static str,
    result: &'static str,
    kind: Kind,
}

const REGISTRY: &[Registration] = &[
    Registration {
        path: "crates/gents-tools/src/surface/root_admission.rs",
        function: "tool_root_refusal",
        result: "ToolRootRefusal",
        kind: Kind::Owner,
    },
    Registration {
        path: "crates/gents-protocol/src/node_readiness.rs",
        function: "unavailable_rejection_reason",
        result: "AgentReadinessUnavailableReason",
        kind: Kind::PersistedText,
    },
];

fn compact(tokens: impl ToTokens) -> String {
    tokens
        .to_token_stream()
        .to_string()
        .split_whitespace()
        .collect()
}

fn option_result(function: &ItemFn, result: &str, borrowed: bool) -> bool {
    let syn::ReturnType::Type(_, output) = &function.sig.output else {
        return false;
    };
    let syn::Type::Path(path) = output.as_ref() else {
        return false;
    };
    let Some(option) = path.path.segments.last() else {
        return false;
    };
    if option.ident != "Option" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &option.arguments else {
        return false;
    };
    let Some(syn::GenericArgument::Type(mut target)) = args.args.first().cloned() else {
        return false;
    };
    if borrowed {
        let syn::Type::Reference(reference) = target else {
            return false;
        };
        target = *reference.elem;
    }
    compact(target) == result
}

fn compliant(syntax: &syn::File, function: &ItemFn, entry: &Registration) -> bool {
    let exported_result = syntax.items.iter().any(|item| {
        matches!(item, syn::Item::Enum(item)
            if item.ident == entry.result && !matches!(item.vis, syn::Visibility::Inherited))
    });
    if !exported_result || function.sig.asyncness.is_some() || function.sig.inputs.len() != 1 {
        return false;
    }
    let input = compact(&function.sig.inputs);
    let function_file = syn::File {
        shebang: None,
        attrs: Vec::new(),
        items: vec![syn::Item::Fn(function.clone())],
    };
    match entry.kind {
        Kind::Owner => {
            let exported_error = syntax.items.iter().any(|item| {
                matches!(item, syn::Item::Enum(item) if item.ident == entry.result
                    && item.attrs.iter().any(|attr| compact(attr).contains("thiserror::Error")))
            });
            exported_error
                && input.contains("dynstd::error::Error")
                && option_result(function, entry.result, true)
                && downcast_sites(&function_file) == 1
                && compact(&function.block).contains(&format!("find_source::<{}>", entry.result))
        }
        Kind::PersistedText => {
            let premise = function.attrs.iter().any(|attr| {
                attr.path().is_ident("doc")
                    && attr
                        .to_token_stream()
                        .to_string()
                        .contains("External premise:")
            });
            premise
                && input.trim_end_matches(',').ends_with(":&str")
                && option_result(function, entry.result, false)
                && error_text_sites(&function_file).len() == 1
                && downcast_sites(&function_file) == 0
        }
    }
}

/// Strip only a validated registered function before counting legacy debt.
/// Other functions in the same file still count; stale or invalid entries fail.
fn debt_syntax(path: &str, syntax: &syn::File, owner: bool) -> syn::File {
    let mut debt = syntax.clone();
    for entry in REGISTRY
        .iter()
        .filter(|entry| entry.path == path && matches!(entry.kind, Kind::Owner) == owner)
    {
        let function = syntax
            .items
            .iter()
            .find_map(|item| match item {
                syn::Item::Fn(function) if function.sig.ident == entry.function => Some(function),
                _ => None,
            })
            .unwrap_or_else(|| {
                panic!(
                    "stale classifier registration: {}::{}",
                    path, entry.function
                )
            });
        assert!(compliant(syntax, function, entry),
            "registered classifier no longer satisfies its owner/persisted-text contract: {path}::{}", entry.function);
        debt.items.retain(
            |item| !matches!(item, syn::Item::Fn(function) if function.sig.ident == entry.function),
        );
    }
    debt
}

pub(super) fn downcasts(path: &str, syntax: &syn::File) -> usize {
    downcast_sites(&debt_syntax(path, syntax, true))
}

pub(super) fn text_sites(path: &str, syntax: &syn::File) -> Vec<usize> {
    error_text_sites(&debt_syntax(path, syntax, false))
}

#[test]
fn registrations_validate_contracts_and_do_not_hide_other_functions() {
    let owner = &REGISTRY[0];
    let syntax: syn::File = syn::parse_quote! {
        #[derive(thiserror::Error)]
        pub enum ToolRootRefusal { Refused }
        pub fn tool_root_refusal(error: &(dyn std::error::Error + 'static)) -> Option<&ToolRootRefusal> {
            gents_protocol::error_chain::find_source::<ToolRootRefusal>(error)
        }
        fn unrelated(error: &dyn std::error::Error) { find_source::<ForeignError>(error); }
    };
    assert_eq!(downcasts(owner.path, &syntax), 1);
    let mut invalid = syntax.clone();
    if let syn::Item::Enum(item) = &mut invalid.items[0] {
        item.vis = syn::Visibility::Inherited;
    }
    let syn::Item::Fn(function) = &invalid.items[1] else {
        panic!("function fixture")
    };
    assert!(!compliant(&invalid, function, owner));

    let adapter = &REGISTRY[1];
    let syntax: syn::File = syn::parse_quote! {
        pub enum AgentReadinessUnavailableReason { Refused }
        /// External premise: persisted rejection text has no typed reason field.
        pub fn unavailable_rejection_reason(failure_reason: &str) -> Option<AgentReadinessUnavailableReason> {
            if failure_reason.starts_with("prefix") { Some(AgentReadinessUnavailableReason::Refused) } else { None }
        }
        fn unrelated(error: &str) -> bool { error.contains("new debt") }
    };
    assert_eq!(text_sites(adapter.path, &syntax).len(), 1);
    let mut invalid = syntax.clone();
    let syn::Item::Fn(function) = &mut invalid.items[1] else {
        panic!("function fixture")
    };
    function.attrs.clear();
    let function = function.clone();
    assert!(!compliant(&invalid, &function, adapter));
}
