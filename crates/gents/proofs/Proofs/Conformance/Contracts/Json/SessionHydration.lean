import Proofs.SessionHydration
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types

namespace Conformance.Contracts

open Conformance.ContractCases

structure SessionHydrationDecisionCase where
  name : String
  paired : Bool
  activeMember : Bool
  ownsSession : Bool

def hydrationRequest : SessionHydration.Request :=
  { key := "peer-1:session-1"
  , peer := "peer-1"
  , requester := "did:key:requester-1"
  , agent := "did:key:agent-1"
  , session := "session-1" }

def hydrationOwnedDocument : SessionHydration.Document :=
  { collection := "AgentMessage", id := "owned"
  , requester := hydrationRequest.requester
  , agent := hydrationRequest.agent
  , session := hydrationRequest.session }

def hydrationForeignDocument : SessionHydration.Document :=
  { collection := "AgentMessage", id := "foreign"
  , requester := "did:key:requester-2"
  , agent := hydrationRequest.agent
  , session := hydrationRequest.session }

def hydrationWrongCollectionDocument : SessionHydration.Document :=
  { collection := "AgentSession", id := "wrong-collection"
  , requester := hydrationRequest.requester
  , agent := hydrationRequest.agent
  , session := hydrationRequest.session }

def hydrationCatalog (w : SessionHydrationDecisionCase) : SessionHydration.Catalog :=
  { pairedPeers := if w.paired then [hydrationRequest.peer].toFinset else ∅
  , activeMembers := if w.activeMember then [hydrationRequest.requester].toFinset else ∅
  , sessions := if w.ownsSession then [SessionHydration.ownedSession hydrationRequest].toFinset else ∅
  , documents := [hydrationOwnedDocument, hydrationForeignDocument,
      hydrationWrongCollectionDocument].toFinset }

def sessionHydrationDecisionCases : List SessionHydrationDecisionCase :=
  [ { name := "admitted", paired := true, activeMember := true, ownsSession := true }
  , { name := "unpaired", paired := false, activeMember := true, ownsSession := true }
  , { name := "inactive_member", paired := true, activeMember := false, ownsSession := true }
  , { name := "unowned_session", paired := true, activeMember := true, ownsSession := false } ]

def sessionHydrationDecisionCaseJson (w : SessionHydrationDecisionCase) : String :=
  let cat := hydrationCatalog w
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"paired\":" ++ boolString w.paired ++ ","
    ++ "\"active_member\":" ++ boolString w.activeMember ++ ","
    ++ "\"owns_session\":" ++ boolString w.ownsSession ++ ","
    ++ "\"expected_admit\":" ++ boolString (SessionHydration.decideAdmits cat hydrationRequest) ++ ","
    ++ "\"expected_selected_count\":" ++
      toString (SessionHydration.selectedDocuments cat hydrationRequest).card
    ++ "}"

def sessionHydrationDecisionCasesJson : String :=
  jsonArray (sessionHydrationDecisionCases.map sessionHydrationDecisionCaseJson)

structure SessionHydrationProgressCase where
  name : String
  prevPhase : String
  prevMerged : Nat
  prevServed : Option Nat
  merged : Nat
  served : Option Nat
  failed : Bool

def parsePhase (name : String) : SessionHydration.ClientPhase :=
  match name with
  | "requested" => .requested
  | "serving" => .serving
  | "complete" => .complete
  | "failed" => .failed
  | _ => .idle

def progressPrev (w : SessionHydrationProgressCase) : SessionHydration.ClientProgress :=
  { phase := parsePhase w.prevPhase
  , mergedCount := w.prevMerged
  , servedCount := w.prevServed }

def sessionHydrationProgressCases : List SessionHydrationProgressCase :=
  [ { name := "open_requests"
    , prevPhase := "idle", prevMerged := 0, prevServed := none
    , merged := 0, served := none, failed := false }
  , { name := "serving_partial"
    , prevPhase := "requested", prevMerged := 0, prevServed := none
    , merged := 2, served := some 5, failed := false }
  , { name := "complete_when_covered"
    , prevPhase := "serving", prevMerged := 2, prevServed := some 5
    , merged := 5, served := some 5, failed := false }
  , { name := "empty_session_completes"
    , prevPhase := "requested", prevMerged := 0, prevServed := none
    , merged := 0, served := some 0, failed := false }
  , { name := "cannot_complete_early"
    , prevPhase := "serving", prevMerged := 2, prevServed := some 5
    , merged := 4, served := some 5, failed := false }
  , { name := "failed_stays_failed"
    , prevPhase := "serving", prevMerged := 1, prevServed := some 3
    , merged := 3, served := some 3, failed := true }
  ]

def optionNatString : Option Nat → String
  | some n => toString n
  | none => "null"

def phaseString : SessionHydration.ClientPhase → String
  | .idle => "idle"
  | .requested => "requested"
  | .serving => "serving"
  | .complete => "complete"
  | .failed => "failed"

def sessionHydrationProgressCaseJson (w : SessionHydrationProgressCase) : String :=
  let next := SessionHydration.observe (progressPrev w) w.merged w.served w.failed
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"prev_phase\":" ++ jsonString w.prevPhase ++ ","
    ++ "\"prev_merged\":" ++ toString w.prevMerged ++ ","
    ++ "\"prev_served\":" ++ optionNatString w.prevServed ++ ","
    ++ "\"merged\":" ++ toString w.merged ++ ","
    ++ "\"served\":" ++ optionNatString w.served ++ ","
    ++ "\"failed\":" ++ boolString w.failed ++ ","
    ++ "\"expected_phase\":" ++ jsonString (phaseString next.phase) ++ ","
    ++ "\"expected_merged\":" ++ toString next.mergedCount ++ ","
    ++ "\"expected_complete\":" ++
      boolString (decide (next.phase = SessionHydration.ClientPhase.complete))
    ++ "}"

def sessionHydrationProgressCasesJson : String :=
  jsonArray (sessionHydrationProgressCases.map sessionHydrationProgressCaseJson)

end Conformance.Contracts
