/-! One receive on a GraphQL subscription stream: what the caller gets, whether
the stream stays open, and how it ended. -/

namespace SubscriptionStream

/-- What reaches the stream before the next receive returns. `lateData` is
data that arrives after the renewal deadline, `silence` is nothing at all, and
`malformed` is a response that closes on bytes that are not UTF-8.
External premise: the pinned DefraDB sends `next` envelopes and `complete`
over SSE and authenticates a subscription only at its handshake, so a stream
kept past its bearer's life keeps delivering. -/
inductive Arrival where
  | data
  | denial
  | errorsList
  | errorsObject
  | errorsText
  | complete
  | closed
  | unknownEvent
  | malformed
  | lateData
  | silence
  deriving DecidableEq, Repr

inductive Received where
  | data | nothing | denied | rejected | unknownEvent | streamError | pending
  deriving DecidableEq, Repr

inductive End where
  | renewalDue | completed
  deriving DecidableEq, Repr

structure Step where
  received : Received
  /-- What the next receive returns when nothing further arrives. -/
  again : Received
  ended : Option End
  /-- Whether the failure, if any, is one a write owner would read as an
  ambiguous result worth another attempt. -/
  retryable : Bool
  deriving DecidableEq, Repr

def bearerLifetimeSeconds : Nat := 60

/-- A locally minted bearer is renewed at half its life, while it is still
valid. -/
def renewalPeriodSeconds : Nat := bearerLifetimeSeconds / 2

def closes (received : Received) (ended : Option End) : Step :=
  { received, again := .nothing, ended, retryable := false }

/-- An open stream with a due renewal ends at its deadline on the next
receive; any other open stream waits. -/
def stays (due : Bool) (received : Received) : Step :=
  { received, again := if due then .nothing else .pending, ended := none, retryable := false }

/-- `minted` says the stream was authorized by a locally minted bearer, which
arms a renewal deadline; `taken` says the caller took that cadence for
itself. A delegated credential is the caller's and arms nothing. -/
def step (minted taken : Bool) (arrival : Arrival) : Step :=
  let due := minted && !taken
  match arrival with
  | .data => stays due .data
  | .denial => closes .denied none
  | .errorsList | .errorsObject | .errorsText => closes .rejected none
  | .complete | .closed => closes .nothing (some .completed)
  | .unknownEvent => closes .unknownEvent none
  | .malformed => closes .streamError none
  | .lateData => if due then closes .nothing (some .renewalDue) else stays false .data
  | .silence => if due then closes .nothing (some .renewalDue) else stays false .pending

/-- The period a caller may keep the stream after taking its renewal: offered
once, and only for a minted bearer. -/
def takeRenewal (minted alreadyTaken : Bool) : Option Nat :=
  if minted && !alreadyTaken then some renewalPeriodSeconds else none

theorem renewal_falls_inside_the_bearer_life :
    renewalPeriodSeconds * 2 = bearerLifetimeSeconds := by decide

theorem an_errors_envelope_is_never_data (minted taken : Bool) (arrival : Arrival)
    (h : (step minted taken arrival).received = .data) :
    arrival = .data ∨ arrival = .lateData := by
  cases minted <;> cases taken <;> cases arrival <;> simp_all [step, closes, stays]

theorem a_denial_ends_the_stream_and_is_not_repeated (minted taken : Bool) :
    step minted taken .denial = closes .denied none := rfl

theorem anything_but_data_or_waiting_closes_the_stream (minted taken : Bool) (arrival : Arrival)
    (h : (step minted taken arrival).again = .pending) :
    (step minted taken arrival).received = .data
      ∨ (step minted taken arrival).received = .pending := by
  cases minted <;> cases taken <;> cases arrival <;> simp_all [step, closes, stays]

theorem an_untaken_minted_stream_never_waits_past_its_deadline (arrival : Arrival) :
    (step true false arrival).again = .nothing := by
  cases arrival <;> rfl

theorem completion_is_not_a_renewal (minted taken : Bool) :
    (step minted taken .complete).ended = some .completed
      ∧ (step minted taken .closed).ended = some .completed := ⟨rfl, rfl⟩

theorem a_due_renewal_does_not_wait_for_data :
    step true false .silence = closes .nothing (some .renewalDue) := rfl

theorem renewal_ends_only_an_untaken_minted_stream (minted taken : Bool) (arrival : Arrival)
    (h : (step minted taken arrival).ended = some .renewalDue) :
    minted = true ∧ taken = false := by
  cases minted <;> cases taken <;> cases arrival <;> simp_all [step, closes, stays]

theorem a_taken_renewal_delivers_past_the_deadline (minted : Bool) :
    step minted true .lateData = stays false .data := by
  cases minted <;> rfl

theorem renewal_is_offered_once (minted : Bool) : takeRenewal minted true = none := by
  cases minted <;> rfl

end SubscriptionStream
