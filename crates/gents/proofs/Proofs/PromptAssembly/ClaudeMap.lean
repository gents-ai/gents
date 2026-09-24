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
  | missingContinuationOrigin
  | foreignContinuationOrigin
  | ambiguousContinuationOrigin
  | missingReasoningWitness
  | alteredReasoning
  | invalidReplayAssociation
  | duplicateReplayAssociation
  | missingRequiredReplay
  | requiredReplayInPrefix
  | invalidReplaySplit
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
  | .missingContinuationOrigin => "missingContinuationOrigin"
  | .foreignContinuationOrigin => "foreignContinuationOrigin"
  | .ambiguousContinuationOrigin => "ambiguousContinuationOrigin"
  | .missingReasoningWitness => "missingReasoningWitness"
  | .alteredReasoning => "alteredReasoning"
  | .invalidReplayAssociation => "invalidReplayAssociation"
  | .duplicateReplayAssociation => "duplicateReplayAssociation"
  | .missingRequiredReplay => "missingRequiredReplay"
  | .requiredReplayInPrefix => "requiredReplayInPrefix"
  | .invalidReplaySplit => "invalidReplaySplit"

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
  provisionalSignature : Option String
  provisionalRedacted : Option String
  sealed : List StreamBlock
  deriving DecidableEq, Repr

def contentStep (st : StreamState) : ContentStep :=
  { provisionalThinking :=
      match st.pending with
      | some (.thinking _ fragments _ _) => some (String.join fragments.reverse)
      | _ => none
  , provisionalSignature :=
      match st.pending with
      | some (.thinking _ _ fragments true) => some (String.join fragments.reverse)
      | _ => none
  , provisionalRedacted :=
      match st.pending with
      | some (.redacted _ data) => some data
      | _ => none
  , sealed := st.content }

private def foldContentTrace (surface : Surface) (events : List StreamEvent) :
    Except MapError (StreamState × List ContentStep) :=
  events.foldlM (fun (state, observations) event => do
    let next ← step surface state event
    return (next, observations ++ [contentStep next])) (StreamState.init, [])

/-- Only fully decoded stream events enter this prefix. An abort does not
invoke EOF sealing or turn incomplete transport JSON into a typed part. -/
def runDecodedPrefixTrace (surface : Surface) (events : List StreamEvent) :
    Except MapError (List ContentStep × List StreamBlock) := do
  let (state, observations) ← foldContentTrace surface events
  return (observations, state.content)

/-- One traversal through `step` yields decoded provisional reasoning fields
and sealed native content. A signature changes the pending block, but does not
append a second text part or replace previously streamed bytes. -/
def runContentTrace (surface : Surface) (events : List StreamEvent) :
    Except MapError (List ContentStep × List StreamBlock) := do
  let (state, observations) ← foldContentTrace surface events
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

def replayBlocks : List (CanonicalOutput.MessageBlock (List UInt8)) →
    Except MapError (List ReplayBlock)
  | [] => .ok []
  | block :: rest =>
      match replayBlock block with
      | .error error => .error error
      | .ok first =>
          match replayBlocks rest with
          | .error error => .error error
          | .ok suffix => .ok (first ++ suffix)

/-- Recursive presentation is the original left-to-right `mapM` followed by
flattening, including which malformed block wins first. -/
theorem replayBlocks_mapM_equiv
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    replayBlocks blocks = (blocks.mapM replayBlock).map List.flatten := by
  induction blocks with
  | nil => rfl
  | cons block rest ih =>
      cases hb : replayBlock block with
      | error error =>
          simp [replayBlocks, hb, List.mapM_cons, Except.map, Except.bind]
          rfl
      | ok first =>
          cases ht : rest.mapM replayBlock with
          | error error =>
              simp [replayBlocks, ih, hb, ht, List.mapM_cons, Except.map, Except.bind]
              rfl
          | ok suffix =>
              simp [replayBlocks, ih, hb, ht, List.mapM_cons, Except.map, Except.bind]
              rfl

/-! ## Provenance-gated replay

The caller supplies the result of the canonical closing-segment → provider-turn
→ rendered-request join. This owner does not derive issuer from a signature,
model name, or Rig's history/prompt carrier. `expectedReasoning` is the exact
wire-relevant reasoning projection of the current canonical continuation:
ordered parts, bytes, signatures, and block positions, but not the native
reasoning-block ID (which Claude does not replay). It must come from the same
trusted owner as `origin`.
Restoring that evidence from a durable checkpoint is a native binding
obligation, not established by this pure model. Provenance and exact blocks
also do not prove provider acceptance if earlier system, tool, or message
prefixes change; provider capability/prefix policy is outside this owner
(#1693). Evidence is supplied for each selected row, potentially several
assistant rows in one ongoing tool-use turn; this owner does not prove upstream
row selection was complete. It imposes no universal thinking-first rule. -/

inductive ReplayUsage where
  | historical
  | requiredCurrent
  deriving DecidableEq, Repr

inductive ReplayOrigin where
  | claudeSubscription
  | foreign
  | missing
  | ambiguous
  deriving DecidableEq, Repr

abbrev ReasoningWitness := List (Nat × List (CanonicalOutput.ReasoningPart (List UInt8)))

structure ReplayInput where
  usage : ReplayUsage
  origin : ReplayOrigin
  expectedReasoning : Option ReasoningWitness
  blocks : List (CanonicalOutput.MessageBlock (List UInt8))
  deriving Repr

def stripHistoricalReasoning :
    List (CanonicalOutput.MessageBlock (List UInt8)) →
    List (CanonicalOutput.MessageBlock (List UInt8))
  | [] => []
  | .reasoning .. :: rest => stripHistoricalReasoning rest
  | block :: rest => block :: stripHistoricalReasoning rest

def reasoningProjectionFrom (index : Nat) :
    List (CanonicalOutput.MessageBlock (List UInt8)) → ReasoningWitness
  | [] => []
  | .reasoning _ parts :: rest =>
      (index, parts) :: reasoningProjectionFrom (index + 1) rest
  | _ :: rest => reasoningProjectionFrom (index + 1) rest

def reasoningProjection (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    ReasoningWitness :=
  reasoningProjectionFrom 0 blocks

/-- Historical reasoning is never sent to a different Claude continuation.
Required current reasoning is replayed only with a unique Claude provenance
join and exact wire-relevant reasoning witness. Existing `replayBlocks` supplies
strict signature, redaction, unsupported-kind and order checks. -/
def narrowReplay (input : ReplayInput) : Except MapError (List ReplayBlock) :=
  match input.usage with
  | .historical => replayBlocks (stripHistoricalReasoning input.blocks)
  | .requiredCurrent =>
      match input.origin with
      | .missing => .error .missingContinuationOrigin
      | .foreign => .error .foreignContinuationOrigin
      | .ambiguous => .error .ambiguousContinuationOrigin
      | .claudeSubscription =>
          match input.expectedReasoning with
          | none => .error .missingReasoningWitness
          | some expected =>
              if reasoningProjection input.blocks == expected then
                replayBlocks input.blocks
              else
                .error .alteredReasoning

def narrowReplayRows (inputs : List ReplayInput) :
    Except MapError (List (List ReplayBlock)) :=
  inputs.mapM narrowReplay

/-! ## Source-preserving reduction and restored continuation

The tag is an existing canonical provider-output coordinate, not a provider
message ID. Rows here are selected assistant occurrences only; user/tool-result
rows and the full provider request remain separate representation bindings.
The native owner must issue a tag from the accepted physical header and carry
it beside the row through checkpoint storage. A native reduction split must
retain each selected assistant occurrence's original source index, not merely
count assistant rows. `resolve` below is an
owner-supplied canonical header/close/capture lookup, not a proof of that DB
join. In particular, equal-byte rows with swapped tags cannot be detected by
this pure model; sidecar integrity remains a native obligation. -/

abbrev ReplayTag := CanonicalOutput.Coordinate

def providerReplayTag (tag : ReplayTag) : Bool :=
  match tag.source with
  | .provider .. => true
  | .auxiliary .. | .tool .. | .authored .. => false

structure TaggedReplayRow where
  source : Option ReplayTag
  blocks : List (CanonicalOutput.MessageBlock (List UInt8))
  deriving Repr

structure ResolvedReplayEvidence where
  origin : ReplayOrigin
  reasoning : ReasoningWitness
  deriving Repr

/-- Selection is the same order-preserving filter used by the source owner.
The independently supplied `required` set is never computed from this list. -/
def selectReplayRows (keep : TaggedReplayRow → Bool) (rows : List TaggedReplayRow) :
    List TaggedReplayRow :=
  rows.filter keep

/-- Argument repair changes only tool-call payload, not a row's coordinate or
the reasoning projection. This is the modeled part of request repair. -/
def repairReplayBlock (repair : List UInt8 → List UInt8) :
    CanonicalOutput.MessageBlock (List UInt8) →
    CanonicalOutput.MessageBlock (List UInt8)
  | .toolCall docId id callId name arguments signature additionalParams =>
      .toolCall docId id callId name (repair arguments) signature additionalParams
  | block => block

def repairReplayRows (repair : List UInt8 → List UInt8)
    (rows : List TaggedReplayRow) : List TaggedReplayRow :=
  rows.map fun row => { row with blocks := row.blocks.map (repairReplayBlock repair) }

theorem selectReplayRows_sublist (keep : TaggedReplayRow → Bool)
    (rows : List TaggedReplayRow) :
    List.Sublist (selectReplayRows keep rows) rows := by
  simpa only [selectReplayRows] using (List.filter_sublist (p := keep) rows)

theorem repairReplayRows_keeps_sources (repair : List UInt8 → List UInt8)
    (rows : List TaggedReplayRow) :
    (repairReplayRows repair rows).map TaggedReplayRow.source =
      rows.map TaggedReplayRow.source := by
  induction rows with
  | nil => rfl
  | cons row rest ih => simp [repairReplayRows, ih]

theorem repairReplayBlock_keeps_reasoning (repair : List UInt8 → List UInt8)
    (index : Nat) (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    reasoningProjectionFrom index (blocks.map (repairReplayBlock repair)) =
      reasoningProjectionFrom index blocks := by
  induction blocks generalizing index with
  | nil => rfl
  | cons block rest ih =>
      cases block <;> simp [repairReplayBlock, reasoningProjectionFrom, ih]

theorem repairReplayRows_keeps_reasoning (repair : List UInt8 → List UInt8)
    (row : TaggedReplayRow) :
    reasoningProjection (row.blocks.map (repairReplayBlock repair)) =
      reasoningProjection row.blocks := by
  exact repairReplayBlock_keeps_reasoning repair 0 row.blocks

def replayTagCount (tag : ReplayTag) (rows : List TaggedReplayRow) : Nat :=
  (rows.filter fun row => row.source == some tag).length

structure ReplayCheckpoint where
  required : List ReplayTag
  prefixRows : List TaggedReplayRow
  retained : List TaggedReplayRow
  deriving Repr

/-- Exact `take`/`drop` split. Every required current reasoning coordinate must
survive exactly once in the retained suffix; reducing it to the summary is not
implicit retirement. Historical rows have no capture requirement. -/
def prepareReplayCheckpoint (required : List ReplayTag)
    (rows : List TaggedReplayRow) (split : Nat) : Except MapError ReplayCheckpoint :=
  if split > rows.length then .error .invalidReplaySplit
  else if !(required.all providerReplayTag) ||
      rows.any (fun row => row.source.any (fun tag => !providerReplayTag tag)) then
    .error .invalidReplayAssociation
  else if required.length != required.eraseDups.length ||
      (rows.filterMap TaggedReplayRow.source).length !=
        (rows.filterMap TaggedReplayRow.source).eraseDups.length then
    .error .duplicateReplayAssociation
  else if required.any (fun tag => replayTagCount tag (rows.take split) != 0) then
    .error .requiredReplayInPrefix
  else if required.any (fun tag => replayTagCount tag (rows.drop split) != 1) then
    .error .missingRequiredReplay
  else
    .ok { required, prefixRows := rows.take split, retained := rows.drop split }

theorem preparedReplayCheckpoint_exact_split (required : List ReplayTag)
    (rows : List TaggedReplayRow) (split : Nat) (checkpoint : ReplayCheckpoint)
    (h : prepareReplayCheckpoint required rows split = .ok checkpoint) :
    checkpoint.prefixRows = rows.take split ∧ checkpoint.retained = rows.drop split := by
  unfold prepareReplayCheckpoint at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  cases h
  exact ⟨rfl, rfl⟩

def narrowResolvedReplay (tag : ReplayTag)
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8)))
    (resolve : ReplayTag → List ResolvedReplayEvidence) :
    Except MapError (List ReplayBlock) :=
  match resolve tag with
  | [] => .error .missingContinuationOrigin
  | [evidence] =>
      narrowReplay (ReplayInput.mk .requiredCurrent evidence.origin
        (some evidence.reasoning) blocks)
  | _ => .error .ambiguousContinuationOrigin

/-- Restore checks the carried association again, then calls the existing
strict replay owner once per row. Resolution is queried only for required
current coordinates. A historical row strips reasoning even if its old capture
is absent or foreign. The returned ordered payload is the one input for
estimation, capture and send at the native adapter; this assistant-only model
does not establish complete request assembly or equality of separately
parameterized serializers. -/
def restoreAndNarrowReplay (checkpoint : ReplayCheckpoint)
    (resolve : ReplayTag → List ResolvedReplayEvidence) :
    Except MapError (List (List ReplayBlock)) :=
  let rows := checkpoint.prefixRows ++ checkpoint.retained
  match prepareReplayCheckpoint checkpoint.required rows checkpoint.prefixRows.length with
  | .error error => .error error
  | .ok checked =>
      checked.retained.mapM fun row =>
        match row.source with
        | some tag =>
            if tag ∈ checkpoint.required then
              narrowResolvedReplay tag row.blocks resolve
            else
              narrowReplay (ReplayInput.mk .historical .missing none row.blocks)
        | none =>
            narrowReplay (ReplayInput.mk .historical .missing none row.blocks)

theorem required_replay_not_silently_removed (required : List ReplayTag)
    (rows : List TaggedReplayRow) (split : Nat) (checkpoint : ReplayCheckpoint)
    (h : prepareReplayCheckpoint required rows split = .ok checkpoint)
    (tag : ReplayTag) (htag : tag ∈ required) :
    replayTagCount tag checkpoint.prefixRows = 0 ∧
      replayTagCount tag checkpoint.retained = 1 := by
  unfold prepareReplayCheckpoint at h
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  split at h <;> try contradiction
  rename_i _ _ _ hprefix hsuffix
  cases h
  constructor
  · by_contra hnonzero
    apply hprefix
    simp only [List.any_eq_true]
    exact ⟨tag, htag, by simp [hnonzero]⟩
  · by_contra hnotone
    apply hsuffix
    simp only [List.any_eq_true]
    exact ⟨tag, htag, by simp [hnotone]⟩

theorem restored_success_retains_each_required_once
    (checkpoint : ReplayCheckpoint)
    (resolve : ReplayTag → List ResolvedReplayEvidence)
    (replay : List (List ReplayBlock))
    (h : restoreAndNarrowReplay checkpoint resolve = .ok replay)
    (tag : ReplayTag) (htag : tag ∈ checkpoint.required) :
    replayTagCount tag checkpoint.prefixRows = 0 ∧
      replayTagCount tag checkpoint.retained = 1 := by
  unfold restoreAndNarrowReplay at h
  cases hprepared : prepareReplayCheckpoint checkpoint.required
      (checkpoint.prefixRows ++ checkpoint.retained) checkpoint.prefixRows.length with
  | error error => simp [hprepared] at h
  | ok checked =>
      have hcounts := required_replay_not_silently_removed checkpoint.required
        (checkpoint.prefixRows ++ checkpoint.retained) checkpoint.prefixRows.length
        checked hprepared tag htag
      have hsplit := preparedReplayCheckpoint_exact_split checkpoint.required
        (checkpoint.prefixRows ++ checkpoint.retained) checkpoint.prefixRows.length
        checked hprepared
      simpa [hsplit.1, hsplit.2] using hcounts

def isThinkingReplay : ReplayBlock → Bool
  | .signedThinking .. | .redactedThinking .. => true
  | .text .. | .toolUse .. => false

theorem historical_success_has_no_thinking
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8)))
    (replay : List ReplayBlock)
    (hsuccess : replayBlocks (stripHistoricalReasoning blocks) = .ok replay) :
    replay.all (fun block => !isThinkingReplay block) = true := by
  induction blocks generalizing replay with
  | nil =>
      simp [stripHistoricalReasoning, replayBlocks] at hsuccess
      subst replay
      rfl
  | cons block rest ih =>
      cases block with
      | reasoning _ _ =>
          simp [stripHistoricalReasoning] at hsuccess
          exact ih replay hsuccess
      | text payload =>
          by_cases hempty : payload.isEmpty
          · cases htail : replayBlocks (stripHistoricalReasoning rest) with
            | error error =>
                simp [stripHistoricalReasoning, replayBlocks, replayBlock, hempty,
                  htail] at hsuccess
            | ok tail =>
                simp [stripHistoricalReasoning, replayBlocks, replayBlock, hempty,
                  htail] at hsuccess
                cases hsuccess
                exact ih replay htail
          · cases htail : replayBlocks (stripHistoricalReasoning rest) with
            | error error =>
                simp [stripHistoricalReasoning, replayBlocks, replayBlock, hempty,
                  htail] at hsuccess
            | ok tail =>
                simp [stripHistoricalReasoning, replayBlocks, replayBlock, hempty,
                  htail] at hsuccess
                cases hsuccess
                simpa [isThinkingReplay] using ih tail htail
      | toolCall docId id callId name arguments signature additionalParams =>
          cases htail : replayBlocks (stripHistoricalReasoning rest) with
          | error error =>
              simp [stripHistoricalReasoning, replayBlocks, replayBlock, htail] at hsuccess
          | ok tail =>
              simp [stripHistoricalReasoning, replayBlocks, replayBlock, htail] at hsuccess
              cases hsuccess
              simpa [isThinkingReplay] using ih tail htail
      | toolResult docId id callId parts =>
          simp [stripHistoricalReasoning, replayBlocks, replayBlock] at hsuccess
      | media media =>
          simp [stripHistoricalReasoning, replayBlocks, replayBlock] at hsuccess

theorem historical_narrow_success_has_no_thinking
    (input : ReplayInput) (replay : List ReplayBlock)
    (husage : input.usage = .historical)
    (hsuccess : narrowReplay input = .ok replay) :
    replay.all (fun block => !isThinkingReplay block) = true := by
  simp [narrowReplay, husage] at hsuccess
  exact historical_success_has_no_thinking input.blocks replay hsuccess

theorem historical_narrowing_idempotent
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    stripHistoricalReasoning (stripHistoricalReasoning blocks) =
      stripHistoricalReasoning blocks := by
  induction blocks with
  | nil => rfl
  | cons block rest ih =>
      cases block <;> simp [stripHistoricalReasoning, ih]

theorem required_claude_replays_exact_blocks
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    narrowReplay (ReplayInput.mk .requiredCurrent .claudeSubscription
      (some (reasoningProjection blocks)) blocks) =
      replayBlocks blocks := by
  simp [narrowReplay]

theorem required_claude_reasoning_parts_keep_exact_bytes
    (parts : List (CanonicalOutput.ReasoningPart (List UInt8))) :
    narrowReplay (ReplayInput.mk .requiredCurrent .claudeSubscription
      (some (reasoningProjection [.reasoning none parts]))
      [.reasoning none parts]) = parts.mapM replayPart := by
  rw [required_claude_replays_exact_blocks]
  cases hparts : parts.mapM replayPart <;>
    simp [replayBlocks, replayBlock, hparts]

theorem required_success_has_claude_origin_and_exact_witness
    (input : ReplayInput) (replay : List ReplayBlock)
    (husage : input.usage = .requiredCurrent)
    (hsuccess : narrowReplay input = .ok replay) :
    input.origin = .claudeSubscription ∧
      input.expectedReasoning = some (reasoningProjection input.blocks) ∧
      replayBlocks input.blocks = .ok replay := by
  cases input with
  | mk usage origin expectedReasoning blocks =>
    cases usage with
    | historical => contradiction
    | requiredCurrent =>
      cases origin with
      | foreign => simp [narrowReplay] at hsuccess
      | missing => simp [narrowReplay] at hsuccess
      | ambiguous => simp [narrowReplay] at hsuccess
      | claudeSubscription =>
        cases expectedReasoning with
        | none => simp [narrowReplay] at hsuccess
        | some expected =>
          by_cases hexact : reasoningProjection blocks = expected
          · simp [narrowReplay, hexact] at hsuccess ⊢
            exact hsuccess
          · simp [narrowReplay, hexact] at hsuccess

theorem narrowResolvedReplay_success_has_unique_claude_evidence
    (tag : ReplayTag) (blocks : List (CanonicalOutput.MessageBlock (List UInt8)))
    (resolve : ReplayTag → List ResolvedReplayEvidence) (replay : List ReplayBlock)
    (h : narrowResolvedReplay tag blocks resolve = .ok replay) :
    ∃ evidence, resolve tag = [evidence] ∧
      evidence.origin = .claudeSubscription ∧
      evidence.reasoning = reasoningProjection blocks ∧
      replayBlocks blocks = .ok replay := by
  unfold narrowResolvedReplay at h
  cases hresolve : resolve tag with
  | nil => simp [hresolve] at h
  | cons first rest =>
      cases rest with
      | nil =>
          have howner := required_success_has_claude_origin_and_exact_witness
            (ReplayInput.mk .requiredCurrent first.origin (some first.reasoning) blocks)
            replay rfl (by simpa [hresolve] using h)
          exact ⟨first, rfl, howner.1,
            Option.some.inj howner.2.1, howner.2.2⟩
      | cons second tail => simp [hresolve] at h

private theorem mapM_except_success_at_member {α β : Type}
    (f : α → Except MapError β) (rows : List α) (result : List β)
    (h : rows.mapM f = .ok result) (row : α) (hrow : row ∈ rows) :
    ∃ value, f row = .ok value := by
  induction rows generalizing result with
  | nil => simp at hrow
  | cons first rest ih =>
      cases hfirst : f first with
      | error error =>
          simp [List.mapM_cons, hfirst] at h
          change (Except.error error : Except MapError (List β)) = .ok result at h
          cases h
      | ok firstValue =>
          cases hrest : rest.mapM f with
          | error error =>
              simp [List.mapM_cons, hfirst, hrest] at h
              change (Except.error error : Except MapError (List β)) = .ok result at h
              cases h
          | ok restValues =>
              simp only [List.mem_cons] at hrow
              rcases hrow with rfl | htail
              · exact ⟨firstValue, hfirst⟩
              · exact ih restValues hrest htail

/-- A successful restored replay has an exactly-once retained assistant row
and an exactly-one canonical resolution for *every* independently required
coordinate. Each resolved row passed the same strict replay codec in list
order. This is conditional on native issuance, durable sidecar integrity and
the supplied canonical resolution; it does not prove those DB operations. -/
theorem restored_success_has_all_required_claude_evidence
    (checkpoint : ReplayCheckpoint)
    (resolve : ReplayTag → List ResolvedReplayEvidence)
    (replay : List (List ReplayBlock))
    (h : restoreAndNarrowReplay checkpoint resolve = .ok replay)
    (tag : ReplayTag) (htag : tag ∈ checkpoint.required) :
    replayTagCount tag checkpoint.prefixRows = 0 ∧
    replayTagCount tag checkpoint.retained = 1 ∧
    ∃ row, row ∈ checkpoint.retained ∧ row.source = some tag ∧
      ∃ evidence rowReplay, resolve tag = [evidence] ∧
        evidence.origin = .claudeSubscription ∧
        evidence.reasoning = reasoningProjection row.blocks ∧
        replayBlocks row.blocks = .ok rowReplay := by
  have hcounts := restored_success_retains_each_required_once
    checkpoint resolve replay h tag htag
  refine ⟨hcounts.1, hcounts.2, ?_⟩
  have hrow : ∃ row, row ∈ checkpoint.retained ∧ row.source = some tag := by
    unfold replayTagCount at hcounts
    cases hfiltered : checkpoint.retained.filter
        (fun row => row.source == some tag) with
    | nil => simp [hfiltered] at hcounts
    | cons row rest =>
        have hmem : row ∈ checkpoint.retained.filter
            (fun row => row.source == some tag) := by
          rw [hfiltered]
          simp
        have hparts := List.mem_filter.mp hmem
        exact ⟨row, hparts.1, by simpa using hparts.2⟩
  obtain ⟨row, hmem, hsource⟩ := hrow
  let runRow : TaggedReplayRow → Except MapError (List ReplayBlock) := fun candidate =>
    match candidate.source with
    | some candidateTag =>
        if candidateTag ∈ checkpoint.required then
          narrowResolvedReplay candidateTag candidate.blocks resolve
        else
          narrowReplay (ReplayInput.mk .historical .missing none candidate.blocks)
    | none => narrowReplay (ReplayInput.mk .historical .missing none candidate.blocks)
  cases hprepared : prepareReplayCheckpoint checkpoint.required
      (checkpoint.prefixRows ++ checkpoint.retained) checkpoint.prefixRows.length with
  | error error => simp [restoreAndNarrowReplay, hprepared] at h
  | ok checked =>
      have hsplit := preparedReplayCheckpoint_exact_split checkpoint.required
        (checkpoint.prefixRows ++ checkpoint.retained) checkpoint.prefixRows.length
        checked hprepared
      have hmap : checkpoint.retained.mapM runRow = .ok replay := by
        simpa [restoreAndNarrowReplay, hprepared, runRow, hsplit.2] using h
      obtain ⟨rowReplay, hsuccess⟩ :=
        mapM_except_success_at_member runRow checkpoint.retained replay hmap row hmem
      have hnarrow : narrowResolvedReplay tag row.blocks resolve = .ok rowReplay := by
        simpa [runRow, hsource, htag] using hsuccess
      obtain ⟨evidence, hresolution, horigin, hwitness, hcodec⟩ :=
        narrowResolvedReplay_success_has_unique_claude_evidence
          tag row.blocks resolve rowReplay hnarrow
      exact ⟨row, hmem, hsource, evidence, rowReplay,
        hresolution, horigin, hwitness, hcodec⟩

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
  cases hparts : parts.mapM replayPart <;>
    simp [replayBlocks, replayBlock, hparts]

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

theorem decoded_signature_prefix_survives_abort_without_sealing :
    (runDecodedPrefixTrace {} [.thinkingStart 0 "", .signatureDelta 0 "署",
      .signatureDelta 0 "名"]).map (fun result =>
        (result.1.map (·.provisionalSignature), result.2)) =
      .ok ([none, some "署", some "署名"], []) ∧
    runContentStream {} [.thinkingStart 0 "", .signatureDelta 0 "署",
      .signatureDelta 0 "名"] = .error .incompleteBlock := by
  native_decide

theorem decoded_redacted_prefix_survives_abort_without_sealing :
    (runDecodedPrefixTrace {} [.redactedStart 0 "opaque"]).map (fun result =>
        (result.1.map (·.provisionalRedacted), result.2)) =
      .ok ([some "opaque"], []) ∧
    runContentStream {} [.redactedStart 0 "opaque"] = .error .incompleteBlock := by
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
