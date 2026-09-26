import Proofs.Transcript.State
import Proofs.CanonicalOutput.Message
import Proofs.PromptAssembly.ReplayFrontier

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
model name, wire format, or Rig's history/prompt carrier. `ReasoningWitness`
is the exact canonical native reasoning projection: ordered parts, bytes,
signatures, and original block positions, but not the provider's display ID.
Restoring that evidence from a durable checkpoint is a native binding
obligation, not established by this pure model. Provenance and exact blocks
also do not prove provider acceptance if earlier system, tool, or message
prefixes change; provider capability/prefix policy is outside this owner
(#1693). Evidence is supplied for each selected row, potentially several
assistant rows in one ongoing tool-use turn; this owner does not prove upstream
row selection was complete. It imposes no universal thinking-first rule. -/

inductive ReplayOrigin where
  | claudeSubscription
  | acceptedProvider
  | foreign
  | missing
  | ambiguous
  deriving DecidableEq, Repr

abbrev ReasoningWitness := List (Nat × List (CanonicalOutput.ReasoningPart (List UInt8)))

def reasoningProjectionFrom (index : Nat) :
    List (CanonicalOutput.MessageBlock (List UInt8)) → ReasoningWitness
  | [] => []
  | .reasoning _ parts :: rest =>
      (index, parts) :: reasoningProjectionFrom (index + 1) rest
  | _ :: rest => reasoningProjectionFrom (index + 1) rest

def reasoningProjection (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    ReasoningWitness :=
  reasoningProjectionFrom 0 blocks

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
  /-- Physical accepted header, not a provider message ID or a logical request ID. -/
  physicalHeader : Option String := none
  /-- Canonical block indices carried through every durable projection rewrite.
  Missing entries are unknown provenance, not freshly enumerated indices. -/
  blockIndices : List Nat := []
  blocks : List (CanonicalOutput.MessageBlock (List UInt8))
  deriving DecidableEq, Repr

structure ReplayIssuer where
  family : String
  endpoint : String
  deriving DecidableEq, Repr

inductive ReplayWire where
  | claudeMessages
  | responses
  deriving DecidableEq, Repr

/-- One selected provider-input item: the context item (system, tools and
route, without non-prefix request parameters or cache hints), then each
message's header and content blocks, with reasoning blocks as reasoning items.
Native derives these from the canonical JSON of the actual built request or the
accepted transport capture, never from a second serializer. -/
abbrev ReplayFlatItem := ReplayFrontier.FlatItem (List UInt8) (List UInt8)

structure ResolvedReplayEvidence where
  origin : ReplayOrigin
  reasoning : ReasoningWitness
  issuer : ReplayIssuer
  wire : ReplayWire
  physicalHeader : String
  /-- True only for a completed, published physical provider turn. Partial
  received bytes remain audit data and are not replayable under this rule. -/
  complete : Bool
  /-- The flattened accepted request that produced this turn; `none` when the
  capture is missing or undecodable, which makes the turn non-replayable. -/
  captured : Option (List ReplayFlatItem)
  deriving Repr

/-- Each candidate is one native reasoning block in its physical source row.
`complete` and `issuer` below are premises supplied by the accepted header,
closing segment and rendered-request join. The model cannot infer them from
signatures, native bytes or a provider display name. -/
structure ReasoningCandidate where
  source : Option ReplayTag
  physicalHeader : Option String
  blockIndex : Option Nat
  indicesComplete : Bool
  parts : List (CanonicalOutput.ReasoningPart (List UInt8))
  deriving Repr

def strictlyIncreasingIndices (indices : List Nat) : Bool :=
  decide (List.Pairwise (· < ·) indices)

def replayRowIndicesValid (row : TaggedReplayRow) : Bool :=
  row.blockIndices.length == row.blocks.length &&
    strictlyIncreasingIndices row.blockIndices

def reasoningCandidatesFrom (source : Option ReplayTag)
    (physicalHeader : Option String) (indicesComplete : Bool) :
    List Nat → List (CanonicalOutput.MessageBlock (List UInt8)) →
      List ReasoningCandidate
  | _, [] => []
  | indices, .reasoning _ parts :: rest =>
      { source, physicalHeader, blockIndex := indices.head?,
        indicesComplete,
        parts } ::
        reasoningCandidatesFrom source physicalHeader indicesComplete indices.tail rest
  | indices, _ :: rest =>
      reasoningCandidatesFrom source physicalHeader indicesComplete indices.tail rest

def reasoningCandidates (rows : List TaggedReplayRow) : List ReasoningCandidate :=
  rows.flatMap fun row => reasoningCandidatesFrom row.source row.physicalHeader
    (replayRowIndicesValid row) row.blockIndices row.blocks

def originalReasoningWitness (row : TaggedReplayRow) : ReasoningWitness :=
  (reasoningCandidates [row]).filterMap fun candidate =>
    candidate.blockIndex.map fun index => (index, candidate.parts)

/-- Replayability is endpoint-family specific. This validates native payload
shape only; acceptance and source closure remain external provenance premises. -/
def replayableReasoning (wire : ReplayWire)
    (parts : List (CanonicalOutput.ReasoningPart (List UInt8))) : Bool :=
  if parts.isEmpty then false
  else match wire with
    | .claudeMessages => (parts.mapM replayPart).isOk
    | .responses =>
        parts.any (fun part => match part with
          | .encrypted bytes => !bytes.isEmpty
          | _ => false) &&
        parts.all (fun part => match part with
          | .encrypted bytes => !bytes.isEmpty
          | .summary _ => true
          | _ => false)

def validReasoningCandidate (issuer : ReplayIssuer) (wire : ReplayWire)
    (resolve : ReplayTag → List ResolvedReplayEvidence)
    (candidate : ReasoningCandidate) : Bool :=
  match candidate.source, candidate.physicalHeader with
  | some tag, some header =>
      if !providerReplayTag tag then false
      else match resolve tag with
        | [evidence] =>
            evidence.origin == .acceptedProvider &&
            evidence.issuer == issuer && evidence.wire == wire &&
            evidence.physicalHeader == header &&
            evidence.complete &&
            candidate.indicesComplete &&
            candidate.blockIndex.any (fun index =>
              (index, candidate.parts) ∈ evidence.reasoning) &&
            replayableReasoning wire candidate.parts
        | _ => false
  | _, _ => false

def validForReplayProjection (issuer : ReplayIssuer) (wire : ReplayWire)
    (resolve : ReplayTag → List ResolvedReplayEvidence)
    (candidates : List ReasoningCandidate) (candidate : ReasoningCandidate) : Bool :=
  validReasoningCandidate issuer wire resolve candidate &&
    ((candidates.filter fun other =>
      other.source == candidate.source &&
      other.physicalHeader == candidate.physicalHeader &&
      other.blockIndex == candidate.blockIndex).length == 1)

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

/-- Source shaping filters whole native blocks with their original canonical
indices. The sidecar is admitted before shaping; neither retained blocks nor
their reasoning witnesses are reindexed by the shaped provider position. -/
def selectIndexedReplayBlocks (keep : Nat →
    CanonicalOutput.MessageBlock (List UInt8) → Bool) :
    List Nat → List (CanonicalOutput.MessageBlock (List UInt8)) →
      List Nat × List (CanonicalOutput.MessageBlock (List UInt8))
  | index :: indices, block :: blocks =>
      let (selectedIndices, selectedBlocks) :=
        selectIndexedReplayBlocks keep indices blocks
      if keep index block then
        (index :: selectedIndices, block :: selectedBlocks)
      else (selectedIndices, selectedBlocks)
  | _, _ => ([], [])

def selectReplayBlocks (keep : Nat →
    CanonicalOutput.MessageBlock (List UInt8) → Bool)
    (row : TaggedReplayRow) : Except MapError TaggedReplayRow :=
  if replayRowIndicesValid row then
    let (blockIndices, blocks) :=
      selectIndexedReplayBlocks keep row.blockIndices row.blocks
    .ok { row with blockIndices, blocks }
  else .error .invalidReplayAssociation

theorem selectIndexedReplayBlocks_indices_sublist
    (keep : Nat → CanonicalOutput.MessageBlock (List UInt8) → Bool)
    (indices : List Nat) (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    List.Sublist (selectIndexedReplayBlocks keep indices blocks).1 indices := by
  induction indices generalizing blocks with
  | nil => simp [selectIndexedReplayBlocks]
  | cons index rest ih =>
      cases blocks with
      | nil => simp [selectIndexedReplayBlocks]
      | cons block remaining =>
          by_cases h : keep index block
          · simp [selectIndexedReplayBlocks, h, ih]
          · simp [selectIndexedReplayBlocks, h, ih]

theorem selectIndexedReplayBlocks_blocks_sublist
    (keep : Nat → CanonicalOutput.MessageBlock (List UInt8) → Bool)
    (indices : List Nat) (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    List.Sublist (selectIndexedReplayBlocks keep indices blocks).2 blocks := by
  induction blocks generalizing indices with
  | nil => simp [selectIndexedReplayBlocks]
  | cons block rest ih =>
      cases indices with
      | nil => simp [selectIndexedReplayBlocks]
      | cons index remaining =>
          by_cases h : keep index block
          · simp [selectIndexedReplayBlocks, h, ih]
          · simp [selectIndexedReplayBlocks, h, ih]

theorem selectReplayBlocks_keeps_physical_coordinates
    (keep : Nat → CanonicalOutput.MessageBlock (List UInt8) → Bool)
    (row selected : TaggedReplayRow)
    (h : selectReplayBlocks keep row = .ok selected) :
    selected.source = row.source ∧
      selected.physicalHeader = row.physicalHeader ∧
      List.Sublist selected.blockIndices row.blockIndices ∧
      List.Sublist selected.blocks row.blocks := by
  unfold selectReplayBlocks at h
  split at h <;> try contradiction
  cases h
  exact ⟨rfl, rfl,
    selectIndexedReplayBlocks_indices_sublist keep row.blockIndices row.blocks,
    selectIndexedReplayBlocks_blocks_sublist keep row.blockIndices row.blocks⟩

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

/-- Exact `take`/`drop` split and source-association validation. `required`
names the pending tool round, which a reduction must not summarize; it is not
a replay selector. -/
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


def stripFirstReasoningBlocks (count : Nat) (indices : List Nat) :
    List (CanonicalOutput.MessageBlock (List UInt8)) →
    Nat × List Nat × List (CanonicalOutput.MessageBlock (List UInt8))
  | [] => (count, [], [])
  | block :: rest =>
      let next := match block with
        | .reasoning .. => if count > 0 then count - 1 else count
        | _ => count
      let (remaining, keptIndices, kept) :=
        stripFirstReasoningBlocks next indices.tail rest
      let keepIndex := match indices.head? with
        | some index => index :: keptIndices
        | none => keptIndices
      match block with
      | .reasoning .. =>
          if count > 0 then (remaining, keptIndices, kept)
          else (remaining, keepIndex, block :: kept)
      | _ => (remaining, keepIndex, block :: kept)

theorem stripFirstReasoningBlocks_exact_candidates
    (source : Option ReplayTag) (header : Option String) (complete : Bool)
    (count : Nat) (indices : List Nat)
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8)))
    (hlen : indices.length = blocks.length) :
    let result := stripFirstReasoningBlocks count indices blocks
    reasoningCandidatesFrom source header complete result.2.1 result.2.2 =
      (reasoningCandidatesFrom source header complete indices blocks).drop count := by
  induction blocks generalizing count indices with
  | nil =>
      cases indices with
      | nil => simp [stripFirstReasoningBlocks, reasoningCandidatesFrom]
      | cons _ _ => simp at hlen
  | cons block rest ih =>
      cases indices with
      | nil => simp at hlen
      | cons index tail =>
          have htail : tail.length = rest.length := by simpa using hlen
          cases block <;> cases count <;>
            simp [stripFirstReasoningBlocks, reasoningCandidatesFrom,
              ih _ _ htail]

theorem stripFirstReasoningBlocks_remaining
    (source : Option ReplayTag) (header : Option String) (complete : Bool)
    (count : Nat) (indices : List Nat)
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8)))
    (hlen : indices.length = blocks.length) :
    (stripFirstReasoningBlocks count indices blocks).1 = count -
      (reasoningCandidatesFrom source header complete indices blocks).length := by
  induction blocks generalizing count indices with
  | nil =>
      cases indices with
      | nil => simp [stripFirstReasoningBlocks, reasoningCandidatesFrom]
      | cons _ _ => simp at hlen
  | cons block rest ih =>
      cases indices with
      | nil => simp at hlen
      | cons index tail =>
          have htail : tail.length = rest.length := by simpa using hlen
          cases block <;> cases count <;>
            simp [stripFirstReasoningBlocks, reasoningCandidatesFrom,
              ih _ _ htail, Nat.succ_sub_succ_eq_sub]

theorem stripFirstReasoningBlocks_indices_sublist
    (count : Nat) (indices : List Nat)
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8))) :
    List.Sublist (stripFirstReasoningBlocks count indices blocks).2.1 indices := by
  induction blocks generalizing count indices with
  | nil => simp [stripFirstReasoningBlocks]
  | cons block rest ih =>
      cases block <;> cases indices <;> cases count <;>
        simp [stripFirstReasoningBlocks, ih, List.Sublist.cons, List.Sublist.cons₂]

theorem stripFirstReasoningBlocks_keeps_alignment
    (count : Nat) (indices : List Nat)
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8)))
    (hlen : indices.length = blocks.length) :
    (stripFirstReasoningBlocks count indices blocks).2.1.length =
      (stripFirstReasoningBlocks count indices blocks).2.2.length := by
  induction blocks generalizing count indices with
  | nil =>
      cases indices with
      | nil => simp [stripFirstReasoningBlocks]
      | cons _ _ => simp at hlen
  | cons block rest ih =>
      cases indices with
      | nil => simp at hlen
      | cons index tail =>
          have htail : tail.length = rest.length := by simpa using hlen
          cases block <;> cases count <;>
            simp [stripFirstReasoningBlocks, ih _ _ htail]

theorem stripFirstReasoningBlocks_keeps_valid_indices
    (count : Nat) (indices : List Nat)
    (blocks : List (CanonicalOutput.MessageBlock (List UInt8)))
    (hlen : indices.length = blocks.length)
    (hordered : strictlyIncreasingIndices indices = true) :
    let result := stripFirstReasoningBlocks count indices blocks
    result.2.1.length = result.2.2.length ∧
      strictlyIncreasingIndices result.2.1 = true := by
  have haligned := stripFirstReasoningBlocks_keeps_alignment count indices blocks hlen
  have hsub := stripFirstReasoningBlocks_indices_sublist count indices blocks
  have hpair : List.Pairwise (· < ·) indices := by
    exact of_decide_eq_true (by simpa [strictlyIncreasingIndices] using hordered)
  have hpair' := hpair.sublist hsub
  exact ⟨haligned, by simpa [strictlyIncreasingIndices] using
    (decide_eq_true hpair')⟩

def stripFirstReasoningRows (count : Nat) : List TaggedReplayRow → List TaggedReplayRow
  | [] => []
  | row :: rest =>
      let (remaining, blockIndices, blocks) :=
        stripFirstReasoningBlocks count row.blockIndices row.blocks
      let physicalHeader := if replayRowIndicesValid row then row.physicalHeader else none
      { row with blocks, blockIndices, physicalHeader } ::
        stripFirstReasoningRows remaining rest

theorem stripFirstReasoningRows_preserves_valid (count : Nat)
    (rows : List TaggedReplayRow)
    (hvalid : rows.all replayRowIndicesValid = true) :
    (stripFirstReasoningRows count rows).all replayRowIndicesValid = true := by
  induction rows generalizing count with
  | nil => rfl
  | cons row rest ih =>
      simp only [List.all_cons, Bool.and_eq_true] at hvalid ⊢
      have hrow : row.blockIndices.length = row.blocks.length ∧
          strictlyIncreasingIndices row.blockIndices = true := by
        simpa [replayRowIndicesValid, Bool.and_eq_true] using hvalid.1
      have hblock := stripFirstReasoningBlocks_keeps_valid_indices count
        row.blockIndices row.blocks hrow.1 hrow.2
      simp [stripFirstReasoningRows, replayRowIndicesValid, hvalid.1,
        hblock.1, hblock.2, ih _ hvalid.2]

theorem stripFirstReasoningRows_exact_candidates (count : Nat)
    (rows : List TaggedReplayRow)
    (hvalid : rows.all replayRowIndicesValid = true)
    (hpost : (stripFirstReasoningRows count rows).all replayRowIndicesValid = true) :
    reasoningCandidates (stripFirstReasoningRows count rows) =
      (reasoningCandidates rows).drop count := by
  induction rows generalizing count with
  | nil => simp [stripFirstReasoningRows, reasoningCandidates]
  | cons row rest ih =>
      simp only [List.all_cons, Bool.and_eq_true] at hvalid
      let result := stripFirstReasoningBlocks count row.blockIndices row.blocks
      let outputRow : TaggedReplayRow :=
        { source := row.source,
          physicalHeader := if replayRowIndicesValid row then row.physicalHeader else none,
          blockIndices := result.2.1, blocks := result.2.2 }
      have hpost' : replayRowIndicesValid outputRow = true ∧
          (stripFirstReasoningRows result.1 rest).all replayRowIndicesValid = true := by
        simpa [stripFirstReasoningRows, result, outputRow] using hpost
      have hlen : row.blockIndices.length = row.blocks.length := by
        simp [replayRowIndicesValid] at hvalid
        exact hvalid.1.1
      have hblock := stripFirstReasoningBlocks_exact_candidates row.source
        row.physicalHeader true count row.blockIndices row.blocks hlen
      have hremaining := stripFirstReasoningBlocks_remaining row.source
        row.physicalHeader true count row.blockIndices row.blocks hlen
      simp only [reasoningCandidates, stripFirstReasoningRows, List.flatMap_cons]
      simp only [hvalid.1, ite_true]
      have htail := ih result.1 hvalid.2 hpost'.2
      simp only [reasoningCandidates, result] at htail
      rw [htail]
      have hout : replayRowIndicesValid
          { source := row.source, physicalHeader := row.physicalHeader,
            blockIndices := (stripFirstReasoningBlocks count row.blockIndices row.blocks).2.1,
            blocks := (stripFirstReasoningBlocks count row.blockIndices row.blocks).2.2 } = true := by
        simpa [outputRow, result, hvalid.1] using hpost'.1
      rw [hout]
      simp only [result] at hblock
      rw [hblock, hremaining]
      simp only [List.drop_append_eq_append_drop]

theorem stripFirstReasoningRows_exact_candidates_of_valid (count : Nat)
    (rows : List TaggedReplayRow)
    (hvalid : rows.all replayRowIndicesValid = true) :
    reasoningCandidates (stripFirstReasoningRows count rows) =
      (reasoningCandidates rows).drop count := by
  exact stripFirstReasoningRows_exact_candidates count rows hvalid
    (stripFirstReasoningRows_preserves_valid count rows hvalid)

/-! ## Replay selection over accepted turns

A turn is one physical accepted row's reasoning. Selection has two phases.
First, the longest suffix of turns whose provenance and payload are valid is
assembled into the actual provider body (a turn failing those checks cannot be
encoded, and everything before it is dropped). Second, each assembled turn is
located in that body and judged against its accepted capture
(`ReplayFrontier.turnOk`); the longest admissible suffix is replayed. Whole
turns are kept or dropped, so removal is always a leading run of reasoning. -/

/-- One accepted row's reasoning, with whether the row keeps an encodable
ordinary block once its reasoning is removed. The Claude encoder omits an empty
assistant message, which would shift every later ordinary item. -/
structure TurnGroup where
  candidates : List ReasoningCandidate
  ordinary : Bool
  deriving Repr

def encodableOrdinary : CanonicalOutput.MessageBlock (List UInt8) → Bool
  | .text payload => !payload.isEmpty
  | .toolCall .. => true
  | .reasoning .. | .toolResult .. | .media .. => false

def turnGroups (rows : List TaggedReplayRow) : List TurnGroup :=
  rows.filterMap fun row =>
    match reasoningCandidates [row] with
    | [] => none
    | group => some { candidates := group, ordinary := row.blocks.any encodableOrdinary }

def groupCandidates (groups : List TurnGroup) : List ReasoningCandidate :=
  groups.flatMap (·.candidates)

theorem reasoningCandidates_cons (row : TaggedReplayRow) (rest : List TaggedReplayRow) :
    reasoningCandidates (row :: rest) = reasoningCandidates [row] ++ reasoningCandidates rest := by
  simp [reasoningCandidates]

theorem turnGroups_candidates (rows : List TaggedReplayRow) :
    groupCandidates (turnGroups rows) = reasoningCandidates rows := by
  induction rows with
  | nil => simp [turnGroups, groupCandidates, reasoningCandidates]
  | cons row rest ih =>
      rw [reasoningCandidates_cons, ← ih]
      unfold turnGroups
      cases hgroup : reasoningCandidates [row] with
      | nil => simp [List.filterMap_cons, hgroup, groupCandidates]
      | cons first others => simp [List.filterMap_cons, hgroup, groupCandidates]

/-- Wire reasoning items one canonical reasoning block renders to: one Claude
block per part, one Responses `reasoning` item per block. -/
def wireReasoningCount : ReplayWire → ReasoningCandidate → Nat
  | .claudeMessages, candidate => candidate.parts.length
  | .responses, _ => 1

/-- Items between a turn's start and its first reasoning item: the Claude
assistant message header; Responses has none. -/
def turnHeaderOffset : ReplayWire → Nat
  | .claudeMessages => 1
  | .responses => 0

def turnBase (issuer : ReplayIssuer) (wire : ReplayWire)
    (resolve : ReplayTag → List ResolvedReplayEvidence)
    (candidates : List ReasoningCandidate) (group : TurnGroup) : Bool :=
  !group.candidates.isEmpty && (wire == .responses || group.ordinary) &&
    group.candidates.all (validForReplayProjection issuer wire resolve candidates)

def turnCaptured (resolve : ReplayTag → List ResolvedReplayEvidence)
    (group : TurnGroup) : Option (List ReplayFlatItem) :=
  match group.candidates.head?.bind (·.source) with
  | none => none
  | some tag =>
      match resolve tag with
      | [evidence] => evidence.captured
      | _ => none

def reasoningPositions (body : List ReplayFlatItem) : List Nat :=
  (body.zip (List.range body.length)).filterMap fun entry =>
    match entry.1 with
    | .reasoning _ => some entry.2
    | .ordinary _ => none

abbrev TurnLayout := List (List UInt8) × List (Nat × List UInt8)

def headerAt (offset : Nat) (body : List ReplayFlatItem) (start : Nat) : Bool :=
  offset == 0 ||
    (match body[start]? with
     | some (.ordinary _) => true
     | _ => false)

def locateTurnsFrom (offset : Nat) (body : List ReplayFlatItem) (positions : List Nat)
    (anchoredItems : List (Nat × List UInt8)) : Nat → List Nat → Option (List TurnLayout)
  | _, [] => some []
  | seen, count :: rest => do
      let position ← positions[seen]?
      if position < offset then none else
      let start := position - offset
      if !headerAt offset body start then none else
      let tail ← locateTurnsFrom offset body positions anchoredItems (seen + count) rest
      some ((ReplayFrontier.ords (body.take start), (anchoredItems.drop seen).take count) :: tail)

/-- Locate each assembled turn's ordinary prefix and anchored reasoning items in
the actual body. Any disagreement between the expected wire counts and the body
fails closed. -/
def locateTurns (offset : Nat) (body : List ReplayFlatItem) (counts : List Nat) :
    Option (List TurnLayout) :=
  let positions := reasoningPositions body
  if counts.sum != positions.length || counts.any (· == 0) then none
  else locateTurnsFrom offset body positions (ReplayFrontier.anchored body) 0 counts

theorem locateTurnsFrom_length (offset : Nat) (body : List ReplayFlatItem)
    (positions : List Nat) (anchoredItems : List (Nat × List UInt8)) (seen : Nat)
    (counts : List Nat) (layouts : List TurnLayout)
    (h : locateTurnsFrom offset body positions anchoredItems seen counts = some layouts) :
    layouts.length = counts.length := by
  induction counts generalizing seen layouts with
  | nil => simp [locateTurnsFrom] at h; simp [← h]
  | cons count rest ih =>
      simp only [locateTurnsFrom, Option.bind_eq_bind] at h
      cases hpos : positions[seen]? with
      | none => simp [hpos] at h
      | some position =>
          simp only [hpos, Option.some_bind] at h
          by_cases hlt : position < offset
          · simp [hlt] at h
          · by_cases hhdr : headerAt offset body (position - offset) = true
            · cases htail : locateTurnsFrom offset body positions anchoredItems
                  (seen + count) rest with
              | none => simp [hlt, hhdr, htail] at h
              | some tail =>
                  simp [hlt, hhdr, htail] at h
                  subst h
                  simp [ih _ _ htail]
            · simp [hlt, hhdr] at h

/-- Accepted turns in order, as the frontier sees them. -/
def assembledTurns (issuer : ReplayIssuer) (wire : ReplayWire)
    (resolve : ReplayTag → List ResolvedReplayEvidence)
    (candidates : List ReasoningCandidate) (groups : List TurnGroup)
    (layouts : List TurnLayout) : List (ReplayFrontier.Turn (List UInt8) (List UInt8)) :=
  (groups.zip layouts).map fun entry =>
    { base := turnBase issuer wire resolve candidates entry.1,
      prefixOrds := entry.2.1, items := entry.2.2,
      captured := turnCaptured resolve entry.1 }

/-- Phase one: the longest suffix of turns with valid provenance and payload. -/
def replayBaseKept (rows : List TaggedReplayRow) (issuer : ReplayIssuer) (wire : ReplayWire)
    (resolve : ReplayTag → List ResolvedReplayEvidence) : List TurnGroup :=
  ReplayFrontier.maxAdmissibleSuffix
    (fun _ group => turnBase issuer wire resolve (reasoningCandidates rows) group) (turnGroups rows)

/-- The rows the owned loop assembles to locate phase-one turns. -/
def replayStage (rows : List TaggedReplayRow) (issuer : ReplayIssuer) (wire : ReplayWire)
    (resolve : ReplayTag → List ResolvedReplayEvidence) : List TaggedReplayRow :=
  stripFirstReasoningRows ((reasoningCandidates rows).length -
    (groupCandidates (replayBaseKept rows issuer wire resolve)).length) rows

/-- The replayed turns: a suffix of the physical turn groups. `assemble` is the
owned loop's actual request builder applied to rows. -/
def replayedTurnGroups (rows : List TaggedReplayRow) (issuer : ReplayIssuer)
    (wire : ReplayWire) (resolve : ReplayTag → List ResolvedReplayEvidence)
    (assemble : List TaggedReplayRow → List ReplayFlatItem) : List TurnGroup :=
  let baseKept := replayBaseKept rows issuer wire resolve
  let counts := baseKept.map fun group => (group.candidates.map (wireReasoningCount wire)).sum
  match locateTurns (turnHeaderOffset wire) (assemble (replayStage rows issuer wire resolve))
      counts with
  | none => []
  | some layouts =>
      let turns := assembledTurns issuer wire resolve (reasoningCandidates rows) baseKept layouts
      baseKept.drop
        (turns.length - (ReplayFrontier.maxAdmissibleSuffix ReplayFrontier.turnOk turns).length)

def restoreHistoricalReasoningSuffix (rows : List TaggedReplayRow) (issuer : ReplayIssuer)
    (wire : ReplayWire) (resolve : ReplayTag → List ResolvedReplayEvidence)
    (assemble : List TaggedReplayRow → List ReplayFlatItem) : List TaggedReplayRow :=
  stripFirstReasoningRows
    ((reasoningCandidates rows).length -
      (groupCandidates (replayedTurnGroups rows issuer wire resolve assemble)).length) rows

theorem replayedTurnGroups_suffix (rows : List TaggedReplayRow) (issuer : ReplayIssuer)
    (wire : ReplayWire) (resolve : ReplayTag → List ResolvedReplayEvidence)
    (assemble : List TaggedReplayRow → List ReplayFlatItem) :
    replayedTurnGroups rows issuer wire resolve assemble <:+ turnGroups rows := by
  unfold replayedTurnGroups
  have hbase : replayBaseKept rows issuer wire resolve <:+ turnGroups rows :=
    ReplayFrontier.maxAdmissibleSuffix_suffix _ _
  dsimp only
  split
  · exact List.nil_suffix
  · exact (List.drop_suffix _ _).trans hbase

/-- Replay keeps exactly the reasoning of a suffix of whole accepted turns, byte
for byte, and drops the leading run before it. -/
theorem restoreHistoricalReasoningSuffix_exact_candidates (rows : List TaggedReplayRow)
    (issuer : ReplayIssuer) (wire : ReplayWire)
    (resolve : ReplayTag → List ResolvedReplayEvidence)
    (assemble : List TaggedReplayRow → List ReplayFlatItem)
    (hvalid : rows.all replayRowIndicesValid = true) :
    reasoningCandidates (restoreHistoricalReasoningSuffix rows issuer wire resolve assemble) =
      groupCandidates (replayedTurnGroups rows issuer wire resolve assemble) := by
  obtain ⟨dropped, hsplit⟩ := replayedTurnGroups_suffix rows issuer wire resolve assemble
  unfold restoreHistoricalReasoningSuffix
  rw [stripFirstReasoningRows_exact_candidates_of_valid _ _ hvalid, ← turnGroups_candidates,
    ← hsplit]
  simp [groupCandidates, List.flatMap_append]

/-- Every replayed turn passed the capture check against exactly the replayed
turns before it, so its assembled prefix is its accepted producing prefix with a
leading run of reasoning removed (`ReplayFrontier.kept_turn_prefix_is_leading_removal`). -/
theorem replayed_turns_admissible
    (turns : List (ReplayFrontier.Turn (List UInt8) (List UInt8))) :
    ReplayFrontier.admissible ReplayFrontier.turnOk
      (ReplayFrontier.maxAdmissibleSuffix ReplayFrontier.turnOk turns) = true :=
  ReplayFrontier.maxAdmissibleSuffix_admissible _ turns

/-- Claude's strict replay codec over already selected rows. Selection is
`restoreHistoricalReasoningSuffix`; this only validates the association split
and encodes each retained row. -/
def restoreContiguousReplay (checkpoint : ReplayCheckpoint) :
    Except MapError (List (List ReplayBlock)) := do
  let _ ← prepareReplayCheckpoint []
    (checkpoint.prefixRows ++ checkpoint.retained) checkpoint.prefixRows.length
  checkpoint.retained.mapM fun row => replayBlocks row.blocks

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
