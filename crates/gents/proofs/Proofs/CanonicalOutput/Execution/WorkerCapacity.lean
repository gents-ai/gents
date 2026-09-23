import Proofs.CanonicalOutput.Execution.Projection
import Proofs.CanonicalOutput.Execution.SessionComposition
import Proofs.CanonicalOutput.Execution.Examples
import Proofs.CanonicalOutput.Execution.ToolDeliveryCases
import Proofs.DescendantGraph
import Mathlib.Data.Finset.Card

/-!
Local request-worker capacity, distinct from backend inference admission.

This is the local resource and application-trace composition for the
waiting-parent scheduler repair. The request/lease/tool owners remain
authoritative; capacity operations cannot mutate their worlds. Native
continuation retention, atomic admission and cancellation observation remain
implementation obligations; the finite witnesses do not assert fairness.
-/
namespace CanonicalOutput.Execution.WorkerCapacity

abbrev Ticket := DocId × Generation

structure State where
  activeLimit : Nat
  parkedLimit : Nat
  active : Finset Ticket := ∅
  parked : Finset Ticket := ∅
  /-- Local continuation binding, never a second durable tool lifecycle. -/
  dependencies : List (Ticket × DocId) := []
  deriving DecidableEq

def Safe (s : State) : Prop :=
  s.active.card ≤ s.activeLimit ∧ s.parked.card ≤ s.parkedLimit ∧
    Disjoint s.active s.parked

def initial (activeLimit parkedLimit : Nat) : State :=
  { activeLimit, parkedLimit }

theorem initial_safe (a p : Nat) : Safe (initial a p) := by
  simp [Safe, initial]

/-- Waiting preserves the exact generation ticket and frees active capacity.
Overflow rejects admission; it must never fall back to holding a worker while
waiting for another request. Callers must arrange this before starting a wait. -/
def park (s : State) (ticket : Ticket) : Option State :=
  if ticket ∈ s.active ∧ ticket ∉ s.parked ∧ s.parked.card < s.parkedLimit then
    some { s with active := s.active.erase ticket, parked := insert ticket s.parked }
  else none

def acquire (s : State) (ticket : Ticket) : Option State :=
  if ticket ∉ s.active ∧ s.active.card < s.activeLimit then
    some { s with active := insert ticket s.active, parked := s.parked.erase ticket }
  else none

/-- New work cannot consume a parked continuation or its retained dependency.
Only `resumeAfterChild` may move such a ticket back to active capacity. -/
def admitFresh (s : State) (ticket : Ticket) : Option State :=
  if ticket ∈ s.parked || (s.dependencies.lookup ticket).isSome then none
  else acquire s ticket

theorem admitFresh_requires_unparked {s t : State} {ticket : Ticket}
    (h : admitFresh s ticket = some t) :
    ticket ∉ s.parked ∧ s.dependencies.lookup ticket = none := by
  unfold admitFresh at h
  split at h
  · contradiction
  · rename_i guard
    simp only [Bool.or_eq_false_iff, decide_eq_false_iff_not, Bool.not_eq_true] at guard
    refine ⟨guard.1, ?_⟩
    cases hlookup : s.dependencies.lookup ticket with
    | none => rfl
    | some document => simp [hlookup] at guard

theorem parked_ticket_cannot_admitFresh (s : State) (ticket : Ticket)
    (h : ticket ∈ s.parked) : admitFresh s ticket = none := by
  simp [admitFresh, h]

/-- Cleanup does not need a live lease: an expired continuation must still be
able to release its local resources, without authorizing a durable write. -/
def release (s : State) (ticket : Ticket) : State :=
  { s with
    active := s.active.erase ticket
    parked := s.parked.erase ticket
    dependencies := s.dependencies.filter (fun entry => entry.1 != ticket) }

theorem park_safe {s t : State} {ticket : Ticket}
    (hs : Safe s) (h : park s ticket = some t) : Safe t := by
  unfold park at h
  split at h
  next guards =>
    cases Option.some.inj h
    rcases hs with ⟨ha, hp, hd⟩
    rcases guards with ⟨hin, hout, room⟩
    refine ⟨le_trans Finset.card_erase_le ha, ?_, ?_⟩
    · simp only [Finset.card_insert_of_not_mem hout]
      omega
    · rw [Finset.disjoint_left] at hd ⊢
      intro x hx hx'
      simp only [Finset.mem_erase, Finset.mem_insert] at hx hx'
      rcases hx' with heq | hm
      · exact hx.1 heq
      · exact hd hx.2 hm
  next => contradiction

theorem park_success_parked {s t : State} {ticket : Ticket}
    (h : park s ticket = some t) : ticket ∈ t.parked ∧ ticket ∉ t.active := by
  unfold park at h
  split at h
  · cases Option.some.inj h
    simp
  · contradiction

theorem acquire_safe {s t : State} {ticket : Ticket}
    (hs : Safe s) (h : acquire s ticket = some t) : Safe t := by
  unfold acquire at h
  split at h
  next guards =>
    cases Option.some.inj h
    rcases hs with ⟨ha, hp, hd⟩
    rcases guards with ⟨hout, room⟩
    refine ⟨?_, le_trans Finset.card_erase_le hp, ?_⟩
    · simp only [Finset.card_insert_of_not_mem hout]
      omega
    · rw [Finset.disjoint_left] at hd ⊢
      intro x hx hx'
      simp only [Finset.mem_insert, Finset.mem_erase] at hx hx'
      rcases hx with heq | hm
      · exact hx'.1 heq
      · exact hd hm hx'.2
  next => contradiction

theorem release_safe {s : State} (hs : Safe s) (ticket : Ticket) :
    Safe (release s ticket) := by
  rcases hs with ⟨ha, hp, hd⟩
  refine ⟨le_trans Finset.card_erase_le ha,
    le_trans Finset.card_erase_le hp, ?_⟩
  exact hd.mono (Finset.erase_subset _ _) (Finset.erase_subset _ _)

/-- Capacity alone is never permission to resume an expired/replaced request.
Tool readiness and cancellation remain additional composition obligations. -/
def resumeOwned (s : State) (world : World) (generation : Generation) : Option State :=
  if (world.requestId, generation) ∈ s.parked ∧
      RequestExecutionLease.admitted world.lease .mutationWriteGate generation ∧
      world.lease.request = .processing then
    acquire s (world.requestId, generation)
  else none

theorem stale_owner_cannot_resume (s : State) (world : World) (generation : Generation)
    (h : ¬ RequestExecutionLease.admitted world.lease .mutationWriteGate generation) :
    resumeOwned s world generation = none := by
  simp [resumeOwned, h]

theorem resumeOwned_safe {s t : State} (world : World) (generation : Generation)
    (hs : Safe s) (h : resumeOwned s world generation = some t) : Safe t := by
  unfold resumeOwned at h
  split at h
  · exact acquire_safe hs h
  · contradiction

theorem resumeOwned_requires_current_ticket {s t : State} (world : World)
    (generation : Generation) (h : resumeOwned s world generation = some t) :
    (world.requestId, generation) ∈ s.parked ∧
      RequestExecutionLease.admitted world.lease .mutationWriteGate generation ∧
      world.lease.request = .processing := by
  unfold resumeOwned at h
  split at h
  · rename_i guard
    exact guard
  · contradiction

/-- The descendant resolver's authorized projection. `bridgeDocument` and
`controlDocument` are physical tool documents supplied by the owner adapter,
not child-request arguments. The edge remains the graph owner's decision. -/
structure ExistingChildSelection where
  viewer : DescendantGraph.Viewer
  callerOwner : DescendantGraph.SessionOwner
  bridgeOwner : DescendantGraph.SessionOwner
  edge : DescendantGraph.Edge
  bridgeDocument : DocId
  controlDocument : DocId
  deriving DecidableEq

def currentWaitControl (world : World) (generation : Generation)
    (control : OwnedTool) : Bool :=
  acceptedHeaderBindsToolGeneration world control generation &&
    control.context.state == .running && control.context.childRequestId.isNone &&
    world.messages.any (fun message =>
      message.header.request == some world.requestId &&
        message.header.publication == .requestExecution generation &&
        message.header.session == world.sessionId &&
        (targetIntent message control.document).any (fun intent => intent.name == "wait_subagent"))

def existingChildBound (world : World) (selection : ExistingChildSelection)
    (bridge : OwnedTool) : Bool :=
  DescendantGraph.sessionControllable selection.callerOwner selection.bridgeOwner
      selection.viewer selection.edge &&
    selection.edge.rootSessionId == world.sessionId &&
    selection.edge.parentToolCallId == bridge.context.callId &&
    selection.edge.childRequestId == bridge.context.childRequestId.getD 0 &&
    bridge.context.childRequestId.isSome &&
    bridge.document == selection.bridgeDocument &&
    bridge.session == world.sessionId &&
    acceptedHeaderBindsTool world bridge

/-- Explicit background handoff is a durable native invocation reply for the
same physical accepted bridge. A mode flip alone cannot resume a parked
continuation while its child remains running. -/
def durableBackgroundHandoff (world : World) (bridge : OwnedTool) : Bool :=
  bridge.context.state == .running && bridge.context.awaitMode == .background &&
    canonicalToolDelivered world bridge

/-- A later request may wait on an earlier request's authorized bridge. The
current accepted wait control and current lease fence are separate from the
old bridge's accepted header and exact physical descendant binding. -/
def waitForExistingChild (s : State) (world : World) (generation : Generation)
    (selection : ExistingChildSelection) (document : DocId) : Option State :=
  if !SessionComposition.currentClaim world ||
      !(decide (RequestExecutionLease.admitted world.lease .mutationWriteGate generation)) ||
      world.lease.request != .processing || document != selection.bridgeDocument then none
  else match ownedToolByDocument? world selection.controlDocument,
      ownedToolByDocument? world document with
  | some control, some bridge =>
      if !currentWaitControl world generation control ||
          !existingChildBound world selection bridge ||
          bridge.context.state != .running || selection.edge.lifecycle != .running then none
      else (park s (world.requestId, generation)).map fun next =>
        { next with dependencies :=
          ((world.requestId, generation), document) ::
            s.dependencies.filter (fun entry => entry.1 != (world.requestId, generation)) }
  | _, _ => none

def resumeAfterExistingChild (s : State) (world : World) (generation : Generation)
    (selection : ExistingChildSelection) (cancellationAllows : Bool) : Option State :=
  if !cancellationAllows || !SessionComposition.currentClaim world ||
      s.dependencies.lookup (world.requestId, generation) != some selection.bridgeDocument then none
  else match ownedToolByDocument? world selection.controlDocument,
      ownedToolByDocument? world selection.bridgeDocument with
  | some control, some bridge =>
      if !currentWaitControl world generation control ||
          !existingChildBound world selection bridge ||
          !(decide (isTerminal bridge.context.state) &&
              DescendantGraph.terminal selection.edge.lifecycle ||
            durableBackgroundHandoff world bridge &&
              selection.edge.lifecycle == .running &&
              selection.edge.awaitMode == .background) then none
      else (resumeOwned s world generation).map fun next =>
        { next with dependencies :=
          s.dependencies.filter (fun entry => entry.1 != (world.requestId, generation)) }
  | _, _ => none

/-- Interruption of the current waiting request vetoes resumption even when
the earlier spawning request's bridge has valid terminal evidence. -/
theorem existing_wait_current_caller_interruption_blocks_resume
    (s : State) (world : World) (generation : Generation)
    (selection : ExistingChildSelection) :
    resumeAfterExistingChild s world generation selection false = none := by
  simp [resumeAfterExistingChild]

theorem existing_wait_substituted_document_refused (s : State) (world : World)
    (generation : Generation) (selection : ExistingChildSelection) (document : DocId)
    (h : document ≠ selection.bridgeDocument) :
    waitForExistingChild s world generation selection document = none := by
  simp [waitForExistingChild, h]

theorem existing_wait_stale_current_owner_refused (s : State) (world : World)
    (generation : Generation) (selection : ExistingChildSelection) (document : DocId)
    (h : ¬ RequestExecutionLease.admitted world.lease .mutationWriteGate generation) :
    waitForExistingChild s world generation selection document = none := by
  simp [waitForExistingChild, h]

/-- Park only behind an exact, accepted, running child bridge. The existing
tool owner supplies its physical identity and generation binding; no caller
boolean substitutes for those facts. This also covers an explicit wait on an
already-backgrounded child. -/
def waitForChild (s : State) (world : World) (generation : Generation)
    (document : DocId) : Option State :=
  if !SessionComposition.currentClaim world ||
      !(decide (RequestExecutionLease.admitted world.lease .mutationWriteGate generation)) ||
      world.lease.request != .processing then none
  else match ownedToolByDocument? world document with
  | none => none
  | some tool =>
      if !(acceptedHeaderBindsToolGeneration world tool generation) ||
          tool.context.childRequestId.isNone || tool.context.state != .running then none
      else (park s (world.requestId, generation)).map fun next =>
        { next with dependencies :=
          ((world.requestId, generation), document) ::
            s.dependencies.filter (fun entry => entry.1 != (world.requestId, generation)) }

/-- A parked continuation cannot swap its dependency for a different finished
tool. Completion is observed through the existing bridge owner. Cancellation
of the parent remains an additional scheduler admission veto at the join. -/
def resumeAfterChild (s : State) (world : World) (generation : Generation)
    (cancellationAllows : Bool) : Option State :=
  if !cancellationAllows || !SessionComposition.currentClaim world then none
  else match s.dependencies.lookup (world.requestId, generation) with
  | none => none
  | some document => match ownedToolByDocument? world document with
    | none => none
    | some tool =>
        if !(acceptedHeaderBindsToolGeneration world tool generation) ||
          tool.context.childRequestId.isNone ||
          !(decide (isTerminal tool.context.state) ||
            durableBackgroundHandoff world tool) then none
        else (resumeOwned s world generation).map fun next =>
          { next with dependencies :=
            s.dependencies.filter (fun entry => entry.1 != (world.requestId, generation)) }

/-- The application may change while a continuation is parked. Its trace
cannot spend or manufacture a local worker ticket; resumption rechecks the
current claim, lease, cancellation decision and exact child bridge. -/
inductive SchedulerTrace : (World × State) → (World × State) → Prop where
  | application {before after : World} (capacity : State)
      (trace : SessionComposition.Trace before after) :
      SchedulerTrace (before, capacity) (after, capacity)
  | wait {world : World} {before after : State} (generation : Generation)
      (document : DocId) (h : waitForChild before world generation document = some after) :
      SchedulerTrace (world, before) (world, after)
  | waitExisting {world : World} {before after : State} (generation : Generation)
      (selection : ExistingChildSelection) (document : DocId)
      (h : waitForExistingChild before world generation selection document = some after) :
      SchedulerTrace (world, before) (world, after)
  | resume {world : World} {before after : State} (generation : Generation)
      (cancellationAllows : Bool)
      (h : resumeAfterChild before world generation cancellationAllows = some after) :
      SchedulerTrace (world, before) (world, after)
  | resumeExisting {world : World} {before after : State} (generation : Generation)
      (selection : ExistingChildSelection) (cancellationAllows : Bool)
      (h : resumeAfterExistingChild before world generation selection cancellationAllows = some after) :
      SchedulerTrace (world, before) (world, after)
  | acquire {world : World} {before after : State} (ticket : Ticket)
      (h : admitFresh before ticket = some after) :
      SchedulerTrace (world, before) (world, after)
  | release (world : World) (before : State) (ticket : Ticket) :
      SchedulerTrace (world, before) (world, WorkerCapacity.release before ticket)
  | trans {first second third : World × State} :
      SchedulerTrace first second → SchedulerTrace second third → SchedulerTrace first third

theorem waitForChild_safe {s t : State} (world : World) (generation : Generation)
    (document : DocId) (hs : Safe s)
    (h : waitForChild s world generation document = some t) : Safe t := by
  unfold waitForChild at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hp : park s (world.requestId, generation) with
  | none => simp [hp] at h
  | some parked =>
      simp [hp] at h
      cases h
      simpa [Safe] using park_safe hs hp

theorem waitForExistingChild_safe {s t : State} (world : World)
    (generation : Generation) (selection : ExistingChildSelection) (document : DocId)
    (hs : Safe s)
    (h : waitForExistingChild s world generation selection document = some t) : Safe t := by
  unfold waitForExistingChild at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hp : park s (world.requestId, generation) with
  | none => simp [hp] at h
  | some parked =>
      simp [hp] at h
      cases h
      simpa [Safe] using park_safe hs hp

theorem admitFresh_safe {s t : State} {ticket : Ticket}
    (hs : Safe s) (h : admitFresh s ticket = some t) : Safe t := by
  unfold admitFresh at h
  split at h
  · contradiction
  · exact acquire_safe hs h

theorem resumeAfterChild_safe {s t : State} (world : World) (generation : Generation)
    (cancellationAllows : Bool) (hs : Safe s)
    (h : resumeAfterChild s world generation cancellationAllows = some t) : Safe t := by
  unfold resumeAfterChild at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hr : resumeOwned s world generation with
  | none => simp [hr] at h
  | some resumed =>
      simp [hr] at h
      cases h
      simpa [Safe] using resumeOwned_safe world generation hs hr

theorem resumeAfterExistingChild_safe {s t : State} (world : World)
    (generation : Generation) (selection : ExistingChildSelection)
    (cancellationAllows : Bool) (hs : Safe s)
    (h : resumeAfterExistingChild s world generation selection cancellationAllows = some t) :
    Safe t := by
  unfold resumeAfterExistingChild at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hr : resumeOwned s world generation with
  | none => simp [hr] at h
  | some resumed =>
      simp [hr] at h
      cases h
      simpa [Safe] using resumeOwned_safe world generation hs hr

theorem SchedulerTrace.safe {before after : World × State}
    (trace : SchedulerTrace before after) (hs : Safe before.2) : Safe after.2 := by
  induction trace with
  | application _ _ => exact hs
  | wait generation document h => exact waitForChild_safe _ generation document hs h
  | waitExisting generation selection document h =>
      exact waitForExistingChild_safe _ generation selection document hs h
  | resume generation allows h => exact resumeAfterChild_safe _ generation allows hs h
  | resumeExisting generation selection allows h =>
      exact resumeAfterExistingChild_safe _ generation selection allows hs h
  | acquire ticket h => exact admitFresh_safe hs h
  | release world before ticket => exact release_safe hs ticket
  | trans _ _ ih₁ ih₂ => exact ih₂ (ih₁ hs)

theorem SchedulerTrace.applicationTrace {before after : World × State}
    (trace : SchedulerTrace before after) :
    SessionComposition.Trace before.1 after.1 := by
  induction trace with
  | application _ application => exact application
  | wait _ _ _ => exact .refl _
  | waitExisting _ _ _ _ => exact .refl _
  | resume _ _ _ => exact .refl _
  | resumeExisting _ _ _ _ => exact .refl _
  | acquire _ _ => exact .refl _
  | release _ _ _ => exact .refl _
  | trans _ _ ih₁ ih₂ => exact .trans ih₁ ih₂

theorem waitForChild_records_exact_dependency {s t : State} (world : World)
    (generation : Generation) (document : DocId)
    (h : waitForChild s world generation document = some t) :
    (world.requestId, generation) ∈ t.parked ∧
      (world.requestId, generation) ∉ t.active ∧
      t.dependencies.lookup (world.requestId, generation) = some document := by
  unfold waitForChild at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hp : park s (world.requestId, generation) with
  | none => simp [hp] at h
  | some parked =>
      simp [hp] at h
      cases h
      obtain ⟨hparked, hactive⟩ := park_success_parked hp
      exact ⟨hparked, hactive, by simp⟩

theorem cancelled_parent_cannot_resume (s : State) (world : World)
    (generation : Generation) :
    resumeAfterChild s world generation false = none := by
  simp [resumeAfterChild]

theorem stale_parent_cannot_resume (s : State) (world : World)
    (generation : Generation)
    (h : ¬ RequestExecutionLease.admitted world.lease .mutationWriteGate generation) :
    resumeAfterChild s world generation true = none := by
  simp only [resumeAfterChild]
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  split <;> try rfl
  simp [resumeOwned, h]

theorem resumeAfterChild_requires_live_owner {s t : State} (world : World)
    (generation : Generation) (cancellationAllows : Bool)
    (h : resumeAfterChild s world generation cancellationAllows = some t) :
    (world.requestId, generation) ∈ s.parked ∧
      RequestExecutionLease.admitted world.lease .mutationWriteGate generation ∧
      world.lease.request = .processing := by
  unfold resumeAfterChild at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases hr : resumeOwned s world generation with
  | none => simp [hr] at h
  | some resumed => exact resumeOwned_requires_current_ticket world generation hr

/-- An accepted, dispatched child bridge from the existing tool owner, with
the current claim projected from the existing queue/claim fields. -/
private def childClaim (world : World) : World :=
  { world with
    queue := { scope := ⟨1, 1, none⟩, active := some 10, pending := [], terminal := ∅ }
    claimed := some ⟨10, 10, 1, .ordinary⟩
    retry := { world.retry with request := 10 } }

def runningChild : Option World := do
  let accepted ← (acceptAndPublish (Examples.world 5) 7
    Examples.providerTurn Examples.providerMessage []
    [ToolDelivery.Cases.bridgeAdmission]).toOption
  let dispatched ← (dispatch accepted 7 Examples.permit).toOption
  some (childClaim dispatched)

def completedChild : Option World := do
  let running ← runningChild
  let tool ← ownedToolByDocument? running 600
  (ToolDelivery.closeToolOutput running 600
    (.bridge (ToolDelivery.Cases.completedBridge tool.context) .bridge_complete)
    ToolDelivery.Cases.toolOutputClose).toOption

def backgroundedChild : Option World := do
  let running ← runningChild
  (changeToolControl running 7 600 .background).toOption

def receiptedBackgroundChild : Option World := do
  let backgrounded ← backgroundedChild
  (ToolDelivery.publishBackgroundReceipt backgrounded 600
    ToolDelivery.Cases.backgroundReceiptClose
    ToolDelivery.Cases.backgroundReceiptMessage).toOption

private def backgroundHandoffCapacityOne : Option Bool := do
  let running ← runningChild
  let modeOnly ← backgroundedChild
  let receipted ← receiptedBackgroundChild
  let active ← admitFresh (initial 1 1) (10, 7)
  let parked ← waitForChild active running 7 600
  let refused := (resumeAfterChild parked modeOnly 7 true).isNone
  let resumed ← resumeAfterChild parked receipted 7 true
  pure (refused && resumed.active == {(10, 7)} && resumed.parked == ∅ &&
    resumed.dependencies.isEmpty)

theorem background_handoff_requires_receipt_before_exact_resume :
    backgroundHandoffCapacityOne = some true := by native_decide

/-- The executable model witnesses capacity-one parent yield, child use of the
freed worker, actual tool closure, and exact dependency-based resumption. -/
private def composedCapacityOne : Option Bool := do
  let running ← runningChild
  let completed ← completedChild
  let parent ← acquire (initial 1 1) (10, 7)
  let waiting ← waitForChild parent running 7 600
  let child ← acquire waiting (50, 9)
  let free ← pure (release child (50, 9))
  let resumed ← resumeAfterChild free completed 7 true
  pure (resumed.active == {(10, 7)} && resumed.parked == ∅ &&
    resumed.dependencies.isEmpty && free.dependencies.lookup (10, 7) == some 600)

theorem composed_capacity_one_parent_yields_child_resumes :
    composedCapacityOne = some true := by native_decide

private def refusalCases : Option Bool := do
  let running ← runningChild
  let completed ← completedChild
  let parent ← acquire (initial 1 1) (10, 7)
  let waiting ← waitForChild parent running 7 600
  let cancelled := (resumeAfterChild waiting completed 7 false).isNone
  let stale := (resumeAfterChild waiting completed 8 true).isNone
  let early := (resumeAfterChild waiting running 7 true).isNone
  let wrongDependency := (resumeAfterChild
    { waiting with dependencies := [((10, 7), 601)] } completed 7 true).isNone
  let noParking ← acquire (initial 1 0) (10, 7)
  let overflow := (waitForChild noParking running 7 600).isNone
  pure (cancelled && stale && early && wrongDependency && overflow &&
    (10, 7) ∈ waiting.parked)


theorem cancelled_stale_unfinished_and_overflow_refused :
    refusalCases = some true := by native_decide

/-- The old bridge remains in the session world while a later request accepts
its own `wait_subagent` control call. The two accepted headers have distinct
physical request documents and generations. -/
def existingWaitTurn : Segment :=
  { id := 510, coordinate := ⟨11, .provider 0 1 0⟩, writer := .request 8
    flush := some ⟨0,
      [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩,
       ⟨1, 2, some
         { block := 1, part := 0, kind := .arguments
           tool := some ⟨"wait-610", none, "wait_subagent"⟩ }⟩],
      [65, 123, 125]⟩
    close := Examples.providerTurn.close
    createdAt := 5 }

def existingWaitMessage : MessageEnvelope :=
  { Examples.providerMessage with
    header := { Examples.providerMessage.header with
      id := 511, request := some 11, refs := [⟨510, 0⟩, ⟨510, 1⟩],
      publication := .requestExecution 8 }
    key := "wait-subagent-11", sequence := 1, nativeId := some "wait-610"
    blocks := [.text ⟨⟨510, 0⟩, .full⟩,
      .toolCall 610 "wait-610" none "wait_subagent" ⟨⟨510, 1⟩, .full⟩ none none] }

def existingWaitControl : ToolAdmission :=
  ⟨610, { ToolDelivery.Cases.bridgeAdmission.context with
    callId := 610, requestId := 11, childRequestId := none,
    awaitMode := .foreground, spawnBehaviorId := none }, none⟩

def existingWaitSeed : Option World := do
  let old ← runningChild
  some { old with
      requestId := 11
      lease := { old.lease with lease := .active 8 5 10, usedGenerations := [8, 7] }
      queue := { old.queue with active := some 11 }
      claimed := some ⟨11, 11, 1, .ordinary⟩
      retry := { old.retry with request := 11 } }

def existingWaitAccepted : Option World := do
  let current ← existingWaitSeed
  (acceptAndPublish current 8 existingWaitTurn existingWaitMessage []
    [existingWaitControl]).toOption

def existingWaitRunning : Option World := do
  let accepted ← existingWaitAccepted
  (dispatch accepted 8 ⟨610, true, true⟩).toOption

private def laterWaitSeed (old : World) : World :=
  { old with
    requestId := 11
    lease := { old.lease with lease := .active 8 5 10, usedGenerations := [8, 7] }
    queue := { old.queue with active := some 11 }
    claimed := some ⟨11, 11, 1, .ordinary⟩
    retry := { old.retry with request := 11 } }

def existingWaitModeOnlyRunning : Option World := do
  let old ← backgroundedChild
  let accepted ← (acceptAndPublish (laterWaitSeed old) 8 existingWaitTurn
    existingWaitMessage [] [existingWaitControl]).toOption
  (dispatch accepted 8 ⟨610, true, true⟩).toOption

def existingWaitHandoffRunning : Option World := do
  let old ← receiptedBackgroundChild
  let waitMessage := { existingWaitMessage with sequence := 2 }
  let accepted ← (acceptAndPublish (laterWaitSeed old) 8 existingWaitTurn
    waitMessage [] [existingWaitControl]).toOption
  (dispatch accepted 8 ⟨610, true, true⟩).toOption

def existingWaitCompleted : Option World := do
  let running ← existingWaitRunning
  let bridge ← ownedToolByDocument? running 600
  (ToolDelivery.closeToolOutput running 600
    (.bridge (ToolDelivery.Cases.completedBridge bridge.context) .bridge_complete)
    ToolDelivery.Cases.toolOutputClose).toOption

def existingChildSelection : ExistingChildSelection :=
  { viewer := ⟨10, 1, 1, 1⟩
    callerOwner := ⟨"session-1", "did:owner", none⟩
    bridgeOwner := ⟨"session-1", "did:owner", none⟩
    edge :=
      { rootRequestId := 10, rootSessionId := 1, parentRequestId := 10,
        parentToolCallId := 600, childRequestId := 42, childSessionId := some 2,
        ownerPrincipal := 1, controlPrincipal := 1, childPrincipal := 2,
        behaviorId := 8, lineageId := 1, awaitMode := .background,
        materialization := .local, lifecycle := .running,
        bridgeDurable := true, physicalCorroborated := true, directFromRoot := true }
    bridgeDocument := 600
    controlDocument := 610 }

def existingWaitCapacityOne : Option Bool := do
  let running ← existingWaitRunning
  let completed ← existingWaitCompleted
  let active ← admitFresh (initial 1 1) (11, 8)
  let parked ← waitForExistingChild active running 8 existingChildSelection 600
  let child ← admitFresh parked (42, 9)
  let released := release child (42, 9)
  let resumed ← resumeAfterExistingChild released completed 8
    { existingChildSelection with edge :=
      { existingChildSelection.edge with lifecycle := .completed } } true
  pure (resumed.active == {(11, 8)} && resumed.parked == ∅ &&
    resumed.dependencies.isEmpty && released.dependencies.lookup (11, 8) == some 600)

theorem existing_wait_capacity_one_resumes_prior_request_bridge :
    existingWaitCapacityOne = some true := by native_decide

private def existingWaitBackgroundHandoff : Option Bool := do
  let modeOnly ← existingWaitModeOnlyRunning
  let receipted ← existingWaitHandoffRunning
  let active ← admitFresh (initial 1 1) (11, 8)
  let modeParked ← waitForExistingChild active modeOnly 8 existingChildSelection 600
  let parked ← waitForExistingChild active receipted 8 existingChildSelection 600
  let refused := (resumeAfterExistingChild modeParked modeOnly 8
    existingChildSelection true).isNone
  let resumed ← resumeAfterExistingChild parked receipted 8
    existingChildSelection true
  pure (refused && resumed.active == {(11, 8)} && resumed.parked == ∅ &&
    resumed.dependencies.isEmpty)

theorem existing_wait_background_handoff_requires_exact_receipt :
    existingWaitBackgroundHandoff = some true := by native_decide

def existingWaitRefusals : Option Bool := do
  let running ← existingWaitRunning
  let completed ← existingWaitCompleted
  let active ← admitFresh (initial 1 1) (11, 8)
  let parked ← waitForExistingChild active running 8 existingChildSelection 600
  let stale := { completed with lease := { completed.lease with lease := .active 9 5 10 } }
  let unauthorized := { existingChildSelection with
    callerOwner := ⟨"session-1", "did:other", none⟩ }
  let wrongChild := { existingChildSelection with edge :=
    { existingChildSelection.edge with childRequestId := 43 } }
  let wrongControl := { existingChildSelection with controlDocument := 600 }
  pure ((waitForExistingChild active running 8 existingChildSelection 601).isNone &&
    (waitForExistingChild active running 8 unauthorized 600).isNone &&
    (waitForExistingChild active stale 8 existingChildSelection 600).isNone &&
    (resumeAfterExistingChild parked running 8 existingChildSelection true).isNone &&
    (resumeAfterExistingChild parked stale 8
      { existingChildSelection with edge :=
        { existingChildSelection.edge with lifecycle := .completed } } true).isNone &&
    (waitForExistingChild active running 8 wrongChild 600).isNone &&
    (waitForExistingChild active running 8 wrongControl 600).isNone)

theorem existing_wait_rejects_substitution_authority_and_stale_owner :
    existingWaitRefusals = some true := by native_decide

/-- Executed resource regression: the parent is retained, not terminalized,
while the child borrows the only active worker and then releases it. -/
def capacityOneRun : Option (Finset Ticket × Finset Ticket) := do
  let parent ← acquire (initial 1 1) (10, 7)
  let waiting ← park parent (10, 7)
  let child ← acquire waiting (20, 9)
  let resumed ← acquire (release child (20, 9)) (10, 7)
  pure (resumed.active, resumed.parked)

theorem capacity_one_parent_child_parent :
    capacityOneRun = some ({(10, 7)}, ∅) := by native_decide

theorem capacity_one_child_blocked_before_yield :
    (acquire (initial 1 1) (10, 7) >>= fun s => acquire s (20, 9)) = none := by
  native_decide

end CanonicalOutput.Execution.WorkerCapacity
