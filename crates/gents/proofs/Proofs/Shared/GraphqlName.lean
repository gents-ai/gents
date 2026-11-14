/-! The GraphQL interpolation defences: which strings are legal as a GraphQL
identifier, which collection identifiers the `__` introspection prefix
reserves, and the exact escaping a value receives before it is spliced into a
GraphQL string literal. A value in identifier position is never escaped, so its
whole defence is the Name grammar; a value in string position is quoted, so its
defence is the escape table. -/

namespace GraphqlName

inductive NameOutcome where
  | accepted
  | empty
  | badStart
  | badCharacter
  deriving DecidableEq, Repr

inductive CollectionOutcome where
  | accepted
  | empty
  | badStart
  | badCharacter
  | reservedPrefix
  deriving DecidableEq, Repr

def nameStartOk (c : Char) : Bool :=
  ('a' ≤ c && c ≤ 'z') || ('A' ≤ c && c ≤ 'Z') || c = '_'

def nameCharOk (c : Char) : Bool :=
  nameStartOk c || ('0' ≤ c && c ≤ '9')

def nameOutcome (name : String) : NameOutcome :=
  match name.toList with
  | [] => .empty
  | first :: rest =>
      if nameStartOk first then
        if rest.all nameCharOk then .accepted else .badCharacter
      else .badStart

def collectionOutcome (name : String) : CollectionOutcome :=
  match nameOutcome name with
  | .accepted => if name.startsWith "__" then .reservedPrefix else .accepted
  | .empty => .empty
  | .badStart => .badStart
  | .badCharacter => .badCharacter

def hexDigit (n : Nat) : Char :=
  "0123456789abcdef".toList.getD (n % 16) '0'

def hex4 (n : Nat) : List Char :=
  [hexDigit (n / 4096), hexDigit (n / 256), hexDigit (n / 16), hexDigit n]

def escapeChar (c : Char) : List Char :=
  if c = '\\' then ['\\', '\\']
  else if c = '"' then ['\\', '"']
  else if c = '\n' then ['\\', 'n']
  else if c = '\r' then ['\\', 'r']
  else if c = '\t' then ['\\', 't']
  else if c.val.toNat ≤ 31 then '\\' :: 'u' :: hex4 c.val.toNat
  else [c]

def escapeChars : List Char → List Char
  | [] => []
  | c :: rest => escapeChar c ++ escapeChars rest

def escape (value : String) : String := String.mk (escapeChars value.toList)

theorem conforming_names_are_accepted :
    nameOutcome "AgentResponse" = .accepted
      ∧ nameOutcome "_docID" = .accepted
      ∧ nameOutcome "_eq" = .accepted
      ∧ nameOutcome "snake_case_2" = .accepted
      ∧ nameOutcome "a" = .accepted
      ∧ nameOutcome "_" = .accepted
      ∧ nameOutcome "WebhookEvent" = .accepted := by
  native_decide

theorem nonconforming_names_are_rejected :
    nameOutcome "" = .empty
      ∧ nameOutcome "1abc" = .badStart
      ∧ nameOutcome "Msg (limit: 1) { _docID }" = .badCharacter
      ∧ nameOutcome "a b" = .badCharacter
      ∧ nameOutcome "a-b" = .badCharacter
      ∧ nameOutcome "a.b" = .badCharacter
      ∧ nameOutcome "Msg{" = .badCharacter
      ∧ nameOutcome "a\"b" = .badCharacter
      ∧ nameOutcome "a\nb" = .badCharacter
      ∧ nameOutcome "naïve" = .badCharacter
      ∧ nameOutcome "名前" = .badStart
      ∧ nameOutcome (String.mk ['a', Char.ofNat 0x200b, 'b']) = .badCharacter
      ∧ nameOutcome " AgentResponse" = .badStart
      ∧ nameOutcome "AgentResponse " = .badCharacter := by
  native_decide

theorem introspection_names_are_reserved :
    collectionOutcome "__Type" = .reservedPrefix
      ∧ collectionOutcome "__schema" = .reservedPrefix
      ∧ collectionOutcome "__typename" = .reservedPrefix
      ∧ collectionOutcome "__" = .reservedPrefix := by
  native_decide

theorem a_single_leading_underscore_is_not_reserved :
    collectionOutcome "_Private" = .accepted := by
  native_decide

theorem the_string_escapes_are_exact :
    escape "hello" = "hello"
      ∧ escape "he\"llo" = "he\\\"llo"
      ∧ escape "line1\nline2" = "line1\\nline2"
      ∧ escape "cr\r" = "cr\\r"
      ∧ escape "tab\there" = "tab\\there"
      ∧ escape "back\\slash" = "back\\\\slash"
      ∧ escape "mixed\"\n\\" = "mixed\\\"\\n\\\\" := by
  native_decide

end GraphqlName
