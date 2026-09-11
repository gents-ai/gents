import Proofs.ApplyReconcile.Publication
import Proofs.Conformance.ContractTypes
import Mathlib.Data.Finset.Card

/-! Executable publication witnesses. Fixture rows call `publish` itself; there
is no second installer or per-write ordering contract. Regenerate JSON and replace
the old Rust bridge in the next conformance layer. -/
namespace ApplyReconcile.ContractCases

open Conformance.Contracts

private def manifestOf : List (DocRef × DesiredFields) → Manifest
  | [] => ⟨fun _ => none, ∅, by simp⟩
  | (d, f) :: rest =>
    let m := manifestOf rest
    { docs := fun r => if r = d then some f else m.docs r
      support := insert d m.support
      support_iff := by
        intro r
        by_cases h : r = d
        · simp [h]
        · simp [h, m.support_iff] }

private def row (c : Collection) (id : String) (refs : List DocRef := [])
    (owner : String := "did:owner") : DocRef × DesiredFields :=
  (⟨c, id, owner⟩, { content := id, refs := refs })

private def chain : List (DocRef × DesiredFields) :=
  [row .agentBehavior "behavior" [⟨.agentContext, "context", "did:owner"⟩],
   row .agentContext "context" [⟨.tools, "tools", "did:owner"⟩], row .tools "tools"]

private def cycle : List (DocRef × DesiredFields) :=
  [row .agentBehavior "behavior" [⟨.agentContext, "context", "did:owner"⟩],
   row .agentContext "context" [⟨.tools, "tools", "did:owner"⟩],
   row .tools "tools" [⟨.subagentTarget, "target", "did:owner"⟩],
   row .subagentTarget "target" [⟨.agentBehavior, "behavior", "did:owner"⟩]]

private def missing : List (DocRef × DesiredFields) :=
  [row .agentBehavior "behavior" [⟨.agentContext, "absent", "did:owner"⟩]]

private def foreign : List (DocRef × DesiredFields) :=
  [row .agentBehavior "behavior" [⟨.agentContext, "foreign", "did:other"⟩],
   row .agentContext "foreign" [] "did:other"]

private def independentOwners : List (DocRef × DesiredFields) :=
  [row .tools "mine", row .tools "theirs" [] "did:other"]

private def sameLabels : List (DocRef × DesiredFields) :=
  [row .agentBehavior "coding" [⟨.agentContext, "context", "did:owner"⟩],
   row .agentContext "context", row .agentBehavior "coding"
     [⟨.agentContext, "context", "did:other"⟩] "did:other",
   row .agentContext "context" [] "did:other"]

private def foreignShadow : List (DocRef × DesiredFields) :=
  [row .agentBehavior "coding" [⟨.agentContext, "context", "did:owner"⟩],
   row .agentContext "context" [] "did:other"]

private def scenarios : List (String × List (DocRef × DesiredFields)) :=
  [("closed_chain", chain), ("same_owner_cycle", cycle),
   ("missing_reference_rejected", missing), ("foreign_reference_rejected", foreign),
   ("independent_owners", independentOwners), ("same_labels_distinct_owners", sameLabels),
   ("foreign_same_label_cannot_satisfy_reference", foreignShadow), ("empty_snapshot", [])]

example : (manifestOf chain).referencesClosed = true := by decide
example : (manifestOf cycle).referencesClosed = true := by decide
example : (manifestOf missing).referencesClosed = false := by decide
example : (manifestOf foreign).referencesClosed = false := by decide
example : (manifestOf independentOwners).referencesClosed = true := by decide

example : (manifestOf sameLabels).referencesClosed = true := by decide
example : (manifestOf sameLabels).support.card = 4 := by decide

private def docRefJson (d : DocRef) : String :=
  "{\"collection\":" ++ jsonString (ConfigDocuments.Collection.collectionName d.collection)
    ++ ",\"id\":" ++ jsonString d.id
    ++ ",\"agent_did\":" ++ jsonString d.agentDid ++ "}"

/-- Project fixture keys only; no installer logic is duplicated here. -/
private def desiredJson (keys : List DocRef) (desired : DocRef → Option DesiredFields) : String :=
  jsonArray (keys.filterMap fun d => (desired d).map fun f =>
    "{\"ref\":" ++ docRefJson d
      ++ ",\"content\":" ++ jsonString f.content ++ ",\"refs\":"
      ++ jsonArray ((keys.filter (fun r => r ∈ f.refs)).map docRefJson) ++ "}")

private def liveJson (keys : List DocRef) (live : DocRef → Option LiveFields) : String :=
  jsonArray (keys.filterMap fun d => (live d).map fun value =>
    "{\"ref\":" ++ docRefJson d ++ ",\"value\":" ++ jsonString value ++ "}")

private def scenarioJson (name : String) (entries : List (DocRef × DesiredFields)) : String :=
  let prior := manifestOf [row .skill "retained-before-publication"]
  let candidate := manifestOf entries
  let keys : List DocRef := ([(⟨.skill, "retained-before-publication", "did:owner"⟩ : DocRef), ⟨.agentContext, "absent", "did:owner"⟩]
    ++ entries.map Prod.fst ++ entries.flatMap (fun entry => entry.2.refs)).eraseDups
  let old : LiveState := { desired := prior.docs, live := fun _ => some "runtime-observation" }
  let after := publish old candidate
  let retry := publish after candidate
  "{\"name\":" ++ jsonString name
    ++ ",\"accepted\":" ++ toString candidate.referencesClosed
    ++ ",\"candidate\":" ++ desiredJson keys candidate.docs
    ++ ",\"pre_desired\":" ++ desiredJson keys old.desired
    ++ ",\"pre_live\":" ++ liveJson keys old.live
    ++ ",\"expected_after_live\":" ++ liveJson keys after.live
    ++ ",\"expected_after_desired\":" ++ desiredJson keys after.desired
    ++ ",\"expected_retry_desired\":" ++ desiredJson keys retry.desired
    ++ ",\"observations_preserved\":"
      ++ toString (keys.all fun d => after.live d == old.live d) ++ "}"

def applyReconcileCasesJson : String :=
  jsonArray (scenarios.map fun (name, entries) => scenarioJson name entries)

end ApplyReconcile.ContractCases
