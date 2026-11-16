import Proofs.Store.DocumentAcpChoice

/-! What a write a User authored must present on a home whose document access
control runs on an embedded Vera node (gents#3831).

Vera acts for an identity only under a short-lived delegation that identity's
own key signed for the submitting node. DefraDB signs the Node's and the
Agents' delegations itself, with the keys the runtime holds. A User's key stays
on the client, so the client mints the delegation and the runtime only carries
it: as the request's bearer over HTTP, and inside the command the client
already signs where the runtime acts for that User in its own process.
Nothing here changes who may do what; a home on the node's own store is not
touched by any of it. -/

namespace Identity.UserDelegation

open DocumentAcpChoice (Provider)

/-- What a delegation must name for a node's Vera access control to act under
it: the node's submitting worker and its deployment. External premise: DefraDB
reports both in the node's light client status, and only a node whose document
access control runs on Vera reports them. -/
structure Audience where
  worker : String
  deployment : Nat
  deriving DecidableEq, Repr

/-- The claims of a delegation. `policyCommands` is the one scope DefraDB takes
from a caller: policy commands, nothing narrower or wider, and no relay or
request claim. `signed` is the issuer's own key having signed these claims. -/
structure Delegation where
  issuer : String
  audience : Audience
  notBefore : Nat
  expires : Nat
  policyCommands : Bool
  signed : Bool
  deriving DecidableEq, Repr

/-- Decision (gents#3831): a delegation is minted per request and lives two
minutes. -/
def lifetimeSeconds : Nat := 120

/-- External premise: DefraDB dates a delegation's validity from thirty seconds
before it is issued. -/
def backdateSeconds : Nat := 30

/-- External premise: the block time of an idle embedded Vera node trails the
clock by at most this much. -/
def idleTrailSeconds : Nat := 20

/-- Whoever holds keys. A client holds its User's key and no other. The
runtime holds the Node's key and derives its Agents' keys; it never holds a
User's (`Identity.Node`). -/
inductive Holder where
  | client (user : String)
  | runtime (node : String) (agents : List String)
  deriving DecidableEq, Repr

def Holder.signsFor : Holder → String → Bool
  | .client user, issuer => user == issuer
  | .runtime node agents, issuer => node == issuer || agents.contains issuer

/-- The one minting point. A delegation is issued only by the holder of its
issuer's key, for the audience a node reported, at the holder's clock. -/
def mint (holder : Holder) (issuer : String) (audience : Audience) (now : Nat) : Option Delegation :=
  if holder.signsFor issuer then
    some { issuer, audience, notBefore := now - backdateSeconds, expires := now + lifetimeSeconds,
           policyCommands := true, signed := true }
  else none

theorem the_runtime_never_mints_a_user_delegation (node user : String) (agents : List String)
    (audience : Audience) (now : Nat) (notNode : node ≠ user) (notAgent : user ∉ agents) :
    mint (.runtime node agents) user audience now = none := by
  simp [mint, Holder.signsFor, notNode, notAgent]

theorem a_client_mints_only_as_its_own_user (user issuer : String) (audience : Audience)
    (now : Nat) (delegation : Delegation)
    (h : mint (.client user) issuer audience now = some delegation) :
    delegation.issuer = user := by
  unfold mint at h
  split at h
  · rename_i signs
    cases h
    simpa [Holder.signsFor] using (Eq.symm (by simpa [Holder.signsFor] using signs))
  · cases h

/-- External premise: Vera and DefraDB accept a delegation from its
not-before time through its expiry, both inclusive. -/
def Delegation.currentAt (delegation : Delegation) (now : Nat) : Bool :=
  delegation.notBefore ≤ now && now ≤ delegation.expires

/-- A delegation minted at `issued` is current when its write reaches a block
`elapsed` seconds later, for any block time that trails the clock by no more
than an idle node's does and any `elapsed` within the lifetime. -/
theorem a_minted_delegation_outlives_an_idle_block (holder : Holder) (issuer : String)
    (audience : Audience) (issued elapsed trail : Nat) (delegation : Delegation)
    (minted : mint holder issuer audience issued = some delegation)
    (withinLifetime : elapsed ≤ lifetimeSeconds) (trails : trail ≤ idleTrailSeconds) :
    delegation.currentAt (issued + elapsed - trail) = true := by
  unfold mint at minted
  split at minted
  · cases minted
    simp only [Delegation.currentAt, backdateSeconds, lifetimeSeconds, idleTrailSeconds,
      Bool.and_eq_true, decide_eq_true_eq] at *
    omega
  · cases minted

/-- What DefraDB says of a token presented for a caller, at its own clock.
External premise (`DocumentACP::caller_of_delegation`): it names the issuer
only for a delegation its issuer signed, for policy commands, for this node's
worker and deployment, inside its validity interval; any other delegation is
refused, and a token that is not a delegation is not its to judge. -/
inductive Verdict where
  | issuedBySigner
  | issuedByAnother
  | refused
  deriving DecidableEq, Repr

def Delegation.validFor (delegation : Delegation) (node : Audience) (now : Nat) : Bool :=
  delegation.signed && delegation.policyCommands && decide (delegation.audience = node)
    && delegation.currentAt now

def verdict (node : Audience) (now : Nat) (signer : String) (delegation : Delegation) : Verdict :=
  if delegation.validFor node now then
    if delegation.issuer = signer then .issuedBySigner else .issuedByAnother
  else .refused

/-- The caller DefraDB's HTTP boundary authenticates from a delegation
presented as the bearer. -/
def presentedOverHttp (node : Audience) (now : Nat) (delegation : Delegation) : Option String :=
  if verdict node now delegation.issuer delegation = .issuedBySigner then some delegation.issuer
  else none

/-- The identity of a delegation inside a signed payload. External premise:
the digest is collision resistant, represented as injectivity (as
`Identity.KeyDerivation` represents key derivation). Decision: it is the
SHA-256 of the delegation's compact serialization, in lowercase hexadecimal,
and it is a field of the command's canonical signing payload. -/
structure Digest where
  of : Delegation → String
  injective : ∀ left right, of left = of right → left = right

/-- A command a User signed. `delegationDigest` is covered by the command's
signature; a command that names none carries no delegation. -/
structure Command where
  signer : String
  delegationDigest : Option String
  deriving DecidableEq, Repr

/-- What a command carries, compared with what its signed payload names. -/
inductive Carried where
  | absent
  /-- The attached delegation is the one the signed payload names. -/
  | bound
  /-- A delegation is attached that the signed payload does not name. -/
  | lifted
  deriving DecidableEq, Repr

def carried (digest : Digest) (command : Command) (attached : Option Delegation) : Carried :=
  match attached with
  | none => .absent
  | some delegation =>
    if command.delegationDigest = some (digest.of delegation) then .bound else .lifted

inductive Author where
  | user
  | node
  | agent
  deriving DecidableEq, Repr

inductive Admission where
  /-- The write proceeds as it did before this model, and nothing the command
  carries is given to DefraDB. -/
  | unchanged
  /-- The write runs as the signer with the carried delegation as the
  request's bearer. -/
  | forwarded
  | delegationRequired
  | delegationNotBound
  | delegationRefused
  | issuerNotSigner
  deriving DecidableEq, Repr

/-- What the runtime does with a write it is about to make in its own process
for the author of a command it verified. `provider` is the home's recorded
choice. Admission comes before the command's nonce or receipt is consumed and
before anything is written, so a refused command leaves no trace and is
answered by signing a new one. -/
def admit (provider : Provider) (author : Author) (carried : Carried) (verdict : Verdict) :
    Admission :=
  match provider, author with
  | .nodeStore, _ => .unchanged
  | .embeddedVera, .node => .unchanged
  | .embeddedVera, .agent => .unchanged
  | .embeddedVera, .user =>
    match carried with
    | .absent => .delegationRequired
    | .lifted => .delegationNotBound
    | .bound =>
      match verdict with
      | .issuedBySigner => .forwarded
      | .issuedByAnother => .issuerNotSigner
      | .refused => .delegationRefused

def Admission.runs : Admission → Bool
  | .unchanged => true
  | .forwarded => true
  | _ => false

/-- Whether an admission uses up the command: its nonce, or the receipt that
answers it. -/
def Admission.consumesCommand (admission : Admission) : Bool := admission.runs

def admitCommand (digest : Digest) (provider : Provider) (author : Author) (node : Audience)
    (now : Nat) (command : Command) (attached : Option Delegation) : Admission :=
  admit provider author (carried digest command attached)
    (match attached with
      | some delegation => verdict node now command.signer delegation
      | none => .refused)

theorem admitCommand_attached (digest : Digest) (node : Audience) (now : Nat)
    (command : Command) (delegation : Delegation) :
    admitCommand digest .embeddedVera .user node now command (some delegation) =
      if command.delegationDigest = some (digest.of delegation) then
        if delegation.validFor node now then
          if delegation.issuer = command.signer then .forwarded else .issuerNotSigner
        else .delegationRefused
      else .delegationNotBound := by
  by_cases named : command.delegationDigest = some (digest.of delegation)
  · cases valid : delegation.validFor node now
    · simp [admitCommand, admit, carried, verdict, named, valid]
    · by_cases own : delegation.issuer = command.signer
      · simp [admitCommand, admit, carried, verdict, named, valid, own]
      · simp [admitCommand, admit, carried, verdict, named, valid, own]
  · simp [admitCommand, admit, carried, named]

theorem a_user_write_on_vera_runs_only_under_its_own_bound_current_delegation
    (digest : Digest) (node : Audience) (now : Nat) (command : Command)
    (attached : Option Delegation)
    (h : (admitCommand digest .embeddedVera .user node now command attached).runs = true) :
    ∃ delegation, attached = some delegation
      ∧ command.delegationDigest = some (digest.of delegation)
      ∧ delegation.issuer = command.signer
      ∧ delegation.audience = node
      ∧ delegation.currentAt now = true
      ∧ delegation.policyCommands = true
      ∧ delegation.signed = true := by
  cases attached with
  | none => simp [admitCommand, admit, carried, Admission.runs] at h
  | some delegation =>
    rw [admitCommand_attached] at h
    by_cases named : command.delegationDigest = some (digest.of delegation)
    · cases valid : delegation.validFor node now
      · simp [named, valid, Admission.runs] at h
      · by_cases own : delegation.issuer = command.signer
        · simp only [Delegation.validFor, Bool.and_eq_true, decide_eq_true_eq] at valid
          exact ⟨delegation, rfl, named, own, valid.1.2, valid.2, valid.1.1.2, valid.1.1.1⟩
        · simp [named, valid, own, Admission.runs] at h
    · simp [named, Admission.runs] at h

theorem a_user_write_on_vera_is_forwarded_or_refused (carried : Carried) (verdict : Verdict) :
    admit .embeddedVera .user carried verdict ≠ .unchanged := by
  cases carried <;> cases verdict <;> decide

/-- A delegation taken from the command it was signed into and attached to
another command is refused, whatever else holds of it. -/
theorem a_lifted_delegation_is_refused (digest : Digest) (node : Audience) (now : Nat)
    (other : Command) (delegation : Delegation)
    (notNamed : other.delegationDigest ≠ some (digest.of delegation)) :
    admitCommand digest .embeddedVera .user node now other (some delegation)
      = .delegationNotBound := by
  rw [admitCommand_attached, if_neg notNamed]

/-- A command names at most one delegation: one bound to another delegation
does not admit this one. -/
theorem a_command_bound_to_another_delegation_refuses_this_one (digest : Digest)
    (node : Audience) (now : Nat) (command : Command) (named attached : Delegation)
    (names : command.delegationDigest = some (digest.of named)) (differs : named ≠ attached) :
    admitCommand digest .embeddedVera .user node now command (some attached)
      = .delegationNotBound := by
  apply a_lifted_delegation_is_refused
  rw [names]
  intro same
  exact differs (digest.injective _ _ (Option.some.inj same))

theorem an_invalid_delegation_fails_closed (digest : Digest) (node : Audience) (now : Nat)
    (command : Command) (delegation : Delegation)
    (invalid : delegation.validFor node now = false) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  rw [admitCommand_attached]
  by_cases named : command.delegationDigest = some (digest.of delegation)
  · simp [named, invalid, Admission.runs]
  · simp [named, Admission.runs]

theorem an_expired_delegation_fails_closed (digest : Digest) (node : Audience) (now : Nat)
    (command : Command) (delegation : Delegation) (expired : delegation.expires < now) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  apply an_invalid_delegation_fails_closed
  have stale : delegation.currentAt now = false := by
    simp only [Delegation.currentAt, Bool.and_eq_false_iff, decide_eq_false_iff_not]
    omega
  simp [Delegation.validFor, stale]

theorem a_delegation_for_another_node_fails_closed (digest : Digest) (node : Audience) (now : Nat)
    (command : Command) (delegation : Delegation) (elsewhere : delegation.audience ≠ node) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  apply an_invalid_delegation_fails_closed
  simp [Delegation.validFor, elsewhere]

theorem a_refused_command_is_not_consumed (provider : Provider) (author : Author)
    (carried : Carried) (verdict : Verdict)
    (refused : (admit provider author carried verdict).runs = false) :
    (admit provider author carried verdict).consumesCommand = false := refused

/-- After any refusal the same User answers with a new command carrying a
delegation minted now, and it is admitted. -/
theorem a_refused_command_is_signed_again (digest : Digest) (node : Audience) (now : Nat)
    (user : String) (delegation : Delegation)
    (minted : mint (.client user) user node now = some delegation) :
    admitCommand digest .embeddedVera .user node now
      { signer := user, delegationDigest := some (digest.of delegation) } (some delegation)
      = .forwarded := by
  unfold mint at minted
  split at minted
  · cases minted
    rw [admitCommand_attached]
    simp [Delegation.validFor, Delegation.currentAt]
  · cases minted

theorem node_and_agent_writes_are_unchanged (provider : Provider) (author : Author)
    (carried : Carried) (verdict : Verdict) (runtimeHeld : author ≠ .user) :
    admit provider author carried verdict = .unchanged := by
  cases provider <;> cases author <;> simp_all [admit]

theorem a_home_on_the_node_store_is_unchanged (author : Author) (carried : Carried)
    (verdict : Verdict) : admit .nodeStore author carried verdict = .unchanged := rfl

/-- The binding holds on the command path alone. External premise: DefraDB's
HTTP boundary takes any delegation that is current for this node as its
issuer's bearer, with no command beside it. Whoever can read a command that
carries a delegation therefore holds that User's bearer for policy commands
until it expires. -/
theorem the_binding_does_not_reach_the_http_boundary (node : Audience) (now : Nat)
    (delegation : Delegation) (valid : delegation.validFor node now = true) :
    presentedOverHttp node now delegation = some delegation.issuer := by
  simp [presentedOverHttp, verdict, valid]

/-- The credential a client endpoint presents, at the one place that mints it.
`forwardsCallerBearer` is an endpoint built around a bearer its caller
presented, passed on as it is. `nodeNamesAudience` is the node having reported
a delegation audience: only a home on an embedded Vera node does. Otherwise
the endpoint mints what it minted before this model. -/
inductive Credential where
  | anonymous
  | callerBearer
  | signerRequired
  | identityToken
  | delegation
  deriving DecidableEq, Repr

def credential (forwardsCallerBearer principal holdsKey nodeNamesAudience : Bool) : Credential :=
  if forwardsCallerBearer then .callerBearer
  else if !principal then .anonymous
  else if !holdsKey then .signerRequired
  else if nodeNamesAudience then .delegation
  else .identityToken

theorem a_delegation_is_minted_only_with_the_held_key_for_a_vera_home
    (forwardsCallerBearer principal holdsKey nodeNamesAudience : Bool)
    (h : credential forwardsCallerBearer principal holdsKey nodeNamesAudience = .delegation) :
    forwardsCallerBearer = false ∧ principal = true ∧ holdsKey = true
      ∧ nodeNamesAudience = true := by
  cases forwardsCallerBearer <;> cases principal <;> cases holdsKey <;> cases nodeNamesAudience
    <;> simp_all [credential]

theorem a_home_on_the_node_store_gets_the_credential_it_got
    (forwardsCallerBearer principal holdsKey : Bool) :
    credential forwardsCallerBearer principal holdsKey false ≠ .delegation := by
  cases forwardsCallerBearer <;> cases principal <;> cases holdsKey <;> decide

end Identity.UserDelegation
