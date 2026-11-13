namespace Identity.SignedCommand

/-! Signed client commands

`/accounts/usage/read` and `/config/owner-staged` are authenticated twice:
DefraDB's bearer extractor names the caller, and the command names its signer
and carries that signer's signature over its canonical payload. The runtime
checks that the two DIDs are the same, that the command is current, that the
signature verifies and that its nonce was not consumed before, and only then
acts for that signer. The served node's principal never substitutes for it:
a usage read is answered from the runtime's own observations, and an
owner-staged change is decided by `Identity.OwnerStaged.outcome` with the
signer as the requesting actor. -/

inductive CommandDecision where
  | misdirected
  | forbidden
  | executes
  deriving DecidableEq, Repr

/-- `caller` is the DID the bearer extractor authenticated (`none` without a
bearer); `signatureValid` covers currency, signature and the one-time nonce.
An anonymous caller or one that is not the signer is refused like a bad
signature, before the command is interpreted. -/
def decideCommand (authorityMatches : Bool) (caller : Option String) (signer : String)
    (signatureValid : Bool) : CommandDecision :=
  if !authorityMatches then .misdirected
  else if caller == some signer && signatureValid then .executes
  else .forbidden

/-- The actor a dispatched command acts for. -/
def commandActsFor (authorityMatches : Bool) (caller : Option String) (signer : String)
    (signatureValid : Bool) : Option String :=
  if decideCommand authorityMatches caller signer signatureValid = .executes then some signer
  else none

theorem command_executes_only_for_its_authenticated_signer (authorityMatches : Bool)
    (caller : Option String) (signer : String) (signatureValid : Bool)
    (h : decideCommand authorityMatches caller signer signatureValid = .executes) :
    authorityMatches = true ∧ caller = some signer ∧ signatureValid = true := by
  unfold decideCommand at h
  cases authorityMatches <;> cases signatureValid <;> simp_all

theorem command_acts_for_its_caller (authorityMatches : Bool) (caller : Option String)
    (signer actor : String) (signatureValid : Bool)
    (h : commandActsFor authorityMatches caller signer signatureValid = some actor) :
    caller = some actor := by
  unfold commandActsFor at h
  split at h
  · rename_i executes
    cases h
    exact (command_executes_only_for_its_authenticated_signer _ _ _ _ executes).2.1
  · cases h

theorem anonymous_command_never_executes (authorityMatches : Bool) (signer : String)
    (signatureValid : Bool) : decideCommand authorityMatches none signer signatureValid ≠ .executes := by
  intro h
  have := (command_executes_only_for_its_authenticated_signer _ _ _ _ h).2.1
  cases this

theorem foreign_authority_never_executes_command (caller : Option String) (signer : String)
    (signatureValid : Bool) : decideCommand false caller signer signatureValid = .misdirected := by
  simp [decideCommand]

end Identity.SignedCommand
