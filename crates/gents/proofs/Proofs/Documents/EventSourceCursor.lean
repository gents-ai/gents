/-! Seeding an EventSource arrival cursor: the position a new cursor starts
after. A source collection that does not exist yet has no arrivals, so its
cursor starts at 0 and the first arrival it later receives is pending. -/

namespace EventSourceCursor

def seedAfter (collectionExists : Bool) (head : Nat) : Nat :=
  if collectionExists then head else 0

theorem existing_collection_seeds_at_head (head : Nat) :
    seedAfter true head = head := rfl

theorem missing_collection_seeds_at_zero (head : Nat) :
    seedAfter false head = 0 := rfl

theorem first_arrival_of_missing_collection_is_pending (head : Nat) :
    seedAfter false head < 1 := by
  simp [seedAfter]

/-- Journal positions of the first `count` arrivals after the collection exists. -/
def arrivalPositions (count : Nat) : List Nat :=
  (List.range count).map (· + 1)

def pendingAfter (after : Nat) (positions : List Nat) : List Nat :=
  positions.filter fun position => decide (after < position)

/-- Publishing a Trigger does not checkpoint; only a fire moves the cursor. -/
def afterTriggerPublication (after : Nat) : Nat := after

theorem source_registered_before_collection_retains_its_first_arrival :
    pendingAfter (seedAfter false 0) (arrivalPositions 1) = [1] := by
  decide

theorem trigger_publication_keeps_missing_collection_cursor :
    afterTriggerPublication (seedAfter false 0) = 0 := rfl

end EventSourceCursor
