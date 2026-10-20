import Proofs.SelfConfig.Types
import Proofs.PluginJob.Types

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
  can restore every Tools group and the categories. -/
  toolsAuthority : Bool
  deriving DecidableEq, Repr

def retained (old new : Bool) : Bool := !old || new

/-- No lockout (#1796) is the only lockout protection; operator grants are
bounded separately by `keepsGrants`. The Engineer is refused only a candidate
that turns off its self-config tool, or drops the agents group, the no-lockout
guard, or the `tools` authority it needs to restore any of them; dropping the
guard or that authority first would make the lockout a two-step edit. -/
def keepsControl (decode : Doc → Option Control) (stored candidate : Doc) : Bool :=
  match decode stored, decode candidate with
  | some old, some new =>
      new.selfConfig && retained old.agents new.agents
        && retained old.noLockout new.noLockout
        && retained old.toolsAuthority new.toolsAuthority
  | _, _ => false

/-- A plugin's job block: its typed ceiling and its operator-owned policy. -/
structure JobGrant where
  ceiling : PluginJob.Ceiling
  policy : PluginJob.Policy
  deriving DecidableEq

instance : Repr JobGrant := ⟨fun _ _ => Std.Format.text "JobGrant"⟩

/-- Operator-managed grants projected from a Tools document by the canonical
typed decoder; absent flags are false. -/
structure Grants where
  /-- `self_config.enable_pack_install`. -/
  packInstall : Bool
  /-- `integrations.plugins[].job` by plugin coordinate (`PluginToolRef.plugin`).
  A plugin with no entry has the empty ceiling (`PluginJob.Ceiling.empty`). -/
  job : List (String × JobGrant) := []
  deriving DecidableEq, Repr

def Grants.bot : Grants := { packInstall := false }

def Grants.le (a b : Grants) : Bool := (!a.packInstall || b.packInstall)

/-- Whether each grant the candidate `c` carries stays within its own bound
against the stored Tools `s` and the invoker's `held` grants; each grant is
judged on its own, never as a whole vector. -/
def Grants.boundedBy (c s held : Grants) : Bool :=
  (!c.packInstall || s.packInstall || held.packInstall)

/-- Adding a block widens: an absent block is the empty ceiling and carries no
policy to keep. -/
def JobGrant.narrows : Option JobGrant → Option JobGrant → Bool
  | none, _ => true
  | some _, none => false
  | some c, some s => decide (c.ceiling ≤ s.ceiling) && decide (c.policy = s.policy)

/-- The job grant is stored-bounded, never held-bounded: every job block the
candidate `c` carries, every entry of a repeated plugin coordinate included,
narrows the block the stored Tools `s` carry for the same plugin and keeps its
policy. It is a conjunct of its own beside
`Grants.boundedBy`, whose held bound (and `Grants.le`) covers pack installation
only; inside it, holding a job block would let a holder grant one to a document
that has none. -/
def keepsJobGrants (c s : Grants) : Bool :=
  c.job.all fun e => JobGrant.narrows (some e.2) (s.job.lookup e.1)

/-- A self-config write is accepted when every operator-managed grant of the
candidate stays within its own bound against the Tools document it replaces;
pack installation is held-bounded, so a holder may grant it to a sibling and
nobody else can. Unlike `keepsControl` this guard always runs: the native owner
calls it from the shared validate slot, not the opt-in no-lockout slot.
Job grants are bounded by the stored Tools alone (`keepsJobGrants`): a model write
may narrow a plugin's job ceiling but cannot widen it, add a block or edit its
policy, whatever the invoker holds.
Operator writes (the desktop, `config apply`) do not pass through this guard. -/
def keepsGrants (decode : Doc → Option Grants) (held : Grants) (stored candidate : Doc) :
    Bool :=
  match decode stored, decode candidate with
  | some s, some c => c.boundedBy s held && keepsJobGrants c s
  | _, _ => false

/-- The grant bound across a reselection: bounded like `keepsGrants` from the
previously selected Tools; with no previous selection, bounded against
`Grants.bot`, so pack installation needs the invoker to hold it, and a job block
is refused (`Grants.bot` carries none). A direct clone is checked against the
invoking agent's held grants in the same transaction that reads the source and
publishes the candidate. -/
def reselectionKeepsGrants (decode : Doc → Option Grants) (held : Grants)
    (before after : Option Doc) : Bool :=
  match before, after with
  | _, none => true
  | some storedTools, some candidateTools =>
      keepsGrants decode held storedTools candidateTools
  | none, some candidateTools => (decode candidateTools).any
      (fun c => c.boundedBy Grants.bot held && keepsJobGrants c Grants.bot)

/-- `reselectionKeepsGrants` on the Tools documents a Context or Agent
selects; `none` includes a reference to a missing document, which the reference
validator refuses on its own. -/
def chainKeepsGrants (decode : Doc → Option Grants) (held : Grants)
    (resolve : Doc → Option Doc) (stored candidate : Doc) : Bool :=
  reselectionKeepsGrants decode held (resolve stored) (resolve candidate)

/-- The invoker's reachability, projected from its own agent document:
`enabled` (absent is true) and whether its tags carry the Engineer tag. -/
structure Reach where
  enabled : Bool
  engineerTag : Bool
  deriving DecidableEq, Repr

/-- The agent half of no lockout: the invoker stays enabled and keeps the
Engineer tag it had; dropping the tag first would make self-disable a
two-step edit. Other tags and fields stay editable. -/
def keepsReach (decode : Doc → Option Reach) (stored candidate : Doc) : Bool :=
  match decode stored, decode candidate with
  | some old, some new => new.enabled && retained old.engineerTag new.engineerTag
  | _, _ => false

end SelfConfig
