import Proofs.Identity.Household
import Proofs.Identity.ClientAgentCreation
import Proofs.ToolPolicy.PluginData

namespace Identity.ClientAgentJournal
open Lean

/-- Pending client publication records are private to their native creator.
Creating an unregistered legacy record cannot establish ownership, and a
client must sign its own journal writes rather than delegating to the runtime. -/
def privatePolicy : List Household.ResourceDefinition :=
  [{ name := "journals", relations := [],
     permissions := [Household.permission "read" [] [],
       Household.permission "update" [] [], Household.permission "delete" [] []] }]

def privateFixture : Household.State :=
  { objects := [⟨"journals", "record", "user"⟩], relations := [], enrolled := [] }

/-- DefraDB signs embedded writes with its configured local signer. A client
journal's explicit actor must be that signer to preserve native provenance. -/
def localSignerMatches (client signer : String) : Bool :=
  !client.isEmpty && client == signer

theorem mismatched_local_signer_refused (client signer : String)
    (different : (client == signer) = false) : localSignerMatches client signer = false := by
  simp [localSignerMatches, different]

theorem journal_only_owner_can_read_and_write :
    [Household.allowedIn privatePolicy 5 privateFixture "user" "journals" "record" "read",
     Household.allowedIn privatePolicy 5 privateFixture "user" "journals" "record" "update",
     Household.allowedIn privatePolicy 5 privateFixture "user" "journals" "record" "delete",
     Household.allowedIn privatePolicy 5 privateFixture "other" "journals" "record" "read",
     Household.allowedIn privatePolicy 5 privateFixture "other" "journals" "record" "update",
     Household.allowedIn privatePolicy 5 privateFixture "other" "journals" "record" "delete"] =
    [true, true, true, false, false, false] := by native_decide

/-- Roles identify independently authenticated actors supplied by the native
conformance adapter. Anonymous access has no identity and is denied. -/
def privateAccessCases : List (String × String × Bool) :=
  [("owner", "user"), ("other", "other"), ("runtime", "runtime"),
   ("agent", "agent"), ("anonymous", "")].flatMap fun (role, actor) =>
    ["read", "update", "delete"].map fun action =>
      (role, action, Household.allowedIn privatePolicy 5 privateFixture actor "journals" "record" action)

def privatePolicyCasesJson : String :=
  (Json.mkObj [("policy_yaml", toJson (Household.policyYamlOf privatePolicy)),
    ("checks", toJson (privateAccessCases.map fun (role, action, expected) =>
      Json.mkObj [("role", toJson role), ("action", toJson action), ("expected", toJson expected)])),
    ("signer_checks", toJson (["client", "other", "empty"].map fun role =>
      let actor := if role == "client" then "user" else if role == "other" then "other" else ""
      Json.mkObj [("role", toJson role), ("expected", toJson (localSignerMatches actor "user"))]))]).compress

/-- Client-only publication records are not pack or agent datastore data. -/
def protectedCollection : String := "PendingChatSubmission"

theorem protected_collection_plugin_query_refused (limit : Nat) (own granted : Bool) :
    ToolPolicy.PluginData.queryAdmitted limit own true granted = false := by
  simp [ToolPolicy.PluginData.queryAdmitted]

theorem protected_collection_plugin_write_refused
    (op : ToolPolicy.ApplicationWrite.Operation) (observation : ToolPolicy.PluginData.Observation)
    (isProtected : observation.runtimeOrProtected = true) :
    ToolPolicy.PluginData.admitted op observation = false := by
  simp [ToolPolicy.PluginData.admitted, isProtected]

inductive Stage where
  | commit | parent | executor
  deriving BEq, DecidableEq, Repr

inductive Status where
  | fresh | pending | succeeded | failed
  deriving BEq, DecidableEq, Repr

/-- A journal identifies the retained client intent, not an authorization
grant. The client and physical Agent/Node identities are checked by the native
owners. The permit is local to the process and is lost on restart. A pending
record is ambiguous after that loss even if dispatch never happened. -/
structure Journal where
  client : String
  agentDid : String
  nodeDid : String
  agentDoc : String
  nodeDoc : String
  commit : Status := .fresh
  parent : Status := .fresh
  executor : Status := .fresh
  permit : Option Stage := none
  deriving BEq, DecidableEq, Repr

def status (j : Journal) : Stage → Status
  | .commit => j.commit
  | .parent => j.parent
  | .executor => j.executor

def setStatus (j : Journal) (stage : Stage) (next : Status) : Journal :=
  match stage with
  | .commit => { j with commit := next }
  | .parent => { j with parent := next }
  | .executor => { j with executor := next }

def retryable (s : Status) : Bool := s == .fresh || s == .failed

/-- `failed` means native evidence confirms no effect; an ambiguous error is
pending. The marker reservation must be durable before a dispatch permit is
returned. Failed or ambiguous persistence supplies no permit. -/
def mayRecord (j : Journal) (stage : Stage) : Bool :=
  !j.client.isEmpty && !j.agentDid.isEmpty && !j.nodeDid.isEmpty &&
    !j.agentDoc.isEmpty && !j.nodeDoc.isEmpty && j.permit.isNone &&
    retryable (status j stage) && (match stage with
      | .commit => j.parent == .fresh && j.executor == .fresh
      | .parent => j.commit == .succeeded
      | .executor => j.commit == .succeeded && j.parent == .succeeded)

def recordAttempt (j : Journal) (stage : Stage) (durable : Bool) : Journal :=
  if durable && mayRecord j stage then
    { (setStatus j stage .pending) with permit := some stage }
  else j

def mayDispatch (j : Journal) (stage : Stage) : Bool :=
  j.permit == some stage && status j stage == .pending

def dispatch (j : Journal) (stage : Stage) : Journal :=
  if mayDispatch j stage then { j with permit := none } else j

/-- The response is an authenticated observation of this exact attempt.
Unknown responses stay pending; a successful prefix is never reissued. -/
def finishAttempt (j : Journal) (stage : Stage) (success confirmedFailure : Bool) : Journal :=
  if j.permit.isNone && status j stage == .pending then
    if success then setStatus j stage .succeeded
    else if confirmedFailure then setStatus j stage .failed
    else j
  else j

def restart (j : Journal) : Journal := { j with permit := none }

/-- A save of another document has no authority over this journal. -/
def saveUnrelated (j : Journal) : Journal := j

inductive RecoveryDisposition where
  | proceed | bindingMismatch | commitUnknown | explicitRepair
  deriving BEq, DecidableEq, Repr

/-- A retained, committed publication with any unfinished relationship is
not automatically resumed, even if the missing operation was never dispatched
or its failure is known. Reconstructing authority for it requires explicit
repair; this decision neither implements repair nor grants ACP authority. -/
def recoveryDisposition (actorMatches recordPresent bindingMatches : Bool)
    (j : Journal) : RecoveryDisposition :=
  if !actorMatches then .bindingMismatch
  else if !recordPresent then .proceed
  else if !bindingMatches then .bindingMismatch
  else if j.commit == .failed ||
      (j.commit == .succeeded && j.parent == .succeeded && j.executor == .succeeded) then .proceed
  else if j.commit != .succeeded then .commitUnknown
  else .explicitRepair

theorem interrupted_fresh_suffix_requires_repair :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded }
    recoveryDisposition true true true (restart j) = .explicitRepair := by decide

theorem interrupted_failed_suffix_requires_repair :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded, executor := .failed }
    recoveryDisposition true true true (restart j) = .explicitRepair := by decide

theorem completed_publication_needs_no_repair :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded, executor := .succeeded }
    recoveryDisposition true true true (restart j) = .proceed := by decide

theorem record_preserves_binding (j : Journal) (stage : Stage) (durable : Bool) :
    let next := recordAttempt j stage durable
    next.client = j.client ∧ next.agentDid = j.agentDid ∧ next.nodeDid = j.nodeDid ∧
      next.agentDoc = j.agentDoc ∧ next.nodeDoc = j.nodeDoc := by
  unfold recordAttempt
  split
  · cases stage <;> simp [setStatus]
  · simp

theorem recorded_commit_cannot_replay_after_restart :
    let initial : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc" }
    let lost := restart (recordAttempt initial .commit true)
    mayRecord lost .commit = false ∧ mayDispatch lost .commit = false := by decide

theorem completed_parent_is_not_regranted :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded, executor := .failed }
    mayRecord j .parent = false ∧ mayRecord j .executor = true := by decide

theorem missing_durable_reservation_has_no_permit (j : Journal) (stage : Stage) :
    recordAttempt j stage false = j := by rfl

theorem unrelated_save_preserves_journal (j : Journal) : saveUnrelated j = j := by rfl

inductive Action where
  | record (stage : Stage) (durable : Bool)
  | dispatch (stage : Stage)
  | finish (stage : Stage) (success confirmedFailure : Bool)
  | restart | saveUnrelated

def applyAction (j : Journal) : Action → Journal
  | .record stage durable => recordAttempt j stage durable
  | .dispatch stage => dispatch j stage
  | .finish stage success failure => finishAttempt j stage success failure
  | .restart => restart j
  | .saveUnrelated => saveUnrelated j

def stageString : Stage → String
  | .commit => "commit" | .parent => "parent" | .executor => "executor"

def statusString : Status → String
  | .fresh => "fresh" | .pending => "pending" | .succeeded => "succeeded" | .failed => "failed"

def journalJson (j : Journal) : Json := Json.mkObj
  [("client", toJson j.client), ("agent_did", toJson j.agentDid), ("node_did", toJson j.nodeDid),
   ("agent_doc", toJson j.agentDoc), ("node_doc", toJson j.nodeDoc),
   ("commit", toJson (statusString j.commit)), ("parent", toJson (statusString j.parent)),
   ("executor", toJson (statusString j.executor)), ("permit", toJson (j.permit.map stageString))]

def actionJson : Action → Json
  | .record stage durable => Json.mkObj [("action", toJson "record"),
      ("stage", toJson (stageString stage)), ("durable", toJson durable)]
  | .dispatch stage => Json.mkObj [("action", toJson "dispatch"), ("stage", toJson (stageString stage))]
  | .finish stage success failure => Json.mkObj [("action", toJson "finish"),
      ("stage", toJson (stageString stage)), ("success", toJson success), ("confirmed_failure", toJson failure)]
  | .restart => Json.mkObj [("action", toJson "restart")]
  | .saveUnrelated => Json.mkObj [("action", toJson "save_unrelated")]

def trace : Journal → List Action → List Json
  | _, [] => []
  | before, action :: rest =>
      let after := applyAction before action
      Json.mkObj [("action", actionJson action), ("before", journalJson before),
        ("after", journalJson after)] :: trace after rest

def initial : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc" }

structure RecoveryCase where
  name : String
  actorMatches : Bool
  recordPresent : Bool
  bindingMatches : Bool
  commit : Status
  parent : Status
  executor : Status

def recoveryCases : List RecoveryCase :=
  [ ⟨"no-record", true, false, true, .fresh, .fresh, .fresh⟩
  , ⟨"wrong-actor", false, false, true, .fresh, .fresh, .fresh⟩
  , ⟨"wrong-binding", true, true, false, .succeeded, .succeeded, .succeeded⟩
  , ⟨"confirmed-aborted-commit", true, true, true, .failed, .fresh, .fresh⟩
  , ⟨"unconfirmed-commit", true, true, true, .pending, .fresh, .fresh⟩
  , ⟨"fresh-commit-record", true, true, true, .fresh, .fresh, .fresh⟩
  , ⟨"parent-fresh", true, true, true, .succeeded, .fresh, .fresh⟩
  , ⟨"parent-pending", true, true, true, .succeeded, .pending, .fresh⟩
  , ⟨"parent-failed", true, true, true, .succeeded, .failed, .fresh⟩
  , ⟨"executor-fresh", true, true, true, .succeeded, .succeeded, .fresh⟩
  , ⟨"executor-pending", true, true, true, .succeeded, .succeeded, .pending⟩
  , ⟨"executor-failed", true, true, true, .succeeded, .succeeded, .failed⟩
  , ⟨"all-complete", true, true, true, .succeeded, .succeeded, .succeeded⟩ ]

def recoveryString : RecoveryDisposition → String
  | .proceed => "proceed" | .bindingMismatch => "binding_mismatch"
  | .commitUnknown => "commit_unknown" | .explicitRepair => "explicit_repair"

def recoveryCaseJson (c : RecoveryCase) : Json :=
  let j := { initial with commit := c.commit, parent := c.parent, executor := c.executor }
  Json.mkObj [("name", toJson c.name), ("actor_matches", toJson c.actorMatches),
    ("record_present", toJson c.recordPresent), ("binding_matches", toJson c.bindingMatches),
    ("commit", toJson (statusString c.commit)), ("parent", toJson (statusString c.parent)),
    ("executor", toJson (statusString c.executor)),
    ("expected", toJson (recoveryString (recoveryDisposition c.actorMatches c.recordPresent c.bindingMatches j)))]

def recoveryCasesJson : String := (toJson (recoveryCases.map recoveryCaseJson)).compress

def commitSuccess : List Action := [.record .commit true, .dispatch .commit, .finish .commit true false]
def parentSuccess : List Action := [.record .parent true, .dispatch .parent, .finish .parent true false]
def cases : List (String × List Action) :=
  [("complete", commitSuccess ++ parentSuccess ++ [.record .executor true, .dispatch .executor,
      .finish .executor true false]),
   ("unknown-commit-no-replay", [.record .commit true, .dispatch .commit, .restart,
      .record .commit true, .dispatch .commit]),
   ("recorded-before-dispatch-no-replay", [.record .commit true, .restart,
      .record .commit true, .dispatch .commit]),
   ("reservation-failed", [.record .commit false, .dispatch .commit]),
   ("confirmed-commit-failure-retry", [.record .commit true, .dispatch .commit,
      .finish .commit false true, .record .commit true, .dispatch .commit, .finish .commit true false]),
   ("unknown-parent-no-replay", commitSuccess ++ [.record .parent true, .dispatch .parent,
      .finish .parent false false, .restart, .record .parent true, .dispatch .parent]),
   ("failed-suffix-does-not-replay-prefix", commitSuccess ++ parentSuccess ++
      [.record .executor true, .dispatch .executor, .finish .executor false true,
       .record .parent true, .record .executor true, .dispatch .executor, .finish .executor true false]),
   ("unrelated-save-independent", commitSuccess ++ parentSuccess ++ [.saveUnrelated])]

def casesJson : String := (toJson (cases.map fun (name, actions) =>
  Json.mkObj [("name", toJson name), ("initial", journalJson initial),
    ("steps", toJson (trace initial actions))])).compress

end Identity.ClientAgentJournal
