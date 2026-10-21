import Proofs.Basic

namespace StreamingResponse.ReasoningDeltas

/-- Stream fragments have no semantic boundary between adjacent unsigned text
with the same reasoning identity. Signatures and intervening text do have a
boundary. This models `AssistantTurnAccumulator.push_reasoning_delta`. -/
inductive Part where
  | reasoning (id signature : Option Nat) (bytes : List Nat)
  | text (bytes : List Nat)
  deriving DecidableEq, Repr

def appendDelta (id : Option Nat) (fragment : List Nat) : List Part → List Part
  | [] => [.reasoning id none fragment]
  | part :: rest =>
      match rest with
      | [] =>
          match part with
          | .reasoning prior none bytes =>
              if prior = id then [.reasoning id none (bytes ++ fragment)]
              else [part, .reasoning id none fragment]
          | _ => [part, .reasoning id none fragment]
      | _ :: _ => part :: appendDelta id fragment rest

def partBytes : Part → List Nat
  | .reasoning _ _ bytes | .text bytes => bytes

def isProtected : Part → Bool
  | .reasoning _ none _ => false
  | _ => true

theorem appendDelta_preserves_bytes (parts : List Part) (id : Option Nat)
    (fragment : List Nat) :
    (appendDelta id fragment parts).flatMap partBytes =
      parts.flatMap partBytes ++ fragment := by
  induction parts with
  | nil => simp [appendDelta, partBytes]
  | cons part rest ih =>
      cases rest with
      | nil =>
          cases part with
          | text bytes => simp [appendDelta, partBytes]
          | reasoning prior signature bytes =>
              cases signature with
              | none =>
                  by_cases h : prior = id <;> simp [appendDelta, partBytes, h]
              | some signature => simp [appendDelta, partBytes]
      | cons next rest =>
          simpa only [appendDelta, List.flatMap_cons, List.append_assoc] using
            congrArg (partBytes part ++ ·) ih

theorem appendDelta_preserves_protected_parts (parts : List Part) (id : Option Nat)
    (fragment : List Nat) :
    (appendDelta id fragment parts).filter isProtected = parts.filter isProtected := by
  induction parts with
  | nil => simp [appendDelta, isProtected]
  | cons part rest ih =>
      cases rest with
      | nil =>
          cases part with
          | text bytes => simp [appendDelta, isProtected]
          | reasoning prior signature bytes =>
              cases signature with
              | none => by_cases h : prior = id <;> simp [appendDelta, isProtected, h]
              | some signature => simp [appendDelta, isProtected]
      | cons next rest =>
          simpa only [appendDelta, List.filter_cons] using
            congrArg (fun tail => if isProtected part then part :: tail else tail) ih

structure Case where
  name : String
  before : List Part
  id : Option Nat
  fragment : List Nat
  expected : List Part
  deriving Repr

def caseOf (name : String) (before : List Part) (id : Option Nat)
    (fragment : List Nat) : Case :=
  { name, before, id, fragment, expected := appendDelta id fragment before }

def cases : List Case :=
  [ caseOf "first_fragment" [] none [65]
  , caseOf "same_identity" [.reasoning (some 1) none [65]] (some 1) [66]
  , caseOf "unicode_bytes" [.reasoning none none [206, 187]] none [240, 159, 166, 128]
  , caseOf "different_identity" [.reasoning (some 1) none [65]] (some 2) [66]
  , caseOf "signed_boundary" [.reasoning (some 1) (some 7) [65]] (some 1) [66]
  , caseOf "text_boundary" [.reasoning none none [65], .text [88]] none [66]
  , caseOf "last_part_only" [.reasoning none (some 7) [65], .reasoning none none [66]] none [67]
  , caseOf "empty_fragment" [.reasoning none none [65]] none []
  ]

end StreamingResponse.ReasoningDeltas
