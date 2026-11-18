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

structure RevokedReaderInput where
  reopened : Bool
  deriving ToJson

structure RevokedReaderOutput where
  documentAcp : Bool
  hostRead : Bool
  deriving ToJson

/-- The client revokes its own `reader` relation on the received decision.
Document ACP then grants it nothing, and it still reads the decision as the
key hosting its own store (`Household.nativeRead`), before and after the
store is reopened. -/
@[conformance_family]
def revokedReader : Family RevokedReaderInput RevokedReaderOutput where
  inputs := [{ reopened := false }, { reopened := true }]
  model := fun _ =>
    let revoked := apply clientTransportPublished
      (.revoke ⟨"enrollment_records", "decision", "reader", .actor "client"⟩)
    { documentAcp := allowed 5 revoked "client" "enrollment_records" "decision" "read"
      hostRead := nativeRead true 5 revoked "client" "enrollment_records" "decision" }

end Conformance.ClientTransport
