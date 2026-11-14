namespace ClientShell.RequestPatchPublication

inductive CacheState where
  | empty
  | memoized
  deriving DecidableEq, Repr

def publish (state : CacheState) (accepted : Bool) : CacheState :=
  if accepted then .memoized else state

def reusable (state : CacheState) (sameSignature fenceAccepted rowsCurrent : Bool) : Bool :=
  state == .memoized && sameSignature && fenceAccepted && rowsCurrent

inductive FreshReadAction where
  | mergeFresh
  | rejectCandidate
  | noPatch
  deriving DecidableEq, Repr

def freshReadAction (fenceAccepted patchAvailable : Bool) : FreshReadAction :=
  if !patchAvailable then .noPatch
  else if fenceAccepted then .mergeFresh
  else .rejectCandidate

theorem rejected_publication_is_not_memoized :
    publish .empty false = .empty := by
  decide

theorem accepted_publication_is_memoized :
    publish .empty true = .memoized := by
  decide

theorem cached_patch_requires_current_fence_and_rows (sameSignature fenceAccepted rowsCurrent : Bool) :
    reusable .memoized sameSignature fenceAccepted rowsCurrent = true →
      sameSignature = true ∧ fenceAccepted = true ∧ rowsCurrent = true := by
  intro h
  simpa [reusable, Bool.and_eq_true, and_assoc] using h

end ClientShell.RequestPatchPublication
