namespace TranscriptInputOrigin

/-- Facts come from a canonical message's role and publication and its accepted
request lineage. Prompt text is never evidence of who authored an input. -/
structure Facts where
  userRole : Bool
  outputObligation : Bool
  completionNotification : Bool
  goalContinuation : Bool
  completionWake : Bool
  parentTool : Bool
  trigger : Bool
  deriving DecidableEq, Repr

inductive Kind where
  | person | sessionMessage | trigger | goalContinuation
  | backgroundWake | backgroundResult | runtimeControl | otherRole
  deriving DecidableEq, Repr

/-- Runtime control is not a person's message. The more specific publication
precedes the request's origin: a background result can land in a person's turn. -/
def classify (facts : Facts) : Kind :=
  if !facts.userRole then .otherRole
  else if facts.outputObligation then .runtimeControl
  else if facts.completionNotification then .backgroundResult
  else if facts.goalContinuation then .goalContinuation
  else if facts.completionWake then .backgroundWake
  else if facts.parentTool then .sessionMessage
  else if facts.trigger then .trigger
  else .person

def classification (userRole outputObligation completionNotification goalContinuation
    completionWake parentTool trigger : Bool) : Kind :=
  classify ⟨userRole, outputObligation, completionNotification, goalContinuation,
    completionWake, parentTool, trigger⟩

theorem ordinary_input_is_person :
    classify ⟨true, false, false, false, false, false, false⟩ = .person := by rfl

theorem obligation_is_control (f : Facts) (role : f.userRole = true)
    (obligation : f.outputObligation = true) : classify f = .runtimeControl := by
  simp [classify, role, obligation]

theorem published_completion_precedes_request_origin (f : Facts)
    (role : f.userRole = true) (ordinary : f.outputObligation = false)
    (completion : f.completionNotification = true) : classify f = .backgroundResult := by
  simp [classify, role, ordinary, completion]

end TranscriptInputOrigin
