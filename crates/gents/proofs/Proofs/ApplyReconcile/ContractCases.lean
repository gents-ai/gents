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

private def boolJson (b : Bool) : String := if b then "true" else "false"

/-- `expected` rows: `content` is `null` when the document must be absent. -/
private def expectedJson (rows : List (DocRef × Option DesiredFields)) : String :=
  jsonArray (rows.map fun (d, f) =>
    "{\"ref\":" ++ docRefJson d ++ ",\"content\":"
      ++ (match f with | some x => jsonString x.content | none => "null") ++ "}")

private def expectedFn (rows : List (DocRef × Option DesiredFields)) :
    DocRef → Option DesiredFields :=
  fun d => (rows.find? fun r => r.1 = d).bind Prod.snd

private def ctx : DocRef := ⟨.agentContext, "context", "did:owner"⟩
private def tools : DocRef := ⟨.tools, "tools", "did:owner"⟩
private def absent : DocRef := ⟨.agentContext, "absent", "did:owner"⟩

private def priorRows : List (DocRef × DesiredFields) :=
  [(ctx, { content := "prompt-v1", refs := [tools] }), (tools, { content := "tools-v1", refs := [] })]

private def candidateRows : List (DocRef × DesiredFields) :=
  [(ctx, { content := "prompt-v2", refs := [tools] }), (tools, { content := "tools-v1", refs := [] })]

private def publishIfScenarios : List (String × List (DocRef × Option DesiredFields)) :=
  [("all_expectations_match",
      [(ctx, some { content := "prompt-v1", refs := [tools] }),
       (tools, some { content := "tools-v1", refs := [] })]),
   ("target_drifted", [(ctx, some { content := "prompt-v0", refs := [tools] })]),
   ("closure_document_drifted",
      [(ctx, some { content := "prompt-v1", refs := [tools] }),
       (tools, some { content := "tools-v0", refs := [] })]),
   ("expected_absent_but_present", [(ctx, none)]),
   ("expected_absent_and_absent", [(absent, none)]),
   ("expected_present_but_absent", [(absent, some { content := "x", refs := [] })]),
   ("empty_scope_is_publish", [])]

/-- A document-pack install guards every document it writes, so its expectation
scope is exactly the candidate's support and no smaller. These scenarios are
emitted at that shape, over a candidate that also creates a member absent when
the install reads it, so a native pack consumer can execute the modeled
candidate itself instead of projecting members away. -/
private def packCandidateRows : List (DocRef × DesiredFields) :=
  (absent, { content := "created-v1", refs := [] }) :: candidateRows

private def packScopedScenarios : List (String × List (DocRef × Option DesiredFields)) :=
  [("pack_scope_all_match",
      [(ctx, some { content := "prompt-v1", refs := [tools] }),
       (tools, some { content := "tools-v1", refs := [] }), (absent, none)]),
   ("pack_scope_absent_member_present",
      [(ctx, none), (tools, some { content := "tools-v1", refs := [] }), (absent, none)]),
   ("pack_scope_member_drifted",
      [(ctx, some { content := "prompt-v0", refs := [tools] }),
       (tools, some { content := "tools-v1", refs := [] }), (absent, none)])]

private def publishIfScenarioJson (name : String)
    (rows : List (DocRef × Option DesiredFields))
    (candidateRows : List (DocRef × DesiredFields) := candidateRows) : String :=
  let old : LiveState := { desired := (manifestOf priorRows).docs, live := fun _ => none }
  let candidate := manifestOf candidateRows
  let scope := rows.map Prod.fst
  let after := publishIf old scope (expectedFn rows) candidate
  let keys : List DocRef := [ctx, tools, absent]
  "{\"name\":" ++ jsonString name
    ++ ",\"expected\":" ++ expectedJson rows
    ++ ",\"pre_desired\":" ++ desiredJson keys old.desired
    ++ ",\"candidate\":" ++ desiredJson keys candidate.docs
    ++ ",\"applied\":" ++ boolJson (expectationsHold old scope (expectedFn rows))
    ++ ",\"expected_after_desired\":" ++ desiredJson keys after.desired ++ "}"

def publishIfCasesJson : String :=
  jsonArray ((publishIfScenarios.map fun (name, rows) => publishIfScenarioJson name rows)
    ++ (packScopedScenarios.map fun (name, rows) =>
          publishIfScenarioJson name rows packCandidateRows))

end ApplyReconcile.ContractCases
