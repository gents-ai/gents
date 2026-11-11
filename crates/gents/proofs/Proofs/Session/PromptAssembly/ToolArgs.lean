import Proofs.Basic

namespace PromptAssembly

inductive ToolArgs (P : Type) where
  | object (payload : P)
  | str (parsed : Option P)
  | array
  | scalar
  | null

def ToolArgs.IsObject {P : Type} : ToolArgs P → Prop
  | .object _ => True
  | _ => False

@[simp] theorem ToolArgs.isObject_object {P : Type} (p : P) :
    (ToolArgs.object p).IsObject := trivial

def normalizeArgs {P : Type} (empty : P) : ToolArgs P → ToolArgs P
  | .object p => .object p
  | .str (some p) => .object p
  | .str none => .object empty
  | .array => .object empty
  | .scalar => .object empty
  | .null => .object empty

theorem normalize_isObject {P : Type} (empty : P) (v : ToolArgs P) :
    (normalizeArgs empty v).IsObject := by
  cases v with
  | object p => trivial
  | str parsed => cases parsed <;> trivial
  | array => trivial
  | scalar => trivial
  | null => trivial

theorem normalize_eq_object {P : Type} (empty : P) (v : ToolArgs P) :
    ∃ p, normalizeArgs empty v = .object p := by
  cases v with
  | object p => exact ⟨p, rfl⟩
  | str parsed => cases parsed with
    | none => exact ⟨empty, rfl⟩
    | some p => exact ⟨p, rfl⟩
  | array => exact ⟨empty, rfl⟩
  | scalar => exact ⟨empty, rfl⟩
  | null => exact ⟨empty, rfl⟩

theorem normalize_fixpoint_of_isObject {P : Type} (empty : P)
    {v : ToolArgs P} (h : v.IsObject) : normalizeArgs empty v = v := by
  cases v with
  | object p => rfl
  | str parsed => exact absurd h not_false
  | array => exact absurd h not_false
  | scalar => exact absurd h not_false
  | null => exact absurd h not_false

theorem normalize_idempotent {P : Type} (empty : P) (v : ToolArgs P) :
    normalizeArgs empty (normalizeArgs empty v) = normalizeArgs empty v :=
  normalize_fixpoint_of_isObject empty (normalize_isObject empty v)

theorem normalize_salvages_str {P : Type} (empty : P) (p : P) :
    normalizeArgs empty (.str (some p)) = .object p := rfl

theorem normalize_nonobject_to_empty {P : Type} (empty : P)
    (v : ToolArgs P) (hobj : ¬ v.IsObject)
    (hsalvage : ∀ p, v ≠ .str (some p)) :
    normalizeArgs empty v = .object empty := by
  cases v with
  | object p => exact absurd trivial hobj
  | str parsed => cases parsed with
    | none => rfl
    | some p => exact absurd rfl (hsalvage p)
  | array => rfl
  | scalar => rfl
  | null => rfl

class LeafSanitizer (P : Type) where
  sanitize : P → P
  idempotent : ∀ p, sanitize (sanitize p) = sanitize p

def repairArgs {P : Type} [LeafSanitizer P] (empty : P) (v : ToolArgs P) : ToolArgs P :=
  match normalizeArgs empty v with
  | .object p => .object (LeafSanitizer.sanitize p)
  | other => other

theorem repair_isObject {P : Type} [LeafSanitizer P] (empty : P) (v : ToolArgs P) :
    (repairArgs empty v).IsObject := by
  have hn := normalize_isObject empty v
  unfold repairArgs
  cases h : normalizeArgs empty v with
  | object p => trivial
  | str parsed => rw [h] at hn; exact absurd hn not_false
  | array => rw [h] at hn; exact absurd hn not_false
  | scalar => rw [h] at hn; exact absurd hn not_false
  | null => rw [h] at hn; exact absurd hn not_false

theorem repair_on_object {P : Type} [LeafSanitizer P] (empty : P) (p : P) :
    repairArgs empty (.object p) = .object (LeafSanitizer.sanitize p) := rfl

theorem repair_idempotent {P : Type} [LeafSanitizer P] (empty : P) (v : ToolArgs P) :
    repairArgs empty (repairArgs empty v) = repairArgs empty v := by
  have hn := normalize_isObject empty v
  cases h : normalizeArgs empty v with
  | object p =>
      have hr : repairArgs empty v = .object (LeafSanitizer.sanitize p) := by
        unfold repairArgs; rw [h]
      rw [hr, repair_on_object, LeafSanitizer.idempotent]
  | str parsed => rw [h] at hn; exact absurd hn not_false
  | array => rw [h] at hn; exact absurd hn not_false
  | scalar => rw [h] at hn; exact absurd hn not_false
  | null => rw [h] at hn; exact absurd hn not_false

theorem repair_normalize_fixpoint {P : Type} [LeafSanitizer P] (empty : P) (v : ToolArgs P) :
    normalizeArgs empty (repairArgs empty v) = repairArgs empty v :=
  normalize_fixpoint_of_isObject empty (repair_isObject empty v)

theorem repair_is_payload_only {P : Type} [LeafSanitizer P] (empty : P) (p : P) :
    repairArgs empty (.object p) = .object (LeafSanitizer.sanitize p) :=
  repair_on_object empty p

end PromptAssembly
