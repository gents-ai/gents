import Proofs.Runtime.StartupAdmission
import Proofs.Conformance.Family

namespace Runtime.StartupAdmission

@[conformance_family]
def tryDemote : Conformance.Family DemoteInput DemoteObservation :=
  ⟨[false, true].flatMap (fun pending => [false, true].flatMap fun registered =>
    [false, true].map fun durable => ⟨pending, registered, durable⟩), demote⟩

@[conformance_family]
def becomeReady : Conformance.Family ReadyInput ReadyObservation :=
  ⟨[false, true].flatMap (fun closing => [false, true].map fun retryFirst => ⟨closing, retryFirst⟩), ready⟩

end Runtime.StartupAdmission
