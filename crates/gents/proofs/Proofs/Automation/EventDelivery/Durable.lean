import Proofs.Automation.Triggers.Durable

namespace EventDelivery.Durable
open Triggers.Durable

/-- This is receiving-node commit order, not global creation or wall-clock order. -/
structure Arrival where
  position : Nat
  identity : Identity
  deriving DecidableEq, Repr

/-- Source documents must remain readable until delivery. -/
structure Journal where
  head : Nat
  entries : List Arrival
  deriving DecidableEq, Repr

/-- Progress is node-local, never replicated, and retained across restart and
re-enable. A cursor seeds in the configuration transaction that registers its
consumer, not when an engine first observes the source. -/
structure Cursor where
  seeded : Bool := false
  after : Nat := 0
  deriving DecidableEq, Repr

def seed (cursor : Cursor) (head : Nat) : Cursor :=
  if cursor.seeded then cursor else { seeded := true, after := head }

def pending (cursor : Cursor) (sourceOrder : List Arrival) (enabled : Bool) : List Arrival :=
  if cursor.seeded && enabled then sourceOrder.filter fun entry => cursor.after < entry.position
  else []

/-- Under the trusted-writer ACP1 trial a callback result is published before
its parent grant and repair marker. An unreadable row is ambiguous, so the
consumer leaves its arrival pending. This can block later arrivals if access is
never granted or the row is deleted. -/
def callbackResultReady (visible linked : Bool) : Bool := visible && linked

def callbackResultCursor (before position : Nat) (visible linked : Bool) : Nat :=
  if callbackResultReady visible linked then position else before

theorem callback_result_waits_for_visible_repair (before position : Nat) :
    callbackResultCursor before position false false = before ∧
    callbackResultCursor before position false true = before ∧
    callbackResultCursor before position true false = before ∧
    callbackResultCursor before position true true = position := by
  simp [callbackResultCursor, callbackResultReady]

inductive Disposition where
  | excluded
  | legacySerialBusy
  | receiptRequired
  deriving DecidableEq, Repr

/-- `busy` is the legacy serial engine's observed decision, not a caller-selected
bypass. Superseding execution cannot substitute for an authoritative receipt. -/
def disposition (mode : ConcurrencyMode) (busy eligible : Bool) : Disposition :=
  if !eligible then .excluded
  else match mode with
    | .serial => if busy then .legacySerialBusy else .receiptRequired
    | .queuedSerial | .parallel | .latestOnly => .receiptRequired

def certified (committed : State) (mode : ConcurrencyMode) (busy : Bool)
    (eligible : Arrival → Bool) (entry : Arrival) : Bool :=
  match disposition mode busy (eligible entry) with
  | .excluded | .legacySerialBusy => true
  | .receiptRequired => admitted committed entry.identity

def prefixCertified (cursor : Cursor) (committed : State) (journal : Journal)
    (through : Nat) (mode : ConcurrencyMode) (busy : Bool)
    (eligible : Arrival → Bool) : Bool :=
  journal.entries.all fun entry =>
    if cursor.after < entry.position && entry.position ≤ through then
      certified committed mode busy eligible entry
    else true

def checkpointAccepted (cursor : Cursor) (committed : State) (journal : Journal)
    (through : Nat) (mode : ConcurrencyMode) (busy : Bool)
    (eligible : Arrival → Bool) (checkpointCommitted : Bool) : Bool :=
  cursor.seeded && checkpointCommitted && decide (cursor.after ≤ through) &&
    decide (through ≤ journal.head) &&
    prefixCertified cursor committed journal through mode busy eligible

def acknowledge (cursor : Cursor) (committed : State) (journal : Journal)
    (through : Nat) (mode : ConcurrencyMode) (busy : Bool)
    (eligible : Arrival → Bool) (checkpointCommitted : Bool) : Cursor :=
  if checkpointAccepted cursor committed journal through mode busy eligible checkpointCommitted then
    { cursor with after := through }
  else cursor

inductive Consumer where
  | trigger (mode : ConcurrencyMode) (enabled : Bool)
  | callbackBinding (bindingEnabled callbackEnabled : Bool)
  deriving DecidableEq, Repr

def Consumer.enabled : Consumer → Bool
  | .trigger _ enabled => enabled
  | .callbackBinding binding callback => binding && callback

def Consumer.mode : Consumer → ConcurrencyMode
  | .trigger mode _ => mode
  | .callbackBinding _ _ => .parallel

def Consumer.eligible (consumer : Consumer) (eligible : Arrival → Bool) : Arrival → Bool :=
  if consumer.enabled then eligible else fun _ => true

/-- A callback binding's receipt is its event `CallbackInvocation`, keyed by
binding, collection and document; an edit after admission is never a second
arrival. -/
structure CallbackAdmission where
  identity : Identity
  version : String
  deriving DecidableEq, Repr

def CallbackAdmission.fire (admission : CallbackAdmission) : Fire :=
  { identity := admission.identity, session := "", serial := false,
    emitOutcome := false, goalBacked := false }

def admitCallback (state : State) (admission : CallbackAdmission) : State :=
  admit state admission.fire

def DeliveredPrefix (initialSeed : Nat) (cursor : Cursor) (committed : State)
    (journal : Journal) (eligible : Arrival → Bool) : Prop :=
  ∀ entry ∈ journal.entries, initialSeed < entry.position →
    entry.position ≤ cursor.after → eligible entry = true →
    admitted committed entry.identity = true

def Reliable (mode : ConcurrencyMode) : Prop := mode ≠ .serial

theorem seed_once (cursor : Cursor) (first later : Nat) :
    seed (seed cursor first) later = seed cursor first := by
  simp only [seed]
  split <;> simp_all

theorem initial_seed_delivered (head : Nat) (committed : State) (journal : Journal)
    (eligible : Arrival → Bool) :
    DeliveredPrefix head (seed {} head) committed journal eligible := by
  intro entry _ hafter hbefore _
  simp [seed] at hbefore
  exact False.elim ((Nat.not_lt_of_ge hbefore) hafter)

theorem disabled_does_not_deliver (cursor : Cursor) (source : List Arrival) :
    pending cursor source false = [] := by simp [pending]

theorem restart_preserves_pending (cursor : Cursor) (source : List Arrival)
    (head : Nat) (h : cursor.seeded = true) :
    pending (seed cursor head) source true = pending cursor source true := by
  simp [seed, h]

theorem reenable_after_restart_delivers_exact_pending (cursor : Cursor)
    (source : List Arrival) (head : Nat) (h : cursor.seeded = true) :
    pending (seed cursor head) source false = [] ∧
      pending (seed cursor head) source true =
        source.filter (fun entry => cursor.after < entry.position) := by
  simp [seed, pending, h]

theorem pending_is_subsequence (cursor : Cursor) (source : List Arrival) (enabled : Bool) :
    (pending cursor source enabled).Sublist source := by
  simp only [pending]
  split
  · exact List.filter_sublist source
  · exact List.nil_sublist _

theorem reliable_certified_is_admitted (committed : State) (mode : ConcurrencyMode)
    (busy : Bool) (eligible : Arrival → Bool) (entry : Arrival)
    (hmode : Reliable mode) (heligible : eligible entry = true) :
    certified committed mode busy eligible entry = admitted committed entry.identity := by
  cases mode <;> simp_all [Reliable, certified, disposition]

theorem prefix_member_certified (cursor : Cursor) (committed : State) (journal : Journal)
    (through : Nat) (mode : ConcurrencyMode) (busy : Bool) (eligible : Arrival → Bool)
    (entry : Arrival) (hprefix : prefixCertified cursor committed journal through mode busy eligible = true)
    (hmem : entry ∈ journal.entries) (hafter : cursor.after < entry.position)
    (hbefore : entry.position ≤ through) :
    certified committed mode busy eligible entry = true := by
  have h := (List.all_eq_true.mp hprefix) entry hmem
  simpa [hafter, hbefore] using h

theorem acknowledge_preserves_delivered_prefix
    (initialSeed : Nat) (cursor : Cursor) (committed : State) (journal : Journal)
    (through : Nat) (mode : ConcurrencyMode) (busy : Bool) (eligible : Arrival → Bool)
    (commit : Bool) (hmode : Reliable mode)
    (hprior : DeliveredPrefix initialSeed cursor committed journal eligible) :
    DeliveredPrefix initialSeed
      (acknowledge cursor committed journal through mode busy eligible commit)
      committed journal eligible := by
  unfold acknowledge checkpointAccepted
  split
  · rename_i hguard
    have hprefix : prefixCertified cursor committed journal through mode busy eligible = true :=
      (Bool.and_eq_true_iff.mp hguard).2
    intro entry hmem hseed hbefore heligible
    by_cases hold : entry.position ≤ cursor.after
    · exact hprior entry hmem hseed hold heligible
    · have hafter : cursor.after < entry.position := Nat.lt_of_not_ge hold
      have hcert := prefix_member_certified cursor committed journal through mode busy eligible
        entry hprefix hmem hafter hbefore
      rwa [reliable_certified_is_admitted committed mode busy eligible entry hmode heligible] at hcert
  · exact hprior

theorem admission_preserves_delivered_prefix (initialSeed : Nat) (cursor : Cursor)
    (committed : State) (journal : Journal) (eligible : Arrival → Bool) (fire : Fire)
    (hprior : DeliveredPrefix initialSeed cursor committed journal eligible) :
    DeliveredPrefix initialSeed cursor (admit committed fire) journal eligible := by
  intro entry hmem hseed hbefore heligible
  have hreceipt := hprior entry hmem hseed hbefore heligible
  simp only [admit]
  split <;> simp_all [admitted]

theorem acknowledge_never_regresses (cursor : Cursor) (committed : State)
    (journal : Journal) (through : Nat) (mode : ConcurrencyMode) (busy : Bool)
    (eligible : Arrival → Bool) (commit : Bool) :
    cursor.after ≤ (acknowledge cursor committed journal through mode busy eligible commit).after := by
  unfold acknowledge checkpointAccepted
  split
  · rename_i hguard
    have hbounded := (Bool.and_eq_true_iff.mp
      (Bool.and_eq_true_iff.mp (Bool.and_eq_true_iff.mp hguard).1).1).2
    simpa using hbounded
  · exact Nat.le_refl _

theorem failed_checkpoint_preserves_cursor (cursor : Cursor) (committed : State)
    (journal : Journal) (through : Nat) (mode : ConcurrencyMode) (busy : Bool)
    (eligible : Arrival → Bool) :
    acknowledge cursor committed journal through mode busy eligible false = cursor := by
  simp [acknowledge, checkpointAccepted]

theorem unadmitted_prefix_match_cannot_advance (cursor : Cursor) (committed : State)
    (journal : Journal) (through : Nat) (mode : ConcurrencyMode) (busy : Bool)
    (eligible : Arrival → Bool) (commit : Bool) (entry : Arrival)
    (hmode : Reliable mode) (hmem : entry ∈ journal.entries)
    (hafter : cursor.after < entry.position) (hbefore : entry.position ≤ through)
    (heligible : eligible entry = true) (hmissing : admitted committed entry.identity = false) :
    acknowledge cursor committed journal through mode busy eligible commit = cursor := by
  unfold acknowledge checkpointAccepted
  split
  · rename_i hguard
    have hprefix := (Bool.and_eq_true_iff.mp hguard).2
    have hcert := prefix_member_certified cursor committed journal through mode busy eligible
      entry hprefix hmem hafter hbefore
    rw [reliable_certified_is_admitted committed mode busy eligible entry hmode heligible,
      hmissing] at hcert
    contradiction
  · rfl

theorem legacy_serial_busy_is_explicit_exclusion :
    disposition .serial true true = .legacySerialBusy := rfl

theorem latest_only_still_requires_receipt (busy : Bool) :
    disposition .latestOnly busy true = .receiptRequired := rfl

theorem replay_after_admission_crash (committed : State) (fire : Fire) (didCommit : Bool) :
    admitTransaction (admitTransaction committed fire didCommit) fire true =
      admitTransaction committed fire true := by
  cases didCommit
  · rfl
  · exact admission_crash_after_commit committed fire

theorem callback_binding_is_reliable (binding callback : Bool) :
    Reliable (Consumer.callbackBinding binding callback).mode := by
  simp [Reliable, Consumer.mode]

theorem admit_callback_receipts (state : State) (admission : CallbackAdmission) :
    admitted (admitCallback state admission) admission.identity = true := by
  unfold admitCallback admit
  by_cases h : admission.identity ∈ state.receipts
  · simp_all [CallbackAdmission.fire, admitted]
  · simp [h, CallbackAdmission.fire, Triggers.outcomeSourceAllowed, admitted]

theorem edited_source_not_readmitted (state : State) (admission : CallbackAdmission)
    (version : String) :
    admitCallback (admitCallback state admission) { admission with version } =
      admitCallback state admission :=
  admit_duplicate _ _ (admit_callback_receipts state admission)

theorem disabled_consumer_checkpoints_only_receipts (consumer : Consumer)
    (hdisabled : consumer.enabled = false) (initialSeed : Nat) (cursor : Cursor)
    (committed : State) (journal : Journal) (through : Nat) (busy : Bool)
    (eligible : Arrival → Bool) (commit : Bool) (hmode : Reliable consumer.mode)
    (hprior : DeliveredPrefix initialSeed cursor committed journal fun _ => true) :
    DeliveredPrefix initialSeed
      (acknowledge cursor committed journal through consumer.mode busy
        (consumer.eligible eligible) commit)
      committed journal (fun _ => true) := by
  have h : consumer.eligible eligible = fun _ => true := by
    simp [Consumer.eligible, hdisabled]
  rw [h]
  exact acknowledge_preserves_delivered_prefix initialSeed cursor committed journal
    through consumer.mode busy _ commit hmode hprior

/-- One arrival on one callback binding across engine crashes. `admit`: an
engine admits the arrival with a binding it read earlier, then stops before its
checkpoint; admission rereads the binding and refuses once it is disabled.
`edit`: the source document gains a version. `disable`: the binding is
disabled. `restart`: a new engine delivers the pending arrivals of its enabled
bindings, admitting each and checkpointing the admitted prefix; a disabled
binding is not delivered and its cursor stays. -/
inductive CallbackStep where
  | admit | edit | disable | restart
  deriving DecidableEq, Repr

structure CallbackWorld where
  enabled : Bool := true
  version : Nat := 0
  committed : State := {}
  cursor : Cursor := { seeded := true, after := 0 }
  deriving DecidableEq, Repr

def callbackArrival : Arrival := ⟨1, ⟨"owner", "binding", "BacklogWork", "pending"⟩⟩

def callbackJournal : Journal := ⟨1, [callbackArrival]⟩

def CallbackWorld.admission (w : CallbackWorld) (entry : Arrival) : CallbackAdmission :=
  ⟨entry.identity, toString w.version⟩

def CallbackWorld.consumer (w : CallbackWorld) : Consumer := .callbackBinding w.enabled true

def callbackAdmit (w : CallbackWorld) : CallbackWorld × Bool :=
  if w.enabled then
    ({ w with committed := admitCallback w.committed (w.admission callbackArrival) }, true)
  else (w, false)

def callbackRestart (w : CallbackWorld) : CallbackWorld :=
  if !w.enabled then w
  else
    let committed := (pending w.cursor callbackJournal.entries w.enabled).foldl
      (fun state entry => admitCallback state (w.admission entry)) w.committed
    let cursor := acknowledge w.cursor committed callbackJournal callbackJournal.head
      w.consumer.mode false (w.consumer.eligible fun _ => true) true
    { w with committed, cursor }

def callbackStep (w : CallbackWorld) : CallbackStep → CallbackWorld × Option Bool
  | .admit => let (next, published) := callbackAdmit w; (next, some published)
  | .edit => ({ w with version := w.version + 1 }, none)
  | .disable => ({ w with enabled := false }, none)
  | .restart => (callbackRestart w, none)

/-- `admitted` is the admission result of an `admit` step. `invocations` counts
the binding's admitted invocations; `checkpointed` says the cursor passed the
arrival. -/
structure CallbackObservation where
  admitted : Option Bool
  invocations : Nat
  checkpointed : Bool
  deriving DecidableEq, Repr

def callbackRun : CallbackWorld → List CallbackStep → List CallbackObservation
  | _, [] => []
  | w, step :: rest =>
    let (next, published) := callbackStep w step
    { admitted := published, invocations := next.committed.receipts.length,
      checkpointed := decide (callbackArrival.position ≤ next.cursor.after) } ::
      callbackRun next rest

theorem edited_document_admitted_before_a_crash_is_not_readmitted :
    callbackRun {} [.admit, .edit, .restart] =
      [⟨some true, 1, false⟩, ⟨none, 1, false⟩, ⟨none, 1, true⟩] := by
  decide

theorem admission_refuses_a_binding_disabled_after_its_snapshot (w : CallbackWorld)
    (h : w.enabled = false) : callbackAdmit w = (w, false) := by
  simp [callbackAdmit, h]

/-- One arrival on one queued-serial event trigger whose source has no filter,
under persisted enable and disable. `admit` writes the trigger's delivery: an
existing receipt is returned as a duplicate even after a disable; otherwise the
persisted disable refuses it. `checkpoint` asks to move the cursor past the
arrival, as exclusion and acknowledgement both do; with no filter the arrival
is eligible, so only its committed receipt certifies it. -/
inductive TriggerStep where
  | disable | enable | admit | checkpoint
  deriving DecidableEq, Repr

inductive TriggerAdmission where
  | refused | admitted | duplicate
  deriving DecidableEq, Repr

structure TriggerWorld where
  enabled : Bool := true
  committed : State := {}
  cursor : Cursor := { seeded := true, after := 0 }
  deriving DecidableEq, Repr

def triggerArrival : Arrival := ⟨1, ⟨"owner", "trigger", "WorkA", "held"⟩⟩

def triggerJournal : Journal := ⟨1, [triggerArrival]⟩

def triggerFire : Fire :=
  { identity := triggerArrival.identity, session := "trigger-session", serial := true,
    emitOutcome := false, goalBacked := false }

def triggerAdmit (w : TriggerWorld) : TriggerWorld × TriggerAdmission :=
  if admitted w.committed triggerFire.identity then (w, .duplicate)
  else if !w.enabled then (w, .refused)
  else ({ w with committed := admit w.committed triggerFire }, .admitted)

def triggerCheckpoint (w : TriggerWorld) : TriggerWorld × Bool :=
  let consumer := Consumer.trigger .queuedSerial w.enabled
  let accepted := checkpointAccepted w.cursor w.committed triggerJournal triggerArrival.position
    consumer.mode false (consumer.eligible fun _ => true) true
  let cursor := acknowledge w.cursor w.committed triggerJournal triggerArrival.position
    consumer.mode false (consumer.eligible fun _ => true) true
  ({ w with cursor }, accepted)

/-- `admission` and `checkpointed` are the results of `admit` and `checkpoint`
steps. `fires` and `requests` count the committed receipts and their requests;
`cursor` is the consumer's checkpoint. -/
structure TriggerObservation where
  admission : Option TriggerAdmission
  checkpointed : Option Bool
  fires : Nat
  requests : Nat
  cursor : Nat
  deriving DecidableEq, Repr

def triggerStep (w : TriggerWorld) :
    TriggerStep → TriggerWorld × Option TriggerAdmission × Option Bool
  | .disable => ({ w with enabled := false }, none, none)
  | .enable => ({ w with enabled := true }, none, none)
  | .admit => let (next, result) := triggerAdmit w; (next, some result, none)
  | .checkpoint => let (next, accepted) := triggerCheckpoint w; (next, none, some accepted)

def triggerRun : TriggerWorld → List TriggerStep → List TriggerObservation
  | _, [] => []
  | w, step :: rest =>
    let (next, admission, checkpointed) := triggerStep w step
    { admission, checkpointed, fires := next.committed.receipts.length,
      requests := next.committed.requests.length, cursor := next.cursor.after } ::
      triggerRun next rest

theorem persisted_disable_blocks_stale_admission_and_exclusion_but_not_committed_ack :
    triggerRun {} [.disable, .admit, .checkpoint, .enable, .admit, .disable, .admit, .checkpoint] =
      [ ⟨none, none, 0, 0, 0⟩, ⟨some .refused, none, 0, 0, 0⟩, ⟨none, some false, 0, 0, 0⟩
      , ⟨none, none, 0, 0, 0⟩, ⟨some .admitted, none, 1, 1, 0⟩, ⟨none, none, 1, 1, 0⟩
      , ⟨some .duplicate, none, 1, 1, 0⟩, ⟨none, some true, 1, 1, 1⟩ ] := by
  decide

theorem disabled_trigger_refuses_a_new_admission (w : TriggerWorld) (h : w.enabled = false)
    (fresh : admitted w.committed triggerFire.identity = false) :
    triggerAdmit w = (w, .refused) := by
  simp [triggerAdmit, h, fresh]

theorem committed_receipt_is_a_duplicate (w : TriggerWorld)
    (h : admitted w.committed triggerFire.identity = true) :
    triggerAdmit w = (w, .duplicate) := by
  simp [triggerAdmit, h]

end EventDelivery.Durable
