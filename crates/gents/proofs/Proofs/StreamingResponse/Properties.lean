import Proofs.StreamingResponse.Transition

namespace StreamingResponse

open CanonicalOutput

theorem projectPublished_typed {observation : Observation} {id : DocId}
    {message : MessageEnvelope} {native : ReconstructedMessage}
    (hdenied : id ∉ observation.deniedHeaders)
    (hmessage : messageAt observation.messages id = .ok message)
    (hscope : messageScoped observation message = true)
    (hdependencies : observation.dependencyDenials.any (fun denial =>
      message.header.refs.any fun ref => ref.closeId == denial.rootCloseId) = false)
    (hnative :
      reconstructMessage observation.records observation.deniedSegments message = .ok native) :
    projectPublished observation id = .published message native := by
  simp [projectPublished, hdenied, hmessage, hscope, hdependencies, hnative]

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
  simp [project, hscope, hmessage, hclose, ownerLive, hsource]

theorem open_preview_requires_validated_prefix {observation : Observation}
    {streams : Streams}
    (hmessage : observation.target.messageId = none)
    (hscope : targetScoped observation = true)
    (hclose : observeClose observation.records observation.target.coordinate = .open)
    (hdenied : sourceDenied observation = false)
    (hlive : ownerLive observation = true)
    (hreconstruct : reconstructOpen observation = .ok streams) :
    project observation = .live streams := by
  simp [project, hscope, hmessage, hclose, hdenied, hlive, hreconstruct]

theorem open_denial_is_not_loading {observation : Observation}
    (hmessage : observation.target.messageId = none)
    (hscope : targetScoped observation = true)
    (hclose : observeClose observation.records observation.target.coordinate = .open)
    (hdenied : sourceDenied observation = true) :
    project observation = .denied := by
  simp [project, hscope, hmessage, hclose, hdenied]

theorem nonterminal_partial_is_loading {observation : Observation}
    (closing : Segment) :
    targetScoped observation = true →
      sourceDenied observation = false →
      observation.dependencyDenials.any (fun denial => denial.rootCloseId == closing.id) = false →
      observation.requestTerminal = false →
      projectUnheadedClosed observation closing .partial = .loading := by
  intro hscope hdenied hdependency h
  simp [projectUnheadedClosed, hscope, hdenied, hdependency, h]

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
      message.header.request == some observation.request &&
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
  split <;> try simp
  split
  · exact classifyTerminalError_ne_live _ streams
  · split <;> try simp
    split <;> try simp
    · exact resolvedReferencingHeaders_error_ne_live observation closing _ streams (by assumption)
    · split <;> simp

theorem closed_source_is_never_live {observation : Observation}
    {closing : Segment} {outcome : Outcome} {streams : Streams}
    (hmessage : observation.target.messageId = none)
    (hscope : targetScoped observation = true)
    (hclosed : observeClose observation.records observation.target.coordinate =
      .closed closing outcome) :
    project observation ≠ .live streams := by
  simp [project, hmessage, hscope, hclosed]
  exact projectUnheadedClosed_ne_live observation closing outcome streams

theorem retained_stream_is_not_opaque {headers : List Header} {closing : Segment}
    {streams : Streams} {stream : Declaration × List UInt8}
    (h : stream ∈ retainedStreams headers closing streams) :
    stream.1.kind ≠ .opaque := by
  simp [retainedStreams, visibleStreams] at h
  exact h.2

theorem retained_stream_filter_characterization (headers : List Header)
    (closing : Segment) (streams : Streams) :
    retainedStreams headers closing streams =
      ((streams.zipIdx.filter fun entry =>
        !streamReferenced headers closing entry.2).map (·.1)).filter
          (fun stream => stream.1.kind != .opaque) := rfl

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
