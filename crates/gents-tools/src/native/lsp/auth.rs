use gents_documents::FileToolMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LspAction {
    Diagnostics,
    Definition,
    TypeDefinition,
    Implementation,
    References,
    Hover,
    Symbols,
    Status,
    Capabilities,
    Reload,
    Rename,
    RenameFile,
    CodeActionsList,
    CodeActionsApply,
    RequestRead,
    RequestWrite,
}

impl LspAction {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "diagnostics" => Self::Diagnostics,
            "definition" => Self::Definition,
            "type_definition" | "typeDefinition" => Self::TypeDefinition,
            "implementation" => Self::Implementation,
            "references" => Self::References,
            "hover" => Self::Hover,
            "symbols" => Self::Symbols,
            "status" => Self::Status,
            "capabilities" => Self::Capabilities,
            "reload" => Self::Reload,
            "rename" => Self::Rename,
            "rename_file" | "renameFile" => Self::RenameFile,
            "code_actions" | "codeActions" => Self::CodeActionsList,
            "request" => Self::RequestRead,
            _ => return None,
        })
    }

    pub fn mutates(self) -> bool {
        matches!(
            self,
            Self::Rename | Self::RenameFile | Self::CodeActionsApply | Self::RequestWrite
        )
    }

    pub fn may_cold_start(self) -> bool {
        !matches!(self, Self::Status | Self::Reload)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspMutationSource {
    ForegroundReturnedEdit,
    ServerApplyEdit,
}

pub fn lsp_advertised(lsp: bool, file: FileToolMode) -> bool {
    lsp && !matches!(file, FileToolMode::Off)
}

pub fn lsp_action_authorized(lsp: bool, file: FileToolMode, action: LspAction) -> bool {
    lsp_advertised(lsp, file) && (!action.mutates() || matches!(file, FileToolMode::ReadWrite))
}

pub fn lsp_apply_authorized(lsp: bool, file: FileToolMode, src: LspMutationSource) -> bool {
    lsp_advertised(lsp, file)
        && matches!(file, FileToolMode::ReadWrite)
        && matches!(src, LspMutationSource::ForegroundReturnedEdit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readonly_rejects_rename() {
        assert!(!lsp_action_authorized(
            true,
            FileToolMode::ReadOnly,
            LspAction::Rename
        ));
    }

    #[test]
    fn lsp_false_never_authorized() {
        assert!(!lsp_action_authorized(
            false,
            FileToolMode::ReadWrite,
            LspAction::Hover
        ));
    }

    #[test]
    fn server_apply_edit_never_authorized() {
        assert!(!lsp_apply_authorized(
            true,
            FileToolMode::ReadWrite,
            LspMutationSource::ServerApplyEdit
        ));
    }

    #[test]
    fn foreground_edit_authorized_when_advertised_readwrite() {
        assert!(lsp_apply_authorized(
            true,
            FileToolMode::ReadWrite,
            LspMutationSource::ForegroundReturnedEdit
        ));
    }
}

#[cfg(test)]
mod conformance {
    use super::{
        lsp_action_authorized, lsp_advertised, lsp_apply_authorized, LspAction, LspMutationSource,
    };
    use gents_documents::FileToolMode;
    use gents_lean_vocab::lean_lsp_action_cases;

    #[test]
    fn lsp_actions_mutate_exactly_as_the_lean_model() {
        gents_lean_vocab::families::lsp_lsp_action_mutates::replay(|i| {
            LspAction::mutates(i.lsp_action)
        });
    }

    fn file_from_rank(rank: u8) -> FileToolMode {
        match rank {
            0 => FileToolMode::Off,
            1 => FileToolMode::ReadOnly,
            2 => FileToolMode::ReadWrite,
            rank => panic!("unknown Lean file rank {rank}"),
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

    #[test]
    fn generated_lsp_action_cases_match_rust_authorization() {
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
}
