import Proofs.Observe.FileInventory.Source
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types
import Proofs.Conformance.Family

namespace Conformance.Contracts

open Conformance.ContractCases

def hydrationRequest : FileInventory.Request :=
  { key := "inventory-1:session-1"
  , requester := "did:key:requester-1"
  , node := "did:key:node-1"
  , session := "session-1"
  , nativeSession := 2 }

def hydrationClosureInput : FileInventory.ClosureInput :=
  { request := hydrationRequest
  , bases := []
  , roots := [201]
  , requirements := []
  , messages := [CanonicalOutput.Hydration.Examples.originMessage,
      CanonicalOutput.Hydration.Examples.childMessage]
  , segments := [CanonicalOutput.Hydration.Examples.closing]
  , access := CanonicalOutput.Hydration.Examples.access.map fun observation =>
      { observation with scope := hydrationRequest.authorizationScope }
  , deniedHeaders := []
  , deniedSegments := [] }

def hydrationCollectionName : CanonicalOutput.Hydration.Collection → String
  | .agentRequest => "AgentRequest"
  | .agentMessage => "AgentMessage"
  | .agentToolCall => "AgentToolCall"
  | .agentOutputSegment => "AgentOutputSegment"
  | .compactionEntry => "CompactionEntry"

def hydrationDocumentKeyJson (key : CanonicalOutput.Hydration.DocumentKey) : String :=
  "{" ++ "\"collection\":" ++ jsonString (hydrationCollectionName key.collection) ++ "," ++
    "\"id\":" ++ toString key.id ++ "}"

def optionalNatJson : Option Nat → String
  | none => "null"
  | some value => toString value

def payloadRefJson (reference : CanonicalOutput.PayloadRef) : String :=
  "{" ++ "\"close_id\":" ++ toString reference.closeId ++ "," ++
    "\"stream\":" ++ toString reference.stream ++ "}"

def sourceJson : CanonicalOutput.Source → String
  | .provider scope turn attempt =>
      "{\"kind\":\"provider\",\"scope\":" ++ toString scope ++
        ",\"turn\":" ++ toString turn ++ ",\"attempt\":" ++ toString attempt ++ "}"
  | .auxiliary kind scope turn attempt =>
      "{\"kind\":\"auxiliary\",\"auxiliary_kind\":" ++
        jsonString (match kind with
          | .compaction => "compaction"
          | .compactionFallback => "compaction_fallback"
          | .title => "title"
          | .accessReview => "access_review") ++
        ",\"scope\":" ++ toString scope ++
        ",\"turn\":" ++ toString turn ++ ",\"attempt\":" ++ toString attempt ++ "}"
  | .toolEffectArguments call => "{\"kind\":\"tool_effect_arguments\",\"owner\":" ++ toString call ++ "}"
  | .tool call => "{\"kind\":\"tool\",\"owner\":" ++ toString call ++ "}"
  | .authored key => "{\"kind\":\"authored\",\"owner\":" ++ toString key ++ "}"

def writerJson : CanonicalOutput.Writer → String
  | .request generation => "{\"kind\":\"request\",\"owner\":" ++ toString generation ++ "}"
  | .tool call => "{\"kind\":\"tool\",\"owner\":" ++ toString call ++ "}"

def payloadKindJson : CanonicalOutput.PayloadKind → String
  | .text => "text" | .reasoning => "reasoning" | .signature => "signature"
  | .reasoningStop => "reasoning_stop"
  | .summary => "summary" | .encrypted => "encrypted" | .redacted => "redacted"
  | .arguments => "arguments" | .toolOutput => "tool_output" | .media => "media"

def segmentDeclaration (segment : CanonicalOutput.Segment) :
    Option CanonicalOutput.Declaration :=
  (segment.flush.bind fun flush => flush.runs.head?).bind (·.declaration)

def accessStateName : CanonicalOutput.Hydration.AccessState → String
  | .authorized => "authorized" | .missing => "missing" | .denied => "denied"

def hydrationAccessJson (access : CanonicalOutput.Hydration.ProvenanceAccess) : String :=
  "{" ++ "\"key\":" ++ hydrationDocumentKeyJson access.key ++ "," ++
    "\"state\":" ++ jsonString (accessStateName access.state) ++ "," ++
    "\"requester\":" ++ jsonString access.scope.requester ++ "," ++
    "\"node\":" ++ jsonString access.scope.node ++ "," ++
    "\"session\":" ++ jsonString access.scope.session ++ "," ++
    "\"native_session\":" ++ toString access.scope.nativeSession ++ "}"

def hydrationRequestJson (request : FileInventory.Request) : String :=
  "{" ++ "\"key\":" ++ jsonString request.key ++ "," ++
    "\"requester\":" ++ jsonString request.requester ++ "," ++
    "\"node\":" ++ jsonString request.node ++ "," ++
    "\"session\":" ++ jsonString request.session ++ "," ++
    "\"native_session\":" ++ toString request.nativeSession ++ "}"

def optionalPayloadRefJson : Option CanonicalOutput.PayloadRef → String
  | none => "null"
  | some reference => payloadRefJson reference

def hydrationEffectJson (row : CanonicalOutput.Hydration.EffectRow) : String :=
  "{" ++ "\"id\":" ++ toString row.id ++ "," ++
    "\"parent\":" ++ optionalNatJson row.parent ++ "," ++
    "\"ordinal\":" ++ optionalNatJson row.ordinal ++ "," ++
    "\"spawned\":" ++ boolString row.spawned ++ "," ++
    "\"tool_call_id\":" ++ jsonString row.toolCallId ++ "," ++
    "\"terminal\":" ++ optionalPayloadRefJson row.terminal ++ "}"

deriving instance Lean.ToJson for FileInventory.Request
deriving instance Lean.ToJson for CanonicalOutput.PayloadRef
deriving instance Lean.ToJson for CanonicalOutput.ToolIdentity
deriving instance Lean.ToJson for CanonicalOutput.Hydration.EffectRow

structure ClosureDocumentKeyWire where
  collection : String
  id : Nat
  deriving Lean.ToJson

def ClosureDocumentKeyWire.ofModel (key : CanonicalOutput.Hydration.DocumentKey) :
    ClosureDocumentKeyWire :=
  ⟨hydrationCollectionName key.collection, key.id⟩

structure ClosureMessageWire where
  id : Nat
  session : Nat
  request : Option Nat
  origin : Option Nat
  toolDelivery : Option Nat
  refs : List CanonicalOutput.PayloadRef
  deriving Lean.ToJson

def ClosureMessageWire.ofModel (message : CanonicalOutput.MessageEnvelope) : ClosureMessageWire :=
  ⟨message.header.id, message.header.session, message.header.request, message.header.origin,
    (match message.header.publication with
      | .toolDelivery call => some call
      | _ => none),
    message.header.refs⟩

structure ClosureOwnerWire where
  kind : String
  auxiliaryKind : Option String
  scope : Option Nat
  turn : Option Nat
  attempt : Option Nat
  owner : Option Nat
  deriving Lean.ToJson

def ClosureOwnerWire.ofSource : CanonicalOutput.Source → ClosureOwnerWire
  | .provider scope turn attempt => ⟨"provider", none, some scope, some turn, some attempt, none⟩
  | .auxiliary kind scope turn attempt =>
      ⟨"auxiliary", some (match kind with
        | .compaction => "compaction"
        | .compactionFallback => "compaction_fallback"
        | .title => "title"
        | .accessReview => "access_review"), some scope, some turn, some attempt, none⟩
  | .toolEffectArguments call => ⟨"tool_effect_arguments", none, none, none, none, some call⟩
  | .tool call => ⟨"tool", none, none, none, none, some call⟩
  | .authored key => ⟨"authored", none, none, none, none, some key⟩

def ClosureOwnerWire.ofWriter : CanonicalOutput.Writer → ClosureOwnerWire
  | .request generation => ⟨"request", none, none, none, none, some generation⟩
  | .tool call => ⟨"tool", none, none, none, none, some call⟩

structure ClosureSegmentWire where
  id : Nat
  kind : String
  tool : Option CanonicalOutput.ToolIdentity
  request : Nat
  source : ClosureOwnerWire
  writer : ClosureOwnerWire
  payload : List Nat
  deriving Lean.ToJson

def ClosureSegmentWire.ofModel (segment : CanonicalOutput.Segment) : ClosureSegmentWire :=
  ⟨segment.id, (segmentDeclaration segment).map (payloadKindJson ·.kind) |>.getD "text",
    (segmentDeclaration segment).bind (·.tool), segment.coordinate.request,
    .ofSource segment.coordinate.source, .ofWriter segment.writer,
    (segment.flush.map (·.payload) |>.getD []).map (·.toNat)⟩

structure ClosureAccessWire where
  key : ClosureDocumentKeyWire
  state : String
  requester : String
  node : String
  session : String
  nativeSession : Nat
  deriving Lean.ToJson

def ClosureAccessWire.ofModel (access : CanonicalOutput.Hydration.ProvenanceAccess) :
    ClosureAccessWire :=
  ⟨.ofModel access.key, accessStateName access.state, access.scope.requester, access.scope.node,
    access.scope.session, access.scope.nativeSession⟩

structure HydrationClosureWire where
  request : FileInventory.Request
  roots : List Nat
  messages : List ClosureMessageWire
  segments : List ClosureSegmentWire
  access : List ClosureAccessWire
  deniedHeaders : List Nat
  deniedSegments : List Nat
  bases : List ClosureDocumentKeyWire
  observedEffects : List CanonicalOutput.Hydration.EffectRow
  deriving Lean.ToJson

/-- The closure owner is handed the effects ACP observed, not the raw effect
rows; the wire carries exactly those. -/
def HydrationClosureWire.ofModel (input : FileInventory.ClosureInput) : HydrationClosureWire :=
  ⟨input.request, input.roots, input.messages.map ClosureMessageWire.ofModel,
    input.segments.map ClosureSegmentWire.ofModel, input.access.map ClosureAccessWire.ofModel,
    input.deniedHeaders, input.deniedSegments,
    input.bases.map (ClosureDocumentKeyWire.ofModel ·.key),
    CanonicalOutput.Hydration.observedEffects input.request.authorizationScope input.access
      input.effects⟩

structure ClosureCaseInput where
  name : String
  closureInput : HydrationClosureWire
  deriving Lean.ToJson

private def documentKeyBefore (left right : ClosureDocumentKeyWire) : Bool :=
  left.collection < right.collection || (left.collection == right.collection && left.id ≤ right.id)

/-- The closure's documents as a set, ordered by collection name and id;
`none` when the closure owner rejects the input. -/
def closureDocuments (input : FileInventory.ClosureInput) : Option (List ClosureDocumentKeyWire) :=
  match CanonicalOutput.Hydration.buildManifest input.request.authorizationScope
      input.request.nativeSession input.bases input.roots input.requirements input.messages
      input.segments input.access input.deniedHeaders input.deniedSegments input.dependencyDenials
      input.effects with
  | .error _ => none
  | .ok manifest => some ((manifest.dedup.map ClosureDocumentKeyWire.ofModel).mergeSort documentKeyBefore)

def deniedClosureInput (key : CanonicalOutput.Hydration.DocumentKey) :
    FileInventory.ClosureInput :=
  { hydrationClosureInput with access := hydrationClosureInput.access.map fun observation =>
      if observation.key = key then { observation with state := .denied } else observation }

/-- A fork whose content was copied into its own session; the source session
contributes nothing to its closure. -/
def hydrationForkCopyInput : FileInventory.ClosureInput :=
  { hydrationClosureInput with
    messages := [CanonicalOutput.Hydration.Examples.childMessage]
    segments := [CanonicalOutput.Hydration.Examples.closingCopy]
    access := CanonicalOutput.Hydration.Examples.copyAccess.map fun observation =>
      { observation with scope := hydrationRequest.authorizationScope } }

def hydrationEffectInput : FileInventory.ClosureInput :=
  { hydrationClosureInput with
    roots := [220]
    messages := [CanonicalOutput.Hydration.Examples.parentDelivery]
    segments := CanonicalOutput.Hydration.Examples.effectSegments
    access := CanonicalOutput.Hydration.Examples.effectAccess.map fun observation =>
      { observation with scope := hydrationRequest.authorizationScope }
    effects := [CanonicalOutput.Hydration.Examples.child,
      CanonicalOutput.Hydration.Examples.forgedChild,
      CanonicalOutput.Hydration.Examples.unreachableChild] }

def hydrationBaseParentInput : FileInventory.ClosureInput :=
  { hydrationEffectInput with
    bases := [.toolCall 300], roots := [], messages := []
    segments := [CanonicalOutput.Hydration.Examples.childArguments,
      CanonicalOutput.Hydration.Examples.childOutput] }

def fileInventoryClosureCases : List (String × FileInventory.ClosureInput) :=
  [ ("exact_request_closure", hydrationClosureInput)
  , ("wrong_root_session_rejected",
      { hydrationClosureInput with request := { hydrationRequest with nativeSession := 1 } })
  , ("message_acp_denied",
      deniedClosureInput ⟨.agentMessage, 201⟩)
  , ("segment_acp_denied",
      deniedClosureInput ⟨.agentOutputSegment, 100⟩)
  , ("foreign_scope_evidence_rejected",
      { hydrationClosureInput with access := hydrationClosureInput.access.map fun observation =>
          { observation with scope := { observation.scope with requester := "did:key:requester-foreign" } } })
  , ("fork_copy_without_origin", hydrationForkCopyInput)
  , ("fork_copy_preferred_over_visible_origin",
      { hydrationForkCopyInput with
        messages := hydrationClosureInput.messages
        segments := hydrationClosureInput.segments ++ hydrationForkCopyInput.segments
        access := hydrationClosureInput.access ++ hydrationForkCopyInput.access })
  , ("fork_copy_acp_denied",
      { hydrationForkCopyInput with access := hydrationForkCopyInput.access.map fun observation =>
          if observation.key = ⟨.agentOutputSegment, 300⟩
          then { observation with state := .denied } else observation })
  , ("plugin_effect_children", hydrationEffectInput)
  , ("plugin_effect_slot_conflict",
      { hydrationEffectInput with
        effects := hydrationEffectInput.effects ++
          [{ CanonicalOutput.Hydration.Examples.child with id := 403, terminal := none }]
        access := hydrationEffectInput.access ++
          [⟨hydrationRequest.authorizationScope, ⟨.agentToolCall, 403⟩, .authorized⟩] })
  , ("plugin_effect_parent_base_only", hydrationBaseParentInput)
  , ("plugin_effect_child_denied",
      { hydrationBaseParentInput with
        access := hydrationBaseParentInput.access.map (fun observation =>
          if observation.key = ⟨.agentToolCall, 400⟩
          then { observation with state := .denied } else observation) })
  , ("plugin_effect_arguments_missing",
      { hydrationEffectInput with segments := [CanonicalOutput.Hydration.Examples.parentOutput,
          CanonicalOutput.Hydration.Examples.childOutput] }) ]

example : ["fork_copy_without_origin", "fork_copy_preferred_over_visible_origin",
    "fork_copy_acp_denied"].all (fun name => (fileInventoryClosureCases.map (·.1)).contains name) =
    true := by native_decide

/-- The wire carries each case to the native closure owner; the model evaluates
the case its name selects. -/
@[conformance_family]
def fileInventoryClosure : Family ClosureCaseInput (Option (List ClosureDocumentKeyWire)) where
  inputs := fileInventoryClosureCases.map fun (name, input) =>
    { name, closureInput := .ofModel input }
  model := fun i => ((fileInventoryClosureCases.find? (·.1 == i.name)).map (·.2)).bind
    closureDocuments

def terminalWriteString : FileInventory.TerminalWriteResult → String
  | .committed => "committed"
  | .failed => "failed"
  | .notAttempted => "not_attempted"


end Conformance.Contracts
