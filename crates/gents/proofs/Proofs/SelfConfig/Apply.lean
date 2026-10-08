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

def runStep (validate guard : Doc → Bool) (t : Target) (s : Store)
    (p : Patch) : Store × Bool :=
  match step validate guard t (s t) p with
  | some merged => (fun t' => if t' = t then merged else s t', true)
  | none => (s, false)

/-- The invoker's control over itself, projected from its Tools by the
canonical typed decoder (absent flags are false; absent categories select the
default set, which includes `tools`). Decode errors must fail the shared
validator; this model does not parse or duplicate the nested schema. -/
structure Control where
  /-- `self_config.enable_self_config`: the `config` tool exists. -/
  selfConfig : Bool
  /-- `agents`: the agents tool group on Tools. -/
  agents : Bool
  /-- `self_config.self_config_no_lockout`: this guard applies to later writes. -/
  noLockout : Bool
  /-- The effective self-config categories grant `tools`, the one category that
  can restore every Tools group, categories and grants included. -/
  toolsAuthority : Bool
  deriving DecidableEq, Repr

/-- A capability the invoker had must remain. -/
def retained (old new : Bool) : Bool := !old || new

/-- No lockout (#1796) is the only self-protection. The Engineer is a full
self-writing agent: it may edit its own Tools and target itself with
automation, and every such write is checked the normal way (preview, ACP, typed
validation). It is refused only a candidate that turns off its self-config tool,
or drops the agents group, the no-lockout guard, or the `tools` authority it
needs to restore any of them. Dropping the guard or that authority first would
make the lockout a two-step edit, so both are retained like the tools they
protect. Agent and backend enablement are the same invariant on the other
reference-chain documents and remain their existing typed guards. -/
def keepsControl (decode : Doc → Option Control) (stored candidate : Doc) : Bool :=
  match decode stored, decode candidate with
  | some old, some new =>
      new.selfConfig && retained old.agents new.agents
        && retained old.noLockout new.noLockout
        && retained old.toolsAuthority new.toolsAuthority
  | _, _ => false

/-- The invoker's reachability, projected from its own agent document:
`enabled` (absent is true) and whether its tags carry the Engineer tag. -/
structure Reach where
  enabled : Bool
  engineerTag : Bool
  deriving DecidableEq, Repr

/-- The agent half of no lockout: the invoker stays enabled and keeps the
Engineer tag it had. The tag is how the desktop reaches the Engineer and what
`SelfConfig.agentDecision` protects from edit or disable; dropping it first would
make self-disable a two-step edit. Other tags and fields stay editable. -/
def keepsReach (decode : Doc → Option Reach) (stored candidate : Doc) : Bool :=
  match decode stored, decode candidate with
  | some old, some new => new.enabled && retained old.engineerTag new.engineerTag
  | _, _ => false

end SelfConfig
