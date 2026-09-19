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

end StreamingResponse
