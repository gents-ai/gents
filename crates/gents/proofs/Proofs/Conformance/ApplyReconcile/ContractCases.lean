import Proofs.Documents.ApplyReconcile.Publication
import Proofs.Conformance.Family
import Mathlib.Data.Finset.Card
namespace ApplyReconcile.ContractCases

open Conformance Lean

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
  [row .agent "agent" [⟨.agentContext, "context", "did:owner"⟩],
   row .agentContext "context" [⟨.tools, "tools", "did:owner"⟩], row .tools "tools"]

private def cycle : List (DocRef × DesiredFields) :=
  [row .agent "agent" [⟨.agentContext, "context", "did:owner"⟩],
   row .agentContext "context" [⟨.tools, "tools", "did:owner"⟩],
   row .tools "tools" [⟨.agentTarget, "target", "did:owner"⟩],
   row .agentTarget "target" [⟨.agent, "agent", "did:owner"⟩]]

private def missing : List (DocRef × DesiredFields) :=
  [row .agent "agent" [⟨.agentContext, "absent", "did:owner"⟩]]

private def foreign : List (DocRef × DesiredFields) :=
  [row .agent "agent" [⟨.agentContext, "foreign", "did:other"⟩],
   row .agentContext "foreign" [] "did:other"]

private def independentOwners : List (DocRef × DesiredFields) :=
  [row .tools "mine", row .tools "theirs" [] "did:other"]

private def sameLabels : List (DocRef × DesiredFields) :=
  [row .agent "coding" [⟨.agentContext, "context", "did:owner"⟩],
   row .agentContext "context", row .agent "coding"
     [⟨.agentContext, "context", "did:other"⟩] "did:other",
   row .agentContext "context" [] "did:other"]

private def foreignShadow : List (DocRef × DesiredFields) :=
  [row .agent "coding" [⟨.agentContext, "context", "did:owner"⟩],
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

deriving instance ToJson for ConfigDocuments.Collection, DocRef, DesiredFields

structure Desired where
  doc : DocRef
  content : String
  refs : List DocRef
  deriving ToJson

structure Content where
  doc : DocRef
  content : Option String
  deriving ToJson

structure Expected where
  doc : DocRef
  fields : Option DesiredFields
  deriving ToJson

structure Observation where
  doc : DocRef
  value : Option String
  deriving ToJson

private def desiredRows (rows : List (DocRef × DesiredFields)) : List Desired :=
  rows.map fun (d, f) => { doc := d, content := f.content, refs := f.refs }

private def manifestOfRows (rows : List Desired) : Manifest :=
  manifestOf (rows.map fun r => (r.doc, { content := r.content, refs := r.refs }))

private def contents (keys : List DocRef) (desired : DocRef → Option DesiredFields) :
    List Content :=
  keys.map fun d => { doc := d, content := (desired d).map (·.content) }

structure PublishInput where
  name : String
  preDesired : List Desired
  preLive : List Observation
  candidate : List Desired
  observe : List DocRef
  deriving ToJson

structure PublishOutput where
  accepted : Bool
  afterDesired : List Content
  afterLive : List Observation
  retryDesired : List Content
  deriving ToJson

private def publishInput (name : String) (entries : List (DocRef × DesiredFields)) :
    PublishInput :=
  let retained : DocRef := ⟨.skill, "retained-before-publication", "did:owner"⟩
  let keys : List DocRef := ([retained, ⟨.agentContext, "absent", "did:owner"⟩]
    ++ entries.map Prod.fst ++ entries.flatMap (fun entry => entry.2.refs)).eraseDups
  { name := name
    preDesired := desiredRows [row .skill "retained-before-publication"]
    preLive := keys.map fun d => { doc := d, value := some "runtime-observation" }
    candidate := desiredRows entries
    observe := keys }

@[conformance_family]
def publishCases : Family PublishInput PublishOutput where
  inputs := scenarios.map fun (name, entries) => publishInput name entries
  model := fun i =>
    let candidate := manifestOfRows i.candidate
    let old : LiveState :=
      { desired := (manifestOfRows i.preDesired).docs
        live := fun d => (i.preLive.find? fun o => o.doc = d).bind (·.value) }
    let after := publish old candidate
    let retry := publish after candidate
    { accepted := candidate.referencesClosed
      afterDesired := contents i.observe after.desired
      afterLive := i.observe.map fun d => { doc := d, value := after.live d }
      retryDesired := contents i.observe retry.desired }

private def expectedFn (rows : List Expected) : DocRef → Option DesiredFields :=
  fun d => (rows.find? fun r => r.doc = d).bind (·.fields)

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

structure PublishIfInput where
  name : String
  expected : List Expected
  preDesired : List Desired
  candidate : List Desired
  observe : List DocRef
  deriving ToJson

structure PublishIfOutput where
  applied : Bool
  after : List Content
  replayApplied : Bool
  afterReplay : List Content
  deriving ToJson

@[conformance_family]
def publishIfCases : Family PublishIfInput PublishIfOutput where
  inputs := publishIfScenarios.map fun (name, rows) =>
    { name := name
      expected := rows.map fun (d, f) => { doc := d, fields := f }
      preDesired := desiredRows priorRows
      candidate := desiredRows candidateRows
      observe := [ctx, tools, absent] }
  model := fun i =>
    let old : LiveState :=
      { desired := (manifestOfRows i.preDesired).docs, live := fun _ => none }
    let candidate := manifestOfRows i.candidate
    let scope := i.expected.map (·.doc)
    let after := publishIf old scope (expectedFn i.expected) candidate
    let replay := publishIf after scope (expectedFn i.expected) candidate
    { applied := expectationsHold old scope (expectedFn i.expected)
      after := contents i.observe after.desired
      replayApplied := expectationsHold after scope (expectedFn i.expected)
      afterReplay := contents i.observe replay.desired }

end ApplyReconcile.ContractCases
