import Proofs.CanonicalOutput.Terminal
import Lean

namespace CanonicalOutput

inductive PresentationPart where
  | range (startByte endByte : Nat)
  | literal (bytes : List UInt8)
  deriving DecidableEq, Repr

inductive Presentation where
  | full
  | composed (parts : List PresentationPart)
  deriving DecidableEq, Repr

structure PayloadSpec where
  reference : PayloadRef
  presentation : Presentation := .full
  deriving DecidableEq, Repr

inductive MessageError where
  | reconstruction (error : ReconstructionError)
  | invalidUtf8
  | invalidJson
  | invalidRange
  | invalidPresentation
  | wrongPayloadKind
  | invalidRole
  | invalidPublication
  | referenceMismatch
  | metadataMismatch
  | wrongSource
  | nativeOrder
  | nativePosition
  deriving DecidableEq, Repr

def utf8? (bytes : List UInt8) : Option String :=
  String.fromUTF8? (ByteArray.mk bytes.toArray)

def presentPart (bytes : List UInt8) : PresentationPart → Except MessageError (List UInt8)
  | .literal literal =>
      if (utf8? literal).isSome then .ok literal else .error .invalidUtf8
  | .range start stop =>
      if start ≤ stop ∧ stop ≤ bytes.length then
        let preceding := bytes.take start
        let selected := (bytes.drop start).take (stop - start)
        let suffix := bytes.drop stop
        if (utf8? preceding).isSome && (utf8? selected).isSome && (utf8? suffix).isSome then
          .ok selected
        else .error .invalidUtf8
      else .error .invalidRange

def present (bytes : List UInt8) : Presentation → Except MessageError (List UInt8)
  | .full => if (utf8? bytes).isSome then .ok bytes else .error .invalidUtf8
  | .composed parts => do
      if !(utf8? bytes).isSome then .error .invalidUtf8
      else return (← parts.mapM (presentPart bytes)).flatten

theorem full_presentation_preserves_bytes (bytes : List UInt8)
    (h : (utf8? bytes).isSome = true) : present bytes .full = .ok bytes := by
  simp [present, h]

/-- A candidate starts at a UTF-8 boundary, retains no more than the requested
byte budget, and leaves a valid UTF-8 prefix in the immutable source. -/
def terminalTailCandidate (raw : List UInt8) (budget start : Nat) : Bool :=
  decide (start ≤ raw.length) && decide (raw.length - start ≤ budget) &&
    (utf8? (raw.take start)).isSome && (utf8? (raw.drop start)).isSome

/-- Candidate offsets are scanned in ascending byte order, so the first valid
one retains the longest possible suffix. The full length is the safe fallback
when the raw source itself is valid. -/
def terminalTailStart (raw : List UInt8) (budget : Nat) : Nat :=
  ((List.range (raw.length + 1)).find? (terminalTailCandidate raw budget)).getD raw.length

theorem terminalTailStart_valid (raw : List UInt8) (budget : Nat)
    (hraw : (utf8? raw).isSome = true) :
    terminalTailCandidate raw budget (terminalTailStart raw budget) = true := by
  let p := terminalTailCandidate raw budget
  have hempty : (utf8? ([] : List UInt8)).isSome = true := by native_decide
  have hlast : p raw.length = true := by
    simp [p, terminalTailCandidate, hraw, hempty]
  have hsome : ((List.range (raw.length + 1)).find? p).isSome = true := by
    apply List.find?_isSome.mpr
    exact ⟨raw.length, by simp, hlast⟩
  unfold terminalTailStart
  cases hfind : (List.range (raw.length + 1)).find? p with
  | none => simp [hfind] at hsome
  | some start =>
      simpa [p, hfind] using (List.find?_some hfind)

theorem terminalTailStart_is_longest (raw : List UInt8) (budget start : Nat)
    (hraw : (utf8? raw).isSome = true)
    (hbetter : start < terminalTailStart raw budget) :
    terminalTailCandidate raw budget start = false := by
  let p := terminalTailCandidate raw budget
  have hempty : (utf8? ([] : List UInt8)).isSome = true := by native_decide
  have hlast : p raw.length = true := by
    simp [p, terminalTailCandidate, hraw, hempty]
  have hsome : ((List.range (raw.length + 1)).find? p).isSome = true := by
    apply List.find?_isSome.mpr
    exact ⟨raw.length, by simp, hlast⟩
  unfold terminalTailStart at hbetter
  cases hfind : (List.range (raw.length + 1)).find? p with
  | none => simp [hfind] at hsome
  | some chosen =>
      have hminimal := (List.find?_range_eq_some.mp hfind).2.2 start
      have hbefore : start < chosen := by
        change start < ((List.range (raw.length + 1)).find? p).getD raw.length at hbetter
        simpa [hfind] using hbetter
      have : p start = false := by
        simpa using hminimal hbefore
      exact this

theorem terminalTailStart_bounds (raw : List UInt8) (budget : Nat)
    (hraw : (utf8? raw).isSome = true) :
    terminalTailStart raw budget ≤ raw.length ∧
      raw.length - terminalTailStart raw budget ≤ budget := by
  have h := terminalTailStart_valid raw budget hraw
  simp only [terminalTailCandidate, Bool.and_eq_true, decide_eq_true_eq] at h
  exact ⟨h.1.1.1, h.1.1.2⟩

/-- Preserve the immutable committed bytes and expose only their longest
UTF-8-safe bounded tail, followed by the terminal cause. Invalid source or
cause bytes are rejected rather than repaired. -/
def terminalDiagnosticPresentation (raw cause : List UInt8) (tailBudget : Nat) :
    Except MessageError Presentation := do
  if !(utf8? raw).isSome || !(utf8? cause).isSome then
    .error .invalidUtf8
  else
    let start := terminalTailStart raw tailBudget
    if start == raw.length then
      .ok (.composed [.literal cause])
    else
      .ok (.composed [.range start raw.length, .literal [10], .literal cause])

theorem terminalDiagnosticPresentation_renders (raw cause : List UInt8) (tailBudget : Nat)
    (hraw : (utf8? raw).isSome = true)
    (hcause : (utf8? cause).isSome = true) :
    (terminalDiagnosticPresentation raw cause tailBudget).bind (present raw) =
      .ok (if terminalTailStart raw tailBudget = raw.length then cause
           else raw.drop (terminalTailStart raw tailBudget) ++ [10] ++ cause) := by
  let start := terminalTailStart raw tailBudget
  have hvalid := terminalTailStart_valid raw tailBudget hraw
  simp only [terminalTailCandidate, Bool.and_eq_true, decide_eq_true_eq] at hvalid
  have hbound : start ≤ raw.length := hvalid.1.1.1
  have hprefix : (utf8? (raw.take start)).isSome = true := hvalid.1.2
  have htail : (utf8? (raw.drop start)).isSome = true := hvalid.2
  have hnewline : (utf8? [10]).isSome = true := by native_decide
  by_cases hempty : start = raw.length
  · dsimp only [start] at hempty
    simp [terminalDiagnosticPresentation, hraw, hcause, hempty, Except.bind,
      present, presentPart]
  · dsimp only [start] at hempty hbound hprefix htail
    have htake :
        (raw.drop (terminalTailStart raw tailBudget)).take
            (raw.length - terminalTailStart raw tailBudget) =
          raw.drop (terminalTailStart raw tailBudget) := by
      simpa [List.length_drop] using
        (List.take_length (raw.drop (terminalTailStart raw tailBudget)))
    have hemptyUtf8 : (utf8? ([] : List UInt8)).isSome = true := by native_decide
    simp [terminalDiagnosticPresentation, hraw, hcause, hempty, Except.bind,
      present, presentPart, hbound, hprefix, htail, hnewline, hemptyUtf8, htake,
      List.take_length]
    rfl

inductive ReasoningPart (α : Type) where
  | text (payload : α) (signature : Option String)
  | encrypted (payload : α)
  | redacted (payload : α)
  | summary (payload : α)
  deriving DecidableEq, Repr

inductive MediaData (α : Type) where
  | url (url : String)
  | base64 (payload : α)
  | raw (payload : α)
  | string (payload : α)
  | unknown
  deriving DecidableEq, Repr

structure Media (α : Type) where
  kind : MediaKind
  data : MediaData α
  mediaType : Option String := none
  detail : Option String := none
  additionalParams : Option String := none
  deriving DecidableEq, Repr

inductive ResultPart (α : Type) where
  | text (payload : α)
  | media (value : Media α)
  deriving DecidableEq, Repr

inductive MessageBlock (α : Type) where
  | text (payload : α)
  | reasoning (id : Option String) (parts : List (ReasoningPart α))
  | toolCall (docId : DocId) (id : String) (callId : Option String) (name : String)
      (arguments : α) (signature additionalParams : Option String)
  | toolResult (docId : DocId) (id : String) (callId : Option String)
      (parts : List (ResultPart α))
  | media (value : Media α)
  deriving DecidableEq, Repr

structure MessageEnvelope where
  header : Header
  key : String
  sequence : Nat
  nativeId : Option String
  blocks : List (MessageBlock PayloadSpec)
  createdAt : Time
  deriving DecidableEq, Repr

structure ReconstructedMessage where
  role : MessageRole
  nativeId : Option String
  blocks : List (MessageBlock (List UInt8))
  deriving DecidableEq, Repr

def mediaRefs (media : Media PayloadSpec) : List PayloadRef :=
  match media.data with
  | .url _ | .unknown => []
  | .base64 p | .raw p | .string p => [p.reference]

def blockRefs : MessageBlock PayloadSpec → List PayloadRef
  | .text p => [p.reference]
  | .reasoning _ parts => parts.map fun part =>
      match part with
      | .text p _ | .encrypted p | .redacted p | .summary p => p.reference
  | .toolCall _ _ _ _ p _ _ => [p.reference]
  | .toolResult _ _ _ parts => parts.flatMap fun part =>
      match part with
      | .text p => [p.reference]
      | .media value => mediaRefs value
  | .media value => mediaRefs value

def envelopeRefs (message : MessageEnvelope) : List PayloadRef :=
  message.blocks.flatMap blockRefs

structure PositionedRef where
  block : Nat
  part : Nat
  reference : PayloadRef
  deriving DecidableEq, Repr

def positionedMediaRef (block part : Nat) (media : Media PayloadSpec) : List PositionedRef :=
  (mediaRefs media).map fun reference => ⟨block, part, reference⟩

def positionedReasoningRefs (block : Nat) : Nat → List (ReasoningPart PayloadSpec) →
    List PositionedRef
  | _, [] => []
  | part, head :: tail =>
      let reference := match head with
        | .text payload _ | .encrypted payload | .redacted payload | .summary payload =>
            payload.reference
      ⟨block, part, reference⟩ :: positionedReasoningRefs block (part + 1) tail

def positionedResultRefs (block : Nat) : Nat → List (ResultPart PayloadSpec) →
    List PositionedRef
  | _, [] => []
  | part, head :: tail =>
      let here := match head with
        | .text payload => [⟨block, part, payload.reference⟩]
        | .media media => positionedMediaRef block part media
      here ++ positionedResultRefs block (part + 1) tail

def positionedBlockRefs : Nat → List (MessageBlock PayloadSpec) → List PositionedRef
  | _, [] => []
  | block, head :: tail =>
      let here := match head with
        | .text payload => [⟨block, 0, payload.reference⟩]
        | .reasoning _ parts => positionedReasoningRefs block 0 parts
        | .toolCall _ _ _ _ payload _ _ => [⟨block, 0, payload.reference⟩]
        | .toolResult _ _ _ parts => positionedResultRefs block 0 parts
        | .media media => positionedMediaRef block 0 media
      here ++ positionedBlockRefs (block + 1) tail

structure ToolIntent where
  call : DocId
  providerId : String
  name : String
  arguments : PayloadRef
  deriving DecidableEq, Repr

def toolIntents (message : MessageEnvelope) : List ToolIntent :=
  message.blocks.filterMap fun block => match block with
    | .toolCall call id _ name args _ _ => some ⟨call, id, name, args.reference⟩
    | _ => none

def roleAllows {α : Type} (role : MessageRole) : MessageBlock α → Bool
  | .text _ => true
  | .reasoning _ _ | .toolCall _ _ _ _ _ _ _ => role == .assistant
  | .toolResult _ _ _ parts => role == .user && parts.all (fun part =>
      match part with | .text _ => true | .media value => value.kind == .image)
  | .media value => role == .user || (role == .assistant && value.kind == .image)

/-- This is the structural shape of the persisted native message, not the
provider-input boundary. Native user and assistant block vectors may be empty
so a payload-free publication needs no invented bytes. The owned provider-input
adapter may impose its conventional nonempty-message requirement later. -/
def validNativeShape (message : MessageEnvelope) : Bool :=
  message.blocks.all (roleAllows message.header.role) &&
    (match message.header.role with
    | .system => message.nativeId.isNone && match message.blocks with
        | [.text _] => true
        | _ => false
    | .user => message.nativeId.isNone
    | .assistant => true)

def recoveryTextOnly : List (MessageBlock PayloadSpec) → Bool
  | [] => true
  | .text payload :: rest => payload.presentation == .full && recoveryTextOnly rest
  | _ => false

/-- Publication provenance constrains the header even when it has no payload.
Recovery is the deliberately narrow text-only fallback; forks detach request
membership and carry one exact origin. -/
def validPublicationShape (message : MessageEnvelope) : Bool :=
  match message.header.publication with
  | .requestExecution _ => message.header.request.isSome && message.header.origin.isNone
  | .requestRecovery _ =>
      message.header.request.isSome && message.header.origin.isNone &&
      message.header.role == .assistant && message.header.outcome == .partial &&
      message.nativeId.isNone && recoveryTextOnly message.blocks
  | .toolDelivery call =>
      message.header.request.isSome && message.header.origin.isNone &&
      message.header.role == .user && message.blocks.all (fun block => match block with
        | .text _ => true
        | .toolResult doc _ _ _ => doc == call
        | _ => false)
  | .fork origin =>
      message.header.request.isNone && message.header.origin == some origin

/-- Native JSON parsing is executable. Exact source bytes remain the result;
parsing never serializes or repairs them. Media bytes below retain their encoded
native representation; the platform media decoder is a separate bridge check. -/
def resolveMessagePayload (records : List Segment) (denied : List DocId)
    (allowed : List PayloadKind) (json : Bool) (spec : PayloadSpec) :
    Except MessageError (List UInt8) := do
  let (declaration, bytes) ← (reconstructPayload records denied spec.reference).mapError
    MessageError.reconstruction
  if !(allowed.contains declaration.kind) then .error .wrongPayloadKind
  else if !(allowed.contains .text || allowed.contains .toolOutput) &&
      spec.presentation != .full then .error .invalidPresentation
  else
    let rendered ← present bytes spec.presentation
    if json then
      match utf8? rendered with
      | none => .error .invalidUtf8
      | some text => match Lean.Json.parse text with
        | .error _ => .error .invalidJson
        | .ok _ => .ok rendered
    else .ok rendered

def declaredPayload (records : List Segment) (denied : List DocId)
    (spec : PayloadSpec) : Except MessageError (Segment × Declaration) := do
  let closing ← (resolveClose records denied spec.reference).mapError
    (fun error => MessageError.reconstruction (.lookup error))
  let (declaration, _) ← (reconstructPayload records denied spec.reference).mapError
    MessageError.reconstruction
  return (closing, declaration)

def publicationAllowsSource (header : Header) (closing : Segment) : Bool :=
  match header.publication with
  | .requestExecution generation =>
      header.request == some closing.coordinate.request &&
      match closing.coordinate.source, closing.writer with
      | .provider _ _ _, .request writerGeneration => writerGeneration == generation
      | .authored _, .request writerGeneration => writerGeneration == generation
      | _, _ => false
  | .requestRecovery _ =>
      header.request == some closing.coordinate.request &&
      match closing.coordinate.source, closing.writer with
      | .provider _ _ _, .request _ => true
      | _, _ => false
  | .toolDelivery call =>
      match closing.coordinate.source, closing.writer with
      | .tool sourceCall, .tool writerCall => sourceCall == call && writerCall == call
      /- Authored immediate receipts remain in the parent request. Ordinary
      background notifications may belong to a separately authenticated wake
      request while retaining exact refs to the parent tool source above. -/
      | .authored _, .tool writerCall =>
          header.request == some closing.coordinate.request && writerCall == call
      | _, _ => false
  | .fork _ => true

def validateReferenceSource (records : List Segment) (denied : List DocId)
    (header : Header) (reference : PayloadRef) : Except MessageError Unit := do
  let closing ← (resolveClose records denied reference).mapError
    (fun error => MessageError.reconstruction (.lookup error))
  if publicationAllowsSource header closing then .ok () else .error .wrongSource

def validateMediaDeclaration (records : List Segment) (denied : List DocId)
    (media : Media PayloadSpec) : Except MessageError Unit :=
  match media.data with
  | .url _ | .unknown => .ok ()
  | .base64 payload | .raw payload | .string payload => do
      let (_, declaration) ← declaredPayload records denied payload
      if declaration.kind == .media && declaration.mediaKind == some media.kind then .ok ()
      else .error .metadataMismatch

def validateToolResultSource (records : List Segment) (denied : List DocId)
    (header : Header) (call : DocId) (reference : PayloadRef) : Except MessageError Unit := do
  let closing ← (resolveClose records denied reference).mapError
    (fun error => MessageError.reconstruction (.lookup error))
  match closing.coordinate.source, closing.writer with
  | .tool sourceCall, .tool writerCall =>
      if sourceCall == call && writerCall == call then .ok () else .error .wrongSource
  | .authored _, .tool writerCall =>
      let publicationMatches := match header.publication with
        | .toolDelivery deliveryCall =>
            deliveryCall == call && header.request == some closing.coordinate.request
        /- A fork retains exact origin refs. Its closure is validated by the
        fork/hydration owner rather than rewritten into the child request. -/
        | .fork _ => true
        | _ => false
      if writerCall == call && publicationMatches then .ok () else .error .wrongSource
  | _, _ => .error .wrongSource

def validateBlockMetadata (records : List Segment) (denied : List DocId) :
    Header → MessageBlock PayloadSpec → Except MessageError Unit
  | _, .text _ | _, .reasoning _ _ => .ok ()
  | _, .toolCall _ id callId name arguments _ _ => do
      let (_, declaration) ← declaredPayload records denied arguments
      if declaration.kind == .arguments &&
          declaration.tool == some ⟨id, callId, name⟩ then .ok ()
      else .error .metadataMismatch
  | header, .toolResult call _ _ parts =>
      parts.forM fun part => match part with
      | .text payload => validateToolResultSource records denied header call payload.reference
      | .media media => do
          let _ ← validateMediaDeclaration records denied media
          match media.data with
          | .url _ | .unknown => .ok ()
          | .base64 payload | .raw payload | .string payload =>
              validateToolResultSource records denied header call payload.reference
  | _, .media media => validateMediaDeclaration records denied media

def reconstructMedia (resolve : List PayloadKind → Bool → PayloadSpec →
    Except MessageError (List UInt8)) (media : Media PayloadSpec) :
    Except MessageError (Media (List UInt8)) := do
  let data : MediaData (List UInt8) ← match media.data with
    | .url url => pure (.url url)
    | .unknown => pure .unknown
    | .base64 p => do pure (.base64 (← resolve [.media] false p))
    | .raw p => do pure (.raw (← resolve [.media] false p))
    | .string p => do pure (.string (← resolve [.media] false p))
  return ⟨media.kind, data, media.mediaType, media.detail, media.additionalParams⟩

def reconstructBlock (resolve : List PayloadKind → Bool → PayloadSpec →
    Except MessageError (List UInt8)) :
    MessageBlock PayloadSpec → Except MessageError (MessageBlock (List UInt8))
  | .text p => return .text (← resolve [.text, .toolOutput] false p)
  | .reasoning id parts => do
      let parts ← parts.mapM fun part => match part with
        | .text p signature => return .text (← resolve [.reasoning] false p) signature
        | .encrypted p => return .encrypted (← resolve [.opaque] false p)
        | .redacted p => return .redacted (← resolve [.opaque] false p)
        | .summary p => return .summary (← resolve [.summary] false p)
      return .reasoning id parts
  | .toolCall doc id call name args sig extra =>
      return .toolCall doc id call name (← resolve [.arguments] true args) sig extra
  | .toolResult doc id call parts => do
      let parts ← parts.mapM fun part => match part with
        | .text p => return .text (← resolve [.toolOutput] false p)
        | .media value => return .media (← reconstructMedia resolve value)
      return .toolResult doc id call parts
  | .media value => return .media (← reconstructMedia resolve value)

def providerPositions (records : List Segment) (denied : List DocId)
    (refs : List PayloadRef) : Except MessageError (List (Coordinate × Declaration)) := do
  let positions ← refs.mapM fun reference => do
    let closing ← (resolveClose records denied reference).mapError
      (fun error => MessageError.reconstruction (.lookup error))
    match closing.coordinate.source with
    | .provider _ _ _ =>
        let (declaration, _) ← (reconstructPayload records denied reference).mapError
          MessageError.reconstruction
        pure (some (closing.coordinate, declaration))
    | _ => pure none
  pure (positions.filterMap id)

/-- Each provider source keeps its native order, including signed reasoning.
Recovery may leave gaps; tool-owned output can be wrapped/reused at new header
positions and is not mistakenly compared with its source's position. -/
def nativePositionsOrdered : List (Coordinate × Declaration) → Bool
  | [] => true
  | head :: tail =>
      tail.all (fun next => head.1 != next.1 || declarationBefore head.2 next.2) &&
        nativePositionsOrdered tail

def validateProviderOrder (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) : Except MessageError Unit := do
  let positions ← providerPositions records denied (envelopeRefs message)
  if nativePositionsOrdered positions then .ok () else .error .nativeOrder

def validateExactProviderPositions (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) : Except MessageError Unit :=
  (positionedBlockRefs 0 message.blocks).forM fun expected => do
    let closing ← (resolveClose records denied expected.reference).mapError
      (fun error => MessageError.reconstruction (.lookup error))
    match closing.coordinate.source with
    | .provider _ _ _ =>
        let (declaration, _) ← (reconstructPayload records denied expected.reference).mapError
          MessageError.reconstruction
        if declaration.block == expected.block && declaration.part == expected.part then .ok ()
        else .error .nativePosition
    | _ => .ok ()

def validateRecoveryPositions (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) : Except MessageError Unit := do
  let _ ← validateProviderOrder records denied message
  (envelopeRefs message).forM fun reference => do
    let closing ← (resolveClose records denied reference).mapError
      (fun error => MessageError.reconstruction (.lookup error))
    match closing.coordinate.source with
    | .provider _ _ _ =>
        let (declaration, _) ← (reconstructPayload records denied reference).mapError
          MessageError.reconstruction
        if declaration.kind == .text && declaration.part == 0 then .ok ()
        else .error .nativePosition
    | _ => .error .wrongSource

def validateMessageStructure (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) : Except MessageError Unit := do
  if !validPublicationShape message then .error .invalidPublication
  else
    let _ ← (envelopeRefs message).forM
      (validateReferenceSource records denied message.header)
    let _ ← message.blocks.forM (validateBlockMetadata records denied message.header)
    match message.header.publication with
    | .requestExecution _ => validateExactProviderPositions records denied message
    | .requestRecovery _ => validateRecoveryPositions records denied message
    | .toolDelivery _ | .fork _ => validateProviderOrder records denied message

def reconstructMessage (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) : Except MessageError ReconstructedMessage := do
  if message.header.refs ≠ envelopeRefs message then .error .referenceMismatch
  else if !validNativeShape message then .error .invalidRole
  else
    let _ ← validateMessageStructure records denied message
    let blocks ← message.blocks.mapM (reconstructBlock (resolveMessagePayload records denied))
    return ⟨message.header.role, message.nativeId, blocks⟩

theorem reconstruction_preserves_native_identity
    (records : List Segment) (denied : List DocId)
    (message : MessageEnvelope) (native : ReconstructedMessage)
    (h : reconstructMessage records denied message = .ok native) :
    native.role = message.header.role ∧ native.nativeId = message.nativeId := by
  simp only [reconstructMessage] at h
  split at h
  · contradiction
  · split at h
    · contradiction
    · cases horder : validateMessageStructure records denied message with
      | error error => simp [horder, Bind.bind, Except.bind] at h
      | ok validated =>
        cases hblocks : message.blocks.mapM
            (reconstructBlock (resolveMessagePayload records denied)) with
        | error error => simp [horder, hblocks, Bind.bind, Except.bind] at h
        | ok blocks =>
            simp [horder, hblocks, Bind.bind, Except.bind, pure] at h
            cases h
            exact ⟨rfl, rfl⟩

theorem signed_reasoning_keeps_signature
    (resolve : List PayloadKind → Bool → PayloadSpec → Except MessageError (List UInt8))
    (id signature : Option String) (payload : PayloadSpec) (bytes : List UInt8)
    (h : resolve [.reasoning] false payload = .ok bytes) :
    reconstructBlock resolve (.reasoning id [.text payload signature]) =
      .ok (.reasoning id [.text bytes signature]) := by simp [reconstructBlock, h]

end CanonicalOutput
