import Proofs.Observe.FileInventory.Source
import Proofs.Conformance.Contracts.Json.Helpers
import Proofs.Conformance.ContractCases.Types

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

def hydrationMessageJson (message : CanonicalOutput.MessageEnvelope) : String :=
  "{" ++ "\"id\":" ++ toString message.header.id ++ "," ++
    "\"session\":" ++ toString message.header.session ++ "," ++
    "\"request\":" ++ optionalNatJson message.header.request ++ "," ++
    "\"origin\":" ++ optionalNatJson message.header.origin ++ "," ++
    "\"tool_delivery\":" ++ optionalNatJson (match message.header.publication with
      | .toolDelivery call => some call
      | _ => none) ++ "," ++
    "\"refs\":" ++ jsonArray (message.header.refs.map payloadRefJson) ++ "}"

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

def byteListJson (bytes : List UInt8) : String := jsonArray (bytes.map (toString ·.toNat))

def payloadKindJson : CanonicalOutput.PayloadKind → String
  | .text => "text" | .reasoning => "reasoning" | .signature => "signature"
  | .reasoningStop => "reasoning_stop"
  | .summary => "summary" | .encrypted => "encrypted" | .redacted => "redacted"
  | .arguments => "arguments" | .toolOutput => "tool_output" | .media => "media"

def segmentDeclaration (segment : CanonicalOutput.Segment) :
    Option CanonicalOutput.Declaration :=
  (segment.flush.bind fun flush => flush.runs.head?).bind (·.declaration)

def toolIdentityJson : Option CanonicalOutput.ToolIdentity → String
  | none => "null"
  | some tool => "{\"id\":" ++ jsonString tool.id ++ ",\"name\":" ++ jsonString tool.name ++ "}"

def hydrationSegmentJson (segment : CanonicalOutput.Segment) : String :=
  "{" ++ "\"id\":" ++ toString segment.id ++ "," ++
    "\"kind\":" ++ jsonString ((segmentDeclaration segment).map (payloadKindJson ·.kind)
      |>.getD "text") ++ "," ++
    "\"tool\":" ++ toolIdentityJson ((segmentDeclaration segment).bind (·.tool)) ++ "," ++
    "\"request\":" ++ toString segment.coordinate.request ++ "," ++
    "\"source\":" ++ sourceJson segment.coordinate.source ++ "," ++
    "\"writer\":" ++ writerJson segment.writer ++ "," ++
    "\"payload\":" ++ byteListJson (segment.flush.map (·.payload) |>.getD []) ++ "}"

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

def hydrationClosureInputJson (input : FileInventory.ClosureInput) : String :=
  "{" ++
    "\"request\":" ++ hydrationRequestJson input.request ++ "," ++
    "\"roots\":" ++ jsonArray (input.roots.map toString) ++ "," ++
    "\"messages\":" ++ jsonArray (input.messages.map hydrationMessageJson) ++ "," ++
    "\"segments\":" ++ jsonArray (input.segments.map hydrationSegmentJson) ++ "," ++
    "\"access\":" ++ jsonArray (input.access.map hydrationAccessJson) ++ "," ++
    "\"denied_headers\":" ++ jsonArray (input.deniedHeaders.map toString) ++ "," ++
    "\"denied_segments\":" ++ jsonArray (input.deniedSegments.map toString) ++ "," ++
    "\"bases\":" ++ jsonArray (input.bases.map (hydrationDocumentKeyJson ·.key)) ++ "," ++
    "\"observed_effects\":" ++ jsonArray ((CanonicalOutput.Hydration.observedEffects
      input.request.authorizationScope input.access input.effects).map hydrationEffectJson) ++ "}"

def closureDocumentsJson (input : FileInventory.ClosureInput) : String :=
  match CanonicalOutput.Hydration.buildManifest input.request.authorizationScope
      input.request.nativeSession input.bases input.roots input.requirements input.messages
      input.segments input.access input.deniedHeaders input.deniedSegments input.dependencyDenials
      input.effects with
  | .error _ => "null"
  | .ok manifest => jsonArray (manifest.map hydrationDocumentKeyJson)


def closureCaseJson (name : String) (input : FileInventory.ClosureInput) : String :=
  "{" ++ "\"name\":" ++ jsonString name ++ "," ++
    "\"closure_input\":" ++ hydrationClosureInputJson input ++ "," ++
    "\"expected_closure_documents\":" ++ closureDocumentsJson input ++ "}"

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

def fileInventoryClosureCasesJson : String := jsonArray
  [ closureCaseJson "exact_request_closure" hydrationClosureInput
  , closureCaseJson "wrong_root_session_rejected"
      { hydrationClosureInput with request := { hydrationRequest with nativeSession := 1 } }
  , closureCaseJson "message_acp_denied"
      (deniedClosureInput ⟨.agentMessage, 201⟩)
  , closureCaseJson "segment_acp_denied"
      (deniedClosureInput ⟨.agentOutputSegment, 100⟩)
  , closureCaseJson "foreign_scope_evidence_rejected"
      { hydrationClosureInput with access := hydrationClosureInput.access.map fun observation =>
          { observation with scope := { observation.scope with requester := "did:key:requester-foreign" } } }
  , closureCaseJson "fork_copy_without_origin" hydrationForkCopyInput
  , closureCaseJson "fork_copy_preferred_over_visible_origin"
      { hydrationForkCopyInput with
        messages := hydrationClosureInput.messages
        segments := hydrationClosureInput.segments ++ hydrationForkCopyInput.segments
        access := hydrationClosureInput.access ++ hydrationForkCopyInput.access }
  , closureCaseJson "fork_copy_acp_denied"
      { hydrationForkCopyInput with access := hydrationForkCopyInput.access.map fun observation =>
          if observation.key = ⟨.agentOutputSegment, 300⟩
          then { observation with state := .denied } else observation }
  , closureCaseJson "plugin_effect_children" hydrationEffectInput
  , closureCaseJson "plugin_effect_slot_conflict"
      { hydrationEffectInput with
        effects := hydrationEffectInput.effects ++
          [{ CanonicalOutput.Hydration.Examples.child with id := 403, terminal := none }]
        access := hydrationEffectInput.access ++
          [⟨hydrationRequest.authorizationScope, ⟨.agentToolCall, 403⟩, .authorized⟩] }
  , closureCaseJson "plugin_effect_parent_base_only" hydrationBaseParentInput
  , closureCaseJson "plugin_effect_child_denied"
      { hydrationBaseParentInput with
        access := hydrationBaseParentInput.access.map (fun observation =>
          if observation.key = ⟨.agentToolCall, 400⟩
          then { observation with state := .denied } else observation) }
  , closureCaseJson "plugin_effect_arguments_missing"
      { hydrationEffectInput with segments := [CanonicalOutput.Hydration.Examples.parentOutput,
          CanonicalOutput.Hydration.Examples.childOutput] } ]

def terminalWriteString : FileInventory.TerminalWriteResult → String
  | .committed => "committed"
  | .failed => "failed"
  | .notAttempted => "not_attempted"


end Conformance.Contracts
