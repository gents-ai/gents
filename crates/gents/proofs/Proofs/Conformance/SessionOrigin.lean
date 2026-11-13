import Proofs.Session.SessionPartition
import Proofs.Conformance.Family

namespace Conformance.SessionOrigin
open Lean DurableLineage SessionPartition

structure CallRequest where
  requestId : String
  call : String
  deriving Lean.ToJson

structure ProjectionInput where
  requests : List CallRequest
  calls : List String
  deriving Lean.ToJson

structure CallProjection where
  call : String
  request : Option String
  deriving Lean.ToJson

structure ProjectionOutput where
  projections : List CallProjection
  deriving Lean.ToJson

private def callRow (r : CallRequest) : RequestRow :=
  { createdAt := 1, docId := r.requestId, session := "child", isPublic := true
  , causeRequestDoc := some "bae-parent", causeToolCallDoc := some r.call }

@[conformance_family]
def projection : Conformance.Family ProjectionInput ProjectionOutput where
  inputs :=
    [ { requests := [ { requestId := "a1", call := "call-one" }
                    , { requestId := "b1", call := "call-two" }
                    , { requestId := "b2", call := "call-two" } ]
      , calls := ["call-one", "call-two"] }
    , { requests := [ { requestId := "a1", call := "call-one" } ]
      , calls := ["call-one", "call-two"] } ]
  model := fun i =>
    let rows := i.requests.map callRow
    { projections := i.calls.map fun c =>
        { call := c, request := (singleRequestProjection rows c).map (·.docId) } }

structure SessionInput where
  id : String
  starter : Option String
  deriving Lean.ToJson

structure RequestInput where
  doc : String
  session : String
  cause : Option String
  createdAt : Nat
  deriving Lean.ToJson

structure LineageInput where
  sessions : List SessionInput
  requests : List RequestInput
  scopes : List String
  deriving Lean.ToJson

structure LineageView where
  session : String
  startedBy : Option String
  started : List String
  sent : List String
  received : List String
  deriving Lean.ToJson

structure LineageOutput where
  views : List LineageView
  deriving Lean.ToJson

private def requestRow (r : RequestInput) : RequestRow :=
  { createdAt := r.createdAt, docId := r.doc, session := r.session, isPublic := true
  , causeRequestDoc := r.cause, causeToolCallDoc := none }

@[conformance_family]
def lineage : Conformance.Family LineageInput LineageOutput where
  inputs :=
    [ { sessions := [ { id := "parent", starter := none }, { id := "existing", starter := none }
                    , { id := "started", starter := none }, { id := "other", starter := none } ]
      , requests := [ { doc := "p1", session := "parent", cause := none, createdAt := 1 }
                    , { doc := "s1", session := "existing", cause := none, createdAt := 0 }
                    , { doc := "s2", session := "existing", cause := some "p1", createdAt := 2 }
                    , { doc := "c1", session := "started", cause := some "p1", createdAt := 2 }
                    , { doc := "o1", session := "other", cause := none, createdAt := 1 }
                    , { doc := "a0", session := "started", cause := some "o1", createdAt := 2 } ]
      , scopes := ["parent", "started", "existing", "other"] }
    , { sessions := [ { id := "solo", starter := none } ]
      , requests := [ { doc := "x1", session := "solo", cause := none, createdAt := 1 } ]
      , scopes := ["solo"] } ]
  model := fun i =>
    let rows := i.requests.map requestRow
    let sessions : List SessionPartition.Session :=
      i.sessions.map fun s => { id := s.id, starter := s.starter }
    { views := i.scopes.map fun s =>
        { session := s, startedBy := starterOf sessions s, started := startedList sessions s
        , sent := sentList rows sessions s, received := receivedList rows sessions s } }

end Conformance.SessionOrigin
