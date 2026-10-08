import Proofs.Triggers.Durable

namespace EventDelivery.Durable
open Triggers.Durable

/-- DefraDB supplies a durable first-arrival sequence per receiving node and
collection, including replicated arrivals. Each physical document appears once;
this is receiving-node commit order, not global creation or wall-clock order. -/
structure Arrival where
  position : Nat
  identity : Identity
  deriving DecidableEq, Repr

/-- The native snapshot certifies a complete prefix through `head`, including
arrivals whose payload is hidden by ACP. The native ACP/read owner classifies
those as excluded; subscription notifications cannot certify completeness.
Source documents must remain readable until delivery. -/
structure Journal where
  head : Nat
  entries : List Arrival
  deriving DecidableEq, Repr

/-- Registration seeds the native head even for disabled consumers. Progress is
node-local, never replicated, and retained across restart and re-enable. Every
per-document consumer of a source owns one cursor: a task trigger, whose receipt
is its `TriggerFire`, and a callback binding, whose receipt is its
`CallbackInvocation` for the arrival. Each seeds in the configuration
transaction that registers it, not when an engine first observes the source. -/
structure Cursor where
  seeded : Bool := false
  after : Nat := 0
  deriving DecidableEq, Repr

def seed (cursor : Cursor) (head : Nat) : Cursor :=
  if cursor.seeded then cursor else { seeded := true, after := head }

def pending (cursor : Cursor) (sourceOrder : List Arrival) (enabled : Bool) : List Arrival :=
  if cursor.seeded && enabled then sourceOrder.filter fun entry => cursor.after < entry.position
  else []

inductive Disposition where
  | excluded
  | legacySerialBusy
  | receiptRequired
  deriving DecidableEq, Repr

/-- Eligibility combines filtering and owner visibility under one configuration
and ACP snapshot. `busy` is the legacy serial engine's observed decision, not a
caller-selected bypass. Latest-only admits each arrival; superseding execution
cannot substitute for an authoritative receipt. -/
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

/-- Every intervening arrival is checked: a later receipt cannot certify a gap. -/
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

/-- Per-document consumers sharing the cursor owner. A task trigger admits under
its concurrency mode. A callback binding admits like a parallel trigger: every
eligible arrival needs its receipt. It is enabled only while both its binding
and its callback are. -/
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

/-- A disabled consumer excludes nothing: only receipts certify its arrivals,
so unadmitted ones stay pending until it is enabled again. -/
def Consumer.eligible (consumer : Consumer) (eligible : Arrival → Bool) : Arrival → Bool :=
  if consumer.enabled then eligible else fun _ => true

/-- A callback binding's receipt is its event `CallbackInvocation`, keyed by
binding, collection and document. `version` is the source version the
invocation froze; the receipt omits it, so an edit after admission is never a
second arrival. -/
structure CallbackAdmission where
  identity : Identity
  version : String
  deriving DecidableEq, Repr

def CallbackAdmission.fire (admission : CallbackAdmission) : Fire :=
  { identity := admission.identity, session := "", serial := false,
    emitOutcome := false, goalBacked := false }

def admitCallback (state : State) (admission : CallbackAdmission) : State :=
  admit state admission.fire

/-- Only history at/before the initial registration seed is exempt. Legacy
serial's deliberate busy exclusion is outside the reliable-delivery guarantee. -/
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

/-- Both crash boundaries recover to the exact same single-admission state.
The admission owner owns the atomic receipt/request pair. -/
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

/-- Recovery after an edit finds the same receipt instead of admitting again. -/
theorem edited_source_not_readmitted (state : State) (admission : CallbackAdmission)
    (version : String) :
    admitCallback (admitCallback state admission) { admission with version } =
      admitCallback state admission :=
  admit_duplicate _ _ (admit_callback_receipts state admission)

/-- Disabling either the binding or its callback holds every unadmitted
arrival: checkpoints then pass only over receipts. -/
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

end EventDelivery.Durable
