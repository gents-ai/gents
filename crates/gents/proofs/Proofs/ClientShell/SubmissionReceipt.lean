import Lean

namespace ClientSubmissionReceipt
open Lean

/-- The caller retains the canonical request identity before transmission. The
existing local journal's unique request index is the preparation reservation. -/
structure Caller where
  requester : Nat
  request : Nat
  deriving DecidableEq, Repr

/-- The envelope denotes the exact authored input, captures, requested session
and configuration. Equality is structural binding, never matching display text.
Goal assignment and the generated session are frozen in the prepared request. -/
structure Intent where
  caller : Caller
  node : Nat
  envelope : Nat
  deriving DecidableEq, Repr

structure Prepared where
  session : Nat
  request : Nat
  signedBytes : Nat
  deriving DecidableEq, Repr

inductive Outcome where
  | unresolved
  | accepted
  | notDelivered
  deriving DecidableEq, Repr

/-- Cleanup reports retain the original delivery observation and request
identity. A failed local cleanup cannot convert a confirmed delivery into an
unresolved or rejected send, and must not authorize a new request identity. -/
structure Completion where
  prepared : Prepared
  outcome : Outcome
  cleanupFailed : Bool
  deriving DecidableEq, Repr

def completion (prepared : Prepared) (outcome : Outcome) (cleanupSucceeded : Bool) : Completion :=
  ⟨prepared, outcome, outcome != .unresolved && !cleanupSucceeded⟩

theorem cleanup_preserves_delivery (prepared : Prepared) (outcome : Outcome)
    (cleanupSucceeded : Bool) :
    (completion prepared outcome cleanupSucceeded).prepared = prepared ∧
      (completion prepared outcome cleanupSucceeded).outcome = outcome := by
  simp [completion]

theorem unknown_never_claims_cleanup_failure (prepared : Prepared) (cleanupSucceeded : Bool) :
    (completion prepared .unresolved cleanupSucceeded).cleanupFailed = false := by
  simp [completion]

/-- A definite native rejection remains attached to the original request
identity. An ambiguous observation remains unresolved; an absent filtered row
alone is not a rejection. -/
structure RejectionObservation where
  prepared : Prepared
  outcome : Outcome
  nativeError : Option Nat
  deriving DecidableEq, Repr

def rejectionObservation (prepared : Prepared) (outcome : Outcome)
    (nativeError : Option Nat) : RejectionObservation :=
  ⟨prepared, outcome, if outcome == .notDelivered then nativeError else none⟩

theorem rejection_preserves_request (prepared : Prepared) (outcome : Outcome) (nativeError : Option Nat) :
    (rejectionObservation prepared outcome nativeError).prepared = prepared := by rfl

theorem rejection_preserves_native_error (prepared : Prepared) (nativeError : Option Nat) :
    (rejectionObservation prepared .notDelivered nativeError).nativeError = nativeError := by rfl

theorem unresolved_does_not_report_rejection (prepared : Prepared) (nativeError : Option Nat) :
    (rejectionObservation prepared .unresolved nativeError).nativeError = none := by rfl

def rejectionCasesJson : String :=
  let prepared : Prepared := ⟨20, 30, 300⟩
  (toJson (([Outcome.unresolved, .accepted, .notDelivered]).flatMap fun outcome =>
    [none, some 71].map fun nativeError =>
      let observed := rejectionObservation prepared outcome nativeError
      Json.mkObj [("request", toJson prepared.request),
        ("outcome", toJson (match outcome with
          | .unresolved => "unresolved" | .accepted => "accepted" | .notDelivered => "notDelivered")),
        ("native_error", toJson nativeError), ("expected_request", toJson observed.prepared.request),
        ("expected_native_error", toJson observed.nativeError)])).compress

/-- An explicit native error while reading a receipt is evidence about that
read, not proof of non-delivery. The caller retains both the original request
identity and the error while the outcome stays unresolved. -/
structure UncertainObservation where
  prepared : Prepared
  outcome : Outcome
  accessError : Option Nat
  deriving DecidableEq, Repr

def uncertainObservation (prepared : Prepared) (accessError : Option Nat) : UncertainObservation :=
  ⟨prepared, .unresolved, accessError⟩

theorem uncertain_access_preserves_error_and_identity (prepared : Prepared) (accessError : Option Nat) :
    (uncertainObservation prepared accessError).prepared = prepared ∧
      (uncertainObservation prepared accessError).accessError = accessError ∧
      (uncertainObservation prepared accessError).outcome = .unresolved := by
  simp [uncertainObservation]

def uncertainErrorCasesJson : String :=
  let prepared : Prepared := ⟨20, 30, 300⟩
  (toJson ([none, some 71].map fun error =>
    let observed := uncertainObservation prepared error
    Json.mkObj [("request", toJson prepared.request), ("access_error", toJson error),
      ("expected_request", toJson observed.prepared.request),
      ("expected_access_error", toJson observed.accessError),
      ("expected_outcome", toJson "unresolved")])).compress

def cleanupCasesJson : String :=
  let prepared : Prepared := ⟨20, 30, 300⟩
  let preparedJson := fun (value : Prepared) => Json.mkObj
    [("session", toJson value.session), ("request", toJson value.request),
     ("signedBytes", toJson value.signedBytes)]
  let outcomeJson := fun (outcome : Outcome) => toJson <| match outcome with
    | .unresolved => "unresolved" | .accepted => "accepted" | .notDelivered => "notDelivered"
  (toJson (([Outcome.unresolved, .accepted, .notDelivered]).flatMap fun outcome =>
    [true, false].map fun success =>
      let observed := completion prepared outcome success
      Json.mkObj [("prepared", preparedJson prepared), ("outcome", outcomeJson outcome),
        ("cleanup_succeeded", toJson success), ("expected_prepared", preparedJson observed.prepared),
        ("expected_outcome", outcomeJson observed.outcome), ("cleanup_failed", toJson observed.cleanupFailed)])).compress

structure Record where
  intent : Intent
  prepared : Prepared
  outcome : Outcome := .unresolved
  retainedBytes : Bool := true
  acknowledged : Bool := false
  deriving DecidableEq, Repr

abbrev Journal := List Record

inductive Result where
  | refused
  | reconcile (prepared : Prepared)
  | settled (prepared : Prepared) (outcome : Outcome)
  deriving DecidableEq, Repr

def lookup (journal : Journal) (caller : Caller) : Option Record :=
  journal.find? fun record => record.intent.caller == caller

def bound (record : Record) : Bool :=
  record.prepared.request == record.intent.caller.request &&
    record.retainedBytes != record.acknowledged

def result (record : Record) : Result :=
  if !bound record then .refused else
    match record.outcome with
    | .unresolved => if record.retainedBytes then .reconcile record.prepared else .refused
    | outcome => .settled record.prepared outcome

/-- `durable` is the existing journal owner's successful durable preparation,
not an RPC reply or database admission receipt. A failed or ambiguous local
write cannot authorize an owner effect. Retrying an existing caller operation
returns its original signed preparation even if a new proposal was allocated.
The lookup and reservation are one atomic journal decision; an independently
racing read followed by insertion does not implement this transition. -/
def prepare (journal : Journal) (intent : Intent) (proposal : Prepared)
    (durable : Bool) : Journal × Result :=
  if (journal.filter fun record => record.intent.caller == intent.caller).length > 1 then
    (journal, .refused)
  else match lookup journal intent.caller with
  | some record =>
    if record.intent == intent then (journal, result record) else (journal, .refused)
  | none =>
    if durable && proposal.request == intent.caller.request &&
        !(journal.any fun record => record.prepared.request == proposal.request) then
      let record : Record := ⟨intent, proposal, .unresolved, true, false⟩
      (journal ++ [record], result record)
    else (journal, .refused)

/-- This observation is supplied only by the existing signed owner-receipt
reconciler. Transport absence and elapsed time cannot establish non-delivery. -/
def observe (journal : Journal) (caller : Caller) (prepared : Prepared)
    (outcome : Outcome) : Journal :=
  if (journal.filter fun record => record.intent.caller == caller).length != 1 then journal
  else journal.map fun record =>
    if bound record && record.intent.caller == caller && record.prepared == prepared &&
        record.outcome == .unresolved && outcome != .unresolved then
      { record with outcome := outcome }
    else record

/-- Acknowledgement may retire replay bytes after a confirmed outcome. The
caller-to-request binding survives it: a stale restored view must receive the
same result rather than create a second signed request after an acknowledged
reply. Unresolved work cannot be acknowledged away. -/
def acknowledge (journal : Journal) (caller : Caller) (prepared : Prepared) : Journal :=
  if (journal.filter fun record => record.intent.caller == caller).length != 1 then journal
  else journal.map fun record =>
    if bound record && record.intent.caller == caller && record.prepared == prepared &&
        record.outcome != .unresolved then
      { record with retainedBytes := false, acknowledged := true }
    else record

theorem prepare_existing_binding (journal : Journal) (intent : Intent)
    (record : Record) (proposal : Prepared) (durable : Bool)
    (found : lookup journal intent.caller = some record)
    (same : record.intent = intent)
    (unique : (journal.filter fun row => row.intent.caller == intent.caller).length ≤ 1) :
    prepare journal intent proposal durable = (journal, result record) := by
  simp [prepare, found, same, Nat.not_lt.mpr unique]

theorem prepare_scope_mismatch_refused (journal : Journal) (intent : Intent)
    (record : Record) (proposal : Prepared) (durable : Bool)
    (found : lookup journal intent.caller = some record)
    (different : record.intent ≠ intent) :
    prepare journal intent proposal durable = (journal, .refused) := by
  simp only [prepare]
  split <;> simp [found, different]

theorem failed_preparation_cannot_authorize (journal : Journal) (intent : Intent)
    (proposal : Prepared) (absent : lookup journal intent.caller = none) :
    prepare journal intent proposal false = (journal, .refused) := by
  simp only [prepare]
  split <;> simp [absent]

theorem acknowledgement_keeps_bindings (journal : Journal) (caller : Caller)
    (prepared : Prepared) :
    (acknowledge journal caller prepared).map (fun record => (record.intent, record.prepared)) =
      journal.map (fun record => (record.intent, record.prepared)) := by
  unfold acknowledge
  split
  · rfl
  · simp only [List.map_map]
    congr 1
    funext record
    dsimp only [Function.comp_def]
    split <;> rfl

theorem unknown_cannot_be_retired (intent : Intent) (prepared : Prepared) :
    acknowledge [⟨intent, prepared, .unresolved, true, false⟩] intent.caller prepared =
      [⟨intent, prepared, .unresolved, true, false⟩] := by
  simp [acknowledge]

theorem acknowledged_outcome_never_replays (intent : Intent) (prepared : Prepared)
    (outcome : Outcome) (confirmed : outcome ≠ .unresolved)
    (identity : prepared.request = intent.caller.request) :
    result ⟨intent, prepared, outcome, false, true⟩ = .settled prepared outcome := by
  cases outcome <;> simp_all [result, bound]

/-! ### One journal per client key

A desktop's journal is its embedded local store, whose node identity is the
client key (`require_client_signer`): only that key's records are written to
it. The User key is shared by the desktop and the CLI, but they do not share a
journal, and two keys never do. A family of journals is represented by one
list partitioned by `caller.requester`; `scope journal key` is the journal of
`key`. The caller key space is `(requester, request)`: the same request number
under a different signer is a distinct caller whose record lives in that
signer's own journal.

External premise: request identity is not unique across signers at the owner
either. `AgentRequest.request_id` carries no unique index, so both owner reads
a caller reconciles through are scoped by its own key: with retained bytes,
`reconcile_client_request` filters node, session and requester and requires
the retained admission signer and signature; without them,
`reconcile_unretained_submission` filters node, session, requester and
admission signer, all the caller's client key. Another signer's request with
the same identity is therefore never evidence for this caller's send. -/

def scope (journal : Journal) (key : Nat) : Journal :=
  journal.filter fun record => record.intent.caller.requester == key

/-- The journal of client `key` decides a preparation; a proposal signed by
another key is refused before any write. A reservation appends to that key's
journal only, so the family stays partitioned. -/
def prepareAt (key : Nat) (journal : Journal) (intent : Intent) (proposal : Prepared)
    (durable : Bool) : Journal × Result :=
  if intent.caller.requester != key then (journal, .refused) else
    let mine := scope journal key
    let (next, output) := prepare mine intent proposal durable
    (journal ++ next.drop mine.length, output)

/-- Observations and acknowledgements reach only the journal of the caller's
own key. -/
def observeAt (key : Nat) (journal : Journal) (caller : Caller) (prepared : Prepared)
    (outcome : Outcome) : Journal :=
  if caller.requester != key then journal else observe journal caller prepared outcome

def acknowledgeAt (key : Nat) (journal : Journal) (caller : Caller) (prepared : Prepared) :
    Journal :=
  if caller.requester != key then journal else acknowledge journal caller prepared

theorem prepare_shape (journal : Journal) (intent : Intent) (proposal : Prepared)
    (durable : Bool) :
    (prepare journal intent proposal durable).1 = journal ∨
      (prepare journal intent proposal durable).1 =
        journal ++ [⟨intent, proposal, .unresolved, true, false⟩] := by
  unfold prepare
  split
  · exact Or.inl rfl
  · split
    · split <;> exact Or.inl rfl
    · split
      · exact Or.inr rfl
      · exact Or.inl rfl

theorem foreign_signer_cannot_write_a_client_journal (key : Nat) (journal : Journal)
    (intent : Intent) (proposal : Prepared) (durable : Bool)
    (foreign : intent.caller.requester ≠ key) :
    prepareAt key journal intent proposal durable = (journal, .refused) := by
  simp [prepareAt, foreign]

/-- A reservation writes only its own key's journal. -/
theorem prepareAt_writes_only_its_signer (key : Nat) (journal : Journal) (intent : Intent)
    (proposal : Prepared) (durable : Bool) (record : Record)
    (written : record ∈ (prepareAt key journal intent proposal durable).1) :
    record ∈ journal ∨ record.intent.caller.requester = key := by
  unfold prepareAt at written
  by_cases own : intent.caller.requester = key
  · simp only [own, bne_self_eq_false, Bool.false_eq_true, ↓reduceIte] at written
    rcases prepare_shape (scope journal key) intent proposal durable with same | grown
    · rw [same] at written
      simp at written
      exact Or.inl written
    · rw [grown] at written
      simp only [List.drop_left, List.mem_append, List.mem_singleton] at written
      rcases written with old | fresh
      · exact Or.inl old
      · exact Or.inr (by rw [fresh]; exact own)
  · simp only [bne_iff_ne, ne_eq, own, not_false_eq_true, ↓reduceIte] at written
    exact Or.inl written

/-- A different signer is a distinct caller: records of other keys never
decide its preparation, so it reserves in its own journal. -/
theorem different_signer_is_distinct (journal : Journal) (intent : Intent)
    (proposal : Prepared)
    (others : ∀ record ∈ journal, record.intent.caller.requester ≠ intent.caller.requester)
    (identity : proposal.request = intent.caller.request) :
    prepareAt intent.caller.requester journal intent proposal true =
      (journal ++ [⟨intent, proposal, .unresolved, true, false⟩], .reconcile proposal) := by
  have empty : scope journal intent.caller.requester = [] := by
    unfold scope
    rw [List.filter_eq_nil_iff]
    intro record mem
    simpa using others record mem
  simp [prepareAt, empty, prepare, lookup, result, bound, identity]

theorem prepare_keeps_reserved_request (journal : Journal) (intent : Intent)
    (proposal : Prepared) (durable : Bool)
    (reserved : journal.any (fun record => record.prepared.request == proposal.request) = true) :
    (prepare journal intent proposal durable).1 = journal := by
  unfold prepare
  split
  · rfl
  · split
    · split <;> rfl
    · rw [if_neg]
      simp [reserved]

/-- Within one client journal a request identity binds one caller operation:
no other runtime, envelope or proposal can reserve it again. -/
theorem request_identity_is_unique_within_a_client_journal (key : Nat) (journal : Journal)
    (intent : Intent) (proposal : Prepared) (durable : Bool)
    (reserved : (scope journal key).any
      (fun record => record.prepared.request == proposal.request) = true) :
    (prepareAt key journal intent proposal durable).1 = journal := by
  unfold prepareAt
  split
  · rfl
  · simp only [prepare_keeps_reserved_request _ _ _ _ reserved, List.drop_length,
      List.append_nil]

/-! ### Unretained unresolved records

A record this journal wrote keeps its signed bytes while its outcome is
unresolved: `prepare` reserves with bytes, `observe` only settles, and
`acknowledge` retires bytes only after a settled outcome. The `.refused`
branch of `result` for an unresolved record without bytes is therefore never
reached by a journal history; it covers only rows the journal did not write
(an older uncorrelated send, or a lost or reset journal), which the native
caller reconciles through the owner's visible request alone. -/

def retainsUnresolved (journal : Journal) : Prop :=
  ∀ record ∈ journal, record.outcome = .unresolved → record.retainedBytes = true

theorem prepare_retains_unresolved (journal : Journal) (intent : Intent)
    (proposal : Prepared) (durable : Bool) (inv : retainsUnresolved journal) :
    retainsUnresolved (prepare journal intent proposal durable).1 := by
  rcases prepare_shape journal intent proposal durable with same | grown
  · rw [same]; exact inv
  · rw [grown]
    intro record mem
    simp only [List.mem_append, List.mem_singleton] at mem
    rcases mem with old | fresh
    · exact inv record old
    · intro _; rw [fresh]

theorem observe_retains_unresolved (journal : Journal) (caller : Caller)
    (prepared : Prepared) (outcome : Outcome) (inv : retainsUnresolved journal) :
    retainsUnresolved (observe journal caller prepared outcome) := by
  unfold observe
  split
  · exact inv
  · intro record mem
    simp only [List.mem_map] at mem
    obtain ⟨original, member, mapped⟩ := mem
    subst mapped
    split
    · rename_i changed
      intro settled
      simp only [Bool.and_eq_true, bne_iff_ne, ne_eq] at changed
      exact absurd settled changed.2
    · exact inv original member

theorem acknowledge_retains_unresolved (journal : Journal) (caller : Caller)
    (prepared : Prepared) (inv : retainsUnresolved journal) :
    retainsUnresolved (acknowledge journal caller prepared) := by
  unfold acknowledge
  split
  · exact inv
  · intro record mem
    simp only [List.mem_map] at mem
    obtain ⟨original, member, mapped⟩ := mem
    subst mapped
    split
    · rename_i retired
      intro unresolved
      simp only [Bool.and_eq_true, bne_iff_ne, ne_eq] at retired
      exact absurd unresolved retired.2
    · exact inv original member

theorem prepareAt_retains_unresolved (key : Nat) (journal : Journal) (intent : Intent)
    (proposal : Prepared) (durable : Bool) (inv : retainsUnresolved journal) :
    retainsUnresolved (prepareAt key journal intent proposal durable).1 := by
  unfold prepareAt
  split
  · exact inv
  · intro record mem
    simp only [List.mem_append] at mem
    rcases mem with old | fresh
    · exact inv record old
    · have mine : retainsUnresolved (scope journal key) := fun r m =>
        inv r (List.mem_filter.mp m).1
      exact prepare_retains_unresolved _ _ _ _ mine record (List.mem_of_mem_drop fresh)

/-! ### What the person is shown

`Saved` is what a client knows about one saved send; `affordances` is what it
shows and offers. It never claims more than the journal and the owner receipt
prove, and no view leaves the person without a way forward.

An undecided send keeps its signed bytes in the journal (`.retained`): the
person may check it again or resume it, which replays the identical bytes, and
it is never discarded while it can still be proven. A send whose bytes are no
longer retained (`.unretained`: an older uncorrelated send, or a lost or reset
journal) has only the owner's visible request as evidence, and its absence
proves nothing because the original commit may still be in flight. Reporting
it not delivered would invite a resend that duplicates the turn if the
original lands (TLA `ClientSubmissionContention`, `Unretained`); waiting with
no way out leaves the composer blocked forever (`UnretainedWait`). Decision:
the person may set it aside. Setting aside never claims non-delivery and mints
no identity; a late landing of the original still appears in the session, and
anything the person writes next is a new authored message. -/

inductive Unknown where
  | retained
  | unretained
  deriving DecidableEq, Repr

inductive Saved where
  /-- The send or its check is in flight. -/
  | sending
  /-- No reply was recorded and nothing is in flight. -/
  | unsent
  | delivered
  | notDelivered
  | unknown (why : Unknown)
  deriving DecidableEq, Repr

inductive DeliveryView where
  | sending
  | delivered
  | notDelivered
  | awaiting
  | unprovable
  deriving DecidableEq, Repr

structure Affordances where
  view : DeliveryView
  check : Bool
  resume : Bool
  dismiss : Bool
  setAside : Bool
  blocksComposer : Bool
  deriving DecidableEq, Repr

/-- `mailbox` marks an answer to a mailbox question: a delivered answer keeps
the question answered, so it also blocks answering again. -/
def affordances (saved : Saved) (mailbox : Bool) : Affordances :=
  match saved with
  | .sending => ⟨.sending, false, false, false, false, true⟩
  | .unsent => ⟨.awaiting, true, true, false, false, true⟩
  | .delivered => ⟨.delivered, true, false, false, false, mailbox⟩
  | .notDelivered => ⟨.notDelivered, true, false, true, false, false⟩
  | .unknown .retained => ⟨.awaiting, true, true, false, false, true⟩
  | .unknown .unretained => ⟨.unprovable, true, false, false, true, true⟩

/-- Whether the person may author a replacement send under a new identity. -/
def mayResendNewIdentity (view : DeliveryView) : Bool := view == .notDelivered

/-- A view that blocks the composer is in flight, a delivered mailbox answer,
or offers Resume or Set aside. -/
theorem blocking_views_offer_a_way_forward (saved : Saved) (mailbox : Bool)
    (blocks : (affordances saved mailbox).blocksComposer = true) :
    saved = .sending ∨ saved = .delivered ∨ (affordances saved mailbox).resume = true ∨
      (affordances saved mailbox).setAside = true := by
  cases saved with
  | unknown why => cases why <;> simp [affordances]
  | _ => simp_all [affordances]

/-- Only a send that can no longer be proven may be set aside. -/
theorem set_aside_only_without_bytes (saved : Saved) (mailbox : Bool)
    (offered : (affordances saved mailbox).setAside = true) :
    saved = .unknown .unretained := by
  cases saved with
  | unknown why => cases why <;> simp_all [affordances]
  | _ => simp_all [affordances]

/-- Only a proven non-delivery authorizes a new identity: an unprovable send is
never shown as not delivered. -/
theorem only_proven_non_delivery_authorizes_a_resend (saved : Saved) (mailbox : Bool)
    (resend : mayResendNewIdentity (affordances saved mailbox).view = true) :
    saved = .notDelivered := by
  cases saved with
  | unknown why => cases why <;> simp_all [affordances, mayResendNewIdentity]
  | _ => simp_all [affordances, mayResendNewIdentity]

/-- A journal record whose last reconcile could not decide. -/
def savedOf (record : Record) : Saved :=
  match record.outcome with
  | .accepted => .delivered
  | .notDelivered => .notDelivered
  | .unresolved => .unknown (if record.retainedBytes then .retained else .unretained)

/-- A journal-written record never reaches the unprovable view: it keeps its
bytes until it settles. -/
theorem journal_records_never_show_unprovable (journal : Journal)
    (inv : retainsUnresolved journal) (record : Record) (member : record ∈ journal) :
    savedOf record ≠ .unknown .unretained := by
  unfold savedOf
  split
  · simp
  · simp
  · rename_i unresolved
    rw [inv record member unresolved]
    simp

def savedJson : Saved → Json
  | .sending => toJson "sending"
  | .unsent => toJson "unsent"
  | .delivered => toJson "delivered"
  | .notDelivered => toJson "notDelivered"
  | .unknown .retained => toJson "unknownRetained"
  | .unknown .unretained => toJson "unknownUnretained"

def viewJson : DeliveryView → Json
  | .sending => toJson "sending"
  | .delivered => toJson "delivered"
  | .notDelivered => toJson "notDelivered"
  | .awaiting => toJson "awaiting"
  | .unprovable => toJson "unprovable"

def allSaved : List Saved :=
  [.sending, .unsent, .delivered, .notDelivered, .unknown .retained, .unknown .unretained]

def deliveryViewCasesJson : String :=
  (toJson (allSaved.flatMap fun saved => [false, true].map fun mailbox =>
    let shown := affordances saved mailbox
    Json.mkObj [("saved", savedJson saved), ("mailbox", toJson mailbox),
      ("view", viewJson shown.view), ("check", toJson shown.check),
      ("resume", toJson shown.resume), ("dismiss", toJson shown.dismiss),
      ("setAside", toJson shown.setAside), ("blocksComposer", toJson shown.blocksComposer),
      ("mayResendNewIdentity", toJson (mayResendNewIdentity shown.view))])).compress

end ClientSubmissionReceipt
