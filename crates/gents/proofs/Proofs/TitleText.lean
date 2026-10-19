import Proofs.GoalAutomation
import Lean

namespace TitleText

/-- Titles label conversations for readers; they are not identifiers or file names. -/
def preamble : String := "Generate concise conversation titles. Return only a short, human-readable 3-8 word phrase in sentence case, with normal spaces. Preserve proper names and technical terms. Never call tools. Never explain."

def maxWords : Nat := 8
def maxCharacters : Nat := 72

def trim (text : String) : String := GoalAutomation.canonicalObjective text

def unquote (text : String) : String :=
  let quote := fun c => c == '\'' || c == '"' || c == '`'
  String.mk (((trim text).toList.dropWhile quote).reverse.dropWhile quote).reverse

def firstLine (text : String) : String :=
  ((text.splitOn "\n").find? (fun line => !(trim line).isEmpty)).getD "" |> trim

def phrase (text : String) : String :=
  let slug := text.contains '-' && text.toList.all (fun c =>
    (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-')
  String.mk (text.toList.map (fun c => if GoalAutomation.objectiveWhitespace c || (slug && c == '-') then ' ' else c))

def boundedWords : List String → String → String
  | [], acc => acc
  | word :: rest, acc =>
    let next := if acc.isEmpty then word else acc ++ " " ++ word
    if next.length <= maxCharacters then boundedWords rest next
    else if acc.isEmpty then String.mk (word.toList.take maxCharacters) else acc

theorem bounded_words_length (words : List String) (acc : String) (h : acc.length ≤ maxCharacters) :
    (boundedWords words acc).length ≤ maxCharacters := by
  induction words generalizing acc with
  | nil => exact h
  | cons word rest ih =>
    let next := if acc.isEmpty then word else acc ++ " " ++ word
    change (if next.length ≤ maxCharacters then boundedWords rest next
      else if acc.isEmpty then String.mk (word.toList.take maxCharacters) else acc).length ≤ maxCharacters
    split
    · exact ih _ (by assumption)
    · split
      · simp only [String.length_mk, List.length_take]
        exact Nat.min_le_left _ _
      · exact h

def capitalize (text : String) : String :=
  match text.toList with
  | [] => "Conversation"
  | first :: rest => String.mk (first.toUpper :: rest)

def generated (raw fallback : String) : String :=
  let candidate := unquote (firstLine raw)
  let source := if candidate.isEmpty then firstLine fallback else candidate
  let words := ((phrase source).splitOn " ").filter (fun word => !word.isEmpty)
  capitalize (boundedWords (words.take maxWords) "")

def prompt (request : String) (recent : List String) : String :=
  let normalized := String.intercalate " " (((phrase request).splitOn " ").filter (fun word => !word.isEmpty))
  let excerpt := String.mk (normalized.toList.take 600)
  let titles := if recent.isEmpty then "none" else String.intercalate "\n" ((recent.take 5).map (fun title => "- " ++ title))
  "First user request:\n" ++ excerpt ++ "\n\nRecent session titles to avoid repeating:\n" ++ titles

/-- A user-authored name remains exact, including intentional punctuation and spacing. -/
def display (generatedSource : Bool) (text : String) : String :=
  if generatedSource then generated text "" else text

theorem user_title_preserved (text : String) : display false text = text := rfl

theorem empty_generated_title : generated "" "" = "Conversation" := by native_decide

theorem generated_title_is_a_phrase : generated "fix-session-title-format" "" = "Fix session title format" := by native_decide

end TitleText
