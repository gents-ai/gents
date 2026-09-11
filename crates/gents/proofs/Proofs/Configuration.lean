import Proofs.ConfigDefaults
import Mathlib.Data.Rat.Defs

/-!
# Configuration: canonical reference chain and install ownership

Executable formal model of the configuration refactor contracts:

* Install scope fills omitted owners and rejects mismatched owners, while
  preserving explicit foreign delegation identities verbatim. Owners are
  string DIDs (`agent_did`); no separate principal schema is introduced.
* A behavior composes context and inference into one session configuration.
  `Task -> Behavior -> {Context, InferenceProfile -> Backend}` resolution fails
  closed on missing or foreign references and disabled executable documents.
  `resolveTask` is the single Task resolver (owner-checked Task record); graph
  invocation reuses exactly this resolver.
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

/-! ## Owners and install scope -/

/--
Filling install scope: an omitted owner is filled from the install scope; an
explicit owner must match or installation fails. No rewriting of foreign ids.
-/
def fillOwner (scope : String) (declared : Option String) : Option String :=
  match declared with
  | none => some scope
  | some d => if d = scope then some d else none

/-- Accepted owner filling is exactly omission or the declared install scope. -/
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

/-- Exact admission and preservation for every canonical payload, rather than a
special document containing only delegation IDs. -/
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

/-! ## Discovery is credential-scoped observation, never selection -/

/-- Capability projection of one advertised model. Unknown efforts and a known
empty effort catalog remain distinct, matching AdvertisedModel. -/
structure AdvertisedModel where
  modelName : String
  reasoningEfforts : Option (List ReasoningEffort)
  deriving DecidableEq, Repr

/-- Projection of BackendModelCatalog. None is shared backend credentials;
Some DID is the principal whose OAuth credential performed discovery. -/
structure DiscoveryObservation where
  /-- Backend identity inherited from the containing backend observation. -/
  backendId : String
  agentDid : Option String
  models : List AdvertisedModel
  deriving DecidableEq, Repr

/-- The existing Owned wrapper carries the containing backend document's owner,
not another catalog field. Shared credentials (`none`) do not erase backend
ownership. Discovery cannot rewrite inference selection. -/
def catalogFor (owner backendId : String) (credentialScope : Option String)
    (observation : Option (Owned DiscoveryObservation)) : Option DiscoveryObservation :=
  (observation.filter fun catalog => catalog.owner == owner &&
    catalog.value.agentDid == credentialScope && catalog.value.backendId == backendId).map (·.value)

/-- Exact catalog admission covers matching discovery, missing observations, and
foreign owner/backend/credential rejection with one contract. -/
theorem catalogFor_some_iff (owner backendId : String) (scope : Option String)
    (observation : Option (Owned DiscoveryObservation)) (catalog : DiscoveryObservation) :
    catalogFor owner backendId scope observation = some catalog ↔
      observation = some ⟨owner, catalog⟩ ∧ catalog.agentDid = scope ∧ catalog.backendId = backendId := by
  cases observation with
  | none => simp [catalogFor, Option.filter]
  | some observed =>
    cases observed with
    | mk observedOwner value =>
      by_cases ho : observedOwner = owner <;>
        by_cases hs : value.agentDid = scope <;>
        by_cases hb : value.backendId = backendId <;>
        simp_all [catalogFor, Option.filter, Owned.mk.injEq] <;> aesop

/-! ## Absent context and compaction defaults -/

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

/-- Runtime-default compaction: StripThenSummarize at 3/4. -/
def defaultCompaction : CompactionConfig :=
  ⟨.stripThenSummarize, (3 / 4 : Rat)⟩

/-- Omitted or null strategy inside an explicit document uses the same default
as an absent compaction document. Threshold remains an optional authored field. -/
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

/-- A session derives both its context and inference from one behavior. There
is no independent context defaulting or graph-specific model selection path. -/
structure ResolvedSessionConfig where
  context : Context
  inference : SelectedModel
  deriving DecidableEq, Repr

/-! ## One owned reference resolver, one composed session configuration -/

structure Behavior where
  contextId : Option String
  profileId : String
  enabled : Bool
  deriving DecidableEq, Repr

structure Backend where
  enabled : Bool
  deriving DecidableEq, Repr

structure Task where
  behaviorId : String
  enabled : Bool
  deriving DecidableEq, Repr

/-- The profile projection is its required inference selection. Context payloads
have already had their nested same-owner references resolved by the loader.
Lookup keys include agent_did and the authored logical ID. Identical labels
can coexist across principals; returned ownership is still checked against the key. -/
structure Registry where
  tasks : String → String → Option (Owned Task)
  behaviors : String → String → Option (Owned Behavior)
  contexts : String → String → Option (Owned Context)
  profiles : String → String → Option (Owned SelectedModel)
  backends : String → String → Option (Owned Backend)

inductive ResolveError where
  | missingBehavior | disabledBehavior | foreignBehavior
  | missingContext | foreignContext
  | missingProfile | foreignProfile
  | missingBackend | disabledBackend | foreignBackend
  | missingTask | disabledTask | foreignTask
  | missingCapability | callerNotAllowed
  deriving DecidableEq, Repr

/-- Shared reference lookup checks existence and ownership together. Required
fields inside a found document cannot independently disappear. -/
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

/-- A small compositional lemma keeps resolver proofs about the reference chain,
rather than repeated case trees for each failure branch. -/
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

def resolveBehavior (reg : Registry) (scope behaviorId : String) :
    Except ResolveError ResolvedSessionConfig := do
  let behavior ← lookupOwned reg.behaviors scope behaviorId .missingBehavior .foreignBehavior
  if !behavior.enabled then .error .disabledBehavior else do
    let context ← resolveContext reg scope behavior.contextId
    let inference ← lookupOwned reg.profiles scope behavior.profileId .missingProfile .foreignProfile
    let backend ← lookupOwned reg.backends scope inference.backendId .missingBackend .foreignBackend
    if !backend.enabled then .error .disabledBackend
    else .ok ⟨context, inference⟩

/-- The single Task entrance supplies the same combined configuration to ordinary
requests and graph stages. No TaskRun or session lifecycle is introduced here. -/
def resolveTask (reg : Registry) (scope taskId : String) :
    Except ResolveError ResolvedSessionConfig := do
  let task ← lookupOwned reg.tasks scope taskId .missingTask .foreignTask
  if !task.enabled then .error .disabledTask
  else resolveBehavior reg scope task.behaviorId

/-- Complete characterization: a successful session has the exact context and
inference selected by one owned enabled behavior and an owned enabled backend. -/
theorem resolveBehavior_ok_iff (reg : Registry) (scope behaviorId : String)
    (session : ResolvedSessionConfig) :
    resolveBehavior reg scope behaviorId = .ok session ↔
      ∃ behavior backend, reg.behaviors scope behaviorId = some ⟨scope, behavior⟩ ∧
        behavior.enabled = true ∧
        resolveContext reg scope behavior.contextId = .ok session.context ∧
        reg.profiles scope behavior.profileId = some ⟨scope, session.inference⟩ ∧
        reg.backends scope session.inference.backendId = some ⟨scope, backend⟩ ∧
        backend.enabled = true := by
  cases session with
  | mk context inference =>
    simp [resolveBehavior, bind, resolution_bind_ok_iff, ite_eq_iff]
    aesop

theorem resolveTask_ok_iff (reg : Registry) (scope taskId : String)
    (session : ResolvedSessionConfig) :
    resolveTask reg scope taskId = .ok session ↔
      ∃ task, reg.tasks scope taskId = some ⟨scope, task⟩ ∧ task.enabled = true ∧
        resolveBehavior reg scope task.behaviorId = .ok session := by
  simp [resolveTask, bind, resolution_bind_ok_iff, ite_eq_iff]

/-- Omission removes capabilities rather than inheriting another behavior's
context. This applies to successful session resolution, not just a default value. -/
theorem omitted_behavior_context_is_empty (reg : Registry) (scope id : String)
    (behavior : Behavior) (session : ResolvedSessionConfig)
    (hb : reg.behaviors scope id = some ⟨scope, behavior⟩) (ho : behavior.contextId = none)
    (h : resolveBehavior reg scope id = .ok session) : session.context = defaultContext := by
  obtain ⟨found, backend, hf, _, hc, _⟩ := (resolveBehavior_ok_iff reg scope id session).mp h
  have he : found = behavior := by simpa [hb] using hf.symm
  subst found
  simpa [resolveContext, ho] using hc.symm

/-- An owned enabled task delegates to exactly the common behavior resolver. -/
theorem resolveTask_owned_delegates (reg : Registry) (scope id : String) (task : Task)
    (ht : reg.tasks scope id = some ⟨scope, task⟩) (he : task.enabled = true) :
    resolveTask reg scope id = resolveBehavior reg scope task.behaviorId := by
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

theorem disabled_behavior_rejected (reg : Registry) (scope id : String) (behavior : Behavior)
    (hb : reg.behaviors scope id = some ⟨scope, behavior⟩) (he : behavior.enabled = false) :
    resolveBehavior reg scope id = .error .disabledBehavior := by
  simp [resolveBehavior, lookupOwned, hb, he, bind, Except.bind]

theorem behavior_missing_selected_context_rejected (reg : Registry) (scope id key : String)
    (behavior : Behavior) (hb : reg.behaviors scope id = some ⟨scope, behavior⟩)
    (he : behavior.enabled = true) (hr : behavior.contextId = some key)
    (hc : reg.contexts scope key = none) :
    resolveBehavior reg scope id = .error .missingContext := by
  simp [resolveBehavior, resolveContext, lookupOwned, hb, he, hr, hc, bind, Except.bind]

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

/-- Auth is explicit. OAuth lookup uses the executing principal and existing
credential owner; the backend contains no copied OAuth tokens or host identity. -/
inductive BackendAuth where
  | unauthenticated
  | apiKey (key : String)
  | environment (name : String)
  | principalOAuth
  deriving DecidableEq, Repr

def oauthLookupOwner (scope : String) : BackendAuth → Option String
  | .principalOAuth => some scope
  | _ => none

theorem oauth_uses_execution_scope (scope : String) :
    oauthLookupOwner scope .principalOAuth = some scope := rfl

/-! ## Backend capacity defaults -/

/-- Resolved backend limits; authored optional signed integers are validated below. -/
structure BackendControls where
  connectionTimeoutSecs : Nat
  discoveryTimeoutSecs : Nat
  maxConcurrent : Nat
  queuedLimit : Nat
deriving DecidableEq, Repr

/-- Documented defaults: 10s timeout, 1 concurrent request, 100 queued. -/
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

end Configuration
