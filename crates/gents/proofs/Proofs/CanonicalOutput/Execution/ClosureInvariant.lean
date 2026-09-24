import Proofs.CanonicalOutput.Execution.SessionComposition

/-!
# Closure uniqueness across composed execution

Closure uniqueness is a property of the durable segment collection, rather
than an admission premise on the application trace. Exact duplicate delivery
is permitted: every pair of visible closure records at a source coordinate
must denote the same immutable record.
-/
namespace CanonicalOutput.Execution.SessionComposition

def ClosureUnique (state : World) : Prop :=
  ∀ coordinate left, left ∈ closures state.segments coordinate →
    ∀ right, right ∈ closures state.segments coordinate → left = right

private theorem closureUnique_of_segments_eq {before after : World}
    (unique : ClosureUnique before) (h : after.segments = before.segments) :
    ClosureUnique after := by
  simpa [ClosureUnique, h] using unique

private theorem closureUnique_append_nonclosure (records : List Segment) (record : Segment)
    (unique : ∀ coordinate left, left ∈ closures records coordinate →
      ∀ right, right ∈ closures records coordinate → left = right)
    (openRecord : record.close = none) :
    ∀ coordinate left, left ∈ closures (records ++ [record]) coordinate →
      ∀ right, right ∈ closures (records ++ [record]) coordinate → left = right := by
  simpa [closures, sourceRecords, List.filter_append, openRecord] using unique

private theorem closureUnique_append_fresh (records : List Segment) (record : Segment)
    (unique : ∀ coordinate left, left ∈ closures records coordinate →
      ∀ right, right ∈ closures records coordinate → left = right)
    (fresh : (closures records record.coordinate).isEmpty = true) :
    ∀ coordinate left, left ∈ closures (records ++ [record]) coordinate →
      ∀ right, right ∈ closures (records ++ [record]) coordinate → left = right := by
  intro coordinate left hleft right hright
  have members {candidate : Segment} :
      candidate ∈ closures (records ++ [record]) coordinate ↔
        candidate ∈ closures records coordinate ∨
          (candidate = record ∧ record.coordinate = coordinate ∧ record.close.isSome = true) := by
    simp [closures, sourceRecords]
    aesop
  have hempty : closures records record.coordinate = [] := List.isEmpty_iff.mp fresh
  rw [members] at hleft hright
  rcases hleft with hold | ⟨rfl, hcoordinate, _⟩
  · rcases hright with hright | ⟨rfl, hcoordinate, _⟩
    · exact unique coordinate left hold right hright
    · subst coordinate
      simp [hempty] at hold
  · rcases hright with hright | ⟨rfl, _, _⟩
    · subst coordinate
      simp [hempty] at hright
    · rfl

private theorem closureUnique_append_winner (records : List Segment) (record : Segment)
    (unique : ∀ coordinate left, left ∈ closures records coordinate →
      ∀ right, right ∈ closures records coordinate → left = right)
    (winner : (closures (records ++ [record]) record.coordinate).dedup = [record]) :
    ∀ coordinate left, left ∈ closures (records ++ [record]) coordinate →
      ∀ right, right ∈ closures (records ++ [record]) coordinate → left = right := by
  intro coordinate left hleft right hright
  by_cases hc : coordinate = record.coordinate
  · subst coordinate
    have hl : left ∈ (closures (records ++ [record]) record.coordinate).dedup :=
      List.mem_dedup.mpr hleft
    have hr : right ∈ (closures (records ++ [record]) record.coordinate).dedup :=
      List.mem_dedup.mpr hright
    rw [winner] at hl hr
    simp_all
  · have members {candidate : Segment} :
        candidate ∈ closures (records ++ [record]) coordinate ↔
          candidate ∈ closures records coordinate := by
      have hne : record.coordinate ≠ coordinate := fun h => hc h.symm
      simp [closures, sourceRecords, hne]
    exact unique coordinate left (members.mp hleft) right (members.mp hright)

private theorem validateClosingRecord_winner (segments : List Segment) (closing : Segment)
    (h : validateClosingRecord segments closing = true) :
    (closures segments closing.coordinate).dedup = [closing] := by
  unfold validateClosingRecord at h
  cases hc : uniqueRecord LookupError.unavailable .conflictingClosures
      (closures segments closing.coordinate) with
  | error error => simp [hc] at h
  | ok only =>
      simp [hc] at h
      have heq : only = closing := by simpa using h.1.1
      subst only
      exact (uniqueRecord_eq_ok_iff _ _ _ _).mp hc

private theorem appendRaw_preserves (before after : World) (generation : Generation)
    (record : Segment) (unique : ClosureUnique before)
    (h : appendRaw before generation record = .ok after) : ClosureUnique after := by
  unfold appendRaw appendRawCore at h
  split at h <;> try contradiction
  split at h
  · split at h <;> try contradiction
    cases h
    exact unique
  · split at h <;> try contradiction
    split at h <;> try contradiction
    rename_i lease hlease
    dsimp only at h
    split at h <;> try contradiction
    cases h
    apply closureUnique_append_nonclosure before.segments record unique
    simp_all

private theorem retract_preserves (before after : World) (generation : Generation)
    (record : Segment) (unique : ClosureUnique before)
    (h : retractBeforeRetry before generation record = .ok after) : ClosureUnique after := by
  unfold retractBeforeRetry at h
  cases hc : retractBeforeRetryCore before generation record with
  | error error => simp [checked, hc] at h
  | ok post =>
      simp only [checked, hc] at h
      split at h <;> try contradiction
      cases h
      unfold retractBeforeRetryCore at hc
      split at hc <;> try contradiction
      split at hc <;> try contradiction
      split at hc
      · split at hc <;> try contradiction
        cases hc
        exact unique
      · split at hc <;> try contradiction
        rename_i hopen
        split at hc <;> try contradiction
        rename_i hopen
        cases hlease : RequestExecutionLease.step? before.lease
            (.authorizeProducerDecision .mutationWriteGate generation .closeOrRetract) with
        | none => simp [hlease] at hc
        | some lease =>
          simp only [hlease] at hc
          injection hc with he
          subst after
          apply closureUnique_append_fresh before.segments record unique
          simpa [sourceOpen] using hopen

private theorem auxiliaryClose_preserves (before after : World) (generation : Generation)
    (closing : Segment) (unique : ClosureUnique before)
    (h : closeAuxiliary before generation closing = .ok after) : ClosureUnique after := by
  rcases closeAuxiliary_success_effect before after generation closing h with rfl | ⟨_, rfl⟩
  · exact unique
  · apply closureUnique_append_winner before.segments closing unique
    have hp := checked_success _ _ _ h
    simp only [Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] at hp
    exact hp.1.2

private theorem accept_preserves (before after : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope) (targets : List RemoteTarget)
    (admissions : List ToolAdmission) (unique : ClosureUnique before)
    (h : acceptAndPublish before generation closing message targets admissions = .ok after) :
    ClosureUnique after := by
  have hcore := checked_core_success _ _ _ h
  rcases acceptAndPublishCore_success_effect before after generation closing message targets
      admissions hcore with ⟨rfl, _⟩ | ⟨lease, delegated, _, rfl, _⟩
  · exact unique
  · apply closureUnique_append_winner before.segments closing unique
    have valid := (accepted_publication_is_composed_atomically before _ generation closing
      message targets admissions h).2.2.2.2.1
    exact validateClosingRecord_winner _ _ valid

private theorem authored_core_effect (before after : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope)
    (h : publishAuthoredCore before generation closing message = .ok after) :
    after = before ∨ ∃ lease, after = { before with
      lease := lease
      segments := before.segments ++ [closing]
      messages := before.messages ++ [message]
      transcript := appendAuthoredRow before.transcript message } := by
  unfold publishAuthoredCore at h
  dsimp only at h
  repeat' first | contradiction | split at h
  all_goals try { cases h; exact Or.inl rfl }
  all_goals try {
    rename_i lease hlease
    cases h
    exact Or.inr ⟨lease, rfl⟩ }

private theorem authored_preserves (before after : World) (generation : Generation)
    (closing : Segment) (message : MessageEnvelope) (unique : ClosureUnique before)
    (h : publishAuthored before generation closing message = .ok after) :
    ClosureUnique after := by
  have hcore := checked_core_success _ _ _ h
  rcases authored_core_effect before after generation closing message hcore with rfl | ⟨lease, rfl⟩
  · exact unique
  · apply closureUnique_append_winner before.segments closing unique
    have hp := checked_success _ _ _ h
    simp only [Bool.and_eq_true] at hp
    exact validateClosingRecord_winner _ _ hp.1.2

private theorem headerOnly_segments (before after : World) (generation : Generation)
    (message : MessageEnvelope) (admissions : List ToolAdmission)
    (h : publishHeaderOnly before generation message admissions = .ok after) :
    after.segments = before.segments := by
  have hcore := checked_core_success _ _ _ h
  unfold publishHeaderOnlyCore at hcore
  dsimp only at hcore
  repeat' first | contradiction | split at hcore
  all_goals cases hcore
  all_goals rfl

private theorem dispatch_segments (before after : World) (generation : Generation)
    (permit : DispatchPermit) (h : Execution.dispatch before generation permit = .ok after) :
    after.segments = before.segments := by
  unfold Execution.dispatch at h
  have hcore := checked_core_success _ _ _ h
  unfold dispatchCore at hcore
  dsimp only at hcore
  repeat' first | contradiction | split at hcore
  all_goals cases hcore
  all_goals rfl

private theorem spawned_segments (before after : World) (generation : Generation)
    (admission : SpawnedToolAdmission)
    (h : admitSpawnedBackground before generation admission = .ok after) :
    after.segments = before.segments := by
  have hcore := checked_core_success _ _ _ h
  unfold admitSpawnedBackgroundCore at hcore
  dsimp only at hcore
  repeat' first | contradiction | split at hcore
  all_goals cases hcore
  all_goals rfl

private theorem toolControl_segments (before after : World) (generation : Generation)
    (document : DocId) (action : ToolExecution.ToolCallContext.Action)
    (h : changeToolControl before generation document action = .ok after) :
    after.segments = before.segments := by
  have hcore := checked_core_success _ _ _ h
  unfold changeToolControlCore at hcore
  dsimp only at hcore
  repeat' first | contradiction | split at hcore
  all_goals cases hcore
  all_goals rfl

private theorem accountOwnedTools_segments (world : World) (generation : Generation)
    (interruptRunning : Bool) :
    (accountOwnedTools world generation interruptRunning).segments = world.segments := by
  unfold accountOwnedTools
  generalize world.toolContexts = tools
  induction tools generalizing world with
  | nil => rfl
  | cons tool rest ih =>
      simp only [List.foldl_cons]
      rw [ih]

private theorem terminalize_segments (before after : World) (generation : Generation)
    (outcome : RequestExecutionLease.Outcome) (selection : TerminalSelection)
    (h : Execution.terminalize before generation outcome selection = .ok after) :
    after.segments = before.segments := by
  unfold Execution.terminalize at h
  have hcore := checked_core_success _ _ _ h
  unfold terminalizeCore at hcore
  dsimp only at hcore
  repeat' first | contradiction | split at hcore
  all_goals cases hcore
  all_goals first | rfl | exact accountOwnedTools_segments before generation _

private theorem prepareRecoveryItems_segment_provenance
    (world : World) (expected fresh : Generation) (items : List RecoveryItem)
    (prepared result : RecoveryPrepared)
    (h : prepareRecoveryItems world expected fresh items prepared = .ok result) :
    ∀ record ∈ result.segments,
      record ∈ prepared.segments ∨ ∃ item ∈ items, record = item.closing := by
  induction items generalizing prepared result with
  | nil =>
      simp [prepareRecoveryItems] at h
      subst result
      simp
  | cons item rest ih =>
      unfold prepareRecoveryItems at h
      dsimp only at h
      repeat' first | contradiction | split at h
      all_goals
        intro record hrecord
        have hp := ih _ _ h record hrecord
        rcases hp with hp | ⟨prior, hprior, rfl⟩
        · by_cases hbase : record ∈ prepared.segments
          · exact Or.inl hbase
          · right
            refine ⟨item, by simp, ?_⟩
            simp_all
        · exact Or.inr ⟨prior, by simp [hprior]⟩

private theorem closureUnique_of_segment_provenance
    (before after : World) (items : List RecoveryItem)
    (unique : ClosureUnique before)
    (provenance : ∀ record ∈ after.segments,
      record ∈ before.segments ∨ ∃ item ∈ items, record = item.closing)
    (winners : ∀ item ∈ items,
      (closures after.segments item.closing.coordinate).dedup = [item.closing]) :
    ClosureUnique after := by
  intro coordinate left hleft right hright
  have leftParts := hleft
  have rightParts := hright
  simp only [closures, sourceRecords, List.mem_filter, beq_iff_eq,
    Bool.and_eq_true] at leftParts rightParts
  have leftSegment : left ∈ after.segments := leftParts.1.1
  have rightSegment : right ∈ after.segments := rightParts.1.1
  rcases provenance left leftSegment with holdLeft | ⟨item, hitem, rfl⟩
  · rcases provenance right rightSegment with holdRight | ⟨item, hitem, rfl⟩
    · apply unique coordinate left
      · simp [closures, sourceRecords, holdLeft, leftParts.2, leftParts.1.2]
      · simp [closures, sourceRecords, holdRight, rightParts.2, rightParts.1.2]
    · have hw := winners item hitem
      have hm : left ∈ (closures after.segments item.closing.coordinate).dedup := by
        apply List.mem_dedup.mpr
        have hc : item.closing.coordinate = coordinate := rightParts.1.2
        simpa [hc] using hleft
      rw [hw] at hm
      simpa using hm
  · have hw := winners item hitem
    have hm : right ∈ (closures after.segments item.closing.coordinate).dedup := by
      apply List.mem_dedup.mpr
      have hc : item.closing.coordinate = coordinate := leftParts.1.2
      simpa [hc] using hright
    rw [hw] at hm
    simp at hm
    exact hm.symm

private theorem closePartial_preserves (before after : World) (generation : Generation)
    (item : RecoveryItem) (unique : ClosureUnique before)
    (h : closePartialAndPublish before generation item = .ok after) : ClosureUnique after := by
  have hpredicate := checked_success _ _ _ h
  simp only [Bool.and_eq_true] at hpredicate
  have hcore := checked_core_success _ _ _ h
  unfold closePartialAndPublishCore at hcore
  split at hcore
  · cases hcore; exact unique
  · split at hcore <;> try contradiction
    rename_i prepared hprepared
    split at hcore <;> try contradiction
    cases hcore
    apply closureUnique_of_segment_provenance before _ [item] unique
    · exact prepareRecoveryItems_segment_provenance before generation generation [item]
        _ prepared hprepared
    · intro candidate hcandidate
      simp only [List.mem_singleton] at hcandidate
      subst candidate
      simpa using hpredicate.1.1.1.2

private theorem recovery_preserves (before after : World) (expected fresh : Generation)
    (duration deadline : Time) (items : List RecoveryItem) (unique : ClosureUnique before)
    (h : recoverExpiredBatch before expected fresh duration deadline items = .ok after) :
    ClosureUnique after := by
  have hcore := checked_core_success _ _ _ h
  unfold recoverExpiredBatchCore at hcore
  split at hcore
  · cases hcore
    exact unique
  · cases hb : prepareRecoveryBatch before expected fresh items with
    | error error => simp [hb] at hcore
    | ok prepared =>
        simp only [hb] at hcore
        cases hl : RequestExecutionLease.step?
            (preparedRecoveryWorld before prepared expected).lease
            (.recoverExpired .mutationWriteGate expected fresh duration deadline) with
        | none => simp [hl] at hcore
        | some lease =>
            simp only [hl] at hcore
            injection hcore with heq
            subst after
            apply closureUnique_of_segment_provenance before _ items unique
            · intro record hrecord
              have hsegments :
                  (preparedRecoveryWorld before prepared expected).segments = prepared.segments := by
                unfold preparedRecoveryWorld
                exact accountOwnedTools_segments _ expected true
              rw [hsegments] at hrecord
              unfold prepareRecoveryBatch at hb
              split at hb <;> try contradiction
              exact prepareRecoveryItems_segment_provenance before expected fresh items
                ⟨before.segments, before.messages, before.transcript⟩ prepared hb record hrecord
            · intro item hitem
              have post := recovery_is_all_sources_single_winner_and_exact before _ expected fresh
                duration deadline items h
              have hall := List.all_eq_true.mp post.2.2.2
              have hi := hall item hitem
              simp only [Bool.and_eq_true] at hi
              simpa only [beq_iff_eq] using hi.1.1

private theorem terminal_recovery_preserves (before after : World)
    (expected fresh : Generation) (outcome : RequestExecutionLease.Outcome)
    (selection : TerminalSelection) (items : List RecoveryItem)
    (unique : ClosureUnique before)
    (h : recoverExpiredTerminal before expected fresh outcome selection items = .ok after) :
    ClosureUnique after := by
  have hcore := checked_core_success _ _ _ h
  unfold recoverExpiredTerminalCore at hcore
  split at hcore
  · cases hcore; exact unique
  · split at hcore
    · contradiction
    · cases hb : prepareRecoveryBatch before expected fresh items with
      | error error => simp [hb] at hcore
      | ok prepared =>
          simp only [hb] at hcore
          split at hcore
          · contradiction
          · cases hl : RequestExecutionLease.step?
                (preparedRecoveryWorld before prepared expected).lease
                (.recoverExpiredTerminal .mutationWriteGate expected fresh outcome) with
            | none => simp [hl] at hcore
            | some lease =>
                simp only [hl] at hcore
                cases hcore
                apply closureUnique_of_segment_provenance before _ items unique
                · intro record hrecord
                  have hsegments :
                      (preparedRecoveryWorld before prepared expected).segments =
                        prepared.segments := by
                    unfold preparedRecoveryWorld
                    exact accountOwnedTools_segments _ expected true
                  rw [hsegments] at hrecord
                  unfold prepareRecoveryBatch at hb
                  split at hb <;> try contradiction
                  exact prepareRecoveryItems_segment_provenance before expected fresh items
                    ⟨before.segments, before.messages, before.transcript⟩ prepared hb record hrecord
                · intro item hitem
                  have hp := checked_success _ _ _ h
                  simp only [Bool.and_eq_true] at hp
                  have hall := List.all_eq_true.mp hp.2
                  have hi := hall item hitem
                  simp only [Bool.and_eq_true] at hi
                  simpa only [beq_iff_eq] using hi.1.1

private theorem toolAppend_preserves (before after : World) (document : DocId)
    (record : Segment) (unique : ClosureUnique before)
    (h : ToolDelivery.appendToolOutput before document record = .ok after) :
    ClosureUnique after := by
  rcases ToolDelivery.append_success_segment_effect before after document record h with
      same | ⟨added, nonclosure, _, _⟩
  · exact closureUnique_of_segments_eq unique same
  · unfold ClosureUnique
    rw [added]
    exact closureUnique_append_nonclosure before.segments record unique nonclosure

private theorem toolClose_preserves (before after : World) (document : DocId)
    (authority : ToolDelivery.CloseAuthority) (record : Segment)
    (unique : ClosureUnique before)
    (h : ToolDelivery.closeToolOutput before document authority record = .ok after) :
    ClosureUnique after := by
  rcases ToolDelivery.closeToolOutput_success_segment_effect before after document authority
      record h with same | ⟨request, sourceDoc, added, hopen, _, owned⟩
  · exact closureUnique_of_segments_eq unique same
  · unfold ClosureUnique
    rw [added]
    apply closureUnique_append_fresh before.segments record unique
    have coordinate : record.coordinate =
        CanonicalOutput.ToolDelivery.coordinate request sourceDoc := by
      simp [CanonicalOutput.ToolDelivery.ownedRecord] at owned
      exact owned.1.1.1
    simpa [coordinate] using hopen

private theorem evaluate_preserves (operation : Gate.Operation) (before after : World)
    (unique : ClosureUnique before) (h : Gate.evaluate operation before = .ok after) :
    ClosureUnique after := by
  cases operation with
  | renew generation deadline =>
      exact closureUnique_of_segments_eq unique
        (renew_preserves_canonical_output before after generation deadline
          (mapError_success Gate.Error.execution _ _ h)).1
  | append generation record =>
      exact appendRaw_preserves before after generation record unique
        (mapError_success Gate.Error.execution _ _ h)
  | closeAuxiliary generation closing =>
      exact auxiliaryClose_preserves before after generation closing unique
        (mapError_success Gate.Error.execution _ _ h)
  | retract generation record =>
      exact retract_preserves before after generation record unique
        (mapError_success Gate.Error.execution _ _ h)
  | accept generation closing message targets admissions =>
      exact accept_preserves before after generation closing message targets admissions unique
        (mapError_success Gate.Error.execution _ _ h)
  | authored generation closing message =>
      exact authored_preserves before after generation closing message unique
        (mapError_success Gate.Error.execution _ _ h)
  | headerOnly generation message admissions =>
      exact closureUnique_of_segments_eq unique
        (headerOnly_segments before after generation message admissions
          (mapError_success Gate.Error.execution _ _ h))
  | dispatch generation permit =>
      exact closureUnique_of_segments_eq unique
        (dispatch_segments before after generation permit
          (mapError_success Gate.Error.execution _ _ h))
  | admitSpawned generation admission =>
      exact closureUnique_of_segments_eq unique
        (spawned_segments before after generation admission
          (mapError_success Gate.Error.execution _ _ h))
  | toolControl generation document action =>
      exact closureUnique_of_segments_eq unique
        (toolControl_segments before after generation document action
          (mapError_success Gate.Error.execution _ _ h))
  | toolAppend document record =>
      exact toolAppend_preserves before after document record unique
        (mapError_success Gate.Error.delivery _ _ h)
  | toolClose document authority record =>
      exact toolClose_preserves before after document authority record unique
        (mapError_success Gate.Error.delivery _ _ h)
  | toolComplete document authority record message =>
      obtain ⟨closed, hclose, hdeliver⟩ := ToolDelivery.completeAndDeliver_success
        before after document authority record message
        (mapError_success Gate.Error.delivery _ _ h)
      exact closureUnique_of_segments_eq
        (toolClose_preserves before closed document authority record unique hclose)
        (ToolDelivery.publishToolDelivery_preserves_segments closed after document message hdeliver)
  | compact cursor =>
      unfold Gate.evaluate at h
      cases hc : Compaction.advanceCursor? before cursor with
      | none => simp [hc] at h
      | some post =>
          simp [hc] at h
          subst after
          exact closureUnique_of_segments_eq unique
            (Compaction.advanceCursor_preserves_publications before post cursor hc).2.2.1
  | closePartial generation item =>
      exact closePartial_preserves before after generation item unique
        (mapError_success Gate.Error.execution _ _ h)
  | recover expected fresh duration deadline items =>
      exact recovery_preserves before after expected fresh duration deadline items unique
        (mapError_success Gate.Error.execution _ _ h)
  | recoverTerminal expected fresh outcome selection items =>
      exact terminal_recovery_preserves before after expected fresh outcome selection items unique
        (mapError_success Gate.Error.execution _ _ h)
  | revoke expected fresh outcome selection =>
      exact closureUnique_of_segments_eq unique
        (revoke_corrupt_preserves_conflicting_output_facts before after expected fresh outcome
          selection (mapError_success Gate.Error.execution _ _ h)).1
  | terminalize generation outcome selection =>
      exact closureUnique_of_segments_eq unique
        (terminalize_segments before after generation outcome selection
          (mapError_success Gate.Error.execution _ _ h))
  | toolDeliver document message =>
      exact closureUnique_of_segments_eq unique
        (ToolDelivery.publishToolDelivery_preserves_segments before after document message
          (mapError_success Gate.Error.delivery _ _ h))
  | toolGoalDeliver document binding message =>
      exact closureUnique_of_segments_eq unique
        (ToolDelivery.goal_notification_preserves_segments before after document binding message
          (mapError_success Gate.Error.delivery _ _ h))
  | backgroundReceipt document closing message =>
      have hd := mapError_success Gate.Error.delivery _ _ h
      rcases ToolDelivery.background_receipt_segment_effect before after document closing message hd with
          same | ⟨added, _, hopen⟩
      · exact closureUnique_of_segments_eq unique same
      · unfold ClosureUnique
        rw [added]
        exact closureUnique_append_fresh before.segments closing unique
          (by simpa [sourceOpen] using hopen)

private theorem gateCommit_preserves (before after : World) (actor : Gate.Actor)
    (now : Time) (operation : Gate.Operation) (unique : ClosureUnique before)
    (h : Gate.commit before actor now operation = some after) : ClosureUnique after := by
  obtain ⟨execution, heval, rfl⟩ := Gate.commit_reads_current_world before after actor now operation h
  apply closureUnique_of_segments_eq
      (evaluate_preserves operation (Gate.atTime before now) execution (by
        simpa [ClosureUnique, Gate.atTime] using unique) heval)
  rfl

private theorem wake_preserves (before after : World) (actor : Gate.Actor) (now : Time)
    (document : DocId) (message : MessageEnvelope) (entry : SessionQueue.QueueEntry)
    (binding : WakeDocumentBinding) (unique : ClosureUnique before)
    (h : BackgroundGate.commit before actor now document message entry binding = some after) :
    ClosureUnique after := by
  unfold BackgroundGate.commit at h
  split at h <;> try contradiction
  cases hp : BackgroundContinuation.publishAndEnqueue?
      (Gate.atTime before now) document message entry binding before.queue with
  | none => simp [hp] at h
  | some result =>
      simp [hp] at h
      rcases h with ⟨⟨⟨⟨hbefore, hdocument⟩, hmessage⟩, hbinding⟩, rfl⟩
      apply closureUnique_of_segments_eq
        (closureUnique_of_segments_eq unique (by rfl : (Gate.atTime before now).segments = before.segments))
      have published : ToolDelivery.publishWakeNotification (Gate.atTime before now)
          document binding message = .ok result.execution := by
        simpa [BackgroundContinuation.publishNotification, hbefore, hdocument,
          hmessage, hbinding] using result.published
      simpa using ToolDelivery.wake_notification_preserves_segments
        (Gate.atTime before now) result.execution document binding message published

private theorem goal_preserves (goal : GoalAutomation.OperatorResume.Snapshot)
    (before : World) (actor : Gate.Actor) (now : Time)
    (request : GoalAutomation.OperatorResume.ClaimedRequest)
    (binding : GoalContinuation.Binding) (entry : SessionQueue.QueueEntry)
    (result : GoalContinuation.Result) (unique : ClosureUnique before)
    (h : GoalContinuation.publishGoalChild? goal before actor now request binding entry = some result) :
    ClosureUnique result.after := by
  unfold GoalContinuation.publishGoalChild? at h
  dsimp only at h
  repeat' first | contradiction | split at h
  all_goals rcases h with ⟨_, ⟨_, rfl⟩⟩
  all_goals simpa [ClosureUnique, GoalContinuation.releaseGate, Gate.atTime] using unique

/-- Closure uniqueness is preserved by the existing composed application
trace. The trace itself carries no invariant assumptions or extra guards. -/
theorem Trace.closureUnique {before after : World} (trace : Trace before after)
    (unique : ClosureUnique before) : ClosureUnique after := by
  induction trace with
  | refl => exact unique
  | provider actor now operation h =>
      have hg := provider_commit_is_actual_gate_commit _ _ actor now operation h
      have hu := gateCommit_preserves _ _ actor now
        (CompletionRetry.CanonicalGate.gateOperation operation) unique hg
      simpa [ClosureUnique] using hu
  | policy before after now operation claimed h =>
      rw [CompletionRetry.CanonicalGate.stepPolicy_preserves_gate now operation h]
      exact unique
  | gate before after actor now operation h =>
      unfold CompletionRetry.CanonicalGate.commitGate at h
      split at h <;> try contradiction
      exact gateCommit_preserves before after actor now operation.val unique h
  | acquire before after actor independent h =>
      rw [Gate.acquire_preserves_durable_world before after actor independent h]
      exact unique
  | scheduling before after actor event h =>
      rw [Gate.scheduling_preserves_durable_world before after actor event h]
      exact unique
  | activate actor now activation scope budget deadline h =>
      rename_i before after
      unfold SessionComposition.activate at h
      split at h <;> try contradiction
      cases hc : Handover.claimAndActivate before actor now activation with
      | none => simp [hc] at h
      | some world =>
          simp [hc] at h
          cases h
          exact closureUnique_of_segments_eq unique
            (by simpa using (congrArg (fun w : World => w.segments)
              (Handover.successful_claim_frame before world actor now activation hc)))
  | finish actor acknowledged h =>
      rename_i before after
      unfold SessionComposition.finish at h
      cases hc : Handover.finishAndAcknowledge before actor with
      | none => simp [hc] at h
      | some result =>
          simp [hc] at h
          rcases h with ⟨rfl, rfl⟩
          exact closureUnique_of_segments_eq unique
            (by simpa using (congrArg (fun w : World => w.segments)
              (Handover.successful_finish_frame before result actor hc)))
  | activateGoal actor now result published routes authenticated generation duration leaseDeadline scope budget deadline h =>
      rename_i before after
      unfold SessionComposition.activateGoal at h
      cases hc : Handover.claimAndActivate before actor now
          (GoalContinuation.childActivation result routes authenticated generation duration
            leaseDeadline) with
      | none => simp [hc] at h
      | some world =>
          simp [hc] at h
          cases h
          exact closureUnique_of_segments_eq unique
            (by simpa using (congrArg (fun w : World => w.segments)
              (Handover.successful_claim_frame before world actor now _ hc)))
  | beginProcessing before after actor now generation h =>
      exact closureUnique_of_segments_eq unique
        (by simpa using (congrArg (fun w : World => w.segments)
          (Handover.successful_begin_frame before after actor now generation h)))
  | wake before after actor now document message entry binding h =>
      exact wake_preserves before after actor now document message entry binding unique h
  | goal before goal actor now request binding entry result h =>
      exact goal_preserves goal before actor now request binding entry result unique h
  | restart before after actor now document binding closing wake notificationBinding h =>
      obtain ⟨result, _, hc, hn, he, rfl⟩ := RestartRecovery.successful_commit_effect
        before after actor now document binding closing wake notificationBinding h
      have timed : ClosureUnique (Gate.atTime before now) := by
        simpa [ClosureUnique, Gate.atTime] using unique
      have closed := toolClose_preserves (Gate.atTime before now) result.closed document
        (.native (RestartRecovery.closeAction result.cause)) closing timed hc
      have published := RestartRecovery.successful_enqueue_is_actual_notification
        result.closed document binding.notification wake notificationBinding before.queue
        result.continuation hn
      have execution := closureUnique_of_segments_eq closed
        (ToolDelivery.wake_notification_preserves_segments result.closed
          result.continuation.execution document notificationBinding binding.notification published)
      simpa only [he, ClosureUnique] using execution
  | trans left right ihleft ihrigh => exact ihrigh (ihleft unique)

end CanonicalOutput.Execution.SessionComposition
