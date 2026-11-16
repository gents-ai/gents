import Proofs.Store.Identity.Operations
import Proofs.Conformance.Family

namespace Conformance.ClientTransport

open Lean Identity.Household Identity.Operations

structure DecisionAccessInput where
  actor : String
  permission : String
  deriving ToJson

structure DecisionAccessOutput where
  allowed : Bool
  graphqlVisible : Option Bool
  deriving ToJson

@[conformance_family]
def decisionAccess : Family DecisionAccessInput DecisionAccessOutput where
  inputs :=
    [ { actor := "server", permission := "update" }
    , { actor := "client", permission := "read" }
    , { actor := "client", permission := "update" }
    , { actor := "user", permission := "read" }
    , { actor := "agent", permission := "read" }
    , { actor := "outsider", permission := "read" } ]
  model := fun i =>
    let granted := allowed 5 clientTransportPublished i.actor "enrollment_records" "decision" i.permission
    { allowed := granted
      graphqlVisible :=
        if i.permission == "read" && ["user", "agent", "outsider"].contains i.actor then some granted
        else none }

end Conformance.ClientTransport
