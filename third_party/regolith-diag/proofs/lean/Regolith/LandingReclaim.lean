/- A queue's landed block is named by the owner's ledger (Ledger::order,
   src/io_queue/ledger.rs) and by two kovan maps: the landing table
   (QueueShared::landed, src/engine/io/shared.rs) and the unit table
   (IoRuntime::units, src/engine/io/mod.rs). A key removed from a kovan map
   leaves a retired node that kovan frees only on a later step of the thread
   that retired it, a step an idle thread never takes; HashMap and
   HopscotchMap both retire a removed entry so.
   A tiny example: the ledger drops a 44 KiB block and its thread idles. If a
   retired node owns a strong handle, the block lives as long as the idle.
   What must never happen: a block the ledger let go and no reader holds stays
   alive through a map. The laws below hold for every reclamation schedule:
   with weak handles in the maps (Posted::Ready, Weak<Unit>) the block's strong
   count after release is zero; with a strong handle it stays one for any
   number of idle polls. LandingReclaim.tla checks the interleavings. -/
namespace Regolith.LandingReclaim

-- A map node for the block: still posted, retired but not reclaimed, or freed.
inductive Node where
  -- The key is in the map (QueueShared::land, IoRuntime::unit_for).
  | posted
  -- Removed, waiting for kovan (QueueShared::forget, IoRuntime::forget).
  | retired
  -- kovan reclaimed the node and dropped what it owned.
  | gone
  -- Node states can be compared.
  deriving DecidableEq

-- The strong handles a node owns: one when it owns a strong one and is not freed.
def nodeRefs (strong : Bool) : Node → Nat
  -- A freed node owns nothing.
  | .gone => 0
  -- A live or retired node owns its handle, if that handle is strong.
  | _ => if strong then 1 else 0

-- The block's strong count: the ledger's entry, readers' pins, and both maps.
def strongCount (ledger : Bool) (pins : Nat) (strongMap : Bool)
    -- The landing table's node and the unit table's node.
    (landed units : Node) : Nat :=
  -- Each holder adds its own strong handles.
  (if ledger then 1 else 0) + pins + nodeRefs strongMap landed + nodeRefs strongMap units

-- A poll of an idle thread: kovan takes no step, so no node changes.
def idleStep (n : Node) : Node := n

-- The node after k idle polls.
def idlePolls : Nat → Node → Node
  -- No poll leaves the node as it is.
  | 0, n => n
  -- One more poll is one more idle step.
  | k + 1, n => idlePolls k (idleStep n)

-- Weak handles own nothing, whatever state the node is in.
theorem weak_node_owns_nothing (n : Node) : nodeRefs false n = 0 := by
  -- Each node state gives zero once the handle is weak.
  cases n <;> rfl

-- With weak maps, a released block no reader holds is freed, whatever the
-- state of the maps' nodes: reclaimed or not, idle thread or not.
theorem weak_maps_free_released (landed units : Node) :
    -- No ledger entry, no pin, weak maps: the count is zero.
    strongCount false 0 false landed units = 0 := by
  -- Unfold the count and use that weak nodes own nothing.
  simp [strongCount, weak_node_owns_nothing]

-- A strong retired node keeps the block alive through any number of idle polls.
theorem strong_map_keeps_released (k : Nat) :
    -- After k idle polls the released block's count is still at least one.
    1 ≤ strongCount false 0 true (idlePolls k Node.retired) Node.gone := by
  -- Idle steps leave the retired node as it is, for every k.
  have still : idlePolls k Node.retired = Node.retired := by
    -- Induct on the number of idle polls.
    induction k with
    -- No poll: the node is the retired one.
    | zero => rfl
    -- One more poll changes nothing, by the induction hypothesis.
    | succ k ih => simpa [idlePolls, idleStep] using ih
  -- The retired strong node contributes one.
  simp [still, strongCount, nodeRefs]

-- End of the map-reference reclamation laws.
end Regolith.LandingReclaim
