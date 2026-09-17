import Proofs.SelfConfig.Types

namespace SelfConfig

abbrev FieldValue := String

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

def applyEntry (t : Target) (doc : Doc) (e : PatchEntry) : Doc :=
  if e.key ∈ writableFields t then
    fun k => if k = e.key then e.op.value else doc k
  else
    doc

def applyPatch (t : Target) (doc : Doc) (p : Patch) : Doc :=
  p.foldl (applyEntry t) doc

def admissible (t : Target) (p : Patch) : Bool :=
  p.all (fun e => decide (e.key ∈ writableFields t))

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

def Store := Target → Doc

abbrev StorePatch := Target → Patch

def applyStorePatch (s : Store) (p : StorePatch) : Store :=
  fun t => if t ∈ allTargets then applyPatch t (s t) (p t) else s t

def admissibleStorePatch (p : StorePatch) : Bool :=
  allTargets.all (fun t => admissible t (p t))

/-- Sparse and full self-configuration edits share this owner. The canonical
typed closure validator is supplied by the configuration boundary and runs on
the complete candidate before one commit. -/
def runOwnerStep (validate guard : Store → Bool) (s : Store)
    (p : StorePatch) : Store × Bool :=
  let candidate := applyStorePatch s p
  if admissibleStorePatch p && validate candidate && guard candidate then
    (candidate, true)
  else
    (s, false)

def sparseStorePatch (target : Target) (patch : Patch) : StorePatch :=
  fun t => if t = target then patch else []

def runScopedStep (validate guard : Store → Bool) (target : Target) (s : Store)
    (patch : Patch) : Store × Bool :=
  runOwnerStep validate guard s (sparseStorePatch target patch)

/-- Creation and cloning are owner operations rather than self-config patches:
they may establish protected identity/scope fields, but only by validating and
committing the complete materialized closure at once. -/
def materializeClosure (validate : Store → Bool) (stored candidate : Store) :
    Store × Bool :=
  if validate candidate then (candidate, true) else (stored, false)

def runStep (validate guard : Doc → Bool) (t : Target) (s : Store)
    (p : Patch) : Store × Bool :=
  match step validate guard t (s t) p with
  | some merged => (fun t' => if t' = t then merged else s t', true)
  | none => (s, false)

/-- The canonical Tools decoder projects self_config.enable_self_config.
Missing enablement is false. Decode errors must fail the shared validator;
this model does not parse or duplicate the nested configuration schema. -/
def gateOn (decodeEnabled : Doc → Option Bool) (doc : Doc) : Bool :=
  (decodeEnabled doc).getD false

end SelfConfig
