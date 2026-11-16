import Proofs.Store.Identity.ClientAgentCreation
import Proofs.Conformance.Family

namespace Conformance.ClientAgentCreation

open Lean Identity.ClientAgentCreation

deriving instance ToJson for Input, Route, DocumentSchemaSource, DocumentCreateInput

/-- The native route decision observes the create intent, the target
collection, the create operation and the transport. A case whose other
capability facts fail is decided by checks outside this owner. A case without
a signer is decided at execution, before any connection. -/
def routeObserved (i : Input) : Bool :=
  i.supported && i.signerMatchesBearer && i.inputMatchesGraph && i.trustedSchema &&
    i.graphVerified && i.transportReady &&
    (i.signerAvailable || (i.clientIntent && i.agentCollection && i.create && i.http))

@[conformance_family]
def agentRoute : Family Input Route where
  inputs := (cases.map (·.input)).filter routeObserved
  model := fun i => route i

@[conformance_family]
def documentRoute : Family DocumentCreateInput Route where
  inputs := documentCreateCases.map (·.input)
  model := fun i => documentCreateRoute i

end Conformance.ClientAgentCreation
