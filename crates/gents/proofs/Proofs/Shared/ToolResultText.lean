/-! How the text of one tool result is sent to the provider (`gents-loop`
`provider_input::tool_result_text`). Every route must show the model the whole
text, so the projection either sends it as it is or wraps it in one JSON object
`{"text": …}` that carries it.

External premise (measured 2026-11-22 with direct OpenRouter chat-completions
requests, one tool call and its result, the model asked to quote the result):
on the Google Gemini routes (`google/gemini-4-neon`, `google/gemini-3.8-flash`)
a tool message whose text contains a brace-delimited object anywhere, strict
JSON or not (`gents_fs: {…}` before a body, `…; next call: tool {…}` after a
sentence, `fn f() {}`, `{a: 1}`), reaches the model as that object alone; the
rest of the text is dropped. Text with no `{`, and text that is exactly one
JSON object or array, reaches the model whole there and on the OpenAI, Qwen
and Claude routes measured. Several text parts reach Gemini joined, so one
part with an object drops the others. Which brace spans the conversion
accepts is not specified, so the decision reads only whether a `{` occurs. -/

namespace ToolResultText

/-- The text parts of one tool result; images are not text and are kept. -/
inductive Parts where
  | none
  | one
  | many
  deriving DecidableEq, Repr

inductive Wire where
  /-- The parts are sent unchanged. -/
  | asIs
  /-- The text parts become one part: a JSON object whose `text` field holds
  them joined by newlines. -/
  | wrapped
  deriving DecidableEq, Repr

/-- The text reaches the model whole on every measured route. -/
def survives (parts : Parts) (openBrace oneJsonContainer : Bool) : Bool :=
  parts == .none || !openBrace || (parts == .one && oneJsonContainer)

/-- `openBrace`: some text part contains `{`. `oneJsonContainer`: the only text
part is exactly one JSON object or array. -/
def wire (parts : Parts) (openBrace oneJsonContainer : Bool) : Wire :=
  if survives parts openBrace oneJsonContainer then .asIs else .wrapped

/-- The facts of what is sent: a wrapped result is one part holding one JSON
object, which contains `{`. -/
def sent (parts : Parts) (openBrace oneJsonContainer : Bool) : Parts × Bool × Bool :=
  match wire parts openBrace oneJsonContainer with
  | .asIs => (parts, openBrace, oneJsonContainer)
  | .wrapped => (.one, true, true)

theorem what_is_sent_survives (parts : Parts) (openBrace oneJsonContainer : Bool) :
    let (p, b, j) := sent parts openBrace oneJsonContainer
    survives p b j = true := by
  cases parts <;> cases openBrace <;> cases oneJsonContainer <;> rfl

theorem text_that_survives_is_sent_as_it_is (parts : Parts) (openBrace oneJsonContainer : Bool)
    (h : survives parts openBrace oneJsonContainer = true) :
    wire parts openBrace oneJsonContainer = .asIs := by
  simp [wire, h]

theorem only_text_that_would_be_cut_is_wrapped (parts : Parts) (openBrace oneJsonContainer : Bool) :
    wire parts openBrace oneJsonContainer = .wrapped ↔
      survives parts openBrace oneJsonContainer = false := by
  cases parts <;> cases openBrace <;> cases oneJsonContainer <;> decide

theorem wrapping_is_idempotent (parts : Parts) (openBrace oneJsonContainer : Bool) :
    let (p, b, j) := sent parts openBrace oneJsonContainer
    wire p b j = .asIs := by
  cases parts <;> cases openBrace <;> cases oneJsonContainer <;> rfl

theorem a_sentence_then_an_object_is_wrapped :
    wire .one true false = .wrapped := rfl

theorem text_without_braces_is_never_touched (parts : Parts) (oneJsonContainer : Bool) :
    wire parts false oneJsonContainer = .asIs := by
  cases parts <;> rfl

end ToolResultText
