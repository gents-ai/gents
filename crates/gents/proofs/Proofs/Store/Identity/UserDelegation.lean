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
it: the node's submitting worker and its deployment, and the deployment's
genesis identity once the node has proven it. External premise: DefraDB
reports all three in the node's light client status, only a node whose
document access control runs on Vera reports them, and the genesis identity is
absent until its provider has proven the deployment's first block
(defradb.rs#2987). -/
structure Audience where
  worker : String
  deployment : Nat
  genesis : Option String
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

/-- External premise (defradb.rs#3006, `acp::CLOCK_SKEW_ALLOWANCE`): DefraDB
dates every delegation's validity from thirty seconds before it is issued,
general and request-bound alike. -/
def backdateSeconds : Nat := 30

/-- External premise: the block time of an idle embedded Vera node trails the
clock by at most this much. -/
def idleTrailSeconds : Nat := 20

/-- External premise (vera.rs `MAX_OPERATION_TTL`): vera.rs executes a
request-bound delegation only while its operation identity is open, and refuses
one that stays open more than ten minutes past the block that executes it. -/
def requestBoundLimitSeconds : Nat := 600

theorem the_lifetime_fits_a_request_bound_delegation :
    lifetimeSeconds ≤ requestBoundLimitSeconds := by decide

/-- Decision (gents#3831): a stored command lives five minutes from its
signing; the runtime holds it until it is valid on its own clock and answers it
as expired after that. -/
def storedCommandLifetimeSeconds : Nat := 300

/-- Decision (gents#3831): a request-bound delegation a stored command carries
lives 570 seconds, the most DefraDB issues (defradb.rs#3006: the provider's
ten minutes less the clock skew allowance), and so outlives its command by four
and a half minutes in which the registration it authorizes is sent. -/
def requestBoundLifetimeSeconds : Nat := 570

theorem a_request_bound_lifetime_fits_the_provider :
    requestBoundLifetimeSeconds + backdateSeconds ≤ requestBoundLimitSeconds := by decide

theorem a_request_bound_delegation_outlives_its_stored_command :
    storedCommandLifetimeSeconds < requestBoundLifetimeSeconds := by decide

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

/-- What a client knows of its home's clock: the provider's time its home's
status last reported, and the client's own clock when it read it. -/
structure HomeClock where
  providerAtRead : Nat
  clockAtRead : Nat
  deriving DecidableEq, Repr

/-- Decision (gents#3831): a client takes the provider's time now to be the
time its home's status reported, advanced by what its own clock has run since
it read it, and its own clock while it has read none. A clock that is merely
wrong drops out: only how far it ran since the read counts. External premise
(defradb.rs#3006): the provider judges a delegation at its own time, which the
status reports as its light client's newest verified block time. -/
def issueAt (home : Option HomeClock) (clock : Nat) : Nat :=
  match home with
  | none => clock
  | some home => home.providerAtRead + (clock - home.clockAtRead)

/-- How long a general delegation is minted for. External premise
(`vera_delegation::caller_delegation`): DefraDB issues a general delegation at the
signer's own clock and takes only its lifetime. Decision (gents#3831): a client
whose clock is behind its home's lengthens it by how far behind, so the
delegation ends `lifetimeSeconds` after the provider's time, as one issued
there would; a clock ahead is left to the allowance DefraDB sets its start back
by. -/
def generalLifetime (home : Option HomeClock) (clock : Nat) : Nat :=
  lifetimeSeconds + (issueAt home clock - clock)

/-- Decision (gents#3831): a client dates a stored command it signs at the same
estimate of the provider's time (`issueAt`), and it lasts
`storedCommandLifetimeSeconds` from there. -/
def storedCommandWindow (home : Option HomeClock) (clock : Nat) : Nat × Nat :=
  (issueAt home clock, issueAt home clock + storedCommandLifetimeSeconds)

/-- A stored command dated at its home's clock is never early for the runtime,
which reads the provider's clock, and is processed inside its lifetime whenever
the runtime takes it up `wait` after its signing, however wrong the client's
clock: the client read its home's status `trail` behind the provider's time
and ran its own clock on by `elapsed` before it signed. -/
theorem a_stored_command_from_a_skewed_clock_is_current_for_the_runtime
    (providerAtRead clockAtRead elapsed trail wait : Nat)
    (prompt : trail + wait < storedCommandLifetimeSeconds) :
    let window := storedCommandWindow (some ⟨providerAtRead, clockAtRead⟩) (clockAtRead + elapsed)
    window.1 ≤ providerAtRead + trail + elapsed + wait
      ∧ providerAtRead + trail + elapsed + wait < window.2 := by
  simp only [storedCommandWindow, issueAt]
  omega

/-- The one minting point. A delegation is issued only by the holder of its
issuer's key, for the audience a node reported, either general or for the one
write `request` names. External premise (`vera::vera_rs::request_delegation_at`,
defradb.rs#2987, #3006): a request-bound delegation names the deployment's
genesis identity, so none is minted for an audience that names none, and it is
issued at the time its signer names. Decision (gents#3831): a client issues a
request-bound delegation at its estimate of the provider's time (`issueAt`),
and a general one at its own clock for `generalLifetime`. `clock` is the
holder's clock and `home` what it read of its home's. -/
def mint (holder : Holder) (issuer : String) (audience : Audience) (clock : Nat)
    (home : Option HomeClock) (request : Option String) : Option Delegation :=
  if holder.signsFor issuer ∧ (request.isNone ∨ audience.genesis.isSome) then
    let issued := if request.isSome then issueAt home clock else clock
    some { issuer, audience,
           notBefore := issued - backdateSeconds,
           expires := issued + if request.isSome then requestBoundLifetimeSeconds
             else generalLifetime home clock,
           policyCommands := true, request, signed := true }
  else none

theorem a_request_bound_delegation_needs_the_genesis (holder : Holder) (issuer : String)
    (audience : Audience) (now : Nat) (home : Option HomeClock) (write : String)
    (unknown : audience.genesis = none) :
    mint holder issuer audience now home (some write) = none := by
  simp [mint, unknown]

theorem the_runtime_never_mints_a_user_delegation (node user : String) (agents : List String)
    (audience : Audience) (now : Nat) (home : Option HomeClock) (request : Option String)
    (notNode : node ≠ user) (notAgent : user ∉ agents) :
    mint (.runtime node agents) user audience now home request = none := by
  simp [mint, Holder.signsFor, notNode, notAgent]

theorem a_client_mints_only_as_its_own_user (user issuer : String) (audience : Audience)
    (now : Nat) (home : Option HomeClock) (request : Option String) (delegation : Delegation)
    (h : mint (.client user) issuer audience now home request = some delegation) :
    delegation.issuer = user := by
  unfold mint at h
  split at h
  · rename_i signs
    cases h
    simpa [Holder.signsFor] using (Eq.symm (by simpa [Holder.signsFor] using signs.1))
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
    (minted : mint holder issuer audience issued none none = some delegation)
    (withinLifetime : elapsed ≤ lifetimeSeconds) (trails : trail ≤ idleTrailSeconds) :
    delegation.currentAt (issued + elapsed - trail) = true := by
  unfold mint at minted
  split at minted
  · cases minted
    simp only [Delegation.currentAt, backdateSeconds, generalLifetime, issueAt, lifetimeSeconds,
      idleTrailSeconds, Option.isSome_none, Bool.false_eq_true, ite_false, Bool.and_eq_true,
      decide_eq_true_eq] at *
    omega
  · cases minted

/-- A general delegation minted by a client whose clock is behind its home's by
any amount, or ahead by no more than the allowance DefraDB sets its start back
by, is current at the provider's time for its whole lifetime after the client
read its home's status, `trail` behind the provider, and ran its own clock on
by `elapsed`. -/
theorem a_general_delegation_from_a_slow_clock_is_current (holder : Holder) (issuer : String)
    (audience : Audience) (providerAtRead clockAtRead elapsed trail wait : Nat)
    (delegation : Delegation)
    (minted : mint holder issuer audience (clockAtRead + elapsed)
      (some ⟨providerAtRead, clockAtRead⟩) none = some delegation)
    (ahead : clockAtRead ≤ providerAtRead + backdateSeconds)
    (prompt : trail + wait ≤ lifetimeSeconds) :
    delegation.currentAt (providerAtRead + trail + elapsed + wait) = true := by
  unfold mint at minted
  split at minted
  · cases minted
    simp only [Delegation.currentAt, backdateSeconds, generalLifetime, issueAt, lifetimeSeconds,
      Option.isSome_none, Bool.false_eq_true, ite_false, Bool.and_eq_true,
      decide_eq_true_eq] at *
    omega
  · cases minted

/-- The limit `generalLifetime` leaves: DefraDB issues a general delegation at
its signer's clock, so one from a clock further ahead of the provider than the
allowance is not yet valid when it arrives, whatever its client read. -/
theorem a_general_delegation_from_a_fast_clock_is_not_yet_valid (holder : Holder)
    (issuer : String) (audience : Audience) (providerAtRead clockAtRead elapsed : Nat)
    (delegation : Delegation)
    (minted : mint holder issuer audience (clockAtRead + elapsed)
      (some ⟨providerAtRead, clockAtRead⟩) none = some delegation)
    (fast : providerAtRead + backdateSeconds < clockAtRead) :
    delegation.currentAt (providerAtRead + elapsed) = false := by
  unfold mint at minted
  split at minted
  · cases minted
    simp only [Delegation.currentAt, backdateSeconds, Option.isSome_none, Bool.false_eq_true,
      ite_false, Bool.and_eq_false_iff, decide_eq_false_iff_not] at *
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
  /-- A stored update or delete, made with no credential: the provider admits
  it on its signer's own verified permission alone. -/
  | providerDecides
  | delegationRefused
  | issuerNotSigner
  deriving DecidableEq, Repr

/-- What a User's write that is not a stored edit comes to on a Vera home,
by what it carries. -/
def admitCarried (carrier : Carrier) (write : Write) (carried : Carried) (scope : Scope)
    (verdict : Verdict) : Admission :=
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

/-- What the runtime does with a write it is about to make in its own process
for the author of a command it verified. `provider` is the home's recorded
choice. Decision (gents#3831): a stored command carries a request-bound
delegation and a general one in it is refused, so a document never holds a
credential for anything but its own write. Decision (gents#3831): a stored
update or delete carries no credential at all, and whatever it carries is
not presented; the provider admits it on its signer's permission alone.
External premise (defradb.rs#2982): a provider that proves its decisions
admits a verified update or delete made with no credential when its signer
holds the `update` or `delete` permission on evidence at most 30 seconds old,
refuses it otherwise as any refused update or delete, and owes nothing for
it. Decision (gents#3831): any other write for a User that carries no
delegation is refused, never made under another authority; an identity token
is no delegation.
Admission comes before the command's nonce or receipt is consumed and before
anything is written, so a refused command leaves no write and is answered by
signing a new one. Decision (gents#3831): the runtime answers a refused stored
command with a rejected receipt naming the refusal
(`Identity.SessionInputEdit.answer`), so its client learns why. -/
def admit (provider : Provider) (author : Author) (carrier : Carrier) (write : Write)
    (carried : Carried) (scope : Scope) (verdict : Verdict) : Admission :=
  match provider, author with
  | .nodeStore, _ => .unchanged
  | .embeddedVera, .node => .unchanged
  | .embeddedVera, .agent => .unchanged
  | .embeddedVera, .user =>
    if carrier = .stored ∧ write ≠ .create then .providerDecides
    else admitCarried carrier write carried scope verdict

def Admission.runs : Admission → Bool
  | .unchanged => true
  | .forwarded => true
  | .providerDecides => true
  | _ => false

/-- Whether an admission uses up the command's nonce. -/
def Admission.consumesCommand (admission : Admission) : Bool := admission.runs

def admitCommand (digest : Digest) (provider : Provider) (author : Author) (node : Audience)
    (now : Nat) (command : Command) (attached : Option Delegation) : Admission :=
  match attached with
  | some delegation =>
    admit provider author command.carrier command.kind (carried digest command attached)
      (scope command delegation) (verdict node now command.signer delegation)
  | none => admit provider author command.carrier command.kind .absent .general .refused

theorem admitCommand_stored_edit (digest : Digest) (node : Audience) (now : Nat)
    (command : Command) (attached : Option Delegation)
    (edit : command.carrier = .stored ∧ command.kind ≠ .create) :
    admitCommand digest .embeddedVera .user node now command attached
      = .providerDecides := by
  cases attached <;> simp only [admitCommand, admit, if_pos edit]

theorem admitCommand_attached (digest : Digest) (node : Audience) (now : Nat)
    (command : Command) (delegation : Delegation)
    (notEdit : ¬(command.carrier = .stored ∧ command.kind ≠ .create)) :
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
  simp only [admitCommand, admit, if_neg notEdit]
  by_cases named : command.delegationDigest = some (digest.of delegation)
  · by_cases another : scope command delegation = .anotherWrite
    · simp [admitCarried, carried, named, another]
    · by_cases general : scope command delegation = .general ∧ command.carrier = .stored
      · simp [admitCarried, carried, named, another, general]
      · by_cases update : scope command delegation = .thisWrite ∧ command.kind ≠ .create
        · simp [admitCarried, carried, named, another, general, update]
        · cases valid : delegation.validFor node now
          · simp [admitCarried, carried, verdict, named, another, general, update, valid]
          · by_cases own : delegation.issuer = command.signer
            · simp [admitCarried, carried, verdict, named, another, general, update,
                valid, own]
            · simp [admitCarried, carried, verdict, named, another, general, update,
                valid, own]
  · simp [admitCarried, carried, named]

theorem a_user_write_on_vera_runs_only_under_its_own_bound_current_delegation
    (digest : Digest) (node : Audience) (now : Nat) (command : Command)
    (attached : Option Delegation)
    (edit : ¬(command.carrier = .stored ∧ command.kind ≠ .create))
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
  | none => simp [admitCommand, admit, if_neg edit, admitCarried, Admission.runs] at h
  | some delegation =>
    rw [admitCommand_attached _ _ _ _ _ edit] at h
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

/-- A stored update or delete is left to the provider whatever it carries:
nothing it carries is presented. -/
theorem a_stored_update_or_delete_is_left_to_the_provider (write : Write) (carried : Carried)
    (scope : Scope) (verdict : Verdict) (notCreate : write ≠ .create) :
    admit .embeddedVera .user .stored write carried scope verdict = .providerDecides := by
  cases write <;> simp_all [admit]

/-- A write for a User that carries no delegation is refused wherever the
runtime makes it in its own process. -/
theorem a_user_write_without_a_delegation_is_refused (carrier : Carrier) (write : Write)
    (scope : Scope) (verdict : Verdict) (notEdit : ¬(carrier = .stored ∧ write ≠ .create)) :
    (admit .embeddedVera .user carrier write .absent scope verdict).runs = false := by
  cases carrier <;> cases write <;> cases scope <;> cases verdict <;> simp_all <;> decide

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
    (notEdit : ¬(other.carrier = .stored ∧ other.kind ≠ .create))
    (notNamed : other.delegationDigest ≠ some (digest.of delegation)) :
    admitCommand digest .embeddedVera .user node now other (some delegation)
      = .delegationNotBound := by
  rw [admitCommand_attached _ _ _ _ _ notEdit, if_neg notNamed]

/-- A command names at most one delegation: one bound to another delegation
does not admit this one. -/
theorem a_command_bound_to_another_delegation_refuses_this_one (digest : Digest)
    (node : Audience) (now : Nat) (command : Command) (named attached : Delegation)
    (notEdit : ¬(command.carrier = .stored ∧ command.kind ≠ .create))
    (names : command.delegationDigest = some (digest.of named)) (differs : named ≠ attached) :
    admitCommand digest .embeddedVera .user node now command (some attached)
      = .delegationNotBound := by
  apply a_lifted_delegation_is_refused _ _ _ _ _ notEdit
  rw [names]
  intro same
  exact differs (digest.injective _ _ (Option.some.inj same))

theorem a_general_delegation_in_a_stored_command_is_refused (digest : Digest) (node : Audience)
    (now : Nat) (command : Command) (delegation : Delegation)
    (stored : command.carrier = .stored) (general : delegation.request = none)
    (edit : ¬(command.carrier = .stored ∧ command.kind ≠ .create)) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  have unbound : scope command delegation = .general := by simp [scope, general]
  rw [admitCommand_attached _ _ _ _ _ edit]
  by_cases named : command.delegationDigest = some (digest.of delegation)
  · simp [named, unbound, stored, Admission.runs]
  · simp [named, Admission.runs]

theorem a_delegation_for_another_write_is_refused (digest : Digest) (node : Audience)
    (now : Nat) (command : Command) (delegation : Delegation) (request : String)
    (names : delegation.request = some request) (another : request ≠ command.write)
    (edit : ¬(command.carrier = .stored ∧ command.kind ≠ .create)) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  have elsewhere : scope command delegation = .anotherWrite := by simp [scope, names, another]
  rw [admitCommand_attached _ _ _ _ _ edit]
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
    (invalid : delegation.validFor node now = false)
    (edit : ¬(command.carrier = .stored ∧ command.kind ≠ .create)) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  rw [admitCommand_attached _ _ _ _ _ edit]
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
    (command : Command) (delegation : Delegation) (expired : delegation.expires < now)
    (edit : ¬(command.carrier = .stored ∧ command.kind ≠ .create)) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  refine an_invalid_delegation_fails_closed digest node now command delegation ?_ edit
  have stale : delegation.currentAt now = false := by
    simp only [Delegation.currentAt, Bool.and_eq_false_iff, decide_eq_false_iff_not]
    omega
  simp [Delegation.validFor, stale]

theorem a_delegation_for_another_node_fails_closed (digest : Digest) (node : Audience) (now : Nat)
    (command : Command) (delegation : Delegation) (elsewhere : delegation.audience ≠ node)
    (edit : ¬(command.carrier = .stored ∧ command.kind ≠ .create)) :
    (admitCommand digest .embeddedVera .user node now command (some delegation)).runs = false := by
  refine an_invalid_delegation_fails_closed digest node now command delegation ?_ edit
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
    (minted : mint (.client user) user node now none (some write) = some delegation) :
    admitCommand digest .embeddedVera .user node now
      { signer := user, carrier, kind := .create, write,
        delegationDigest := some (digest.of delegation) }
      (some delegation) = .forwarded := by
  unfold mint at minted
  split at minted
  · cases minted
    rw [admitCommand_attached _ _ _ _ _ (by simp)]
    simp [scope, Delegation.validFor, Delegation.currentAt, issueAt]
  · cases minted

/-- A stored create carrying a delegation its client minted when it signed the
command is admitted whenever the runtime processes the command inside the
command's own validity, however wrong the client's clock: the client read its
home's status `trail` behind the provider's time, ran its own clock on by
`elapsed` before it minted, and the runtime processes the command `wait` later.
External premise: on a home whose Vera node runs in the runtime's process, the
runtime and the provider read one clock, and the provider judges the
delegation in a block proposed after the runtime submitted the command.
Decision (gents#3831): a registration or relationship change the
create still owes when its delegation lapses waits for authority, and the
client settles it in the background under a general delegation
(`Identity.OwedRegistration.handling`); the stored command is never signed
again and no one is asked. -/
theorem a_stored_create_is_admitted_through_its_window (digest : Digest) (node : Audience)
    (user write : String) (providerAtRead clockAtRead elapsed trail wait : Nat)
    (delegation : Delegation)
    (minted : mint (.client user) user node (clockAtRead + elapsed)
      (some ⟨providerAtRead, clockAtRead⟩) (some write) = some delegation)
    (trails : trail ≤ idleTrailSeconds) (within : wait ≤ storedCommandLifetimeSeconds) :
    admitCommand digest .embeddedVera .user node (providerAtRead + trail + elapsed + wait)
      { signer := user, carrier := .stored, kind := .create, write,
        delegationDigest := some (digest.of delegation) }
      (some delegation) = .forwarded := by
  unfold mint at minted
  split at minted
  · cases minted
    rw [admitCommand_attached _ _ _ _ _ (by simp)]
    simp only [storedCommandLifetimeSeconds, idleTrailSeconds] at within trails
    simp [scope, Delegation.validFor, Delegation.currentAt, requestBoundLifetimeSeconds, issueAt,
      backdateSeconds]
    omega
  · cases minted

/-- The policy commands a stored create sends for its User: the registration
of the document it creates, then each relationship it writes on it as that
User. Decision (gents#3831): the command carries one request-bound delegation
for each, in this order (`covers`). -/
def storedCreateCommands (registration : String) (relationships : List String) : List String :=
  registration :: relationships

theorem a_stored_create_carries_one_delegation_per_command (registration : String)
    (relationships : List String) (delegations : List Delegation)
    (covered : covers (storedCreateCommands registration relationships) delegations = true) :
    delegations.length = relationships.length + 1 := by
  have := a_create_with_relationships_needs_a_delegation_for_each _ _ covered
  simpa [storedCreateCommands] using this

theorem node_and_agent_writes_are_unchanged (provider : Provider) (author : Author)
    (carrier : Carrier) (write : Write) (carried : Carried) (scope : Scope) (verdict : Verdict)
    (runtimeHeld : author ≠ .user) :
    admit provider author carrier write carried scope verdict = .unchanged := by
  cases provider <;> cases author <;> simp_all [admit]

theorem a_home_on_the_node_store_is_unchanged (author : Author) (carrier : Carrier)
    (write : Write) (carried : Carried) (scope : Scope) (verdict : Verdict) :
    admit .nodeStore author carrier write carried scope verdict = .unchanged := rfl

/-- A direct write the runtime makes in its own process for a request it
serves. The delegation the caller presented as the request's bearer is the
one the write carries: DefraDB authenticated the request by it, so it is
bound to the request as nothing else could be, and it is general. -/
def admitPresented (provider : Provider) (author : Author) (write : Write)
    (presented : Bool) (verdict : Verdict) : Admission :=
  admit provider author .direct write (if presented then .bound else .absent) .general verdict

theorem a_presented_delegation_runs_a_user_write_only_as_its_issuer (write : Write)
    (presented : Bool) (verdict : Verdict)
    (h : (admitPresented .embeddedVera .user write presented verdict).runs = true) :
    presented = true ∧ verdict = .issuedBySigner := by
  cases write <;> cases presented <;> cases verdict
    <;> simp_all [admitPresented, admit, admitCarried, Admission.runs]

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
    (changed : before ≠ after) (mintedFor : delegation.audience = before)
    (edit : ¬(command.carrier = .stored ∧ command.kind ≠ .create)) :
    (admitCommand digest .embeddedVera .user after now command (some delegation)).runs
      = false := by
  refine a_delegation_for_another_node_fails_closed digest after now command delegation ?_ edit
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

/-- What an authenticated request presents at the runtime's HTTP edge. -/
inductive Presented where
  | identityToken
  | delegation
  deriving DecidableEq, Repr

inductive Edge where
  /-- The request goes on as it did before this model; DefraDB judges a
  presented delegation. -/
  | unchanged
  /-- The request is answered with a refusal that tells its client to mint a
  delegation for the audience the status reports. -/
  | delegationRequired
  deriving DecidableEq, Repr

/-- What the runtime does with an authenticated request at its HTTP edge.
Decision (gents#3831): on a Vera home the runtime refuses a User's request
that presents an identity token, since a write under it would reach Vera with
no authority of that User. Decision: the edge refuses the request whole. It
does not tell a read from a write, which would take interpreting each body,
and a client that resolved its node (`resolve`) presents a delegation with
every request. A request presenting nothing is not the edge's to judge, so
the public status, which a client reads to resolve, stays open. Decision: the
edge knows the Node by its DID and takes any other caller of an identity
token as a User, since an Agent acts only in the runtime's own process. -/
def edge (provider : Provider) (author : Author) (presented : Presented) : Edge :=
  match provider, author, presented with
  | .embeddedVera, .user, .identityToken => .delegationRequired
  | _, _, _ => .unchanged

theorem a_user_identity_token_is_refused_on_a_vera_home :
    edge .embeddedVera .user .identityToken = .delegationRequired := rfl

theorem the_edge_of_a_home_on_the_node_store_is_unchanged (author : Author)
    (presented : Presented) : edge .nodeStore author presented = .unchanged := by
  cases author <;> cases presented <;> rfl

theorem the_edge_leaves_runtime_held_callers_unchanged (provider : Provider) (author : Author)
    (presented : Presented) (runtimeHeld : author ≠ .user) :
    edge provider author presented = .unchanged := by
  cases provider <;> cases author <;> cases presented <;> simp_all [edge]

/-- What a client's credential is at the HTTP edge. -/
def Credential.presents : Credential → Option Presented
  | .identityToken => some .identityToken
  | .delegation => some .delegation
  | _ => none

/-- Mixed versions: a client that resolves no audience, as one older than this
model does, mints an identity token, and a Vera home refuses it at the edge. -/
theorem a_client_that_resolved_no_audience_is_refused_on_a_vera_home :
    (credential false true true false).presents.map (edge .embeddedVera .user)
      = some .delegationRequired := rfl

theorem a_client_that_resolved_its_audience_passes_the_edge :
    (credential false true true true).presents.map (edge .embeddedVera .user)
      = some .unchanged := rfl

theorem a_client_of_a_home_on_the_node_store_passes_the_edge (nodeNamesAudience : Bool) :
    (credential false true true nodeNamesAudience).presents.map (edge .nodeStore .user)
      = some .unchanged := by
  cases nodeNamesAudience <;> rfl

end Identity.UserDelegation
