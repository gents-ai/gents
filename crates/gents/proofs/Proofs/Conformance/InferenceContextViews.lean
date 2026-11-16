import Proofs.Inference.ContextViews
import Proofs.Conformance.Family

namespace InferenceContextViews

inductive Cursor where
  | start
  | firstNull
  | secondNull
  | early
  | queued
  | sequenced
  | late
  deriving Repr, Lean.ToJson

def fixture : List (Nat × Key) :=
  [ (0, ⟨none, none, none, 0⟩)
  , (1, ⟨none, none, none, 1⟩)
  , (2, ⟨some 0, none, none, 2⟩)
  , (3, ⟨some 1, none, none, 3⟩)
  , (4, ⟨some 1, some 1, none, 4⟩)
  , (5, ⟨some 1, some 1, some 1, 5⟩) ]

def cursorKey : Cursor → Option Key
  | .start => none
  | .firstNull => some fixture[0]!.2
  | .secondNull => some fixture[1]!.2
  | .early => some fixture[2]!.2
  | .queued => some fixture[3]!.2
  | .sequenced => some fixture[4]!.2
  | .late => some fixture[5]!.2

@[conformance_function]
def page (cursor : Cursor) : List Nat :=
  (((fixture.filter fun (_, key) => (cursorKey cursor).all (after key)).mergeSort
    (fun left right => !after left.2 right.2)).take 2).map (·.1)

end InferenceContextViews
