import Proofs.Mailbox.Properties

namespace Mailbox.Notification

inductive Mode where
  | event
  | condition
  deriving DecidableEq, Repr

/-- Configuration supplies condition identity; runtime supplies event identity. -/
structure Key where
  mode : Mode
  agent : String
  requester : String
  behavior : String
  value : String
  deriving DecidableEq, Repr

def resolveKey (mode : Mode) (agent requester behavior configuredKey eventId : String) : Key :=
  ⟨mode, agent, requester, behavior, if mode = .condition then configuredKey else eventId⟩

theorem condition_ignores_invocation (agent requester behavior key event₁ event₂ : String) :
    resolveKey .condition agent requester behavior key event₁ = resolveKey .condition agent requester behavior key event₂ := by
  rfl

theorem different_events_do_not_coalesce (agent requester behavior key event₁ event₂ : String)
    (h : event₁ ≠ event₂) :
    resolveKey .event agent requester behavior key event₁ ≠ resolveKey .event agent requester behavior key event₂ := by
  simp [resolveKey, h]

theorem behavior_scopes_notification (mode : Mode) (agent requester b₁ b₂ key eventId : String)
    (h : b₁ ≠ b₂) : resolveKey mode agent requester b₁ key eventId ≠ resolveKey mode agent requester b₂ key eventId := by
  simp [resolveKey, h]

theorem requester_scopes_notification (mode : Mode) (agent r₁ r₂ behavior key eventId : String)
    (h : r₁ ≠ r₂) : resolveKey mode agent r₁ behavior key eventId ≠ resolveKey mode agent r₂ behavior key eventId := by
  simp [resolveKey, h]

theorem agent_scopes_notification (mode : Mode) (a₁ a₂ requester behavior key eventId : String)
    (h : a₁ ≠ a₂) : resolveKey mode a₁ requester behavior key eventId ≠ resolveKey mode a₂ requester behavior key eventId := by
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
