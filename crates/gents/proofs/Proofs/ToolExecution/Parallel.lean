import Proofs.Basic


namespace ToolExecution.Parallel

/-- Only calls whose owners prove they have no effects may enter a read batch.
All other footprints, including writes and unknown effects, are barriers. -/
inductive Footprint where
  | read | barrier
  deriving Repr, BEq, DecidableEq

structure Call where
  key : Nat
  footprint : Footprint
  deriving Repr, BEq, DecidableEq

/-- A duplicate is a barrier because the repeated-failure policy depends on
preceding outcomes. The limit also reserves invalid-call budget for every
accepted call. -/
def takeReads (limit : Nat) (seen : List Nat) : List Call → List Call
  | [] => []
  | c :: rest =>
    if limit = 0 ∨ c.footprint = .barrier ∨ c.key ∈ seen then []
    else c :: takeReads (limit - 1) (c.key :: seen) rest

inductive Admission where
  | run | skip | terminate
  deriving Repr, BEq, DecidableEq

/-- Hook decisions are consumed in call order. Termination prevents any later
admission, while previously admitted runs remain owed a terminal outcome. -/
def accepted : List Admission → List Nat
  | [] => []
  | .terminate :: _ => []
  | .skip :: rest => (accepted rest).map (· + 1)
  | .run :: rest => 0 :: (accepted rest).map (· + 1)

/-- Results are projected by call order, independently of completion order.
A cancelled accepted call is represented by its settled cancellation result. -/
def orderedResults (calls : List Nat) (completed : List (Nat × Nat)) : List (Nat × Option Nat) :=
  calls.map fun call => (call, (completed.find? fun row => row.1 = call).map Prod.snd)

/-- Settlements carry durable terminal results; duplicate or missing call ids
cannot finalize a batch. Cancellation and termination first request cancellation
of every accepted call, and remain pending until the same settlement condition. -/
inductive Stop where
  | none | cancelled | terminated
  deriving Repr, BEq, DecidableEq

structure Batch where
  accepted : List Nat
  settled : List (Nat × Nat)
  stop : Stop
  deriving Repr

def complete (batch : Batch) : Bool :=
  batch.accepted.Nodup && (batch.settled.map Prod.fst).Nodup &&
  batch.accepted.all (fun id => (batch.settled.map Prod.fst).contains id) &&
  batch.settled.all (fun row => batch.accepted.contains row.1)

/-- After cancellation or termination, every accepted call without a durable
settlement is a cancellation target. Finalization remains gated on settlement. -/
def cancellationTargets (batch : Batch) : List Nat :=
  if batch.stop = .none then [] else
    if !batch.accepted.Nodup ∨ !(batch.settled.map Prod.fst).Nodup then batch.accepted
    else batch.accepted.filter fun id => !(batch.settled.map Prod.fst).contains id

def finish (batch : Batch) : Option (List (Nat × Option Nat)) :=
  if complete batch then some (orderedResults batch.accepted batch.settled) else none

/-- A later result can stop the batch while an earlier call is still running.
The outstanding earlier call must be cancelled before the batch can finish;
completion order does not change provider-visible result order. -/
theorem later_termination_cancels_earlier :
    cancellationTargets ⟨[0, 1], [(1, 10)], .terminated⟩ = [0] ∧
    finish ⟨[0, 1], [(1, 10)], .terminated⟩ = none ∧
    finish ⟨[0, 1], [(1, 10), (0, 0)], .terminated⟩ =
      some [(0, some 0), (1, some 10)] := by decide

theorem barrier_stops (limit : Nat) (seen : List Nat) (key : Nat) (rest : List Call) :
    takeReads limit seen (⟨key, .barrier⟩ :: rest) = [] := by
  simp [takeReads]

theorem zero_stops (seen : List Nat) (calls : List Call) : takeReads 0 seen calls = [] := by
  cases calls <;> simp [takeReads]

theorem active_has_no_cancellation_targets (batch : Batch) (h : batch.stop = .none) :
    cancellationTargets batch = [] := by
  simp [cancellationTargets, h]

theorem termination_stops (rest : List Admission) : accepted (.terminate :: rest) = [] := rfl

theorem ordered_ids (calls : List Nat) (completed : List (Nat × Nat)) :
    (orderedResults calls completed).map Prod.fst = calls := by
  simp [orderedResults, List.map_map, Function.comp_def]

/-- A batch is a prefix of the remaining calls: no call overtakes a barrier or
any earlier call. -/
theorem take_reads_prefix (limit : Nat) (seen : List Nat) (calls : List Call) :
    takeReads limit seen calls <+: calls := by
  induction calls generalizing limit seen with
  | nil => simp [takeReads]
  | cons c rest ih =>
    simp only [takeReads]
    split
    · exact List.nil_prefix
    · exact List.cons_prefix_cons.mpr ⟨rfl, ih _ _⟩

/-- The invalid-call reservation bounds the number of concurrently started calls. -/
theorem take_reads_bounded (limit : Nat) (seen : List Nat) (calls : List Call) :
    (takeReads limit seen calls).length ≤ limit := by
  induction calls generalizing limit seen with
  | nil => simp [takeReads]
  | cons c rest ih =>
    simp only [takeReads]
    split
    · simp
    · rename_i h
      have : limit ≠ 0 := fun hz => h (Or.inl hz)
      have := ih (limit - 1) (c.key :: seen)
      simp only [List.length_cons]
      omega

/-- Every started call is a read whose identity is fresh, so the repeated-failure
owner never has to judge a repeat whose predecessor is still running. -/
theorem take_reads_fresh_reads (limit : Nat) (seen : List Nat) (calls : List Call) :
    ∀ c ∈ takeReads limit seen calls, c.footprint = .read ∧ c.key ∉ seen := by
  induction calls generalizing limit seen with
  | nil => simp [takeReads]
  | cons c rest ih =>
    simp only [takeReads]
    split
    · simp
    · rename_i h
      intro d hd
      simp only [List.mem_cons] at hd
      rcases hd with rfl | hd
      · refine ⟨?_, fun hk => h (Or.inr (Or.inr hk))⟩
        cases hf : d.footprint
        · rfl
        · exact absurd hf (fun hb => h (Or.inr (Or.inl hb)))
      · have := ih (limit - 1) (c.key :: seen) d hd
        exact ⟨this.1, fun hk => this.2 (List.mem_cons_of_mem _ hk)⟩

theorem take_reads_keys_nodup (limit : Nat) (seen : List Nat) (calls : List Call) :
    ((takeReads limit seen calls).map Call.key).Nodup := by
  induction calls generalizing limit seen with
  | nil => simp [takeReads]
  | cons c rest ih =>
    simp only [takeReads]
    split
    · simp
    · refine List.nodup_cons.mpr ⟨?_, ih _ _⟩
      intro hk
      obtain ⟨d, hd, hdk⟩ := List.mem_map.mp hk
      exact (take_reads_fresh_reads _ _ rest d hd).2 (hdk ▸ List.mem_cons_self _ _)

/-- A stopped batch leaves no accepted call both unsettled and unrequested, and
never re-cancels a call whose terminal result is already durable. -/
theorem stop_partitions_accepted (batch : Batch) (h : batch.stop ≠ .none)
    (ha : batch.accepted.Nodup) (hs : (batch.settled.map Prod.fst).Nodup) :
    (∀ id ∈ batch.accepted,
      id ∈ batch.settled.map Prod.fst ∨ id ∈ cancellationTargets batch) ∧
    (∀ id ∈ cancellationTargets batch, id ∉ batch.settled.map Prod.fst) := by
  have ht : cancellationTargets batch =
      batch.accepted.filter fun id => !(batch.settled.map Prod.fst).contains id := by
    simp [cancellationTargets, h, ha, hs]
  rw [ht]
  constructor
  · intro id hid
    by_cases hin : id ∈ batch.settled.map Prod.fst
    · exact Or.inl hin
    · exact Or.inr (List.mem_filter.mpr ⟨hid, by simpa using hin⟩)
  · intro id hid
    simpa using (List.mem_filter.mp hid).2

end ToolExecution.Parallel
