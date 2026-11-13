import Proofs.Documents.DesiredStatePlan
import Proofs.Conformance.Family

namespace Conformance.DesiredStatePlan

open Lean _root_.DesiredStatePlan

deriving instance ToJson for Document

structure Plan where
  documents : List Document
  deriving ToJson

private def singles : List Document :=
  [false, true].flatMap fun undeclaredField =>
    [false, true].flatMap fun sparseReplacement =>
      [none, some 0, some 2].map fun replacementOwner =>
        { owner := 0, id := 0, undeclaredField, sparseReplacement, replacementOwner }

private def pairs : List (List Document) :=
  [ [clean 3 1, clean 4 1]
  , [clean 3 1, clean 3 1]
  , [clean 3 1, clean 3 2]
  , [clean 3 1, clean 4 2]
  , [clean 3 1, { clean 4 1 with undeclaredField := true }]
  , [clean 3 1, { clean 3 2 with replacementOwner := some 4 }]
  , [clean 3 1, { clean 4 1 with replacementOwner := some 3 }] ]

@[conformance_family]
def admission : Family Plan Bool where
  inputs := [] :: (singles.map ([·]) ++ pairs) |>.map ({ documents := · })
  model := fun plan => admitted plan.documents

end Conformance.DesiredStatePlan
