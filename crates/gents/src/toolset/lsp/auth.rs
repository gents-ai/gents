use crate::tool_surface::FileToolMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

pub fn lsp_apply_authorized(
    lsp: bool,
    file: FileToolMode,
    src: LspMutationSource,
) -> bool {
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
