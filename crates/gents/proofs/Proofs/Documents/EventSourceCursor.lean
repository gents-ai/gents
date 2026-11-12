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

end EventSourceCursor
