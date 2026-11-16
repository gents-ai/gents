import Proofs.Session.PromptAssembly.ContextCalibration
import Proofs.Session.PromptAssembly.ReasoningIdentity
import Proofs.Conformance.Family

namespace Conformance.RequestAssembly

open Lean PromptAssembly.ContextCalibration PromptAssembly.ReasoningIdentity

structure CapacityInput where
  ratio : Nat
  raw : Nat
  contextWindow : Nat
  maxOutputTokens : Nat
  deriving ToJson

structure Dispatched where
  maxOutputTokens : Nat
  contextTokens : Nat
  deriving ToJson

@[conformance_family]
def dispatchCapacity : Family CapacityInput (Option Dispatched) where
  inputs :=
    [10000, 40000].flatMap fun ratio =>
      [1000, 2048].flatMap fun maxOutputTokens =>
        [500, 1200, 2500, 3999, 4000, 5000].map fun raw =>
          { ratio, raw, contextWindow := 4000, maxOutputTokens }
  model := fun i =>
    let s := seed i.ratio []
    (s.dispatchCap i.raw i.contextWindow i.maxOutputTokens).map fun cap =>
      { maxOutputTokens := cap, contextTokens := s.tokens i.raw }

deriving instance ToJson for Rebuild

structure RebuildInput where
  ratio : Nat
  beforeRaw : Nat
  afterRaw : Nat
  contextWindow : Nat
  thresholdBasisPoints : Nat
  maxOutputTokens : Nat
  deriving ToJson

structure RebuildObservation where
  rebuild : Rebuild
  compactorCalls : Nat
  commits : Nat
  replacesSession : Bool
  keepsAnchor : Bool
  deriving ToJson

/-- Every case runs on a session anchored at a call counted as one token, so
the anchor never lowers the count and only its survival is observed. -/
def anchoredSession (ratio : Nat) : State :=
  { seed ratio [] with anchor := some ⟨1, 0, 1⟩ }

@[conformance_family]
def turnRebuild : Family RebuildInput RebuildObservation where
  inputs :=
    [(20000, 30000, 30000), (20000, 40000, 30000), (20000, 40000, 40000),
     (20000, 40000, 100000), (10000, 80000, 40000), (10000, 80000, 76000)].map
      fun (ratio, beforeRaw, afterRaw) =>
        { ratio, beforeRaw, afterRaw, contextWindow := 100000, thresholdBasisPoints := 7500,
          maxOutputTokens := 8000 }
  model := fun i =>
    let r := (anchoredSession i.ratio).rebuild i.beforeRaw i.afterRaw i.contextWindow
      i.thresholdBasisPoints i.maxOutputTokens
    { rebuild := r, compactorCalls := r.compactorCalls, commits := r.commits,
      replacesSession := r.replacesSession, keepsAnchor := r.keepsAnchor }

deriving instance ToJson for EvidenceOrigin, Block

structure IdentityInput where
  origin : EvidenceOrigin
  complete : Bool
  hasIdentity : Bool
  content : List Block
  deriving ToJson

structure IdentityObservation where
  reasoningReplayed : Bool
  retained : List Block
  deriving ToJson

@[conformance_family]
def reasoningIdentity : Family IdentityInput IdentityObservation where
  inputs :=
    [EvidenceOrigin.acceptedProvider, .recoveredProvider].flatMap fun origin =>
      [false, true].flatMap fun complete =>
        [false, true].map fun hasIdentity =>
          { origin, complete, hasIdentity, content := [.reasoning, .text] }
  model := fun i =>
    let replayed := replaysReasoning i.origin i.complete i.hasIdentity
    { reasoningReplayed := replayed, retained := retained replayed i.content }

end Conformance.RequestAssembly
