import Proofs.ToolCalls.Background.Observation
import Proofs.Conformance.Family

namespace Background.Observation
deriving instance Lean.ToJson for Change
deriving instance Lean.ToJson for Wake
end Background.Observation

namespace Conformance.BackgroundWaitChange

open Lean Background.Observation

structure ChangeWire where
  collection : String
  docId : String
  deriving ToJson

structure Poll where
  toolCollection : String
  toolDocId : String
  requestCollection : String
  callerDocId : Option String
  feedClosed : Bool
  resyncRequired : Bool
  changes : List ChangeWire
  feedOpen : Bool
  deriving ToJson

structure Observed where
  change : Change
  wake : Wake
  feedOpen : Bool
  deriving ToJson

private def scopeOf (p : Poll) : Scope :=
  { toolCollection := p.toolCollection, toolDocId := p.toolDocId
  , requestCollection := p.requestCollection, callerDocId := p.callerDocId }

private def batchOf (p : Poll) : Option Batch :=
  if p.feedClosed then none
  else some { changes := p.changes.map fun c => ⟨c.collection, c.docId⟩
            , resyncRequired := p.resyncRequired }

private def candidates : List ChangeWire :=
  [ ⟨"tools", "tool"⟩, ⟨"tools", "other-tool"⟩, ⟨"tools", ""⟩
  , ⟨"requests", "caller"⟩, ⟨"requests", "other-caller"⟩, ⟨"requests", ""⟩
  , ⟨"unknown", "unrelated"⟩ ]

private def batches : List (List ChangeWire) :=
  [[], [⟨"tools", "unrelated-physical-document"⟩, ⟨"requests", "unrelated-physical-document"⟩]]
    ++ candidates.map (fun c => [c])
    ++ [[⟨"tools", "other-tool"⟩, ⟨"requests", "caller"⟩]]

private def poll (caller : Option String) (closed resync feedOpen : Bool)
    (changes : List ChangeWire) : Poll :=
  { toolCollection := "tools", toolDocId := "tool", requestCollection := "requests"
  , callerDocId := caller, feedClosed := closed, resyncRequired := resync, changes, feedOpen }

private def polls : List Poll :=
  [some "caller", none].flatMap fun caller =>
    [false, true].flatMap fun resync =>
      [true, false].flatMap fun feedOpen =>
        batches.map (poll caller false resync feedOpen)
  ++ [true, false].map fun feedOpen => poll (some "caller") true false feedOpen []

@[conformance_family]
def classification : Conformance.Family Poll Observed where
  inputs := polls
  model := fun p =>
    let change := classify (scopeOf p) (batchOf p)
    let state : State := { rescanAt := 1, feedOpen := p.feedOpen }
    { change
    , wake := wake state { now := 0, cancelled := false, deadlineAt := 1, change }
    , feedOpen := (observeChange state change).feedOpen }

end Conformance.BackgroundWaitChange
