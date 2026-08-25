import Proofs.SessionHydration
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types

namespace Conformance.Contracts

open Conformance.ContractCases

structure SessionHydrationDecisionCase where
  name : String
  paired : Bool
  pairingRequesterMatches : Bool
  pairingAgentMatches : Bool
  activeMember : Bool
  membershipNetworkMatches : Bool
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
  { appliedPairingRoutes := if w.paired then
      [{ peer := hydrationRequest.peer
       , requester := if w.pairingRequesterMatches then hydrationRequest.requester else "did:key:requester-2"
       , agent := if w.pairingAgentMatches then hydrationRequest.agent else "did:key:agent-2" }].toFinset
      else ∅
  , selectedNetwork := "network-1"
  , verifiedActiveMemberships := if w.activeMember then
      [{ network := if w.membershipNetworkMatches then "network-1" else "network-2"
       , member := hydrationRequest.requester }].toFinset else ∅
  , sessions := if w.ownsSession then [SessionHydration.ownedSession hydrationRequest].toFinset else ∅
  , documents := [hydrationOwnedDocument, hydrationForeignDocument,
      hydrationWrongCollectionDocument].toFinset }

def sessionHydrationDecisionCases : List SessionHydrationDecisionCase :=
  [ { name := "admitted", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "unpaired", paired := false,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "pairing_wrong_requester", paired := true,
      pairingRequesterMatches := false, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "pairing_wrong_agent", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := false, activeMember := true,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "inactive_member", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := false,
      membershipNetworkMatches := true, ownsSession := true }
  , { name := "foreign_network_member", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := false, ownsSession := true }
  , { name := "unowned_session", paired := true,
      pairingRequesterMatches := true, pairingAgentMatches := true, activeMember := true,
      membershipNetworkMatches := true, ownsSession := false } ]

def sessionHydrationDecisionCaseJson (w : SessionHydrationDecisionCase) : String :=
  let cat := hydrationCatalog w
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"paired\":" ++ boolString w.paired ++ ","
    ++ "\"pairing_requester_matches\":" ++ boolString w.pairingRequesterMatches ++ ","
    ++ "\"pairing_agent_matches\":" ++ boolString w.pairingAgentMatches ++ ","
    ++ "\"active_member\":" ++ boolString w.activeMember ++ ","
    ++ "\"membership_network_matches\":" ++ boolString w.membershipNetworkMatches ++ ","
    ++ "\"owns_session\":" ++ boolString w.ownsSession ++ ","
    ++ "\"expected_admit\":" ++ boolString (SessionHydration.decideAdmits cat hydrationRequest) ++ ","
    ++ "\"expected_selected_count\":" ++
      toString (SessionHydration.selectedDocuments cat hydrationRequest).card
    ++ "}"

def sessionHydrationDecisionCasesJson : String :=
  jsonArray (sessionHydrationDecisionCases.map sessionHydrationDecisionCaseJson)

structure SessionHydrationProgressCase where
  name : String
  prevSession : String
  prevAgent : String
  session : String
  agent : String
  prevPhase : String
  prevMerged : Nat
  prevServed : Option Nat
  merged : Nat
  served : Option Nat
  failed : Bool
  beginRequest : Bool

def parsePhase (name : String) : SessionHydration.ClientPhase :=
  match name with
  | "requested" => .requested
  | "serving" => .serving
  | "complete" => .complete
  | "failed" => .failed
  | _ => .idle

def progressPrev (w : SessionHydrationProgressCase) : SessionHydration.ClientProgress :=
  { session := w.prevSession
  , agent := w.prevAgent
  , phase := parsePhase w.prevPhase
  , mergedCount := w.prevMerged
  , servedCount := w.prevServed }

def sessionHydrationProgressCases : List SessionHydrationProgressCase :=
  [ { name := "open_requests"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := "idle", prevMerged := 0, prevServed := none
    , merged := 0, served := none, failed := false, beginRequest := false }
  , { name := "serving_partial"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := "requested", prevMerged := 0, prevServed := none
    , merged := 2, served := some 5, failed := false, beginRequest := false }
  , { name := "complete_when_covered"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := "serving", prevMerged := 2, prevServed := some 5
    , merged := 5, served := some 5, failed := false, beginRequest := false }
  , { name := "empty_session_completes"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := "requested", prevMerged := 0, prevServed := none
    , merged := 0, served := some 0, failed := false, beginRequest := false }
  , { name := "cannot_complete_early"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := "serving", prevMerged := 2, prevServed := some 5
    , merged := 4, served := some 5, failed := false, beginRequest := false }
  , { name := "failure_is_observed"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := "serving", prevMerged := 1, prevServed := some 3
    , merged := 3, served := some 3, failed := true, beginRequest := false }
  , { name := "failed_stays_failed_without_retry"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := "failed", prevMerged := 1, prevServed := some 3
    , merged := 3, served := some 3, failed := false, beginRequest := false }
  , { name := "retry_resets_failed"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-1", agent := "agent-1"
    , prevPhase := "failed", prevMerged := 3, prevServed := some 3
    , merged := 3, served := some 3, failed := false, beginRequest := true }
  , { name := "switch_session_resets_progress"
    , prevSession := "session-1", prevAgent := "agent-1"
    , session := "session-2", agent := "agent-1"
    , prevPhase := "complete", prevMerged := 5, prevServed := some 5
    , merged := 0, served := none, failed := false, beginRequest := false }
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
  let next := if w.beginRequest then SessionHydration.beginRequest w.session w.agent
    else SessionHydration.observe (progressPrev w) w.merged w.served w.failed w.session w.agent
  "{"
    ++ "\"name\":" ++ jsonString w.name ++ ","
    ++ "\"prev_session\":" ++ jsonString w.prevSession ++ ","
    ++ "\"prev_agent\":" ++ jsonString w.prevAgent ++ ","
    ++ "\"session\":" ++ jsonString w.session ++ ","
    ++ "\"agent\":" ++ jsonString w.agent ++ ","
    ++ "\"prev_phase\":" ++ jsonString w.prevPhase ++ ","
    ++ "\"prev_merged\":" ++ toString w.prevMerged ++ ","
    ++ "\"prev_served\":" ++ optionNatString w.prevServed ++ ","
    ++ "\"merged\":" ++ toString w.merged ++ ","
    ++ "\"served\":" ++ optionNatString w.served ++ ","
    ++ "\"failed\":" ++ boolString w.failed ++ ","
    ++ "\"begin_request\":" ++ boolString w.beginRequest ++ ","
    ++ "\"expected_phase\":" ++ jsonString (phaseString next.phase) ++ ","
    ++ "\"expected_merged\":" ++ toString next.mergedCount ++ ","
    ++ "\"expected_complete\":" ++
      boolString (decide (next.phase = SessionHydration.ClientPhase.complete))
    ++ "}"

def sessionHydrationProgressCasesJson : String :=
  jsonArray (sessionHydrationProgressCases.map sessionHydrationProgressCaseJson)

end Conformance.Contracts
