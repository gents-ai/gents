import Proofs.P2P.PairingReconcile.OwnedDiff

/-! One reconcile tick for one peer: read the desired wiring, dial if the peer
is not known to be connected, issue the owned diff against the transport, and
record what was applied. -/

namespace PairingReconcile.Tick

open PairingReconcile.OwnedDiff

/-- A replicator as the transport holds it. `filter` is `none` when the
transport does not report filters. -/
structure Replicator where
  address : Address
  collections : List String
  filter : Option (List LiveScope)
  deriving DecidableEq, Repr

/-- Premise (transport reporting): a replicator is reported at the address it
was installed at, with the collections it carries and, when the transport
reports filters, the filter it was installed with. The embedded transport's
replication tests are the evidence. -/
def observe (replicator : Replicator) : LiveRoute :=
  { address := replicator.address
    collections := some replicator.collections
    filterUnobserved := replicator.filter.isNone
    filter := replicator.filter
    observed := some [replicator.address] }

structure World where
  replicators : List Replicator
  applied : Applied
  deriving DecidableEq, Repr

/-- What one tick reads. `desired` is `none` when no wiring is desired for the
peer; `desiredReadable` is false when the desired state could not be read at
all, which is not the same thing. -/
structure Sweep where
  desiredReadable : Bool
  desired : Option Desired
  activeReadable : Bool
  activePeers : List String
  deriving DecidableEq, Repr

structure Outcome where
  readFailed : Bool
  dialed : Bool
  peerActive : Bool
  ops : List Op
  liveMatches : Bool
  world : World
  deriving DecidableEq, Repr

def noWiring : Desired := { routes := [], collections := [], filter := [] }

def installed (desired : Desired) (address : Address) : Replicator :=
  { address, collections := desired.collections
    filter := some (desired.filter.map (⟨·, .predicate⟩)) }

/-- The effect of one issued operation. An install replaces whatever the
transport held at that address and records the route with the desired scope; a
teardown removes it from both, and the applied scope goes with the last
route. -/
def applyOp (desired : Desired) (world : World) (op : Op) : World :=
  match op.kind with
  | .install =>
      match desired.routes.find? (·.text == op.address) with
      | none => world
      | some address =>
          { replicators :=
              world.replicators.filter (·.address.text != op.address) ++ [installed desired address]
            applied :=
              { routes := world.applied.routes.filter (·.text != op.address) ++ [address]
                filter := desired.filter } }
  | .teardown =>
      let routes := world.applied.routes.filter (·.text != op.address)
      { replicators := world.replicators.filter (·.address.text != op.address)
        applied := { routes, filter := if routes.isEmpty then [] else world.applied.filter } }

/-- The peer counts as connected only when the active-peer list could be read
and names the peer of a dialable desired route. An unreadable list, another
peer's entry, and a route with no dial address all leave it unknown. -/
def knownActive (sweep : Sweep) (desired : Desired) : Bool :=
  sweep.activeReadable &&
    desired.routes.any fun route =>
      route.dial.isSome && route.peer.any sweep.activePeers.contains

/-- Decision: an unknown connection is dialed, and so is a known one whose
applied routes differ from the desired ones, because the endpoint moved. -/
def dials (sweep : Sweep) (desired : Desired) (applied : Applied) : Bool :=
  !desired.routes.isEmpty &&
    ((!applied.routes.isEmpty && !sameMembers (texts applied.routes) (texts desired.routes)) ||
      !knownActive sweep desired)

/-- Decision: a failed read of the desired state changes nothing, whereas an
absent desired state tears down everything this node applied and forgets the
applied record. -/
def tick (sweep : Sweep) (world : World) : Outcome :=
  if !sweep.desiredReadable then
    { readFailed := true, dialed := false, peerActive := false, ops := [], liveMatches := false
      world }
  else
    let desired := sweep.desired.getD noWiring
    let ops := ownedDiff desired (world.replicators.map observe) world.applied
    let after := ops.foldl (applyOp desired) world
    { readFailed := false
      dialed := dials sweep desired world.applied
      peerActive := knownActive sweep desired || dials sweep desired world.applied
      ops
      liveMatches := liveMatches desired (after.replicators.map observe) after.applied
      world :=
        if sweep.desired.isNone && !after.applied.routes.isEmpty then
          { after with applied := { routes := [], filter := [] } }
        else after }

theorem read_failure_changes_nothing (sweep : Sweep) (world : World)
    (h : sweep.desiredReadable = false) :
    (tick sweep world).world = world ∧ (tick sweep world).ops = [] ∧
      (tick sweep world).dialed = false := by
  simp [tick, h]

theorem applyOp_keeps_other_addresses (desired : Desired) (world : World) (op : Op)
    (replicator : Replicator) (h_mem : replicator ∈ world.replicators)
    (h_other : replicator.address.text ≠ op.address) :
    replicator ∈ (applyOp desired world op).replicators := by
  unfold applyOp
  split
  · split
    · exact h_mem
    · simp [h_mem, h_other]
  · simp [h_mem, h_other]

theorem fold_keeps_untouched_addresses (desired : Desired) (ops : List Op) (world : World)
    (replicator : Replicator) (h_mem : replicator ∈ world.replicators)
    (h_other : ∀ op ∈ ops, replicator.address.text ≠ op.address) :
    replicator ∈ (ops.foldl (applyOp desired) world).replicators := by
  induction ops generalizing world with
  | nil => exact h_mem
  | cons op rest ih =>
      simp only [List.foldl_cons]
      exact ih _ (applyOp_keeps_other_addresses desired world op replicator h_mem
        (h_other op (List.mem_cons_self _ _)))
        (fun other h_rest => h_other other (List.mem_cons_of_mem _ h_rest))

/-- Every operation of a sweep names an applied route or a route with no live
replicator. -/
theorem op_is_applied_or_missing {desired : Desired} {live : List LiveRoute} {applied : Applied}
    {op : Op} (h : op ∈ ownedDiff desired live applied) :
    op.address ∈ texts applied.routes ∨ op.address ∉ texts (live.map (·.address)) := by
  simp only [ownedDiff, List.mem_append, List.mem_map, List.mem_flatMap] at h
  rcases h with (⟨a, ha, rfl⟩ | ⟨a, ha, h⟩) | ⟨a, ha, rfl⟩
  · exact Or.inl (mem_ordered.mp (List.mem_filter.mp ha).1)
  · have hmem := (List.mem_filter.mp ha).2
    simp only [Bool.and_eq_true, List.contains_iff_mem] at hmem
    simp at h
    rcases h with rfl | rfl <;> exact Or.inl hmem.1.1
  · have hmem := (List.mem_filter.mp ha).2
    exact Or.inr (by simpa using hmem)

/-- A replicator at an address this node never applied survives every tick. -/
theorem unowned_replicator_survives (sweep : Sweep) (world : World) (replicator : Replicator)
    (h_mem : replicator ∈ world.replicators)
    (h_unowned : replicator.address.text ∉ texts world.applied.routes) :
    replicator ∈ (tick sweep world).world.replicators := by
  unfold tick
  split
  · exact h_mem
  · have h_kept := fold_keeps_untouched_addresses (sweep.desired.getD noWiring)
      (ownedDiff (sweep.desired.getD noWiring) (world.replicators.map observe) world.applied)
      world replicator h_mem (by
        intro op h_op h_same
        rcases op_is_applied_or_missing h_op with h_applied | h_missing
        · exact h_unowned (h_same ▸ h_applied)
        · apply h_missing
          simp only [texts, List.map_map, List.mem_map, Function.comp]
          exact ⟨replicator, h_mem, by simpa [observe] using h_same⟩)
    simp only []
    split <;> exact h_kept

end PairingReconcile.Tick
