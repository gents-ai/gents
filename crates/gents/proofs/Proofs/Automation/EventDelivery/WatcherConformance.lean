import Proofs.Automation.EventDelivery.Watcher
import Lean

namespace EventDelivery.WatcherConformance
open Lean EventDelivery.Watcher

structure Case where
  name : String
  activeAgents : List String
  disabledAgents : List String
  scans : List ScopedScan
  legacyOrder : List String
  arrivalOrder : List String

def cases : List Case :=
  [ ⟨"empty-authorized", ["a"], [], [⟨"a", .rows []⟩], [], []⟩
  , ⟨"denied-is-not-empty", ["a"], [], [⟨"a", .denied⟩], [], []⟩
  , ⟨"missing-root-is-not-empty", ["a"], [], [⟨"a", .missingRoot⟩], [], []⟩
  , ⟨"missing-scan-is-not-empty", ["a"], [], [], [], []⟩
  , ⟨"denied-does-not-block-healthy", ["a", "b", "c"], [],
      [⟨"a", .denied⟩, ⟨"b", .rows [⟨"later", "b"⟩]⟩, ⟨"c", .rows [⟨"earlier", "c"⟩]⟩],
      ["later", "earlier"], ["earlier", "later"]⟩
  , ⟨"missing-root-does-not-block-healthy", ["a", "b"], [],
      [⟨"a", .missingRoot⟩, ⟨"b", .rows [⟨"healthy", "b"⟩]⟩], ["healthy"], ["healthy"]⟩
  , ⟨"different-agents-native-order", ["a", "b"], [],
      [⟨"a", .rows [⟨"later", "a"⟩]⟩, ⟨"b", .rows [⟨"earlier", "b"⟩]⟩],
      ["later", "earlier"], ["earlier", "later"]⟩
  , ⟨"legacy-before-native", ["a", "b"], [],
      [⟨"a", .rows [⟨"legacy-late", "a"⟩, ⟨"journal", "a"⟩]⟩,
       ⟨"b", .rows [⟨"legacy-early", "b"⟩]⟩],
      ["legacy-early", "legacy-late", "journal"], ["journal"]⟩
  , ⟨"duplicates-are-physical", ["a", "a"], [],
      [⟨"a", .rows [⟨"same", "a"⟩, ⟨"same", "a"⟩]⟩], ["same"], ["same"]⟩
  , ⟨"foreign-selected-agent-is-not-dispatched", ["a"], [],
      [⟨"a", .rows [⟨"foreign", "b"⟩]⟩], ["foreign"], ["foreign"]⟩
  , ⟨"unresolved-agent-is-not-scanned", [], [], [⟨"a", .rows [⟨"unresolved", "a"⟩]⟩], [], ["unresolved"]⟩
  , ⟨"reconfigured-agent-is-not-scanned", ["b"], [],
      [⟨"a", .rows [⟨"old", "a"⟩]⟩, ⟨"b", .rows [⟨"new", "b"⟩]⟩],
      ["old", "new"], ["old", "new"]⟩
  , ⟨"disabled-denied-is-not-a-failure", ["a"], ["a"], [⟨"a", .denied⟩], [], []⟩
  , ⟨"disabled-unscanned-is-not-a-failure", ["a"], ["a"], [], [], []⟩
  , ⟨"disabled-beside-healthy", ["a", "b"], ["a"],
      [⟨"a", .denied⟩, ⟨"b", .rows [⟨"healthy", "b"⟩]⟩], ["healthy"], ["healthy"]⟩
  , ⟨"disabled-rows-are-not-dispatched", ["a", "b"], ["a"],
      [⟨"a", .rows [⟨"held", "a"⟩]⟩, ⟨"b", .rows [⟨"healthy", "b"⟩]⟩],
      ["held", "healthy"], ["held", "healthy"]⟩
  , ⟨"failure-beside-disabled-still-rescans", ["a", "b", "c"], ["a"],
      [⟨"a", .denied⟩, ⟨"b", .missingRoot⟩, ⟨"c", .rows [⟨"healthy", "c"⟩]⟩],
      ["healthy"], ["healthy"]⟩
  , ⟨"reenabled-is-scanned-again", ["a", "b"], [],
      [⟨"a", .rows [⟨"held", "a"⟩]⟩, ⟨"b", .rows [⟨"healthy", "b"⟩]⟩],
      ["held", "healthy"], ["held", "healthy"]⟩
  , ⟨"disabled-inactive-agent-is-ignored", ["b"], ["a"],
      [⟨"b", .rows [⟨"healthy", "b"⟩]⟩], ["healthy"], ["healthy"]⟩ ]

def actorCases : List (String × List ActorCandidate) :=
  [ ("disabled-resolved", [⟨"a", true, true, false, true⟩])
  , ("disabled-unresolved", [⟨"a", true, false, false, true⟩])
  , ("mixed", [⟨"a", true, true, true, false⟩, ⟨"b", true, true, false, true⟩,
      ⟨"c", true, false, false, true⟩, ⟨"d", false, true, false, false⟩,
      ⟨"e", true, true, false, false⟩]) ]

end EventDelivery.WatcherConformance
