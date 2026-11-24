import Proofs.Shared.Basic

namespace Compaction.Cursor

/-- A compaction cursor names the last stored message of its session that the
summary covers. Provider input loads only the messages after the cursor, so a
cursor past the last stored message hides later messages no summary covers, and
a fork keeps a copied cursor only beside the message it names. Every cursor,
saved by the compaction owner, advanced by the runtime or copied by a fork, is
decided here. -/
def namesStoredMessage (stored : List Nat) (cursor : Nat) : Bool :=
  stored.contains cursor

theorem namesStoredMessage_iff (stored : List Nat) (cursor : Nat) :
    namesStoredMessage stored cursor = true ↔ cursor ∈ stored := by
  simp [namesStoredMessage]

theorem cursor_past_every_stored_message_is_refused (stored : List Nat) (cursor : Nat)
    (past : ∀ sequence ∈ stored, sequence < cursor) :
    namesStoredMessage stored cursor = false := by
  cases h : namesStoredMessage stored cursor
  · rfl
  · exact absurd (past cursor ((namesStoredMessage_iff stored cursor).mp h)) (Nat.lt_irrefl _)

end Compaction.Cursor
