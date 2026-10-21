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

inductive LegacyObservation where
  | empty | records | unavailable
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
lease or source grant. While the helper lives, its OS advisory guard spans reads and checkpoints.
Physical helper death releases the guard before the host necessarily observes
connection loss. An already acknowledged unused grant may still be invoked in
that interval; the durable unresolved sequence blocks every new connection.
Connection-loss observation revokes unused grants without settling real promises. Webview cancellation cannot retire a live guard or
settle the promise. Only the real host continuation may report settlement. -/
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

/-- A host may observe helper death later than the OS releases its lock. Only a
reply for the exact candidate installed before death may still arrive; a dead
helper cannot persist a later proposal. This is transport ordering, not a new
lease or editor effect policy. -/
structure Detached where
  holder : Holder
  candidate : Option Proposal := none
  deriving DecidableEq, Repr

structure World where
  stored : Stored := .absent
  holder : Option Holder := none
  orphans : List (Nat × Effect) := []
  detached : List Detached := []
  deriving DecidableEq, Repr

inductive Output where
  | acquired | busy | loaded | checkpoint | acknowledged | refused
  | effect (value : Effect)
  | settled | cancelled | held | released | withdrawn
  | guardLost | helperLost | orphanSettled
  deriving DecidableEq, Repr

inductive Input where
  | acquire (caller : Nat) (observation : GuardObservation)
  | load (caller : Nat) (legacy : LegacyObservation)
  | admit (caller : Nat) (id : String) (targets : List Target)
      (documents : List Document) (observations : Observations)
  | advance (caller : Nat) (action : Action)
  | finishWrite (caller : Nat) (receipt : Receipt)
  | launch (caller : Nat)
  | withdraw (caller : Nat)
  | settle (caller : Nat)
  | cancel (caller : Nat)
  | release (caller : Nat)
  | physicalLoss (caller : Nat) (candidateInstalled : Bool)
  | helperLost (caller : Nat) (candidateInstalled : Bool)
  | settleOrphan (caller : Nat)
  deriving DecidableEq, Repr

private def owned (world : World) (caller : Nat)
    (transition : Holder → World × Output) : World × Output :=
  match world.holder.filter (fun holder => holder.caller == caller) with
  | some holder => transition holder
  | none => match world.detached.find? (fun value => value.holder.caller == caller) with
    | some value => transition value.holder
    | none => (world, .refused)

private def keep (world : World) (holder : Holder) (output : Output) : World × Output :=
  if world.detached.any (fun value => value.holder.caller == holder.caller) then
    ({world with detached := world.detached.map fun value =>
       if value.holder.caller == holder.caller then
         {holder, candidate := if value.holder.pending == holder.pending then value.candidate else none}
       else value}, output)
  else ({world with holder := some holder}, output)

@[simp] private theorem keep_stored (world : World) (holder : Holder) (output : Output) :
    (keep world holder output).1.stored = world.stored := by
  simp only [keep]
  split <;> rfl

private def usable (holder : Holder) : Bool :=
  holder.loaded && !holder.refused && holder.pending.isNone && holder.running == .none

private def refuse (world : World) (holder : Holder) : World × Output :=
  keep world {holder with refused := true, grant := .none} .refused

/-- A lost helper releases its OS lock independently of the editor/save promise.
An unresolved existing operation therefore blocks the whole workspace on a new
connection, not only matching physical actions. A closed definitively untouched,
rejected or settled operation preserves the existing per-action reservation
policy. No timestamp, PID, new postimage or observed orphan settlement clears an
unresolved operation automatically. -/
def unresolved (ledger : Ledger) : Bool :=
  ledger.any fun operation => !operation.sequence.closed || operation.sequence.items.any fun progress =>
    progress.restore == .pending || progress.restore == .unknown ||
    (progress.restore == .applied &&
      (progress.save.state == .pending || progress.save.state == .saving || progress.save.state == .unknown))

private def read (world : World) (holder : Holder) (legacy : LegacyObservation) : World × Output :=
  if holder.loaded || holder.refused || holder.pending.isSome then (world, .refused)
  else if legacy != .empty then refuse world holder
  else match world.stored with
  | .absent => keep world {holder with loaded := true, ledger := []} .loaded
  | .present version ledger =>
    if version == 1 && ledgerValid ledger && !unresolved ledger then
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
    let allowed := match world.detached.find? (fun value => value.holder.caller == holder.caller) with
      | none => true
      | some value => value.candidate == some proposal
    let detached := world.detached.any (fun value => value.holder.caller == holder.caller)
    let stored := if receipt.installed && allowed && !detached then Stored.present 1 proposal.ledger else world.stored
    let next := {world with stored}
    if allowed && receipt.installed && receipt.durable && receipt.acknowledged then
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
    else if world.orphans.any (fun orphan => orphan.1 == caller) ||
      world.detached.any (fun value => value.holder.caller == caller) then (world, .refused)
    else match observation with
    | .acquired => keep world {caller} .acquired
    | .busy => (world, .busy)
    | .unavailable => (world, .refused)
  | .load caller legacy => owned world caller fun holder =>
    if world.detached.any (fun value => value.holder.caller == caller) then refuse world holder
    else read world holder legacy
  | .admit caller id targets documents observations =>
    owned world caller (fun holder => admit world holder id targets documents observations)
  | .advance caller action => owned world caller (fun holder => advance world holder action)
  | .finishWrite caller receipt => owned world caller (fun holder => finishWrite world holder receipt)
  | .launch caller => owned world caller (launch world)
  | .withdraw caller => owned world caller fun holder =>
    if !usable holder || holder.grant == .none then (world, .refused)
    else keep world {holder with grant := .none} .withdrawn
  | .settle caller => owned world caller fun holder =>
    if holder.running == .none then (world, .refused)
    else keep world {holder with running := .none} .settled
  | .cancel caller => owned world caller fun holder =>
    keep world {holder with cancelled := true, grant := .none} .cancelled
  | .release caller => owned world caller fun holder =>
    if holder.pending.isSome || holder.running != .none then (world, .held)
    else if world.detached.any (fun value => value.holder.caller == caller) then
      ({world with detached := world.detached.filter (fun value => value.holder.caller != caller)}, .refused)
    else ({world with holder := none}, .released)
  | .physicalLoss caller candidateInstalled =>
    match world.holder with
    | none => (world, .refused)
    | some holder =>
      if holder.caller != caller then (world, .refused)
      else
        let candidate := if candidateInstalled then holder.pending else none
        let stored := match candidate with
          | some proposal => Stored.present 1 proposal.ledger
          | none => world.stored
        ({world with stored, holder := none, detached := world.detached ++ [⟨holder, candidate⟩]}, .guardLost)
  | .helperLost caller candidateInstalled => owned world caller fun holder =>
    let alreadyDetached := world.detached.any (fun value => value.holder.caller == caller)
    let stored := match holder.pending with
      | some proposal => if candidateInstalled && !alreadyDetached then Stored.present 1 proposal.ledger else world.stored
      | none => world.stored
    let orphans := if holder.running == .none then world.orphans
      else world.orphans ++ [(caller, holder.running)]
    ({world with stored, holder := world.holder.filter (fun current => current.caller != caller), detached := world.detached.filter (fun value => value.holder.caller != caller), orphans}, .helperLost)
  | .settleOrphan caller =>
    if world.orphans.any (fun orphan => orphan.1 == caller) then
      ({world with orphans := world.orphans.filter (fun orphan => orphan.1 != caller)}, .orphanSettled)
    else (world, .refused)

/-- Every other output is an observation or checkpoint request, never an effect. -/
def Output.editorEffect : Output → Effect
  | .effect value => value
  | _ => .none

theorem empty_guard_cannot_launch (stored : Stored) (caller : Nat) :
    (step ⟨stored, none, [], []⟩ (.launch caller)).2 = .refused := by
  simp [step, owned, Option.filter]

theorem held_guard_cannot_be_replaced (stored : Stored) (holder : Holder)
    (caller : Nat) (observation : GuardObservation) :
    step ⟨stored, some holder, [], []⟩ (.acquire caller observation) = (⟨stored, some holder, [], []⟩, .busy) := by
  simp [step]

theorem cancellation_preserves_running_promise (stored : Stored) (holder : Holder) :
    (step ⟨stored, some holder, [], []⟩ (.cancel holder.caller)).1.holder =
      some {holder with cancelled := true, grant := .none} := by
  simp [step, owned, Option.filter, keep]

theorem running_promise_cannot_release (stored : Stored) (holder : Holder)
    (running : (holder.running != .none) = true) :
    step ⟨stored, some holder, [], []⟩ (.release holder.caller) = (⟨stored, some holder, [], []⟩, .held) := by
  simp [step, owned, Option.filter, running]

theorem pending_checkpoint_cannot_release (stored : Stored) (holder : Holder)
    (pending : holder.pending.isSome = true) :
    step ⟨stored, some holder, [], []⟩ (.release holder.caller) = (⟨stored, some holder, [], []⟩, .held) := by
  simp [step, owned, Option.filter, pending]

theorem helper_loss_releases_guard (world : World) (holder : Holder) (installed : Bool)
    (owns : world.holder = some holder) :
    (step world (.helperLost holder.caller installed)).1.holder = none := by
  simp [step, owned, Option.filter, owns]

theorem orphan_settlement_cannot_rewrite_journal (world : World) (caller : Nat) :
    (step world (.settleOrphan caller)).1.stored = world.stored := by
  simp only [step]
  split <;> rfl

/-- Only the host's unused grant may be withdrawn. Its editor/save effect has not
been invoked; independently owned editor save observations retain their existing
premises. Withdrawal changes no stored marker and cannot reissue an effect from
the already pending/saving sequence. -/
theorem withdrawal_cannot_rewrite_journal (world : World) (caller : Nat) :
    (step world (.withdraw caller)).1.stored = world.stored := by
  simp only [step, owned]
  split
  · split <;> simp_all
  · split
    · split <;> simp_all
    · rfl

theorem unresolved_journal_refuses_new_connection (ledger : Ledger) (caller : Nat)
    (orphans : List (Nat × Effect)) (blocked : unresolved ledger = true) :
    (step ⟨.present 1 ledger, some {caller}, orphans, []⟩ (.load caller .empty)).2 = .refused := by
  simp [step, owned, Option.filter, read, blocked, refuse, keep]

end Workspace.ChangeReview.Editor.Checkpoint
