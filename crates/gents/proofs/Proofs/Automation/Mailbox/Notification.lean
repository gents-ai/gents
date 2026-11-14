import Proofs.Automation.Mailbox.Properties

namespace Mailbox.Notification

inductive Mode where
  | event
  | condition
  deriving DecidableEq, Repr

structure Key where
  mode : Mode
  nodeDid : String
  requester : String
  agentDid : String
  value : String
  deriving DecidableEq, Repr

def resolveKey (mode : Mode) (nodeDid requester agentDid configuredKey eventId : String) : Key :=
  ⟨mode, nodeDid, requester, agentDid, if mode = .condition then configuredKey else eventId⟩

theorem condition_ignores_invocation (nodeDid requester agentDid key event₁ event₂ : String) :
    resolveKey .condition nodeDid requester agentDid key event₁ = resolveKey .condition nodeDid requester agentDid key event₂ := by
  rfl

theorem different_events_do_not_coalesce (nodeDid requester agentDid key event₁ event₂ : String)
    (h : event₁ ≠ event₂) :
    resolveKey .event nodeDid requester agentDid key event₁ ≠ resolveKey .event nodeDid requester agentDid key event₂ := by
  simp [resolveKey, h]

theorem agent_scopes_notification (mode : Mode) (nodeDid requester b₁ b₂ key eventId : String)
    (h : b₁ ≠ b₂) : resolveKey mode nodeDid requester b₁ key eventId ≠ resolveKey mode nodeDid requester b₂ key eventId := by
  simp [resolveKey, h]

theorem requester_scopes_notification (mode : Mode) (nodeDid r₁ r₂ agentDid key eventId : String)
    (h : r₁ ≠ r₂) : resolveKey mode nodeDid r₁ agentDid key eventId ≠ resolveKey mode nodeDid r₂ agentDid key eventId := by
  simp [resolveKey, h]

theorem node_scopes_notification (mode : Mode) (n₁ n₂ requester agentDid key eventId : String)
    (h : n₁ ≠ n₂) : resolveKey mode n₁ requester agentDid key eventId ≠ resolveKey mode n₂ requester agentDid key eventId := by
  simp [resolveKey, h]

inductive WriteOutcome where
  | created
  | reused
  | updated
  deriving DecidableEq, Repr

def decideWrite (mode : Mode) (openExists sameContent : Bool) : WriteOutcome :=
  if !openExists then .created
  else if mode = .condition ∧ !sameContent then .updated
  else .reused

theorem event_retries_reuse (sameContent : Bool) :
    decideWrite .event true sameContent = .reused := by
  simp [decideWrite]

theorem unchanged_condition_reuses : decideWrite .condition true true = .reused := by rfl

theorem changed_condition_updates : decideWrite .condition true false = .updated := by rfl

structure ContentRow where
  item : Item
  content : String
  deriving DecidableEq, Repr

def updateContent (row : ContentRow) (content : String) : ContentRow :=
  if row.item.status = .open then { row with content := content } else row

theorem update_preserves_envelope (row : ContentRow) (content : String) :
    (updateContent row content).item = row.item := by
  simp [updateContent]; split <;> rfl

theorem terminal_content_is_immutable (row : ContentRow) (content : String)
    (h : row.item.status ≠ .open) : updateContent row content = row := by
  simp [updateContent, h]

/-! Content writes of one condition notification within a single request
stream: the stored row keeps the provenance of the request that created it. -/

structure ContentStep where
  requestId : String
  requester : String
  causeDocId : String
  title : String
  deriving DecidableEq, Repr

structure StoredContent where
  title : String
  requester : String
  requestId : String
  causeDocId : String
  deriving DecidableEq, Repr

structure ContentObservation where
  outcome : WriteOutcome
  title : String
  requesterAdmitted : Bool
  requestId : Option String
  causeDocId : Option String
  sameDocument : Bool
  deriving DecidableEq, Repr

def stepOutcome (previous : Option StoredContent) (step : ContentStep) : WriteOutcome :=
  match previous with
  | none => decideWrite .condition false true
  | some stored => decideWrite .condition true (stored.title == step.title)

def advanceContent (previous : Option StoredContent) (step : ContentStep) : StoredContent :=
  match previous with
  | none => ⟨step.title, step.requester, step.requestId, step.causeDocId⟩
  | some stored =>
    match stepOutcome (some stored) step with
    | .updated => { stored with title := step.title }
    | _ => stored

def observeContent : Option StoredContent → List ContentStep → List ContentObservation
  | _, [] => []
  | previous, step :: rest =>
    let stored := advanceContent previous step
    ⟨stepOutcome previous step, stored.title, stored.requester == step.requester,
      some stored.requestId, some stored.causeDocId, previous.isSome⟩ ::
      observeContent (some stored) rest

def updatedSequence : List ContentStep :=
  [ ⟨"one", "did:test:delegate", "input-one", "Disk 81%, Docker unavailable"⟩,
    ⟨"two", "did:test:delegate", "input-two", "Disk 82%, Docker unavailable"⟩ ]

def repeatedSequence : List ContentStep :=
  [ ⟨"one", "did:test:delegate", "input-one", "Disk 81%, Docker unavailable"⟩,
    ⟨"two", "did:test:delegate", "input-two", "Disk 81%, Docker unavailable"⟩ ]

theorem content_tool_stamps_current_request_and_reuses_configured_condition :
    (observeContent none updatedSequence).map
      (fun o => (o.outcome, o.requesterAdmitted, o.requestId, o.causeDocId, o.sameDocument)) =
    [ (.created, true, some "one", some "input-one", false),
      (.updated, true, some "one", some "input-one", true) ] := by
  decide

theorem repeated_content_reuses_the_open_condition :
    (observeContent none repeatedSequence).map (fun o => (o.outcome, o.title)) =
    [ (.created, "Disk 81%, Docker unavailable"), (.reused, "Disk 81%, Docker unavailable") ] := by
  decide

end Mailbox.Notification
