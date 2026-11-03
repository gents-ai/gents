import Proofs.AgentSession
import Lean

namespace Conformance.TaskLineage
open Lean

private def scopeJson (scope : AgentSession.Scope) : Json := Json.mkObj
  [("node", toJson scope.node), ("session", toJson scope.session),
   ("requester", toJson scope.requester)]

private def rowJson (row : AgentSession.TaskLineageRow) : Json := Json.mkObj
  [("scope", scopeJson row.scope), ("task", toJson row.task),
   ("starter", row.starter.map scopeJson |>.getD Json.null)]

def cases : List Json :=
  let root : AgentSession.Scope := ⟨1, 10, some 7⟩
  let child : AgentSession.Scope := ⟨1, 11, some 7⟩
  let grandchild : AgentSession.Scope := ⟨1, 12, some 7⟩
  let wrongRequester : AgentSession.Scope := ⟨1, 10, none⟩
  let foreign : AgentSession.Scope := ⟨2, 10, some 7⟩
  let rootRow : AgentSession.TaskLineageRow := ⟨root, some 42, none⟩
  let childRow : AgentSession.TaskLineageRow := ⟨child, none, some root⟩
  let grandchildRow : AgentSession.TaskLineageRow := ⟨grandchild, none, some child⟩
  [ ("direct", root, [rootRow]),
    ("child", child, [rootRow, childRow]),
    ("grandchild", grandchild, [rootRow, childRow, grandchildRow]),
    ("missing_parent", child, [childRow]),
    ("ambiguous_parent", child, [rootRow, rootRow, childRow]),
    ("requester_mismatch", child, [rootRow, { childRow with starter := some wrongRequester }]),
    ("foreign_parent", child, [rootRow, { childRow with starter := some foreign }, ⟨foreign, some 43, none⟩]),
    ("cycle", child, [{ childRow with starter := some grandchild }, grandchildRow]),
    ("unverified_starter", child, [rootRow, { childRow with starter := none }]),
    ("direct_task_wins", child, [rootRow, { childRow with task := some 44 }]) ].map
    fun (name, scope, rows) => Json.mkObj
      [("name", toJson name), ("operation", toJson "task_lineage"),
       ("owner", toJson (1 : Nat)), ("scope", scopeJson scope),
       ("rows", toJson (rows.map rowJson)),
       ("task", toJson (AgentSession.originatingTask 1 scope rows [] (rows.length + 1)))]

example : AgentSession.originatingTask 1 ⟨1, 11, some 7⟩
    [⟨⟨1, 10, some 7⟩, some 42, none⟩,
     ⟨⟨1, 11, some 7⟩, none, some ⟨1, 10, some 7⟩⟩] [] 3 = some 42 := by decide

example : AgentSession.originatingTask 1 ⟨1, 11, some 7⟩
    [⟨⟨1, 10, some 7⟩, some 42, none⟩,
     ⟨⟨1, 10, some 7⟩, some 42, none⟩,
     ⟨⟨1, 11, some 7⟩, none, some ⟨1, 10, some 7⟩⟩] [] 4 = none := by decide

example : AgentSession.originatingTask 1 ⟨1, 11, some 7⟩
    [⟨⟨1, 11, some 7⟩, none, some ⟨2, 10, some 7⟩⟩,
     ⟨⟨2, 10, some 7⟩, some 42, none⟩] [] 3 = none := by decide

example : AgentSession.originatingTask 1 ⟨1, 11, some 7⟩
    [⟨⟨1, 11, some 7⟩, none, some ⟨1, 12, some 7⟩⟩,
     ⟨⟨1, 12, some 7⟩, none, some ⟨1, 11, some 7⟩⟩] [] 3 = none := by decide

end Conformance.TaskLineage
