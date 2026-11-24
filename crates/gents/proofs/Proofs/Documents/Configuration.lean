import Proofs.Documents.ConfigDefaults
import Proofs.Documents.Configuration.PackIdentity
import Mathlib.Data.Rat.Defs

/-!
# Configuration: canonical reference chain and install ownership

Executable formal model of the configuration refactor contracts:

* Install scope fills omitted owners and rejects mismatched owners, while
  preserving explicit foreign delegation identities verbatim. Owners are
  string DIDs (`node_did`); no separate node schema is introduced.
* A agent composes context and inference into one session configuration.
  `Task -> Agent -> {Context, InferenceProfile -> Backend}` resolution fails
  closed on missing or foreign references and disabled executable documents.
  `resolveTask` is the single Task resolver (owner-checked Task record).
* Discovery observations cannot rewrite the selected model/effort.
* Absent context means no instructions, no skills, no tools, and runtime-default
  compaction; absent compaction selection means `StripThenSummarize` at 3/4.
-/

namespace Configuration

/-- JSON parser details are outside the model. Preserve the distinction needed
for compact authoring contracts; strict required fields have no default. -/
inductive AuthoredField (α : Type) where
  | omitted
  | null
  | value (v : α)
  deriving DecidableEq, Repr

def resolveDefault {α : Type} (fallback : α) : AuthoredField α → α
  | .omitted | .null => fallback
  | .value v => v

def compact {α : Type} [DecidableEq α] (fallback value : α) : AuthoredField α :=
  if value = fallback then .omitted else .value value

theorem compact_roundtrip {α : Type} [DecidableEq α] (fallback value : α) :
    resolveDefault fallback (compact fallback value) = value := by
  by_cases h : value = fallback <;> simp [compact, resolveDefault, h]

theorem omitted_null_same_default {α : Type} (fallback : α) :
    resolveDefault fallback .omitted = resolveDefault fallback .null := rfl

/-- Canonical effort vocabulary. Omission means provider default; explicit
`none` remains a distinct selection. -/
inductive ReasoningEffort where
  | none | minimal | low | medium | high | xhigh | max | ultra
  deriving DecidableEq, Repr

def fillOwner (scope : String) (declared : Option String) : Option String :=
  match declared with
  | none => some scope
  | some d => if d = scope then some d else none

@[simp] theorem fillOwner_ok_iff (scope : String) (declared : Option String) (owner : String) :
    fillOwner scope declared = some owner ↔
      owner = scope ∧ (declared = none ∨ declared = some scope) := by
  cases declared with
  | none => simp [fillOwner, eq_comm]
  | some d => by_cases h : d = scope <;> simp_all [fillOwner, eq_comm]

/-- Every installed document has one owner. Payloads project only the fields
needed by resolution, keeping impossible split-field registry states out. -/
structure Owned (α : Type) where
  owner : String
  value : α
  deriving DecidableEq, Repr

/-- Installation fills only the root owner. The canonical payload is unchanged,
including foreign delegation identities and provenance; no second document shape. -/
def installOwned {α : Type} (scope : String) (declared : Option String) (payload : α) :
    Option (Owned α) :=
  (fillOwner scope declared).map fun owner => ⟨owner, payload⟩

theorem installOwned_ok_iff {α : Type} (scope : String) (declared : Option String)
    (payload : α) (installed : Owned α) :
    installOwned scope declared payload = some installed ↔
      installed = ⟨scope, payload⟩ ∧ (declared = none ∨ declared = some scope) := by
  cases declared with
  | none => simp [installOwned, fillOwner, eq_comm]
  | some d => by_cases h : d = scope <;> simp_all [installOwned, fillOwner, eq_comm]

/-- The model/effort selected through the only legal path. -/
structure SelectedModel where
  backendId : String
  model : String
  effort : Option ReasoningEffort
deriving DecidableEq, Repr

/-- Canonical persisted effort names, the only spelling the runtime parses or
writes. -/
def ReasoningEffort.wireName : ReasoningEffort → String
  | .none => "none"
  | .minimal => "minimal"
  | .low => "low"
  | .medium => "medium"
  | .high => "high"
  | .xhigh => "xhigh"
  | .max => "max"
  | .ultra => "ultra"

/-- Capability projection of one advertised model. Unknown efforts and a known
empty effort catalog remain distinct, matching AdvertisedModel. -/
structure AdvertisedModel where
  modelName : String
  reasoningEfforts : Option (List ReasoningEffort)
  deriving DecidableEq, Repr

/-- Projection of BackendModelCatalog. None is shared backend credentials;
Some DID is the node whose OAuth credential performed discovery. -/
structure DiscoveryObservation where
  /-- Backend identity inherited from the containing backend observation. -/
  backendId : String
  nodeDid : Option String
  models : List AdvertisedModel
  deriving DecidableEq, Repr

/-- The existing Owned wrapper carries the containing backend document's owner,
not another catalog field. Shared credentials (`none`) do not erase backend
ownership. Discovery cannot rewrite inference selection. -/
def catalogFor (owner backendId : String) (credentialScope : Option String)
    (observation : Option (Owned DiscoveryObservation)) : Option DiscoveryObservation :=
  (observation.filter fun catalog => catalog.owner == owner &&
    catalog.value.nodeDid == credentialScope && catalog.value.backendId == backendId).map (·.value)

theorem catalogFor_some_iff (owner backendId : String) (scope : Option String)
    (observation : Option (Owned DiscoveryObservation)) (catalog : DiscoveryObservation) :
    catalogFor owner backendId scope observation = some catalog ↔
      observation = some ⟨owner, catalog⟩ ∧ catalog.nodeDid = scope ∧ catalog.backendId = backendId := by
  cases observation with
  | none => simp [catalogFor, Option.filter]
  | some observed =>
    cases observed with
    | mk observedOwner value =>
      by_cases ho : observedOwner = owner <;>
        by_cases hs : value.nodeDid = scope <;>
        by_cases hb : value.backendId = backendId <;>
        simp_all [catalogFor, Option.filter, Owned.mk.injEq] <;> aesop

/-- Durable strategies; internal reduction modes stay in the compaction owner. -/
inductive CompactionStrategy where
  | stripToolResults
  | stripThenSummarize
deriving DecidableEq, Repr

/-- Resolved compaction projection; authored strategy uses the generic default decoder. -/
structure CompactionConfig where
  strategy : CompactionStrategy
  threshold : Rat
deriving DecidableEq, Repr

def defaultCompaction : CompactionConfig :=
  ⟨.stripThenSummarize, (3 / 4 : Rat)⟩

def resolveCompaction (strategy : AuthoredField CompactionStrategy) (threshold : Option Rat) :
    CompactionConfig :=
  ⟨resolveDefault defaultCompaction.strategy strategy, threshold.getD defaultCompaction.threshold⟩

theorem explicit_compaction_omitted_defaults :
    resolveCompaction .omitted none = defaultCompaction := rfl

theorem explicit_compaction_null_defaults :
    resolveCompaction .null none = defaultCompaction := rfl

/-- Content projection after the loader resolves same-owner context references.
The loader must preserve literal prompt text and the explicit skill/tool selections;
publication checks their reference ownership. This is not an authored schema copy. -/
structure Context where
  instructions : String := ""
  skillIds : List String := []
  toolNames : List String := []
  compaction : CompactionConfig := defaultCompaction
  deriving DecidableEq, Repr

def defaultContext : Context := {}

/-- A session derives both its context and inference from one agent. There
is no independent context defaulting or model selection path. -/
structure ResolvedSessionConfig where
  context : Context
  inference : SelectedModel
  deriving DecidableEq, Repr

structure Agent where
  contextId : Option String
  profileId : String
  enabled : Bool
  deriving DecidableEq, Repr

structure Backend where
  enabled : Bool
  deriving DecidableEq, Repr

structure Task where
  agentDid : String
  enabled : Bool
  deriving DecidableEq, Repr

/-- The profile projection is its required inference selection. Context payloads
have already had their nested same-owner references resolved by the loader.
Lookup keys include node_did and the authored logical ID. Identical labels
can coexist across nodes; returned ownership is still checked against the key. -/
structure Registry where
  tasks : String → String → Option (Owned Task)
  agents : String → String → Option (Owned Agent)
  contexts : String → String → Option (Owned Context)
  profiles : String → String → Option (Owned SelectedModel)
  backends : String → String → Option (Owned Backend)

inductive ResolveError where
  | missingAgent | disabledAgent | foreignAgent
  | missingContext | foreignContext
  | missingProfile | foreignProfile
  | missingBackend | disabledBackend | foreignBackend
  | missingTask | disabledTask | foreignTask
  | missingCapability | callerNotAllowed
  | missingPlugin | pluginDigestMismatch
  deriving DecidableEq, Repr

def lookupOwned {α : Type} (docs : String → String → Option (Owned α)) (scope id : String)
    (missing foreign : ResolveError) : Except ResolveError α :=
  match docs scope id with
  | none => .error missing
  | some doc => if doc.owner = scope then .ok doc.value else .error foreign

@[simp] theorem lookupOwned_ok_iff {α : Type} (docs : String → String → Option (Owned α))
    (scope id : String) (missing foreign : ResolveError) (value : α) :
    lookupOwned docs scope id missing foreign = .ok value ↔
      docs scope id = some ⟨scope, value⟩ := by
  cases hd : docs scope id with
  | none => simp [lookupOwned, hd]
  | some doc =>
    cases doc with
    | mk owner payload =>
      by_cases ho : owner = scope <;> simp [lookupOwned, hd, ho, Owned.mk.injEq]

@[simp] theorem resolution_bind_ok_iff {α β : Type} (result : Except ResolveError α)
    (next : α → Except ResolveError β) (value : β) :
    result.bind next = .ok value ↔ ∃ x, result = .ok x ∧ next x = .ok value := by
  cases result <;> simp [Except.bind]

/-- Only absence of a reference invokes defaults. A selected but missing/foreign
context must fail, rather than silently lose tools, skills, or instructions. -/
def resolveContext (reg : Registry) (scope : String) : Option String → Except ResolveError Context
  | none => .ok defaultContext
  | some id => lookupOwned reg.contexts scope id .missingContext .foreignContext

@[simp] theorem resolveContext_ok_iff (reg : Registry) (scope : String)
    (id : Option String) (context : Context) :
    resolveContext reg scope id = .ok context ↔
      match id with
      | none => context = defaultContext
      | some key => reg.contexts scope key = some ⟨scope, context⟩ := by
  cases id <;> simp [resolveContext, eq_comm]

def resolveAgent (reg : Registry) (scope agentDid : String) :
    Except ResolveError ResolvedSessionConfig := do
  let agent ← lookupOwned reg.agents scope agentDid .missingAgent .foreignAgent
  if !agent.enabled then .error .disabledAgent else do
    let context ← resolveContext reg scope agent.contextId
    let inference ← lookupOwned reg.profiles scope agent.profileId .missingProfile .foreignProfile
    let backend ← lookupOwned reg.backends scope inference.backendId .missingBackend .foreignBackend
    if !backend.enabled then .error .disabledBackend
    else .ok ⟨context, inference⟩

/-- The single Task entrance supplies the same combined configuration to every
request. No TaskRun or session lifecycle is introduced here. -/
def resolveTask (reg : Registry) (scope taskId : String) :
    Except ResolveError ResolvedSessionConfig := do
  let task ← lookupOwned reg.tasks scope taskId .missingTask .foreignTask
  if !task.enabled then .error .disabledTask
  else resolveAgent reg scope task.agentDid

theorem resolveAgent_ok_iff (reg : Registry) (scope agentDid : String)
    (session : ResolvedSessionConfig) :
    resolveAgent reg scope agentDid = .ok session ↔
      ∃ agent backend, reg.agents scope agentDid = some ⟨scope, agent⟩ ∧
        agent.enabled = true ∧
        resolveContext reg scope agent.contextId = .ok session.context ∧
        reg.profiles scope agent.profileId = some ⟨scope, session.inference⟩ ∧
        reg.backends scope session.inference.backendId = some ⟨scope, backend⟩ ∧
        backend.enabled = true := by
  cases session with
  | mk context inference =>
    simp [resolveAgent, bind, resolution_bind_ok_iff, ite_eq_iff]
    aesop

theorem resolveTask_ok_iff (reg : Registry) (scope taskId : String)
    (session : ResolvedSessionConfig) :
    resolveTask reg scope taskId = .ok session ↔
      ∃ task, reg.tasks scope taskId = some ⟨scope, task⟩ ∧ task.enabled = true ∧
        resolveAgent reg scope task.agentDid = .ok session := by
  simp [resolveTask, bind, resolution_bind_ok_iff, ite_eq_iff]

/-- Omission removes capabilities rather than inheriting another agent's
context. This applies to successful session resolution, not just a default value. -/
theorem omitted_agent_context_is_empty (reg : Registry) (scope id : String)
    (agent : Agent) (session : ResolvedSessionConfig)
    (hb : reg.agents scope id = some ⟨scope, agent⟩) (ho : agent.contextId = none)
    (h : resolveAgent reg scope id = .ok session) : session.context = defaultContext := by
  obtain ⟨found, backend, hf, _, hc, _⟩ := (resolveAgent_ok_iff reg scope id session).mp h
  have he : found = agent := by simpa [hb] using hf.symm
  subst found
  simpa [resolveContext, ho] using hc.symm

theorem resolveTask_owned_delegates (reg : Registry) (scope id : String) (task : Task)
    (ht : reg.tasks scope id = some ⟨scope, task⟩) (he : task.enabled = true) :
    resolveTask reg scope id = resolveAgent reg scope task.agentDid := by
  simp [resolveTask, lookupOwned, ht, he, bind, Except.bind]

theorem resolveTask_missing_rejected (reg : Registry) (scope id : String)
    (ht : reg.tasks scope id = none) : resolveTask reg scope id = .error .missingTask := by
  simp [resolveTask, lookupOwned, ht, bind, Except.bind]

theorem resolveTask_foreign_owner (reg : Registry) (scope id : String) (doc : Owned Task)
    (ht : reg.tasks scope id = some doc) (ho : doc.owner ≠ scope) :
    resolveTask reg scope id = .error .foreignTask := by
  simp [resolveTask, lookupOwned, ht, ho, bind, Except.bind]

theorem disabled_owned_task_rejected (reg : Registry) (scope id : String) (task : Task)
    (ht : reg.tasks scope id = some ⟨scope, task⟩) (he : task.enabled = false) :
    resolveTask reg scope id = .error .disabledTask := by
  simp [resolveTask, lookupOwned, ht, he, bind, Except.bind]

theorem disabled_agent_rejected (reg : Registry) (scope id : String) (agent : Agent)
    (hb : reg.agents scope id = some ⟨scope, agent⟩) (he : agent.enabled = false) :
    resolveAgent reg scope id = .error .disabledAgent := by
  simp [resolveAgent, lookupOwned, hb, he, bind, Except.bind]

/-- A node's default agent selection (`default_agent_did` on its one
`DefaultAgentSelection` document) selects the agent for requests that
name none, so a default must be an owned, enabled agent. A disabled default
would leave every such request to fail with `disabledAgent`; publication
(`ConfigReferences`) rejects the candidate instead, whichever write disables the
default or points the default at a disabled agent. Clients that couple the
Default and Enabled controls only guide input toward this rule. -/
def defaultAgentPublishable (reg : Registry) (scope : String) : Option String → Bool
  | none => true
  | some id =>
    match lookupOwned reg.agents scope id .missingAgent .foreignAgent with
    | .ok agent => agent.enabled
    | .error _ => false

/-- Bootstrap may publish a node before selecting its first agent.
Once configured, clearing that selection prevents runtime readiness on the next
start. Full replacements must name a default explicitly; omission is not a patch.
Reference validity remains with `defaultAgentPublishable`. -/
def defaultAgentReplacementAllowed (current candidate : Option String) : Bool :=
  current.isNone || candidate.isSome

theorem configured_default_cannot_be_cleared (current : String) :
    defaultAgentReplacementAllowed (some current) none = false := by rfl

theorem bootstrap_default_may_be_omitted :
    defaultAgentReplacementAllowed none none = true := by rfl

theorem explicit_default_replacement_allowed (current : Option String) (candidate : String) :
    defaultAgentReplacementAllowed current (some candidate) = true := by
  simp [defaultAgentReplacementAllowed]

theorem disabled_default_not_publishable (reg : Registry) (scope id : String)
    (agent : Agent) (hb : reg.agents scope id = some ⟨scope, agent⟩)
    (he : agent.enabled = false) :
    defaultAgentPublishable reg scope (some id) = false := by
  simp [defaultAgentPublishable, lookupOwned, hb, he]

private theorem lookupOwned_error_cases {α : Type} {docs : String → String → Option (Owned α)}
    {scope id : String} {missing foreign e : ResolveError}
    (h : lookupOwned docs scope id missing foreign = .error e) : e = missing ∨ e = foreign := by
  unfold lookupOwned at h
  split at h
  · exact Or.inl (Except.error.inj h).symm
  · split at h
    · contradiction
    · exact Or.inr (Except.error.inj h).symm

theorem publishable_default_not_disabled (reg : Registry) (scope id : String)
    (h : defaultAgentPublishable reg scope (some id) = true) :
    resolveAgent reg scope id ≠ .error .disabledAgent := by
  cases hl : lookupOwned reg.agents scope id .missingAgent .foreignAgent with
  | error e => simp [defaultAgentPublishable, hl] at h
  | ok agent =>
    have he : agent.enabled = true := by simpa [defaultAgentPublishable, hl] using h
    intro hr
    simp only [resolveAgent, hl, he, bind, Except.bind] at hr
    cases hc : resolveContext reg scope agent.contextId with
    | error e =>
      simp only [hc, Bool.not_true, Bool.false_eq_true, ↓reduceIte, Except.error.injEq] at hr
      subst hr
      cases hid : agent.contextId with
      | none => simp [resolveContext, hid] at hc
      | some key =>
        simp only [resolveContext, hid] at hc
        rcases lookupOwned_error_cases hc with h | h <;> cases h
    | ok context =>
      simp only [hc] at hr
      cases hp : lookupOwned reg.profiles scope agent.profileId .missingProfile .foreignProfile with
      | error e =>
        simp only [hp, Bool.not_true, Bool.false_eq_true, ↓reduceIte, Except.error.injEq] at hr
        subst hr
        rcases lookupOwned_error_cases hp with h | h <;> cases h
      | ok inference =>
        simp only [hp] at hr
        cases hk : lookupOwned reg.backends scope inference.backendId .missingBackend .foreignBackend with
        | error e =>
          simp only [hk, Bool.not_true, Bool.false_eq_true, ↓reduceIte, Except.error.injEq] at hr
          subst hr
          rcases lookupOwned_error_cases hk with h | h <;> cases h
        | ok backend =>
          simp only [hk] at hr
          by_cases hb : backend.enabled = false <;> simp [hb] at hr

theorem agent_missing_selected_context_rejected (reg : Registry) (scope id key : String)
    (agent : Agent) (hb : reg.agents scope id = some ⟨scope, agent⟩)
    (he : agent.enabled = true) (hr : agent.contextId = some key)
    (hc : reg.contexts scope key = none) :
    resolveAgent reg scope id = .error .missingContext := by
  simp [resolveAgent, resolveContext, lookupOwned, hb, he, hr, hc, bind, Except.bind]

theorem selected_context_missing_rejected (reg : Registry) (scope id : String)
    (h : reg.contexts scope id = none) : resolveContext reg scope (some id) = .error .missingContext := by
  simp [resolveContext, lookupOwned, h]

theorem selected_context_foreign_rejected (reg : Registry) (scope id : String)
    (doc : Owned Context) (hd : reg.contexts scope id = some doc) (ho : doc.owner ≠ scope) :
    resolveContext reg scope (some id) = .error .foreignContext := by
  simp [resolveContext, lookupOwned, hd, ho]

theorem omitted_context_capabilities_empty :
    defaultContext.instructions = "" ∧ defaultContext.skillIds = [] ∧
      defaultContext.toolNames = [] ∧ defaultContext.compaction = defaultCompaction := by
  exact ⟨rfl, rfl, rfl, rfl⟩

/-- Auth is explicit. OAuth lookup uses the executing node and existing
credential owner; the backend contains no copied OAuth tokens or host identity.
A node-OAuth `account` is an opaque, DID-free reference minted when an
account is first stored; `none` is the provider's original account.
`nodeOAuthPool` rotates every call over all enabled accounts of the backend's
provider on the executing node (`AccountRotation`); drawing paid extra usage
when every account is past its limit is the operator's choice, off by default.
The raw credential constructors below also represent transient authoring
inputs and legacy records. They must not be stored in Agent-readable backend
metadata. `BackendMetadataAuth` is the storage selection for fresh metadata. -/
inductive BackendAuth where
  | unauthenticated
  | apiKey (key : String)
  | credential (id version : String)
  | environment (name : String)
  | nodeOAuth (account : Option String)
  | nodeOAuthPool (allowOverage : Bool)
  | awsCredentials (accessKeyId secretAccessKey : String) (sessionToken : Option String)
  | googleCredentials (credentialsJson : String)
  deriving DecidableEq, Repr

/-- User-managed metadata selects a private credential by opaque id and
rotation version; the native secret resource owns its contents. -/
inductive BackendMetadataAuth where
  | unauthenticated
  | credential (id version : String)
  | environment (name : String)
  | nodeOAuth (account : Option String)
  | nodeOAuthPool (allowOverage : Bool)
  deriving DecidableEq, Repr

def oauthLookupOwner (scope : String) : BackendAuth → Option String
  | .nodeOAuth _ | .nodeOAuthPool _ => some scope
  | _ => none

theorem oauth_uses_execution_scope (scope : String) (account : Option String) :
    oauthLookupOwner scope (.nodeOAuth account) = some scope := rfl

theorem oauth_pool_uses_execution_scope (scope : String) (allowOverage : Bool) :
    oauthLookupOwner scope (.nodeOAuthPool allowOverage) = some scope := rfl

structure OAuthAccountRow where
  owner : String
  kind : String
  account : Option String
  enabled : Bool
  deriving DecidableEq, Repr

/-- The executing node's enabled row of `kind` with exactly the requested
account reference: a reference never falls back to another account, and another
node's row never resolves. Order among equal matches is the Rust resolver's
(earliest-connected enabled first, `inference/auth/credential.rs`); it is not modeled,
because it only matters once a node holds several accounts. -/
def oauthResolve (scope kind : String) (account : Option String)
    (rows : List OAuthAccountRow) : Option OAuthAccountRow :=
  rows.find? fun r => r.owner == scope && r.kind == kind && r.account == account && r.enabled

theorem oauth_resolve_sound {scope kind : String} {account : Option String}
    {rows : List OAuthAccountRow} {r : OAuthAccountRow}
    (h : oauthResolve scope kind account rows = some r) :
    r ∈ rows ∧ r.owner = scope ∧ r.kind = kind ∧ r.account = account ∧ r.enabled = true := by
  have hp := List.find?_some h
  simp only [Bool.and_eq_true, beq_iff_eq] at hp
  exact ⟨List.mem_of_find?_eq_some h, hp.1.1.1, hp.1.1.2, hp.1.2, hp.2⟩

/-- A pool backend's accounts: every enabled row of `kind` the executing node
owns. Another node's row never joins the pool. Rotation order is the Rust
resolver's (earliest-connected first, `oauth_credential.rs`) and is not modeled,
as for `oauthResolve`. -/
def oauthPool (scope kind : String) (rows : List OAuthAccountRow) : List OAuthAccountRow :=
  rows.filter fun r => r.owner == scope && r.kind == kind && r.enabled

theorem oauth_pool_sound {scope kind : String} {rows : List OAuthAccountRow}
    {r : OAuthAccountRow} (h : r ∈ oauthPool scope kind rows) :
    r ∈ rows ∧ r.owner = scope ∧ r.kind = kind ∧ r.enabled = true := by
  simp only [oauthPool, List.mem_filter, Bool.and_eq_true, beq_iff_eq] at h
  exact ⟨h.1, h.2.1.1, h.2.1.2, h.2.2⟩

theorem oauth_pool_holds_every_enabled_row {scope kind : String}
    {rows : List OAuthAccountRow} {r : OAuthAccountRow} (hr : r ∈ rows)
    (ho : r.owner = scope) (hk : r.kind = kind) (he : r.enabled = true) :
    r ∈ oauthPool scope kind rows := by
  simp [oauthPool, List.mem_filter, hr, ho, hk, he]

structure BackendControls where
  connectionTimeoutSecs : Nat
  discoveryTimeoutSecs : Nat
  maxConcurrent : Nat
  queuedLimit : Nat
deriving DecidableEq, Repr

/-- The controls of a backend document that sets none: ten-second connection and
discovery timeouts, four concurrent inference calls and a queue of one hundred.

Decision: the default admits four concurrent calls per backend, for every
provider kind, so a second session, a delegate Agent or a parallel workflow step
runs beside the first call on the same backend instead of waiting for it. A
backend document's `max_concurrent` replaces it, and one session still runs one
request at a time (`SessionQueueState.active`), whatever the backend admits. -/
def defaultBackendControls : BackendControls := ⟨10, 10, 4, 100⟩

def resolveBackendControls (connect discovery concurrent queue : Option Int) :
    Option BackendControls := do
  let connectionTimeoutSecs ← ConfigDefaults.resolveNat defaultBackendControls.connectionTimeoutSecs 1 connect
  let discoveryTimeoutSecs ← ConfigDefaults.resolveNat defaultBackendControls.discoveryTimeoutSecs 1 discovery
  let maxConcurrent ← ConfigDefaults.resolveNat defaultBackendControls.maxConcurrent 1 concurrent
  let queuedLimit ← ConfigDefaults.resolveNat defaultBackendControls.queuedLimit 0 queue
  pure ⟨connectionTimeoutSecs, discoveryTimeoutSecs, maxConcurrent, queuedLimit⟩

/-- Admission guarantees positive timers and capacity; queue depth is naturally
nonnegative only after signed authored input has passed validation. -/
theorem resolved_backend_controls_positive (connect discovery concurrent queue : Option Int)
    (controls : BackendControls)
    (h : resolveBackendControls connect discovery concurrent queue = some controls) :
    0 < controls.connectionTimeoutSecs ∧ 0 < controls.discoveryTimeoutSecs ∧
      0 < controls.maxConcurrent := by
  simp only [resolveBackendControls, bind, Option.bind] at h
  split at h <;> simp_all
  next c hc =>
    split at h <;> simp_all
    next d hd =>
      split at h <;> simp_all
      next n hn =>
        split at h <;> simp_all
        next q hq =>
          have hb1 := ConfigDefaults.resolveNat_lower_bound 10 1 connect c (by decide) hc
          have hb2 := ConfigDefaults.resolveNat_lower_bound 10 1 discovery d (by decide) hd
          have hb3 := ConfigDefaults.resolveNat_lower_bound 4 1 concurrent n (by decide) hn
          cases h
          exact ⟨hb1, hb2, hb3⟩

theorem backend_defaults_resolve :
    resolveBackendControls none none none none = some defaultBackendControls := rfl

theorem backend_zero_capacity_rejected (connect discovery queue : Option Int) :
    resolveBackendControls connect discovery (some 0) queue = none := by
  simp [resolveBackendControls, defaultBackendControls, ConfigDefaults.resolveNat, bind, Option.bind]
  split <;> simp_all
  split <;> simp_all

theorem backend_zero_queue_allowed :
    resolveBackendControls none none none (some 0) = some ⟨10, 10, 4, 0⟩ := rfl

theorem backend_negative_queue_rejected (queue : Int) (h : queue < 0) :
    resolveBackendControls none none none (some queue) = none := by
  have hn : ¬ (0 : Int) ≤ queue := by omega
  simp [resolveBackendControls, defaultBackendControls, ConfigDefaults.resolveNat, hn]

/-- An existing dependency retains its own install variable, including an
explicitly unset value. Only a per-pack override may replace that value;
ambient variables are consulted for a root install or a first installation. -/
def resolvePackInstallVariable (dependency installed : Bool)
    (previous override : Option (Option String)) (ambient : Option String) : Option (Option String) :=
  match override with
  | some value => some value
  | none => if dependency && installed then previous else some ambient

theorem dependency_variable_preserved (previous ambient : Option String) :
    resolvePackInstallVariable true true (some previous) none ambient = some previous := by
  rfl

theorem explicit_pack_variable_wins (dependency installed : Bool) (previous : Option (Option String))
    (override ambient : Option String) :
    resolvePackInstallVariable dependency installed previous (some override) ambient = some override := by
  rfl

theorem first_dependency_uses_ambient (ambient : Option String) :
    resolvePackInstallVariable true false none none ambient = some ambient := by
  rfl

theorem root_uses_current_environment (installed : Bool) (previous : Option (Option String)) (ambient : Option String) :
    resolvePackInstallVariable false installed previous none ambient = some ambient := by
  rfl

/-- Older records without an observed binding do not authorize substituting
the current caller's environment for an installed dependency. -/
theorem unknown_dependency_binding_fails_closed (ambient : Option String) :
    resolvePackInstallVariable true true none none ambient = none := by
  rfl

/-- Upgrades render the current template using an explicitly supplied value,
    or the installed binding when no replacement is supplied. -/
def resolvePackUpgradeVariable (previous supplied : Option String) : Option String :=
  supplied.orElse fun _ => previous

theorem omitted_upgrade_value_preserved (previous : Option String) :
    resolvePackUpgradeVariable previous none = previous := by rfl

theorem supplied_upgrade_value_wins (previous : Option String) (supplied : String) :
    resolvePackUpgradeVariable previous (some supplied) = some supplied := by rfl

/-- Node identity comes from the installation scope, not from pack variables. -/
def packVariableOverrideAllowed (name : String) : Bool :=
  name != "GENTS_PACK_NODE_DID"

theorem node_identity_override_refused :
    packVariableOverrideAllowed "GENTS_PACK_NODE_DID" = false := by decide

/-- An installed dependency can be reused without recovering its original
environment only when the same artifact is requested and the caller has not
explicitly selected replacement bindings. The snapshot is authoritative only
if the installation record and every recorded document still match at commit. -/
def reuseInstalledDependency (installed sameArtifact explicitOverride : Bool) : Bool :=
  installed && sameArtifact && !explicitOverride

def commitDependencySnapshot (sameRecord sameDocuments : Bool) : Bool :=
  sameRecord && sameDocuments

theorem replacement_binding_disallows_reuse (installed sameArtifact : Bool) :
    reuseInstalledDependency installed sameArtifact true = false := by
  simp [reuseInstalledDependency]

theorem changed_dependency_snapshot_refused (sameRecord : Bool) :
    commitDependencySnapshot sameRecord false = false := by
  simp [commitDependencySnapshot]

theorem changed_dependency_record_refused (sameDocuments : Bool) :
    commitDependencySnapshot false sameDocuments = false := by
  simp [commitDependencySnapshot]

/-- Local pack preparation has the root and declared dependency manifests and
an installed-plugin inventory. Every declared reference must resolve through
those existing resources before configuration writes begin. -/
def pluginReferencesAvailable (references shipped installed : List String) : Bool :=
  references.all fun reference => decide (reference ∈ shipped ∨ reference ∈ installed)

theorem pluginReferencesAvailable_iff (references shipped installed : List String) :
    pluginReferencesAvailable references shipped installed = true ↔
      ∀ reference ∈ references, reference ∈ shipped ∨ reference ∈ installed := by
  simp [pluginReferencesAvailable, List.all_eq_true]

theorem missing_plugin_refuses_install (references shipped installed : List String)
    (reference : String) (hReference : reference ∈ references)
    (hShipped : reference ∉ shipped) (hInstalled : reference ∉ installed) :
    pluginReferencesAvailable references shipped installed = false := by
  cases h : pluginReferencesAvailable references shipped installed with
  | false => rfl
  | true =>
      have resolved := (pluginReferencesAvailable_iff references shipped installed).mp h
        reference hReference
      exact False.elim (resolved.elim hShipped hInstalled)


/-- A pin is checked against the artifact that will be installed; an
unpublished older artifact at the same coordinate does not satisfy it. -/
def pluginPinMatches (pin : Option String) (actual : String) : Bool :=
  match pin with
  | none => true
  | some expected => expected == actual

theorem mismatched_plugin_pin_refuses (expected actual : String) (h : expected ≠ actual) :
    pluginPinMatches (some expected) actual = false := by
  simp [pluginPinMatches, h]

/-- The surviving graph must accept every newly published plugin digest. -/
def pluginReplacementSafe (pins : List (Option String)) (replacement : String) : Bool :=
  pins.all fun pin => pluginPinMatches pin replacement

theorem mismatched_survivor_rejected (expected replacement : String) (h : expected ≠ replacement) :
    pluginReplacementSafe [some expected] replacement = false := by
  simp [pluginReplacementSafe, pluginPinMatches, h]

/-- Callback rejection precedes commit; an unconfirmed commit may have won and
must retain the published plugin until its outcome can be observed. -/
inductive PackDocumentOutcome where
  | committed | callbackRejected | unconfirmed
  deriving DecidableEq, Repr

def retainPluginPublication : PackDocumentOutcome → Bool
  | .committed => true
  | .callbackRejected => false
  | .unconfirmed => true

theorem rejected_document_restores_plugin : retainPluginPublication .callbackRejected = false := rfl
theorem unknown_commit_keeps_plugin : retainPluginPublication .unconfirmed = true := rfl

/-- A hosted document effect is admitted only for the authenticated client,
after verifying its signed graph against this transaction's snapshot and the
client-approved pack plan. Native document ACP remains authoritative. -/
def hostedPackEffectAllowed (actorMatches graphVerified planApproved nativeAuthorized : Bool) : Bool :=
  actorMatches && graphVerified && planApproved && nativeAuthorized

theorem hosted_effect_requires_verified_graph (actor plan native : Bool) :
    hostedPackEffectAllowed actor false plan native = false := by
  simp [hostedPackEffectAllowed]

theorem hosted_effect_requires_approved_plan (actor graph native : Bool) :
    hostedPackEffectAllowed actor graph false native = false := by
  simp [hostedPackEffectAllowed]

/-- What a wiring plan reads of an installed plugin: the pack that owns it,
by digest and coordinate, and the rest of its record the plan writes from
(its name, version, artifact digest and declaration), here `record`. -/
structure WireSubject where
  packDigest : String
  coordinate : String
  /-- The plugin's own coordinate, `namespace/name`, which the wiring pins. -/
  plugin : String
  record : String
  deriving DecidableEq, Repr

/-- Where a client's wiring plan takes the plugin's record from. -/
inductive WireRecord where
  /-- Inside an install: from the pack the client itself just sent. -/
  | sentPack (subject : WireSubject)
  /-- From the plugin store of the client's own home, which the runtime
  serves: state the client keeps itself. -/
  | ownHome (subject : WireSubject)
  /-- A standalone wire over a runtime: from the record the runtime serves. -/
  | served (subject : WireSubject)
  deriving DecidableEq, Repr

def WireRecord.subject : WireRecord → WireSubject
  | .sentPack subject | .ownHome subject | .served subject => subject

/-- Decision (gents#3962): a client wiring a plugin over a runtime plans from
a record it can tie to a pack it chose itself, never from the runtime's word.
Inside an install that is the pack it just sent, and over a runtime that
serves the client's own home it is that home's plugin store. Otherwise the
runtime serves the record, and the client takes it only when it is exactly
what a pack the client approved and recorded (`approved`) yields: an unknown
pack digest, or a known digest with any other record, plans nothing and
nothing is signed. Whatever the source, the record must name the plugin the
client asked to wire (`requested`): the wiring pins the record's coordinate,
so a record of any other plugin, approved or not, plans nothing. -/
def wirePlanSubject (requested : String) (approved : List WireSubject) (record : WireRecord) :
    Option WireSubject :=
  if record.subject.plugin ≠ requested then none
  else match record with
    | .sentPack subject => some subject
    | .ownHome subject => some subject
    | .served subject => if approved.contains subject then some subject else none

theorem a_record_of_another_plugin_plans_nothing (requested : String)
    (approved : List WireSubject) (record : WireRecord) (other : record.subject.plugin ≠ requested) :
    wirePlanSubject requested approved record = none := by
  simp [wirePlanSubject, other]

theorem a_planned_record_names_the_requested_plugin (requested : String)
    (approved : List WireSubject) (record : WireRecord) (subject : WireSubject)
    (planned : wirePlanSubject requested approved record = some subject) :
    subject.plugin = requested := by
  unfold wirePlanSubject at planned
  split at planned
  · cases planned
  · rename_i named
    cases record with
    | sentPack sent => simp at planned; subst planned; simpa [WireRecord.subject] using named
    | ownHome kept => simp at planned; subst planned; simpa [WireRecord.subject] using named
    | served served =>
      simp only at planned
      split at planned
      · simp at planned; subst planned; simpa [WireRecord.subject] using named
      · cases planned

theorem a_served_record_of_no_approved_pack_plans_nothing (requested : String)
    (approved : List WireSubject)
    (subject : WireSubject) (unknown : ∀ kept ∈ approved, kept.packDigest ≠ subject.packDigest) :
    wirePlanSubject requested approved (.served subject) = none := by
  have absent : subject ∉ approved := fun mem => unknown subject mem rfl
  simp [wirePlanSubject, absent]

theorem an_altered_served_record_plans_nothing (requested : String)
    (approved : List WireSubject)
    (subject : WireSubject)
    (altered : ∀ kept ∈ approved, kept.packDigest = subject.packDigest → kept ≠ subject) :
    wirePlanSubject requested approved (.served subject) = none := by
  have absent : subject ∉ approved := fun mem => altered subject mem rfl rfl
  simp [wirePlanSubject, absent]

theorem a_wire_plans_only_from_its_own_pack_or_an_approved_one (requested : String)
    (approved : List WireSubject) (record : WireRecord) (subject : WireSubject)
    (planned : wirePlanSubject requested approved record = some subject) :
    record = .sentPack subject ∨ record = .ownHome subject ∨ subject ∈ approved := by
  unfold wirePlanSubject at planned
  split at planned
  · cases planned
  · cases record with
    | sentPack sent => simp at planned; exact .inl (by rw [planned])
    | ownHome kept => simp at planned; exact .inr (.inl (by rw [planned]))
    | served served =>
      simp only at planned
      split at planned
      · rename_i kept
        simp at planned
        subst planned
        exact .inr (.inr (by simpa using kept))
      · cases planned

/-- A write the runtime proposes while a client wires is signed only when it
matches the client's own plan (`hostedPackEffectAllowed`); a wire that plans
nothing has no plan to match, so it signs nothing. -/
theorem a_wire_with_no_plan_signs_no_write (requested : String) (approved : List WireSubject)
    (record : WireRecord) (unplanned : wirePlanSubject requested approved record = none)
    (actor graph native : Bool) :
    hostedPackEffectAllowed actor graph (wirePlanSubject requested approved record).isSome native
      = false := by
  simp [unplanned, hostedPackEffectAllowed]

/-- Decision (gents#3962): over a runtime that serves another home, a client
signs only document ids that the pack it approved yields: the Tools and Skill
ids of the plugin's own pack, which the wiring reads its reviewed job from
and attaches to the agent's Context, and the AgentTarget ids of each pack it
wires targets from (`--targets-from`). A pack's ids are its configuration's
own, fixed by the pack, the node and the install variables the client
supplied, so the approval records them (`approved`); when the approval could
not derive them, say because the configuration reads a variable only the
runtime's environment supplies, it records none and nothing of that pack is
wired. An id the runtime reports that the pack does not yield plans
nothing. -/
def wireApprovedIds (approved : Option (List String)) (served : List String) :
    Option (List String) :=
  match approved with
  | none => none
  | some ids => if served.all ids.contains then some served else none

theorem an_id_the_approved_pack_does_not_yield_plans_nothing (ids served : List String)
    (target : String) (reported : target ∈ served) (foreign : target ∉ ids) :
    wireApprovedIds (some ids) served = none := by
  have : served.all ids.contains = false := by
    rw [List.all_eq_false]
    exact ⟨target, reported, by simpa using foreign⟩
  simp [wireApprovedIds, this]

theorem an_approval_without_ids_wires_none (served : List String) :
    wireApprovedIds none served = none := rfl

theorem wired_ids_are_approved_ids (ids served planned : List String)
    (wired : wireApprovedIds (some ids) served = some planned) : ∀ target ∈ planned, target ∈ ids := by
  simp only [wireApprovedIds] at wired
  split at wired
  · rename_i all
    simp at wired
    subst wired
    intro target mem
    simpa using (List.all_eq_true.mp all) target mem
  · cases wired

/-- One part of a document id a pack's configuration yields: text the pack,
the node and the install variables the client supplied fix, or the DID of an
agent slot the install provisions, which the approval cannot know. A slot the
client bound to an agent at approval is text. -/
inductive IdPart where
  | text (value : String)
  | slot (name : String)
  deriving DecidableEq, Repr

def IdPart.fill (agent : String) : IdPart → String
  | .text value => value
  | .slot _ => agent

def IdPart.isSlot : IdPart → Bool
  | .text _ => false
  | .slot _ => true

/-- Decision (gents#3980 review): an id a pack's configuration derives from an
agent slot's DID (`${GENTS_PACK_AGENT_DID.worker}:tools`) is fixed only once
the install provisions that agent. The client never takes that DID from the
runtime: it fills a slot only with the agent it names itself, the agent the
wire attaches the plugin to (`agent`), and only for the plugin's own pack; a
pack it wires targets from fills no slot (`agent = none`), so an id of such a
slot yields nothing. -/
def fillId (agent : Option String) (parts : List IdPart) : Option String :=
  match agent with
  | some agent => some (String.join (parts.map (IdPart.fill agent)))
  | none => if parts.any IdPart.isSlot then none else some (String.join (parts.map (IdPart.fill "")))

/-- The ids an approval yields once its slots are filled (`fillId`); an id
that cannot be filled is not yielded. -/
def wireFilledIds (approved : Option (List (List IdPart))) (agent : Option String) :
    Option (List String) :=
  approved.map (·.filterMap (fillId agent))

theorem a_slot_id_fills_nothing_without_the_wired_agent (parts : List IdPart)
    (slotted : parts.any IdPart.isSlot = true) : fillId none parts = none := by
  simp [fillId, slotted]

theorem a_filled_id_puts_only_the_wired_agent_in_its_slots (agent : String)
    (parts : List IdPart) :
    fillId (some agent) parts = some (String.join (parts.map (IdPart.fill agent))) :=
  rfl

theorem wired_filled_ids_are_approved_templates (templates : List (List IdPart))
    (agent : Option String) (served planned : List String)
    (wired : wireApprovedIds (wireFilledIds (some templates) agent) served = some planned) :
    ∀ id ∈ planned, ∃ parts ∈ templates, fillId agent parts = some id := by
  intro id mem
  have := wired_ids_are_approved_ids _ served planned wired id mem
  simpa using List.mem_filterMap.mp this

/-- Decision (gents#3962): over a runtime that serves another home, the job
block a wire writes for a plugin is the one its own pack's Tools carry for
it, which the client reads from the node. The client signs it only when it is
the job the pack it approved yields (`approved`, `none` when the approval
could not derive it); a job the runtime reports that the pack does not yield,
including a job where the pack has none or none where it has one, plans
nothing. -/
def wireReviewedJob (approved : Option (Option String)) (served : Option String) :
    Option (Option String) :=
  match approved with
  | none => none
  | some job => if served = job then some job else none

theorem a_job_the_approved_pack_does_not_yield_plans_nothing (job : Option String)
    (served : Option String) (other : served ≠ job) :
    wireReviewedJob (some job) served = none := by
  simp [wireReviewedJob, other]

theorem a_wired_job_is_the_approved_job (approved : Option (Option String))
    (served job : Option String) (wired : wireReviewedJob approved served = some job) :
    approved = some job := by
  cases approved with
  | none => cases wired
  | some kept =>
    simp only [wireReviewedJob] at wired
    split at wired
    · simp at wired; rw [wired]
    · cases wired

/-- One approval a client keeps for a node: the pack it approved, by digest
and coordinate, and the subjects that pack yields. -/
structure PackApproval where
  packDigest : String
  coordinate : String
  subjects : List WireSubject
  /-- The Tools, Skill and AgentTarget ids the pack yields, when the
  approval could derive them (`wireFilledIds`). -/
  yields : Option (List (List IdPart))
  deriving DecidableEq, Repr

/-- What a client finds where it keeps the packs it approved for a node. -/
inductive ApprovalsFile where
  | absent
  /-- Larger than the bound a client reads. -/
  | oversized
  /-- Not a record of approvals in the encoding the client writes. -/
  | unparseable
  | parsed (approvals : List PackApproval)
  deriving DecidableEq, Repr

/-- An approval holds only what its pack yields: each subject names the
approval's digest and coordinate, and no digest is approved twice. -/
def approvalsConsistent (approvals : List PackApproval) : Bool :=
  approvals.all (fun approval => approval.subjects.all fun subject =>
      subject.packDigest == approval.packDigest && subject.coordinate == approval.coordinate)
    && (approvals.map PackApproval.packDigest).Nodup

/-- Decision (gents#3962): the subjects a client's approvals record lets a
served record match (`wirePlanSubject`). A client that never approved a
pack has none. A record it cannot read in full, too large, undecodable or
inconsistent, is refused rather than read as empty: an empty reading is not
what the client approved, and a partly edited record cannot be told apart
from one the client wrote. -/
def approvedSubjects : ApprovalsFile → Option (List WireSubject)
  | .absent => some []
  | .oversized => none
  | .unparseable => none
  | .parsed approvals =>
    if approvalsConsistent approvals then some (approvals.flatMap PackApproval.subjects) else none

theorem an_unreadable_approvals_record_is_refused :
    approvedSubjects .oversized = none ∧ approvedSubjects .unparseable = none := ⟨rfl, rfl⟩

theorem a_client_that_approved_nothing_has_no_subjects : approvedSubjects .absent = some [] := rfl

theorem an_approved_subject_names_its_approval (approvals : List PackApproval)
    (subjects : List WireSubject) (subject : WireSubject)
    (read : approvedSubjects (.parsed approvals) = some subjects) (mem : subject ∈ subjects) :
    ∃ approval ∈ approvals, subject.packDigest = approval.packDigest
      ∧ subject.coordinate = approval.coordinate := by
  simp only [approvedSubjects] at read
  split at read
  · rename_i consistent
    simp at read
    subst read
    obtain ⟨approval, member, holds⟩ := List.mem_flatMap.mp mem
    refine ⟨approval, member, ?_⟩
    simp only [approvalsConsistent, Bool.and_eq_true, List.all_eq_true] at consistent
    have := consistent.1 approval member subject holds
    simp at this
    exact ⟨this.1, this.2⟩
  · cases read

/-- Decision (gents#3980 review): a client may approve several versions of a
pack, each by its digest. The approval that vouches for the plugin's own
pack, its Tools and Skill ids and the reviewed job (`wireReviewedJob`), is
the one whose digest the planned record names, the approval that
vouched for the record (`wirePlanSubject`); another version's approval of
the same coordinate never does. -/
def wireOwnApproval (approvals : List PackApproval) (subject : WireSubject) :
    Option PackApproval :=
  approvals.find? (·.packDigest = subject.packDigest)

/-- Decision (gents#3980 review): the approval that vouches for a pack a wire
takes targets from is the one whose digest the node's installation record of
that coordinate names (`installed`). The client picks no version itself: when
the record names no digest it approved for that coordinate, however many it
approved, the pack is refused. -/
def wireTargetApproval (approvals : List PackApproval) (coordinate : String)
    (installed : Option String) : Option PackApproval :=
  approvals.find? fun approval =>
    approval.coordinate = coordinate ∧ some approval.packDigest = installed

/-- The Tools and Skill ids of the plugin's own pack a client signs for: the
`served` ids only when the approval that vouched for the record yields each,
its slots filled with the wired `agent`. -/
def wireOwnPackIds (approvals : List PackApproval) (subject : WireSubject) (agent : String)
    (served : List String) : Option (List String) :=
  (wireOwnApproval approvals subject).bind fun approval =>
    wireApprovedIds (wireFilledIds approval.yields (some agent)) served

/-- The AgentTarget ids of a pack a client wires targets from: the `served`
ids only when the approval of the installed digest yields each, filling no
slot. -/
def wireTargetPackIds (approvals : List PackApproval) (coordinate : String)
    (installed : Option String) (served : List String) : Option (List String) :=
  (wireTargetApproval approvals coordinate installed).bind fun approval =>
    wireApprovedIds (wireFilledIds approval.yields none) served

theorem the_own_pack_is_vouched_for_by_the_records_approval (approvals : List PackApproval)
    (subject : WireSubject) (approval : PackApproval)
    (chosen : wireOwnApproval approvals subject = some approval) :
    approval ∈ approvals ∧ approval.packDigest = subject.packDigest := by
  unfold wireOwnApproval at chosen
  exact ⟨List.mem_of_find?_eq_some chosen, by simpa using List.find?_some chosen⟩

theorem another_version_never_vouches_for_the_record (approvals : List PackApproval)
    (subject : WireSubject) (other : PackApproval)
    (version : other.packDigest ≠ subject.packDigest) :
    wireOwnApproval approvals subject ≠ some other := fun chosen =>
  version (the_own_pack_is_vouched_for_by_the_records_approval approvals subject other chosen).2

theorem a_target_pack_is_vouched_for_by_its_installed_digest (approvals : List PackApproval)
    (coordinate : String) (installed : Option String) (approval : PackApproval)
    (chosen : wireTargetApproval approvals coordinate installed = some approval) :
    approval ∈ approvals ∧ approval.coordinate = coordinate
      ∧ some approval.packDigest = installed := by
  unfold wireTargetApproval at chosen
  exact ⟨List.mem_of_find?_eq_some chosen, by simpa using List.find?_some chosen⟩

theorem a_target_pack_whose_installed_digest_is_not_approved_is_refused
    (approvals : List PackApproval) (coordinate : String) (installed : Option String)
    (unapproved : ∀ approval ∈ approvals,
      approval.coordinate = coordinate → some approval.packDigest ≠ installed) :
    wireTargetApproval approvals coordinate installed = none := by
  unfold wireTargetApproval
  rw [List.find?_eq_none]
  intro approval mem
  simpa using unapproved approval mem

theorem own_pack_ids_come_from_the_records_approval (approvals : List PackApproval)
    (subject : WireSubject) (agent : String) (served planned : List String)
    (wired : wireOwnPackIds approvals subject agent served = some planned) :
    ∃ approval ∈ approvals, approval.packDigest = subject.packDigest ∧
      ∀ id ∈ planned, ∃ parts ∈ approval.yields.getD [], fillId (some agent) parts = some id := by
  unfold wireOwnPackIds at wired
  cases chosen : wireOwnApproval approvals subject with
  | none => simp [chosen] at wired
  | some approval =>
    rw [chosen] at wired
    simp only [Option.bind] at wired
    obtain ⟨mem, digest⟩ := the_own_pack_is_vouched_for_by_the_records_approval _ _ _ chosen
    refine ⟨approval, mem, digest, ?_⟩
    cases yields : approval.yields with
    | none => simp [yields, wireFilledIds, wireApprovedIds] at wired
    | some templates =>
      rw [yields] at wired
      simpa using wired_filled_ids_are_approved_templates templates _ served planned wired

theorem target_pack_ids_come_from_the_installed_approval (approvals : List PackApproval)
    (coordinate : String) (installed : Option String) (served planned : List String)
    (wired : wireTargetPackIds approvals coordinate installed served = some planned) :
    ∃ approval ∈ approvals, approval.coordinate = coordinate ∧
      some approval.packDigest = installed ∧
      ∀ id ∈ planned, ∃ parts ∈ approval.yields.getD [], fillId none parts = some id := by
  unfold wireTargetPackIds at wired
  cases chosen : wireTargetApproval approvals coordinate installed with
  | none => simp [chosen] at wired
  | some approval =>
    rw [chosen] at wired
    simp only [Option.bind] at wired
    obtain ⟨mem, named, digest⟩ :=
      a_target_pack_is_vouched_for_by_its_installed_digest _ _ _ _ chosen
    refine ⟨approval, mem, named, digest, ?_⟩
    cases yields : approval.yields with
    | none => simp [yields, wireFilledIds, wireApprovedIds] at wired
    | some templates =>
      rw [yields] at wired
      simpa using wired_filled_ids_are_approved_templates templates _ served planned wired

/-- Plugin-only hosted installation preserves existing pack documents. Its
  installed plugin records are staged by the host from the reviewed archive;
  the client independently replays the same native-authorized pack record. -/
def hostedPluginInstallAllowed (reviewedDigest clientPlan nativeAuthorized : Bool) : Bool :=
  reviewedDigest && clientPlan && nativeAuthorized

theorem unreviewed_hosted_plugin_is_denied (plan native : Bool) :
    hostedPluginInstallAllowed false plan native = false := by
  simp [hostedPluginInstallAllowed]

theorem hosted_plugin_needs_native_authority (reviewed plan : Bool) :
    hostedPluginInstallAllowed reviewed plan false = false := by
  simp [hostedPluginInstallAllowed]

/-- A hosted graph reaches native commit only after the User's independent
replay matches every proposed effect and signals that its plan is complete. -/
def hostedPackCommitAllowed (effectsMatched clientFinished : Bool) : Bool :=
  effectsMatched && clientFinished

/-- A hosted install reports success only after its required host record has
been written. A failed host write cannot undo a committed document graph. -/
def hostedPackReportedSuccess (committed homeRequired homeWritten : Bool) : Bool :=
  committed && (!homeRequired || homeWritten)

theorem hosted_pack_missing_home_record_is_not_success :
    hostedPackReportedSuccess true true false = false := by decide

def hostedPackHomeCases : List Bool := [false, true]

theorem hosted_pack_disconnect_before_finish_cannot_commit (effectsMatched : Bool) :
    hostedPackCommitAllowed effectsMatched false = false := by
  simp [hostedPackCommitAllowed]

def hostedPackEffectCases : List (Bool × Bool × Bool) :=
  [(false, false, true), (false, true, true), (true, false, true),
    (true, true, false), (true, true, true)]

/-- Once a commit request may have succeeded, later failed attempts cannot
prove that the earlier attempt did not commit. A successful receipt resolves
the operation; otherwise only histories with no uncertain commit may restore. -/
inductive PackAttemptEvidence where
  | noCommit | unknownCommit
  deriving DecidableEq, Repr

def packCommitUnconfirmed (attempts : List PackAttemptEvidence) : Bool :=
  attempts.any fun attempt => match attempt with | .noCommit => false | .unknownCommit => true

def packFinalityCases : List (List PackAttemptEvidence) :=
  [[], [.noCommit], [.noCommit, .noCommit], [.unknownCommit],
    [.unknownCommit, .noCommit], [.noCommit, .unknownCommit, .noCommit]]

theorem later_begin_failure_cannot_clear_unknown_commit :
    packCommitUnconfirmed [.unknownCommit, .noCommit] = true := by decide

/-- Wiring holds the shared plugin store lock from reading the digest through
configuration publication. Competing store mutation cannot publish in that interval. -/
def wireAndPluginPublishAllowed (wireHoldsLock : Bool) : Bool := !wireHoldsLock

/-- A staged replacement is not available to plugin execution until the
publisher releases the store lock after native transaction finality. -/
def stagedPluginResolutionAllowed (publisherHoldsLock : Bool) : Bool := !publisherHoldsLock

theorem staged_plugin_cannot_be_resolved : stagedPluginResolutionAllowed true = false := by decide

theorem plugin_publish_blocked_during_wire :
    wireAndPluginPublishAllowed true = false := by decide

/-- Publishing a plugin pin requires an embedded transaction on the runtime's
own store. A preview or removal does not publish a new plugin reference. -/
def wireTargetAllowed (localTarget preview remove : Bool) : Bool :=
  localTarget || preview || remove

theorem graphql_wire_cannot_publish_pin : wireTargetAllowed false false false = false := by decide

/-- Untagged documents retain the existing adoption behavior. A tagged document
can be written only by the same full pack coordinate; an old unqualified tag
does not establish a publisher identity. -/
def packDocumentWriteAllowed (coordinate : String) (origin : Option String) : Bool :=
  match origin with
  | none => true
  | some existing => existing == coordinate

def packDocumentRemovalAllowed (coordinate : String) (origin : Option String) : Bool :=
  packDocumentWriteAllowed coordinate origin

theorem pack_document_other_publisher_refused :
    packDocumentWriteAllowed "other/demo" (some "acme/demo") = false := by decide

theorem pack_document_unqualified_origin_refused :
    packDocumentWriteAllowed "acme/demo" (some "demo") = false := by decide

theorem pack_document_untagged_adoptable :
    packDocumentWriteAllowed "acme/demo" none = true := by decide

/-- Only a node-local installer can inspect or publish plugin bytes. A remote
GraphQL install may write plugin-free documents, but recorded pack metadata is
not evidence of host file availability. -/
def pluginInstallAvailable (references shipped installed : List String) (isLocal : Bool) : Bool :=
  if isLocal then pluginReferencesAvailable references shipped installed
  else references.isEmpty && shipped.isEmpty

theorem remote_plugin_install_requires_no_plugins (references shipped installed : List String) :
    pluginInstallAvailable references shipped installed false = true ↔
      references = [] ∧ shipped = [] := by
  simp [pluginInstallAvailable]

/-- Removing plugin files must not invalidate any surviving agent's Tools.
References are collected after the same transaction's document deletions, so an
agent removed with its pack does not prevent that pack's removal. -/
def pluginRemovalSafe (references removed : List String) : Bool :=
  references.all fun reference => decide (reference ∉ removed)

/-- A User or the Engineer retires installed plugins: retirement is a pack
removal, which only the User's own client and an agent holding the pack
install grant reach. It is refused while a Tools document the retirer can
read still selects one of the plugins. Document access filters that read, so
a Tools the retirer cannot read may still name a retired plugin; its agent
then fails that tool's resolution with "plugin is not installed", which the
maintainer accepts over a plugin nobody can remove. `observed` is false when
the node's configuration was not read at all (a home that has no node), which
proves nothing and refuses. An empty retirement needs no read. -/
def pluginRetirementAllowed (references removed : List String) (observed : Bool) : Bool :=
  removed.isEmpty || (observed && pluginRemovalSafe references removed)

theorem unobserved_node_refuses_retirement (references removed : List String)
    (nonempty : removed.isEmpty = false) :
    pluginRetirementAllowed references removed false = false := by
  simp [pluginRetirementAllowed, nonempty]

theorem unselected_plugins_retire (references removed : List String)
    (unselected : pluginRemovalSafe references removed = true) :
    pluginRetirementAllowed references removed true = true := by
  simp [pluginRetirementAllowed, unselected]

/-- A notice that plugin files are gone keeps the node's installation record
unless the plugins were retired: files that disappeared outside a retirement
leave the durable evidence in place, and the notice may still refresh the
record so watchers observe the change. A retirement releases the record. -/
def retainPluginStoreRecord (plugins : List String) (retired : Bool) : Bool :=
  !plugins.isEmpty && !retired

theorem retired_plugins_release_their_record (plugins : List String) :
    retainPluginStoreRecord plugins true = false := by
  simp [retainPluginStoreRecord]

/-- The installation record a coordinate has when a plugin-store notice
arrives: none, one a pack install wrote, or one only the plugin store wrote. -/
inductive PluginNoticePrior where
  | absent
  | pack
  | store
  deriving DecidableEq, Repr

/-- Decision: a plugin-store notice, a disappearance or a retirement, owns only
the record the plugin store wrote. A pack's own record outlives both, so a pack
that ships no documents and no plugins stays installed and removable. -/
def pluginNoticeKeepsRecord (prior : PluginNoticePrior) (plugins : List String)
    (retired : Bool) : Bool :=
  match prior with
  | .absent => false
  | .pack => true
  | .store => retainPluginStoreRecord plugins retired

/-- Only a pack's record is listed as an installed pack and removed as one; a
plugin-store record is removed through its plugins. -/
def pluginNoticeListsPack (prior : PluginNoticePrior) (plugins : List String)
    (retired : Bool) : Bool :=
  prior == .pack && pluginNoticeKeepsRecord prior plugins retired

theorem a_documentless_pack_install_is_not_a_plugin_store_record (retired : Bool) :
    pluginNoticeKeepsRecord .pack [] retired = true ∧
      pluginNoticeListsPack .pack [] retired = true := by
  cases retired <;> decide

theorem pluginRemovalSafe_iff (references removed : List String) :
    pluginRemovalSafe references removed = true ↔
      ∀ reference ∈ references, reference ∉ removed := by
  simp [pluginRemovalSafe, List.all_eq_true]

theorem referenced_plugin_refuses_removal (references removed : List String)
    (reference : String) (hReference : reference ∈ references) (hRemoved : reference ∈ removed) :
    pluginRemovalSafe references removed = false := by
  cases h : pluginRemovalSafe references removed with
  | false => rfl
  | true => exact False.elim ((pluginRemovalSafe_iff references removed).mp h reference hReference hRemoved)

theorem selected_plugin_refuses_retirement (references removed : List String)
    (reference : String) (hReference : reference ∈ references) (hRemoved : reference ∈ removed) :
    pluginRetirementAllowed references removed true = false := by
  have nonempty : removed.isEmpty = false := by
    cases removed with
    | nil => cases hRemoved
    | cons _ _ => rfl
  simp [pluginRetirementAllowed, nonempty,
    referenced_plugin_refuses_removal references removed reference hReference hRemoved]

end Configuration
