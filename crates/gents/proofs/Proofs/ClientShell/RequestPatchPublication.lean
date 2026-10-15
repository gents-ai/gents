/-! A selected-request patch is reusable only after its publication was
accepted, and only while both its read fence and its physical request rows
remain current. A nonreusable cache entry does not veto an authoritative fresh
read: that candidate is admitted by its captured publication fence. -/
namespace ClientShell.RequestPatchPublication

inductive CacheState where
  | empty
  | memoized
  deriving DecidableEq, Repr

def publish (state : CacheState) (accepted : Bool) : CacheState :=
  if accepted then .memoized else state

def reusable (state : CacheState) (sameSignature fenceAccepted rowsCurrent : Bool) : Bool :=
  state == .memoized && sameSignature && fenceAccepted && rowsCurrent

/-- Once the authoritative read is complete, cache contents no longer decide
whether its candidate can publish. Only the final store fence and presence of
the requested patch matter. -/
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
