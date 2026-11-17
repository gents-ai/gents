import Proofs.Store.Identity.UserDelegation
import Proofs.Store.TransactionReplay

/-! What a client keeps of the delegation audience its home's public status
reports, and how it recovers when a request is refused for its credential
(gents#3831).

A client keeps the audience with its record of the home, so that it mints for
it without reading the status before every request. The record follows the
status: a runtime that restarts under another worker or deployment reports
another audience and refuses every delegation minted for the old one
(`UserDelegation.a_restart_under_another_audience_refuses_the_old_credential`);
the client reads the status again and sends the request once more, with no one
asked. A home on the node's own store reports none, and its client presents the
identity token it presented before this model. -/

namespace Identity.HeldAudience

open Identity.UserDelegation (Audience Reported Resolution resolve Delegation mint
  presentedOverHttp credential Credential verdict)

/-- What a client's record of its home holds of the home's audience. -/
inductive Held where
  | unknown
  | known
  deriving DecidableEq, Repr

/-- What reading the status does to the record. -/
inductive Change where
  | unchanged
  | learned
  | changed
  | forgotten
  deriving DecidableEq, Repr

/-- Decision (gents#3831): the record follows what the status names. A status
naming another audience than the held one replaces it, since the runtime came
back under another worker or deployment. A status naming none forgets it: the
home is on the node's own store, or its runtime is older than this model. A
status that could not read its audience now leaves the record as it is: the
home is still on Vera, a delegation for the audience it named last is useless
without the User's key, and a request it refuses is resolved again (`resend`).
`sameAudience` is whether the status names the audience the record holds. -/
def change (held : Held) (reported : Reported) (sameAudience : Bool) : Change :=
  match held, reported with
  | .unknown, .named => .learned
  | .known, .named => if sameAudience then .unchanged else .changed
  | .known, .absent => .forgotten
  | _, _ => .unchanged

/-- Decision (gents#3831): a client holding an audience that names no genesis
identity reads the status again before it mints a request-bound delegation,
since none can be minted for that audience
(`UserDelegation.a_request_bound_delegation_needs_the_genesis`); a provider
names it once it has proven its deployment's first block. A client holding no
audience mints no delegation, and one whose audience names it mints at once. -/
def rereadForGenesis (held : Held) (genesisKnown : Bool) : Bool :=
  held == .known && !genesisKnown

theorem a_client_whose_audience_names_its_genesis_mints_at_once (held : Held) :
    rereadForGenesis held true = false := by
  cases held <;> decide

/-- What a status read reports, with the audience it names. -/
inductive Status where
  | absent
  | named (audience : Audience)
  | unavailable
  deriving DecidableEq, Repr

def Status.reported : Status → Reported
  | .absent => .absent
  | .named _ => .named
  | .unavailable => .unavailable

def Held.of : Option Audience → Held
  | none => .unknown
  | some _ => .known

def Status.names (status : Status) (held : Option Audience) : Bool :=
  match status, held with
  | .named audience, some kept => audience == kept
  | _, _ => false

/-- The record after a status read. -/
def observe (held : Option Audience) : Status → Option Audience
  | .absent => none
  | .named audience => some audience
  | .unavailable => held

theorem a_record_changes_exactly_when_the_change_says_so (held : Option Audience)
    (status : Status) :
    (observe held status = held) ↔
      change (Held.of held) status.reported (status.names held) = .unchanged := by
  cases held with
  | none => cases status <;> simp [observe, change, Held.of, Status.reported, Status.names]
  | some kept =>
    cases status with
    | named audience =>
      by_cases same : audience = kept
      · simp [observe, change, Held.of, Status.reported, Status.names, same]
      · simp [observe, change, Held.of, Status.reported, Status.names, same]
    | absent => simp [observe, change, Held.of, Status.reported, Status.names]
    | unavailable => simp [observe, change, Held.of, Status.reported, Status.names]

/-- The credential a record yields at the one minting point
(`UserDelegation.credential`), for a client holding its User's key. -/
def recordCredential (held : Option Audience) : Credential :=
  credential false true true held.isSome

theorem a_record_resolves_as_its_status_does (held : Option Audience) (audience : Audience) :
    recordCredential (observe held (.named audience)) = .delegation
      ∧ resolve .named = .delegation := ⟨rfl, rfl⟩

/-- A home on the node's own store leaves a record that mints what it minted
before this model. -/
theorem a_home_on_the_node_store_keeps_the_identity_token (held : Option Audience) :
    recordCredential (observe held .absent) = .identityToken := rfl

/-- Why a node refused a client's request, as far as its credential goes. -/
inductive Refusal where
  /-- The edge of a Vera home refused an identity token (`UserDelegation.edge`). -/
  | delegationRequired
  /-- DefraDB refused a delegation the client presented: minted for another
  audience, or no longer current. -/
  | delegationRefused
  /-- Anything else, which no status could fix. -/
  | other
  deriving DecidableEq, Repr

/-- What a node answered a request with, as far as its credential goes. -/
inductive Answer where
  | unauthorized
  | forbidden
  | other
  deriving DecidableEq, Repr

/-- External premise: the edge of a Vera home answers a User's identity token
with 401 (`UserDelegation.edge`), and DefraDB answers a delegation it does not
take, for another worker or deployment or no longer current, with 403.
Decision (gents#3831): a client reads a 401 or 403 to a credential it
presented as a refusal of that credential. Whether the request is sent again is
the status's to decide (`resend`), so an ordinary 401 or 403 on a home whose
status changes nothing is told as it was before. -/
def refusalOf (answer : Answer) (presented : Credential) : Refusal :=
  match answer, presented with
  | .other, _ => .other
  | _, .identityToken => .delegationRequired
  | _, .delegation => .delegationRefused
  | _, _ => .other

/-- Decision (gents#3831): a request refused for its credential reads the
status again and is sent again once, silently, when that changed the record to
name an audience; otherwise the refusal is what the person is told. A request
already sent again is never sent a third time, so a client does not loop
against a runtime that keeps refusing it. -/
def resend (resent : Bool) (refusal : Refusal) (record : Change) : Bool :=
  !resent && decide (refusal ≠ .other) && (record == .learned || record == .changed)

theorem a_request_presenting_no_credential_is_never_sent_again (answer : Answer)
    (presented : Credential) (resent : Bool) (record : Change)
    (none : presented ≠ .identityToken ∧ presented ≠ .delegation) :
    resend resent (refusalOf answer presented) record = false := by
  cases answer <;> cases presented <;> simp_all [refusalOf] <;> cases resent <;> rfl

theorem a_request_is_sent_again_at_most_once (refusal : Refusal) (record : Change) :
    resend true refusal record = false := rfl

theorem only_a_credential_refusal_is_sent_again (resent : Bool) (record : Change) :
    resend resent .other record = false := by
  cases resent <;> rfl

/-- A home on the node's own store never sends a request again: its status
names no audience, so its record never comes to name one. -/
theorem a_home_on_the_node_store_never_resends (resent : Bool) (refusal : Refusal)
    (held : Held) (sameAudience : Bool) :
    resend resent refusal (change held .absent sameAudience) = false := by
  cases resent <;> cases refusal <;> cases held <;> rfl

theorem a_client_mints_for_its_held_audience (user : String) (node : Audience) (now : Nat)
    (delegation : Delegation) (minted : mint (.client user) user node now none = some delegation) :
    presentedOverHttp node now delegation = some user := by
  unfold mint at minted
  split at minted
  · cases minted
    simp [presentedOverHttp, verdict, Delegation.validFor, Delegation.currentAt]
  · cases minted

/-- After its runtime comes back under another audience, a client that held
the old one is refused, learns the new one from the status, sends the request
once more, and is taken as its User. -/
theorem a_client_recovers_from_a_restart_under_another_audience (user : String)
    (before after : Audience) (now : Nat) (stale fresh : Delegation)
    (changed : before ≠ after) (mintedFor : stale.audience = before)
    (minted : mint (.client user) user after now none = some fresh) :
    presentedOverHttp after now stale = none
      ∧ observe (some before) (.named after) = some after
      ∧ change (Held.of (some before)) (Status.named after).reported
          ((Status.named after).names (some before)) = .changed
      ∧ resend false .delegationRefused .changed = true
      ∧ presentedOverHttp after now fresh = some user := by
  refine ⟨?_, rfl, ?_, rfl, a_client_mints_for_its_held_audience user after now fresh minted⟩
  · have elsewhere : stale.audience ≠ after := by rw [mintedFor]; exact changed
    simp [presentedOverHttp, verdict, Delegation.validFor, elsewhere]
  · have differs : (after == before) = false := by
      simp only [beq_eq_false_iff_ne, ne_eq]
      exact fun same => changed same.symm
    simp [change, Held.of, Status.reported, Status.names, differs]

/-- A client that has not yet read its Vera home's status presents an identity
token, is refused at the edge, learns the audience and sends the request once
more as its User. -/
theorem a_client_learns_its_audience_from_the_refusal (user : String) (node : Audience)
    (now : Nat) (fresh : Delegation)
    (minted : mint (.client user) user node now none = some fresh) :
    (recordCredential none).presents.map (UserDelegation.edge .embeddedVera .user)
        = some .delegationRequired
      ∧ change .unknown .named false = .learned
      ∧ resend false .delegationRequired .learned = true
      ∧ presentedOverHttp node now fresh = some user :=
  ⟨rfl, rfl, rfl, a_client_mints_for_its_held_audience user node now fresh minted⟩

/-- Where in a transaction a request was refused for its credential. -/
inductive UnitStage where
  /-- Its begin or a request inside it: the commit was not sent. -/
  | beforeCommit
  /-- Its commit. -/
  | commit
  deriving DecidableEq, Repr

/-- Decision (gents#3831): a transaction is a unit, so one of its requests
refused for its credential is not sent again alone; the transaction it belonged
to is gone with the runtime that refused it. The owner reads the status, and
when that would send a lone request again (`resend`) it runs the whole
transaction again once, as a rerun (`TransactionReplay.rerunHonoured`): a
single-attempt or observing transaction is not run again, and one that already
sent a client-signed write runs again only where conflict rebuilds are
authorized. A refusal at commit is told: the client cannot tell whether a
commit answered with a refusal landed, and running a landed unit again could
apply it twice. External premise: DefraDB commits an HTTP transaction only on
its commit request, so a transaction whose commit was not sent did not land,
whether or not its discard was answered; the HTTP transport holds no native
operation that could poison it. -/
def resendUnit (resent : Bool) (refusal : Refusal) (record : Change) (stage : UnitStage)
    (mode : TransactionReplay.Mode) (dispatched : Bool) : Bool :=
  resend resent refusal record && stage == .beforeCommit
    && TransactionReplay.rerunHonoured mode true false dispatched

theorem a_refused_commit_is_never_run_again (resent : Bool) (refusal : Refusal)
    (record : Change) (mode : TransactionReplay.Mode) (dispatched : Bool) :
    resendUnit resent refusal record .commit mode dispatched = false := by
  simp [resendUnit]

theorem a_unit_is_run_again_at_most_once (refusal : Refusal) (record : Change)
    (stage : UnitStage) (mode : TransactionReplay.Mode) (dispatched : Bool) :
    resendUnit true refusal record stage mode dispatched = false := by
  simp [resendUnit, resend]

theorem a_unit_is_run_again_only_when_a_lone_request_would_be (resent : Bool)
    (refusal : Refusal) (record : Change) (stage : UnitStage) (mode : TransactionReplay.Mode)
    (dispatched : Bool) (h : resendUnit resent refusal record stage mode dispatched = true) :
    resend resent refusal record = true := by
  simp_all [resendUnit]

theorem a_unit_that_sent_a_signed_write_runs_again_only_as_a_rebuild (resent : Bool)
    (refusal : Refusal) (record : Change) (stage : UnitStage) (mode : TransactionReplay.Mode)
    (h : resendUnit resent refusal record stage mode true = true) :
    mode = .signedConflictRebuild := by
  cases mode <;> simp_all [resendUnit, TransactionReplay.rerunHonoured]

theorem a_unit_on_the_node_store_is_never_run_again (resent : Bool) (refusal : Refusal)
    (held : Held) (sameAudience : Bool) (stage : UnitStage) (mode : TransactionReplay.Mode)
    (dispatched : Bool) :
    resendUnit resent refusal (change held .absent sameAudience) stage mode dispatched = false := by
  simp [resendUnit, a_home_on_the_node_store_never_resends]

/-- What a client tells the person of a request that stayed refused. -/
inductive Told where
  /-- Gents on this device is to be updated: the home asks for a credential
  this version cannot present. -/
  | update
  /-- The refusal as the client told it before this model. -/
  | refusal
  deriving DecidableEq, Repr

/-- Decision (gents#3831): a request still refused after the client read the
status (`resend`) is told as an update when the edge of a Vera home answered
that it requires a delegation (`UserDelegation.edge`): a client that resolves
its audience never meets that answer, so one that does cannot present what the
home asks for. External premise: only that edge answers with that code. -/
def told (edgeRequiresDelegation : Bool) : Told :=
  if edgeRequiresDelegation then .update else .refusal

theorem a_client_refused_at_the_edge_is_told_to_update : told true = .update := rfl

theorem a_home_on_the_node_store_tells_refusals_as_before : told false = .refusal := rfl

end Identity.HeldAudience
