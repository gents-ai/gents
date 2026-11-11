import Proofs.Tools.SelfConfig.Theorems
import Proofs.Tools.SelfConfig.CategoryGrants
import Proofs.Documents.Configuration

/-!
# Agent operations: one admission for every front end

Create, clone, edit, disable and choosing the default agent are decided by
`agentAdmission`, whoever asks: the model's config tool, the CLI, the desktop
or any later adapter. The decision reads the acting actor's ACP grants
(`SelfConfig.ActorGrants`), the node's Agent catalog and the request; it reads
nothing else about the actor and nothing about the front end.

Decision (maintainer, 2026-11-11, #3656): protect only against lockout. No
agent is protected by name, tag or role: the Engineer is configured like any
other Agent. What remains are the rules that keep the node working: the
default agent stays enabled, and an actor cannot disable itself.

Rust owner: `crates/gents/src/tools/builtin/self_config/agent.rs`
(`decide_agent_operation`), moving to the operations layer
(`crates/gents/src/operations/agent.rs`).
-/

namespace SelfConfig

/-- One Agent of the node's catalog. `tags` are carried so that the admission
can be shown not to read them (`admission_ignores_catalog_tags`). -/
structure AgentEntry where
  id : String
  enabled : Bool
  tags : List String := []
  deriving DecidableEq, Repr

structure AgentCatalog where
  agents : List AgentEntry
  defaultId : Option String
  deriving DecidableEq, Repr

def AgentCatalog.present (c : AgentCatalog) (id : String) : Bool :=
  c.agents.any (·.id == id)

def AgentCatalog.enabled (c : AgentCatalog) (id : String) : Bool :=
  c.agents.any (fun a => a.id == id && a.enabled)

/-! ## Inputs -/

structure AgentCreateInput where
  name : String
  systemPrompt : String
  profile : String
  cloneFrom : String := ""
  deriving DecidableEq, Repr

/-- No implicit profile exists: create names a nonblank published inference
profile. A fresh agent needs a prompt; a clone needs an enabled source. -/
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

def editNameOk : Option PatchOp → Bool
  | some (.set n) => n != ""
  | _ => true

def editPromptOk : Option PatchOp → Bool
  | some (.set p) => p.trim != ""
  | _ => true

def editProfileOk (profiles : List String) : Option PatchOp → Bool
  | none => true
  | some .clear => false
  | some (.set p) => p.trim != "" && profiles.contains p

/-- An Agent may select no Context (`Configuration.resolveAgent` accepts an
absent one); a selected Context is named by a nonblank ID. Whether it exists is
the reference validator's decision. -/
def editContextOk : Option PatchOp → Bool
  | some (.set id) => id.trim != ""
  | _ => true

theorem omitted_edit_fields_admitted (profiles : List String) :
    editNameOk none = true ∧ editPromptOk none = true ∧
      editProfileOk profiles none = true ∧ editContextOk none = true := by
  simp [editNameOk, editPromptOk, editProfileOk, editContextOk]

theorem profile_clear_rejected (profiles : List String) :
    editProfileOk profiles (some .clear) = false := rfl

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

theorem single_field_edit_preserves_others (t : Target) (doc : Doc) (e : PatchEntry)
    (k : FieldKey) (h : k ≠ e.key) : applyPatch t doc [e] k = doc k := by
  show applyEntry t doc e k = doc k
  unfold applyEntry
  split <;> simp [h]

/-- The one agent edit: every writable Agent field (`writableFields .agent`)
plus `system_prompt`, which writes the Agent's selected Context. Disabling is
`enabled := false`, whichever verb carries it. -/
structure AgentEditFields where
  name : Option PatchOp := none
  description : Option PatchOp := none
  systemPrompt : Option PatchOp := none
  profile : Option PatchOp := none
  contextId : Option PatchOp := none
  enabled : Option Bool := none
  tags : Option PatchOp := none
  deriving DecidableEq, Repr

/-- The Agent document fields the edit writes, and `system_prompt`, which its
Context carries. -/
def agentEditKeys : List FieldKey :=
  ["display_name", "description", "system_prompt", "inference_profile_id", "context_id",
   "enabled", "tags"]

/-- The edit covers exactly the Agent's writable fields, and `system_prompt`. -/
theorem agent_edit_covers_writable_fields :
    writableFields .agent ++ ["system_prompt"] = ["display_name", "description", "context_id",
      "inference_profile_id", "enabled", "tags", "system_prompt"] ∧
    (writableFields .agent ++ ["system_prompt"]).all agentEditKeys.contains = true ∧
    agentEditKeys.all (writableFields .agent ++ ["system_prompt"]).contains = true := by
  decide

def AgentEditFields.writesAgent (f : AgentEditFields) : Bool :=
  f.name.isSome || f.description.isSome || f.systemPrompt.isSome || f.profile.isSome
    || f.contextId.isSome || f.enabled.isSome || f.tags.isSome

def agentEditFieldsOk (profiles : List String) (f : AgentEditFields) : Bool :=
  editNameOk f.name && editPromptOk f.systemPrompt && editProfileOk profiles f.profile
    && editContextOk f.contextId

/-- `disable` is sugar for the edit `enabled := false`; choosing the default is
an edit with `makeDefault` (usually with no fields). -/
inductive AgentOperation where
  | create (input : AgentCreateInput)
  | edit (fields : AgentEditFields)
  | disable
  deriving DecidableEq, Repr

def disableFields : AgentEditFields := { enabled := some false }

def AgentOperation.normalize : AgentOperation → AgentOperation
  | .disable => .edit disableFields
  | op => op

structure AgentRequest where
  operation : AgentOperation
  target : String
  makeDefault : Bool := false
  deriving DecidableEq, Repr

def chooseDefault (target : String) : AgentRequest :=
  { operation := .edit {}, target, makeDefault := true }

/-! ## The actor -/

inductive ActorKind where
  | person
  | agent
  deriving DecidableEq, Repr

/-- The front end that carried the request. Admission never reads it. -/
inductive FrontEnd where
  | tool
  | cli
  | desktop
  deriving DecidableEq, Repr

structure Actor where
  did : String
  kind : ActorKind
  grants : ActorGrants
  deriving DecidableEq, Repr

/-! ## The state after the request -/

/-- The `enabled` value the request writes on its target, if any. -/
def AgentRequest.writesEnabled (r : AgentRequest) : Option Bool :=
  match r.operation.normalize with
  | .create _ => some true
  | .edit f => f.enabled
  | .disable => some false

def enabledAfter (c : AgentCatalog) (r : AgentRequest) (id : String) : Bool :=
  if id = r.target then r.writesEnabled.getD (c.enabled id) else c.enabled id

def defaultAfter (c : AgentCatalog) (r : AgentRequest) : Option String :=
  if r.makeDefault then some r.target else c.defaultId

/-- Which documents the request writes, and so which categories it needs: a
new Agent and the default need the catalog; an edit of an existing Agent's
fields needs the agent category on that Agent. Choosing the default writes the
Node's `DefaultAgentSelection` document (`SelfConfig.initDefaultSelection`),
whose update is exactly the catalog
(`SelfConfig.default_selection_write_is_the_catalog`). -/
def AgentRequest.needsCatalog (r : AgentRequest) : Bool :=
  r.makeDefault || match r.operation.normalize with
    | .create _ => true
    | _ => false

def AgentRequest.needsAgent (r : AgentRequest) : Bool :=
  match r.operation.normalize with
  | .edit f => f.writesAgent
  | _ => false

/-! ## Admission -/

/-- Why a request is refused. Each refusal names what the caller can change. -/
inductive AgentRefusal where
  /-- The actor lacks the catalog's grants (`nodes manage_agents`). -/
  | catalogNotHeld
  /-- The actor lacks the agent category on the target (`agents update`). -/
  | agentNotHeld
  | targetExists
  | targetMissing
  | invalidInput
  /-- The request would leave the node's default agent disabled. -/
  | defaultDisabled
  /-- The request would disable the acting actor itself. -/
  | selfLockout
  deriving DecidableEq, Repr

/-- The result of admission.

Decision (maintainer, 2026-11-11, #3656): a refused preview is a successful
answer, not an error: it reports the refusal and carries the same recovery (the
next call) that the refused commit's error would carry. The refusal is computed
once, here, for both (`preview_refusal_matches_commit`). -/
abbrev AgentAdmission := Except AgentRefusal Unit

/-- The categories the request needs, read from ACP: the catalog for a new
Agent or the Node's default, the agent category on an edited target. -/
def catalogCheck (actor : Actor) (r : AgentRequest) : List (Bool × AgentRefusal) :=
  [(!r.needsCatalog || actor.grants.catalog, .catalogNotHeld)]

def agentCheck (actor : Actor) (r : AgentRequest) : List (Bool × AgentRefusal) :=
  [(!r.needsAgent || actor.grants.configures.contains r.target, .agentNotHeld)]

def operationChecks (profiles : List String) (c : AgentCatalog) (r : AgentRequest) :
    List (Bool × AgentRefusal) :=
  match r.operation.normalize with
  | .create i => [(!c.present r.target, .targetExists), (agentCreateInputOk profiles c i, .invalidInput)]
  | .edit f => [(c.present r.target, .targetMissing), (agentEditFieldsOk profiles f, .invalidInput)]
  | .disable => []

/-- The only protections: the default agent stays enabled, and the actor does
not disable itself. -/
def lockoutChecks (c : AgentCatalog) (actor : Actor) (r : AgentRequest) :
    List (Bool × AgentRefusal) :=
  [ (!(defaultAfter c r == some r.target && !enabledAfter c r r.target), .defaultDisabled)
  , (!(r.target == actor.did && !enabledAfter c r r.target), .selfLockout) ]

/-- Ordered: the catalog, the target's existence and the input, the agent
category on the target, then the lockout rules. The first failing check is the
refusal. The catalog is read as the actor, so an Agent hidden from it is absent
(`Identity.Household.self_config_absent_and_hidden_are_indistinguishable`). -/
def admissionChecks (profiles : List String) (c : AgentCatalog) (actor : Actor)
    (r : AgentRequest) : List (Bool × AgentRefusal) :=
  catalogCheck actor r ++ operationChecks profiles c r ++ agentCheck actor r
    ++ lockoutChecks c actor r

/-- The single admission owner for agent operations. -/
def agentAdmission (profiles : List String) (c : AgentCatalog) (actor : Actor)
    (r : AgentRequest) : AgentAdmission :=
  match (admissionChecks profiles c actor r).find? (fun x => !x.1) with
  | some (_, refusal) => .error refusal
  | none => .ok ()

def admitted (profiles : List String) (c : AgentCatalog) (actor : Actor)
    (r : AgentRequest) : Bool :=
  (admissionChecks profiles c actor r).all (·.1)

theorem agentAdmission_ok_iff (profiles : List String) (c : AgentCatalog) (actor : Actor)
    (r : AgentRequest) :
    agentAdmission profiles c actor r = .ok () ↔ admitted profiles c actor r = true := by
  unfold agentAdmission admitted
  cases hf : (admissionChecks profiles c actor r).find? (fun x => !x.1) with
  | some x =>
      have hmem := List.mem_of_find?_eq_some hf
      have hx := List.find?_some hf
      simp only [reduceCtorEq, false_iff, List.all_eq_true, not_forall]
      exact ⟨x, hmem, by simpa using hx⟩
  | none =>
      simp only [true_iff, List.all_eq_true]
      intro x hx
      have := List.find?_eq_none.mp hf x hx
      simpa using this

/-- Every front end's adapter decides through `agentAdmission`. -/
def frontEndAdmission (_frontEnd : FrontEnd) (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (r : AgentRequest) : AgentAdmission :=
  agentAdmission profiles c actor r

/-! ## Properties -/

/-- The facts every admitted request satisfies. -/
theorem admitted_facts (profiles : List String) (c : AgentCatalog) (actor : Actor)
    (r : AgentRequest) (h : admitted profiles c actor r = true) :
    (r.needsCatalog = true → actor.grants.catalog = true) ∧
    (r.needsAgent = true → r.target ∈ actor.grants.configures) ∧
    (defaultAfter c r = some r.target → enabledAfter c r r.target = true) ∧
    (r.target = actor.did → enabledAfter c r r.target = true) := by
  simp only [admitted, admissionChecks, List.all_append, Bool.and_eq_true] at h
  obtain ⟨⟨⟨hc, -⟩, ha⟩, hl⟩ := h
  simp only [catalogCheck, agentCheck, lockoutChecks, List.all_cons, List.all_nil,
    Bool.and_true, Bool.and_eq_true] at hc ha hl
  refine ⟨?_, ?_, ?_, ?_⟩
  · intro hn; simpa [hn] using hc
  · intro hn; simpa [hn] using ha
  · intro hd; simpa [hd] using hl.1
  · intro hs; simpa [hs] using hl.2

theorem admission_ignores_front_end (a b : FrontEnd) (profiles : List String)
    (c : AgentCatalog) (actor : Actor) (r : AgentRequest) :
    frontEndAdmission a profiles c actor r = frontEndAdmission b profiles c actor r := rfl

/-- Admission does not depend on whether the actor is a person or an Agent. -/
theorem admission_ignores_actor_kind (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (k : ActorKind) (r : AgentRequest) :
    agentAdmission profiles c { actor with kind := k } r = agentAdmission profiles c actor r :=
  rfl

/-- Two actors with the same DID and the same grants are decided alike. -/
theorem admission_reads_only_grants (profiles : List String) (c : AgentCatalog)
    (a b : Actor) (r : AgentRequest) (hd : a.did = b.did) (hg : a.grants = b.grants) :
    agentAdmission profiles c a r = agentAdmission profiles c b r := by
  simp [agentAdmission, admissionChecks, catalogCheck, agentCheck, lockoutChecks, hd, hg]

/-- Without the catalog's grants, create, clone and choosing the default are
refused, whoever the actor is. -/
theorem catalog_refused_without_grants (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (r : AgentRequest) (hneed : r.needsCatalog = true)
    (hheld : actor.grants.catalog = false) :
    agentAdmission profiles c actor r = .error .catalogNotHeld := by
  simp [agentAdmission, admissionChecks, catalogCheck, hneed, hheld]

/-- Without the agent category on the target, an edit or disable is refused,
whoever the actor is. -/
theorem agent_refused_without_grants (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (r : AgentRequest) (hneed : r.needsAgent = true)
    (hheld : r.target ∉ actor.grants.configures) :
    admitted profiles c actor r = false := by
  cases h : admitted profiles c actor r
  · rfl
  · exact absurd ((admitted_facts profiles c actor r h).2.1 hneed) hheld

/-- With every check but the agent category passing, that check is the
refusal. -/
theorem agent_refusal_names_the_category (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (r : AgentRequest)
    (hpre : (catalogCheck actor r ++ operationChecks profiles c r).all (·.1) = true)
    (hneed : r.needsAgent = true) (hheld : r.target ∉ actor.grants.configures) :
    agentAdmission profiles c actor r = .error .agentNotHeld := by
  have hnone : (catalogCheck actor r ++ operationChecks profiles c r).find? (fun x => !x.1)
      = none := by
    rw [List.find?_eq_none]
    intro x hx
    have := List.all_eq_true.mp hpre x hx
    simp [this]
  have heq : admissionChecks profiles c actor r =
      (catalogCheck actor r ++ operationChecks profiles c r) ++
        (agentCheck actor r ++ lockoutChecks c actor r) := by
    simp [admissionChecks, List.append_assoc]
  simp only [agentAdmission, heq, List.find?_append, hnone]
  simp [agentCheck, hneed, hheld]

theorem disable_needs_agent_category (target : String) :
    ({ operation := .disable, target } : AgentRequest).needsAgent = true := rfl

theorem create_needs_catalog (i : AgentCreateInput) (target : String) (makeDefault : Bool) :
    ({ operation := .create i, target, makeDefault } : AgentRequest).needsCatalog = true := by
  cases makeDefault <;> rfl

theorem choose_default_needs_catalog (target : String) :
    (chooseDefault target).needsCatalog = true := rfl

/-- Disable is the edit `enabled := false`: both verbs are one decision. -/
theorem disable_is_enabled_false_edit (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (target : String) (makeDefault : Bool) :
    agentAdmission profiles c actor { operation := .disable, target, makeDefault } =
      agentAdmission profiles c actor { operation := .edit disableFields, target, makeDefault } :=
  rfl

/-- No admitted request leaves the node without an enabled default agent: if
the default was enabled before, the default after is enabled. -/
theorem admitted_keeps_default_enabled (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (r : AgentRequest) (d : String) (hd : c.defaultId = some d)
    (hen : c.enabled d = true) (h : admitted profiles c actor r = true) :
    ∃ d', defaultAfter c r = some d' ∧ enabledAfter c r d' = true := by
  have f := admitted_facts profiles c actor r h
  by_cases hm : r.makeDefault = true
  · exact ⟨r.target, by simp [defaultAfter, hm], f.2.2.1 (by simp [defaultAfter, hm])⟩
  · have hda : defaultAfter c r = some d := by simp [defaultAfter, hm, hd]
    refine ⟨d, hda, ?_⟩
    by_cases ht : d = r.target
    · subst ht; exact f.2.2.1 hda
    · simp [enabledAfter, ht, hen]

/-- The default agent cannot be disabled, by any verb. -/
theorem default_disable_refused (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (r : AgentRequest) (hd : defaultAfter c r = some r.target)
    (hoff : r.writesEnabled = some false) :
    admitted profiles c actor r = false := by
  cases h : admitted profiles c actor r
  · rfl
  · have := (admitted_facts profiles c actor r h).2.2.1 hd
    simp [enabledAfter, hoff] at this

/-- A disable cannot also make its target the default. -/
theorem disable_making_default_refused (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (target : String) :
    admitted profiles c actor { operation := .disable, target, makeDefault := true } = false :=
  default_disable_refused profiles c actor _ (by simp [defaultAfter]) rfl

/-- No lockout: an admitted request never disables the acting actor. Agent
operations publish only the creation relations (`creationRelations`) and revoke
no ACP relation, so the actor's grants are not reduced either; with a
negation-free policy (`Identity.Household.policy`) adding relations removes no
permission. -/
theorem admitted_keeps_actor_enabled (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (r : AgentRequest) (hen : c.enabled actor.did = true)
    (h : admitted profiles c actor r = true) :
    enabledAfter c r actor.did = true := by
  have f := admitted_facts profiles c actor r h
  by_cases ht : actor.did = r.target
  · rw [ht]; exact f.2.2.2 ht.symm
  · simp [enabledAfter, ht, hen]

theorem self_disable_admission_refused (profiles : List String) (c : AgentCatalog) (actor : Actor)
    (r : AgentRequest) (hs : r.target = actor.did) (hoff : r.writesEnabled = some false) :
    admitted profiles c actor r = false := by
  cases h : admitted profiles c actor r
  · rfl
  · have := (admitted_facts profiles c actor r h).2.2.2 hs
    simp [enabledAfter, hoff] at this

/-- Nothing in admission reads an Agent's tags, so no tag (the Engineer's
`gents:engineer` included) protects an Agent or widens an actor. -/
theorem admission_ignores_catalog_tags (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (r : AgentRequest) (retag : AgentEntry → List String) :
    agentAdmission profiles { c with agents := c.agents.map fun a => { a with tags := retag a } }
      actor r = agentAdmission profiles c actor r := by
  have hp : ∀ id, AgentCatalog.present
      { c with agents := c.agents.map fun a => { a with tags := retag a } } id =
      c.present id := by
    intro id; simp [AgentCatalog.present, Function.comp_def]
  have he : ∀ id, AgentCatalog.enabled
      { c with agents := c.agents.map fun a => { a with tags := retag a } } id =
      c.enabled id := by
    intro id; simp [AgentCatalog.enabled, Function.comp_def]
  simp [agentAdmission, admissionChecks, operationChecks, lockoutChecks, agentCreateInputOk,
    enabledAfter, defaultAfter, hp, he]

/-- Nor does an edit's own tag value change its decision. -/
theorem admission_ignores_edited_tags (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (f : AgentEditFields) (t₁ t₂ : PatchOp) (target : String)
    (makeDefault : Bool) :
    agentAdmission profiles c actor
        { operation := .edit { f with tags := some t₁ }, target, makeDefault } =
      agentAdmission profiles c actor
        { operation := .edit { f with tags := some t₂ }, target, makeDefault } := by
  simp [agentAdmission, admissionChecks, catalogCheck, agentCheck, operationChecks,
    lockoutChecks, AgentOperation.normalize, AgentRequest.needsCatalog, AgentRequest.needsAgent,
    AgentEditFields.writesAgent, agentEditFieldsOk, enabledAfter, defaultAfter,
    AgentRequest.writesEnabled]

theorem create_requires_unused_id (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (i : AgentCreateInput) (target : String) (makeDefault : Bool)
    (h : c.present target = true) :
    admitted profiles c actor { operation := .create i, target, makeDefault } = false := by
  simp [admitted, admissionChecks, operationChecks, AgentOperation.normalize, h]

/-! ## Preview and commit -/

inductive Mode where
  | preview
  | commit
  deriving DecidableEq, Repr

/-- What the caller receives. A refused preview is a successful answer that
carries the refusal; a refused commit is an error carrying the same refusal. -/
inductive AgentOutcome where
  | admitted
  | previewRefused (refusal : AgentRefusal)
  | commitRefused (refusal : AgentRefusal)
  deriving DecidableEq, Repr

def AgentOutcome.succeeded : AgentOutcome → Bool
  | .commitRefused _ => false
  | _ => true

def AgentOutcome.refusal : AgentOutcome → Option AgentRefusal
  | .admitted => none
  | .previewRefused r | .commitRefused r => some r

def agentOutcome (mode : Mode) : AgentAdmission → AgentOutcome
  | .ok () => .admitted
  | .error r => match mode with
    | .preview => .previewRefused r
    | .commit => .commitRefused r

theorem preview_refusal_matches_commit (a : AgentAdmission) :
    (agentOutcome .preview a).refusal = (agentOutcome .commit a).refusal ∧
      (agentOutcome .preview a).succeeded = true := by
  cases a with
  | ok u => cases u; simp [agentOutcome, AgentOutcome.refusal, AgentOutcome.succeeded]
  | error r => simp [agentOutcome, AgentOutcome.refusal, AgentOutcome.succeeded]

/-! ## Materialization -/

/-- A create or edit result resolves as one context-plus-inference
configuration from the shared authoring loader's candidate registry; a request
that leaves its target disabled is admitted but starts no session. Candidate
construction is a loader refinement obligation. -/
def materializedAgent (profiles : List String) (c : AgentCatalog) (actor : Actor)
    (r : AgentRequest) (candidate : Configuration.Registry) (node : String) :
    Option Configuration.ResolvedSessionConfig :=
  if admitted profiles c actor r = true ∧ enabledAfter c r r.target = true then
    (Configuration.resolveAgent candidate node r.target).toOption
  else none

theorem materializedAgent_iff (profiles : List String) (c : AgentCatalog) (actor : Actor)
    (r : AgentRequest) (candidate : Configuration.Registry) (node : String)
    (session : Configuration.ResolvedSessionConfig) :
    materializedAgent profiles c actor r candidate node = some session ↔
      admitted profiles c actor r = true ∧ enabledAfter c r r.target = true
        ∧ Configuration.resolveAgent candidate node r.target = .ok session := by
  unfold materializedAgent
  by_cases h : admitted profiles c actor r = true ∧ enabledAfter c r r.target = true
  · simp only [if_pos h]
    cases hr : Configuration.resolveAgent candidate node r.target <;>
      simp_all [Except.toOption]
  · simp only [if_neg h, reduceCtorEq]
    tauto

theorem rejected_request_materializes_nothing (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (r : AgentRequest) (candidate : Configuration.Registry) (node : String)
    (h : admitted profiles c actor r = false) :
    materializedAgent profiles c actor r candidate node = none := by
  simp [materializedAgent, h]

theorem disable_materializes_nothing (profiles : List String) (c : AgentCatalog)
    (actor : Actor) (target : String) (makeDefault : Bool)
    (candidate : Configuration.Registry) (node : String) :
    materializedAgent profiles c actor { operation := .disable, target, makeDefault }
      candidate node = none := by
  simp [materializedAgent, enabledAfter, AgentRequest.writesEnabled, AgentOperation.normalize,
    disableFields]

end SelfConfig
