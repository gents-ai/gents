import Proofs.Conformance.CanonicalOutputWire
import Proofs.Conformance.ContractCases.CompactionCanonicalProjection

namespace Conformance.CompactionCanonicalProjection

open Lean Conformance.ContractCases.CompactionCanonicalProjection

structure CanonicalInput where
  name : String
  session : Nat
  request : Nat
  segments : Json
  messages : Json
  source : List Nat
  contextWindow : Nat
  thresholdBasisPoints : Nat
  configuredMaxOutputTokens : Nat
  canFit : Bool
  prefixLength : Nat
  checkpoint : Nat
  initialEstimateTokens : Nat
  rebuiltEstimateTokens : Nat
  deriving ToJson

structure CanonicalObservation where
  initialNative : Option Json
  rebuiltCheckpoint : Option Nat
  rebuiltNative : Option Json
  result : String
  outputTokens : Option Nat
  deriving ToJson

def caseNamed (name : String) : Option CanonicalCase :=
  canonicalCases.find? (·.name == name)

theorem canonical_case_names_are_unique :
    (canonicalCases.map (·.name)).eraseDups.length = canonicalCases.length := by
  native_decide

/-- model-production-gap: production composes canonical reconstruction,
provider projection, token estimation and reduction inside gents-loop request
assembly over the serialized provider request; no production function takes a
canonical world, selected identities and supplied estimates and returns the
native messages handed to initial projection and checkpoint rebuilding. -/
@[conformance_family]
def unreached : Family CanonicalInput (Option CanonicalObservation) where
  inputs := canonicalCases.map fun w =>
    { name := w.name, session := w.world.sessionId, request := w.world.requestId
      segments := CanonicalOutputWire.segments w.world.segments
      messages := CanonicalOutputWire.messages w.world.messages
      source := w.source, contextWindow := w.contextWindow
      thresholdBasisPoints := w.thresholdBasisPoints
      configuredMaxOutputTokens := w.configuredMaxOutputTokens, canFit := w.canFit
      prefixLength := w.prefixLength, checkpoint := w.checkpoint
      initialEstimateTokens := w.initialEstimateTokens
      rebuiltEstimateTokens := w.rebuiltEstimateTokens }
  model := fun i => (caseNamed i.name).map fun w =>
    let result := canonicalRun w
    let rebuilt := rebuiltNative result
    { initialNative := (canonicalInputNative w).map CanonicalOutputWire.reconstructedList
      rebuiltCheckpoint := rebuilt.map (·.1)
      rebuiltNative := rebuilt.map fun value => CanonicalOutputWire.reconstructedList value.2
      result := canonicalResultName result
      outputTokens := canonicalOutputTokens result }

end Conformance.CompactionCanonicalProjection
