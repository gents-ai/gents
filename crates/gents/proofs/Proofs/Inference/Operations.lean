import Proofs.Documents.Configuration

/-! The inference operations every client calls: writing a backend or a
profile, choosing an account, and model discovery. One handler serves the CLI,
the desktop and an Agent's config tool, so the only thing that changes a
decision here is who signs the transaction. DefraDB document ACP authorizes
the write itself; nothing here grants access. -/

namespace InferenceOperations

open Configuration (BackendAuth)

/-- Whose rules an operation runs under. `agent` is the restrictive set and
the default: the Engineer administers its Node and is still an `agent` here,
because an Agent does not handle secrets, whatever it may administer. -/
inductive Actor where
  | person
  | agent
  deriving DecidableEq, Repr

/-- What signs for the transaction's actor. -/
inductive Signer where
  /-- A key the client itself holds for the acting DID, as the CLI and the
  desktop sign, or a request the runtime authenticated as signed by one. -/
  | clientHeld
  /-- The signer a runtime manages for one of its Agents. -/
  | managedAgent
  /-- Nothing: a read-only transaction such as a preview, an access with no
  identity, or one whose signer does not match its actor. -/
  | none
  deriving DecidableEq, Repr

/-- What the node knows of the DID the transaction acts as. -/
inductive ActingDid where
  /-- The transaction names no actor. -/
  | absent
  /-- The Node's own DID: the node key acts. -/
  | node
  | agentOfNode
  /-- Any other DID: a User, another node's Agent, or one the node has never
  seen. The DID alone does not tell these apart. -/
  | other
  deriving DecidableEq, Repr

/-- The actor kind, decided closed: the person rules need positive evidence,
a client-held signer for a DID that is neither the Node nor one of its Agents.
An Agent's key never leaves its runtime, so a client-held key is a person's.
Everything else, including whatever the handler cannot tell, gets the Agent
rules. -/
def actorKind (signer : Signer) (did : ActingDid) : Actor :=
  match signer, did with
  | .clientHeld, .other => .person
  | _, _ => .agent

theorem person_needs_evidence (signer : Signer) (did : ActingDid)
    (h : actorKind signer did = .person) : signer = .clientHeld ∧ did = .other := by
  cases signer <;> cases did <;> simp_all [actorKind]

theorem no_signer_is_an_agent (did : ActingDid) : actorKind .none did = .agent := by
  cases did <;> rfl

theorem node_key_is_an_agent (signer : Signer) : actorKind signer .node = .agent := by
  cases signer <;> rfl

theorem node_agent_is_an_agent (signer : Signer) : actorKind signer .agentOfNode = .agent := by
  cases signer <;> rfl

theorem unknown_did_without_client_key_is_an_agent (signer : Signer) (h : signer ≠ .clientHeld) :
    actorKind signer .other = .agent := by
  cases signer <;> simp_all [actorKind]

/-- The OAuth account a backend names; a non-OAuth backend names none. -/
def oauthRef : BackendAuth → Option String
  | .nodeOAuth a => a
  | _ => none

/-- What an Agent's patch may leave in a backend's `auth`. Environment
references are free. OAuth account references are a person's to choose: a
node-OAuth candidate keeps the stored reference and a non-OAuth backend counts
as the original account. A rotation pool and its overage choice are a person's
too: a candidate pool must equal the stored one. Raw secrets (an API key, a
stored credential, static cloud credentials) can be kept but never introduced
or changed. -/
def authPatchAllowed (stored candidate : BackendAuth) : Bool :=
  match candidate with
  | .apiKey _ => decide (candidate = stored)
  | .credential .. => decide (candidate = stored)
  | .awsCredentials .. => decide (candidate = stored)
  | .googleCredentials _ => decide (candidate = stored)
  | .nodeOAuth a => decide (a = oauthRef stored)
  | .nodeOAuthPool _ => decide (candidate = stored)
  | _ => true

/-- The account-choice fence for an Agent moving a profile onto backend
`next` from backend `current`. A backend is its provider kind and auth, and
`dflt` is each provider's default account. A profile may keep its account,
move to an account-free backend, or move to another provider's default
account; it never switches between two accounts of one provider and never
reaches a pool it does not already use. -/
def backendChoiceAllowed (dflt : String → Option String)
    (current : Option (String × BackendAuth)) (next : String × BackendAuth) : Bool :=
  match next.2 with
  | .nodeOAuth r =>
    match current with
    | some (kind, auth) =>
      if kind = next.1 then decide (auth = next.2) else decide (r = dflt next.1)
    | none => decide (r = dflt next.1)
  | .nodeOAuthPool _ => decide (current = some next)
  | _ => true

/-- What a backend's `auth` asks of the node, as far as these decisions read
it. -/
inductive AuthClass where
  /-- No credential, or a reference to an environment variable. -/
  | accountFree
  /-- An API key, a stored credential or static cloud credentials. -/
  | rawSecret
  | oauthAccount
  | oauthPool
  deriving DecidableEq, Repr

def AuthClass.of : BackendAuth → AuthClass
  | .unauthenticated | .environment _ => .accountFree
  | .apiKey _ | .credential .. | .awsCredentials .. | .googleCredentials _ => .rawSecret
  | .nodeOAuth _ => .oauthAccount
  | .nodeOAuthPool _ => .oauthPool

/-- Whether the candidate `auth` keeps what the stored backend holds: the same
account reference for a node-OAuth candidate, the same value otherwise. -/
def keepsAuth (stored candidate : BackendAuth) : Bool :=
  match candidate with
  | .nodeOAuth a => decide (a = oauthRef stored)
  | _ => decide (candidate = stored)

/-- The handler's answer to a backend write. Every refusal is a rule about an
Agent actor; a person is admitted and DefraDB then authorizes the write. -/
inductive BackendDecision where
  | admitted
  /-- An Agent may not introduce or change a raw secret. -/
  | rawCredential
  /-- An Agent may not choose the OAuth account a backend uses. -/
  | oauthAccount
  /-- An Agent may not introduce a rotation pool or change its overage. -/
  | oauthPool
  /-- An Agent creates only unauthenticated OpenAI-compatible backends. -/
  | localServerOnly
  /-- A backend an Agent creates starts enabled, so discovery can reach it. -/
  | startsEnabled
  deriving DecidableEq, Repr

/-- An edit of an existing backend, whichever client sends the patch. -/
def backendUpdateDecision (actor : Actor) (auth : AuthClass) (keeps : Bool) : BackendDecision :=
  match actor with
  | .person => .admitted
  | .agent =>
    if keeps then .admitted
    else match auth with
      | .accountFree => .admitted
      | .rawSecret => .rawCredential
      | .oauthAccount => .oauthAccount
      | .oauthPool => .oauthPool

/-- A new backend. `localServer` says the candidate is an OpenAI-compatible
endpoint with no authentication. -/
def backendCreateDecision (actor : Actor) (localServer enabled : Bool) : BackendDecision :=
  match actor with
  | .person => .admitted
  | .agent =>
    if !localServer then .localServerOnly
    else if !enabled then .startsEnabled
    else .admitted

theorem person_writes_any_backend (auth : AuthClass) (keeps localServer enabled : Bool) :
    backendUpdateDecision .person auth keeps = .admitted ∧
      backendCreateDecision .person localServer enabled = .admitted := ⟨rfl, rfl⟩

/-- The finite table is the auth fence: an Agent's edit is admitted exactly
when `authPatchAllowed` holds of the stored and candidate auth. -/
theorem agent_update_is_auth_fence (stored candidate : BackendAuth) :
    backendUpdateDecision .agent (AuthClass.of candidate) (keepsAuth stored candidate) = .admitted ↔
      authPatchAllowed stored candidate = true := by
  cases candidate <;>
    simp [backendUpdateDecision, AuthClass.of, keepsAuth, authPatchAllowed]

theorem agent_never_introduces_a_secret (stored candidate : BackendAuth)
    (hc : AuthClass.of candidate = .rawSecret)
    (h : backendUpdateDecision .agent (AuthClass.of candidate) (keepsAuth stored candidate) = .admitted) :
    candidate = stored := by
  cases candidate <;> simp_all [backendUpdateDecision, AuthClass.of, keepsAuth]

theorem agent_creates_only_local_servers (enabled : Bool) :
    backendCreateDecision .agent false enabled = .localServerOnly := rfl

/-- How the backend a profile would move to relates to the one it uses. -/
inductive CurrentBackend where
  /-- The profile is new: it uses no backend yet. -/
  | absent
  /-- The same provider kind and the same auth. -/
  | same
  /-- The same provider kind with different auth. -/
  | sameProvider
  | otherProvider
  deriving DecidableEq, Repr

def CurrentBackend.of (current : Option (String × BackendAuth)) (next : String × BackendAuth) :
    CurrentBackend :=
  match current with
  | none => .absent
  | some (kind, auth) =>
    if kind = next.1 then (if auth = next.2 then .same else .sameProvider) else .otherProvider

/-- The handler's answer to the backend a profile write selects. -/
inductive ChoiceDecision where
  | admitted
  /-- An Agent may not move a profile to another account. -/
  | otherAccount
  /-- An Agent may not move a profile onto a pool it does not use. -/
  | otherPool
  deriving DecidableEq, Repr

/-- The account-choice decision. `next` is the class of the selected backend's
auth and `isDefault` says its account is its provider's default. -/
def profileChoiceDecision (actor : Actor) (next : AuthClass) (current : CurrentBackend)
    (isDefault : Bool) : ChoiceDecision :=
  match actor with
  | .person => .admitted
  | .agent =>
    match next with
    | .oauthAccount =>
      match current with
      | .same => .admitted
      | .sameProvider => .otherAccount
      | .absent | .otherProvider => if isDefault then .admitted else .otherAccount
    | .oauthPool => if current = .same then .admitted else .otherPool
    | .accountFree | .rawSecret => .admitted

theorem person_selects_any_account (next : AuthClass) (current : CurrentBackend)
    (isDefault : Bool) : profileChoiceDecision .person next current isDefault = .admitted := rfl

/-- Whether `next` names its provider's default account. -/
def namesDefault (dflt : String → Option String) (next : String × BackendAuth) : Bool :=
  match next.2 with
  | .nodeOAuth r => decide (r = dflt next.1)
  | _ => false

/-- The finite table is the account-choice fence. -/
theorem agent_choice_is_account_fence (dflt : String → Option String)
    (current : Option (String × BackendAuth)) (next : String × BackendAuth) :
    profileChoiceDecision .agent (AuthClass.of next.2) (CurrentBackend.of current next)
        (namesDefault dflt next) = .admitted ↔
      backendChoiceAllowed dflt current next = true := by
  obtain ⟨nextKind, nextAuth⟩ := next
  cases current with
  | none =>
    cases nextAuth <;>
      simp [profileChoiceDecision, AuthClass.of, CurrentBackend.of, namesDefault,
        backendChoiceAllowed]
  | some c =>
    obtain ⟨kind, auth⟩ := c
    by_cases hk : kind = nextKind
    · subst hk
      by_cases ha : auth = nextAuth
      · subst ha
        cases auth <;>
          simp [profileChoiceDecision, AuthClass.of, CurrentBackend.of, namesDefault,
            backendChoiceAllowed]
      · cases nextAuth <;>
          simp [profileChoiceDecision, AuthClass.of, CurrentBackend.of, namesDefault,
            backendChoiceAllowed, ha]
    · cases nextAuth <;>
        simp [profileChoiceDecision, AuthClass.of, CurrentBackend.of, namesDefault,
          backendChoiceAllowed, hk]

/-- How a caller names the profile it edits: through the Agent that selects
it, or by the profile's own ID. -/
inductive ProfileRoute where
  | anchored
  | exactId
  deriving DecidableEq, Repr

/-- The decision a profile write gets on a route. A new profile
(`CurrentBackend.absent`) is a create on either route. The route is an input
so that every client path is replayed against the one decision. -/
def profileWriteDecision (route : ProfileRoute) (actor : Actor) (next : AuthClass)
    (current : CurrentBackend) (isDefault : Bool) : ChoiceDecision :=
  match route with
  | .anchored | .exactId => profileChoiceDecision actor next current isDefault

/-- The fence does not depend on how the profile was named: an exact-ID edit
is decided as the anchored edit is. -/
theorem fence_holds_on_every_route (route route' : ProfileRoute) (actor : Actor)
    (next : AuthClass) (current : CurrentBackend) (isDefault : Bool) :
    profileWriteDecision route actor next current isDefault =
      profileWriteDecision route' actor next current isDefault := by
  cases route <;> cases route' <;> rfl

theorem agent_never_switches_accounts_of_one_provider (route : ProfileRoute) (isDefault : Bool) :
    profileWriteDecision route .agent .oauthAccount .sameProvider isDefault = .otherAccount := by
  cases route <;> rfl

/-- What model discovery does for a stored backend. -/
inductive DiscoveryAnswer where
  /-- Ask the provider and record what it advertises. -/
  | contactsProvider
  /-- Answer with the observation already stored; the provider is not asked. -/
  | storedObservation
  deriving DecidableEq, Repr

/-- `credentialed` says the backend is anything but an unauthenticated
OpenAI-compatible endpoint. An Agent never causes a credential to be sent. -/
def discoveryAnswer (actor : Actor) (credentialed : Bool) : DiscoveryAnswer :=
  match actor, credentialed with
  | .agent, true => .storedObservation
  | _, _ => .contactsProvider

theorem agent_never_contacts_a_credentialed_provider :
    discoveryAnswer .agent true = .storedObservation := rfl

/-- The operations on a node's signed-in accounts and on which of them a
backend or profile uses. -/
inductive AccountOperation where
  | list
  /-- Move a profile onto the backend of a chosen account. -/
  | selectForProfile
  /-- Switch a backend between one account and the pool. -/
  | setPool
  | label
  | disable
  | remove
  deriving DecidableEq, Repr

/-- Accounts are a person's: an Agent reads the inventory and nothing else. -/
def accountOperationAdmitted (actor : Actor) (operation : AccountOperation) : Bool :=
  match actor, operation with
  | .person, _ => true
  | .agent, .list => true
  | .agent, _ => false

theorem agent_changes_no_account (operation : AccountOperation)
    (h : accountOperationAdmitted .agent operation = true) : operation = .list := by
  cases operation <;> simp_all [accountOperationAdmitted]

/-- No input lacking the person evidence is admitted to an account change. -/
theorem account_change_needs_person_evidence (signer : Signer) (did : ActingDid)
    (operation : AccountOperation) (hop : operation ≠ .list)
    (h : accountOperationAdmitted (actorKind signer did) operation = true) :
    signer = .clientHeld ∧ did = .other := by
  apply person_needs_evidence
  cases hk : actorKind signer did with
  | person => rfl
  | agent => rw [hk] at h; exact absurd (agent_changes_no_account operation h) hop

/-- No input lacking the person evidence introduces or changes a secret, an
OAuth account or a pool on a backend. -/
theorem credential_write_needs_person_evidence (signer : Signer) (did : ActingDid)
    (auth : AuthClass) (hauth : auth ≠ .accountFree)
    (h : backendUpdateDecision (actorKind signer did) auth false = .admitted) :
    signer = .clientHeld ∧ did = .other := by
  apply person_needs_evidence
  cases hk : actorKind signer did with
  | person => rfl
  | agent => rw [hk] at h; cases auth <;> simp_all [backendUpdateDecision]

/-- No input lacking the person evidence creates anything but a local server. -/
theorem credentialed_create_needs_person_evidence (signer : Signer) (did : ActingDid)
    (enabled : Bool) (h : backendCreateDecision (actorKind signer did) false enabled = .admitted) :
    signer = .clientHeld ∧ did = .other := by
  apply person_needs_evidence
  cases hk : actorKind signer did with
  | person => rfl
  | agent => rw [hk] at h; simp [backendCreateDecision] at h

/-- No input lacking the person evidence moves a profile to another account
of its provider. -/
theorem account_switch_needs_person_evidence (signer : Signer) (did : ActingDid)
    (route : ProfileRoute) (isDefault : Bool)
    (h : profileWriteDecision route (actorKind signer did) .oauthAccount .sameProvider isDefault =
      .admitted) :
    signer = .clientHeld ∧ did = .other := by
  apply person_needs_evidence
  cases hk : actorKind signer did with
  | person => rfl
  | agent => rw [hk] at h; cases route <;> simp [profileWriteDecision, profileChoiceDecision] at h

/-- No input lacking the person evidence contacts a credentialed provider. -/
theorem credentialed_contact_needs_person_evidence (signer : Signer) (did : ActingDid)
    (h : discoveryAnswer (actorKind signer did) true = .contactsProvider) :
    signer = .clientHeld ∧ did = .other := by
  apply person_needs_evidence
  cases hk : actorKind signer did with
  | person => rfl
  | agent => rw [hk] at h; simp [discoveryAnswer] at h

end InferenceOperations
