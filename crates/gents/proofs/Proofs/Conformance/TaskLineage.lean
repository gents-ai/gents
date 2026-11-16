import Proofs.Session.AgentSession
import Proofs.Conformance.Family
import Lean

namespace Conformance.TaskLineage
open Lean

deriving instance ToJson for AgentSession.Scope, AgentSession.TaskLineageRow

private def root : AgentSession.Scope := ⟨1, 10, some 7⟩
private def child : AgentSession.Scope := ⟨1, 11, some 7⟩
private def grandchild : AgentSession.Scope := ⟨1, 12, some 7⟩
private def wrongRequester : AgentSession.Scope := ⟨1, 10, none⟩
private def foreign : AgentSession.Scope := ⟨2, 10, some 7⟩
private def rootRow : AgentSession.TaskLineageRow := ⟨root, some 42, none⟩
private def childRow : AgentSession.TaskLineageRow := ⟨child, none, some root⟩
private def grandchildRow : AgentSession.TaskLineageRow := ⟨grandchild, none, some child⟩

structure LineageInput where
  name : String
  operation : String
  owner : Nat
  scope : AgentSession.Scope
  rows : List AgentSession.TaskLineageRow
  deriving ToJson

structure LineageOutput where
  task : Option Nat
  deriving ToJson

@[conformance_family]
def originating : Family LineageInput LineageOutput where
  inputs :=
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
      fun (name, scope, rows) => { name, operation := "task_lineage", owner := 1, scope, rows }
  model := fun i => { task := AgentSession.originatingTask i.owner i.scope i.rows [] (i.rows.length + 1) }

structure ScopedInput where
  name : String
  operation : String
  scope : AgentSession.Scope
  rows : List AgentSession.TaskLineageRow
  deriving ToJson

structure ScopedOutput where
  taskNode : Option Nat
  task : Option Nat
  deriving ToJson

@[conformance_family]
def scopedTask : Family ScopedInput ScopedOutput where
  inputs :=
    [ ("same_node", child, [rootRow, childRow]),
      ("cross_node", child, [{ childRow with starter := some foreign }, ⟨foreign, some 43, none⟩]),
      ("unreadable_parent", child, [{ childRow with starter := some foreign }]),
      ("ambiguous_parent", child, [{ childRow with starter := some foreign }, ⟨foreign, some 43, none⟩, ⟨foreign, some 44, none⟩]),
      ("requester_mismatch", child, [rootRow, { childRow with starter := some wrongRequester }]),
      ("cycle", child, [{ childRow with starter := some grandchild }, grandchildRow]),
      ("direct_task_wins", child, [{ childRow with task := some 44 }]) ].map
      fun (name, scope, rows) => { name, operation := "task_lineage_scoped", scope, rows }
  model := fun i =>
    let result := AgentSession.originatingScopedTask i.scope i.rows [] (i.rows.length + 1)
    { taskNode := result.map (·.node), task := result.map (·.task) }

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
