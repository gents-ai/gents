import Proofs.Conformance.ContractCases.Types
import Proofs.PromptAssembly

/-!
# PromptAssembly contract cases

Witness rows for the provider-input sanitizer, the assembled layer order, and
tool-argument repair.

**Every expected value in this file is computed by running the Lean model**, not
written by hand. `expected` is literally `Provider.sanitizeForProvider input`;
`expectedTwice` is the model applied twice; `splits` are the model applied to
each suffix. That is what makes the Rust fence mechanical rather than social: a
change to either the model or the Rust sanitizer breaks the equality, and no
human transcription sits in between.

Provider-validity of each expected output is *not* emitted as data, because it
is a theorem — `Provider.sanitizeForProvider_sound`. If production reproduces
the emitted output exactly, it inherits that validity. This is why the Rust
fence needs no hand-rolled pairing oracle.
-/

namespace Conformance.ContractCases

open PromptAssembly.Content (Item)
open PromptAssembly.Provider (ProviderRow)

/-- A content item, flattened for emission. `value` is the text/reasoning index
for `text`/`other`, and the tool-call id for `call`. -/
structure PromptAssemblyItemCase where
  item : String
  value : Nat
  deriving Repr

structure PromptAssemblyRowCase where
  role : String
  kind : String
  callIds : List Nat
  content : List PromptAssemblyItemCase
  deriving Repr

structure PromptAssemblySplitCase where
  index : Nat
  expected : List PromptAssemblyRowCase
  deriving Repr

structure PromptAssemblySanitizeCase where
  name : String
  input : List PromptAssemblyRowCase
  expected : List PromptAssemblyRowCase
  expectedTwice : List PromptAssemblyRowCase
  splits : List PromptAssemblySplitCase
  deriving Repr

structure PromptAssemblyLayerCase where
  name : String
  skillCount : Nat
  summaryCount : Nat
  conversationLen : Nat
  slots : List String
  deriving Repr

structure PromptAssemblyRepairCase where
  name : String
  input : String
  expected : String
  expectedTwice : String
  payloadOnly : Bool
  deriving Repr

/-! ## Building witness rows -/

private def itemCase : Item → PromptAssemblyItemCase
  | .text index => { item := "text", value := index }
  | .other index => { item := "other", value := index }
  | .call callId => { item := "call", value := callId }

/-- The announced call ids, read off the content in content order.

`Finset` has no computable ordered projection here (`Finset.toList` is
noncomputable and the contract generator must actually run), so the emitted list
comes from the content instead. Coherence makes that the same set: for an
assistant row, `Content.callsOf content = callIds` by `Coherent`, which
`witnessesAreCoherent` below discharges for every input witness and
`Provider.allCoherent_sanitizeForProvider` propagates to every output. -/
private def announcedIds (content : List Item) : List Nat :=
  content.filterMap Item.callId?

private def roleName : Transcript.MessageRole → String
  | .user => "user"
  | .assistant => "assistant"

private def rowCase (pr : ProviderRow) : PromptAssemblyRowCase :=
  match pr.row.kind with
  | .ordinary =>
    { role := roleName pr.row.role
    , kind := "ordinary"
    , callIds := []
    , content := pr.content.map itemCase }
  | .assistantToolCalls _ =>
    { role := roleName pr.row.role
    , kind := "assistantToolCalls"
    , callIds := announcedIds pr.content
    , content := pr.content.map itemCase }
  | .toolResult callId _ =>
    { role := roleName pr.row.role
    , kind := "toolResult"
    , callIds := [callId]
    , content := pr.content.map itemCase }

private def rowCases (rows : List ProviderRow) : List PromptAssemblyRowCase :=
  rows.map rowCase

/-! ## The witness transcripts

Constructed so that every row is `Coherent`: an assistant row's announced call
set is exactly the set of `call` items in its content. -/

private def mkRow (sequence : Nat) (role : Transcript.MessageRole)
    (kind : Transcript.MessageKind) (content : List Item) : ProviderRow :=
  { row := ⟨sequence, 0, sequence, role, kind⟩, content := content }

/-- An assistant turn announcing `ids`, optionally carrying prose first. -/
private def assistantCalls (sequence : Nat) (ids : List Nat)
    (prose : List Item := []) : ProviderRow :=
  mkRow sequence .assistant (.assistantToolCalls ids.toFinset)
    (prose ++ ids.map Item.call)

/-- A tool result closing `callId`. Production threads exactly one result per
user message (`loop_stream.rs`), so one row is one result. -/
private def toolResult (sequence : Nat) (callId : Nat) : ProviderRow :=
  mkRow sequence .user (.toolResult callId ⟨0, callId, 0⟩) []

/-- Ordinary user prose. -/
private def userText (sequence : Nat) (index : Nat) : ProviderRow :=
  mkRow sequence .user .ordinary [Item.text index]

/-- Assistant prose with no tool calls. -/
private def assistantText (sequence : Nat) (index : Nat) : ProviderRow :=
  mkRow sequence .assistant .ordinary [Item.text index]

private def witnessTranscripts : List (String × List ProviderRow) :=
  [ ("empty", [])
  , ("clean-paired-turn",
      [ userText 0 0
      , assistantCalls 1 [1, 2]
      , toolResult 2 1
      , toolResult 3 2
      , userText 4 1 ])
  , ("orphaned-result-before-its-call",
      [ toolResult 0 1
      , assistantCalls 1 [1] ])
  , ("unpaired-call-is-dropped",
      [ userText 0 0
      , assistantCalls 1 [1, 2]
      , toolResult 2 1
      , assistantCalls 3 [3] ])
  , ("result-after-conversation-resumes",
      [ assistantCalls 0 [1]
      , userText 1 0
      , toolResult 2 1 ])
  , ("loop-threaded-turn-is-a-fixpoint",
      [ assistantCalls 0 [1, 2]
      , toolResult 1 1
      , toolResult 2 2 ])
  , -- The case the row-only model cannot express: assistant prose rides along
    -- with a call that never resolved. Production keeps the message and its
    -- prose; the row is demoted to `.ordinary`.
    ("assistant-prose-survives-its-unpaired-call",
      [ userText 0 0
      , assistantCalls 1 [1] [Item.text 7]
      , userText 2 1 ])
  , ("assistant-prose-with-mixed-paired-and-unpaired-calls",
      [ assistantCalls 0 [1, 2] [Item.text 7, Item.other 8]
      , toolResult 1 1 ])
  , -- Content arriving out of canonical order: text after the calls. Stage 3
    -- reorders it; the announced call set is unchanged.
    ("content-order-is-normalized",
      [ mkRow 0 .assistant (.assistantToolCalls [1].toFinset)
          [Item.call 1, Item.other 5, Item.text 6]
      , toolResult 1 1 ])
  , -- Empty messages: Rust drops them, asymmetrically across the two stages
    -- (an empty user message goes in stage 1, an empty assistant message is
    -- carried through and pruned in stage 2). The row-only model kept both.
    ("empty-messages-are-dropped",
      [ mkRow 0 .assistant .ordinary []
      , mkRow 1 .user .ordinary []
      , userText 2 0 ])
  , -- An empty message does *not* end the active turn: Rust clears pending
    -- calls only on plain content. The pair must survive it intact.
    ("empty-message-does-not-break-an-open-turn",
      [ assistantCalls 0 [1]
      , mkRow 1 .user .ordinary []
      , toolResult 2 1 ])
  , ("empty-assistant-message-between-paired-turns",
      [ assistantCalls 0 [1]
      , toolResult 1 1
      , mkRow 2 .assistant .ordinary []
      , userText 3 0 ])
  , ("interleaved-blocks",
      [ assistantCalls 0 [1]
      , toolResult 1 1
      , assistantText 2 3
      , assistantCalls 3 [2, 4]
      , toolResult 4 2
      , toolResult 5 4
      , userText 6 9 ])
  ]

/-- Every witness row is `Coherent` — its announced call set is exactly the
`call` items in its content. This is what licenses `announcedIds` to read the
emitted call ids off the content, and it is the hypothesis
`Provider.sanitizeForProvider_sound` and `_idempotent` need. Checked by
`decide`, so a witness that drifts out of coherence fails the build. -/
theorem witnessesAreCoherent :
    ∀ witness ∈ witnessTranscripts, PromptAssembly.Provider.AllCoherent witness.2 := by
  decide

/-- The *other* premise of `Provider.sanitizeForProvider_sound` and
`_idempotent`. Without this the contract's claim — that production reproducing
an emitted output thereby inherits provider-validity — does not actually follow,
because the theorem would be quantified over inputs the witnesses need not
satisfy. Checked by `decide`, so a witness that reuses a call id across rows
fails the build. -/
theorem witnessesHaveUniqueCallIds :
    ∀ witness ∈ witnessTranscripts,
      PromptAssembly.UniqueCallIds
        (PromptAssembly.Provider.project witness.2) := by
  decide

/-- Both premises together, so the soundness the emitted rows rest on is
discharged for every witness rather than asserted in a comment. -/
theorem witnessOutputsAreProviderValid :
    ∀ witness ∈ witnessTranscripts,
      PromptAssembly.ProviderValid
        (PromptAssembly.Provider.project
          (PromptAssembly.Provider.sanitizeForProvider witness.2)) := by
  intro witness hwitness
  exact PromptAssembly.Provider.sanitizeForProvider_sound
    (witnessesHaveUniqueCallIds witness hwitness)
    (witnessesAreCoherent witness hwitness)

private def splitCases (rows : List ProviderRow) : List PromptAssemblySplitCase :=
  (List.range (rows.length + 1)).map fun index =>
    { index := index
    , expected := rowCases (PromptAssembly.Provider.sanitizeForProvider (rows.drop index)) }

private def sanitizeCase (witness : String × List ProviderRow) :
    PromptAssemblySanitizeCase :=
  let rows := witness.2
  let once := PromptAssembly.Provider.sanitizeForProvider rows
  { name := witness.1
  , input := rowCases rows
  , expected := rowCases once
  , expectedTwice := rowCases (PromptAssembly.Provider.sanitizeForProvider once)
  , splits := splitCases rows }

def promptAssemblySanitizeCases : List PromptAssemblySanitizeCase :=
  witnessTranscripts.map sanitizeCase

/-! ## Layer order

Emitted from `PromptAssembly.Template.assembleWithContext`, whose
`assembleWithContext_tail` theorem fixes the tail as `contextPreamble, prompt`. -/

private def slotName : PromptAssembly.Slot → String
  | .preamble => "preamble"
  | .summaryReminder => "summaryReminder"
  | .skillReminder index => s!"skillReminder:{index}"
  | .conversation index => s!"conversation:{index}"
  | .contextPreamble => "contextPreamble"
  | .prompt => "prompt"

private def layerShapes : List (String × Nat × Nat × Nat) :=
  [ ("bare", 0, 0, 0)
  , ("conversation-only", 0, 0, 3)
  , ("summary-and-conversation", 0, 1, 2)
  , ("skills-summary-and-conversation", 2, 1, 2)
  , ("skills-only", 3, 0, 0)
  ]

def promptAssemblyLayerCases : List PromptAssemblyLayerCase :=
  layerShapes.map fun shape =>
    { name := shape.1
    , skillCount := shape.2.1
    , summaryCount := shape.2.2.1
    , conversationLen := shape.2.2.2
    , slots :=
        (PromptAssembly.Template.assembleWithContext
          shape.2.1 shape.2.2.1 shape.2.2.2).map slotName }

/-! ## Tool-argument repair

Emitted from `PromptAssembly.ToolArgs.repairArgs`, fencing
`repair_is_payload_only` (repair rewrites argument payloads only) and
`repair_idempotent` (a second pass is a no-op). -/

/-- A stand-in payload type whose leaf sanitizer is idempotent, matching the
shape of the Rust repair: normalize to an object, then sanitize leaves. -/
private inductive Payload where
  | empty
  | raw
  | sanitized
  deriving DecidableEq, Repr

private def sanitizePayload : Payload → Payload
  | .empty => .empty
  | .raw => .sanitized
  | .sanitized => .sanitized

private instance : PromptAssembly.LeafSanitizer Payload where
  sanitize := sanitizePayload
  idempotent := by intro p; cases p <;> rfl

private def argsName : PromptAssembly.ToolArgs Payload → String
  | .object .empty => "object:empty"
  | .object .raw => "object:raw"
  | .object .sanitized => "object:sanitized"
  | .str none => "str:unparsed"
  | .str (some .empty) => "str:object:empty"
  | .str (some .raw) => "str:object:raw"
  | .str (some .sanitized) => "str:object:sanitized"
  | .array => "array"
  | .scalar => "scalar"
  | .null => "null"

private def repairVectors : List (String × PromptAssembly.ToolArgs Payload) :=
  [ ("object-passes-through", .object .raw)
  , ("empty-object-passes-through", .object .empty)
  , ("stringified-object-salvages", .str (some .raw))
  , ("unparsable-string-collapses", .str none)
  , ("array-collapses", .array)
  , ("scalar-collapses", .scalar)
  , ("null-collapses", .null)
  ]

/-- Whether the repair rewrote only the payload — i.e. the result is an object
whose payload is the leaf-sanitized original. True exactly on object inputs,
which is what `repair_is_payload_only` states. -/
private def isPayloadOnly : PromptAssembly.ToolArgs Payload → Bool
  | .object _ => true
  | _ => false

def promptAssemblyRepairCases : List PromptAssemblyRepairCase :=
  repairVectors.map fun vector =>
    let once := PromptAssembly.repairArgs Payload.empty vector.2
    { name := vector.1
    , input := argsName vector.2
    , expected := argsName once
    , expectedTwice := argsName (PromptAssembly.repairArgs Payload.empty once)
    , payloadOnly := isPayloadOnly vector.2 }

end Conformance.ContractCases
