import Proofs.ConfigDefaults
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
  agentId : String
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

def resolveAgent (reg : Registry) (scope agentId : String) :
    Except ResolveError ResolvedSessionConfig := do
  let agent ← lookupOwned reg.agents scope agentId .missingAgent .foreignAgent
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
  else resolveAgent reg scope task.agentId

theorem resolveAgent_ok_iff (reg : Registry) (scope agentId : String)
    (session : ResolvedSessionConfig) :
    resolveAgent reg scope agentId = .ok session ↔
      ∃ agent backend, reg.agents scope agentId = some ⟨scope, agent⟩ ∧
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
        resolveAgent reg scope task.agentId = .ok session := by
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
    resolveTask reg scope id = resolveAgent reg scope task.agentId := by
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

/-- A node's `default_agent_id` selects the agent for requests that
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
account is first stored; `none` is the provider's original account. -/
inductive BackendAuth where
  | unauthenticated
  | apiKey (key : String)
  | environment (name : String)
  | nodeOAuth (account : Option String)
  deriving DecidableEq, Repr

def oauthLookupOwner (scope : String) : BackendAuth → Option String
  | .nodeOAuth _ => some scope
  | _ => none

theorem oauth_uses_execution_scope (scope : String) (account : Option String) :
    oauthLookupOwner scope (.nodeOAuth account) = some scope := rfl

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

structure BackendControls where
  connectionTimeoutSecs : Nat
  discoveryTimeoutSecs : Nat
  maxConcurrent : Nat
  queuedLimit : Nat
deriving DecidableEq, Repr

def defaultBackendControls : BackendControls := ⟨10, 10, 1, 100⟩

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
          have hb3 := ConfigDefaults.resolveNat_lower_bound 1 1 concurrent n (by decide) hn
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
    resolveBackendControls none none none (some 0) = some ⟨10, 10, 1, 0⟩ := rfl

theorem backend_negative_queue_rejected (queue : Int) (h : queue < 0) :
    resolveBackendControls none none none (some queue) = none := by
  have hn : ¬ (0 : Int) ≤ queue := by omega
  simp [resolveBackendControls, defaultBackendControls, ConfigDefaults.resolveNat, hn]

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

/-- An installed pin is rechecked while holding the store lock through the
document commit, so a concurrent publisher cannot change its artifact between
the check and the corresponding configuration write. -/
def installedPinCommitAllowed (pin : Option String) (underLock : String) : Bool :=
  pluginPinMatches pin underLock

/-- Wiring holds the shared plugin store lock from reading the digest through
configuration publication. Competing store mutation cannot publish in that interval. -/
def wireAndPluginPublishAllowed (wireHoldsLock : Bool) : Bool := !wireHoldsLock

theorem plugin_publish_blocked_during_wire :
    wireAndPluginPublishAllowed true = false := by decide

/-- Publishing a plugin pin requires an embedded transaction on the runtime's
own store. A preview or removal does not publish a new plugin reference. -/
def wireTargetAllowed (localTarget preview remove : Bool) : Bool :=
  localTarget || preview || remove

theorem graphql_wire_cannot_publish_pin : wireTargetAllowed false false false = false := by decide

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

end Configuration
