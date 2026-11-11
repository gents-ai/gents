import Proofs.Lsp.Types

namespace Lsp

open ToolPolicy

theorem readonly_rejects_mutating
    (action : LspAction) (h : action.mutates = true) :
    ¬lspActionAuthorized true FileCap.readOnly action := by
  intro happ
  rcases happ with ⟨_, hmut⟩
  simp [h] at hmut

theorem false_lsp_never_authorized (file : FileCap) (action : LspAction) :
    ¬lspActionAuthorized false file action := by
  intro happ
  rcases happ with ⟨hadv, _⟩
  cases hadv.1

theorem off_file_never_advertised (lsp : Bool) :
    ¬lspAdvertised lsp FileCap.off := by
  intro h
  exact h.2 rfl

theorem advertised_readwrite_authorizes
    (action : LspAction) :
    lspActionAuthorized true FileCap.readWrite action := by
  refine ⟨⟨rfl, by decide⟩, ?_⟩
  cases action <;> simp [LspAction.mutates]

theorem server_apply_edit_never_authorized (lsp : Bool) (file : FileCap) :
    ¬lspApplyAuthorized lsp file LspMutationSource.serverApplyEdit := by
  intro happ
  cases happ.2.2

theorem foreground_edit_authorized_when_advertised_readwrite :
    lspApplyAuthorized true FileCap.readWrite
      LspMutationSource.foregroundReturnedEdit := by
  exact ⟨⟨rfl, by decide⟩, ⟨rfl, rfl⟩⟩

end Lsp
