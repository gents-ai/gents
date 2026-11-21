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
identity token it presented before this model. The client keeps a report only
when the home's node key signed it over a nonce the client sent with that read
(gents#3964). -/

namespace Identity.HeldAudience

open Identity.UserDelegation (Audience Reported Resolution resolve Delegation mint
  presentedOverHttp credential Credential verdict HomeClock issueAt generalLifetime backdateSeconds)

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
  /-- The audience is the one held, and the home's clock is not where the
  record put it. -/
  | reclocked
  | forgotten
  deriving DecidableEq, Repr

/-- Decision (gents#3831): the record follows what the status names. A status
naming another audience than the held one replaces it, since the runtime came
back under another worker or deployment. A status naming none forgets it: the
home is on the node's own store, or its runtime is older than this model. A
status that could not read its audience now leaves the record as it is: the
home is still on Vera, a delegation for the audience it named last is useless
without the User's key, and a request it refuses is resolved again (`resend`).
`sameAudience` is whether the status names the audience the record holds.
Decision (gents#3831): a status naming the held audience whose clock is not
where the record put it (`clockMoved`) replaces the record's clock, so a client
whose own clock was set since it last read the status mints at the provider's
time again. -/
def change (held : Held) (reported : Reported) (sameAudience clockMoved : Bool) : Change :=
  match held, reported with
  | .unknown, .named => .learned
  | .known, .named =>
    if !sameAudience then .changed else if clockMoved then .reclocked else .unchanged
  | .known, .absent => .forgotten
  | _, _ => .unchanged

/-- What a home's status says of its audience. -/
inductive Reading where
  | absent
  | named (audience : Audience)
  | unavailable
  deriving DecidableEq, Repr

/-- What a home's status reports for its client to mint at: its audience and
the provider's time it mints at (`UserDelegation.issueAt`). -/
structure Report where
  reading : Reading
  providerTime : Option Nat
  deriving DecidableEq, Repr

def Report.unavailable : Report := ⟨.unavailable, none⟩

def Report.reported (report : Report) : Reported :=
  match report.reading with
  | .absent => .absent
  | .named _ => .named
  | .unavailable => .unavailable

/-- Which statement of a node key a signature is. A node key signs other
statements too, such as enrollment offers; each names its kind first, so no
signature of one is taken for another. -/
inductive Domain where
  | homeStatus
  | other
  deriving DecidableEq, Repr

/-- What a home signs of a status read: the kind of statement, the nonce its
client sent with the read, the node the status names, and its report. -/
structure Statement where
  domain : Domain
  nonce : Nat
  node : String
  report : Report
  deriving DecidableEq, Repr

/-- A signature as verifying it shows it: the key it verifies under and the
statement it covers. External premise: a node key's signature scheme is
unforgeable, so only the holder of the key a status names can make a signature
that verifies under it, and only over the statement it signed. -/
structure Signature where
  key : String
  statement : Statement
  deriving DecidableEq, Repr

/-- A status as a client reads it: the node it names, its report, and the
signature it carries, if any. -/
structure SignedStatus where
  node : String
  report : Report
  signature : Option Signature
  deriving DecidableEq, Repr

/-- Decision (gents#3964): a status is the saved home's when it names the node
the saved record names and carries that node key's signature of a home status
statement over the nonce the client sent with this read, that node, and the
report. Anyone answering at the home's address can name the home's node and
the audience of any Vera node, which is public; only the home's key can sign
it, and a fresh nonce keeps a status the home signed before, for a deployment
it has since left, from being answered again. -/
def authentic (home : String) (nonce : Nat) (status : SignedStatus) : Bool :=
  status.node == home
    && status.signature == some ⟨home, ⟨.homeStatus, nonce, home, status.report⟩⟩

/-- Decision (gents#3831, gents#3964): what a saved home's status reports
counts only when it is the home's (`authentic`); any other status, from another
node, unsigned, or signed over anything else, reports nothing: the record keeps
what it holds and the client never mints a delegation for an audience the home
did not sign to send to this home's endpoint. -/
def reportedOfHome (home : String) (nonce : Nat) (status : SignedStatus) : Report :=
  if authentic home nonce status then status.report else .unavailable

/-- Decision (gents#3831): a client holding an audience that names no genesis
identity reads the status again before it mints a request-bound delegation,
since none can be minted for that audience
(`UserDelegation.a_request_bound_delegation_needs_the_genesis`); a provider
names it once it has proven its deployment's first block. So does one that
holds no clock of its home: a request-bound delegation is issued at the
provider's time (`UserDelegation.issueAt`), and the stored command carrying it
is processed after its client is gone, so a wrong clock is not caught by a
refusal its client could answer. A client holding no audience mints no
delegation, and one whose audience names the genesis and whose clock is held
mints at once. -/
def rereadBeforeRequestBound (held : Held) (genesisKnown clockKnown : Bool) : Bool :=
  held == .known && (!genesisKnown || !clockKnown)

theorem a_client_whose_record_is_complete_mints_at_once (held : Held) :
    rereadBeforeRequestBound held true true = false := by
  cases held <;> decide

/-- What a status read reports, with the audience it names and the home's
clock as the client read it. -/
inductive Status where
  | absent
  | named (audience : Audience) (clock : Option HomeClock)
  | unavailable
  deriving DecidableEq, Repr

def Status.reported : Status → Reported
  | .absent => .absent
  | .named _ _ => .named
  | .unavailable => .unavailable

/-- What a client keeps of its home: the audience it mints for and the home's
clock it mints at (`UserDelegation.issueAt`). -/
structure Record where
  audience : Audience
  clock : Option HomeClock
  deriving DecidableEq, Repr

def Held.of : Option Record → Held
  | none => .unknown
  | some _ => .known

def Status.names (status : Status) (held : Option Record) : Bool :=
  match status, held with
  | .named audience _, some kept => audience == kept.audience
  | _, _ => false

/-- Decision (gents#3831): a home's clock read again has moved when the record
held none, or when the provider's time it reports is further than the
allowance DefraDB sets a delegation's start back by from where the held clock
puts it. A read reporting no clock moves nothing. -/
def clockMoved (held : Option HomeClock) (read : Option HomeClock) : Bool :=
  match held, read with
  | none, some _ => true
  | some kept, some now =>
    let expected := issueAt (some kept) now.clockAtRead
    expected + backdateSeconds < now.providerAtRead
      || now.providerAtRead + backdateSeconds < expected
  | _, none => false

def Status.moves (status : Status) (held : Option Record) : Bool :=
  match status, held with
  | .named _ clock, some kept => clockMoved kept.clock clock
  | _, _ => false

/-- The record after a status read. -/
def observe (held : Option Record) : Status → Option Record
  | .absent => none
  | .named audience clock =>
    match held with
    | some kept =>
      if audience ≠ kept.audience ∨ clockMoved kept.clock clock then some ⟨audience, clock⟩
      else held
    | none => some ⟨audience, clock⟩
  | .unavailable => held

theorem a_moved_clock_is_another_clock (held read : Option HomeClock)
    (moved : clockMoved held read = true) : read ≠ held := by
  intro same
  subst same
  cases read with
  | none => simp [clockMoved] at moved
  | some now => simp [clockMoved, issueAt] at moved; omega

theorem a_record_changes_exactly_when_the_change_says_so (held : Option Record)
    (status : Status) :
    (observe held status = held) ↔
      change (Held.of held) status.reported (status.names held) (status.moves held)
        = .unchanged := by
  cases held with
  | none => cases status <;> simp [observe, change, Held.of, Status.reported, Status.names]
  | some kept =>
    cases status with
    | named audience clock =>
      by_cases same : audience = kept.audience
      · by_cases moved : clockMoved kept.clock clock = true
        · have other := a_moved_clock_is_another_clock _ _ moved
          simp only [observe, change, Held.of, Status.reported, Status.names, Status.moves, same,
            moved, ne_eq, not_true_eq_false, or_true, ite_true, beq_self_eq_true, Bool.not_true,
            Bool.false_eq_true, ite_false, reduceCtorEq, iff_false, Option.some.injEq]
          intro back
          exact other (by rw [← back])
        · simp [observe, change, Held.of, Status.reported, Status.names, Status.moves, same,
            moved]
      · have differs : (audience == kept.audience) = false := by simpa using same
        simp only [observe, change, Held.of, Status.reported, Status.names, Status.moves,
          differs, Bool.not_false, ite_true, reduceCtorEq, iff_false, ne_eq, same,
          not_false_eq_true, true_or, Option.some.injEq]
        intro back
        exact same (by rw [← back])
    | absent => simp [observe, change, Held.of, Status.reported, Status.names]
    | unavailable => simp [observe, change, Held.of, Status.reported, Status.names]

/-- How the record a client keeps differs from the one it held before:
`before` and `after` are whether each held an audience, `sameAudience` whether
both name one audience and `sameClock` whether both put the home's clock at one
place. -/
def recordChange (before after : Held) (sameAudience sameClock : Bool) : Change :=
  match before, after with
  | .unknown, .unknown => .unchanged
  | .unknown, .known => .learned
  | .known, .unknown => .forgotten
  | .known, .known =>
    if !sameAudience then .changed else if !sameClock then .reclocked else .unchanged

/-- Decision (gents#3963): a request refused for its credential is resent for
how the record its client keeps once its status read is kept differs from the
record its credential was minted from (`presented`), never for what that read
alone changed. The clones of an endpoint share one record and read the status
one at a time (TLA+ `HeldAudienceReads`), so a request refused beside another
can wait while its peer reads and keeps the newer audience; its own read then
changes nothing, yet its credential was minted for the audience the home left,
and the newest one held would be taken. A read made alone reports exactly what
it changed (`a_lone_read_reports_what_it_changed`). A record taken before the
credential was minted, rather than the one it was minted from, can only be
older, since the record never returns to one it left; it may resend once a
request the newer record would have refused anyway, and never resends one more
time than `resend` allows. -/
def changeSince (presented kept : Option Record) : Change :=
  recordChange (Held.of presented) (Held.of kept)
    (match presented, kept with
      | some before, some after => before.audience == after.audience
      | _, _ => false)
    (match presented, kept with
      | some before, some after => before.clock == after.clock
      | _, _ => false)

theorem a_lone_read_reports_what_it_changed (held : Option Record) (status : Status) :
    changeSince held (observe held status)
      = change (Held.of held) status.reported (status.names held) (status.moves held) := by
  cases held with
  | none =>
    cases status <;> simp [changeSince, recordChange, observe, change, Held.of, Status.reported,
      Status.names, Status.moves]
  | some kept =>
    cases status with
    | absent => simp [changeSince, recordChange, observe, change, Held.of, Status.reported]
    | unavailable =>
      simp [changeSince, recordChange, observe, change, Held.of, Status.reported, Status.names,
        Status.moves]
    | named audience clock =>
      by_cases same : audience = kept.audience
      · by_cases moved : clockMoved kept.clock clock = true
        · have other := a_moved_clock_is_another_clock _ _ moved
          subst same
          simp [changeSince, recordChange, observe, change, Held.of, Status.reported, Status.names,
            Status.moves, moved, Ne.symm other]
        · subst same
          simp [changeSince, recordChange, observe, change, Held.of, Status.reported, Status.names,
            Status.moves, moved]
      · have differs : (kept.audience == audience) = false := by
          simpa using Ne.symm same
        have differs' : (audience == kept.audience) = false := by simpa using same
        simp [changeSince, recordChange, observe, change, Held.of, Status.reported, Status.names,
          Status.moves, same, differs, differs']

/-- The credential a record yields at the one minting point
(`UserDelegation.credential`), for a client holding its User's key. -/
def recordCredential (held : Option Record) : Credential :=
  credential false true true held.isSome

theorem a_record_resolves_as_its_status_does (held : Option Record) (audience : Audience)
    (clock : Option HomeClock) :
    recordCredential (observe held (.named audience clock)) = .delegation
      ∧ resolve .named = .delegation := by
  constructor
  · cases held with
    | none => rfl
    | some kept =>
      simp only [observe]
      split <;> rfl
  · rfl

/-- A home on the node's own store leaves a record that mints what it minted
before this model. -/
theorem a_home_on_the_node_store_keeps_the_identity_token (held : Option Record) :
    recordCredential (observe held .absent) = .identityToken := rfl

/-- What a client keeps of a report it read on its own clock `clock`. -/
def Report.status (report : Report) (clock : Nat) : Status :=
  match report.reading with
  | .absent => .absent
  | .named audience => .named audience (report.providerTime.map (⟨·, clock⟩))
  | .unavailable => .unavailable

theorem a_report_keeps_what_it_reports (report : Report) (clock : Nat) :
    (report.status clock).reported = report.reported := by
  cases report with
  | mk reading providerTime => cases reading <;> rfl

/-- A status that names the saved node with an audience that node did not
sign, or that carries no signature at all, changes no record: the client mints
only for the audience it held before, and one that held none mints no
delegation. -/
theorem an_unsigned_audience_changes_no_record (home : String) (nonce clock : Nat)
    (status : SignedStatus) (held : Option Record) (unsigned : authentic home nonce status = false) :
    observe held ((reportedOfHome home nonce status).status clock) = held
      ∧ recordCredential (observe held ((reportedOfHome home nonce status).status clock))
          = recordCredential held := by
  simp [reportedOfHome, unsigned, Report.unavailable, Report.status, observe]

theorem a_status_signed_for_another_nonce_is_refused (home : String) (sent signed : Nat)
    (status : SignedStatus) (other : sent ≠ signed)
    (byHome : status.signature = some ⟨home, ⟨.homeStatus, signed, home, status.report⟩⟩) :
    authentic home sent status = false := by
  simp [authentic, byHome, Ne.symm other]

theorem a_signature_of_another_statement_is_refused (home : String) (nonce : Nat)
    (status : SignedStatus) (domain : Domain) (other : domain ≠ .homeStatus)
    (byHome : status.signature = some ⟨home, ⟨domain, nonce, home, status.report⟩⟩) :
    authentic home nonce status = false := by
  simp [authentic, byHome, other]

theorem a_status_signed_by_another_key_is_refused (home key : String) (nonce : Nat)
    (status : SignedStatus) (statement : Statement) (other : key ≠ home)
    (signed : status.signature = some ⟨key, statement⟩) :
    authentic home nonce status = false := by
  simp [authentic, signed, other]

theorem an_altered_report_is_refused (home : String) (nonce : Nat) (status : SignedStatus)
    (covered : Report) (altered : status.report ≠ covered)
    (byHome : status.signature = some ⟨home, ⟨.homeStatus, nonce, home, covered⟩⟩) :
    authentic home nonce status = false := by
  simp [authentic, byHome, Ne.symm altered]

theorem a_status_from_another_node_is_refused (home : String) (nonce : Nat)
    (status : SignedStatus) (other : status.node ≠ home) :
    authentic home nonce status = false := by
  simp [authentic, other]

/-- The home's own signed status is taken as an accepted status was before
statuses were signed: its report is kept exactly as it reads. -/
theorem a_signed_status_is_kept_as_it_reports (home : String) (nonce clock : Nat)
    (report : Report) (held : Option Record) :
    reportedOfHome home nonce
        ⟨home, report, some ⟨home, ⟨.homeStatus, nonce, home, report⟩⟩⟩ = report
      ∧ observe held ((reportedOfHome home nonce
          ⟨home, report, some ⟨home, ⟨.homeStatus, nonce, home, report⟩⟩⟩).status clock)
        = observe held (report.status clock) := by
  simp [reportedOfHome, authentic]

/-- Decision (gents#3964): a status that is not the home's lets no credential
be sent to the home's endpoint, at the client's first read of it as at every
later one: it reports nothing, and a status that reports nothing resolves to a
refusal (`UserDelegation.resolve`), never to the identity token an unsigned
status could ask for. -/
theorem an_unauthenticated_status_lets_no_credential_be_sent (home : String) (nonce : Nat)
    (status : SignedStatus) (unsigned : authentic home nonce status = false) :
    resolve (reportedOfHome home nonce status).reported = .refused := by
  simp [reportedOfHome, unsigned, Report.unavailable, Report.reported, resolve]

/-- What a home's status says of how its clients authenticate: with the
identity token of a home on the node's own store, with delegations for its
Vera deployment, or not at all yet. -/
inductive Advertised where
  | local
  | vera
  | unavailable
  deriving DecidableEq, Repr

/-- What a client makes of a status the home signed. -/
inductive Reconciled where
  /-- Its report is kept. -/
  | kept
  /-- It reports no audience a client can mint for yet. -/
  | unavailable
  /-- It contradicts itself. -/
  | contradictory
  deriving DecidableEq, Repr

/-- Decision (gents#3964): a signed status states how its clients authenticate
and which audience it mints for, and the two must agree: a home on the node's
own store names no audience, and a Vera home names its own deployment or
none, which its client authentication then names. A signed status whose two
statements disagree is the home's contradiction, refused as such: it is never
read as a status that reported nothing, which would leave a client minting
for the audience it held before at its own clock. `sameDeployment` is whether
the audience names the deployment the client authentication names. -/
def reconcile (advertised : Advertised) (stated : Reported) (sameDeployment : Bool) :
    Reconciled :=
  match advertised, stated with
  | .unavailable, _ => .unavailable
  | _, .unavailable => .unavailable
  | .local, .absent => .kept
  | .local, .named => .contradictory
  | .vera, .absent => .kept
  | .vera, .named => if sameDeployment then .kept else .contradictory

/-- A status counts as the home's when the home signed it and it does not
contradict itself; a contradictory one is refused as an unauthenticated one is
(`recovery`). -/
def homeStatus (signed : Bool) (reconciled : Reconciled) : Bool :=
  signed && reconciled != .contradictory

theorem a_home_on_the_node_store_names_no_audience (sameDeployment : Bool) :
    reconcile .local .named sameDeployment = .contradictory
      ∧ reconcile .local .absent sameDeployment = .kept := ⟨rfl, rfl⟩

theorem a_vera_home_naming_another_deployment_contradicts_itself :
    reconcile .vera .named false = .contradictory := rfl

/-- Why a node refused a client's request, as far as its credential goes. -/
inductive Refusal where
  /-- The edge of a Vera home refused an identity token (`UserDelegation.edge`). -/
  | delegationRequired
  /-- DefraDB refused a delegation the client presented: minted for another
  audience, or no longer current. -/
  | delegationRefused
  /-- DefraDB refused a delegation the client presented because it does not
  hold at the provider's time. -/
  | outsideWindow
  /-- Anything else, which no status could fix. -/
  | other
  deriving DecidableEq, Repr

/-- What a node answered a request with, as far as its credential goes. -/
inductive Answer where
  | unauthorized
  | forbidden
  /-- DefraDB's `DELEGATION_OUTSIDE_WINDOW` error. -/
  | outsideWindow
  | other
  deriving DecidableEq, Repr

/-- External premise: the edge of a Vera home answers a User's identity token
with 401 (`UserDelegation.edge`), and DefraDB answers a delegation it does not
take, for another worker or deployment or no longer current, with 403.
Decision (gents#3831): a client reads a 401 or 403 to a credential it
presented as a refusal of that credential. Whether the request is sent again is
the status's to decide (`resend`), so an ordinary 401 or 403 on a home whose
status changes nothing is told as it was before. External premise
(defradb.rs#3006): DefraDB answers a verified write whose delegation does not
hold at the provider's time with the error code `DELEGATION_OUTSIDE_WINDOW`;
only a presented delegation can be refused so. -/
def refusalOf (answer : Answer) (presented : Credential) : Refusal :=
  match answer, presented with
  | .other, _ => .other
  | .outsideWindow, .delegation => .outsideWindow
  | .outsideWindow, _ => .other
  | _, .identityToken => .delegationRequired
  | _, .delegation => .delegationRefused
  | _, _ => .other

/-- Decision (gents#3831): a request refused for its credential reads the
status again and is sent again once, silently, when that changed the record to
name an audience or moved its clock; otherwise the refusal is what the person
is told. A delegation refused for its window is minted again and sent again
once whatever the read changed: the read reset the clock it is minted at, and a
delegation held past its end is cured by a new one. A request already sent
again is never sent a third time, so a client does not loop against a runtime
that keeps refusing it. -/
def resend (resent : Bool) (refusal : Refusal) (record : Change) : Bool :=
  !resent && decide (refusal ≠ .other)
    && (record == .learned || record == .changed || record == .reclocked
      || refusal == .outsideWindow)

/-- What becomes of a request refused for its credential once the client read
its home's status again. -/
inductive Recovery where
  | resend
  /-- The refusal is what the person is told (`told`). -/
  | tell
  /-- The request fails saying the home's status could not be authenticated. -/
  | unauthenticated
  deriving DecidableEq, Repr

/-- Decision (gents#3964): a request refused for its credential whose home's
status, read again, is not the home's (`authentic`) fails saying the home's
status could not be authenticated: nothing it reported is kept, and the person
learns why the request could not recover rather than only that it was refused.
A home that signs no status, being older than this client or holding no node
key, is told so. A refusal of no credential reads no status. Otherwise the
request goes as `resend` says. -/
def recovery (resent : Bool) (refusal : Refusal) (authenticated : Bool) (record : Change) :
    Recovery :=
  if refusal = .other then .tell
  else if !authenticated then .unauthenticated
  else if resend resent refusal record then .resend
  else .tell

theorem an_unauthenticated_status_never_sends_again (resent : Bool) (refusal : Refusal)
    (record : Change) : recovery resent refusal false record ≠ .resend := by
  unfold recovery
  split <;> simp

theorem a_contradictory_status_never_sends_again (resent : Bool) (refusal : Refusal)
    (record : Change) (signed : Bool) :
    recovery resent refusal (homeStatus signed .contradictory) record ≠ .resend := by
  simpa [homeStatus] using an_unauthenticated_status_never_sends_again resent refusal record

theorem an_authenticated_status_recovers_as_before (resent : Bool) (refusal : Refusal)
    (record : Change) :
    (recovery resent refusal true record = .resend) = (resend resent refusal record = true) := by
  unfold recovery
  split
  · rename_i other
    subst other
    cases resent <;> simp [resend]
  · split <;> simp_all

/-- A request refused for a credential minted for an audience its home has
left is sent again once its client keeps the newer audience, whether its own
read or a peer's read before it kept that audience. -/
theorem a_request_refused_behind_another_is_sent_again (refusal : Refusal)
    (before after : Record) (credential : refusal ≠ .other)
    (moved : before.audience ≠ after.audience) :
    recovery false refusal true (changeSince (some before) (some after)) = .resend := by
  have differs : (before.audience == after.audience) = false := by simpa using moved
  simp [recovery, resend, changeSince, recordChange, Held.of, differs, credential]

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
    (held : Held) (sameAudience clockMoved : Bool) (presented : Credential)
    (answer : Answer) (read : refusal = refusalOf answer presented)
    (noDelegation : presented ≠ .delegation) :
    resend resent refusal (change held .absent sameAudience clockMoved) = false := by
  subst read
  cases resent <;> cases answer <;> cases presented <;> cases held <;> simp_all [resend, refusalOf, change]

/-- A client whose clock is wrong and whose delegation was refused for its
window reads its home's clock again, mints at it and sends once more. -/
theorem a_delegation_refused_for_its_window_is_sent_again_once (record : Change) :
    resend false (refusalOf .outsideWindow .delegation) record = true
      ∧ resend true (refusalOf .outsideWindow .delegation) record = false := by
  cases record <;> decide

theorem a_client_mints_for_its_held_audience (user : String) (node : Audience) (now : Nat)
    (delegation : Delegation) (minted : mint (.client user) user node now none none = some delegation) :
    presentedOverHttp node now delegation = some user := by
  unfold mint at minted
  split at minted
  · cases minted
    simp [presentedOverHttp, verdict, Delegation.validFor, Delegation.currentAt, generalLifetime,
      issueAt]
  · cases minted

/-- After its runtime comes back under another audience, a client that held
the old one is refused, learns the new one from the status, sends the request
once more, and is taken as its User. -/
theorem a_client_recovers_from_a_restart_under_another_audience (user : String)
    (before after : Audience) (now : Nat) (stale fresh : Delegation)
    (changed : before ≠ after) (mintedFor : stale.audience = before)
    (minted : mint (.client user) user after now none none = some fresh) :
    presentedOverHttp after now stale = none
      ∧ observe (some ⟨before, none⟩) (.named after none) = some ⟨after, none⟩
      ∧ change (Held.of (some ⟨before, none⟩)) (Status.named after none).reported
          ((Status.named after none).names (some ⟨before, none⟩))
          ((Status.named after none).moves (some ⟨before, none⟩)) = .changed
      ∧ resend false .delegationRefused .changed = true
      ∧ presentedOverHttp after now fresh = some user := by
  refine ⟨?_, ?_, ?_, rfl, a_client_mints_for_its_held_audience user after now fresh minted⟩
  · have elsewhere : stale.audience ≠ after := by rw [mintedFor]; exact changed
    simp [presentedOverHttp, verdict, Delegation.validFor, elsewhere]
  · have differs : after ≠ before := fun same => changed same.symm
    simp [observe, differs]
  · have differs : (after == before) = false := by
      simp only [beq_eq_false_iff_ne, ne_eq]
      exact fun same => changed same.symm
    simp [change, Held.of, Status.reported, Status.names, differs]

/-- A client that has not yet read its Vera home's status presents an identity
token, is refused at the edge, learns the audience and sends the request once
more as its User. -/
theorem a_client_learns_its_audience_from_the_refusal (user : String) (node : Audience)
    (now : Nat) (fresh : Delegation)
    (minted : mint (.client user) user node now none none = some fresh) :
    (recordCredential none).presents.map (UserDelegation.edge .embeddedVera .user)
        = some .delegationRequired
      ∧ change .unknown .named false false = .learned
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

theorem a_unit_on_the_node_store_is_never_run_again (resent : Bool) (held : Held)
    (sameAudience clockMoved : Bool) (stage : UnitStage) (mode : TransactionReplay.Mode)
    (dispatched : Bool) (presented : Credential) (answer : Answer)
    (noDelegation : presented ≠ .delegation) :
    resendUnit resent (refusalOf answer presented) (change held .absent sameAudience clockMoved)
      stage mode dispatched = false := by
  simp [resendUnit,
    a_home_on_the_node_store_never_resends resent _ held sameAudience clockMoved presented answer
      rfl noDelegation]

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
