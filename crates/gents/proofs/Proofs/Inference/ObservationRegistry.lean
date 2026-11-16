namespace ObservationRegistry

/-! The activity registry of one inference key (`gents-inference`'s
`InferenceObservationRegistry`). A request opens the entry for its key, and no
second request opens while it is live. Each tool call of the live request is
activity until that call closes, and closing one call leaves the others. A
request's end removes the entry with its tool calls. A tool call is fenced by
the request it was opened under: once that request has ended, its close cannot
touch an entry a later request opened for the same key.

Premises outside this model: a request label opens at most once per trace, and
tool call ids are unique among the open calls of a request, since the provider
assigns them. Time is outside the model; every open tool call is fresh, which is
the state the registry reports within its silence window. -/

inductive Activity where
  | awaitingProvider
  | working
  deriving DecidableEq, Repr

structure Entry where
  request : Nat
  tools : List String
  deriving DecidableEq, Repr

abbrev State := Option Entry

inductive Action where
  | openRequest (request : Nat)
  | closeRequest (request : Nat)
  | openTool (request : Nat) (tool : String)
  | closeTool (request : Nat) (tool : String)
  deriving DecidableEq, Repr

def insertTool (tool : String) (tools : List String) : List String :=
  if tool ∈ tools then tools else tool :: tools

def removeTool (tool : String) (tools : List String) : List String :=
  tools.filter (fun other => other != tool)

def step (state : State) : Action → State
  | .openRequest request =>
    match state with
    | none => some ⟨request, []⟩
    | some entry => some entry
  | .closeRequest request =>
    match state with
    | none => none
    | some entry => if entry.request = request then none else some entry
  | .openTool request tool =>
    match state with
    | none => none
    | some entry =>
      if entry.request = request then some { entry with tools := insertTool tool entry.tools }
      else some entry
  | .closeTool request tool =>
    match state with
    | none => none
    | some entry =>
      if entry.request = request then some { entry with tools := removeTool tool entry.tools }
      else some entry

def project : State → Option Activity
  | none => none
  | some entry => some (if entry.tools.isEmpty then Activity.awaitingProvider else Activity.working)

def replay : State → List Action → List (Option Activity)
  | _, [] => []
  | state, action :: rest =>
    let next := step state action
    project next :: replay next rest

theorem closing_request_ends_its_activity (entry : Entry) :
    project (step (some entry) (.closeRequest entry.request)) = none := by
  simp [step, project]

theorem stale_tool_close_is_inert (entry : Entry) (request : Nat) (tool : String)
    (h : entry.request ≠ request) :
    step (some entry) (.closeTool request tool) = some entry := by
  simp [step, h]

theorem concurrent_calls_are_removed_per_call :
    replay none [.openRequest 0, .openTool 0 "first", .openTool 0 "second",
      .closeTool 0 "first", .closeTool 0 "second", .closeRequest 0] =
    [some .awaitingProvider, some .working, some .working, some .working,
      some .awaitingProvider, none] := by
  decide

theorem handed_off_tool_is_fenced_from_its_replacement :
    replay none [.openRequest 0, .openTool 0 "call", .closeRequest 0, .openRequest 1,
      .openTool 1 "call", .closeTool 0 "call", .closeTool 1 "call", .closeRequest 1] =
    [some .awaitingProvider, some .working, none, some .awaitingProvider, some .working,
      some .working, some .awaitingProvider, none] := by
  decide

theorem refused_request_is_inert :
    replay none [.openRequest 0, .openRequest 1, .openTool 1 "a", .closeRequest 1,
      .openTool 0 "a", .closeRequest 0] =
    [some .awaitingProvider, some .awaitingProvider, some .awaitingProvider,
      some .awaitingProvider, some .working, none] := by
  decide

end ObservationRegistry
