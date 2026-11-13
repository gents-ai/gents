import Proofs.Conformance.Family
import Proofs.Tools.Lsp.Cases

namespace Conformance.Contracts

open Lean
open Lsp
open Lsp.ContractCases

structure LspActionInput where
  name : String
  lsp : Bool
  fileRank : Nat
  action : String
  source : String
  deriving ToJson

structure LspAuthorization where
  mutates : Bool
  advertised : Bool
  actionAuthorized : Bool
  applyAuthorized : Bool
  deriving ToJson

/-- The action names the old emitter printed; the family keeps those wire values. -/
def actionOfWire : String → LspAction
  | "diagnostics" => .diagnostics
  | "definition" => .definition
  | "typeDefinition" => .typeDefinition
  | "implementation" => .implementation
  | "references" => .references
  | "hover" => .hover
  | "symbols" => .symbols
  | "status" => .status
  | "capabilities" => .capabilities
  | "reload" => .reload
  | "rename" => .rename
  | "renameFile" => .renameFile
  | "codeActionsList" => .codeActionsList
  | "codeActionsApply" => .codeActionsApply
  | "requestRead" => .requestRead
  | "requestWrite" => .requestWrite
  | _ => .hover

/-- The mutation source names the old emitter printed. -/
def sourceOfWire : String → LspMutationSource
  | "serverApplyEdit" => .serverApplyEdit
  | _ => .foregroundReturnedEdit

@[conformance_family]
def lspActionAuthorization : Family LspActionInput LspAuthorization where
  inputs := Lsp.ContractCases.cases.map fun c =>
    { name := c.name
      lsp := c.lsp
      fileRank := c.fileRank
      action := c.action
      source := c.source }
  model := fun i =>
    let file := fileOfRank i.fileRank
    let action := actionOfWire i.action
    { mutates := action.mutates
      advertised := decide (lspAdvertised i.lsp file)
      actionAuthorized := decide (lspActionAuthorized i.lsp file action)
      applyAuthorized := decide (lspApplyAuthorized i.lsp file (sourceOfWire i.source)) }

end Conformance.Contracts
