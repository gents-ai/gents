import Proofs.CanonicalOutput.Execution.Sequence
import Proofs.CanonicalOutput.Execution.ToolDelivery
import Proofs.CanonicalOutput.Execution.Compaction
import Proofs.StorageWriteGate

/-!
# Local gate / canonical transaction composition

The existing scheduling model describes when the gate holder can be polled and
released. This composition permits only that holder to apply a canonical
operation, against the current durable world rather than a pre-acquisition
snapshot. Operations are pure atomic commit results: native mutex exclusion,
DefraDB commit/rollback and authenticated actor binding remain refinement
obligations. No claim of cross-process exclusion or scheduler fairness is made.
-/
namespace CanonicalOutput.Execution.Gate

abbrev Actor := Nat

inductive Operation where
  | renew (generation : Generation) (expectedDeadline : Time)
  | append (generation : Generation) (record : Segment)
  | closeAuxiliary (generation : Generation) (closing : Segment)
  | retract (generation : Generation) (record : Segment)
  | accept (generation : Generation) (closing : Segment)
      (message : MessageEnvelope)
      (admissions : List ToolAdmission)
  | authored (generation : Generation) (closing : Segment) (message : MessageEnvelope)
  | headerOnly (generation : Generation) (message : MessageEnvelope)
      (admissions : List ToolAdmission)
  | dispatch (generation : Generation) (permit : DispatchPermit)
  | admitSpawned (generation : Generation) (admission : SpawnedToolAdmission)
  | toolControl (generation : Generation) (document : DocId)
      (action : ToolExecution.ToolCallContext.Action)
  | toolAppend (document : DocId) (record : Segment)
  | toolClose (document : DocId) (authority : ToolDelivery.CloseAuthority)
      (record : Segment)
  | toolComplete (document : DocId) (authority : ToolDelivery.CloseAuthority)
      (record : Segment) (message : MessageEnvelope)
  | toolDeliver (document : DocId) (message : MessageEnvelope)
  | toolGoalDeliver (document : DocId) (binding : GoalNotificationBinding)
      (message : MessageEnvelope)
  | backgroundReceipt (parentDocument : DocId) (closing : Segment)
      (message : MessageEnvelope)
  | compact (cursor : Transcript.Sequence)
  | closePartial (generation : Generation) (item : RecoveryItem)
  | recover (expected fresh : Generation) (duration deadline : Time) (items : List RecoveryItem)
  | recoverTerminal (expected fresh : Generation)
      (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
      (items : List RecoveryItem)
  | revoke (expected fresh : Generation) (outcome : RequestExecutionLease.Outcome)
      (selection : TerminalSelection)
  | terminalize (generation : Generation) (outcome : RequestExecutionLease.Outcome)
      (selection : TerminalSelection)

inductive Error where
  | execution (error : Execution.Error)
  | delivery (error : ToolDelivery.Error)
  | compactionRejected
  | purposeRejected
  deriving DecidableEq, Repr

def titleSource : Source → Bool
  | .auxiliary .title _ _ _ => true
  | _ => false

def titleRecord (world : World) (record : Segment) : Bool :=
  record.coordinate.request == world.requestId && titleSource record.coordinate.source

def titleRecoveryItem (world : World) (item : RecoveryItem) : Bool :=
  titleRecord world item.closing && item.message.isNone

/-- Ordinary title terminalization cannot strand an observed audit source. A
corrupt source still has the separate policy-revocation escape, which must not
invent or overwrite its bytes. -/
def titleSourcesDecided (world : World) : Bool :=
  (requestCoordinates world).all fun coordinate =>
    if coordinate.request != world.requestId || !titleSource coordinate.source then true
    else
      match (closures world.segments coordinate).dedup with
      | [closing] =>
          match closing.close with
          | some (.closed .complete count _)
          | some (.closed .«partial» count _) =>
              validateClosingRecord
                (extent world.segments coordinate count ++ [closing]) closing
          | some .retracted =>
              sourceIdentitiesValid world.segments coordinate &&
                closing.flush.isNone && writerMatchesSource coordinate closing.writer &&
                  match validateOpenPrefix world.segments coordinate closing.writer with
                  | .ok _ => true
                  | .error _ => false
          | none => false
      | _ => false

/-- A normal request cannot create a title-source record. Existing records do
not poison renewal or recovery of unrelated sources; each new write is checked
at its own operation boundary. -/
def normalOperationAllowed (_world : World) (operation : Operation) : Bool :=
  match operation with
    | .append _ record => !titleSource record.coordinate.source
    | .retract _ record => !titleSource record.coordinate.source
    | .closeAuxiliary _ closing => !titleSource closing.coordinate.source
    | .closePartial _ item => !titleSource item.closing.coordinate.source
    | .recover _ _ _ _ items =>
        items.all (fun item => !titleSource item.closing.coordinate.source)
    | .recoverTerminal _ _ _ _ items =>
        items.all (fun item => !titleSource item.closing.coordinate.source)
    | _ => true

/-- Title requests may write only their own headerless audit source under the
existing request lease. No ordinary provider, tool, transcript, goal or wake
publication can be introduced by this gate. -/
def titleOperationAllowed (world : World) : Operation → Bool
  | .renew .. => true
  | .append _ record => titleRecord world record
  | .closeAuxiliary _ closing => titleRecord world closing
  | .retract _ record => titleRecord world record
  | .closePartial _ item => titleRecoveryItem world item
  | .recover _ _ _ _ items => items.all (titleRecoveryItem world)
  | .recoverTerminal _ _ _ selection items =>
      selection == .noMessage && items.all (titleRecoveryItem world)
  | .revoke _ _ _ selection => selection == .noMessage
  | .terminalize generation outcome selection =>
      selection == .noMessage &&
        (terminalReplayPresent world generation outcome selection || titleSourcesDecided world)
  | _ => false

def purposeAllows (world : World) (operation : Operation) : Bool :=
  match world.purpose with
  | .normal => normalOperationAllowed world operation
  | .titleAudit => titleOperationAllowed world operation

def evaluateCore (operation : Operation) (world : World) : Except Error World :=
  match operation with
  | .renew generation expectedDeadline =>
      (Execution.renew world generation expectedDeadline).mapError .execution
  | .append generation record => (appendRaw world generation record).mapError .execution
  | .closeAuxiliary generation closing =>
      (Execution.closeAuxiliary world generation closing).mapError .execution
  | .retract generation record =>
      (retractBeforeRetry world generation record).mapError .execution
  | .accept generation closing message admissions =>
      (acceptAndPublish world generation closing message admissions).mapError .execution
  | .authored generation closing message =>
      (publishAuthored world generation closing message).mapError .execution
  | .headerOnly generation message admissions =>
      (publishHeaderOnly world generation message admissions).mapError .execution
  | .dispatch generation permit => (Execution.dispatch world generation permit).mapError .execution
  | .admitSpawned generation admission =>
      (admitSpawnedBackground world generation admission).mapError .execution
  | .toolControl generation document action =>
      (changeToolControl world generation document action).mapError .execution
  | .toolAppend document record =>
      (ToolDelivery.appendToolOutput world document record).mapError .delivery
  | .toolClose document authority record =>
      (ToolDelivery.closeToolOutput world document authority record).mapError .delivery
  | .toolComplete document authority record message =>
      (ToolDelivery.completeAndDeliver world document authority record message).mapError .delivery
  | .toolDeliver document message =>
      (ToolDelivery.publishToolDelivery world document message).mapError .delivery
  | .toolGoalDeliver document binding message =>
      (ToolDelivery.publishGoalNotification world document binding message).mapError .delivery
  | .backgroundReceipt parentDocument closing message =>
      (ToolDelivery.publishBackgroundReceipt world parentDocument closing message).mapError .delivery
  | .compact cursor =>
      match Compaction.advanceCursor? world cursor with
      | some after => .ok after
      | none => .error .compactionRejected
  | .closePartial generation item =>
      (closePartialAndPublish world generation item).mapError .execution
  | .recover expected fresh duration deadline items =>
      (recoverExpiredBatch world expected fresh duration deadline items).mapError .execution
  | .recoverTerminal expected fresh outcome selection items =>
      (recoverExpiredTerminal world expected fresh outcome selection items).mapError .execution
  | .revoke expected fresh outcome selection =>
      (revokeCorrupt world expected fresh outcome selection).mapError .execution
  | .terminalize generation outcome selection =>
      (Execution.terminalize world generation outcome selection).mapError .execution

def evaluate (operation : Operation) (world : World) : Except Error World :=
  if purposeAllows world operation then evaluateCore operation world
  else .error .purposeRejected

theorem evaluate_success_core (operation : Operation) (before after : World)
    (h : evaluate operation before = .ok after) :
    evaluateCore operation before = .ok after := by
  unfold evaluate at h
  split at h <;> try contradiction
  exact h

theorem title_rejects_publication (world : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope)
    (admissions : List ToolAdmission) (h : world.purpose = .titleAudit) :
    evaluate (.accept generation closing message admissions) world =
      .error .purposeRejected := by
  simp [evaluate, purposeAllows, titleOperationAllowed, h]

theorem normal_cannot_append_title_source (world : World) (generation : Generation)
    (record : Segment) (hpurpose : world.purpose = .normal)
    (hsource : titleSource record.coordinate.source = true) :
    evaluate (.append generation record) world = .error .purposeRejected := by
  simp [evaluate, purposeAllows, normalOperationAllowed, hpurpose, hsource]

theorem evaluate_nextSequence_monotone (operation : Operation) (before after : World)
    (h : evaluate operation before = .ok after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  have h := evaluate_success_core operation before after h
  unfold evaluateCore at h
  cases operation with
  | renew generation deadline =>
      have h' := mapError_success Error.execution _ _ h
      rw [renew_preserves_nextSeq before after generation deadline h']
  | append generation record =>
      have h' := mapError_success Error.execution _ _ h
      rw [appendRaw_preserves_nextSeq before after generation record h']
  | closeAuxiliary generation closing =>
      rcases closeAuxiliary_success_effect before after generation closing
        (mapError_success Error.execution _ _ h) with rfl | ⟨_, rfl⟩ <;> simp
  | retract generation record =>
      have h' := mapError_success Error.execution _ _ h
      rw [retractBeforeRetry_preserves_nextSeq before after generation record h']
  | accept generation closing message admissions =>
      exact acceptAndPublish_nextSeq_monotone before after generation closing message
        admissions (mapError_success Error.execution _ _ h)
  | authored generation closing message =>
      exact publishAuthored_nextSeq_monotone before after generation closing message
        (mapError_success Error.execution _ _ h)
  | headerOnly generation message admissions =>
      exact publishHeaderOnly_nextSeq_monotone before after generation message admissions
        (mapError_success Error.execution _ _ h)
  | dispatch generation permit =>
      rw [dispatch_preserves_nextSeq before after generation permit
        (mapError_success Error.execution _ _ h)]
  | admitSpawned generation admission =>
      rw [admitSpawnedBackground_preserves_nextSeq before after generation admission
        (mapError_success Error.execution _ _ h)]
  | toolControl generation document action =>
      rw [changeToolControl_preserves_nextSeq before after generation document action
        (mapError_success Error.execution _ _ h)]
  | toolAppend document record =>
      rw [ToolDelivery.append_preserves_nextSeq before after document record
        (mapError_success Error.delivery _ _ h)]
  | toolClose document authority record =>
      rw [ToolDelivery.close_preserves_nextSeq before after document authority record
        (mapError_success Error.delivery _ _ h)]
  | toolComplete document authority record message =>
      obtain ⟨closed, hclose, hdeliver⟩ := ToolDelivery.completeAndDeliver_success
        before after document authority record message
        (mapError_success Error.delivery _ _ h)
      exact Nat.le_trans
        (by rw [ToolDelivery.close_preserves_nextSeq before closed document authority record hclose])
        (ToolDelivery.publication_nextSeq_monotone closed after document message hdeliver)
  | toolDeliver document message =>
      exact ToolDelivery.publication_nextSeq_monotone before after document message
        (mapError_success Error.delivery _ _ h)
  | toolGoalDeliver document binding message =>
      exact ToolDelivery.goal_notification_nextSeq_monotone before after document binding message
        (mapError_success Error.delivery _ _ h)
  | backgroundReceipt document closing message =>
      exact ToolDelivery.background_receipt_nextSeq_monotone before after document closing message
        (mapError_success Error.delivery _ _ h)
  | compact cursor =>
      cases hcompact : Compaction.advanceCursor? before cursor with
      | none => simp [hcompact] at h
      | some post =>
          simp [hcompact] at h
          subst after
          rw [(Compaction.advanceCursor_preserves_publications before post cursor hcompact).1]
  | closePartial generation item =>
      exact closePartialAndPublish_nextSeq_monotone before after generation item
        (mapError_success Error.execution _ _ h)
  | recover expected fresh duration deadline items =>
      exact recoverExpiredBatch_nextSeq_monotone before after expected fresh duration deadline items
        (mapError_success Error.execution _ _ h)
  | recoverTerminal expected fresh outcome selection items =>
      exact recoverExpiredTerminal_nextSeq_monotone before after expected fresh outcome selection
        items (mapError_success Error.execution _ _ h)
  | revoke expected fresh outcome selection =>
      rw [revokeCorrupt_preserves_nextSeq before after expected fresh outcome selection
        (mapError_success Error.execution _ _ h)]
  | terminalize generation outcome selection =>
      rw [terminalize_preserves_nextSeq before after generation outcome selection
        (mapError_success Error.execution _ _ h)]

def initial (world : World) : World :=
  { world with gateOwner := none, gateSchedule := ⟨.released, true, false⟩ }

def acquire (state : World) (actor : Actor) (independent : Bool) : Option World :=
  if state.gateOwner.isSome || StorageWriteGate.held state.gateSchedule then none
  else some { state with gateOwner := some actor, gateSchedule := ⟨.storage, independent, false⟩ }

def scheduling (state : World) (actor : Actor) (event : StorageWriteGate.Event) : Option World :=
  if state.gateOwner != some actor then none
  else
    let schedule := StorageWriteGate.step state.gateSchedule event
    some { state with
      gateSchedule := schedule
      gateOwner := if StorageWriteGate.held schedule then state.gateOwner else none }

def atTime (world : World) (now : Time) : World :=
  { world with lease := { world.lease with now := now } }

def finishCommit (before execution : World) : World :=
  { execution with
    gateOwner := before.gateOwner
    gateSchedule := { before.gateSchedule with phase := .releasable }
    queue := before.queue
    claimed := before.claimed
    retry := before.retry }

/-- The authoritative read and the result are inside the same held gate.
Failed evaluation commits nothing. The holder must still finish cleanup/release
through the scheduling owner; an error is not an implicit unlocked state. -/
def commit (state : World) (actor : Actor) (now : Time) (operation : Operation) : Option World :=
  if state.gateOwner != some actor || state.gateSchedule.phase != .storage ||
      !StorageWriteGate.pollable state.gateSchedule || now < state.lease.now then none
  else match evaluate operation (atTime state now) with
    | .error _ => none
    | .ok execution => some (finishCommit state execution)

theorem other_actor_cannot_commit (state : World) (actor : Actor) (now : Time)
    (operation : Operation) (h : state.gateOwner ≠ some actor) :
    commit state actor now operation = none := by
  simp [commit, h]

theorem successful_commit_identifies_holder (before after : World) (actor : Actor)
    (now : Time) (operation : Operation)
    (h : commit before actor now operation = some after) : before.gateOwner = some actor := by
  by_contra howner
  rw [other_actor_cannot_commit before actor now operation howner] at h
  contradiction

theorem two_successes_from_one_gate_have_same_actor
    (before left right : World) (actorLeft actorRight : Actor) (timeLeft timeRight : Time)
    (opLeft opRight : Operation)
    (hleft : commit before actorLeft timeLeft opLeft = some left)
    (hright : commit before actorRight timeRight opRight = some right) :
    actorLeft = actorRight := by
  have hl := successful_commit_identifies_holder before left actorLeft timeLeft opLeft hleft
  have hr := successful_commit_identifies_holder before right actorRight timeRight opRight hright
  exact Option.some.inj (hl.symm.trans hr)

theorem held_gate_cannot_be_acquired (state : World) (actor : Actor) (independent : Bool)
    (h : StorageWriteGate.held state.gateSchedule = true) :
    acquire state actor independent = none := by
  simp [acquire, h]

theorem suspended_holder_cannot_commit (state : World) (actor : Actor) (now : Time)
    (operation : Operation) (h : state.gateSchedule = StorageWriteGate.suspended) :
    commit state actor now operation = none := by
  simp [commit, h, StorageWriteGate.suspended, StorageWriteGate.pollable]

theorem scheduling_preserves_durable_world (before after : World) (actor : Actor)
    (event : StorageWriteGate.Event) (h : scheduling before actor event = some after) :
    after = { before with gateOwner := after.gateOwner, gateSchedule := after.gateSchedule } := by
  unfold scheduling at h
  split at h
  · contradiction
  · cases h; rfl

theorem acquire_preserves_durable_world (before after : World) (actor : Actor)
    (independent : Bool) (h : acquire before actor independent = some after) :
    after = { before with gateOwner := after.gateOwner, gateSchedule := after.gateSchedule } := by
  unfold acquire at h
  split at h
  · contradiction
  · cases h; rfl

/-- A core equation, not a postcondition rechecked by a wrapper: successful
commit evaluated this operation on the latest world held by this gate. -/
theorem commit_reads_current_world (before after : World) (actor : Actor) (now : Time)
    (operation : Operation) (h : commit before actor now operation = some after) :
    ∃ execution, evaluate operation (atTime before now) = .ok execution ∧
      after = finishCommit before execution := by
  unfold commit at h
  split at h
  · contradiction
  · cases heval : evaluate operation (atTime before now) with
    | error error => simp [heval] at h
    | ok execution => simp [heval] at h; cases h; exact ⟨execution, rfl, rfl⟩

theorem committed_gate_stays_held (before after : World) (actor : Actor) (now : Time)
    (operation : Operation) (h : commit before actor now operation = some after) :
    after.gateOwner = before.gateOwner ∧ StorageWriteGate.held after.gateSchedule = true := by
  unfold commit at h
  split at h
  · contradiction
  · cases heval : evaluate operation (atTime before now) with
    | error error => simp [heval] at h
    | ok execution => simp [heval] at h; cases h; exact ⟨rfl, rfl⟩

theorem commit_preserves_composed_control (before after : World) (actor : Actor) (now : Time)
    (operation : Operation) (h : commit before actor now operation = some after) :
    after.queue = before.queue ∧ after.claimed = before.claimed ∧ after.retry = before.retry := by
  unfold commit at h
  split at h <;> try contradiction
  cases heval : evaluate operation (atTime before now) with
  | error error => simp [heval] at h
  | ok execution => simp [heval] at h; cases h; exact ⟨rfl, rfl, rfl⟩

/-- Even when storage has returned, a sibling cannot acquire until the
existing owner performs the explicit release step. -/
theorem sibling_waits_for_release (before after : World) (actor other : Actor)
    (now : Time) (operation : Operation)
    (h : commit before actor now operation = some after) (independent : Bool) :
    acquire after other independent = none :=
  held_gate_cannot_be_acquired after other independent
    (committed_gate_stays_held before after actor now operation h).2

theorem next_holder_reads_previous_commit
    (committed released acquired after : World) (previous next : Actor)
    (independent : Bool) (now : Time) (operation : Operation)
    (hrelease : scheduling committed previous .release = some released)
    (hacquire : acquire released next independent = some acquired)
    (hcommit : commit acquired next now operation = some after) :
    acquired = { committed with gateOwner := acquired.gateOwner, gateSchedule := acquired.gateSchedule } ∧
    ∃ execution, evaluate operation (atTime acquired now) = .ok execution ∧
      after = finishCommit acquired execution := by
  have hr := scheduling_preserves_durable_world committed released previous .release hrelease
  have ha := acquire_preserves_durable_world released acquired next independent hacquire
  constructor
  · rw [ha, hr]
  · exact commit_reads_current_world acquired after next now operation hcommit

theorem successful_commit_nextSequence_monotone
    (before after : World) (actor : Actor) (now : Time) (operation : Operation)
    (h : commit before actor now operation = some after) :
    before.transcript.nextSeq ≤ after.transcript.nextSeq := by
  unfold commit at h
  split at h <;> try contradiction
  cases heval : evaluate operation (atTime before now) with
  | error error => simp [heval] at h
  | ok execution =>
      simp [heval] at h
      cases h
      exact evaluate_nextSequence_monotone operation (atTime before now) execution heval

set_option maxHeartbeats 2000000 in
/-- Ordinary gated commits cannot retarget the physical request or session.
Only the separate handover owner changes the active request identity. -/
theorem evaluate_preserves_request_identity (operation : Operation) (before after : World)
    (h : evaluate operation before = .ok after) :
    after.requestId = before.requestId ∧ after.sessionId = before.sessionId := by
  replace h := evaluate_success_core operation before after h
  cases operation <;> simp only [evaluateCore] at h
  all_goals first
    | exact ToolDelivery.tool_write_preserves_request_identity
        (mapError_success Error.delivery _ _ h)
    | skip
  case compact cursor =>
    cases hc : Compaction.advanceCursor? before cursor with
    | none => simp [hc] at h
    | some post =>
      simp [hc] at h
      subst after
      unfold Compaction.advanceCursor? at hc
      repeat' first | contradiction | (solve | cases hc; exact ⟨rfl, rfl⟩) | split at hc
  case accept generation closing message admissions =>
    have hcore := mapError_success Error.execution _ _ h
    replace hcore := checked_core_success _ _ _ hcore
    rcases acceptAndPublishCore_success_effect before after generation closing message
      admissions hcore with ⟨rfl, _⟩ | ⟨_, _, rfl, _⟩ <;> exact ⟨rfl, rfl⟩
  case closeAuxiliary generation closing =>
    rcases closeAuxiliary_success_effect before after generation closing
      (mapError_success Error.execution _ _ h) with rfl | ⟨_, rfl⟩ <;> exact ⟨rfl, rfl⟩
  case toolComplete document authority record message =>
    have hcomposed := mapError_success Error.delivery _ _ h
    obtain ⟨closed, hclose, hdeliver⟩ := ToolDelivery.completeAndDeliver_success
      before after document authority record message hcomposed
    have hc := ToolDelivery.tool_write_preserves_request_identity hclose
    have hd := ToolDelivery.tool_write_preserves_request_identity hdeliver
    exact ⟨hd.1.trans hc.1, hd.2.trans hc.2⟩
  all_goals
    have hcore := mapError_success Error.execution _ _ h
    try replace hcore := checked_core_success _ _ _ hcore
    first
      | exact closePartialAndPublishCore_preserves_request_identity _ _ _ _ hcore
      | exact recoverExpiredBatchCore_preserves_request_identity _ _ _ _ _ _ _ hcore
      | exact recoverExpiredTerminalCore_preserves_request_identity _ _ _ _ _ _ _ hcore
      | exact terminalizeCore_preserves_request_identity _ _ _ _ _ hcore
      | exact revokeCorruptCore_preserves_request_identity _ _ _ _ _ _ hcore
      | simp only [Execution.renew, renewCore, appendRaw, appendRawCore,
          retractBeforeRetryCore, acceptAndPublishCore, publishAuthoredCore,
          publishHeaderOnlyCore, dispatchCore, admitSpawnedBackgroundCore,
          changeToolControlCore] at hcore
        try dsimp only at hcore
        repeat' first
          | contradiction
          | (solve | cases hcore; exact ⟨rfl, rfl⟩)
          | split at hcore

theorem successful_commit_preserves_request_identity (before after : World)
    (actor : Actor) (now : Time) (operation : Operation)
    (h : commit before actor now operation = some after) :
    after.requestId = before.requestId ∧ after.sessionId = before.sessionId := by
  obtain ⟨execution, he, rfl⟩ := commit_reads_current_world before after actor now operation h
  exact evaluate_preserves_request_identity operation (atTime before now) execution he

end CanonicalOutput.Execution.Gate
