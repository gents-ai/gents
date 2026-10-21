import Proofs.Workspace.ChangeReview

namespace Workspace.ChangeReview.Editor.Checkpoint

/-- One workspace-private journal, selected by the editor host rather than a
webview or recorded source path. Decoding `present` requires a bounded, exact
refs-only envelope; malformed, truncated or unreadable data is never absence. -/
inductive Stored where
  | absent
  | present (version : Nat) (ledger : Ledger)
  | corrupt
  | unavailable
  deriving DecidableEq, Repr

inductive GuardObservation where
  | acquired | busy | unavailable
  deriving DecidableEq, Repr

/-- `installed` describes the physical candidate after a write attempt. An effect
requires both a durable receipt and its acknowledgement. The native owner must
flush the file and its replacement directory before acknowledging; an unsupported
or uncertain platform flush is not success. Neither a lost reply nor uncertain
I/O is retried by this session. -/
structure Receipt where
  installed : Bool
  durable : Bool
  acknowledged : Bool
  deriving DecidableEq, Repr

structure Proposal where
  ledger : Ledger
  effect : Effect
  operation : Option String
  deriving DecidableEq, Repr

/-- `caller` names an existing owned helper connection, not an actor identity,
lease or source grant. Its OS advisory guard is held through reads, checkpoints
and the actual editor/save promise. Webview cancellation cannot retire that guard
or settle the promise. Only the real host continuation may report settlement. -/
structure Holder where
  caller : Nat
  loaded : Bool := false
  refused : Bool := false
  cancelled : Bool := false
  ledger : Ledger := []
  operation : Option String := none
  pending : Option Proposal := none
  grant : Effect := .none
  running : Effect := .none
  deriving DecidableEq, Repr

structure World where
  stored : Stored := .absent
  holder : Option Holder := none
  deriving DecidableEq, Repr

inductive Output where
  | acquired | busy | loaded | checkpoint | acknowledged | refused
  | effect (value : Effect)
  | settled | cancelled | held | released
  deriving DecidableEq, Repr

inductive Input where
  | acquire (caller : Nat) (observation : GuardObservation)
  | load (caller : Nat)
  | admit (caller : Nat) (id : String) (targets : List Target)
      (documents : List Document) (observations : Observations)
  | advance (caller : Nat) (action : Action)
  | finishWrite (caller : Nat) (receipt : Receipt)
  | launch (caller : Nat)
  | settle (caller : Nat)
  | cancel (caller : Nat)
  | release (caller : Nat)
  deriving DecidableEq, Repr

private def owned (world : World) (caller : Nat)
    (transition : Holder → World × Output) : World × Output :=
  match world.holder with
  | some holder => if holder.caller == caller then transition holder else (world, .refused)
  | none => (world, .refused)

private def keep (world : World) (holder : Holder) (output : Output) : World × Output :=
  ({world with holder := some holder}, output)

private def usable (holder : Holder) : Bool :=
  holder.loaded && !holder.refused && holder.pending.isNone && holder.running == .none

private def refuse (world : World) (holder : Holder) : World × Output :=
  keep world {holder with refused := true, grant := .none} .refused

private def read (world : World) (holder : Holder) : World × Output :=
  if holder.loaded || holder.refused || holder.pending.isSome then (world, .refused)
  else match world.stored with
  | .absent => keep world {holder with loaded := true, ledger := []} .loaded
  | .present version ledger =>
    if version == 1 && ledgerValid ledger then
      keep world {holder with loaded := true, ledger} .loaded
    else refuse world holder
  | .corrupt | .unavailable => refuse world holder

private def admit (world : World) (holder : Holder) (id : String)
    (targets : List Target) (documents : List Document) (observations : Observations) : World × Output :=
  if !usable holder || holder.cancelled || holder.operation.isSome then (world, .refused)
  else match reserve holder.ledger id targets documents observations true with
  | none => refuse world holder
  | some ledger => keep world {holder with pending := some ⟨ledger, .none, some id⟩} .checkpoint

private def advance (world : World) (holder : Holder) (action : Action) : World × Output :=
  if !usable holder || holder.grant != .none then (world, .refused)
  else match holder.operation with
  | none => (world, .refused)
  | some id => match holder.ledger.find? (fun operation => operation.id == id) with
    | none => refuse world holder
    | some operation =>
      let next := Editor.step operation.sequence action
      if holder.cancelled && next.2 != .none then (world, .refused)
      else
        let ledger := holder.ledger.map fun current =>
          if current.id == id then {current with sequence := next.1} else current
        keep world {holder with pending := some ⟨ledger, next.2, some id⟩} .checkpoint

private def finishWrite (world : World) (holder : Holder) (receipt : Receipt) : World × Output :=
  match holder.pending with
  | none => (world, .refused)
  | some proposal =>
    let stored := if receipt.installed then Stored.present 1 proposal.ledger else world.stored
    let next := {world with stored}
    if receipt.installed && receipt.durable && receipt.acknowledged then
      let grant := if holder.cancelled then Effect.none else proposal.effect
      keep next {holder with ledger := proposal.ledger, operation := proposal.operation, pending := none, grant} .acknowledged
    else
      keep next {holder with loaded := false, refused := true, pending := none, grant := .none} .refused

private def launch (world : World) (holder : Holder) : World × Output :=
  if !usable holder || holder.cancelled || holder.grant == .none then (world, .refused)
  else keep world {holder with grant := .none, running := holder.grant} (.effect holder.grant)

/-- This gate composes the existing Undo reservation/sequence owners. The native
checkpoint primitive grants no editor or source action; only an acknowledged
checkpoint can release the effect already selected by `Editor.step`. -/
def step (world : World) (input : Input) : World × Output :=
  match input with
  | .acquire caller observation =>
    if world.holder.isSome then (world, .busy)
    else match observation with
    | .acquired => keep world {caller} .acquired
    | .busy => (world, .busy)
    | .unavailable => (world, .refused)
  | .load caller => owned world caller (read world)
  | .admit caller id targets documents observations =>
    owned world caller (fun holder => admit world holder id targets documents observations)
  | .advance caller action => owned world caller (fun holder => advance world holder action)
  | .finishWrite caller receipt => owned world caller (fun holder => finishWrite world holder receipt)
  | .launch caller => owned world caller (launch world)
  | .settle caller => owned world caller fun holder =>
    if holder.running == .none then (world, .refused)
    else keep world {holder with running := .none} .settled
  | .cancel caller => owned world caller fun holder =>
    keep world {holder with cancelled := true, grant := .none} .cancelled
  | .release caller => owned world caller fun holder =>
    if holder.pending.isSome || holder.running != .none then (world, .held)
    else ({world with holder := none}, .released)

/-- Every other output is an observation or checkpoint request, never an effect. -/
def Output.editorEffect : Output → Effect
  | .effect value => value
  | _ => .none

theorem empty_guard_cannot_launch (stored : Stored) (caller : Nat) :
    (step ⟨stored, none⟩ (.launch caller)).2 = .refused := by
  simp [step, owned]

theorem held_guard_cannot_be_replaced (stored : Stored) (holder : Holder)
    (caller : Nat) (observation : GuardObservation) :
    step ⟨stored, some holder⟩ (.acquire caller observation) = (⟨stored, some holder⟩, .busy) := by
  simp [step]

theorem cancellation_preserves_running_promise (stored : Stored) (holder : Holder) :
    (step ⟨stored, some holder⟩ (.cancel holder.caller)).1.holder =
      some {holder with cancelled := true, grant := .none} := by
  simp [step, owned, keep]

theorem running_promise_cannot_release (stored : Stored) (holder : Holder)
    (running : (holder.running != .none) = true) :
    step ⟨stored, some holder⟩ (.release holder.caller) = (⟨stored, some holder⟩, .held) := by
  simp [step, owned, running]

theorem pending_checkpoint_cannot_release (stored : Stored) (holder : Holder)
    (pending : holder.pending.isSome = true) :
    step ⟨stored, some holder⟩ (.release holder.caller) = (⟨stored, some holder⟩, .held) := by
  simp [step, owned, pending]

end Workspace.ChangeReview.Editor.Checkpoint
