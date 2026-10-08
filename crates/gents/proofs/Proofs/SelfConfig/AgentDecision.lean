import Proofs.SelfConfig.Theorems
import Proofs.Configuration

namespace SelfConfig

/-- Read-only projection of the node's Agent documents for create, edit and
disable decisions. `protectedIds` are product-owned agents (the Engineer): they
may be cloned but never edited or disabled through agent management, so a
recovery/configuration agent stays available. `defaultId` is the node's default
agent; publication rejects a disabled default, so disabling it is refused here
rather than admitted and left to fail at every reconcile. -/
structure AgentCatalog where
  agents : List (String × Bool)
  protectedIds : List String
  defaultId : Option String
  deriving DecidableEq, Repr

inductive AgentOp where
  | create
  | edit
  | disable
  deriving DecidableEq, Repr

def AgentCatalog.present (c : AgentCatalog) (id : String) : Bool :=
  c.agents.any (fun a => a.1 == id)

def AgentCatalog.mutable (c : AgentCatalog) (id : String) : Bool :=
  !c.protectedIds.contains id

/-- A new agent takes an unused id. -/
def agentCreateDecision (c : AgentCatalog) (target : String) : Bool :=
  !c.present target

def agentEditDecision (c : AgentCatalog) (target : String) : Bool :=
  c.present target && c.mutable target

/-- `makeDefault` is the same request designating the target as default. -/
def agentDisableDecision (c : AgentCatalog) (target : String)
    (makeDefault : Bool) : Bool :=
  c.present target && c.mutable target && !makeDefault
    && decide (some target ≠ c.defaultId)

def agentDecision (c : AgentCatalog) (op : AgentOp) (target : String)
    (makeDefault : Bool) : Bool :=
  match op with
  | .create => agentCreateDecision c target
  | .edit => agentEditDecision c target
  | .disable => agentDisableDecision c target makeDefault

theorem default_disable_rejected (c : AgentCatalog) (target : String)
    (makeDefault : Bool) (h : c.defaultId = some target) :
    agentDisableDecision c target makeDefault = false := by
  simp [agentDisableDecision, h]

theorem protected_edit_or_disable_rejected (c : AgentCatalog) (op : AgentOp)
    (target : String) (makeDefault : Bool) (hop : op ≠ .create)
    (hp : target ∈ c.protectedIds) :
    agentDecision c op target makeDefault = false := by
  cases op <;> simp_all [agentDecision, agentEditDecision, agentDisableDecision,
    AgentCatalog.mutable]

theorem create_requires_unused_id (c : AgentCatalog) (target : String)
    (h : c.present target = true) : agentCreateDecision c target = false := by
  simp [agentCreateDecision, h]

theorem unprotected_edit_accepted (c : AgentCatalog) (target : String)
    (hpresent : c.present target = true) (hp : target ∉ c.protectedIds) :
    agentEditDecision c target = true := by
  simp [agentEditDecision, AgentCatalog.mutable, hpresent, hp]

/-! ## Create input -/

/-- Inputs the agent create decision inspects beyond the id. `cloneFrom` is
empty for a fresh agent. -/
structure AgentCreateInput where
  name : String
  systemPrompt : String
  profile : String
  cloneFrom : String := ""
  deriving DecidableEq, Repr

def AgentCatalog.enabled (c : AgentCatalog) (id : String) : Bool :=
  c.agents.contains (id, true)

/-- No implicit profile exists: create names a nonblank published inference
profile. A fresh agent carries operating instructions; a clone may inherit its
source prompt and must name an enabled source. -/
def agentCreateInputOk (profiles : List String) (c : AgentCatalog)
    (i : AgentCreateInput) : Bool :=
  i.name != "" && i.profile.trim != "" && profiles.contains i.profile
    && (i.cloneFrom != "" || i.systemPrompt.trim != "")
    && (i.cloneFrom == "" || c.enabled i.cloneFrom)

theorem blank_create_profile_rejected (profiles : List String) (c : AgentCatalog)
    (i : AgentCreateInput) (h : i.profile.trim = "") :
    agentCreateInputOk profiles c i = false := by
  simp [agentCreateInputOk, h]

theorem unpublished_create_profile_rejected (profiles : List String) (c : AgentCatalog)
    (i : AgentCreateInput) (h : i.profile ∉ profiles) :
    agentCreateInputOk profiles c i = false := by
  simp [agentCreateInputOk, h]

theorem fresh_create_requires_prompt (profiles : List String) (c : AgentCatalog)
    (i : AgentCreateInput) (hfresh : i.cloneFrom = "") (h : i.systemPrompt.trim = "") :
    agentCreateInputOk profiles c i = false := by
  simp [agentCreateInputOk, hfresh, h]

theorem clone_requires_enabled_source (profiles : List String) (c : AgentCatalog)
    (i : AgentCreateInput) (hclone : i.cloneFrom ≠ "")
    (h : c.enabled i.cloneFrom = false) :
    agentCreateInputOk profiles c i = false := by
  simp [agentCreateInputOk, hclone, h]

/-! ## Edit patches

An edit is a self-config patch: an absent key is `omitted`, `PatchOp.clear`
clears and `PatchOp.set` replaces, so no separate field-update vocabulary
exists. -/

/-- Admission of one edited field. `none` is an omitted field. -/
def editNameOk : Option PatchOp → Bool
  | some (.set n) => n != ""
  | _ => true

def editPromptOk : Option PatchOp → Bool
  | some (.set p) => p.trim != ""
  | _ => true

/-- The profile binding cannot be cleared; a set names a published profile. -/
def editProfileOk (profiles : List String) : Option PatchOp → Bool
  | none => true
  | some .clear => false
  | some (.set p) => p.trim != "" && profiles.contains p

theorem omitted_edit_fields_admitted (profiles : List String) :
    editNameOk none = true ∧ editPromptOk none = true ∧
      editProfileOk profiles none = true := by
  simp [editNameOk, editPromptOk, editProfileOk]

theorem profile_clear_rejected (profiles : List String) :
    editProfileOk profiles (some .clear) = false := rfl

/-- A key absent from the patch keeps its stored value. -/
theorem omitted_field_preserved (t : Target) (doc : Doc) (p : Patch) (k : FieldKey)
    (h : p.any (fun e => e.key == k) = false) : applyPatch t doc p k = doc k := by
  by_contra hne
  have := (containment t doc p k hne).2
  rw [h] at this
  exact absurd this (by simp)

theorem explicit_clear_removes_writable (t : Target) (doc : Doc) (k : FieldKey)
    (hw : k ∈ writableFields t) : applyPatch t doc [⟨k, .clear⟩] k = none := by
  simp [applyPatch, applyEntry, hw, PatchOp.value]

theorem explicit_set_replaces_writable (t : Target) (doc : Doc) (k : FieldKey)
    (v : FieldValue) (hw : k ∈ writableFields t) :
    applyPatch t doc [⟨k, .set v⟩] k = some v := by
  simp [applyPatch, applyEntry, hw, PatchOp.value]

/-- A single-field edit (profile-only, name-only) leaves every other field. -/
theorem single_field_edit_preserves_others (t : Target) (doc : Doc) (e : PatchEntry)
    (k : FieldKey) (h : k ≠ e.key) : applyPatch t doc [e] k = doc k := by
  show applyEntry t doc e k = doc k
  unfold applyEntry
  split <;> simp [h]

/-! ## Default selection and materialization -/

/-- Default selection is part of the same admitted create/edit publication and
points the node at the separately authored target. -/
def defaultAgentAfter (preDefault applied : String) (op : AgentOp)
    (makeDefault : Bool) : String :=
  if op ≠ .disable ∧ makeDefault = true then applied else preDefault

theorem requested_promotion_selects_applied_agent (preDefault applied : String)
    (op : AgentOp) (hop : op ≠ .disable) :
    defaultAgentAfter preDefault applied op true = applied := by
  simp [defaultAgentAfter, hop]

theorem omitted_promotion_keeps_default (preDefault applied : String) (op : AgentOp) :
    defaultAgentAfter preDefault applied op false = preDefault := by
  simp [defaultAgentAfter]

/-- A create or edit result must resolve as one context-plus-inference
configuration, supplied by the shared authoring loader as a candidate registry.
Disable is admitted but starts no session. Candidate construction is a loader
refinement obligation. -/
def materializedAgent (c : AgentCatalog) (op : AgentOp) (target : String)
    (makeDefault : Bool) (candidate : Configuration.Registry) (node : String) :
    Option Configuration.ResolvedSessionConfig :=
  if agentDecision c op target makeDefault = true ∧ op ≠ .disable then
    (Configuration.resolveAgent candidate node target).toOption
  else none

theorem materializedAgent_iff (c : AgentCatalog) (op : AgentOp) (target : String)
    (makeDefault : Bool) (candidate : Configuration.Registry) (node : String)
    (session : Configuration.ResolvedSessionConfig) :
    materializedAgent c op target makeDefault candidate node = some session ↔
      agentDecision c op target makeDefault = true ∧ op ≠ .disable ∧
      Configuration.resolveAgent candidate node target = .ok session := by
  unfold materializedAgent
  by_cases h : agentDecision c op target makeDefault = true ∧ op ≠ .disable
  · simp only [if_pos h]
    cases hr : Configuration.resolveAgent candidate node target <;>
      simp_all [Except.toOption]
  · simp only [if_neg h, reduceCtorEq]
    tauto

theorem rejected_decision_materializes_nothing (c : AgentCatalog) (op : AgentOp)
    (target : String) (makeDefault : Bool) (candidate : Configuration.Registry)
    (node : String) (h : agentDecision c op target makeDefault = false) :
    materializedAgent c op target makeDefault candidate node = none := by
  simp [materializedAgent, h]

theorem disable_materializes_nothing (c : AgentCatalog) (target : String)
    (makeDefault : Bool) (candidate : Configuration.Registry) (node : String) :
    materializedAgent c .disable target makeDefault candidate node = none := by
  simp [materializedAgent]

end SelfConfig
