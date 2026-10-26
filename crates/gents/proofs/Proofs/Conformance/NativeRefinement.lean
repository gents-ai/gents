import Proofs.Background.Observation
import Proofs.Background.ProcessControl
import Proofs.PluginJob.TargetResolution
import Proofs.Conformance.ContractTypes

namespace Conformance.NativeRefinement

open Lean Background.Observation PluginJob.TargetResolution

def changeName : Change → String
  | .quiet => "quiet"
  | .unrelated => "unrelated"
  | .relevant => "relevant"
  | .overflow => "overflow"
  | .closed => "closed"

def wakeName : Wake → String
  | .cancel => "cancel"
  | .deadline => "deadline"
  | .rescan => "rescan"
  | .changed => "changed"
  | .wait => "wait"

def observationCases : List (State × Input) := do
  let now ← [0, 4, 5, 9, 10]
  let cancelled ← [false, true]
  let feedOpen ← [false, true]
  let change ← [.quiet, .unrelated, .relevant, .overflow, .closed]
  pure (⟨5, feedOpen⟩, ⟨now, cancelled, 10, change⟩)

def observationJson (test : State × Input) : Json :=
  let (state, input) := test
  Json.mkObj [
    ("rescan_at", toJson state.rescanAt), ("feed_open", toJson state.feedOpen),
    ("now", toJson input.now), ("cancelled", toJson input.cancelled),
    ("deadline_at", toJson input.deadlineAt), ("change", toJson (changeName input.change)),
    ("expected_wake", toJson (wakeName (wake state input))),
    ("expected_feed_open", toJson (observeChange state input.change).feedOpen),
    ("expected_rescan_at", toJson (observeChange state input.change).rescanAt)]

def observationCasesJson : String := (toJson (observationCases.map observationJson)).compress

def bindings : List Binding :=
  [⟨"review-local", "node-local", "reviewer"⟩,
   ⟨"review-remote", "node-remote", "reviewer"⟩,
   ⟨"review-alias", "node-local", "reviewer"⟩,
   ⟨"reviewer", "node-local", "other-agent"⟩]

def bindingJson (binding : Binding) : Json := Json.mkObj [
  ("wire", toJson binding.wire), ("node", toJson binding.node),
  ("agent", toJson binding.agent)]

def targetCasesJson : String :=
  let cases := do
    let table ← [[], bindings.take 3, bindings]
    let wire ← ["review-local", "review-remote", "review-alias", "reviewer", "missing"]
    let ids ← [[], ["review-local"], ["review-remote"], ["reviewer"], ["review-alias"]]
    let target ← [("node-local", "reviewer"), ("node-remote", "reviewer"),
      ("node-local", "other-agent")]
    pure (Json.mkObj [
      ("bindings", toJson (table.map bindingJson)), ("wire", toJson wire),
      ("ids", toJson ids), ("target", toJson [target.1, target.2]),
      ("expected_resolved", match resolve table wire with
        | none => Json.null
        | some pair => toJson [pair.1, pair.2]),
      ("expected_permitted", toJson (permits table ids target))])
  (toJson cases).compress

def processIdentityCasesJson : String :=
  let cases := do
    let ownedId ← ["call", "spawned:parent-doc"]
    let ownedDoc ← ["tool-doc-a", "tool-doc-b"]
    let requestedId ← ["call", "spawned:parent-doc", "parent-doc"]
    let requestedDoc ← ["tool-doc-a", "tool-doc-b", "missing"]
    pure (Json.mkObj [
      ("owned_id", toJson ownedId), ("owned_doc", toJson ownedDoc),
      ("requested_id", toJson requestedId), ("requested_doc", toJson requestedDoc),
      ("expected_cancel", toJson (Background.ProcessControl.sameInvocation
        ⟨requestedId, requestedDoc⟩ ⟨ownedId, ownedDoc⟩))])
  (toJson cases).compress

end Conformance.NativeRefinement
