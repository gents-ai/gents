import Proofs.StreamingResponse.Transition

namespace StreamingResponse

open CanonicalOutput

theorem auxiliary_audit_is_never_public (observation : Observation)
    (kind : AuxiliaryKind) (scope turn attempt : Nat)
    (h : observation.target.coordinate.source = .auxiliary kind scope turn attempt) :
    project observation = .absent := by
  simp [project, h, Source.isAuxiliary]

theorem messageScoped_implies_scopeResult_ok {observation : Observation}
    {message : MessageEnvelope} (h : messageScoped observation message = true) :
    messageScopeResult observation message = .ok () := by
  unfold messageScoped at h
  unfold messageScopeResult
  cases hp : message.header.publication <;> simp_all
  rename_i origin
  split at h <;> simp_all

theorem projectPublished_typed {observation : Observation} {id : DocId}
    {message : MessageEnvelope} {native : ReconstructedMessage}
    (hdenied : id ∉ observation.deniedHeaders)
    (hmessage : messageAt observation.messages id = .ok message)
    (hcoordinate : messageCoordinateConflict observation.messages message = false)
    (hscope : messageScoped observation message = true)
    (hdependencies : observation.dependencyDenials.any (fun denial =>
      message.header.refs.any fun ref => ref.closeId == denial.rootCloseId) = false)
    (hnative :
      reconstructMessage observation.records observation.deniedSegments message = .ok native) :
    projectPublished observation id = .published message native := by
  simp [projectPublished, hdenied, hmessage,
    hcoordinate, messageScoped_implies_scopeResult_ok hscope, hdependencies, hnative]

namespace ForkOriginCases

def forkFixtureHeader : Header :=
  { id := 2
    session := 1
    request := none
    origin := some 1
    refs := []
    outcome := .complete
    role := .assistant
    publication := .fork 1 }

def forkMessage : MessageEnvelope :=
  { header := forkFixtureHeader
    key := "fork", sequence := 0, nativeId := none, blocks := [], createdAt := 1 }

def originFixtureHeader : Header :=
  { forkFixtureHeader with
    id := 1
    request := some 10
    origin := none
    publication := .requestExecution 7 }

def originMessage : MessageEnvelope :=
  { forkMessage with header := originFixtureHeader }

def observation (messages : List MessageEnvelope) (denied : List DocId := []) : Observation :=
  { request := 20, session := 1, records := [], messages := messages
    deniedHeaders := denied, deniedSegments := [], dependencyDenials := []
    owner := ⟨none, []⟩
    target := ⟨⟨20, .provider 0 0 0⟩, .request 7, some 2⟩
    requestTerminal := false, terminalSelection := none }

example : projectPublished (observation [forkMessage]) 2 = .loading := by native_decide
example : projectPublished (observation [forkMessage] [1]) 2 = .denied := by native_decide
example : projectPublished
    (observation [forkMessage, originMessage, { originMessage with key := "twin" }]) 2 =
      .conflicted := by native_decide

end ForkOriginCases

namespace PrefixCases

def first : Segment :=
  { id := 100
    coordinate := ⟨10, .provider 0 0 0⟩
    writer := .request 7
    flush := some ⟨0, [⟨0, 1, some { block := 0, part := 0, kind := .text }⟩], [65]⟩
    close := none, createdAt := 1 }

def gap : Segment :=
  { first with
    id := 102
    createdAt := 2
    flush := some ⟨2, [⟨0, 1, none⟩], [67]⟩ }

def closing : Segment :=
  { first with
    id := 103
    createdAt := 3
    flush := none
    close := some (.closed .complete 3 [3]) }

def base : Observation :=
  { request := 10
    session := 1
    records := [first]
    messages := []
    deniedHeaders := []
    deniedSegments := []
    dependencyDenials := []
    owner := ⟨some (10, 7), []⟩
    target := ⟨first.coordinate, .request 7, none⟩
    requestTerminal := false
    terminalSelection := none }

def afterGap : Observation :=
  { base with
    records := CanonicalOutput.deliver base.records gap }

def afterClose : Observation :=
  { afterGap with
    records := CanonicalOutput.deliver afterGap.records closing }

example : project base = .live [⟨{ block := 0, part := 0, kind := .text }, [65]⟩] := by
  native_decide

example : project afterGap = .live [⟨{ block := 0, part := 0, kind := .text }, [65]⟩] := by
  native_decide

example : project afterClose =
    .settling [⟨{ block := 0, part := 0, kind := .text }, [65]⟩] := by
  native_decide

end PrefixCases

theorem unheaded_authored_owner_is_never_live {observation : Observation} {key : Nat}
    (h : observation.target.coordinate.source = .authored key) :
    ownerLive observation = false := by
  simp [ownerLive, h]

theorem authored_open_source_is_not_previewed {observation : Observation} {key : Nat}
    (hsource : observation.target.coordinate.source = .authored key)
    (hmessage : observation.target.messageId = none)
    (hscope : targetScoped observation = true)
    (hclose : observeClose observation.records observation.target.coordinate = .open) :
    project observation =
      if sourceDenied observation then .denied else .absent := by
  simp [project, hscope, hmessage, hclose, ownerLive, hsource, Source.isAuxiliary]

theorem open_preview_requires_validated_prefix {observation : Observation}
    {streams : Streams}
    (hmessage : observation.target.messageId = none)
    (hscope : targetScoped observation = true)
    (hclose : observeClose observation.records observation.target.coordinate = .open)
    (hdenied : sourceDenied observation = false)
    (hlive : ownerLive observation = true)
    (hreconstruct : reconstructOpen observation = .ok streams) :
    project observation = .live streams := by
  have haux : observation.target.coordinate.source.isAuxiliary = false := by
    cases hsource : observation.target.coordinate.source with
    | auxiliary kind scope turn attempt => simp [ownerLive, hsource] at hlive
    | provider scope turn attempt => simp [Source.isAuxiliary]
    | tool call => simp [Source.isAuxiliary]
    | authored key => simp [Source.isAuxiliary]
  simp [project, haux, hscope, hmessage, hclose, hdenied, hlive, hreconstruct]

theorem closed_waiting_preserves_prefix_without_claiming_live
    {observation : Observation} {closing : Segment} {outcome : Outcome}
    {streams : Streams}
    (hscope : targetScoped observation = true)
    (hdenied : sourceDenied observation = false)
    (hdependency : observation.dependencyDenials.any
      (fun denial => denial.rootCloseId == closing.id) = false)
    (hterminal : observation.requestTerminal = false)
    (hprefix : reconstructBeforeClose observation closing = .ok streams) :
    projectUnheadedClosed observation closing outcome = .settling streams := by
  simp [projectUnheadedClosed, hscope, hdenied, hdependency, hterminal,
    loadingOrSettlingBeforeClose, hprefix]

theorem open_denial_is_not_loading {observation : Observation}
    (hmessage : observation.target.messageId = none)
    (haux : observation.target.coordinate.source.isAuxiliary = false)
    (hscope : targetScoped observation = true)
    (hclose : observeClose observation.records observation.target.coordinate = .open)
    (hdenied : sourceDenied observation = true) :
    project observation = .denied := by
  simp [project, haux, hscope, hmessage, hclose, hdenied]

theorem nonterminal_partial_is_loading {observation : Observation}
    (closing : Segment) :
    targetScoped observation = true →
      sourceDenied observation = false →
      observation.dependencyDenials.any (fun denial => denial.rootCloseId == closing.id) = false →
      observation.requestTerminal = false →
      reconstructBeforeClose observation closing = .error .loading →
      projectUnheadedClosed observation closing .partial = .loading := by
  intro hscope hdenied hdependency h hprefix
  simp [projectUnheadedClosed, hscope, hdenied, hdependency, h,
    loadingOrSettlingBeforeClose, hprefix]

theorem terminal_selection_missing_is_loading {observation : Observation}
    (closing : Segment)
    (hscope : targetScoped observation = true)
    (hdenied : sourceDenied observation = false)
    (hdependency :
      observation.dependencyDenials.any (fun denial => denial.rootCloseId == closing.id) = false)
    (hterminal : observation.requestTerminal = true)
    (hselection : observation.terminalSelection = none) :
    projectUnheadedClosed observation closing .partial = .loading := by
  simp [projectUnheadedClosed, hscope, hdenied, hdependency, hterminal, hselection,
    resolveTerminalPayload, resolveTerminal, classifyTerminalError, Except.mapError,
    Bind.bind, Except.bind]

private theorem classifyMessageError_ne_live (error : MessageError) (streams : Streams) :
    classifyMessageError error ≠ .live streams := by
  cases error with
  | reconstruction error =>
      cases error with
      | lookup error => cases error <;> simp [classifyMessageError]
      | extent error => cases error <;> simp [classifyMessageError]
      | missingStream => simp [classifyMessageError]
  | _ => simp [classifyMessageError]

private theorem classifyTerminalError_ne_live (error : TerminalPayloadError)
    (streams : Streams) : classifyTerminalError error ≠ .live streams := by
  cases error with
  | selection error => cases error <;> simp [classifyTerminalError]
  | conflictingMessage => simp [classifyTerminalError]
  | reconstruction error =>
      simpa [classifyTerminalError] using classifyMessageError_ne_live error streams

private theorem loadingOrSettling_ne_live (observation : Observation) (streams : Streams) :
    loadingOrSettling observation ≠ .live streams := by
  unfold loadingOrSettling
  cases reconstructOpen observation <;> simp
  rename_i error
  cases error <;> simp

private theorem loadingOrSettlingBeforeClose_ne_live (observation : Observation)
    (closing : Segment) (streams : Streams) :
    loadingOrSettlingBeforeClose observation closing ≠ .live streams := by
  unfold loadingOrSettlingBeforeClose
  cases reconstructBeforeClose observation closing <;> simp
  rename_i error
  cases error <;> simp

private def resolveReferencingCandidate (observation : Observation)
    (candidate : MessageEnvelope) : Except View Header :=
  match messageAt observation.messages candidate.header.id with
  | .error _ => .error .conflicted
  | .ok message =>
      if observation.dependencyDenials.any fun denial =>
          message.header.refs.any fun ref => ref.closeId == denial.rootCloseId then
        .error .denied
      else match reconstructMessage observation.records observation.deniedSegments message with
        | .ok _ => .ok message.header
        | .error error => .error (classifyMessageError error)

private theorem resolveReferencingCandidate_error_ne_live
    (observation : Observation) (candidate : MessageEnvelope)
    (view : View) (streams : Streams)
    (h : resolveReferencingCandidate observation candidate = .error view) :
    view ≠ .live streams := by
  unfold resolveReferencingCandidate at h
  split at h
  · cases h
    simp
  · split at h
    · cases h
      simp
    · split at h
      · contradiction
      · cases h
        exact classifyMessageError_ne_live _ streams

private theorem resolveReferencingCandidates_error_ne_live
    (observation : Observation) (candidates : List MessageEnvelope)
    (view : View) (streams : Streams)
    (h : candidates.mapM (resolveReferencingCandidate observation) = .error view) :
    view ≠ .live streams := by
  induction candidates with
  | nil =>
      change Except.ok [] = Except.error view at h
      contradiction
  | cons candidate rest ih =>
      cases hcandidate : resolveReferencingCandidate observation candidate with
      | error error =>
          rw [List.mapM_cons] at h
          simp [hcandidate] at h
          cases h
          exact resolveReferencingCandidate_error_ne_live observation candidate view streams
            hcandidate
      | ok header =>
          rw [List.mapM_cons] at h
          cases hrest : rest.mapM (resolveReferencingCandidate observation) with
          | error error =>
              simp [hcandidate, hrest] at h
              cases h
              exact ih hrest
          | ok headers =>
              simp [hcandidate, hrest] at h
              change Except.ok (header :: headers) = Except.error view at h
              contradiction

private theorem resolvedReferencingHeaders_error_ne_live
    (observation : Observation) (closing : Segment) (view : View) (streams : Streams)
    (h : resolvedReferencingHeaders observation closing = .error view) :
    view ≠ .live streams := by
  unfold resolvedReferencingHeaders at h
  change ((observation.messages.filter fun message =>
      referencesClose message closing &&
      (match message.header.publication with
       | .toolDelivery _ => true
       | _ => message.header.request == some observation.request) &&
      message.header.session == observation.session).dedup.mapM
        (resolveReferencingCandidate observation)) = .error view at h
  exact resolveReferencingCandidates_error_ne_live observation _ view streams h

/-- Once the exact source has a unique closing record, the live-preview branch
is unreachable. Closed output may still be loading while terminal selection or
dependencies arrive, but it is never represented as a live writer. -/
theorem projectUnheadedClosed_ne_live (observation : Observation)
    (closing : Segment) (outcome : Outcome) (streams : Streams) :
    projectUnheadedClosed observation closing outcome ≠ .live streams := by
  unfold projectUnheadedClosed
  split <;> try simp
  split <;> try simp
  split
  · exact loadingOrSettlingBeforeClose_ne_live observation closing streams
  · split
    · exact classifyTerminalError_ne_live _ streams
    · split
      · split <;> try simp
        · exact resolvedReferencingHeaders_error_ne_live observation closing _ streams (by assumption)
        · split <;> first | exact loadingOrSettlingBeforeClose_ne_live observation closing streams | simp
      · exact loadingOrSettlingBeforeClose_ne_live observation closing streams

theorem closed_source_is_never_live {observation : Observation}
    {closing : Segment} {outcome : Outcome} {streams : Streams}
    (hmessage : observation.target.messageId = none)
    (hscope : targetScoped observation = true)
    (hclosed : observeClose observation.records observation.target.coordinate =
      .closed closing outcome) :
    project observation ≠ .live streams := by
  simp [project, hmessage, hscope, hclosed]
  split
  · simp
  · exact projectUnheadedClosed_ne_live observation closing outcome streams

theorem retained_stream_is_not_private {headers : List Header} {closing : Segment}
    {streams : Streams} {stream : Declaration × List UInt8}
    (h : stream ∈ retainedStreams headers closing streams) :
    stream.1.kind ≠ .encrypted ∧ stream.1.kind ≠ .redacted ∧
      stream.1.kind ≠ .signature := by
  simp [retainedStreams, visibleStreams] at h
  exact ⟨h.2.1.1, h.2.1.2, h.2.2⟩

theorem retained_stream_filter_characterization (headers : List Header)
    (closing : Segment) (streams : Streams) :
    retainedStreams headers closing streams =
      ((streams.zipIdx.filter fun entry =>
        !streamReferenced headers closing entry.2).map (·.1)).filter
          (fun stream => stream.1.kind != .encrypted &&
            stream.1.kind != .redacted && stream.1.kind != .signature) := rfl

theorem segment_delivery_retains_old (observation : Observation)
    (record old : Segment) (hold : old ∈ observation.records) :
    old ∈ CanonicalOutput.deliver observation.records record :=
  CanonicalOutput.delivery_retains _ _ _ hold

/-- Every modeled observation transition is append-only with respect to segment
facts. Owner changes, selection and terminal observation also leave them intact. -/
theorem transition_never_removes_segments {before after : Observation}
    (htransition : Transition before after) (old : Segment)
    (hold : old ∈ before.records) : old ∈ after.records := by
  cases htransition <;> simp_all [CanonicalOutput.delivery_retains]

theorem trace_never_removes_segments {before after : Observation}
    (htrace : Trace before after) (old : Segment)
    (hold : old ∈ before.records) : old ∈ after.records := by
  induction htrace with
  | refl => exact hold
  | step transition rest ih =>
      exact ih (transition_never_removes_segments transition old hold)

end StreamingResponse
