import Proofs.Documents.AgentViews
import Proofs.Conformance.Family

namespace AgentViews

deriving instance Lean.ToJson for Row, Request

structure Input where
  request : Request
  documents : List Row
  deriving Lean.ToJson

def identities (input : Input) : List String :=
  (inventory input.request input.documents).map Row.agentDid

end AgentViews

namespace Conformance

@[conformance_family]
def agentInventory : Family AgentViews.Input (List String) where
  inputs := ["node-a", "node-b"].flatMap fun node =>
    [none, some "reader"].flatMap fun reader =>
      [none, some "agent-b"].flatMap fun agent =>
        [none, some "agent-a"].flatMap fun after =>
          [none, some 1, some 2].map fun limit =>
            { request :=
                { nodeDid := node
                  reader := reader
                  agentDid := agent
                  after := after
                  limit := limit }
              documents := [
                { agentDid := "agent-c", nodeDid := "node-a", readable := true },
                { agentDid := "agent-d", nodeDid := "node-b", readable := true },
                { agentDid := "agent-b", nodeDid := "node-a", readable := true },
                { agentDid := "agent-a", nodeDid := "node-a", readable := true },
                { agentDid := "hidden", nodeDid := "node-a", readable := false }] }
  model := AgentViews.identities

end Conformance
