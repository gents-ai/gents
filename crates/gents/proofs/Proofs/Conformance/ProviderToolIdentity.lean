import Proofs.Conformance.ContractCases.PromptAssembly
import Proofs.Conformance.Family

namespace Conformance.ProviderToolIdentity

open Lean PromptAssembly.ToolCallIdentity

structure IdentityWire where
  id : String
  callId : Option String
  openaiItemId : Option String
  deriving ToJson

def IdentityWire.ofModel (identity : Identity) : IdentityWire :=
  { id := identity.nativeId, callId := identity.callId
    openaiItemId := openAiItemId identity.nativeId }

def IdentityWire.toModel (wire : IdentityWire) : Identity :=
  { nativeId := wire.id, callId := wire.callId }

structure HistoryInput where
  name : String
  responses : Bool
  input : List IdentityWire
  deriving ToJson

def HistoryInput.ofCase (row : ContractCases.ProviderToolIdentityCase) : HistoryInput :=
  { name := row.name, responses := row.responses, input := row.input.map IdentityWire.ofModel }

def HistoryInput.projected (i : HistoryInput) : List IdentityWire :=
  i.input.map fun wire => IdentityWire.ofModel (project i.responses wire.toModel)

/-- A Responses provider always projects call IDs, so only histories marked
for a Responses wire can be dispatched through one. -/
def responsesCases : List HistoryInput :=
  (ContractCases.providerToolIdentityCases.filter (·.responses)).map HistoryInput.ofCase

@[conformance_family]
def normalized : Family HistoryInput (List IdentityWire) where
  inputs := ContractCases.providerToolIdentityCases.map HistoryInput.ofCase
  model := HistoryInput.projected

@[conformance_family]
def projected : Family HistoryInput (List IdentityWire) where
  inputs := responsesCases
  model := HistoryInput.projected

@[conformance_family]
def assembled : Family HistoryInput (List IdentityWire) where
  inputs := responsesCases
  model := HistoryInput.projected

@[conformance_family]
def continued : Family HistoryInput (List (Option String)) where
  inputs := responsesCases
  model := fun i => i.projected.map (·.callId)

end Conformance.ProviderToolIdentity
