import Proofs.Store.Identity.UserDelegation

/-! What a User's client does about the registrations and relationship changes
its User's writes still owe the document access control of a Vera home, and
how a long-lived client keeps the delegation it presented current
(gents#3831).

A write commits before Vera holds what it owes; DefraDB submits that after
the commit under the credential the write carried. A two-minute delegation can
lapse first, when the provider is slow or out of reach, and the entry then
waits for authority while the document stays readable by its creator alone.
Only the User's key can authorize it again, and that key is on the client, so
the client settles it: in the background, with no one asked, under a
delegation minted at the one minting point (`Identity.UserDelegation.mint`).
The runtime hands that delegation to DefraDB for the caller's own entries and
signs nothing. The interleaving of the client, the runtime and the provider is
`tla/OwedSettlement.tla`. -/

namespace Identity.OwedRegistration

open Identity.UserDelegation (Verdict Holder Audience Delegation mint lifetimeSeconds
  idleTrailSeconds a_minted_delegation_outlives_an_idle_block)

/-- Where DefraDB records an owed entry. External premise (`acp::OwedState`):
an entry is staged until submitted, waits for authority once its recorded
credential no longer lets the node submit it, or was refused for good; a
settled entry has no record. -/
inductive Recorded where
  | absent
  | staged
  | awaitingAuthority
  | refused
  deriving DecidableEq, Repr

/-- What the client can observe of one entry its own User owes. -/
inductive Standing where
  | settled
  /-- The node can still submit it as things stand. -/
  | sending
  /-- It waits behind its document's registration or an earlier change. -/
  | heldBehind
  /-- Only a fresh credential of its User moves it. -/
  | awaitingAuthority
  /-- The provider will never hold it through this record. -/
  | failed
  deriving DecidableEq, Repr

/-- What an owed entry DefraDB reports comes to for the client. External
premise (`acp::OwedEntry`): `discarded` is an entry whose record is gone
without the provider holding it; `sendable` one that a submission by the node
could still move; `held` one that waits behind another. -/
def standing (recorded : Recorded) (sendable held discarded : Bool) : Standing :=
  match recorded with
  | .absent => .settled
  | .refused => .failed
  | .staged | .awaitingAuthority =>
    if discarded then .failed
    else if sendable then .sending
    else if held then .heldBehind
    else if recorded = .awaitingAuthority then .awaitingAuthority
    else .sending

/-- What the runtime does with one of its caller's entries in a pass. -/
inductive Handling where
  /-- Hand DefraDB the delegation the caller presented with this request. -/
  | handIn
  | leave
  deriving DecidableEq, Repr

/-- Decision (gents#3831): a pass hands the caller's presented delegation in
for exactly the entries that wait for authority. One the node can still send
needs nothing, one held behind another moves when that one does, and a failed
one is not retried. External premise (`Node::reauthorize_owed_acp_as`): a
general delegation of the entry's actor to this node is checked as a write's
credential is, and DefraDB reports no policy command for an owed entry, so a
general delegation is the one a client can mint for it. -/
def handling : Standing → Handling
  | .awaitingAuthority => .handIn
  | _ => .leave

/-- Whether the runtime serves a pass for a caller at all. Decision
(gents#3831): only for a caller whose request presented its own general
delegation, current for this node; the runtime has nothing else to hand in,
and an identity token is refused at the edge before this. -/
def servesPass (presented : Bool) (verdict : Verdict) : Bool :=
  presented && verdict == .issuedBySigner

/-- What the client does with what a pass reports of one entry. -/
inductive EntryStep where
  | done
  | askAgain
  /-- Tell the person, in product words, that a change was not kept. -/
  | report
  deriving DecidableEq, Repr

def entryStep : Standing → EntryStep
  | .settled => .done
  | .failed => .report
  | .sending | .heldBehind | .awaitingAuthority => .askAgain

/-- Decision (gents#3831): the client asks again thirty seconds after a pass
that left anything to move, and runs one pass at a time. -/
def retrySeconds : Nat := 30

/-- Decision (gents#3831): the client keeps asking while it is online and
anything of its own is left to move. It does not give up after a number of
passes, since an entry left waiting for authority moves only when its client
hands in a credential (`tla/OwedSettlement.tla`, `MCOwedSettlementGivesUp`). -/
def continues (online : Bool) (standings : List Standing) : Bool :=
  online && standings.any fun standing => entryStep standing == .askAgain

theorem a_pass_hands_in_only_for_entries_waiting_for_authority (standing : Standing)
    (h : handling standing = .handIn) : standing = .awaitingAuthority := by
  cases standing <;> simp_all [handling]

theorem an_entry_waiting_for_authority_is_handed_in :
    handling .awaitingAuthority = .handIn := rfl

theorem a_failed_entry_is_reported (sendable held discarded : Bool) :
    entryStep (standing .refused sendable held discarded) = .report := rfl

theorem a_discarded_entry_is_reported (recorded : Recorded) (sendable held : Bool)
    (owed : recorded ≠ .absent) :
    entryStep (standing recorded sendable held true) = .report := by
  cases recorded <;> simp_all [standing, entryStep]

/-- Nothing the client can observe is dropped without a step: every standing
is done, reported, or asked about again. -/
theorem every_standing_has_a_step (standing : Standing) :
    entryStep standing = .done ∨ entryStep standing = .report
      ∨ entryStep standing = .askAgain := by
  cases standing <;> simp [entryStep]

theorem an_online_client_with_an_entry_to_move_continues (standings : List Standing)
    (standing : Standing) (listed : standing ∈ standings)
    (moves : entryStep standing = .askAgain) : continues true standings = true := by
  simp only [continues, Bool.true_and, List.any_eq_true, beq_iff_eq]
  exact ⟨standing, listed, moves⟩

theorem a_settled_or_reported_client_stops (online : Bool) (standings : List Standing)
    (quiet : ∀ standing ∈ standings, entryStep standing ≠ .askAgain) :
    continues online standings = false := by
  cases online
  · rfl
  · simp only [continues, Bool.true_and, List.any_eq_false, beq_iff_eq]
    exact quiet

theorem a_pass_needs_the_callers_own_delegation (presented : Bool) (verdict : Verdict)
    (h : servesPass presented verdict = true) :
    presented = true ∧ verdict = .issuedBySigner := by
  cases presented <;> cases verdict <;> simp_all [servesPass]

/-- What the runtime does with a delegation a client presents again on a
connection it keeps open. -/
inductive Refresh where
  /-- The connection's later writes carry it. -/
  | adopted
  /-- The connection is closed with nothing more written. -/
  | refused
  deriving DecidableEq, Repr

/-- Decision (gents#3831): a long-lived connection adopts a delegation its
client presents again only when it is a general delegation that its
connection's own caller issued, current for this node; anything else closes
the connection, so a connection never acts for another identity than the one
it was opened by. -/
def adopt (general : Bool) (verdict : Verdict) : Refresh :=
  if general && verdict == .issuedBySigner then .adopted else .refused

theorem a_connection_adopts_only_its_callers_general_delegation (general : Bool)
    (verdict : Verdict) (h : adopt general verdict = .adopted) :
    general = true ∧ verdict = .issuedBySigner := by
  cases general <;> cases verdict <;> simp_all [adopt]

/-- Decision (gents#3831): a client presents a fresh delegation on a
connection it keeps open before it sends anything once the one it presented
is half its lifetime old. -/
def refreshAfterSeconds : Nat := lifetimeSeconds / 2

def refreshDue (age : Nat) : Bool := refreshAfterSeconds ≤ age

/-- A client that refreshes when due presents, with whatever it sends, a
delegation at most `refreshAfterSeconds` old, so the runtime acting on that
message within the other half of the lifetime acts under a current
delegation, even at an idle node's trailing block time. -/
theorem a_refreshed_connection_acts_under_a_current_delegation (holder : Holder)
    (issuer : String) (audience : Audience) (issued age elapsed trail : Nat)
    (delegation : Delegation) (minted : mint holder issuer audience issued none = some delegation)
    (fresh : refreshDue age = false) (prompt : elapsed ≤ lifetimeSeconds - refreshAfterSeconds)
    (trails : trail ≤ idleTrailSeconds) :
    delegation.currentAt (issued + (age + elapsed) - trail) = true := by
  apply a_minted_delegation_outlives_an_idle_block holder issuer audience issued (age + elapsed)
    trail delegation minted _ trails
  simp only [refreshDue, refreshAfterSeconds, lifetimeSeconds, decide_eq_false_iff_not,
    Nat.not_le] at fresh prompt ⊢
  omega

end Identity.OwedRegistration
