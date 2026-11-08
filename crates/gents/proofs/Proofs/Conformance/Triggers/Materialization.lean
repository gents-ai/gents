import Proofs.Triggers
import Proofs.Request

def materializedTriggerRequest
    (state : SystemState)
    (intent : FireIntent)
    (seed : RequestSeed) : AgentRequest :=
  { id := s!"dispatched-{state.requests.length}"
  , causedBy :=
      match seed.causedByTriggerId with
      | none => none
      | some tid => some tid
  , concurrency := intent.concurrency
  , isTerminal := false
  , executionOrigin :=
      match seed.causedByTriggerKind with
      | .manual => .interactive
      | .schedule | .event => .scheduled }

theorem materializedTriggerRequest_nonterminal
    (state : SystemState) (intent : FireIntent) (seed : RequestSeed) :
    (materializedTriggerRequest state intent seed).isTerminal = false := by
  rfl

theorem materializedTriggerRequest_origin
    (state : SystemState) (intent : FireIntent) (seed : RequestSeed) :
    (materializedTriggerRequest state intent seed).executionOrigin =
      match seed.causedByTriggerKind with
      | .manual => .interactive
      | .schedule | .event => .scheduled := by
  rfl

theorem dispatch_materializedTriggerRequest_consistentLineage
    (state : SystemState)
    (snap : TriggerSnapshot)
    (intent : FireIntent)
    (seed : RequestSeed)
    (h_dispatch : dispatch snap intent = some seed) :
    consistentLineage seed (materializedTriggerRequest state intent seed).executionOrigin := by
  match h_kind : seed.causedByTriggerKind with
  | .manual =>
      have h_none : seed.causedByTriggerId = none :=
        dispatch_manual_lineage_id_is_none snap intent seed h_dispatch h_kind
      simp [consistentLineage, materializedTriggerRequest, h_kind, h_none]
  | .schedule =>
      simp [consistentLineage, materializedTriggerRequest, h_kind]
  | .event =>
      simp [consistentLineage, materializedTriggerRequest, h_kind]
