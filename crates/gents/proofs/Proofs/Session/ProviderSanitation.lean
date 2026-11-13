import Lean
import Proofs.Session.PromptAssembly.Provider

namespace PromptAssembly.ProviderSanitation
open Lean

structure Part where
  kind : String
  id : Nat
  deriving ToJson, DecidableEq, Repr

structure Row where
  role : String
  result : Option Nat
  content : List Part
  deriving ToJson, DecidableEq, Repr

structure Input where
  rows : List Row
  deriving ToJson

def toProvider (index : Nat) (row : Row) : Provider.ProviderRow :=
  let content := row.content.map fun part =>
    if part.kind == "call" then Content.Item.call part.id else Content.Item.text part.id
  let kind := match row.result with
    | some id => Transcript.MessageKind.toolResult id ⟨1, id, id⟩
    | none =>
      let calls := Content.callsOf content
      if calls = ∅ then Transcript.MessageKind.ordinary else .assistantToolCalls calls
  ⟨⟨index, 1, index, if row.role == "assistant" then .assistant else .user, kind⟩, content⟩

def fromProvider (row : Provider.ProviderRow) : Row :=
  ⟨row.row.role.toDefraDB, match row.row.kind with | .toolResult id _ => some id | _ => none,
    row.content.map fun item => match item with
      | .call id => ⟨"call", id⟩
      | .text id | .other id => ⟨"text", id⟩⟩

/-- Content-bearing sanitation uses the existing provider owner. The fixture
vocabulary has unique call IDs, and numeric text tokens preserve full content
identity through its representation adapter. -/
def sanitize (input : Input) : List Row :=
  (Provider.sanitizeForProviderGlobal (input.rows.zipIdx.map fun (row, index) => toProvider index row)).map fromProvider

def Row.isAssistant (row : Row) : Bool := row.role == "assistant"

def Row.callIds (row : Row) : List Nat :=
  row.content.filterMap fun part => if part.kind == "call" then some part.id else none

/-- A message that neither announces a call nor carries a result; a durable
prefix ending in one is closed. -/
def Row.ordinary (row : Row) : Bool :=
  if row.isAssistant then row.callIds.isEmpty else row.result.isNone

/-- `drop_orphaned_tool_results`: an assistant row opens the calls it announces
(as a set), a result is kept only while its call is still open and consumes it,
and a user row with plain content closes the turn. -/
def perTurnDropOrphaned (pending : List Nat) : List Row → List Row
  | [] => []
  | row :: rest =>
    if row.isAssistant then row :: perTurnDropOrphaned row.callIds.eraseDups rest
    else match row.result with
      | some id =>
          if id ∈ pending then row :: perTurnDropOrphaned (pending.erase id) rest
          else perTurnDropOrphaned pending rest
      | none =>
          if row.content.isEmpty then perTurnDropOrphaned pending rest
          else row :: perTurnDropOrphaned [] rest

/-- The result keys answering the assistant row that precedes `rows`: results
up to the next assistant row or plain user content. -/
def perTurnResolved : List Row → List Nat
  | [] => []
  | row :: rest =>
    if row.isAssistant then []
    else match row.result with
      | some id => id :: perTurnResolved rest
      | none => if row.content.isEmpty then perTurnResolved rest else []

/-- A call survives when its own turn resolves it and no earlier call of the
same row announced the same key. -/
def perTurnKeepCalls (resolved : List Nat) : List Nat → List Part → List Part
  | _, [] => []
  | announced, part :: rest =>
    if part.kind == "call" then
      if part.id ∈ resolved && !(part.id ∈ announced) then
        part :: perTurnKeepCalls resolved (part.id :: announced) rest
      else perTurnKeepCalls resolved announced rest
    else part :: perTurnKeepCalls resolved announced rest

/-- `drop_unpaired_tool_calls`: resolution is per turn, so a key reused by a
later turn neither resolves nor is resolved by an earlier one. -/
def perTurnDropUnpaired : List Row → List Row
  | [] => []
  | row :: rest =>
    if row.isAssistant then
      let kept := perTurnKeepCalls (perTurnResolved rest) [] row.content
      if kept.isEmpty then perTurnDropUnpaired rest
      else { row with content := kept } :: perTurnDropUnpaired rest
    else row :: perTurnDropUnpaired rest

def perTurnAssociation (rows : List Row) : List Row :=
  perTurnDropUnpaired (perTurnDropOrphaned [] rows)

def groupRow (row : Row) : Row :=
  if row.isAssistant then
    { row with content :=
        row.content.filter (·.kind != "call") ++ row.content.filter (·.kind == "call") }
  else row

/-- `sanitize_history_for_provider` on a grouped profile, with occurrences:
unlike `sanitize`, a call key may repeat within a row and across turns. -/
def perTurnSanitize (input : Input) : List Row :=
  (perTurnAssociation input.rows).map groupRow

/-- `safe_to_reduce`: the history ends on an ordinary message and pairing
leaves it unchanged. -/
def perTurnSafeToReduce (input : Input) : Bool :=
  (input.rows.getLast?.map Row.ordinary).getD false && perTurnAssociation input.rows == input.rows

theorem duplicate_call_keys_in_one_turn_do_not_leave_a_dangling_call :
    perTurnSanitize ⟨[⟨"assistant", none, [⟨"call", 1⟩, ⟨"call", 1⟩]⟩,
      ⟨"user", some 1, [⟨"text", 2⟩]⟩, ⟨"user", some 1, [⟨"text", 3⟩]⟩]⟩ =
      [⟨"assistant", none, [⟨"call", 1⟩]⟩, ⟨"user", some 1, [⟨"text", 2⟩]⟩] := by
  native_decide

theorem reused_call_ids_no_longer_shift_the_provider_view_prefix :
    perTurnSanitize ⟨[⟨"assistant", none, [⟨"call", 1⟩]⟩, ⟨"user", none, [⟨"text", 2⟩]⟩]⟩ =
        [⟨"user", none, [⟨"text", 2⟩]⟩] ∧
      perTurnSanitize ⟨[⟨"assistant", none, [⟨"call", 1⟩]⟩, ⟨"user", none, [⟨"text", 2⟩]⟩]⟩ <+:
        perTurnSanitize ⟨[⟨"assistant", none, [⟨"call", 1⟩]⟩, ⟨"user", none, [⟨"text", 2⟩]⟩,
          ⟨"assistant", none, [⟨"call", 1⟩]⟩, ⟨"user", some 1, [⟨"text", 3⟩]⟩]⟩ := by
  native_decide

theorem safe_to_reduce_accepts_reused_provider_ids_across_complete_turns :
    perTurnSafeToReduce ⟨[⟨"user", none, [⟨"text", 1⟩]⟩, ⟨"assistant", none, [⟨"call", 1⟩]⟩,
      ⟨"user", some 1, [⟨"text", 2⟩]⟩, ⟨"assistant", none, [⟨"text", 3⟩]⟩,
      ⟨"user", none, [⟨"text", 4⟩]⟩, ⟨"assistant", none, [⟨"call", 1⟩]⟩,
      ⟨"user", some 1, [⟨"text", 5⟩]⟩, ⟨"assistant", none, [⟨"text", 6⟩]⟩]⟩ = true := by
  native_decide

end PromptAssembly.ProviderSanitation
