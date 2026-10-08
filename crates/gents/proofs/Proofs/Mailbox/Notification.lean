import Proofs.Mailbox.Properties

namespace Mailbox.Notification

inductive Mode where
  | event
  | condition
  deriving DecidableEq, Repr

/-- Configuration supplies condition identity; runtime supplies event identity. -/
structure Key where
  mode : Mode
  nodeDid : String
  requester : String
  agentId : String
  value : String
  deriving DecidableEq, Repr

def resolveKey (mode : Mode) (nodeDid requester agentId configuredKey eventId : String) : Key :=
  ⟨mode, nodeDid, requester, agentId, if mode = .condition then configuredKey else eventId⟩

theorem condition_ignores_invocation (nodeDid requester agentId key event₁ event₂ : String) :
    resolveKey .condition nodeDid requester agentId key event₁ = resolveKey .condition nodeDid requester agentId key event₂ := by
  rfl

theorem different_events_do_not_coalesce (nodeDid requester agentId key event₁ event₂ : String)
    (h : event₁ ≠ event₂) :
    resolveKey .event nodeDid requester agentId key event₁ ≠ resolveKey .event nodeDid requester agentId key event₂ := by
  simp [resolveKey, h]

theorem agent_scopes_notification (mode : Mode) (nodeDid requester b₁ b₂ key eventId : String)
    (h : b₁ ≠ b₂) : resolveKey mode nodeDid requester b₁ key eventId ≠ resolveKey mode nodeDid requester b₂ key eventId := by
  simp [resolveKey, h]

theorem requester_scopes_notification (mode : Mode) (nodeDid r₁ r₂ agentId key eventId : String)
    (h : r₁ ≠ r₂) : resolveKey mode nodeDid r₁ agentId key eventId ≠ resolveKey mode nodeDid r₂ agentId key eventId := by
  simp [resolveKey, h]

theorem node_scopes_notification (mode : Mode) (n₁ n₂ requester agentId key eventId : String)
    (h : n₁ ≠ n₂) : resolveKey mode n₁ requester agentId key eventId ≠ resolveKey mode n₂ requester agentId key eventId := by
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

/-- Content updates cannot change tenancy/identity or revive terminal rows. -/
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

end Mailbox.Notification
