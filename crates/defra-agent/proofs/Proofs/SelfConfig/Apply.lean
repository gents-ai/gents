import Proofs.SelfConfig.Types

/-!
# Self-Configuration Patch Application

The write operation of a self-config tool call: an in-memory partial merge of
a typed patch over the target's writable field set, gated by admissibility,
validation, and the opt-in no-lockout guard, then committed wholesale or
rejected with no mutation.

Two containment layers mirror the production shape:
- `admissible` rejects any patch naming a field outside the writable set (the
  typed patch structs deserialize with unknown fields denied);
- `applyEntry` ignores non-writable keys regardless (defense in depth inside
  the merge itself).
-/

namespace SelfConfig

abbrev FieldValue := String

/-- A document is a partial map from field key to opaque value. -/
def Doc := FieldKey → Option FieldValue

def Doc.ofList (entries : List (FieldKey × FieldValue)) : Doc :=
  fun k => (entries.find? (fun e => e.1 == k)).map (·.2)

inductive PatchOp where
  | set (value : FieldValue)
  | clear
  deriving DecidableEq, Repr

def PatchOp.value : PatchOp → Option FieldValue
  | .set v => some v
  | .clear => none

structure PatchEntry where
  key : FieldKey
  op : PatchOp
  deriving DecidableEq, Repr

abbrev Patch := List PatchEntry

/-- Merge one patch entry. Entries outside the target's writable set are
    no-ops; a `set` overwrites, a `clear` removes. -/
def applyEntry (t : Target) (doc : Doc) (e : PatchEntry) : Doc :=
  if e.key ∈ writableFields t then
    fun k => if k = e.key then e.op.value else doc k
  else
    doc

def applyPatch (t : Target) (doc : Doc) (p : Patch) : Doc :=
  p.foldl (applyEntry t) doc

/-- The typed surface rejects (rather than silently drops) any patch naming a
    field outside the writable set. -/
def admissible (t : Target) (p : Patch) : Bool :=
  p.all (fun e => decide (e.key ∈ writableFields t))

/-- One self-config write: admissibility → in-memory merge → validation and
    guard oracles → accept wholesale or reject with no mutation.

    `validate` abstracts the production validators (structural document
    validation, ref existence, cadence checks); `guard` abstracts the opt-in
    no-lockout check. Both observe the *merged* document. -/
def step (validate guard : Doc → Bool) (t : Target) (stored : Doc)
    (p : Patch) : Option Doc :=
  if admissible t p = true then
    if (validate (applyPatch t stored p) && guard (applyPatch t stored p))
        = true then
      some (applyPatch t stored p)
    else
      none
  else
    none

/-- The store holds one owned document per target. -/
def Store := Target → Doc

/-- Run one write against the store: on acceptance exactly the target document
    becomes the merged result; on rejection the store is returned untouched. -/
def runStep (validate guard : Doc → Bool) (t : Target) (s : Store)
    (p : Patch) : Store × Bool :=
  match step validate guard t (s t) p with
  | some merged => (fun t' => if t' = t then merged else s t', true)
  | none => (s, false)

/-- The no-lockout gate observation: the merged ToolSelection keeps
    `enable_self_config` explicitly on. The gate is opt-in (unset = off), so
    recoverability requires the literal `"true"`. -/
def gateField : FieldKey := "enable_self_config"

def gateOn (doc : Doc) : Bool := doc gateField == some "true"

end SelfConfig
