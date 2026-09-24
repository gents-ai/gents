import Proofs.PromptAssembly.ClaudeMap
import Lean

namespace PromptAssembly.ClaudeWire

open PromptAssembly.ClaudeMap

/-- Wire decoding rejects a present non-text signature before the executable
Claude content-block owner receives any event. -/
inductive WireError where
  | invalidSignatureType
  deriving DecidableEq, Repr

structure ThinkingStart where
  index : Nat
  thinking : String
  signature : Option Lean.Json

def expandThinkingStart (start : ThinkingStart) :
    Except WireError (List StreamEvent) :=
  match start.signature with
  | none => .ok [.thinkingStart start.index start.thinking]
  | some (.str value) =>
      .ok [.thinkingStart start.index start.thinking,
        .signatureDelta start.index value]
  | some _ => .error .invalidSignatureType

inductive EvaluationError where
  | wire (error : WireError)
  | content (error : MapError)
  deriving DecidableEq, Repr

/-- The wire adapter only expands valid representation into existing ClaudeMap
events. It delegates every order and sealing decision to `runContentTrace`. -/
def evaluate (start : ThinkingStart) (later : List StreamEvent) :
    Except EvaluationError (List ContentStep × List StreamBlock) := do
  let initial ← (expandThinkingStart start).mapError EvaluationError.wire
  (runContentTrace ∅ (initial ++ later)).mapError EvaluationError.content

structure Case where
  name : String
  start : ThinkingStart
  later : List StreamEvent
  expected : Except EvaluationError (List ContentStep × List StreamBlock)

def case (name : String) (start : ThinkingStart)
    (later : List StreamEvent) : Case :=
  { name, start, later, expected := evaluate start later }

def cases : List Case :=
  [ case "initial_signature_then_delta"
      ⟨0, "body", some (.str "S")⟩
      [.signatureDelta 0 "T", .contentStop 0]
  , case "missing_initial_signature_then_delta"
      ⟨0, "body", none⟩
      [.signatureDelta 0 "S", .contentStop 0]
  , case "initial_signature_rejects_later_thinking"
      ⟨0, "body", some (.str "S")⟩
      [.thinkingDelta 0 "late", .contentStop 0]
  , case "numeric_initial_signature_rejected_at_wire_boundary"
      ⟨0, "body", some (.num 42)⟩
      [.signatureDelta 0 "T", .contentStop 0] ]

local instance {ε α : Type} [DecidableEq ε] [DecidableEq α] :
    DecidableEq (Except ε α)
  | .error a, .error b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.error.inj e))
  | .error _, .ok _ => .isFalse nofun
  | .ok _, .error _ => .isFalse nofun
  | .ok a, .ok b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.ok.inj e))

theorem start_signature_and_delta_seal_exactly :
    (evaluate ⟨0, "body", some (.str "S")⟩
      [.signatureDelta 0 "T", .contentStop 0]).map (·.2) =
      .ok [.reasoning [.text "body" (some "ST")]] := by native_decide

theorem malformed_signature_is_wire_error :
    evaluate ⟨0, "body", some (.num 42)⟩
      [.signatureDelta 0 "T", .contentStop 0] =
      .error (.wire .invalidSignatureType) := by native_decide

theorem initial_signature_preserves_thinking_order_rule :
    evaluate ⟨0, "body", some (.str "S")⟩
      [.thinkingDelta 0 "late", .contentStop 0] =
      .error (.content .signatureOrder) := by native_decide

end PromptAssembly.ClaudeWire
