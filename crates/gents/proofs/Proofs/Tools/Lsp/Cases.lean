import Proofs.Lsp.Theorems
import Proofs.ToolPolicy.Meet

namespace Lsp.ContractCases

open Lsp ToolPolicy

structure Case where
  name : String
  lsp : Bool
  fileRank : Nat
  action : String
  mutates : Bool
  source : String
  advertised : Bool
  actionAuthorized : Bool
  applyAuthorized : Bool
  deriving Repr

def fileOfRank : Nat → FileCap
  | 0 => .off
  | 1 => .readOnly
  | _ => .readWrite

def mkAction (action : LspAction) (src : LspMutationSource)
    (lsp : Bool) (file : FileCap) (name : String) : Case :=
  { name := name
  , lsp := lsp
  , fileRank := file.rank
  , action :=
      match action with
      | .diagnostics => "diagnostics"
      | .definition => "definition"
      | .typeDefinition => "typeDefinition"
      | .implementation => "implementation"
      | .references => "references"
      | .hover => "hover"
      | .symbols => "symbols"
      | .status => "status"
      | .capabilities => "capabilities"
      | .reload => "reload"
      | .rename => "rename"
      | .renameFile => "renameFile"
      | .codeActionsList => "codeActionsList"
      | .codeActionsApply => "codeActionsApply"
      | .requestRead => "requestRead"
      | .requestWrite => "requestWrite"
  , mutates := action.mutates
  , source :=
      match src with
      | .foregroundReturnedEdit => "foregroundReturnedEdit"
      | .serverApplyEdit => "serverApplyEdit"
  , advertised := decide (lspAdvertised lsp file)
  , actionAuthorized := decide (lspActionAuthorized lsp file action)
  , applyAuthorized := decide (lspApplyAuthorized lsp file src) }

def cases : List Case :=
  [ mkAction .hover .foregroundReturnedEdit false .readWrite "lsp_false_never_authorized"
  , mkAction .hover .foregroundReturnedEdit true .off "file_off_never_advertised"
  , mkAction .rename .foregroundReturnedEdit true .readOnly "readonly_rejects_rename"
  , mkAction .hover .foregroundReturnedEdit true .readOnly "readonly_allows_hover"
  , mkAction .rename .foregroundReturnedEdit true .readWrite "readwrite_allows_rename"
  , mkAction .rename .serverApplyEdit true .readWrite "server_apply_edit_never"
  , mkAction .rename .foregroundReturnedEdit true .readWrite
      "foreground_edit_authorized" ]

end Lsp.ContractCases
