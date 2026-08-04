import Proofs.Basic
import Mathlib.Data.Finset.Basic
import Mathlib.Data.Finset.Prod

/-!
# PersonaRequest lifecycle model

The formal fence for the shared persona-request materializer
(`crate::agent::persona_ops`). A `PersonaConfigRequest` row is a
phone-authored ask to create, clone, edit, or disable a persona's backing
`AgentBehavior`. Three write channels (server reconciler, self-config tool,
CLI) mint the SAME outcome from one row, so admission and materialization
must never drift between them. This model mirrors the two pure cores that
guarantee that:

* `decide_persona_request` — the admission gate. Modelled here as `admits`,
  a Prop with a `Decidable` instance (the `BearerClaim.lean` precedent),
  whose conjuncts mirror the Rust admission verbatim (folded: unknown preset
  names are rejected on create, and `clone_from` must be an existing ENABLED
  behavior).
* `apply_persona_request` — the materializer. Modelled here as `applyStep`,
  which only runs its effect on an admitted request and is idempotent.

The `State` is payload-abstract (the `DirectoryProjection.lean` convention):
behaviors are `(id, enabled)` pairs, selections are an id set, and
`operatorSelections` is kept separate so ownership safety can be stated.
Field contents (prompt, model, tool policy) ride below this abstraction.
-/

namespace PeerRegistryDiscovery
namespace PersonaRequest

/-- The requested operation. `clone_from` is folded into `create` in the
Rust code; here the request carries `cloneFrom` as a field and `create`
branches on whether it is empty (mirroring `PersonaOp::Create { clone_from }`). -/
inductive Op
  | create
  | edit
  | disable
  deriving DecidableEq, Repr

/-- The published options a request is validated against: the model, root,
and inference-profile catalogs for this deployment. -/
structure Catalog where
  models : Finset String
  roots : Finset String
  profiles : Finset String
  deriving DecidableEq, Repr

/-- A typed persona request. String payload fields carry the requested
values; `key` is the request key that derives the minted selection id. -/
structure Request where
  key : String
  op : Op
  name : String
  model : String
  root : String
  preset : String
  profile : String
  cloneFrom : String
  target : String
  deriving DecidableEq, Repr

/-- The agent's config state, payload-abstract. `behaviors` are
`(behavior_id, enabled)` pairs; `selections` are `tool_selection_id`s;
`operatorSelections` are operator-authored selection ids kept separate so
request processing can be proven never to touch them. -/
structure State where
  behaviors : Finset (String × Bool)
  selections : Finset String
  operatorSelections : Finset String
  deriving DecidableEq

-- These admission conjuncts are `abbrev` (reducible) so the `Decidable`
-- instance for `admits` resolves through them via `infer_instance`.

/-- The two built-in preset names (`persona_presets::builtin_preset_names`). -/
abbrev presetKnown (r : Request) : Prop :=
  r.preset = "readonly" ∨ r.preset = "write"

abbrev nameOk (r : Request) : Prop := r.name ≠ ""

abbrev modelOk (cat : Catalog) (r : Request) : Prop := r.model ∈ cat.models

/-- An empty root is always fine ("no root restriction"); a non-empty root
must be published (`validate_root`). -/
abbrev rootOk (cat : Catalog) (r : Request) : Prop :=
  r.root = "" ∨ r.root ∈ cat.roots

abbrev profileOk (cat : Catalog) (r : Request) : Prop := r.profile ∈ cat.profiles

/-- A plain create names a known preset (`validate_preset_name`, folded). -/
abbrev presetCreateOk (r : Request) : Prop := r.preset ≠ "" ∧ presetKnown r

/-- A cloning create must omit `preset` and name an existing ENABLED
behavior (`clone_from` admission). -/
abbrev cloneOk (st : State) (r : Request) : Prop :=
  r.preset = "" ∧ (r.cloneFrom, true) ∈ st.behaviors

/-- `create` splits on whether `cloneFrom` is empty. -/
abbrev createModeOk (st : State) (r : Request) : Prop :=
  (r.cloneFrom = "" ∧ presetCreateOk r) ∨ (r.cloneFrom ≠ "" ∧ cloneOk st r)

/-- The target behavior exists (with either enabled flag) — the `edit` /
`disable` `contains_key` check. -/
abbrev behaviorPresent (st : State) (id : String) : Prop :=
  (id, true) ∈ st.behaviors ∨ (id, false) ∈ st.behaviors

/-- Edit may keep the current selection (empty preset) or name a known one. -/
abbrev editPresetOk (r : Request) : Prop := r.preset = "" ∨ presetKnown r

/-- Admission gate, mirroring `decide_persona_request` conjunct-for-conjunct. -/
def admits (cat : Catalog) (st : State) (r : Request) : Prop :=
  match r.op with
  | Op.create =>
      nameOk r ∧ modelOk cat r ∧ rootOk cat r ∧ profileOk cat r ∧ createModeOk st r
  | Op.edit =>
      behaviorPresent st r.target ∧ nameOk r ∧ modelOk cat r ∧ rootOk cat r ∧
        profileOk cat r ∧ editPresetOk r
  | Op.disable =>
      behaviorPresent st r.target

instance (cat : Catalog) (st : State) (r : Request) : Decidable (admits cat st r) := by
  unfold admits
  cases r.op <;> infer_instance

/-- The minted behavior id (abstractly the request key: `derive_behavior_id`
derives a fresh, collision-free id per request). -/
def mintedBehaviorId (r : Request) : String := r.key

/-- The minted selection id: `sel-{request_key}` (`apply_create`). -/
def selId (r : Request) : String := "sel-" ++ r.key

/-- Flip a behavior's enabled flag to `false`: remove `(id, true)` and add
`(id, false)`. Idempotent (`flipDisabled_idem`). -/
def flipDisabled (b : Finset (String × Bool)) (id : String) : Finset (String × Bool) :=
  insert (id, false) (b.erase (id, true))

/-- The effect of an ADMITTED request (`apply_persona_request` assuming
admission ran). Payload writes ride below the abstraction, so:
* create (preset or clone) mints `(mintedBehaviorId, true)` and `selId`;
* edit rewrites payload only — the behavior/selection id set is preserved,
  so abstractly it is the identity on `State`;
* disable flips the target's enabled flag. -/
def applyAdmitted (st : State) (r : Request) : State :=
  match r.op with
  | Op.create =>
      { st with
          behaviors := insert (mintedBehaviorId r, true) st.behaviors,
          selections := insert (selId r) st.selections }
  | Op.edit => st
  | Op.disable =>
      { st with behaviors := flipDisabled st.behaviors r.target }

/-- One lifecycle step: an inadmissible request is a no-op; an admitted one
runs its effect. -/
def applyStep (cat : Catalog) (st : State) (r : Request) : State :=
  if admits cat st r then applyAdmitted st r else st

/-! ## Equation lemmas for `applyStep` / `applyAdmitted`. -/

theorem applyStep_admitted (cat : Catalog) (st : State) (r : Request)
    (h : admits cat st r) : applyStep cat st r = applyAdmitted st r := by
  unfold applyStep
  rw [if_pos h]

theorem applyAdmitted_create (st : State) (r : Request) (hop : r.op = Op.create) :
    applyAdmitted st r =
      { st with
          behaviors := insert (mintedBehaviorId r, true) st.behaviors,
          selections := insert (selId r) st.selections } := by
  simp only [applyAdmitted, hop]

theorem applyAdmitted_edit (st : State) (r : Request) (hop : r.op = Op.edit) :
    applyAdmitted st r = st := by
  simp only [applyAdmitted, hop]

theorem applyAdmitted_disable (st : State) (r : Request) (hop : r.op = Op.disable) :
    applyAdmitted st r = { st with behaviors := flipDisabled st.behaviors r.target } := by
  simp only [applyAdmitted, hop]

/-! ## Lifecycle theorems. -/

/-- An inadmissible (pending / rejected) request grants nothing: the state is
unchanged. -/
theorem pending_request_grants_nothing (cat : Catalog) (st : State) (r : Request)
    (h : ¬ admits cat st r) : applyStep cat st r = st := by
  unfold applyStep
  rw [if_neg h]

/-- A concrete rejection (create against an unpublished model) changes
nothing — the same no-op guarantee as `pending_request_grants_nothing`, keyed
off a single failing admission conjunct. -/
theorem rejected_changes_nothing (cat : Catalog) (st : State) (r : Request)
    (hop : r.op = Op.create) (hmodel : r.model ∉ cat.models) :
    applyStep cat st r = st := by
  apply pending_request_grants_nothing
  intro hadm
  simp only [admits, hop, modelOk] at hadm
  exact hmodel hadm.2.1

/-- An admitted create mints a well-formed behavior: the minted behavior is
enabled, its fresh selection is in `selections`, and the profile was
validated by admission. -/
theorem admitted_create_mints_wellformed (cat : Catalog) (st : State) (r : Request)
    (hadm : admits cat st r) (hop : r.op = Op.create) :
    (mintedBehaviorId r, true) ∈ (applyStep cat st r).behaviors ∧
      selId r ∈ (applyStep cat st r).selections ∧
      r.profile ∈ cat.profiles := by
  rw [applyStep_admitted cat st r hadm, applyAdmitted_create st r hop]
  refine ⟨Finset.mem_insert_self _ _, Finset.mem_insert_self _ _, ?_⟩
  have ha := hadm
  simp only [admits, hop, profileOk] at ha
  exact ha.2.2.2.1

/-- An admitted clone mints a fresh selection distinct from the source's,
mints the new behavior, and leaves the source behavior present. The
distinctness hypothesis reflects that `selId`/`mintedBehaviorId` are derived
from the unique request key. -/
theorem admitted_clone_copies_selection (cat : Catalog) (st : State) (r : Request)
    (hadm : admits cat st r) (hop : r.op = Op.create) (hclone : r.cloneFrom ≠ "")
    (hdistinct : mintedBehaviorId r ≠ r.cloneFrom) :
    (mintedBehaviorId r, true) ∈ (applyStep cat st r).behaviors ∧
      (r.cloneFrom, true) ∈ (applyStep cat st r).behaviors ∧
      selId r ∈ (applyStep cat st r).selections ∧
      mintedBehaviorId r ≠ r.cloneFrom := by
  have hsrc : (r.cloneFrom, true) ∈ st.behaviors := by
    have ha := hadm
    simp only [admits, hop, createModeOk, cloneOk] at ha
    rcases ha.2.2.2.2 with ⟨he, _⟩ | ⟨_, _, hmem⟩
    · exact absurd he hclone
    · exact hmem
  rw [applyStep_admitted cat st r hadm, applyAdmitted_create st r hop]
  exact ⟨Finset.mem_insert_self _ _, Finset.mem_insert_of_mem hsrc,
    Finset.mem_insert_self _ _, hdistinct⟩

/-- An edit never adds or removes a behavior id: the behavior set is
preserved (it rewrites payload only). -/
theorem admitted_edit_preserves_behavior_set (cat : Catalog) (st : State) (r : Request)
    (hop : r.op = Op.edit) : (applyStep cat st r).behaviors = st.behaviors := by
  by_cases h : admits cat st r
  · rw [applyStep_admitted cat st r h, applyAdmitted_edit st r hop]
  · rw [pending_request_grants_nothing cat st r h]

/-- Disable flips only the enabled flag: the target becomes `(target, false)`
and is no longer `(target, true)`, while every unrelated behavior is
preserved unchanged. -/
theorem disable_only_flips_enabled (cat : Catalog) (st : State) (r : Request)
    (hadm : admits cat st r) (hop : r.op = Op.disable) :
    (r.target, false) ∈ (applyStep cat st r).behaviors ∧
      (r.target, true) ∉ (applyStep cat st r).behaviors ∧
      (∀ b, b.1 ≠ r.target →
        (b ∈ (applyStep cat st r).behaviors ↔ b ∈ st.behaviors)) := by
  have hb : (applyStep cat st r).behaviors
      = insert (r.target, false) (st.behaviors.erase (r.target, true)) := by
    rw [applyStep_admitted cat st r hadm, applyAdmitted_disable st r hop]
  rw [hb]
  refine ⟨Finset.mem_insert_self _ _, ?_, ?_⟩
  · intro hmem
    rw [Finset.mem_insert] at hmem
    rcases hmem with heq | herase
    · simp at heq
    · exact (Finset.mem_erase.mp herase).1 rfl
  · intro b hbne
    rw [Finset.mem_insert, Finset.mem_erase]
    constructor
    · rintro (heq | ⟨_, hmem⟩)
      · exact absurd (congrArg Prod.fst heq) hbne
      · exact hmem
    · intro hmem
      exact Or.inr ⟨fun heq => hbne (congrArg Prod.fst heq), hmem⟩

/-- `flipDisabled` is idempotent. -/
theorem flipDisabled_idem (b : Finset (String × Bool)) (id : String) :
    flipDisabled (flipDisabled b id) id = flipDisabled b id := by
  unfold flipDisabled
  ext x
  simp only [Finset.mem_insert, Finset.mem_erase]
  tauto

/-- The admitted effect is idempotent, per op. -/
theorem applyAdmitted_idem (st : State) (r : Request) :
    applyAdmitted (applyAdmitted st r) r = applyAdmitted st r := by
  cases hop : r.op with
  | create =>
      rw [applyAdmitted_create st r hop, applyAdmitted_create _ r hop]
      simp [Finset.insert_idem]
  | edit =>
      rw [applyAdmitted_edit st r hop, applyAdmitted_edit st r hop]
  | disable =>
      rw [applyAdmitted_disable st r hop, applyAdmitted_disable _ r hop]
      simp [flipDisabled_idem]

/-- Reprocessing the same request is a no-op after the first application
(`apply_persona_request`'s repair short-circuit). -/
theorem applyStep_idempotent (cat : Catalog) (st : State) (r : Request) :
    applyStep cat (applyStep cat st r) r = applyStep cat st r := by
  by_cases h : admits cat st r
  · rw [applyStep_admitted cat st r h]
    by_cases h' : admits cat (applyAdmitted st r) r
    · rw [applyStep_admitted cat (applyAdmitted st r) r h', applyAdmitted_idem]
    · rw [pending_request_grants_nothing cat (applyAdmitted st r) r h']
  · rw [pending_request_grants_nothing cat st r h]
    exact pending_request_grants_nothing cat st r h

/-- Request processing never touches operator-authored selections. -/
theorem applyStep_ownership_safe (cat : Catalog) (st : State) (r : Request) :
    (applyStep cat st r).operatorSelections = st.operatorSelections := by
  by_cases h : admits cat st r
  · rw [applyStep_admitted cat st r h]
    cases hop : r.op with
    | create => rw [applyAdmitted_create st r hop]
    | edit => rw [applyAdmitted_edit st r hop]
    | disable => rw [applyAdmitted_disable st r hop]
  · rw [pending_request_grants_nothing cat st r h]

end PersonaRequest
end PeerRegistryDiscovery
