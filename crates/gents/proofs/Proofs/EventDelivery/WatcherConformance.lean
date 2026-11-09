import Proofs.EventDelivery.Watcher
import Lean

namespace EventDelivery.WatcherConformance
open Lean EventDelivery.Watcher

structure Case where
  name : String
  activeAgents : List String
  scans : List ScopedScan
  legacyOrder : List String
  arrivalOrder : List String

def cases : List Case :=
  [ ⟨"empty-authorized", ["a"], [⟨"a", .rows []⟩], [], []⟩
  , ⟨"denied-is-not-empty", ["a"], [⟨"a", .denied⟩], [], []⟩
  , ⟨"missing-root-is-not-empty", ["a"], [⟨"a", .missingRoot⟩], [], []⟩
  , ⟨"missing-scan-is-not-empty", ["a"], [], [], []⟩
  , ⟨"denied-does-not-block-healthy", ["a", "b", "c"],
      [⟨"a", .denied⟩, ⟨"b", .rows [⟨"later", "b"⟩]⟩, ⟨"c", .rows [⟨"earlier", "c"⟩]⟩],
      ["later", "earlier"], ["earlier", "later"]⟩
  , ⟨"missing-root-does-not-block-healthy", ["a", "b"],
      [⟨"a", .missingRoot⟩, ⟨"b", .rows [⟨"healthy", "b"⟩]⟩], ["healthy"], ["healthy"]⟩
  , ⟨"different-agents-native-order", ["a", "b"],
      [⟨"a", .rows [⟨"later", "a"⟩]⟩, ⟨"b", .rows [⟨"earlier", "b"⟩]⟩],
      ["later", "earlier"], ["earlier", "later"]⟩
  , ⟨"legacy-before-native", ["a", "b"],
      [⟨"a", .rows [⟨"legacy-late", "a"⟩, ⟨"journal", "a"⟩]⟩,
       ⟨"b", .rows [⟨"legacy-early", "b"⟩]⟩],
      ["legacy-early", "legacy-late", "journal"], ["journal"]⟩
  , ⟨"duplicates-are-physical", ["a", "a"],
      [⟨"a", .rows [⟨"same", "a"⟩, ⟨"same", "a"⟩]⟩], ["same"], ["same"]⟩
  , ⟨"foreign-selected-agent-is-not-dispatched", ["a"],
      [⟨"a", .rows [⟨"foreign", "b"⟩]⟩], ["foreign"], ["foreign"]⟩
  , ⟨"unresolved-agent-is-not-scanned", [], [⟨"a", .rows [⟨"unresolved", "a"⟩]⟩], [], ["unresolved"]⟩
  , ⟨"reconfigured-agent-is-not-scanned", ["b"],
      [⟨"a", .rows [⟨"old", "a"⟩]⟩, ⟨"b", .rows [⟨"new", "b"⟩]⟩],
      ["old", "new"], ["old", "new"]⟩ ]

def actorCases : List (String × List ActorCandidate) :=
  [ ("disabled-resolved", [⟨"a", true, true, false⟩])
  , ("disabled-unresolved", [⟨"a", true, false, false⟩])
  , ("mixed", [⟨"a", true, true, true⟩, ⟨"b", true, true, false⟩,
      ⟨"c", true, false, false⟩, ⟨"d", false, true, false⟩]) ]

def actorCaseJson (c : String × List ActorCandidate) : Json := Json.mkObj
  [("name", toJson c.1), ("candidates", toJson (c.2.map fun candidate => Json.mkObj
    [("did", toJson candidate.did), ("configured", toJson candidate.configured),
     ("identity_resolved", toJson candidate.identityResolved), ("available", toJson candidate.available)])),
   ("expected", toJson (scanActors c.2))]

def actorCasesJson : String := (toJson (actorCases.map actorCaseJson)).compress

def rowJson (row : ScopedRow) : Json := Json.mkObj
  [("doc_id", toJson row.docId), ("selected_agent", toJson row.selectedAgent)]

def scanJson (scan : ScopedScan) : Json :=
  let (status, rows) := match scan.view with
    | .denied => ("denied", [])
    | .missingRoot => ("missing_root", [])
    | .rows rows => ("ok", rows)
  Json.mkObj [("actor", toJson scan.actor), ("status", toJson status),
    ("rows", toJson (rows.map rowJson))]

def resultJson (batch : ScopedBatch) : Json := Json.mkObj
  [("rows", toJson (batch.rows.map rowJson)),
   ("failures", toJson (batch.failures.map fun failure =>
      Json.mkObj [("actor", toJson failure.actor), ("reason", toJson failure.reason)])),
   ("needs_rescan", toJson (scopedNeedsRescan batch))]

def caseJson (c : Case) : Json := Json.mkObj
  [("name", toJson c.name), ("active_agents", toJson c.activeAgents),
   ("scans", toJson (c.scans.map scanJson)), ("legacy_order", toJson c.legacyOrder),
   ("arrival_order", toJson c.arrivalOrder),
   ("expected", resultJson (scopedCandidates c.activeAgents c.scans c.legacyOrder c.arrivalOrder))]

def casesJson : String := (toJson (cases.map caseJson)).compress

end EventDelivery.WatcherConformance
