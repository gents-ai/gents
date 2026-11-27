import Proofs.Store.Identity.Household
import Proofs.Store.Identity.ClientAgentCreation
import Proofs.Tools.ToolPolicy.PluginData

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

/-- A client-signed Agent creation commits the document, then grants `parent`,
`executor` and `host` in that order. `host` places the Agent on its creating
Node; without it the Agent never runs on a Vera home, so it is a stage of its
own rather than an unrecorded trailing grant. -/
inductive Stage where
  | commit | parent | executor | host
  deriving BEq, DecidableEq, Repr

/-- `superseded` is a `host` grant the client no longer owes: the owner placed
the Agent on another Node before the client learned its own grant's outcome. -/
inductive Status where
  | fresh | pending | succeeded | failed | superseded
  deriving DecidableEq, Repr

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
  host : Status := .fresh
  permit : Option Stage := none
  deriving BEq, DecidableEq, Repr

def status (j : Journal) : Stage → Status
  | .commit => j.commit
  | .parent => j.parent
  | .executor => j.executor
  | .host => j.host

def setStatus (j : Journal) (stage : Stage) (next : Status) : Journal :=
  match stage with
  | .commit => { j with commit := next }
  | .parent => { j with parent := next }
  | .executor => { j with executor := next }
  | .host => { j with host := next }

def retryable (s : Status) : Bool := s == .fresh || s == .failed

/-- `failed` means native evidence confirms no effect; an ambiguous error is
pending. The marker reservation must be durable before a dispatch permit is
returned. Failed or ambiguous persistence supplies no permit. -/
def mayRecord (j : Journal) (stage : Stage) : Bool :=
  !j.client.isEmpty && !j.agentDid.isEmpty && !j.nodeDid.isEmpty &&
    !j.agentDoc.isEmpty && !j.nodeDoc.isEmpty && j.permit.isNone &&
    retryable (status j stage) && (match stage with
      | .commit => j.parent == .fresh && j.executor == .fresh && j.host == .fresh
      | .parent => j.commit == .succeeded
      | .executor => j.commit == .succeeded && j.parent == .succeeded
      | .host => j.commit == .succeeded && j.parent == .succeeded && j.executor == .succeeded)

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

/-- The prefix a `host` grant follows: commit, parent and executor succeeded. -/
def hostOwed (j : Journal) : Bool :=
  j.commit == .succeeded && j.parent == .succeeded && j.executor == .succeeded

/-- A pending `host` grant is sent again only under the request-bound
delegation retained with it, while that delegation holds.
External premise (vera.rs `docs/operation-identities.md`): a request resent
under the same delegation, for the same actor, operation identity and digest,
returns the first successful outcome without applying it again, so the grant
takes effect once even when the owner withdrew it in between. -/
def resumeAttempt (j : Journal) (tokenValid : Bool) : Journal :=
  if tokenValid && hostOwed j && j.permit.isNone && j.host == .pending then
    { j with permit := some .host }
  else j

/-- What a read of the Agent's `host` relationships shows. -/
inductive HostReading where
  | thisNode | otherNode | absent
  deriving DecidableEq, Repr

/-- A pending `host` grant whose delegation lapsed is settled by reading the
Agent's `host` relationships, never by granting again: a grant for this Node
succeeded, one for another Node means the owner moved the Agent, and none
leaves it pending. -/
def observeHost (j : Journal) (reading : HostReading) : Journal :=
  if hostOwed j && j.permit.isNone && j.host == .pending then
    match reading with
    | .thisNode => { j with host := .succeeded }
    | .otherNode => { j with host := .superseded }
    | .absent => j
  else j

/-- A save of another document has no authority over this journal. -/
def saveUnrelated (j : Journal) : Journal := j

/-- `resendHost` sends the `host` grant: never sent (`fresh`), confirmed not
applied (`failed`), or pending under a delegation that still holds.
`observedHost`, `supersededHost` and `absentHost` follow a read of the Agent's
`host` relationships once a pending grant's delegation lapsed; `absentHost`
grants nothing and reports the Agent as placed on no device. -/
inductive RecoveryDisposition where
  | proceed | bindingMismatch | commitUnknown | explicitRepair
  | resendHost | observedHost | supersededHost | absentHost
  deriving BEq, DecidableEq, Repr

/-- The publication is complete only once every stage, `host` included,
succeeded; a committed creation whose `host` grant is unfinished is not. -/
def complete (j : Journal) : Bool :=
  hostOwed j && j.host == .succeeded

/-- Nothing is owed: complete, or its `host` grant superseded by the owner. -/
def settled (j : Journal) : Bool :=
  complete j || (hostOwed j && j.host == .superseded)

/-- Whether recovery must read the Agent's `host` relationships. -/
def readsHost (j : Journal) (tokenValid : Bool) : Bool :=
  hostOwed j && j.host == .pending && !tokenValid

/-- A retained, committed publication whose `parent` or `executor` is
unfinished is not automatically resumed, even if the missing operation was
never dispatched or its failure is known: reconstructing authority for it
requires explicit repair. An unfinished `host` grant is recovered: sent when
nothing of it may have applied or its delegation still holds, else settled by
`reading`. -/
def recoveryDisposition (actorMatches recordPresent bindingMatches tokenValid : Bool)
    (reading : HostReading) (j : Journal) : RecoveryDisposition :=
  if !actorMatches then .bindingMismatch
  else if !recordPresent then .proceed
  else if !bindingMatches then .bindingMismatch
  else if j.commit == .failed || settled j then .proceed
  else if j.commit != .succeeded then .commitUnknown
  else if !hostOwed j then .explicitRepair
  else if readsHost j tokenValid then
    match reading with
    | .thisNode => .observedHost
    | .otherNode => .supersededHost
    | .absent => .absentHost
  else .resendHost

theorem interrupted_fresh_suffix_requires_repair :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded }
    recoveryDisposition true true true false .absent (restart j) = .explicitRepair := by decide

theorem interrupted_failed_suffix_requires_repair :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded, executor := .failed }
    recoveryDisposition true true true false .absent (restart j) = .explicitRepair := by decide

theorem completed_publication_needs_no_repair :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded, executor := .succeeded, host := .succeeded }
    recoveryDisposition true true true false .absent (restart j) = .proceed := by decide

/-- A crash after the `executor` grant and before the `host` attempt is
recorded leaves `host` fresh: nothing of it can have applied, so it is sent. -/
theorem interrupted_before_host_resends :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded, executor := .succeeded }
    recoveryDisposition true true true false .absent (restart j) = .resendHost := by decide

/-- A crash after the `host` request and before its outcome is recorded
resends it under the retained delegation while that holds. -/
theorem interrupted_host_resends_while_its_delegation_holds :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded, executor := .succeeded, host := .pending }
    recoveryDisposition true true true true .absent (restart j) = .resendHost := by decide

theorem superseded_host_needs_nothing :
    let j : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc", commit := .succeeded, parent := .succeeded, executor := .succeeded, host := .superseded }
    recoveryDisposition true true true false .absent (restart j) = .proceed := by decide

theorem status_beq_iff (a b : Status) : (a == b) = true ↔ a = b := by
  cases a <;> cases b <;> decide

theorem host_recorded_only_after_executor (j : Journal)
    (allowed : mayRecord j .host = true) :
    j.commit = .succeeded ∧ j.parent = .succeeded ∧ j.executor = .succeeded := by
  simp only [mayRecord, Bool.and_eq_true, status_beq_iff] at allowed
  exact ⟨allowed.2.1.1, allowed.2.1.2, allowed.2.2⟩

theorem complete_requires_host (j : Journal) (done : complete j = true) :
    j.host = .succeeded := by
  simp only [complete, hostOwed, Bool.and_eq_true, status_beq_iff] at done
  exact done.2

theorem settled_requires_a_settled_host (j : Journal) (done : settled j = true) :
    j.host = .succeeded ∨ j.host = .superseded := by
  simp only [settled, complete, hostOwed, Bool.or_eq_true, Bool.and_eq_true,
    status_beq_iff] at done
  rcases done with done | done
  · exact Or.inl done.2
  · exact Or.inr done.2

theorem committed_proceed_requires_a_settled_host
    (actorMatches bindingMatches tokenValid : Bool) (reading : HostReading) (j : Journal)
    (committed : j.commit = .succeeded)
    (proceeds : recoveryDisposition actorMatches true bindingMatches tokenValid reading j =
      .proceed) :
    j.host = .succeeded ∨ j.host = .superseded := by
  by_cases done : settled j = true
  · exact settled_requires_a_settled_host j done
  · simp only [Bool.not_eq_true] at done
    cases actorMatches <;> cases bindingMatches <;> cases reading <;>
      simp [recoveryDisposition, done, committed] at proceeds <;>
      (repeat' split at proceeds) <;> simp_all

/-- A pending grant is never sent again once its delegation lapsed. -/
theorem lapsed_pending_host_is_never_resent
    (actorMatches recordPresent bindingMatches : Bool) (reading : HostReading) (j : Journal)
    (pending : j.host = .pending) :
    recoveryDisposition actorMatches recordPresent bindingMatches false reading j ≠
      .resendHost := by
  intro resends
  have hsettled : settled j = false := by simp [settled, complete, pending]
  cases actorMatches <;> cases recordPresent <;> cases bindingMatches <;> cases reading <;>
    simp [recoveryDisposition, hsettled, readsHost, pending] at resends <;>
    (repeat' split at resends) <;> simp_all

theorem lapsed_absent_host_grants_nothing (j : Journal)
    (owed : hostOwed j = true) (pending : j.host = .pending) :
    recoveryDisposition true true true false .absent j = .absentHost ∧
      observeHost j .absent = j := by
  have hsettled : settled j = false := by simp [settled, complete, owed, pending]
  have hcommit : j.commit = .succeeded := by
    simp only [hostOwed, Bool.and_eq_true, decide_eq_true_eq, beq_iff_eq] at owed
    exact owed.1.1
  constructor
  · simp [recoveryDisposition, hsettled, hcommit, owed, readsHost, pending]
  · unfold observeHost; split <;> rfl

theorem resume_requires_a_holding_delegation (j : Journal) : resumeAttempt j false = j := by
  simp [resumeAttempt]

theorem another_nodes_host_is_not_this_grant (j : Journal)
    (owed : hostOwed j = true) (pending : j.host = .pending) (free : j.permit = none) :
    (observeHost j .otherNode).host = .superseded := by
  simp [observeHost, owed, pending, free]

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
  | resume (tokenValid : Bool)
  | observe (reading : HostReading)
  | restart | saveUnrelated

def applyAction (j : Journal) : Action → Journal
  | .record stage durable => recordAttempt j stage durable
  | .dispatch stage => dispatch j stage
  | .finish stage success failure => finishAttempt j stage success failure
  | .resume tokenValid => resumeAttempt j tokenValid
  | .observe reading => observeHost j reading
  | .restart => restart j
  | .saveUnrelated => saveUnrelated j

def initial : Journal := { client := "client", agentDid := "agent", nodeDid := "node", agentDoc := "agent-doc", nodeDoc := "node-doc" }

structure RecoveryCase where
  name : String
  actorMatches : Bool
  recordPresent : Bool
  bindingMatches : Bool
  commit : Status
  parent : Status
  executor : Status
  host : Status
  tokenValid : Bool := false
  reading : HostReading := .absent

def recoveryCases : List RecoveryCase :=
  [ ⟨"no-record", true, false, true, .fresh, .fresh, .fresh, .fresh, false, .absent⟩
  , ⟨"wrong-actor", false, false, true, .fresh, .fresh, .fresh, .fresh, false, .absent⟩
  , ⟨"wrong-binding", true, true, false, .succeeded, .succeeded, .succeeded, .succeeded, false, .absent⟩
  , ⟨"confirmed-aborted-commit", true, true, true, .failed, .fresh, .fresh, .fresh, false, .absent⟩
  , ⟨"unconfirmed-commit", true, true, true, .pending, .fresh, .fresh, .fresh, false, .absent⟩
  , ⟨"fresh-commit-record", true, true, true, .fresh, .fresh, .fresh, .fresh, false, .absent⟩
  , ⟨"parent-fresh", true, true, true, .succeeded, .fresh, .fresh, .fresh, false, .absent⟩
  , ⟨"parent-pending", true, true, true, .succeeded, .pending, .fresh, .fresh, false, .absent⟩
  , ⟨"parent-failed", true, true, true, .succeeded, .failed, .fresh, .fresh, false, .absent⟩
  , ⟨"executor-fresh", true, true, true, .succeeded, .succeeded, .fresh, .fresh, false, .absent⟩
  , ⟨"executor-pending", true, true, true, .succeeded, .succeeded, .pending, .fresh, false, .absent⟩
  , ⟨"executor-failed", true, true, true, .succeeded, .succeeded, .failed, .fresh, false, .absent⟩
  , ⟨"host-fresh", true, true, true, .succeeded, .succeeded, .succeeded, .fresh, false, .absent⟩
  , ⟨"host-failed", true, true, true, .succeeded, .succeeded, .succeeded, .failed, false, .absent⟩
  , ⟨"host-pending-holding", true, true, true, .succeeded, .succeeded, .succeeded, .pending, true, .absent⟩
  , ⟨"host-pending-lapsed-this-node", true, true, true, .succeeded, .succeeded, .succeeded, .pending, false, .thisNode⟩
  , ⟨"host-pending-lapsed-other-node", true, true, true, .succeeded, .succeeded, .succeeded, .pending, false, .otherNode⟩
  , ⟨"host-pending-lapsed-absent", true, true, true, .succeeded, .succeeded, .succeeded, .pending, false, .absent⟩
  , ⟨"host-superseded", true, true, true, .succeeded, .succeeded, .succeeded, .superseded, false, .absent⟩
  , ⟨"all-complete", true, true, true, .succeeded, .succeeded, .succeeded, .succeeded, false, .absent⟩ ]

def commitSuccess : List Action := [.record .commit true, .dispatch .commit, .finish .commit true false]
def parentSuccess : List Action := [.record .parent true, .dispatch .parent, .finish .parent true false]
def executorSuccess : List Action :=
  [.record .executor true, .dispatch .executor, .finish .executor true false]
def cases : List (String × List Action) :=
  [("complete", commitSuccess ++ parentSuccess ++ executorSuccess ++
      [.record .host true, .dispatch .host, .finish .host true false]),
   ("host-before-executor-refused", commitSuccess ++ parentSuccess ++
      [.record .host true, .dispatch .host]),
   ("unknown-host-no-replay", commitSuccess ++ parentSuccess ++ executorSuccess ++
      [.record .host true, .dispatch .host, .restart, .record .host true, .dispatch .host]),
   ("failed-host-retry", commitSuccess ++ parentSuccess ++ executorSuccess ++
      [.record .host true, .dispatch .host, .finish .host false true,
       .record .executor true, .record .host true, .dispatch .host, .finish .host true false]),
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
   ("unrelated-save-independent", commitSuccess ++ parentSuccess ++ [.saveUnrelated]),
   ("pending-host-resumed-while-holding", commitSuccess ++ parentSuccess ++ executorSuccess ++
      [.record .host true, .dispatch .host, .restart, .resume true, .dispatch .host,
       .finish .host true false]),
   ("pending-host-not-resumed-after-lapse", commitSuccess ++ parentSuccess ++ executorSuccess ++
      [.record .host true, .dispatch .host, .restart, .resume false, .dispatch .host]),
   ("lapsed-host-observed-here", commitSuccess ++ parentSuccess ++ executorSuccess ++
      [.record .host true, .dispatch .host, .restart, .observe .thisNode]),
   ("lapsed-host-observed-elsewhere", commitSuccess ++ parentSuccess ++ executorSuccess ++
      [.record .host true, .dispatch .host, .restart, .observe .otherNode,
       .record .host true]),
   ("lapsed-host-absent", commitSuccess ++ parentSuccess ++ executorSuccess ++
      [.record .host true, .dispatch .host, .restart, .observe .absent, .record .host true]),
   ("observe-needs-a-pending-host", commitSuccess ++ parentSuccess ++ executorSuccess ++
      [.observe .thisNode])]

end Identity.ClientAgentJournal
