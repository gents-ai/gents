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

/-- The claims of a delegation. `policyCommands` is the scope: policy
commands, nothing narrower or wider. `request` is the one policy command a
request-bound delegation authorizes, named by that command's digest; a
general delegation names none and authorizes every policy command of its
issuer while it is current. `signed` is the issuer's own key having signed
these claims. External premise (defradb.rs#2972): a request-bound delegation
covers exactly one policy command, so a create that also changes `n`
relationships needs `n + 1` of them. -/
structure Delegation where
  issuer : String
  audience : Audience
  notBefore : Nat
  expires : Nat
  policyCommands : Bool
  request : Option String
  signed : Bool
  deriving DecidableEq, Repr

/-- Decision (gents#3831): a delegation is minted per request and lives two
minutes. -/
def lifetimeSeconds : Nat := 120

/-- External premise (defradb.rs#2972): a request-bound delegation lives at
most ten minutes and is executable only within that long before its expiry,
so a stored command is processed within ten minutes of its signing or signed
again. -/
def requestBoundLimitSeconds : Nat := 600

theorem the_lifetime_fits_a_request_bound_delegation :
    lifetimeSeconds ≤ requestBoundLimitSeconds := by decide

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
issuer's key, for the audience a node reported, at the holder's clock, either
general or for the one write `request` names. -/
def mint (holder : Holder) (issuer : String) (audience : Audience) (now : Nat)
    (request : Option String) : Option Delegation :=
  if holder.signsFor issuer then
    some { issuer, audience, notBefore := now - backdateSeconds, expires := now + lifetimeSeconds,
           policyCommands := true, request, signed := true }
  else none

theorem the_runtime_never_mints_a_user_delegation (node user : String) (agents : List String)
    (audience : Audience) (now : Nat) (request : Option String) (notNode : node ≠ user)
    (notAgent : user ∉ agents) :
    mint (.runtime node agents) user audience now request = none := by
  simp [mint, Holder.signsFor, notNode, notAgent]

theorem a_client_mints_only_as_its_own_user (user issuer : String) (audience : Audience)
    (now : Nat) (request : Option String) (delegation : Delegation)
    (h : mint (.client user) issuer audience now request = some delegation) :
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
    (audience : Audience) (issued elapsed trail : Nat) (request : Option String)
    (delegation : Delegation)
    (minted : mint holder issuer audience issued request = some delegation)
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
presented as the bearer. External premise: it takes a general delegation that
is current for this node as its issuer's bearer, with no command beside it,
and refuses a request-bound one. -/
def presentedOverHttp (node : Audience) (now : Nat) (delegation : Delegation) : Option String :=
  if delegation.request.isNone
      && verdict node now delegation.issuer delegation = .issuedBySigner then
    some delegation.issuer
  else none

/-- The identity of a delegation inside a signed payload. External premise:
the digest is collision resistant, represented as injectivity (as
`Identity.KeyDerivation` represents key derivation). Decision: it is the
SHA-256 of the delegation's compact serialization, in lowercase hexadecimal,
and it is a field of the command's canonical signing payload. -/
structure Digest where
  of : Delegation → String
  injective : ∀ left right, of left = of right → left = right

/-- Where a signed command is when the runtime acts on it. A direct command
is handed to the runtime and acted on in the same exchange; nothing keeps it.
A stored command is a document the runtime reads later, so everyone who may
read that document reads what it carries. -/
inductive Carrier where
  | direct
  | stored
  deriving DecidableEq, Repr

/-- What a write does to a document. External premise (defradb.rs#2972): only
a create sends Vera a policy command of its own, so DefraDB takes a
request-bound delegation for a create alone; an update or a delete under one
is refused and needs a general delegation or a key the node holds. -/
inductive Write where
  | create
  | update
  | delete
  deriving DecidableEq, Repr

/-- A command a User signed. `delegationDigest` and `write` are covered by the
command's signature; a command that names no digest carries no delegation.
`write` is the digest of the exact policy command the runtime submits for the
command, and `kind` what the command does to its document. -/
structure Command where
  signer : String
  carrier : Carrier
  kind : Write
  write : String
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

/-- What an attached delegation authorizes, compared with the command's write. -/
inductive Scope where
  | general
  | thisWrite
  | anotherWrite
  deriving DecidableEq, Repr

def scope (command : Command) (delegation : Delegation) : Scope :=
  match delegation.request with
  | none => .general
  | some request => if request = command.write then .thisWrite else .anotherWrite

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
  request's credential. -/
  | forwarded
  | delegationRequired
  | delegationNotBound
  /-- A stored command carries a general delegation. -/
  | delegationNotRequestBound
  | delegationForAnotherWrite
  /-- A request-bound delegation rides an update or a delete. -/
  | requestBoundTakesNoUpdate
  | delegationRefused
  | issuerNotSigner
  deriving DecidableEq, Repr

/-- What the runtime does with a write it is about to make in its own process
for the author of a command it verified. `provider` is the home's recorded
choice. Decision (gents#3831): a stored command carries a request-bound
delegation and a general one in it is refused, so a document never holds a
credential for anything but its own write. A stored update or delete
therefore has no delegation it may carry: it is refused either as general or
as request-bound, until DefraDB takes a request-bound delegation for one.
Admission comes before the command's nonce or receipt is consumed and before
anything is written, so a refused command leaves no trace and is answered by
signing a new one. -/
def admit (provider : Provider) (author : Author) (carrier : Carrier) (write : Write)
    (carried : Carried) (scope : Scope) (verdict : Verdict) : Admission :=
  match provider, author with
  | .nodeStore, _ => .unchanged
  | .embeddedVera, .node => .unchanged
  | .embeddedVera, .agent => .unchanged
  | .embeddedVera, .user =>
    match carried with
    | .absent => .delegationRequired
    | .lifted => .delegationNotBound
    | .bound =>
      if scope = .anotherWrite then .delegationForAnotherWrite
      else if scope = .general ∧ carrier = .stored then .delegationNotRequestBound
      else if scope = .thisWrite ∧ write ≠ .create then .requestBoundTakesNoUpdate
      else match verdict with
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
  match attached with
  | some delegation =>
    admit provider author command.carrier command.kind (carried digest command attached)
      (scope command delegation) (verdict node now command.signer delegation)
  | none => admit provider author command.carrier command.kind .absent .general .refused

theorem admitCommand_attached (digest : Digest) (node : Audience) (now : Nat)
    (command : Command) (delegation : Delegation) :
    admitCommand digest .embeddedVera .user node now command (some delegation) =
      if command.delegationDigest = some (digest.of delegation) then
        if scope command delegation = .anotherWrite then .delegationForAnotherWrite
        else if scope command delegation = .general ∧ command.carrier = .stored then
          .delegationNotRequestBound
        else if scope command delegation = .thisWrite ∧ command.kind ≠ .create then
          .requestBoundTakesNoUpdate
        else if delegation.validFor node now then
          if delegation.issuer = command.signer then .forwarded else .issuerNotSigner
        else .delegationRefused
      else .delegationNotBound := by
  by_cases named : command.delegationDigest = some (digest.of delegation)
  · by_cases another : scope command delegation = .anotherWrite
    · simp [admitCommand, admit, carried, named, another]
    · by_cases general : scope command delegation = .general ∧ command.carrier = .stored
      · simp [admitCommand, admit, carried, named, another, general]
      · by_cases update : scope command delegation = .thisWrite ∧ command.kind ≠ .create
        · simp [admitCommand, admit, carried, named, another, general, update]
        · cases valid : delegation.validFor node now
          · simp [admitCommand, admit, carried, verdict, named, another, general, update, valid]
          · by_cases own : delegation.issuer = command.signer
            · simp [admitCommand, admit, carried, verdict, named, another, general, update,
                valid, own]
            · simp [admitCommand, admit, carried, verdict, named, another, general, update,
                valid, own]
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
      ∧ delegation.signed = true
      ∧ scope command delegation ≠ .anotherWrite
      ∧ (command.carrier = .stored →
          scope command delegation = .thisWrite ∧ command.kind = .create) := by
  cases attached with
  | none => simp [admitCommand, admit, Admission.runs] at h
  | some delegation =>
    rw [admitCommand_attached] at h
    by_cases named : command.delegationDigest = some (digest.of delegation)
    · by_cases another : scope command delegation = .anotherWrite
      · simp [named, another, Admission.runs] at h
      · by_cases general : scope command delegation = .general ∧ command.carrier = .stored
        · simp [named, another, general, Admission.runs] at h
        · by_cases update : scope command delegation = .thisWrite ∧ command.kind ≠ .create
          · simp [named, another, general, update, Admission.runs] at h
          · cases valid : delegation.validFor node now
            · simp [named, another, general, update, valid, Admission.runs] at h
            · by_cases own : delegation.issuer = command.signer
              · simp only [Delegation.validFor, Bool.and_eq_true, decide_eq_true_eq] at valid
                refine ⟨delegation, rfl, named, own, valid.1.2, valid.2, valid.1.1.2,
                  valid.1.1.1, another, ?_⟩
                intro stored
                have bound : scope command delegation = .thisWrite := by
                  cases found : scope command delegation
                  · exact absurd ⟨found, stored⟩ general
                  · rfl
                  · exact absurd found another
                refine ⟨bound, ?_⟩
                cases kind : command.kind
                · rfl
                · exact absurd ⟨bound, by simp [kind]⟩ update
                · exact absurd ⟨bound, by simp [kind]⟩ update
              · simp [named, another, general, update, valid, own, Admission.runs] at h
    · simp [named, Admission.runs] at h

theorem a_user_write_on_vera_is_forwarded_or_refused (carrier : Carrier) (write : Write)
    (carried : Carried) (scope : Scope) (verdict : Verdict) :
    admit .embeddedVera .user carrier write carried scope verdict ≠ .unchanged := by
  cases carrier <;> cases write <;> cases carried <;> cases scope <;> cases verdict <;> decide

/-- A stored update or delete has no delegation it may carry. -/
theorem a_stored_update_or_delete_is_refused (write : Write) (carried : Carried) (scope : Scope)
    (verdict : Verdict) (notCreate : write ≠ .create) :
    (admit .embeddedVera .user .stored write carried scope verdict).runs = false := by
  cases write <;> cases carried <;> cases scope <;> cases verdict <;> simp_all [admit, Admission.runs]

/-- Whether request-bound delegations cover a write's policy commands: one for
each, in order, and none besides. -/
def covers (policyCommands : List String) (delegations : List Delegation) : Bool :=
  policyCommands.length == delegations.length
    && (policyCommands.zip delegations).all fun pair => pair.2.request == some pair.1

theorem a_create_with_relationships_needs_a_delegation_for_each (policyCommands : List String)
    (delegations : List Delegation) (covered : covers policyCommands delegations = true) :
    delegations.length = policyCommands.length := by
  simp only [covers, Bool.and_eq_true, beq_iff_eq] at covered
  exact covered.1.symm

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

theorem a_general_delegation_in_a_stored_command_is_refused (digest : Digest) (node : Audience)
    (now : Nat) (command : Command) (delegation : Delegation)
    (stored : command.carrier = .stored) (general : delegation.request = none) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  have unbound : scope command delegation = .general := by simp [scope, general]
  rw [admitCommand_attached]
  by_cases named : command.delegationDigest = some (digest.of delegation)
  · simp [named, unbound, stored, Admission.runs]
  · simp [named, Admission.runs]

theorem a_delegation_for_another_write_is_refused (digest : Digest) (node : Audience)
    (now : Nat) (command : Command) (delegation : Delegation) (request : String)
    (names : delegation.request = some request) (another : request ≠ command.write) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  have elsewhere : scope command delegation = .anotherWrite := by simp [scope, names, another]
  rw [admitCommand_attached]
  by_cases named : command.delegationDigest = some (digest.of delegation)
  · simp [named, elsewhere, Admission.runs]
  · simp [named, Admission.runs]

/-- What a stored command carries is no bearer: the HTTP boundary takes it
from nobody. -/
theorem a_request_bound_delegation_is_no_bearer (node : Audience) (now : Nat)
    (delegation : Delegation) (bound : delegation.request.isSome = true) :
    presentedOverHttp node now delegation = none := by
  cases request : delegation.request
  · simp [request] at bound
  · simp [presentedOverHttp, request]

theorem an_invalid_delegation_fails_closed (digest : Digest) (node : Audience) (now : Nat)
    (command : Command) (delegation : Delegation)
    (invalid : delegation.validFor node now = false) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  rw [admitCommand_attached]
  by_cases named : command.delegationDigest = some (digest.of delegation)
  · by_cases another : scope command delegation = .anotherWrite
    · simp [named, another, Admission.runs]
    · by_cases general : scope command delegation = .general ∧ command.carrier = .stored
      · simp [named, another, general, Admission.runs]
      · by_cases update : scope command delegation = .thisWrite ∧ command.kind ≠ .create
        · simp [named, another, general, update, Admission.runs]
        · simp [named, another, general, update, invalid, Admission.runs]
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
    (carrier : Carrier) (write : Write) (carried : Carried) (scope : Scope) (verdict : Verdict)
    (refused : (admit provider author carrier write carried scope verdict).runs = false) :
    (admit provider author carrier write carried scope verdict).consumesCommand = false :=
  refused

/-- After any refusal the same User answers with a new create carrying a
delegation minted now for that command's own write, and it is admitted
whether the command is direct or stored. -/
theorem a_refused_command_is_signed_again (digest : Digest) (node : Audience) (now : Nat)
    (user write : String) (carrier : Carrier) (delegation : Delegation)
    (minted : mint (.client user) user node now (some write) = some delegation) :
    admitCommand digest .embeddedVera .user node now
      { signer := user, carrier, kind := .create, write,
        delegationDigest := some (digest.of delegation) }
      (some delegation) = .forwarded := by
  unfold mint at minted
  split at minted
  · cases minted
    rw [admitCommand_attached]
    simp [scope, Delegation.validFor, Delegation.currentAt]
  · cases minted

theorem node_and_agent_writes_are_unchanged (provider : Provider) (author : Author)
    (carrier : Carrier) (write : Write) (carried : Carried) (scope : Scope) (verdict : Verdict)
    (runtimeHeld : author ≠ .user) :
    admit provider author carrier write carried scope verdict = .unchanged := by
  cases provider <;> cases author <;> simp_all [admit]

theorem a_home_on_the_node_store_is_unchanged (author : Author) (carrier : Carrier)
    (write : Write) (carried : Carried) (scope : Scope) (verdict : Verdict) :
    admit .nodeStore author carrier write carried scope verdict = .unchanged := rfl

/-- The digest binding holds on the command path alone: a general delegation
that is current for this node is its issuer's bearer at the HTTP boundary,
for whoever holds it. That is why only a direct command may carry one. -/
theorem a_general_delegation_is_a_bearer_wherever_it_is_read (node : Audience) (now : Nat)
    (delegation : Delegation) (general : delegation.request = none)
    (valid : delegation.validFor node now = true) :
    presentedOverHttp node now delegation = some delegation.issuer := by
  simp [presentedOverHttp, verdict, general, valid]

/-- Whether the runtime may sign a pack's effects with a User key kept in its
home. Decision (gents#3831): on a Vera home it never does, because that
signature would need a delegation only the User's client may mint; the client
signs each effect, as it does for a hosted install. A home on the node's own
store keeps the route it has for as long as such homes exist. -/
def runtimeSignsWithHeldUserKey : Provider → Bool
  | .nodeStore => true
  | .embeddedVera => false

theorem a_vera_runtime_never_signs_as_a_user :
    runtimeSignsWithHeldUserKey .embeddedVera = false := rfl

/-- What a node's public status says of its delegation audience. -/
inductive Reported where
  /-- The status has no such field: a home on the node's own store, or a
  runtime older than this model. -/
  | absent
  | named
  /-- The home is on a Vera node whose audience could not be read now. -/
  | unavailable
  deriving DecidableEq, Repr

/-- What a runtime puts in its public status. The worker and the deployment
are not secret: a delegation for them is useless without its issuer's key. -/
def statusReports (provider : Provider) (nodeReportsAudience : Bool) : Reported :=
  match provider with
  | .nodeStore => .absent
  | .embeddedVera => if nodeReportsAudience then .named else .unavailable

/-- What a client does when it builds or refreshes an endpoint from a node's
status. It never guesses: a Vera home whose audience it cannot read gets no
credential at all, since an identity token would only be refused later. -/
inductive Resolution where
  | identityToken
  | delegation
  | refused
  deriving DecidableEq, Repr

def resolve : Reported → Resolution
  | .absent => .identityToken
  | .named => .delegation
  | .unavailable => .refused

theorem a_client_mints_a_delegation_only_for_a_home_on_vera (provider : Provider)
    (nodeReportsAudience : Bool)
    (h : resolve (statusReports provider nodeReportsAudience) = .delegation) :
    provider = .embeddedVera ∧ nodeReportsAudience = true := by
  cases provider <;> cases nodeReportsAudience <;> simp_all [resolve, statusReports]

theorem a_home_on_the_node_store_resolves_as_it_did (nodeReportsAudience : Bool) :
    resolve (statusReports .nodeStore nodeReportsAudience) = .identityToken := rfl

/-- A runtime that restarts on the same deployment with the same worker
reports the audience it reported, and delegations minted for it stay good. -/
theorem a_restart_that_changes_nothing_keeps_the_credential (digest : Digest)
    (before after : Audience) (now : Nat) (command : Command) (delegation : Delegation)
    (same : before = after)
    (admitted : admitCommand digest .embeddedVera .user before now command (some delegation)
      = .forwarded) :
    admitCommand digest .embeddedVera .user after now command (some delegation) = .forwarded := by
  rw [← same]
  exact admitted

/-- A runtime that comes back under another worker or deployment refuses every
delegation minted for the old audience; the client reads the status again. -/
theorem a_restart_under_another_audience_refuses_the_old_credential (digest : Digest)
    (before after : Audience) (now : Nat) (command : Command) (delegation : Delegation)
    (changed : before ≠ after) (mintedFor : delegation.audience = before) :
    (admitCommand digest .embeddedVera .user after now command (some delegation)).runs
      = false := by
  apply a_delegation_for_another_node_fails_closed
  rw [mintedFor]
  exact changed

/-- The credential a client endpoint presents, at the one place that mints it.
`forwardsCallerBearer` is an endpoint built around a bearer its caller
presented, passed on as it is. `nodeNamesAudience` is the endpoint holding the
audience it resolved from its node (`resolve`): only a home on an embedded
Vera node names one. Otherwise the endpoint mints what it minted before this
model. -/
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
