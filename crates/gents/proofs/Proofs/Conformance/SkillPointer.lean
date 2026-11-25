import Proofs.Tools.SkillPointer
import Proofs.Conformance.Family

namespace Conformance.SkillPointer

open Lean _root_.SkillPointer

deriving instance ToJson for Entry, Kind, Event

structure Case where
  catalog : List Entry
  events : List Event
  deriving ToJson

private def help (request : String) (path : List String) : Event := ⟨.help, request, path⟩
private def failure (request : String) (path : List String) : Event := ⟨.failure, request, path⟩

private def automate : Entry :=
  ⟨"automate", [["config", "automation"], ["config", "event-source"], ["config", "schedule"],
    ["config", "task"], ["config", "trigger"]]⟩
private def controlAccess : Entry :=
  ⟨"control-access", [["config", "policy"], ["schema", "collections", "policy"]]⟩
private def designData : Entry :=
  ⟨"design-data", [["schema", "collection"], ["schema", "collections"], ["schema", "indexes"]]⟩
private def troubleshoot : Entry := ⟨"troubleshoot", [["config", "trigger", "explain"]]⟩

/-- What each help page and failure names, in order, from a fresh memory. -/
@[conformance_family]
def named : Family Case (List (Option String)) where
  inputs :=
    [ { catalog := [automate, controlAccess, designData, troubleshoot]
        events :=
          [ help "r1" ["config", "trigger"]
          , help "r1" ["config", "trigger", "explain"]
          , help "r1" ["schema", "collections", "policy"]
          , help "r1" ["schema", "indexes", "vector"]
          , help "r1" ["config", "agent"]
          , failure "r1" ["config", "trigger", "create"]
          , failure "r1" ["config", "trigger", "create"]
          , failure "r1" ["config", "task", "run"]
          , failure "r1" ["config", "policy", "check"]
          , failure "r1" ["schema", "collection", "get"]
          , help "r1" ["config", "trigger"]
          , failure "r2" ["config", "trigger", "create"] ] }
    , { catalog := []
        events := [help "r1" ["config", "trigger"], failure "r1" ["config", "trigger", "create"]] }
    , { catalog := [⟨"design-page", [["config", "page"]]⟩, ⟨"page-kit", [["config", "page"], []]⟩]
        events :=
          [ help "r1" ["config", "page"]
          , help "r1" ["config", "page", "sdk"]
          , failure "r1" ["config", "page", "lint"]
          , help "r1" ["config", "trigger"] ] }
    , { catalog := [automate]
        events :=
          [ failure "r1" ["config", "trigger", "create"]
          , failure "r2" ["config", "trigger", "create"]
          , failure "r1" ["config", "trigger", "create"]
          , failure "r1" ["config", "trigger", "create"]
          , failure "r2" ["config", "agent", "get"]
          , failure "r1" ["config", "trigger", "create"] ] } ]
  model := fun c => run c.catalog {} c.events

end Conformance.SkillPointer
