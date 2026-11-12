import Proofs.P2P.Enrollment.State
import Proofs.Conformance.Family

namespace Conformance.EnrollmentCandidateName

open Lean

structure NameInput where
  name : Option String
  deriving ToJson

@[conformance_family]
def fields : Conformance.Family NameInput (List String) where
  inputs := [{ name := none }, { name := some "Ivan" }, { name := some "顾问" }]
  model := fun i => Enrollment.candidateNameFields i.name

end Conformance.EnrollmentCandidateName
