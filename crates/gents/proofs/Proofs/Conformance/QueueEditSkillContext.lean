import Proofs.Session.Enrollment.RequestInput
import Proofs.Conformance.Family

namespace Conformance.QueueEditSkillContext

open Lean Enrollment

deriving instance ToJson for QueueEditResult
deriving instance ToJson for OriginalRequest

structure EditInput where
  selectedSkillIds : List String
  contextSkillIds : List String
  deriving ToJson

structure EditOutput where
  result : QueueEditResult
  original : OriginalRequest
  deriving ToJson

@[conformance_family]
def edit : Family EditInput EditOutput where
  inputs :=
    [ { selectedSkillIds := ["unavailable"], contextSkillIds := [] }
    , { selectedSkillIds := [], contextSkillIds := [] }
    , { selectedSkillIds := ["review"], contextSkillIds := ["review"] }
    , { selectedSkillIds := ["unavailable"], contextSkillIds := ["review"] }
    , { selectedSkillIds := ["review", "unavailable"], contextSkillIds := ["review"] } ]
  model := fun i =>
    let result := queueEditResult i.selectedSkillIds i.contextSkillIds
    { result := result, original := originalAfterEdit result }

end Conformance.QueueEditSkillContext
