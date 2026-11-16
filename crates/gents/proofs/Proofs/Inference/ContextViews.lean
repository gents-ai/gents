import Proofs.Store.Views

namespace InferenceContextViews

structure Key where
  queued : Option Nat
  sequence : Option Nat
  call : Option Nat
  physical : Nat
  deriving BEq, Repr, Inhabited

/-- DefraDB puts null below present values; comparison filters do not include
null, so a descending cursor has to include it explicitly. -/
def below (value cursor : Option Nat) : Bool :=
  match value, cursor with
  | none, some _ => true
  | some value, some cursor => value < cursor
  | _, _ => false

def after (value cursor : Key) : Bool :=
  below value.queued cursor.queued ||
  (value.queued == cursor.queued && below value.sequence cursor.sequence) ||
  (value.queued == cursor.queued && value.sequence == cursor.sequence &&
    below value.call cursor.call) ||
  (value.queued == cursor.queued && value.sequence == cursor.sequence &&
    value.call == cursor.call && value.physical < cursor.physical)

theorem cursor_excludes_itself (key : Key) : after key key = false := by
  rcases key with ⟨queued, sequence, call, physical⟩
  cases queued <;> cases sequence <;> cases call <;> simp [after, below]

def joined (callReadable requestReadable sameNode sameSession sameRequester sameLogical : Bool) : Bool :=
  callReadable && requestReadable && sameNode && sameSession && sameRequester && sameLogical

theorem joined_rows_require_both_grants
    (callReadable requestReadable sameNode sameSession sameRequester sameLogical : Bool)
    (h : joined callReadable requestReadable sameNode sameSession sameRequester sameLogical = true) :
    callReadable = true ∧ requestReadable = true := by
  cases callReadable <;> cases requestReadable <;> simp_all [joined]

end InferenceContextViews
