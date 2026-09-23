import Proofs.CanonicalOutput.Execution.AccountingMetadata
import Proofs.CanonicalOutput.Execution.CoherenceFrame
import Proofs.CanonicalOutput.Execution.ToolDelivery

namespace CanonicalOutput.Execution.ToolDelivery

def clockEdit (document : DocId) (now : Time) (tool : OwnedTool) : OwnedTool :=
  if tool.document == document then
    { tool with context := { tool.context with currentTime := now } }
  else tool

theorem clockEdit_document (document : DocId) (now : Time) (tool : OwnedTool) :
    (clockEdit document now tool).document = tool.document := by
  unfold clockEdit; split <;> rfl

def clockWorld (world : World) (document : DocId) (now : Time) : World :=
  { world with toolContexts := world.toolContexts.map (clockEdit document now) }

theorem clockWorld_owned_lookup (world : World) (document : DocId) (now : Time)
    (key : DocId) :
    ownedToolByDocument? (clockWorld world document now) key =
      (ownedToolByDocument? world key).map (clockEdit document now) := by
  exact ownedToolByDocument_map world (clockEdit document now) key
    (clockEdit_document document now)

theorem clockWorld_binding (world : World) (document : DocId) (now : Time)
    (tool : OwnedTool) :
    acceptedHeaderBindsTool (clockWorld world document now) (clockEdit document now tool) =
      acceptedHeaderBindsTool world tool := by
  apply acceptedHeaderBindsTool_map world (clockEdit document now) tool
  all_goals intro value; unfold clockEdit; split <;> rfl

theorem clockWorld_lifecycle (world : World) (document : DocId) (now : Time)
    (coherent : toolLifecycleProjectionCoherent world = true) :
    toolLifecycleProjectionCoherent (clockWorld world document now) = true := by
  simp only [toolLifecycleProjectionCoherent, Bool.and_eq_true] at coherent ⊢
  refine ⟨⟨?_, ?_⟩, ?_⟩
  · simpa only [clockWorld, List.map_map, Function.comp_def,
      clockEdit_document] using coherent.1.1
  · change (world.toolContexts.map (clockEdit document now)).all _ = true
    rw [List.all_map, List.all_eq_true]
    intro tool hm
    have ht := (List.all_eq_true.mp coherent.1.2) tool hm
    simp only [Function.comp_def, clockEdit_document, clockWorld_binding]
    have session : (clockEdit document now tool).session = tool.session := by
      unfold clockEdit; split <;> rfl
    have provenance : (clockEdit document now tool).provenance = tool.provenance := by
      unfold clockEdit; split <;> rfl
    have state : (clockEdit document now tool).context.state = tool.context.state := by
      unfold clockEdit; split <;> rfl
    have handed : toolHandedOff (clockEdit document now tool) = toolHandedOff tool := by
      unfold clockEdit toolHandedOff; split <;> rfl
    have sequence : (clockEdit document now tool).acceptedSequence = tool.acceptedSequence := by
      unfold clockEdit; split <;> rfl
    simpa only [clockWorld, session, provenance, state, handed, sequence] using ht
  · rw [List.all_eq_true]
    intro row hm
    have hr := (List.all_eq_true.mp coherent.2) row hm
    rw [clockWorld_owned_lookup]
    cases hl : ownedToolByDocument? world row.callId with
    | none => simp [hl] at hr ⊢
    | some tool =>
      simp only [hl, Option.map]
      simp only [hl] at hr
      have provenance : (clockEdit document now tool).provenance = tool.provenance := by
        unfold clockEdit; split <;> rfl
      have session : (clockEdit document now tool).session = tool.session := by
        unfold clockEdit; split <;> rfl
      have sequence : (clockEdit document now tool).acceptedSequence = tool.acceptedSequence := by
        unfold clockEdit; split <;> rfl
      simpa only [provenance, session, sequence] using hr

theorem clockWorld_result (world : World) (document : DocId) (now : Time)
    (tool : OwnedTool) (key : Transcript.ToolResultKey) :
    canonicalToolResultBound (clockWorld world document now) (clockEdit document now tool) key =
      canonicalToolResultBound world tool key := by
  unfold clockEdit clockWorld
  split <;> rfl

theorem clockWorld_receipt (world : World) (document : DocId) (now : Time)
    (tool : OwnedTool) :
    runningReceiptSourceBound (clockWorld world document now) (clockEdit document now tool) =
      runningReceiptSourceBound world tool := by
  unfold clockEdit clockWorld
  split <;> rfl

theorem clockWorld_preserves_toolProjectionCoherent
    (world : World) (document : DocId) (now : Time)
    (coherent : toolProjectionCoherent world = true) :
    toolProjectionCoherent (clockWorld world document now) = true := by
  simp only [toolProjectionCoherent, Bool.and_eq_true] at coherent ⊢
  refine ⟨clockWorld_lifecycle world document now coherent.1, ?_⟩
  change (world.toolContexts.map (clockEdit document now)).all _ = true
  rw [List.all_map, List.all_eq_true]
  intro tool hm
  have ht := (List.all_eq_true.mp coherent.2) tool hm
  have provenance : (clockEdit document now tool).provenance = tool.provenance := by
    unfold clockEdit; split <;> rfl
  simp only [Function.comp_def, provenance, clockEdit_document]
  change (match tool.provenance, transcriptToolByDocument? world tool.document with
    | .acceptedIntent, some row =>
        match row.resultKey with
        | none => true
        | some key => canonicalToolResultBound (clockWorld world document now)
            (clockEdit document now tool) key &&
          (isTerminal row.state ||
            (row.state == .running && runningReceiptSourceBound
              (clockWorld world document now) (clockEdit document now tool)))
    | .spawnedBackground _, none => true
    | _, _ => false) = true
  cases hp : tool.provenance <;> cases hr : transcriptToolByDocument? world tool.document <;>
    simp only [hp, hr] at ht ⊢
  all_goals try exact ht
  rename_i row
  cases hk : row.resultKey with
  | none => rfl
  | some key =>
    simp only [hk, Bool.and_eq_true] at ht ⊢
    exact ⟨by simpa only [clockWorld_result] using ht.1,
      by simpa only [clockWorld_receipt] using ht.2⟩

theorem appendToolOutput_preserves_toolProjectionCoherent
    (before after : World) (document : DocId) (record : Segment)
    (coherent : toolProjectionCoherent before = true)
    (h : appendToolOutput before document record = .ok after) :
    toolProjectionCoherent after = true := by
  rcases append_success_lifecycle_effect before after document record h with rfl | effect
  · exact coherent
  · rcases effect with ⟨tool, observed, segments, found, clocked, appended, rfl⟩
    have member := (ownedTool_lookup_member before document tool found).1
    have toolDocument := (ownedTool_lookup_member before document tool found).2
    subst document
    have observedEq : observed = clockEdit tool.document before.lease.now tool := by
      unfold updateClock at clocked
      split at clocked
      · simp only [Option.some.injEq] at clocked
        rw [← clocked]
        simp [clockEdit]
      · contradiction
    have unique : (before.toolContexts.map (·.document)).Nodup := by
      have lifecycle := toolProjectionCoherent_implies_lifecycle before coherent
      simpa only [toolLifecycleProjectionCoherent, Bool.and_eq_true, decide_eq_true_eq] using
        ((Bool.and_eq_true_iff.mp ((Bool.and_eq_true_iff.mp lifecycle).1)).1)
    have toolsEq : replaceOwnedTool before.toolContexts tool.document observed =
        before.toolContexts.map (clockEdit tool.document before.lease.now) := by
      rw [observedEq]
      calc
        _ = before.toolContexts.map (fun value =>
            if value.document == tool.document then
              clockEdit tool.document before.lease.now value else value) :=
          replaceOwnedTool_eq_map before tool (clockEdit tool.document before.lease.now)
            unique member
        _ = _ := by
          apply List.map_congr_left
          intro value hm
          by_cases selected : value.document = tool.document
          · simp [clockEdit, selected]
          · simp [clockEdit, selected]
    rcases CanonicalOutput.ToolDelivery.appendRecords_success_effect
      before.segments segments tool.requestDoc tool.document before.lease.now record appended with
      replay | fresh
    · subst segments
      rw [toolsEq]
      exact clockWorld_preserves_toolProjectionCoherent before tool.document before.lease.now coherent
    · rcases fresh with ⟨rfl, _, openSource, available⟩
      have freshId : ∀ existing ∈ before.segments, existing.id ≠ record.id := by
        intro existing hm equal
        simp [CanonicalOutput.ToolDelivery.identityAvailable, List.any_eq_false] at available
        exact (available existing hm) equal
      rw [toolsEq]
      apply toolProjectionCoherent_append_open
        (clockWorld before tool.document before.lease.now) record
      · simpa [clockWorld] using freshId
      · simpa [clockWorld] using openSource
      · exact clockWorld_preserves_toolProjectionCoherent
          before tool.document before.lease.now coherent

end CanonicalOutput.Execution.ToolDelivery
