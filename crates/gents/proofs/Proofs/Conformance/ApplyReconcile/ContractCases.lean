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

deriving instance ToJson for ConfigDocuments.Collection, DocRef, DesiredFields

private def owner : String := "did:owner"
private def other : String := "did:other"

private def ref (c : Collection) (id : String) (node : String := owner) : DocRef := ⟨c, id, node⟩

structure Row where
  doc : DocRef
  refs : List DocRef
  deriving ToJson

private def manifestOfRefs (rows : List Row) : Manifest :=
  manifestOf (rows.map fun r => (r.doc, { content := r.doc.id, refs := r.refs }))

private def finsetOf (docs : List DocRef) : Finset DocRef :=
  docs.foldr (fun d acc => insert d acc) ∅

private def ctx : DocRef := ref .agentContext "context"
private def tools : DocRef := ref .tools "tools"
private def retained : DocRef := ref .inferenceBackend "retained"

private def cycle : List Row :=
  [⟨ref .inferenceBackend "backend", []⟩,
   ⟨ref .inferenceProfile "profile", [ref .inferenceBackend "backend"]⟩,
   ⟨ref .agent "agent", [ctx, ref .inferenceProfile "profile"]⟩,
   ⟨ctx, [tools]⟩, ⟨tools, [ref .agentTarget "target"]⟩,
   ⟨ref .agentTarget "target", [ref .agent "agent"]⟩]

/-- Each scenario names its stored documents, the plan's writes and the plan's
removals. A reference names a document of its own node, as production's
references do; a reference naming another node's document is refused by
`written_foreign_reference_rejects`. -/
private def applyScenarios : List (String × List Row × List Row × List DocRef) :=
  [("closed_chain", [⟨retained, []⟩], [⟨ctx, [tools]⟩, ⟨tools, []⟩], []),
   ("same_owner_cycle", [], cycle, []),
   ("missing_reference_rejected", [⟨retained, []⟩], [⟨ctx, [ref .tools "absent"]⟩], []),
   ("reference_resolves_against_stored", [⟨tools, []⟩], [⟨ctx, [tools]⟩], []),
   ("removal_of_referenced_rejected", [⟨ctx, [tools]⟩, ⟨tools, []⟩], [], [tools]),
   ("removal_with_its_referrer", [⟨ctx, [tools]⟩, ⟨tools, []⟩], [], [ctx, tools]),
   ("rewrite_releases_removal", [⟨ctx, [tools]⟩, ⟨tools, []⟩], [⟨ctx, []⟩], [tools]),
   ("independent_owners", [], [⟨tools, []⟩, ⟨ref .tools "tools" other, []⟩], []),
   ("same_labels_distinct_owners", [],
     [⟨ctx, [tools]⟩, ⟨tools, []⟩,
      ⟨ref .agentContext "context" other, [ref .tools "tools" other]⟩,
      ⟨ref .tools "tools" other, []⟩], []),
   ("foreign_same_label_cannot_satisfy_reference", [],
     [⟨ctx, [tools]⟩, ⟨ref .tools "tools" other, []⟩], []),
   ("empty_plan", [⟨retained, []⟩], [], [])]

structure Presence where
  doc : DocRef
  present : Bool
  deriving ToJson

structure ApplyInput where
  name : String
  stored : List Row
  writes : List Row
  removals : List DocRef
  observe : List DocRef
  deriving ToJson

structure ApplyOutput where
  accepted : Bool
  after : List Presence
  retryAccepted : Bool
  afterRetry : List Presence
  deriving ToJson

private def presence (keys : List DocRef) (m : Manifest) : List Presence :=
  keys.map fun d => { doc := d, present := m.contains d }

private def planOf (i : ApplyInput) : Plan :=
  { writes := manifestOfRefs i.writes, removals := finsetOf i.removals }

@[conformance_family]
def applyCases : Family ApplyInput ApplyOutput where
  inputs := applyScenarios.map fun (name, stored, writes, removals) =>
    { name, stored, writes, removals
      observe := ((stored ++ writes).flatMap (fun r => r.doc :: r.refs) ++ removals).eraseDups }
  model := fun i =>
    let p := planOf i
    let old : LiveState := { desired := manifestOfRefs i.stored, live := fun _ => none }
    let after := applyPlan old p
    { accepted := admits (p.over old.desired) p.changed
      after := presence i.observe after.desired
      retryAccepted := admits (p.over after.desired) p.changed
      afterRetry := presence i.observe (applyPlan after p).desired }

private def pruneScenarios : List (String × List Row × List DocRef) :=
  [("unreferenced_left_out", [⟨ctx, []⟩, ⟨tools, []⟩], [ctx]),
   ("referenced_kept", [⟨ctx, [tools]⟩, ⟨tools, []⟩], [ctx]),
   ("referrer_and_target_left_out", [⟨ctx, [tools]⟩, ⟨tools, []⟩], []),
   ("default_selection_kept",
     [⟨ref .defaultAgentSelection owner, []⟩, ⟨tools, []⟩], []),
   ("all_named", [⟨ctx, [tools]⟩, ⟨tools, []⟩], [ctx, tools]),
   ("dangling_stored_refused", [⟨ctx, [ref .tools "absent"]⟩], [])]

structure PruneInput where
  name : String
  live : List Row
  authored : List DocRef
  deriving ToJson

/-- The stored documents prune removes, in stored order; `none` when the stored
state is refused. -/
@[conformance_family]
def pruneCases : Family PruneInput (Option (List DocRef)) where
  inputs := pruneScenarios.map fun (name, live, authored) => { name, live, authored }
  model := fun i =>
    let live := manifestOfRefs i.live
    let authored := manifestOfRefs (i.authored.map fun d => ⟨d, []⟩)
    (prune live authored).map fun removed =>
      (i.live.map (·.doc)).filter (· ∈ removed)

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

private def desiredRows (rows : List (DocRef × DesiredFields)) : List Desired :=
  rows.map fun (d, f) => { doc := d, content := f.content, refs := f.refs }

private def manifestOfRows (rows : List Desired) : Manifest :=
  manifestOf (rows.map fun r => (r.doc, { content := r.content, refs := r.refs }))

private def contents (keys : List DocRef) (desired : DocRef → Option DesiredFields) :
    List Content :=
  keys.map fun d => { doc := d, content := (desired d).map (·.content) }

private def expectedFn (rows : List Expected) : DocRef → Option DesiredFields :=
  fun d => (rows.find? fun r => r.doc = d).bind (·.fields)

private def absent : DocRef := ⟨.agentContext, "absent", "did:owner"⟩

private def priorRows : List (DocRef × DesiredFields) :=
  [(ctx, { content := "prompt-v1", refs := [tools] }), (tools, { content := "tools-v1", refs := [] })]

private def candidateRows : List (DocRef × DesiredFields) :=
  [(ctx, { content := "prompt-v2", refs := [tools] }), (tools, { content := "tools-v1", refs := [] })]

private def applyIfScenarios : List (String × List (DocRef × Option DesiredFields)) :=
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
   ("empty_scope_is_apply", [])]

structure ApplyIfInput where
  name : String
  expected : List Expected
  preDesired : List Desired
  candidate : List Desired
  observe : List DocRef
  deriving ToJson

structure ApplyIfOutput where
  applied : Bool
  after : List Content
  replayApplied : Bool
  afterReplay : List Content
  deriving ToJson

@[conformance_family]
def applyIfCases : Family ApplyIfInput ApplyIfOutput where
  inputs := applyIfScenarios.map fun (name, rows) =>
    { name := name
      expected := rows.map fun (d, f) => { doc := d, fields := f }
      preDesired := desiredRows priorRows
      candidate := desiredRows candidateRows
      observe := [ctx, tools, absent] }
  model := fun i =>
    let old : LiveState :=
      { desired := manifestOfRows i.preDesired, live := fun _ => none }
    let p : Plan := { writes := manifestOfRows i.candidate, removals := ∅ }
    let scope := i.expected.map (·.doc)
    let after := applyPlanIf old scope (expectedFn i.expected) p
    let replay := applyPlanIf after scope (expectedFn i.expected) p
    { applied := expectationsHold old scope (expectedFn i.expected)
      after := contents i.observe after.desired.docs
      replayApplied := expectationsHold after scope (expectedFn i.expected)
      afterReplay := contents i.observe replay.desired.docs }

structure AgentContextInput where
  agentValid : Bool
  contextValid : Bool
  deriving ToJson

@[conformance_family]
def atomicAgentContext : Family AgentContextInput Bool where
  inputs := [false, true].flatMap fun agentValid =>
    [false, true].map fun contextValid => { agentValid, contextValid }
  model := fun i =>
    let candidate := manifestOf (
      [row .agent "agent" [⟨.agentContext, "context", "did:owner"⟩,
        ⟨.inferenceProfile, "profile", "did:owner"⟩],
       row .agentContext "context" [⟨.tools, "tools", "did:owner"⟩]] ++
      (if i.agentValid then [row .inferenceProfile "profile"] else []) ++
      (if i.contextValid then [row .tools "tools"] else []))
    candidate.referencesClosed

end ApplyReconcile.ContractCases
