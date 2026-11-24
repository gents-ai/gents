-- List folds and natural-number arithmetic used by the progress laws.
import Std

/-!
An early validator reads keys in order. On a cold block it keeps the marker
from its completed prefix and its original horizon, then retries the current
item. It never claims the missing item finished and never advances the
horizon past versions the prefix did not check. The pipeline checks changes
above that original horizon through PipelineCheck.early_check_cover.

Correspondence: `prefix_is_the_same_check` is `check_early_with_progress`'s
saved Marker and item cursor; `successful_item_preserves_cost` and
`miss_preserves_cost` are its cursor advance and WouldBlock return. The step
is arbitrary, so the marker may include dirty-before, merge and error state.
-/

-- The resumable-check laws have their own names.
namespace Regolith.ValidationProgress

/-- Resume the unfinished suffix, using the state of the completed prefix. -/
def resume {State Item : Type} (step : State → Item → State)
    -- The complete input, next item and saved marker.
    (items : List Item) (next : Nat) (marker : State) : State :=
  -- A miss keeps the current item in the suffix.
  (items.drop next).foldl step marker

/-- Keeping the prefix marker and checking the suffix is the same ordered
check as checking the whole input once, for every marker transition. -/
theorem prefix_is_the_same_check {State Item : Type}
    -- The owned marker transition and complete input.
    (step : State → Item → State) (items : List Item) (next : Nat) (initial : State) :
    -- The saved prefix is not run again and no suffix item is skipped.
    resume step items next ((items.take next).foldl step initial) =
      items.foldl step initial := by
  -- Unfold the suffix and combine the two ordered folds.
  simp only [resume, ← List.foldl_append, List.take_append_drop]

/-- Finishing an item adds exactly one visit and one completed item. -/
theorem successful_item_preserves_cost (visits completed misses : Nat)
    -- All earlier visits either completed an item or missed.
    (accounted : visits = completed + misses) :
    -- One successful visit pays for one new completed item.
    visits + 1 = (completed + 1) + misses := by
  -- Addition preserves the exact accounting equation.
  omega

/-- A miss adds one visit without falsely completing its current item. -/
theorem miss_preserves_cost (visits completed misses : Nat)
    -- All earlier visits are accounted for.
    (accounted : visits = completed + misses) :
    -- The retry will start at the same current item.
    visits + 1 = completed + (misses + 1) := by
  -- This visit is charged only as a miss.
  omega

-- End the resumable-check laws.
end Regolith.ValidationProgress
