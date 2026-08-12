use gents::tool_surface::FileToolMode;
use gents::toolset::{
    lsp_action_authorized, lsp_advertised, lsp_apply_authorized, LspAction, LspMutationSource,
};

use crate::lean_vocab_test::lean_lsp_action_cases;

fn file_from_rank(rank: u8) -> FileToolMode {
    match rank {
        0 => FileToolMode::Off,
        1 => FileToolMode::ReadOnly,
        _ => FileToolMode::ReadWrite,
    }
}

fn action_from_lean(name: &str) -> LspAction {
    match name {
        "diagnostics" => LspAction::Diagnostics,
        "definition" => LspAction::Definition,
        "typeDefinition" => LspAction::TypeDefinition,
        "implementation" => LspAction::Implementation,
        "references" => LspAction::References,
        "hover" => LspAction::Hover,
        "symbols" => LspAction::Symbols,
        "status" => LspAction::Status,
        "capabilities" => LspAction::Capabilities,
        "reload" => LspAction::Reload,
        "rename" => LspAction::Rename,
        "renameFile" => LspAction::RenameFile,
        "codeActionsList" => LspAction::CodeActionsList,
        "codeActionsApply" => LspAction::CodeActionsApply,
        "requestRead" => LspAction::RequestRead,
        "requestWrite" => LspAction::RequestWrite,
        other => panic!("unknown Lean LspAction {other}"),
    }
}

fn source_from_lean(name: &str) -> LspMutationSource {
    match name {
        "foregroundReturnedEdit" => LspMutationSource::ForegroundReturnedEdit,
        "serverApplyEdit" => LspMutationSource::ServerApplyEdit,
        other => panic!("unknown Lean LspMutationSource {other}"),
    }
}

pub(super) fn generated_lsp_action_cases_match_rust_authorization() {
    let cases = lean_lsp_action_cases();
    assert!(
        !cases.is_empty(),
        "no lsp_action_cases emitted by Lean; regenerate the contract snapshot"
    );
    for case in cases {
        let file = file_from_rank(case.file_rank);
        let action = action_from_lean(&case.action);
        let source = source_from_lean(&case.source);
        assert_eq!(
            action.mutates(),
            case.mutates,
            "case {}: mutates mismatch",
            case.name
        );
        assert_eq!(
            lsp_advertised(case.lsp, file),
            case.advertised,
            "case {}: advertised mismatch",
            case.name
        );
        assert_eq!(
            lsp_action_authorized(case.lsp, file, action),
            case.action_authorized,
            "case {}: action_authorized mismatch",
            case.name
        );
        assert_eq!(
            lsp_apply_authorized(case.lsp, file, source),
            case.apply_authorized,
            "case {}: apply_authorized mismatch",
            case.name
        );
    }
}
