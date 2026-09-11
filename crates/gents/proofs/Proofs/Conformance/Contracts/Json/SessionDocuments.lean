import Proofs.Conformance.ContractCases.SessionDocuments
import Proofs.Conformance.ContractCases.Types
import Lean

namespace Conformance.Contracts
open Lean Conformance.SessionDocuments

def scopeJson (s : AgentSession.Scope) : Json := Json.mkObj
  [("agent", toJson s.agent), ("session", toJson s.session), ("requester", toJson s.requester)]
def requestJson (r : AgentSession.RequestFact) : Json := Json.mkObj
  [("scope", scopeJson r.scope), ("behavior", toJson r.behavior),
   ("created_at", toJson r.createdAt), ("doc_id", toJson r.observed.docId),
   ("request_id", toJson r.observed.requestId), ("state", toJson r.observed.state.toDefraDB)]
private def provenanceJson (p : AgentSession.Provenance) : Json := Json.mkObj
  [("task_id", toJson p.task), ("graph_run_id", toJson p.graphRun),
   ("parent_request_doc_id", toJson p.parentRequestDoc),
   ("fork", p.fork.map (fun f => Json.mkObj
     [("source_session_id", toJson f.sourceSession), ("at_user_turn", toJson f.atUserTurn)])
     |>.getD Json.null)]
def documentJson (d : AgentSession.Document) : Json := Json.mkObj
  [("scope", scopeJson d.scope), ("behavior", toJson d.behavior),
   ("created_at", toJson d.createdAt), ("closed_at", toJson d.closedAt),
   ("provenance", d.provenance.map provenanceJson |>.getD Json.null),
   ("tags", toJson d.tags), ("title", d.title.map (fun t => Json.mkObj
      [("text", toJson t.text), ("source", toJson t.source.toWireName)]) |>.getD Json.null),
   ("observation", d.observation.map (fun o => Json.mkObj
      [("activity", toJson o.activity), ("preview", toJson o.preview),
       ("latest", o.latest.map (fun r => Json.mkObj
         [("doc_id", toJson r.docId), ("request_id", toJson r.requestId),
          ("state", toJson r.state.toDefraDB)]) |>.getD Json.null)]) |>.getD Json.null)]
private def advanceJson (name : String) (r : AgentSession.RequestFact)
    (before : AgentSession.Document := indexed)
    (rows : List AgentSession.RequestFact := [old, newerRequest]) : Json := Json.mkObj
  [("name", toJson name), ("operation", toJson "advance"), ("before", documentJson before),
   ("rows", toJson (rows.map requestJson)), ("request", requestJson r),
   ("normalized_preview", toJson "candidate"), ("now", toJson (4 : Nat)),
   ("after", documentJson (AgentSession.advance before rows r "candidate" 4))]
private def refreshJson (name : String) (event : AgentSession.RequestFact)
    (rows : List AgentSession.RequestFact := [old, event]) : Json := Json.mkObj
  [("name", toJson name), ("operation", toJson "refresh"), ("before", documentJson indexed),
   ("event", requestJson event), ("rows", toJson (rows.map requestJson)), ("now", toJson (4 : Nat)),
   ("after", documentJson (AgentSession.refresh indexed rows event.observed 4))]
private def renameBefore : AgentSession.Document :=
  { indexed with title := some ⟨"task title", .task⟩ }
private def renameJson : Json := Json.mkObj
  [("name", toJson "user_rename_preserves_latest"), ("operation", toJson "rename"),
   ("before", documentJson renameBefore), ("now", toJson (1 : Nat)),
   ("title", Json.mkObj [("text", toJson "renamed"), ("source", toJson "user")]),
   ("after", documentJson (AgentSession.rename renameBefore (some ⟨"renamed", .user⟩) 1))]
private def clearTitleJson : Json := Json.mkObj
  [("name", toJson "user_clear_preserves_latest"), ("operation", toJson "rename"),
   ("before", documentJson renameBefore), ("now", toJson (4 : Nat)),
   ("title", Json.null),
   ("after", documentJson (AgentSession.rename renameBefore none 4))]
private def retryStateJson (s : SessionState) : Json := Json.mkObj
  [("session_id", toJson s.sessionId), ("behavior", toJson s.behaviorId),
   ("latest", toJson s.latest), ("requests", toJson ([1, 3].filter
     (fun id => decide (id ∈ s.requestIds)) |>.map (fun id => Json.mkObj
       [("request_id", toJson id), ("state", toJson (s.ctx id).state.toDefraDB),
        ("origin", toJson (s.ctx id).origin.toDefraDB),
        ("admission", toJson (Conformance.ContractCases.admissionName (s.ctx id).admission)),
        ("deadline", toJson (s.ctx id).deadline), ("current_time", toJson (s.ctx id).currentTime),
        ("retry_count", toJson (s.ctx id).retryCount), ("max_retries", toJson (s.ctx id).maxRetries)])))]
private def retryJson (name : String) (rows : List AgentSession.RequestFact) (parentDoc : Nat) : Json :=
  Json.mkObj [("name", toJson name), ("session", documentJson indexed),
    ("before", retryStateJson retryState), ("rows", toJson (rows.map requestJson)),
    ("parent_doc", toJson parentDoc), ("failed_id", toJson (1 : Nat)), ("new_id", toJson (3 : Nat)),
    ("after", (SessionState.retryFromRows? retryState indexed rows parentDoc 1 3).map
      retryStateJson |>.getD Json.null)]
private def rowJson (r : SessionFork.Row) : Json := Json.mkObj
  [("doc_id", toJson r.docId), ("scope", scopeJson r.scope), ("sequence", toJson r.sequence),
   ("request_id", toJson r.requestId), ("request_doc_id", toJson r.requestDocId),
   ("spill_refs", toJson r.spillRefs)]
private def historyJson (h : SessionFork.History) : Json := Json.mkObj
  [("messages", toJson (h.messages.map rowJson)), ("calls", toJson (h.calls.map rowJson)),
   ("spills", toJson (h.spills.map fun s => Json.mkObj
     [("row", rowJson s.row), ("call_doc_id", toJson s.callDocId)])),
   ("compactions", toJson (h.compactions.map fun c => Json.mkObj
     [("row", rowJson c.row), ("through_sequence", toJson c.throughSequence),
      ("key_session", toJson c.keySession)]))]
private def selectionJson (name : String) (rows : List AgentSession.RequestFact)
    (agent session : Nat) (requester : Option (Option Nat)) : Json := Json.mkObj
  [("name", toJson name), ("requests", toJson (rows.map requestJson)),
   ("agent", toJson agent), ("session", toJson session),
   ("requester_scoped", toJson requester.isSome),
   ("requester", toJson (requester.getD none)),
   ("selected", (AgentSession.latest rows agent session requester).map requestJson |>.getD Json.null)]
private def forkJson (name : String) (source : SessionFork.History)
    (target : AgentSession.Scope) (cut : Nat) (authorized idle coherent : Bool) : Json := Json.mkObj
  [("name", toJson name), ("source", historyJson source), ("parent", scopeJson scope),
   ("child", scopeJson target), ("exclusive_sequence_cut", toJson cut),
   ("document_id_offset", toJson (1000 : Nat)), ("authorized", toJson authorized),
   ("idle", toJson idle), ("coherent", toJson coherent),
   ("published", (SessionFork.publish source scope target remap cut authorized idle coherent).map
      historyJson |>.getD Json.null)]

/-- Full copy inputs and computed durable outputs, not a names-and-PASS manifest.
The exclusive sequence cut is resolved from the user-turn API by the adapter. -/
def sessionDocumentsJson : String := (Json.mkObj
  [("document_reference_encoding", toJson "interned collection plus _docID, not raw _docID"),
   ("retry", toJson [
      retryJson "exact_parent" [old] 101,
      retryJson "newer_authoritative_row" [old, newerRequest] 101,
      retryJson "wrong_physical_parent" [old] 999,
      retryJson "existing_candidate_missing_from_auxiliary_projection" [old, olderExistingCandidate] 101]),
   ("projection", toJson [renameJson, clearTitleJson,
      advanceJson "stale_admission" old,
      advanceJson "current_admission" newerRequest,
      advanceJson "foreign_requester_does_not_freeze_own_index" scopedSuccessor indexed
        [old, newerRequest, scopedSuccessor, foreignRequester],
      advanceJson "present_requester_does_not_freeze_absent_scope" absentScopeRequest absentScopeSession
        [absentScopeRequest, foreignRequester],
      refreshJson "stale_completion" old,
      refreshJson "wrong_physical_completion"
        { newerRequest with observed := { newerRequest.observed with docId := 999 } },
      refreshJson "current_completion" completedRequest,
      refreshJson "stale_event_uses_current_completed_row" processingEvent [completedRequest],
      refreshJson "missing_current_row_noop" newerRequest [],
      refreshJson "foreign_scope_current_row_noop" newerRequest
        [{ completedRequest with scope := { completedRequest.scope with requester := some 99 } }],
      refreshJson "wrong_behavior_current_row_noop" newerRequest [{ completedRequest with behavior := 99 }]]),
   ("selection", toJson [
      selectionJson "timestamp_tie_forward" [old, newerRequest] 1 10 none,
      selectionJson "timestamp_tie_reverse" [newerRequest, old] 1 10 none,
      selectionJson "absent_requester_exact" [old] 1 10 (some none),
      selectionJson "foreign_agent" [old] 9 10 none,
      selectionJson "foreign_session" [old] 1 9 none]),
   ("fork", toJson [
      forkJson "exact_copy" history child 1 true true true,
      forkJson "empty_prefix" history child 0 true true true,
      forkJson "duplicate_message_sequence" duplicateSequenceHistory child 1 true true true,
      forkJson "call_without_source_message" orphanCallHistory child 1 true true true,
      forkJson "wrong_requester" history { child with requester := none } 1 true true true,
      forkJson "busy_source" history child 1 true false true,
      forkJson "mixed_generation" history child 1 true true false,
      forkJson "unauthorized" history child 1 false true true,
      forkJson "cursor_beyond_source"
        { history with compactions := [⟨sourceRow 4 1, 99, 10⟩] } child 100 true true true,
      forkJson "invalid_compaction_sequence"
        { history with compactions := [⟨sourceRow 4 2, 0, 10⟩] } child 1 true true true,
      forkJson "cut_breaks_spill_reference"
        splitLinkHistory child 1 true true true])]).compress
end Conformance.Contracts
