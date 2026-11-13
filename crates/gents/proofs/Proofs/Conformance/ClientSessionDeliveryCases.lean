import Proofs.Session.Client.Types
import Proofs.Conformance.Family

namespace Conformance.ClientSessionDelivery

open Lean

deriving instance ToJson for SessionRowObservation, DirectSessionRead

structure ReadInput where
  sessionRow : SessionRowObservation
  deriving ToJson

structure ReadOutput where
  expected : DirectSessionRead
  deriving ToJson

@[conformance_family]
def directRead : Conformance.Family ReadInput ReadOutput where
  inputs :=
    ([.absent, .canonical, .duplicate, .outOfScope] : List SessionRowObservation).map
      fun sessionRow => { sessionRow }
  model := fun i => { expected := directSessionRead i.sessionRow }

end Conformance.ClientSessionDelivery
