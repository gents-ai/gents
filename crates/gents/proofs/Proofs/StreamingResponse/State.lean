import Proofs.CanonicalOutput.TerminalPayload
import Proofs.CanonicalOutput.Execution.Projection
import Proofs.CanonicalOutput.Hydration

/-!
# Immutable output observation

This replaces the mutable AgentResponse model. Published views come only from a
typed MessageEnvelope that passes full canonical reconstruction. Live preview
is a validated contiguous open prefix owned by the exact live request generation
or tool. Retained Partial diagnostics require an exact terminal selection first.
Request lifecycle, ACP and writer authorization remain supplied observations.
-/
namespace StreamingResponse

open CanonicalOutput

structure OwnerLiveness where
  currentRequest : Option (DocId × Nat)
  liveTools : List DocId
  deriving DecidableEq, Repr

structure Target where
  coordinate : Coordinate
  writer : Writer
  messageId : Option DocId
  deriving DecidableEq, Repr

structure Observation where
  request : DocId
  session : SessionId
  records : List Segment
  messages : List MessageEnvelope
  deniedHeaders : List DocId
  deniedSegments : List DocId
  dependencyDenials : List DependencyDenial
  owner : OwnerLiveness
  target : Target
  requestTerminal : Bool
  terminalSelection : Option TerminalSelection
  deriving DecidableEq, Repr

inductive View where
  | absent
  | live (streams : Streams)
  | loading
  | denied
  | conflicted
  | invalid
  | retracted
  | retainedPartial (streams : Streams)
  | published (message : MessageEnvelope) (native : ReconstructedMessage)
  deriving DecidableEq, Repr

def ownerLive (observation : Observation) : Bool :=
  match observation.target.coordinate.source, observation.target.writer with
  | .provider _ _ _, .request generation =>
      observation.owner.currentRequest ==
        some (observation.target.coordinate.request, generation)
  | .tool sourceCall, .tool writerCall =>
      sourceCall == writerCall && sourceCall ∈ observation.owner.liveTools
  | _, _ => false

def sourceDenied (observation : Observation) : Bool :=
  (sourceRecords observation.records observation.target.coordinate).any
    (fun record => record.id ∈ observation.deniedSegments)

def targetScoped (observation : Observation) : Bool :=
  observation.target.coordinate.request == observation.request

def messageScoped (observation : Observation) (message : MessageEnvelope) : Bool :=
  message.header.session == observation.session &&
    message.header.id == observation.target.messageId &&
    match message.header.publication with
    | .fork origin =>
        message.header.request.isNone && message.header.origin == some origin &&
        match CanonicalOutput.Hydration.lookupMessage observation.messages
            observation.deniedHeaders origin with
        | .ok originMessage =>
            CanonicalOutput.Hydration.forkMetadataMatches message originMessage
        | .error _ => false
    | _ => message.header.request == some observation.request

def visibleStreams (streams : Streams) : Streams :=
  streams.filter fun stream => stream.1.kind != .opaque

inductive CloseObservation where
  | «open»
  | closed (record : Segment) (outcome : Outcome)
  | retracted (record : Segment)
  | conflict
  deriving DecidableEq, Repr

def observeClose (records : List Segment) (coordinate : Coordinate) : CloseObservation :=
  match uniqueRecord Unit.unit Unit.unit (closures records coordinate) with
  | .error () =>
      if (closures records coordinate).isEmpty then .open else .conflict
  | .ok record => match record.close with
      | some (.closed outcome _ _) => .closed record outcome
      | some .retracted => .retracted record
      | none => .conflict

def classifyMessageError : MessageError → View
  | .reconstruction (.lookup .unavailable)
  | .reconstruction (.extent (.missingOrdinal _)) => .loading
  | .reconstruction (.lookup .denied) => .denied
  | .reconstruction (.lookup .conflictingIdentity)
  | .reconstruction (.lookup .conflictingClosures)
  | .reconstruction (.extent (.conflictingOrdinal _)) => .conflicted
  | _ => .invalid

def classifyTerminalError : TerminalPayloadError → View
  | .selection .missingSelection | .selection .missingHeader => .loading
  | .selection .denied => .denied
  | .selection .conflictingHeaders | .conflictingMessage => .conflicted
  | .selection .wrongScope | .selection .ineligibleHeader => .invalid
  | .reconstruction error => classifyMessageError error

def messageAt (messages : List MessageEnvelope) (id : DocId) :
    Except Unit MessageEnvelope :=
  uniqueRecord () () (messages.filter fun message => message.header.id == id)

def projectPublished (observation : Observation) (id : DocId) : View :=
  if id ∈ observation.deniedHeaders then .denied
  else match messageAt observation.messages id with
    | .error () =>
        if (observation.messages.filter fun message => message.header.id == id).isEmpty then
          .loading
        else .conflicted
    | .ok message =>
        if !messageScoped observation message then .invalid
        else if observation.dependencyDenials.any fun denial =>
            message.header.refs.any fun ref => ref.closeId == denial.rootCloseId then .denied
        else match reconstructMessage observation.records observation.deniedSegments message with
          | .ok native => .published message native
          | .error error => classifyMessageError error

/-- Contiguous ordinal reconstruction for an open source. This rejects gaps,
twins, malformed runs, wrong writers and timestamp regressions before preview.
It is never used for authored output, which requires an immutable header. -/
inductive OpenError where | loading | conflicted | invalid
  deriving DecidableEq, Repr

def reconstructOpen (observation : Observation) : Except OpenError Streams := do
  let data := CanonicalOutput.Execution.sourceData
    observation.records observation.target.coordinate
  if data.isEmpty then .error .loading
  else if data.all (fun record => record.writer == observation.target.writer) = false then
    .error .invalid
  else if CanonicalOutput.Execution.timestampsNondecreasing data = false then .error .invalid
  else
  let flushes ← (List.range data.length).mapM fun ordinal =>
    match flushAt data observation.target.writer ordinal with
    | .ok flush => .ok flush
    | .error (.missingOrdinal _) => .error .loading
    | .error (.conflictingOrdinal _) => .error .conflicted
    | .error _ => .error .invalid
  match consumeFlushes flushes [] with
  | .ok streams => .ok (visibleStreams streams)
  | .error _ => .error .invalid

def streamReferenced (headers : List Header) (closing : Segment) (stream : Nat) : Bool :=
  headers.any fun header => header.refs.any fun ref =>
    ref.closeId == closing.id && ref.stream == stream

def retainedStreams (headers : List Header) (closing : Segment) (streams : Streams) :
    Streams :=
  visibleStreams
    ((streams.zipIdx.filter fun entry => !streamReferenced headers closing entry.2).map (·.1))

def referencesClose (message : MessageEnvelope) (closing : Segment) : Bool :=
  message.header.refs.any fun ref => ref.closeId == closing.id

/-- Every visible header that references this source must fully reconstruct
before its refs may be subtracted from retained diagnostics. -/
def resolvedReferencingHeaders (observation : Observation) (closing : Segment) :
    Except View (List Header) :=
  let candidates :=
    (observation.messages.filter fun message =>
      referencesClose message closing &&
      message.header.request == some observation.request &&
      message.header.session == observation.session).dedup
  candidates.mapM fun candidate =>
    match messageAt observation.messages candidate.header.id with
    | .error _ => .error .conflicted
    | .ok message =>
        if observation.dependencyDenials.any fun denial =>
            message.header.refs.any fun ref => ref.closeId == denial.rootCloseId then
          .error .denied
        else match reconstructMessage observation.records observation.deniedSegments message with
          | .ok _ => .ok message.header
          | .error error => .error (classifyMessageError error)

/-- Terminal resolution is performed before classifying unreferenced Partial
streams. A recovery header may reference only selected Text streams; omitted
streams from the same source remain retained diagnostics and never become
authored transcript blocks. -/
def projectUnheadedClosed (observation : Observation) (closing : Segment)
    (outcome : Outcome) : View :=
  if !targetScoped observation then .invalid
  else if sourceDenied observation ||
      observation.dependencyDenials.any (fun denial => denial.rootCloseId == closing.id) then
    .denied
  else if !observation.requestTerminal then .loading
  else match resolveTerminalPayload observation.messages observation.records
      observation.deniedHeaders observation.deniedSegments observation.request
      observation.session observation.terminalSelection observation.dependencyDenials with
    | .error error => classifyTerminalError error
    | .ok _ =>
        match observation.target.coordinate.source, outcome with
        | .provider _ _ _, .partial =>
            match resolvedReferencingHeaders observation closing with
            | .error view => view
            | .ok headers => match reconstructExtent observation.records closing with
              | .error (.missingOrdinal _) => .loading
              | .error (.conflictingOrdinal _) => .conflicted
              | .error _ => .invalid
              | .ok streams => .retainedPartial (retainedStreams headers closing streams)
        | _, _ => .loading

def project (observation : Observation) : View :=
  match observation.target.messageId with
  | some id => projectPublished observation id
  | none =>
      if !targetScoped observation then .invalid
      else
      match observeClose observation.records observation.target.coordinate with
      | .open =>
          if sourceDenied observation then .denied
          else if ownerLive observation then
            match reconstructOpen observation with
            | .ok streams => .live streams
            | .error .loading => .loading
            | .error .conflicted => .conflicted
            | .error .invalid => .invalid
          else .absent
      | .retracted _ => .retracted
      | .conflict => .conflicted
      | .closed closing outcome => projectUnheadedClosed observation closing outcome

end StreamingResponse
