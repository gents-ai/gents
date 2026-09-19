import Proofs.Transcript.Transition

namespace Transcript

/-- Ordering follows from the append operation and the pre-state sequence bound,
not an assumed coherent post-state. -/
theorem append_preserves_ordered (s : TranscriptState) (messageId : MessageId)
    (kind : MessageKind) (h_order : s.OrderedBySequence)
    (h_bound : ∀ row ∈ s.messages, row.sequence < s.nextSeq) :
    (s.appendUserMessage messageId kind).OrderedBySequence := by
  unfold TranscriptState.OrderedBySequence TranscriptState.appendUserMessage
  simp only
  unfold TranscriptState.OrderedBySequence at h_order
  generalize hm : s.messages = rows at h_order h_bound ⊢
  clear hm
  induction rows with
  | nil => simp [StrictlyIncreasingMessages]
  | cons row rest ih =>
    rcases h_order with ⟨h_first, h_rest⟩
    refine ⟨?_, ih h_rest (fun other ho => h_bound other (List.mem_cons_of_mem _ ho))⟩
    intro other ho
    rcases List.mem_append.mp ho with ho | ho
    · exact h_first other ho
    · simp only [List.mem_singleton] at ho
      subst other
      exact h_bound row (List.mem_cons_self _ _)

theorem append_user_advances_nextSeq
    (s : TranscriptState) (messageId : MessageId) (kind : MessageKind) :
    (s.appendUserMessage messageId kind).nextSeq = s.nextSeq + 1 := by
  rfl

private theorem append_row_preserves_strict_order
    (rows : List MessageRow) (row : MessageRow)
    (h_order : StrictlyIncreasingMessages rows)
    (h_bound : ∀ old ∈ rows, old.sequence < row.sequence) :
    StrictlyIncreasingMessages (rows ++ [row]) := by
  induction rows with
  | nil => simp [StrictlyIncreasingMessages]
  | cons first rest ih =>
      rcases h_order with ⟨h_first, h_rest⟩
      refine ⟨?_, ih h_rest (fun old h_old =>
        h_bound old (List.mem_cons_of_mem _ h_old))⟩
      intro other h_other
      rcases List.mem_append.mp h_other with h_other | h_other
      · exact h_first other h_other
      · simp only [List.mem_singleton] at h_other
        subst other
        exact h_bound first (List.mem_cons_self _ _)

theorem accepted_publication_preserves_order
    (s : TranscriptState) (messageId : MessageId) (turn : AssistantTurn)
    (h_publishable : s.PublishableTurn turn)
    (h_order : s.OrderedBySequence)
    (h_bound : ∀ row ∈ s.messages, row.sequence < s.nextSeq) :
    (s.publishAcceptedAssistant messageId turn).OrderedBySequence := by
  unfold TranscriptState.OrderedBySequence TranscriptState.publishAcceptedAssistant
  apply append_row_preserves_strict_order s.messages
  · exact h_order
  · intro row h_mem
    exact h_publishable.2.1 ▸ h_bound row h_mem

theorem partial_publication_preserves_order
    (s : TranscriptState) (messageId : MessageId) (turn : AssistantTurn)
    (h_publishable : s.PublishableTurn turn)
    (h_order : s.OrderedBySequence)
    (h_bound : ∀ row ∈ s.messages, row.sequence < s.nextSeq) :
    (s.publishPartialAssistant messageId turn).OrderedBySequence := by
  unfold TranscriptState.OrderedBySequence TranscriptState.publishPartialAssistant
  apply append_row_preserves_strict_order s.messages
  · exact h_order
  · intro row h_mem
    exact h_publishable.2.1 ▸ h_bound row h_mem

theorem assistantKind_references_call (turn : AssistantTurn)
    (callId : ToolExecution.ToolCallId) (h_call : callId ∈ turn.callIds) :
    (TranscriptState.assistantKind turn).referencesToolCall callId := by
  unfold TranscriptState.assistantKind
  split
  · rename_i h_empty
    simp only [List.isEmpty_iff] at h_empty
    have : False := by simp [h_empty] at h_call
    exact this.elim
  · simp [MessageKind.referencesToolCall, h_call]

/-- The accepted publication transition materializes the immutable assistant
header and each ordered pending row in the same post-state. -/
theorem accepted_publication_is_atomic
    {pre post : TranscriptState} {messageId : MessageId} {turn : AssistantTurn}
    (h_publishable : pre.PublishableTurn turn)
    (h_post : post = pre.publishAcceptedAssistant messageId turn)
    (callId : ToolExecution.ToolCallId) (h_call : callId ∈ turn.callIds) :
    (∃ header, header ∈ post.messages ∧
      header.messageId = messageId ∧
      header.sessionId = pre.sessionId ∧
      header.sequence = pre.nextSeq ∧
      header.reservesToolCall callId turn.sessionId turn.sequence) ∧
    (∃ call, call ∈ post.toolCalls ∧
      call.callId = callId ∧ call.state = .pending ∧
      post.ReservedByPersistedMessage call) ∧
    post.inFlight = pre.inFlight := by
  subst post
  let header : MessageRow :=
    { messageId := messageId
    , sessionId := turn.sessionId
    , sequence := turn.sequence
    , role := .assistant
    , kind := TranscriptState.assistantKind turn }
  let call : ToolCallRow :=
    { sessionId := turn.sessionId
    , callId := callId
    , messageSequence := turn.sequence
    , state := .pending
    , resultKey := none }
  have h_header_mem : header ∈
      (pre.publishAcceptedAssistant messageId turn).messages := by
    simp [TranscriptState.publishAcceptedAssistant, header]
  have h_header_reserves :
      header.reservesToolCall callId turn.sessionId turn.sequence := by
    simp [header, MessageRow.reservesToolCall,
      assistantKind_references_call turn callId h_call]
  have h_call_mem : call ∈
      (pre.publishAcceptedAssistant messageId turn).toolCalls := by
    simp [TranscriptState.publishAcceptedAssistant, TranscriptState.toolRows,
      call, h_call]
  refine ⟨⟨header, h_header_mem, rfl, h_publishable.1,
      h_publishable.2.1, h_header_reserves⟩,
    ⟨call, h_call_mem, rfl, rfl, ?_⟩, rfl⟩
  exact ⟨header, h_header_mem, h_header_reserves⟩

/-- When no older pending intent remains, the newly published rows expose the
provider's call order exactly to the dispatch head. -/
theorem accepted_publication_preserves_call_order
    (s : TranscriptState) (messageId : MessageId) (turn : AssistantTurn)
    (h_pending : s.pendingCallIds = []) :
    (s.publishAcceptedAssistant messageId turn).pendingCallIds = turn.callIds := by
  have h_prefix :
      List.filterMap (fun call =>
        if call.state = .pending then some call.callId else none) s.toolCalls = [] := by
    simpa [TranscriptState.pendingCallIds] using h_pending
  have h_suffix (ids : List ToolExecution.ToolCallId) :
      List.filterMap (fun call : ToolCallRow =>
        if call.state = .pending then some call.callId else none)
        (ids.map fun callId =>
          (⟨turn.sessionId, callId, turn.sequence,
            ToolExecution.ToolCallState.pending, none⟩ : ToolCallRow)) = ids := by
    induction ids with
    | nil => rfl
    | cons id rest ih => simp [ih]
  simp [TranscriptState.pendingCallIds, TranscriptState.publishAcceptedAssistant,
    TranscriptState.toolRows, h_prefix, h_suffix]

/-- Dispatch is necessarily a later transition: its pre-state already contains
both the pending lifecycle row and the reserving assistant header. -/
theorem dispatch_requires_published_header
    {pre : TranscriptState} {callId : ToolExecution.ToolCallId}
    (h_ready : pre.ReadyToDispatch callId) :
    ∃ call, call ∈ pre.toolCalls ∧
      call.callId = callId ∧ call.state = .pending ∧
      pre.ReservedByPersistedMessage call := by
  exact h_ready.1

theorem dispatch_preserves_published_headers
    (s : TranscriptState) (callId : ToolExecution.ToolCallId) :
    (s.dispatchToolCall callId).messages = s.messages := by
  rfl

/-- A terminal Partial publication may retain provenance, but none of its new
rows can satisfy the dispatch boundary. -/
theorem partial_publication_is_nondispatchable
    {pre : TranscriptState} {messageId : MessageId} {turn : AssistantTurn}
    (h_publishable : pre.PublishableTurn turn)
    (callId : ToolExecution.ToolCallId) (h_call : callId ∈ turn.callIds) :
    ¬ (pre.publishPartialAssistant messageId turn).Dispatchable callId := by
  intro h_dispatchable
  rcases h_dispatchable with ⟨call, h_mem, h_id, h_pending, _⟩
  rcases List.mem_append.mp h_mem with h_old | h_new
  · exact (h_publishable.2.2.2 call h_old) (h_id ▸ h_call)
  · simp only [TranscriptState.toolRows, List.mem_map] at h_new
    rcases h_new with ⟨publishedId, h_published, rfl⟩
    simp at h_pending

/-- No legal transition retracts or rewrites an existing immutable header.
Transitions either retain the exact row or append after it. -/
theorem transition_retains_messages
    {pre post : TranscriptState} (h_step : Transition pre post) :
    ∀ row, row ∈ pre.messages → row ∈ post.messages := by
  intro row h_mem
  cases h_step with
  | append_user h_post =>
      subst h_post
      exact List.mem_append_left _ h_mem
  | publish_accepted _ _ h_post =>
      subst h_post
      exact List.mem_append_left _ h_mem
  | publish_partial _ _ h_post =>
      subst h_post
      exact List.mem_append_left _ h_mem
  | reject_unaccepted h_post => simpa [h_post] using h_mem
  | dispatch_tool_call _ h_post => simpa [h_post] using h_mem
  | complete_tool_with_result _ _ h_fresh h_post =>
      subst h_post
      simp [TranscriptState.completeToolWithResult, h_fresh, h_mem]
  | observe_duplicate_tool_result _ h_post => simpa [h_post] using h_mem
  | append_distinct_tool_result _ h_post =>
      subst h_post
      exact List.mem_append_left _ h_mem
  | cancel_published _ h_post => simpa [h_post] using h_mem
  | fail_published _ h_post => simpa [h_post] using h_mem
  | timeout_published _ h_post => simpa [h_post] using h_mem
  | abandon_hook_ownership h_post => simpa [h_post] using h_mem

theorem trace_retains_messages
    {pre post : TranscriptState} (h_trace : Trace pre post) :
    ∀ row, row ∈ pre.messages → row ∈ post.messages := by
  induction h_trace with
  | refl => exact fun _ h => h
  | step h_step _ ih =>
      intro row h_mem
      exact ih row (transition_retains_messages h_step row h_mem)

/-- Tool failure after acceptance changes lifecycle state only; the accepted
assistant header remains durable history. -/
theorem terminalization_never_retracts_header
    (s : TranscriptState) (callId : ToolExecution.ToolCallId)
    (terminal : ToolExecution.ToolCallState) (header : MessageRow)
    (h_header : header ∈ s.messages) :
    header ∈ (s.terminalizeToolCall callId terminal).messages := by
  exact h_header

/-- Duplicate observations execute the same owner and preserve all state,
including sequence allocation and in-flight ownership. -/
theorem duplicate_tool_result_observation_noops (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) (messageId : MessageId) (key : ToolResultKey)
    (h_seen : s.hasToolResultKey key = true) :
    s.completeToolWithResult callId messageId key = s := by
  simp [TranscriptState.completeToolWithResult, h_seen]

theorem complete_tool_with_result_preserves_other_inflight
    (s : TranscriptState)
    (callId otherCallId : ToolExecution.ToolCallId)
    (h_ne : otherCallId ≠ callId)
    (messageId : MessageId) (key : ToolResultKey)
    (h_in : otherCallId ∈ s.inFlight) :
    otherCallId ∈ (s.completeToolWithResult callId messageId key).inFlight := by
  unfold TranscriptState.completeToolWithResult
  split
  · exact h_in
  · simp [Finset.mem_erase, h_ne, h_in]

theorem complete_tool_with_result_preserves_fresh_key
    (s : TranscriptState)
    (callId : ToolExecution.ToolCallId) (messageId : MessageId)
    (key otherKey : ToolResultKey) (h_ne : otherKey ≠ key)
    (h_fresh : s.hasToolResultKey otherKey = false) :
    (s.completeToolWithResult callId messageId key).hasToolResultKey otherKey =
      false := by
  unfold TranscriptState.completeToolWithResult
  split
  · exact h_fresh
  simp only [TranscriptState.hasToolResultKey,
    List.any_append, List.any_cons, List.any_nil, Bool.or_eq_false_iff] at h_fresh ⊢
  refine ⟨h_fresh, ?_, trivial⟩
  simp [MessageRow.isToolResultFor, MessageKind.toolResultKey?, h_ne.symm]

theorem parallel_results_complete_independently
    (s : TranscriptState)
    (firstCallId siblingCallId : ToolExecution.ToolCallId)
    (h_ne : siblingCallId ≠ firstCallId)
    (messageId : MessageId)
    (firstKey siblingKey : ToolResultKey) (h_key_ne : siblingKey ≠ firstKey)
    (h_sibling_in : siblingCallId ∈ s.inFlight)
    (h_sibling_fresh : s.hasToolResultKey siblingKey = false) :
    siblingCallId ∈ (s.completeToolWithResult firstCallId messageId firstKey).inFlight ∧
      (s.completeToolWithResult firstCallId messageId firstKey).hasToolResultKey
        siblingKey = false :=
  ⟨complete_tool_with_result_preserves_other_inflight
      s firstCallId siblingCallId h_ne messageId firstKey h_sibling_in,
    complete_tool_with_result_preserves_fresh_key
      s firstCallId messageId firstKey siblingKey h_key_ne h_sibling_fresh⟩

theorem explicit_inflight_drain_removes_ownership
    (s : TranscriptState)
    (callId : ToolExecution.ToolCallId)
    (terminal : ToolExecution.ToolCallState) :
    callId ∉ (s.terminalizeToolCall callId terminal).inFlight := by
  simp [TranscriptState.terminalizeToolCall]

def abandonWitnessToolCall : ToolCallRow :=
  { sessionId := 0
  , callId := 1
  , messageSequence := 0
  , state := .running
  , resultKey := none
  }

def abandonWitnessPre : TranscriptState :=
  { sessionId := 0
  , nextSeq := 1
  , messages := []
  , toolCalls := [abandonWitnessToolCall]
  , inFlight := insert 1 ∅
  }

def abandonWitnessPost : TranscriptState :=
  abandonWitnessPre.abandonHookOwnership

theorem abandon_hook_ownership_not_strong_drain :
    Transition abandonWitnessPre abandonWitnessPost ∧
      ¬ abandonWitnessPost.StrongDrain := by
  constructor
  · exact Transition.abandon_hook_ownership rfl
  · intro h_strong
    have h_not_running :=
      h_strong abandonWitnessToolCall
        (by simp [abandonWitnessPost, TranscriptState.abandonHookOwnership, abandonWitnessPre])
    exact h_not_running rfl

end Transcript
