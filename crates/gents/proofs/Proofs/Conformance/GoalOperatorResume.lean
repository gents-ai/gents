import Proofs.GoalAutomation.OperatorResume
import Proofs.Conformance.ContractTypes
namespace Conformance.GoalOperatorResumeContracts
open GoalAutomation.OperatorResume Conformance.Contracts

def parentBinding : Binding :=
  ⟨1, "owner", "session", 10, 100, some "graph-correlation", some "source",
    some "context", some "workspace-authority", "full-semantic-fingerprint", 20, 1⟩
def initial : Snapshot :=
  ⟨⟨.paused, 2, false, false⟩, 0, none, 10, [], 37, some 1000⟩
def request : Request := ⟨.paused, 0, true, true, true, true, parentBinding⟩
/-- Explicit durable expectation, never computed from resume. -/
def published : Snapshot :=
  ⟨⟨.active, 0, false, false⟩, 1, some 10, 20, [parentBinding], 37, some 1000⟩
def later : Snapshot :=
  { published with goal := { published.goal with status := .paused }, latestRequest := 30 }

structure ResumeCase where
  name : String
  before : Snapshot
  request : Request
  commit : Bool
  expected : Snapshot
  outcome : Outcome
  deriving DecidableEq, Repr

def resumeCases : List ResumeCase :=
  [ ⟨"atomic_publication", initial, request, true, published, .created⟩
  , ⟨"staging_failure_rolls_back", initial, request, false, initial, .rolledBack⟩
  , ⟨"lost_ack_returns_same_child", published, request, true, published, .recovered⟩
  , ⟨"retry_after_later_progress", later, request, true, later, .recovered⟩
  , ⟨"unauthorized_cannot_publish", initial, {request with authorized := false}, true, initial, .denied⟩
  , ⟨"foreign_parent_cannot_recover", published, {request with parentBelongsToGoal := false}, true, published, .denied⟩
  , ⟨"non_latest_parent_cannot_publish", {initial with latestRequest := 30}, request, true,
       {initial with latestRequest := 30}, .illegal⟩
  , ⟨"nonterminal_parent_cannot_publish", initial, {request with terminalParent := false}, true, initial, .illegal⟩
  , ⟨"busy_session_cannot_publish", initial, {request with sessionIdle := false}, true, initial, .illegal⟩
  , ⟨"foreign_fingerprint_conflicts",
       {published with children := [{parentBinding with semanticFingerprint := "foreign"}]},
       request, true,
       {published with children := [{parentBinding with semanticFingerprint := "foreign"}]}, .conflict⟩
  , ⟨"budget_limited_cannot_resume", {initial with goal := {initial.goal with status := .budgetLimited}},
       {request with expectedStatus := .budgetLimited}, true,
       {initial with goal := {initial.goal with status := .budgetLimited}}, .illegal⟩
  ]

theorem cases_replay_explicit_expectations :
  ∀ c ∈ resumeCases, resume c.before c.request c.commit = (c.expected, c.outcome) := by decide

theorem published_retry_is_noop : resume published request true = (published, .recovered) := by decide

theorem stale_pause_after_resume_is_noop : controllerWrite published .active 0 .pause = published := by decide

def configCases : List (Goals.Status × Goals.Status × Bool) :=
  [(.active, .active, true), (.paused, .active, false), (.blocked, .active, false),
   (.usageLimited, .active, false), (.budgetLimited, .active, false), (.complete, .active, false),
   (.paused, .paused, true)]
theorem configCases_match :
  ∀ c ∈ configCases, configMaySetStatus c.1 c.2.1 = c.2.2 := by decide

private def b (x : Bool) : String := if x then "true" else "false"
private def n : Option Nat → String | none => "null" | some x => toString x
private def str : Option String → String | none => "null" | some x => jsonString x
private def status (x : Goals.Status) : String := jsonString x.toDefraDB
private def outcome : Outcome → String
 | .denied => "denied" | .stale => "stale" | .illegal => "illegal" | .conflict => "conflict"
 | .rolledBack => "rolled_back" | .created => "created" | .recovered => "recovered"
def bindingJson (x : Binding) : String :=
  "{\"goal\":" ++ toString x.goal ++ ",\"owner\":" ++ jsonString x.owner ++
  ",\"session\":" ++ jsonString x.session ++ ",\"predecessor\":" ++ toString x.predecessor ++
  ",\"predecessor_doc\":" ++ toString x.predecessorDoc ++ ",\"correlation\":" ++ str x.correlation ++
  ",\"source_document\":" ++ str x.sourceDocument ++ ",\"trigger_context\":" ++ str x.triggerContext ++
  ",\"workspace_fingerprint\":" ++ str x.workspaceFingerprint ++
  ",\"semantic_fingerprint\":" ++ jsonString x.semanticFingerprint ++
  ",\"child\":" ++ toString x.child ++ ",\"sequence\":" ++ toString x.sequence ++ "}"
def snapshotJson (x : Snapshot) : String :=
  "{\"status\":" ++ status x.goal.status ++ ",\"blocked_audits\":" ++ toString x.goal.blockedAudits ++
  ",\"wrapup_requested\":" ++ b x.goal.wrapupRequested ++ ",\"wrapup_completed\":" ++ b x.goal.wrapupCompleted ++
  ",\"sequence\":" ++ toString x.sequence ++ ",\"last_continued_from\":" ++ n x.lastContinuedFrom ++
  ",\"latest_request\":" ++ toString x.latestRequest ++ ",\"children\":" ++ jsonArray (x.children.map bindingJson) ++
  ",\"tokens_used\":" ++ toString x.tokensUsed ++ ",\"token_budget\":" ++ n x.tokenBudget ++ "}"
def requestJson (x : Request) : String :=
  "{\"expected_status\":" ++ status x.expectedStatus ++ ",\"expected_sequence\":" ++ toString x.expectedSequence ++
  ",\"authorized\":" ++ b x.authorized ++ ",\"parent_belongs_to_goal\":" ++ b x.parentBelongsToGoal ++
  ",\"terminal_parent\":" ++ b x.terminalParent ++ ",\"session_idle\":" ++ b x.sessionIdle ++
  ",\"binding\":" ++ bindingJson x.binding ++ "}"
def caseJson (c : ResumeCase) : String :=
  "{\"name\":" ++ jsonString c.name ++ ",\"before\":" ++ snapshotJson c.before ++
  ",\"request\":" ++ requestJson c.request ++ ",\"commit\":" ++ b c.commit ++
  ",\"expected\":" ++ snapshotJson c.expected ++ ",\"outcome\":" ++ jsonString (outcome c.outcome) ++ "}"
def resumeCasesJson : String := jsonArray (resumeCases.map caseJson)
def configCasesJson : String := jsonArray (configCases.map fun c =>
  "{\"current\":" ++ status c.1 ++ ",\"target\":" ++ status c.2.1 ++ ",\"allowed\":" ++ b c.2.2 ++ "}")
end Conformance.GoalOperatorResumeContracts
