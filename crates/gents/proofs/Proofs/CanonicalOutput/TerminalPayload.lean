import Proofs.CanonicalOutput.Message

namespace CanonicalOutput

inductive TerminalPayloadError where
  | selection (error : TerminalError)
  | conflictingMessage
  | reconstruction (error : MessageError)
  deriving DecidableEq, Repr

/-- The common exact-answer boundary for UI, subagent completion and mailbox
delivery. A terminal lifecycle alone does not make a selected message available.
Explicit NoMessage is distinct from an unresolved selection or incomplete replica.
The caller's existing request/ACP owner supplies terminal lifecycle and scope. -/
def resolveTerminalPayload (messages : List MessageEnvelope) (records : List Segment)
    (deniedHeaders deniedSegments : List DocId) (request : DocId) (session : SessionId)
    (selection : Option TerminalSelection)
    (dependencyDenials : List DependencyDenial := []) :
    Except TerminalPayloadError (Option ReconstructedMessage) := do
  let selected ← (resolveTerminal (messages.map (·.header)) deniedHeaders request session
    selection).mapError TerminalPayloadError.selection
  match selected with
  | none => .ok none
  | some header =>
      let message ← uniqueRecord TerminalPayloadError.conflictingMessage .conflictingMessage
        (messages.filter (fun message => message.header == header))
      if dependencyDenials.any (fun denial =>
          message.header.refs.any (fun ref => ref.closeId == denial.rootCloseId)) then
        .error (.reconstruction (.reconstruction (.lookup .denied)))
      else do
        let native ← (reconstructMessage records deniedSegments message).mapError
          TerminalPayloadError.reconstruction
        .ok (some native)

theorem absent_terminal_selection_has_no_answer
    (messages : List MessageEnvelope) (records : List Segment)
    (deniedHeaders deniedSegments : List DocId) (request : DocId) (session : SessionId) :
    resolveTerminalPayload messages records deniedHeaders deniedSegments request session none =
      .error (.selection .missingSelection) := rfl

theorem no_message_needs_no_payload
    (messages : List MessageEnvelope) (records : List Segment)
    (deniedHeaders deniedSegments : List DocId) (request : DocId) (session : SessionId) :
    resolveTerminalPayload messages records deniedHeaders deniedSegments request session
      (some .noMessage) = .ok none := rfl

theorem unknown_selected_header_is_not_latest
    (messages : List MessageEnvelope) (records : List Segment)
    (request selected : DocId) (session : SessionId)
    (h : (messages.map (·.header)).filter (fun header => header.id == selected) = []) :
    resolveTerminalPayload messages records [] [] request session (some (.message selected)) =
      .error (.selection .missingHeader) := by
  simp [resolveTerminalPayload, resolveTerminal, h]
  rfl

namespace Examples

def selectedMessage : MessageEnvelope :=
  { header :=
      { id := 200, session := 1, request := some 10, origin := none
        refs := [⟨100, 0⟩], outcome := .complete, role := .assistant
        publication := .requestExecution 7 }
    key := "selected", sequence := 0, nativeId := none, createdAt := 5
    blocks := [.text ⟨⟨100, 0⟩, .full⟩] }

def deniedDependencyIsDenied : Bool :=
  match resolveTerminalPayload [selectedMessage] [] [] [] 10 1
      (some (.message 200)) [⟨100, 101⟩] with
  | .error (.reconstruction (.reconstruction (.lookup .denied))) => true
  | _ => false

example : deniedDependencyIsDenied = true := by native_decide

def absentDependencyIsIncomplete : Bool :=
  match resolveTerminalPayload [selectedMessage] [] [] [] 10 1
      (some (.message 200)) with
  | .error (.reconstruction (.reconstruction (.lookup .unavailable))) => true
  | _ => false

example : absentDependencyIsIncomplete = true := by native_decide

end Examples

end CanonicalOutput
