import Proofs.Transcript.State
import Proofs.CanonicalOutput.Message

/-!
# Claude content-block map (Track B / B2)

Anthropic Messages HTTP is the only Claude wire. Its `tool_use` /
`tool_result` content blocks are mapped here onto existing `ToolCallId`s /
`MessageKind` rows. This file does not model the HTTP transport, the agent's subscription credential
token read, or oat.

Unmapped names (including Claude-native `Bash`) and `tool_use` on an empty
gents surface fail closed: the block does not become a call. No aliases.
Text-only turns (no `tool_use`) succeed even on an empty surface.

Id identity: a successful map returns the same `ToolCallId` it was given.
The runtime's `toolu_*` → `Nat` injection is a separate plumbing obligation
(`UniqueCallIds`); this map does not remap.
-/

namespace PromptAssembly.ClaudeMap

/-- Only explicit supported effort is emitted by this mapper; absent settings
emit no thinking configuration. Provider defaults and capability policy are
outside this mapper. Catalog support is an adapter fact; the serializer narrows
to the provider's effort vocabulary. -/
def selectedEffort (supported : Option (List String)) (requested : Option String) : Option String :=
  supported.bind fun choices =>
    requested.filter (fun value => value ∈ choices && value ∈ ["low", "medium", "high", "xhigh", "max"])

theorem absent_effort_omitted (supported : Option (List String)) :
    selectedEffort supported none = none := by
  cases supported <;> simp [selectedEffort]

theorem unknown_support_omitted : selectedEffort none (some "high") = none := rfl

theorem unsupported_effort_omitted : selectedEffort (some []) (some "high") = none := by
  native_decide

theorem supported_effort_preserved :
    selectedEffort (some ["low", "medium", "high", "xhigh", "max"]) (some "high") = some "high" := by
  native_decide

open ToolExecution (ToolCallId)
open Transcript (MessageKind)

/-- One Claude content block. `name` is the on-the-wire tool name. -/
inductive Block where
  | text
  | toolUse (id : ToolCallId) (name : String)
  | toolResult (id : ToolCallId)
  deriving DecidableEq, Repr

/-- Gents names advertised this turn. Empty = A2b text-only fence. -/
abbrev Surface := Finset String

inductive MapError where
  | emptySurface
  | unmappedName (name : String)
  | duplicateId (id : ToolCallId)
  | overlappingBlock (id : ToolCallId)
  | wrongBlock
  | wrongIndex (index : Nat)
  | incompleteBlock
  | signatureOrder
  | missingSignature
  | malformedRedacted
  | unsupportedReasoning
  | unsupportedReplayBlock
  deriving DecidableEq, Repr

def errorName : MapError → String
  | .emptySurface => "emptySurface"
  | .unmappedName name => "unmappedName:" ++ name
  | .duplicateId id => "duplicateId:" ++ toString id
  | .overlappingBlock id => "overlappingBlock:" ++ toString id
  | .wrongBlock => "wrongBlock"
  | .wrongIndex index => "wrongIndex:" ++ toString index
  | .incompleteBlock => "incompleteBlock"
  | .signatureOrder => "signatureOrder"
  | .missingSignature => "missingSignature"
  | .malformedRedacted => "malformedRedacted"
  | .unsupportedReasoning => "unsupportedReasoning"
  | .unsupportedReplayBlock => "unsupportedReplayBlock"

def blockTag : Block → String
  | .text => "text"
  | .toolUse id name => s!"toolUse:{id}:{name}"
  | .toolResult id => s!"toolResult:{id}"

/-- Map one `tool_use`. Empty surface or a name not on it fails closed. -/
def mapToolUse (surface : Surface) (id : ToolCallId) (name : String) :
    Except MapError ToolCallId :=
  if surface = ∅ then
    .error .emptySurface
  else if name ∈ surface then
    .ok id
  else
    .error (.unmappedName name)

theorem mapToolUse_empty (id : ToolCallId) (name : String) :
    mapToolUse ∅ id name = .error .emptySurface := by
  simp [mapToolUse]

theorem mapToolUse_ok_mem {surface : Surface} {id : ToolCallId} {name : String}
    (h : mapToolUse surface id name = .ok id) : name ∈ surface := by
  unfold mapToolUse at h
  by_cases hEmpty : surface = ∅
  · simp [hEmpty] at h
  · by_cases hMem : name ∈ surface
    · exact hMem
    · simp [hEmpty, hMem] at h

theorem mapToolUse_ok_nonempty {surface : Surface} {id : ToolCallId} {name : String}
    (h : mapToolUse surface id name = .ok id) : surface ≠ ∅ := by
  intro hempty
  simp [mapToolUse, hempty] at h

theorem mapToolUse_preserves_id {surface : Surface} {id id' : ToolCallId}
    {name : String} (h : mapToolUse surface id name = .ok id') : id' = id := by
  unfold mapToolUse at h
  by_cases hEmpty : surface = ∅
  · simp [hEmpty] at h
  · by_cases hMem : name ∈ surface
    · simp [hEmpty, hMem] at h
      exact h.symm
    · simp [hEmpty, hMem] at h

theorem mapToolUse_unmapped {surface : Surface} {id : ToolCallId} {name : String}
    (hne : surface ≠ ∅) (hmem : name ∉ surface) :
    mapToolUse surface id name = .error (.unmappedName name) := by
  simp [mapToolUse, hne, hmem]

/-- `Bash` is not `bash`. No aliases. -/
theorem mapToolUse_no_bash_alias (id : ToolCallId) :
    mapToolUse {("bash" : String)} id "Bash" = .error (.unmappedName "Bash") := by
  apply mapToolUse_unmapped
  · simp
  · simp

def toolUsePairs : List Block → List (ToolCallId × String)
  | [] => []
  | .toolUse id name :: rest => (id, name) :: toolUsePairs rest
  | _ :: rest => toolUsePairs rest

def mapPairs (surface : Surface) :
    List (ToolCallId × String) → Except MapError (Finset ToolCallId)
  | [] => .ok ∅
  | (id, name) :: rest =>
    match mapPairs surface rest with
    | .error e => .error e
    | .ok acc =>
      if id ∈ acc then
        .error (.duplicateId id)
      else
        match mapToolUse surface id name with
        | .error e => .error e
        | .ok _ => .ok (insert id acc)

/-- Assistant-turn map: `tool_use` blocks → native call-id set, or fail closed. -/
def mapTurn (surface : Surface) (blocks : List Block) :
    Except MapError (Finset ToolCallId) :=
  mapPairs surface (toolUsePairs blocks)

theorem mapTurn_text_only (surface : Surface) :
    mapTurn surface [.text] = .ok ∅ := rfl

theorem mapTurn_empty_surface_text :
    mapTurn ∅ [.text] = .ok ∅ := rfl

theorem mapTurn_empty_surface_tool_use (id : ToolCallId) :
    mapTurn ∅ [.toolUse id "bash"] = .error .emptySurface := by
  simp [mapTurn, mapPairs, toolUsePairs, mapToolUse]

theorem mapTurn_mapped (name : String) (id : ToolCallId) :
    name ∈ ({name} : Surface) →
    mapTurn {name} [.toolUse id name] = .ok {id} := by
  intro hmem
  simp [mapTurn, mapPairs, toolUsePairs, mapToolUse, hmem]

theorem mapTurn_unmapped (id : ToolCallId) :
    mapTurn {("bash" : String)} [.toolUse id "Bash"] =
      .error (.unmappedName "Bash") := by
  simp [mapTurn, mapPairs, toolUsePairs, mapToolUse_no_bash_alias]

theorem mapTurn_duplicate (id : ToolCallId) :
    mapTurn {("bash" : String)}
        [.toolUse id "bash", .toolUse id "bash"] =
      .error (.duplicateId id) := by
  have hmem : ("bash" : String) ∈ ({("bash" : String)} : Surface) := by simp
  simp [mapTurn, mapPairs, toolUsePairs, mapToolUse, hmem]

/-- A successful turn's call ids are exactly the `tool_use` ids, as `MessageKind`. -/
def mappedKind (ids : Finset ToolCallId) : MessageKind :=
  if ids = ∅ then .ordinary else .assistantToolCalls ids

theorem mappedKind_text : mappedKind ∅ = .ordinary := by
  simp [mappedKind]

theorem mappedKind_calls (id : ToolCallId) :
    mappedKind {id} = .assistantToolCalls {id} := by
  simp [mappedKind]

/-! ## System assembly (single-wire Messages HTTP)

`Transcript.MessageRole` has no `system`; the wire-side row type here carries
just what assembly needs: a `System` row's text, or "some other row". -/

inductive Msg where
  | system (text : String)
  | other (tag : String)
  deriving DecidableEq, Repr

/-- The Claude Code identity block. `system[0]` on every request; the agent's
subscription credential routes on it. Checked against Rust `CLAUDE_CODE_IDENTITY`
by the vocab test. -/
def identity : String := "You are Claude Code, Anthropic's official CLI for Claude."

/-- Pull `System` rows out in order; everything else is untouched. Rust also
trims a whitespace-only preamble and drops blank `System` rows before this
split; the model does not represent that and no witness covers it. -/
def splitSystem : List Msg → List String × List Msg
  | [] => ([], [])
  | .system t :: rest =>
    let (sys, others) := splitSystem rest
    (t :: sys, others)
  | m :: rest =>
    let (sys, others) := splitSystem rest
    (sys, m :: others)

/-- `system[]` on the wire: identity first, then the preamble, then the `System`
rows verbatim (blank-row dropping happens in Rust before `rows` is built). -/
def systemBlocks (preamble : Option String) (rows : List String) : List String :=
  identity :: (preamble.toList ++ rows)

theorem systemBlocks_head (preamble : Option String) (rows : List String) :
    (systemBlocks preamble rows).head? = some identity := rfl

theorem systemBlocks_tail_verbatim (preamble : Option String) (rows : List String) :
    (systemBlocks preamble rows).tail = preamble.toList ++ rows := rfl

def isSystem : Msg → Bool
  | .system _ => true
  | .other _ => false

/-- The remaining list contains no `System` row, and the split loses nothing:
the system texts are exactly the `System` rows in order. -/
theorem splitSystem_partition (msgs : List Msg) :
    (splitSystem msgs).2.all (fun m => !isSystem m) = true ∧
    (splitSystem msgs).1 = (msgs.filter isSystem).map (fun m =>
      match m with | .system t => t | .other _ => "") ∧
    (splitSystem msgs).2 = msgs.filter (fun m => !isSystem m) := by
  induction msgs with
  | nil => simp [splitSystem]
  | cons m rest ih =>
    obtain ⟨h1, h2, h3⟩ := ih
    cases m with
    | system t => simp [splitSystem, isSystem, List.filter, h1, h2, h3]
    | other tag => simp [splitSystem, isSystem, List.filter, h1, h2, h3]

/-- `tools` is absent from the wire for an empty surface. -/
def toolsField : List String → Option (List String)
  | [] => none
  | tools => some tools

theorem toolsField_empty : toolsField [] = none := rfl

theorem toolsField_nonempty (t : String) (rest : List String) :
    toolsField (t :: rest) = some (t :: rest) := rfl

/-! ## Tool-block accumulation (SSE)

One `tool_use` block arrives as `content_block_start` (with a usually-empty
`input`), zero or more `input_json_delta` fragments, and `content_block_stop`.
Defect C1 seeded the start input and appended deltas (`{}{...}`). Here the
deltas are the arguments whenever any arrived. -/

inductive StreamEvent where
  | text (t : String)
  | start (id : ToolCallId) (name : String) (input : Option String)
  | delta (fragment : String)
  | stop
  | thinkingStart (index : Nat) (initialText : String)
  | thinkingDelta (index : Nat) (fragment : String)
  | signatureDelta (index : Nat) (fragment : String)
  | redactedStart (index : Nat) (data : String)
  | contentStop (index : Nat)
  deriving DecidableEq, Repr

def accumulate (start : Option String) (deltas : List String) : String :=
  match deltas with
  | [] => start.getD "{}"
  | _ => String.join deltas

theorem accumulate_ignores_start_when_streamed (start : Option String)
    (deltas : List String) (h : deltas ≠ []) :
    accumulate start deltas = String.join deltas := by
  cases deltas with
  | nil => exact absurd rfl h
  | cons d rest => rfl

theorem accumulate_uses_start_when_no_deltas (start : Option String) :
    accumulate start [] = start.getD "{}" := rfl

structure Pending where
  id : ToolCallId
  name : String
  start : Option String
  /-- Deltas in reverse arrival order (consed); flushed as `deltas.reverse`. -/
  deltas : List String
  deriving Repr

/-- One ordered native-content observation from the same SSE fold that maps
tool calls. Reasoning uses the canonical native part vocabulary. Omitted
thinking text may be empty, but its opaque signature must still be retained.
Anthropic streams signature deltas before block stop and requires unchanged
thinking/redacted blocks on tool continuation. -/
inductive StreamBlock where
  | text (value : String)
  | reasoning (parts : List (CanonicalOutput.ReasoningPart String))
  | toolUse (id : ToolCallId) (name arguments : String)
  deriving DecidableEq, Repr

inductive PendingBlock where
  | tool (pending : Pending)
  | thinking (index : Nat) (textFragments signatureFragments : List String)
      (signatureStarted : Bool)
  | redacted (index : Nat) (data : String)
  deriving Repr

structure StreamState where
  pending : Option PendingBlock
  /-- Only reasoning events carry SSE indices in this bounded projection;
  legacy text/tool events do not establish a global block-index sequence. -/
  lastIndexedBlock : Option Nat
  seen : List ToolCallId
  out : List (ToolCallId × String)
  content : List StreamBlock
  deriving Repr

def StreamState.init : StreamState :=
  { pending := none, lastIndexedBlock := none, seen := [], out := [], content := [] }

/-- Legacy tool blocks may flush at EOF (duplicate id, surface, arguments).
Thinking/redacted blocks must close explicitly before they become replayable. -/
def flush (surface : Surface) (st : StreamState) : Except MapError StreamState :=
  match st.pending with
  | none => .ok st
  | some (.thinking ..) | some (.redacted ..) => .error .incompleteBlock
  | some (.tool p) =>
    if p.id ∈ st.seen then
      .error (.duplicateId p.id)
    else
      match mapToolUse surface p.id p.name with
      | .error e => .error e
      | .ok _ =>
        .ok { pending := none
            , lastIndexedBlock := st.lastIndexedBlock
            , seen := p.id :: st.seen
            , out := st.out ++ [(p.id, accumulate p.start p.deltas.reverse)]
            , content := st.content ++ [.toolUse p.id p.name (accumulate p.start p.deltas.reverse)] }

def finishContent (st : StreamState) (index : Nat) : Except MapError StreamState :=
  match st.pending with
  | some (.thinking current text signature _) =>
      if current != index then .error (.wrongIndex index)
      else
        let signed := String.join signature.reverse
        if signed == "" then .error .missingSignature
        else
          let cleared := { st with pending := none }
          let indexed := { cleared with lastIndexedBlock := some index }
          .ok { indexed with
            content := st.content ++ [.reasoning [.text (String.join text.reverse) (some signed)]] }
  | some (.redacted current data) =>
      if current != index then .error (.wrongIndex index)
      else if data == "" then .error .malformedRedacted
      else
        let cleared := { st with pending := none }
        let indexed := { cleared with lastIndexedBlock := some index }
        .ok { indexed with content := st.content ++ [.reasoning [.redacted data]] }
  | _ => .error .wrongBlock

def step (surface : Surface) (st : StreamState) : StreamEvent → Except MapError StreamState
  | .text value =>
      match st.pending with
      | some _ => .error .wrongBlock
      | none => .ok { st with content := st.content ++ [.text value] }
  | .start id name input =>
    match st.pending with
    | some _ => .error (.overlappingBlock id)
    | none => .ok { st with pending := some (.tool { id := id, name := name, start := input, deltas := [] }) }
  | .delta fragment =>
    match st.pending with
    | none => .ok st
    | some (.tool p) => .ok { st with pending := some (.tool { p with deltas := fragment :: p.deltas }) }
    | _ => .error .wrongBlock
  | .stop => flush surface st
  | .thinkingStart index initialText =>
      if st.pending.isSome then .error .wrongBlock
      else if st.lastIndexedBlock.any (index ≤ ·) then .error (.wrongIndex index)
      else .ok { st with pending := some (.thinking index [initialText] [] false) }
  | .thinkingDelta index fragment =>
      match st.pending with
      | some (.thinking current text signature started) =>
          if current != index then .error (.wrongIndex index)
          else if started then .error .signatureOrder
          else .ok { st with pending := some (.thinking current (fragment :: text) signature false) }
      | _ => .error .wrongBlock
  | .signatureDelta index fragment =>
      match st.pending with
      | some (.thinking current text signature _) =>
          if current != index then .error (.wrongIndex index)
          else .ok { st with pending := some (.thinking current text (fragment :: signature) true) }
      | _ => .error .wrongBlock
  | .redactedStart index data =>
      if st.pending.isSome then .error .wrongBlock
      else if st.lastIndexedBlock.any (index ≤ ·) then .error (.wrongIndex index)
      else .ok { st with pending := some (.redacted index data) }
  | .contentStop index => finishContent st index

/-- Left to right, first error wins; EOF flushes only a legacy tool block. -/
def runStream (surface : Surface) (events : List StreamEvent) :
    Except MapError (List (ToolCallId × String)) :=
  (events.foldlM (step surface) StreamState.init >>= flush surface) |>.map (·.out)

/-- The ordered native-content projection uses the very same fold and flush as
`runStream`; there is no second SSE acceptance policy. -/
structure ContentStep where
  provisionalThinking : Option String
  sealed : List StreamBlock
  deriving DecidableEq, Repr

def contentStep (st : StreamState) : ContentStep :=
  { provisionalThinking :=
      match st.pending with
      | some (.thinking _ fragments _ _) => some (String.join fragments.reverse)
      | _ => none
  , sealed := st.content }

/-- One traversal through `step` yields both provisional thinking previews and
the sealed native content. A signature changes the pending block, but does not
append a second text part or replace previously streamed bytes. -/
def runContentTrace (surface : Surface) (events : List StreamEvent) :
    Except MapError (List ContentStep × List StreamBlock) := do
  let (state, observations) ← events.foldlM (fun (state, observations) event => do
    let next ← step surface state event
    return (next, observations ++ [contentStep next])) (StreamState.init, [])
  let finished ← flush surface state
  return (observations, finished.content)

def runContentStream (surface : Surface) (events : List StreamEvent) :
    Except MapError (List StreamBlock) :=
  (runContentTrace surface events).map (·.2)

/-- Anthropic assistant continuation must replay each reconstructed signed or
redacted reasoning part unchanged and in order. These are native payload bytes,
not display text. Generic encrypted/summary parts are not Anthropic signatures
and cannot be relabeled as such. A missing signature is not synthesized. -/
inductive ReplayBlock where
  | text (payload : List UInt8)
  | signedThinking (payload : List UInt8) (signature : String)
  | redactedThinking (payload : List UInt8)
  | toolUse (id name : String) (arguments : List UInt8)
  deriving DecidableEq, Repr

def replayPart : CanonicalOutput.ReasoningPart (List UInt8) →
    Except MapError ReplayBlock
  | .text payload (some signature) =>
      if signature == "" then .error .missingSignature
      else .ok (.signedThinking payload signature)
  | .text _ none => .error .missingSignature
  | .redacted payload =>
      if payload.isEmpty then .error .malformedRedacted
      else .ok (.redactedThinking payload)
  | .encrypted _ | .summary _ => .error .unsupportedReasoning

def replayBlock : CanonicalOutput.MessageBlock (List UInt8) →
    Except MapError (List ReplayBlock)
  | .text payload =>
      if payload.isEmpty then .ok [] else .ok [.text payload]
  | .reasoning _ parts => parts.mapM replayPart
  | .toolCall _ id _ name arguments _ _ =>
      .ok [.toolUse id name arguments]
  | .toolResult .. | .media .. => .error .unsupportedReplayBlock

def replayBlocks (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    Except MapError (List ReplayBlock) := do
  return (← blocks.mapM replayBlock).flatten

theorem replay_signed_bytes_and_signature (payload : List UInt8) (signature : String)
    (nonempty : signature ≠ "") :
    replayPart (.text payload (some signature)) =
      .ok (.signedThinking payload signature) := by
  simp [replayPart, nonempty]

theorem replay_redacted_bytes (payload : List UInt8) (nonempty : payload ≠ []) :
    replayPart (.redacted payload) = .ok (.redactedThinking payload) := by
  cases payload with
  | nil => contradiction
  | cons _ _ => rfl

theorem replay_reasoning_parts_in_order
    (parts : List (CanonicalOutput.ReasoningPart (List UInt8))) :
    replayBlocks [.reasoning none parts] = parts.mapM replayPart := by
  simp [replayBlocks, replayBlock]

theorem replay_empty_ordinary_text_is_omitted :
    replayBlocks [.text []] = .ok [] := by
  rfl

theorem replay_assistant_media_fails_closed :
    replayBlocks [.media { kind := .image, data := .unknown }] =
      .error .unsupportedReplayBlock := by
  rfl

/-- `Except` ships no `DecidableEq`; the `runStream` witnesses below decide
equality on `Except MapError (List (ToolCallId × String))`. -/
local instance instDecidableEqExcept {ε α : Type} [DecidableEq ε] [DecidableEq α] :
    DecidableEq (Except ε α)
  | .error a, .error b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.error.inj e))
  | .error _, .ok _ => .isFalse nofun
  | .ok _, .error _ => .isFalse nofun
  | .ok a, .ok b =>
    if h : a = b then .isTrue (h ▸ rfl) else .isFalse (fun e => h (Except.ok.inj e))

theorem runStream_text_only (surface : Surface) :
    runStream surface [.text "hi"] = .ok [] := rfl

theorem runStream_deltas_win :
    runStream {("echo" : String)}
      [.start 1 "echo" (some "{}"), .delta "{\"text\":", .delta " \"hi\"}", .stop] =
      .ok [(1, "{\"text\": \"hi\"}")] := by
  native_decide

theorem runStream_start_input_without_deltas :
    runStream {("echo" : String)} [.start 1 "echo" (some "{\"a\":1}"), .stop] =
      .ok [(1, "{\"a\":1}")] := by
  native_decide

theorem runStream_overlap :
    runStream {("echo" : String)} [.start 1 "echo" none, .start 2 "echo" none, .stop] =
      .error (.overlappingBlock 2) := by
  native_decide

theorem runStream_duplicate :
    runStream {("echo" : String)}
      [.start 1 "echo" none, .stop, .start 1 "echo" none, .stop] =
      .error (.duplicateId 1) := by
  native_decide

theorem runStream_unterminated_flushes :
    runStream {("echo" : String)} [.start 1 "echo" none, .delta "{}"] = .ok [(1, "{}")] := by
  native_decide

theorem signed_thinking_fragments_seal_once :
    runContentStream {} [.thinkingStart 0 "", .thinkingDelta 0 "考",
      .thinkingDelta 0 "慮", .signatureDelta 0 "署", .signatureDelta 0 "名",
      .contentStop 0] =
      .ok [.reasoning [.text "考慮" (some "署名")]] := by
  native_decide

theorem provisional_thinking_keeps_previous_bytes :
    (runContentTrace {} [.thinkingStart 0 "", .thinkingDelta 0 "考",
      .thinkingDelta 0 "慮", .signatureDelta 0 "署", .signatureDelta 0 "名",
      .contentStop 0]).map (fun result => result.1.map (·.provisionalThinking)) =
      .ok [some "", some "考", some "考慮", some "考慮", some "考慮", none] := by
  native_decide

theorem empty_thinking_text_keeps_signature :
    runContentStream {} [.thinkingStart 0 "", .signatureDelta 0 "sig", .contentStop 0] =
      .ok [.reasoning [.text "" (some "sig")]] := by
  native_decide

theorem initial_thinking_text_precedes_deltas :
    runContentStream {} [.thinkingStart 0 "初", .thinkingDelta 0 "続",
      .signatureDelta 0 "sig", .contentStop 0] =
      .ok [.reasoning [.text "初続" (some "sig")]] := by
  native_decide

theorem initial_thinking_text_is_provisional :
    (runContentTrace {} [.thinkingStart 0 "初", .thinkingDelta 0 "続",
      .signatureDelta 0 "sig", .contentStop 0]).map
      (fun result => result.1.map (·.provisionalThinking)) =
      .ok [some "初", some "初続", some "初続", none] := by
  native_decide

theorem redacted_before_tool_keeps_order :
    runContentStream {("echo" : String)}
      [.redactedStart 0 "opaque", .contentStop 0,
       .start 1 "echo" none, .delta "{}", .stop] =
      .ok [.reasoning [.redacted "opaque"], .toolUse 1 "echo" "{}"] := by
  native_decide

theorem unsigned_thinking_does_not_seal :
    runContentStream {} [.thinkingStart 0 "", .thinkingDelta 0 "text", .contentStop 0] =
      .error .missingSignature := by
  native_decide

theorem thinking_eof_is_not_tool_eof_flush :
    runContentStream {} [.thinkingStart 0 "", .signatureDelta 0 "sig"] =
      .error .incompleteBlock := by
  native_decide

theorem replay_preserves_ordered_reasoning_parts :
    replayBlocks [.reasoning none [.text [65] (some "sig"), .redacted [66]],
      .toolCall 1 "call-1" none "echo" [123, 125] none none] =
      .ok [.signedThinking [65] "sig", .redactedThinking [66],
           .toolUse "call-1" "echo" [123, 125]] := by
  native_decide

theorem replay_rejects_generic_encrypted :
    replayBlocks [.reasoning none [.encrypted [65]]] = .error .unsupportedReasoning := by
  native_decide

theorem replay_rejects_empty_signature :
    replayBlocks [.reasoning none [.text [] (some "")]] = .error .missingSignature := by
  native_decide

theorem replay_rejects_empty_redacted :
    replayBlocks [.reasoning none [.redacted []]] = .error .malformedRedacted := by
  native_decide

end PromptAssembly.ClaudeMap
