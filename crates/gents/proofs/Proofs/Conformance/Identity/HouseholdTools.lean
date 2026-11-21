import Proofs.Conformance.Identity.Conformance
import Proofs.Conformance.Family

namespace Conformance.Household

open Lean Identity Identity.Household

structure McpUseInput where
  sameActor : Bool
  sameChain : Bool
  sameSelection : Bool
  selected : Bool
  serviceVisible : Bool
  enabled : Bool
  deriving ToJson

def mcpUseInputs : List McpUseInput :=
  [⟨true, true, true, true, true, true⟩, ⟨false, true, true, true, true, true⟩,
   ⟨true, false, true, true, true, true⟩, ⟨true, true, false, true, true, true⟩,
   ⟨true, true, true, false, true, true⟩, ⟨true, true, true, true, false, true⟩,
   ⟨true, true, true, true, true, false⟩]

def mcpMayUse (i : McpUseInput) : Bool :=
  McpService.mayUse i.sameActor i.sameChain i.sameSelection i.selected i.serviceVisible i.enabled

/-- The meta-tool fixture holds a static selection whose chain and Tools
selection never change and whose service stays visible; it varies the actor,
the selected service list and the registry's `enabled` field. -/
@[conformance_family]
def mcpServiceLookup : Family McpUseInput Bool where
  inputs := mcpUseInputs.filter fun i => i.sameChain && i.sameSelection && i.serviceVisible
  model := mcpMayUse

/-- The native-policy scenario selects an enabled service through Tools
documents and varies the actor, the selection chain, the native `selected_by`
edge and the Tools selection. -/
@[conformance_family]
def mcpServiceNativePolicy : Family McpUseInput Bool where
  inputs := mcpUseInputs.filter fun i => i.selected && i.enabled
  model := mcpMayUse

structure UxCreateInput where
  name : String
  remote : Bool
  signedGraph : Bool
  deriving ToJson

def uxCreateInputs : List UxCreateInput :=
  [⟨"embedded-held", false, false⟩, ⟨"remote-unsigned", true, false⟩,
   ⟨"remote-approved-graph", true, true⟩]

@[conformance_family]
def uxCreateEmbedded : Family UxCreateInput Bool where
  inputs := uxCreateInputs.filter fun i => !i.remote
  model := fun i => Configuration.uxCreateTransportSupported i.remote i.signedGraph

@[conformance_family]
def uxCreateRemote : Family UxCreateInput Bool where
  inputs := uxCreateInputs.filter fun i => i.remote
  model := fun i => Configuration.uxCreateTransportSupported i.remote i.signedGraph

structure SelectionInput where
  name : String
  actor : String
  revoked : Bool
  deriving ToJson

@[conformance_family]
def toolsSelection : Family SelectionInput Bool where
  inputs := [⟨"selected", "selected", false⟩, ⟨"sibling", "sibling", false⟩,
    ⟨"revoked-context", "selected", true⟩]
  model := fun i =>
    let state := if i.revoked then Household.apply Configuration.toolsSelectionFixture (.revoke
        ⟨"configuration", "context", "selected_by", .entitySet "agents" "agent" "executor"⟩)
      else Configuration.toolsSelectionFixture
    Household.allowed 8 state i.actor "configuration" "tools" "read"

structure SkillVisibility where
  read : Bool
  tool : Bool
  deriving ToJson

@[conformance_family]
def skillSelection : Family SelectionInput SkillVisibility where
  inputs := [⟨"selected", "selected", false⟩, ⟨"sibling", "sibling", false⟩,
    ⟨"revoked", "selected", true⟩]
  model := fun i =>
    let state := if i.revoked then Household.apply Configuration.skillSelectionFixture (.revoke
        ⟨"configuration", "skill", "selected_by", .entitySet "configuration" "context" "selected_by"⟩)
      else Configuration.skillSelectionFixture
    { read := Household.allowed 8 state i.actor "configuration" "skill" "read"
      tool := Configuration.skillToolMayReturn state i.actor "skill" }

end Conformance.Household
