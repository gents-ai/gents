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
  | settling (streams : Streams)
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

/-- Scope failure is structural invalidity, but an unavailable fork origin is
still a replica observation and retains its missing/denied/conflict class. -/
def messageScopeResult (observation : Observation)
    (message : MessageEnvelope) : Except View Unit :=
  if message.header.session != observation.session ||
      !(message.header.id == observation.target.messageId) then .error .invalid
  else match message.header.publication with
  | .fork origin =>
      if message.header.request.isSome || message.header.origin != some origin then
        .error .invalid
      else match CanonicalOutput.Hydration.lookupMessage observation.messages
          observation.deniedHeaders origin with
      | .error .headerUnavailable => .error .loading
      | .error .headerDenied => .error .denied
      | .error .conflictingMessages => .error .conflicted
      | .error _ => .error .invalid
      | .ok originMessage =>
          if CanonicalOutput.Hydration.forkMetadataMatches message originMessage then .ok ()
          else .error .invalid
  | _ => if message.header.request == some observation.request then .ok ()
      else .error .invalid

def visibleStreams (streams : Streams) : Streams :=
  streams.filter fun stream =>
    stream.1.kind != .encrypted && stream.1.kind != .redacted &&
      stream.1.kind != .signature

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

def messageCoordinateConflict (messages : List MessageEnvelope)
    (message : MessageEnvelope) : Bool :=
  messages.any fun other => other != message &&
    (other.header.id == message.header.id ||
      (other.header.session == message.header.session && other.key == message.key) ||
      (other.header.session == message.header.session && other.sequence == message.sequence))

/-- Contiguous ordinal reconstruction for an open source. This stops at the
first gap and rejects twins, malformed consumed runs, wrong writers and timestamp regressions.
It is never used for authored output, which requires an immutable header. -/
inductive OpenError where | loading | conflicted | invalid
  deriving DecidableEq, Repr

def contiguousFlushes (records : List Segment) (writer : Writer) :
    Nat → Nat → Except OpenError (List Flush)
  | _, 0 => .ok []
  | ordinal, fuel + 1 =>
      match flushAt records writer ordinal with
      | .error (.missingOrdinal _) => .ok []
      | .error (.conflictingOrdinal _) => .error .conflicted
      | .error _ => .error .invalid
      | .ok flush => do
          let rest ← contiguousFlushes records writer (ordinal + 1) fuel
          .ok (flush :: rest)

/-- The durable audit reads the same validated dense prefix as live output, but
keeps every received field and native declaration. A missing replica ordinal
ends the observed prefix; closure state never widens its replay eligibility. -/
def reconstructAuditPrefix (observation : Observation) (limit : Option Nat) :
    Except OpenError Streams := do
  let source := CanonicalOutput.Execution.sourceData
    observation.records observation.target.coordinate
  let data := match limit with
    | none => source
    | some count => source.filter fun (record : Segment) =>
        record.flush.any (fun (flush : Flush) => flush.ordinal < count)
  if data.isEmpty then .error .loading
  else if data.all (fun record => record.writer == observation.target.writer) = false then
    .error .invalid
  else if data.all (CanonicalOutput.exactIdentityAt data) = false then .error .invalid
  else if CanonicalOutput.timestampsNondecreasing data = false then .error .invalid
  else if !(data.filterMap (fun (record : Segment) =>
      record.flush.map (fun (flush : Flush) => flush.ordinal))).Nodup then
    .error .conflicted
  else
  let flushes ← contiguousFlushes data observation.target.writer 0 (limit.getD data.length)
  if flushes.isEmpty then .error .loading else
  match consumeFlushes flushes [] with
  | .ok streams => .ok streams
  | .error _ => .error .invalid

def reconstructPrefix (observation : Observation) (limit : Option Nat) :
    Except OpenError Streams :=
  (reconstructAuditPrefix observation limit).map visibleStreams

def reconstructOpen (observation : Observation) : Except OpenError Streams :=
  reconstructPrefix observation none

def reconstructBeforeClose (observation : Observation) (closing : Segment) :
    Except OpenError Streams :=
  match closing.close with
  | some (.closed _ count _) => reconstructPrefix observation (some count)
  | _ => .error .invalid

def loadingOrSettling (observation : Observation) : View :=
  match reconstructOpen observation with
  | .ok streams => .settling streams
  | .error .loading => .loading
  | .error .conflicted => .conflicted
  | .error .invalid => .invalid

def loadingOrSettlingBeforeClose (observation : Observation) (closing : Segment) : View :=
  match reconstructBeforeClose observation closing with
  | .ok streams => .settling streams
  | .error .loading => .loading
  | .error .conflicted => .conflicted
  | .error .invalid => .invalid

private def referencedTargetClose? (observation : Observation) :
    List PayloadRef → Option Segment
  | [] => none
  | ref :: rest =>
      match resolveClose observation.records observation.deniedSegments ref with
      | .ok closing =>
          if closing.coordinate == observation.target.coordinate &&
              closing.writer == observation.target.writer then some closing
          else referencedTargetClose? observation rest
      | .error _ => referencedTargetClose? observation rest

def settlingForReferencedTarget (observation : Observation)
    (message : MessageEnvelope) : View :=
  if sourceDenied observation then .denied
  else match referencedTargetClose? observation message.header.refs with
  | some closing => loadingOrSettlingBeforeClose observation closing
  | none => .loading

def projectPublished (observation : Observation) (id : DocId) : View :=
  if id ∈ observation.deniedHeaders then .denied
  else match messageAt observation.messages id with
    | .error () =>
        if (observation.messages.filter fun message => message.header.id == id).isEmpty then
          .loading
        else .conflicted
    | .ok message =>
        if messageCoordinateConflict observation.messages message then .conflicted
        else match messageScopeResult observation message with
        | .error view => view
        | .ok () => if observation.dependencyDenials.any fun denial =>
            message.header.refs.any fun ref => ref.closeId == denial.rootCloseId then .denied
          else match reconstructMessage observation.records observation.deniedSegments message with
            | .ok native => .published message native
            | .error (.reconstruction (.lookup .unavailable)) => .loading
            | .error (.reconstruction (.extent (.missingOrdinal _))) =>
                settlingForReferencedTarget observation message
            | .error error => classifyMessageError error

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
      (match message.header.publication with
       | .toolDelivery _ => true
       | _ => message.header.request == some observation.request) &&
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
  else if !observation.requestTerminal then loadingOrSettlingBeforeClose observation closing
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
              | .error (.missingOrdinal _) => loadingOrSettlingBeforeClose observation closing
              | .error (.conflictingOrdinal _) => .conflicted
              | .error _ => .invalid
              | .ok streams => .retainedPartial (retainedStreams headers closing streams)
        | _, _ => loadingOrSettlingBeforeClose observation closing

def project (observation : Observation) : View :=
  if observation.target.coordinate.source.isAuxiliary then .absent
  else
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
