import Proofs.ToolPolicy.Types

namespace Lsp

open ToolPolicy

inductive LspAction where
  | diagnostics
  | definition
  | typeDefinition
  | implementation
  | references
  | hover
  | symbols
  | status
  | capabilities
  | reload
  | rename
  | renameFile
  | codeActionsList
  | codeActionsApply
  | requestRead
  | requestWrite
  deriving DecidableEq, Repr

namespace LspAction

def mutates : LspAction → Bool
  | .rename | .renameFile | .codeActionsApply | .requestWrite => true
  | _ => false

end LspAction

inductive LspMutationSource where
  | foregroundReturnedEdit
  | serverApplyEdit
  deriving DecidableEq, Repr

def lspAdvertised (lsp : Bool) (file : FileCap) : Prop :=
  lsp = true ∧ file ≠ FileCap.off

def lspActionAuthorized (lsp : Bool) (file : FileCap) (action : LspAction) : Prop :=
  lspAdvertised lsp file ∧ (¬action.mutates ∨ file = FileCap.readWrite)

def lspApplyAuthorized (lsp : Bool) (file : FileCap) (src : LspMutationSource) : Prop :=
  lspAdvertised lsp file ∧
    file = FileCap.readWrite ∧
    src = LspMutationSource.foregroundReturnedEdit

instance (lsp : Bool) (file : FileCap) : Decidable (lspAdvertised lsp file) := by
  unfold lspAdvertised
  infer_instance

instance (lsp : Bool) (file : FileCap) (action : LspAction) :
    Decidable (lspActionAuthorized lsp file action) := by
  unfold lspActionAuthorized
  infer_instance

instance (lsp : Bool) (file : FileCap) (src : LspMutationSource) :
    Decidable (lspApplyAuthorized lsp file src) := by
  unfold lspApplyAuthorized
  infer_instance

end Lsp
