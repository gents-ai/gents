import Proofs.CanonicalOutput.Reconstruction

namespace CanonicalOutput

inductive TerminalSelection where
  | message (id : DocId)
  | noMessage
  deriving DecidableEq, Repr

inductive TerminalError where
  | missingSelection
  | missingHeader
  | denied
  | conflictingHeaders
  | wrongScope
  | ineligibleHeader
  deriving DecidableEq, Repr

def terminalPublicationEligible : MessagePublication → Bool
  | .requestExecution _ | .requestRecovery _ => true
  | .toolDelivery _ | .fork _ => false

/-- The caller has observed terminal request lifecycle. Header dependency
reconstruction follows this selection; success here is not complete hydration.
Agent/requester authorization is supplied by the existing ACP owner, not inferred
    from these Nat labels. Physical request and session membership are checked
    here, along with assistant role and request publication eligibility. -/
def resolveTerminal (headers : List Header) (denied : List DocId)
    (request : DocId) (session : SessionId) :
    Option TerminalSelection → Except TerminalError (Option Header)
  | none => .error .missingSelection
  | some .noMessage => .ok none
  | some (.message id) =>
      if id ∈ denied then .error .denied
      else match uniqueRecord .missingHeader .conflictingHeaders
          (headers.filter (fun header => header.id == id)) with
      | .error error => .error error
      | .ok header =>
          if header.request ≠ some request ∨ header.session ≠ session then
            .error .wrongScope
          else if header.role ≠ .assistant ∨
              terminalPublicationEligible header.publication = false then
            .error .ineligibleHeader
          else .ok (some header)

theorem terminal_without_selection_never_chooses_latest (headers : List Header)
    (denied : List DocId) (request : DocId) (session : SessionId) :
    resolveTerminal headers denied request session none = .error .missingSelection := rfl

theorem explicit_no_message_is_not_loading (headers : List Header)
    (denied : List DocId) (request : DocId) (session : SessionId) :
    resolveTerminal headers denied request session (some .noMessage) = .ok none := rfl

theorem unrelated_header_cannot_change_terminal_selection (headers : List Header)
    (denied : List DocId) (request : DocId) (session : SessionId)
    (selected : DocId) (late : Header) (h : late.id ≠ selected) :
    resolveTerminal (headers ++ [late]) denied request session (some (.message selected)) =
      resolveTerminal headers denied request session (some (.message selected)) := by
  simp [resolveTerminal, List.filter_append, h]

theorem fork_does_not_acquire_request_membership (header : Header)
    (id request : DocId) (session : SessionId) :
    resolveTerminal [forkHeader header id session] [] request session (some (.message id)) =
      .error .wrongScope := by
  simp [resolveTerminal, forkHeader, uniqueRecord_singleton]

theorem non_assistant_cannot_be_terminal_output (header : Header)
    (request : DocId) (session : SessionId)
    (hrequest : header.request = some request) (hsession : header.session = session)
    (hrole : header.role ≠ .assistant) :
    resolveTerminal [header] [] request session (some (.message header.id)) =
      .error .ineligibleHeader := by
  simp [resolveTerminal, hrequest, hsession, hrole, uniqueRecord_singleton]

theorem tool_delivery_cannot_be_terminal_output (header : Header)
    (request call : DocId) (session : SessionId)
    (hrequest : header.request = some request) (hsession : header.session = session)
    (hrole : header.role = .assistant)
    (hpublication : header.publication = .toolDelivery call) :
    resolveTerminal [header] [] request session (some (.message header.id)) =
      .error .ineligibleHeader := by
  simp [resolveTerminal, hrequest, hsession, hrole, hpublication,
    terminalPublicationEligible, uniqueRecord_singleton]

end CanonicalOutput
