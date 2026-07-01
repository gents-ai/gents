import Proofs.Basic

/-!
# PromptAssembly ToolArgs (issues #589 / #590)

Value-granular model of the tool-call **argument-shape** invariant at the
provider boundary: every `tool_calls[].function.arguments` a provider
receives must be a JSON **object**. Backends render history through
templates that iterate `arguments.items()`, so a non-object value —
`Value::String` (the #589/#590 production poison), `Array`, scalar, or
`null` — deterministically crashes template rendering before any token is
generated.

This module is BELOW the row granularity of `PromptAssembly.State`: a
`Transcript.MessageRow` carries only the call-id set of an assistant turn,
so argument payloads are invisible to `sanitize` and its theorems (T1–T5).
Argument normalization is applied POINTWISE to each tool call and never
adds, drops, or reorders rows, so the row-granular theorems are untouched
by construction; this module carries the value-granular contract instead.

The abstraction: a payload type `P` stands for the (opaque) content of a
JSON object, and the one JSON-specific fact the model needs — whether a
string re-parses to an object after the tolerant escape-only repair pass
(Rust `llm::tool::repair_tool_arguments`) — is baked into the constructor:
`.str (some p)` is a string that salvages to object `p`; `.str none` is a
string that does not (non-object JSON or unparseable even after repair).

Rust mirror: `llm::tool::normalize_tool_call_arguments`, applied at BOTH
seams of the rig converter — `rig_compat::from_rig_tool_call` (ingest:
nothing non-object is ever accumulated into durable history) and
`rig_compat::to_rig_tool_call` (egress: pre-existing poisoned durable
history self-heals at request build). The theorems:

- **N1 `normalize_isObject`** — normalization always yields an object
  (provider-shape soundness; egress can never emit a non-object).
- **N2 `normalize_fixpoint_of_isObject`** — an object passes through
  UNCHANGED (the healthy tool-call flow has no regression).
- **N3 `normalize_idempotent`** — ingest-then-egress normalization
  collapses; double application is harmless.
- **N4 `normalize_salvages_str`** — a string that (post-repair) parses to
  an object recovers THAT object, not the empty fallback (the #589
  corrupt-payload salvage: the intended call survives).

Rust conformance: `crates/defra-agent/tests/conformance/prompt_assembly.rs`
(the `PromptAssembly` home in the structure fence).
-/

namespace PromptAssembly

/-- A tool-call `arguments` value, abstracted to its provider-relevant
shape. `P` is the opaque payload of a JSON object. `.str (some p)` models a
JSON string that re-parses (after the escape-only repair pass) to the
object `p`; `.str none` models a string that does not. `.array`, `.scalar`
and `.null` cover the remaining non-object JSON shapes. -/
inductive ToolArgs (P : Type) where
  | object (payload : P)
  | str (parsed : Option P)
  | array
  | scalar
  | null

/-- Provider-valid argument shape: a JSON object. -/
def ToolArgs.IsObject {P : Type} : ToolArgs P → Prop
  | .object _ => True
  | _ => False

@[simp] theorem ToolArgs.isObject_object {P : Type} (p : P) :
    (ToolArgs.object p).IsObject := trivial

/-- The shared normalization policy (Rust
`llm::tool::normalize_tool_call_arguments`): an object is unchanged; a
string that salvages to an object becomes that object; every other shape —
unsalvageable string, array, scalar, null — becomes the empty object
`empty`. -/
def normalizeArgs {P : Type} (empty : P) : ToolArgs P → ToolArgs P
  | .object p => .object p
  | .str (some p) => .object p
  | .str none => .object empty
  | .array => .object empty
  | .scalar => .object empty
  | .null => .object empty

/-- **N1 (soundness).** Normalization always yields an object: no egress
path can hand the provider a non-object `arguments` value. -/
theorem normalize_isObject {P : Type} (empty : P) (v : ToolArgs P) :
    (normalizeArgs empty v).IsObject := by
  cases v with
  | object p => trivial
  | str parsed => cases parsed <;> trivial
  | array => trivial
  | scalar => trivial
  | null => trivial

/-- N1 in existential form: the normalized value IS `.object` of some
payload. -/
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

/-- **N2 (object fixpoint).** A well-formed object passes through
unchanged: the healthy tool-call flow is untouched by the boundary. -/
theorem normalize_fixpoint_of_isObject {P : Type} (empty : P)
    {v : ToolArgs P} (h : v.IsObject) : normalizeArgs empty v = v := by
  cases v with
  | object p => rfl
  | str parsed => exact absurd h not_false
  | array => exact absurd h not_false
  | scalar => exact absurd h not_false
  | null => exact absurd h not_false

/-- **N3 (idempotence).** Normalizing twice is normalizing once: the ingest
seam and the egress seam compose without drift, so a value persisted
normalized re-egresses byte-identical. -/
theorem normalize_idempotent {P : Type} (empty : P) (v : ToolArgs P) :
    normalizeArgs empty (normalizeArgs empty v) = normalizeArgs empty v :=
  normalize_fixpoint_of_isObject empty (normalize_isObject empty v)

/-- **N4 (salvage).** A stringified object recovers its own payload — the
intended call survives the boundary rather than collapsing to the empty
fallback. This is the #589 recovery guarantee for the salvageable class. -/
theorem normalize_salvages_str {P : Type} (empty : P) (p : P) :
    normalizeArgs empty (.str (some p)) = .object p := rfl

/-- Non-object shapes collapse to the EMPTY object — never to some other
payload. Together with N4 this pins the entire coercion table. -/
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

end PromptAssembly
