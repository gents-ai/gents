/-! A selected-request patch is reusable only after its publication was
accepted, and only while both its read fence and its physical request rows
remain current. -/
namespace ClientShell.RequestPatchPublication

inductive CacheState where
  | empty
  | memoized
  deriving DecidableEq, Repr

def publish (state : CacheState) (accepted : Bool) : CacheState :=
  if accepted then .memoized else state

def reusable (state : CacheState) (sameSignature fenceAccepted rowsCurrent : Bool) : Bool :=
  state == .memoized && sameSignature && fenceAccepted && rowsCurrent

theorem rejected_publication_is_not_memoized :
    publish .empty false = .empty := by
  decide

theorem accepted_publication_is_memoized :
    publish .empty true = .memoized := by
  decide

theorem cached_patch_requires_current_fence_and_rows (sameSignature fenceAccepted rowsCurrent : Bool) :
    reusable .memoized sameSignature fenceAccepted rowsCurrent = true →
      sameSignature = true ∧ fenceAccepted = true ∧ rowsCurrent = true := by
  simp [reusable]

end ClientShell.RequestPatchPublication
