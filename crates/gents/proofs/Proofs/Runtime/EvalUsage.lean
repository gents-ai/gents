import Lean

/-! What an eval trial is charged for, and which of it is the subject agent's
own work. Times are milliseconds; ids are the trial home's requests. -/

namespace Runtime.EvalUsage
open Lean

/-- An `AgentRequest` of the trial home with its stored `purpose`,
`session_id`, and claim and terminal times. -/
structure Request where
  id : Nat
  purpose : String
  session : String
  claimed : Option Nat
  terminal : Option Nat
  deriving BEq, ToJson

/-- An `InferenceCall`: the request it names, its `call_kind`, `call_seq`,
recorded counts and run times. -/
structure Call where
  request : Option Nat
  kind : String
  seq : Nat
  input : Option Nat
  output : Option Nat
  cached : Option Nat
  started : Option Nat
  ended : Option Nat
  deriving BEq, ToJson

/-- An `AgentToolCall`: whether it names a plugin parent
(`plugin_parent_tool_call_doc_id`) or a spawning call
(`spawned_by_tool_call_doc_id`). -/
structure Tool where
  request : Option Nat
  name : String
  state : String
  pluginParent : Bool
  spawned : Bool
  started : Option Nat
  completed : Option Nat
  deriving BEq, ToJson

/-- A finished trial home: the trial's session and the requests its stages
submitted, in stage order. -/
structure Home where
  session : String
  stages : List Nat
  requests : List Request
  calls : List Call
  tools : List Tool
  deriving ToJson

/-- A sum of counts is known only when every count is. A partial sum is not
a total: shutdown interrupts calls before they record usage, and their
tokens were still spent. -/
def total (counts : List (Option Nat)) : Option Nat :=
  counts.foldr (fun count sum => do
    let count ← count
    let sum ← sum
    pure (count + sum)) (some 0)

def titleAudit (home : Home) (call : Call) : Bool :=
  home.requests.any fun request => call.request == some request.id && request.purpose == "title-audit"

/-- A trial is charged for every inference call in its home but its session
title audit's: naming the session is runtime work the subject never asked
for, and shutdown interrupts it whenever it outlives the stages. A call
naming no request of the home is charged. -/
def charged (home : Home) (call : Call) : Bool :=
  !titleAudit home call

structure HomeUsage where
  calls : Nat
  input : Option Nat
  output : Option Nat
  reportedInput : Nat
  reportedOutput : Nat
  deriving ToJson

def reported (counts : List (Option Nat)) : Nat :=
  (counts.filterMap id).foldl (· + ·) 0

def homeUsage (home : Home) : HomeUsage :=
  let calls := home.calls.filter (charged home)
  { calls := calls.length
    input := total (calls.map (·.input))
    output := total (calls.map (·.output))
    reportedInput := reported (calls.map (·.input))
    reportedOutput := reported (calls.map (·.output)) }

/-- The subject's own requests are its stage requests and every public request
of its trial session. A delegated agent runs in a session of its own, a title
audit or access review is not public, and an automation fires into sessions
of its own; none of them is the subject's work. -/
def subjectRequest (home : Home) (request : Request) : Bool :=
  request.purpose == "normal" && (request.session == home.session || home.stages.contains request.id)

def own (home : Home) (request : Option Nat) : Bool :=
  home.requests.any fun candidate => request == some candidate.id && subjectRequest home candidate

structure Totals where
  calls : Nat
  input : Option Nat
  output : Option Nat
  cached : Option Nat
  unreported : Nat
  deriving ToJson

def totals (calls : List Call) : Totals :=
  { calls := calls.length
    input := total (calls.map (·.input))
    output := total (calls.map (·.output))
    cached := total (calls.map (·.cached))
    unreported := (calls.filter fun call => call.input.isNone || call.output.isNone).length }

inductive ToolClass where
  | direct
  | script
  | insideScript
  deriving BEq

/-- A call a script makes is a row of its own naming the script call as its
plugin parent, so the script and its calls are never one row. A spawned
process's row is the execution of a `spawn_process` call already counted. -/
def toolClass (tool : Tool) : Option ToolClass :=
  if tool.spawned then none
  else if tool.pluginParent then some .insideScript
  else if tool.name == "script" then some .script
  else some .direct

def failed (tool : Tool) : Bool :=
  tool.state == "failed" || tool.state == "timedOut"

structure Count where
  calls : Nat
  failed : Nat
  deriving ToJson

def count (tools : List Tool) (kind : ToolClass) : Count :=
  let tools := tools.filter fun tool => toolClass tool == some kind
  { calls := tools.length, failed := (tools.filter failed).length }

/-- Each interval that started, or nothing once one started without an end.
One that never started ran for no time. -/
def started : List (Option Nat × Option Nat) → Option (List (Nat × Nat))
  | [] => some []
  | (none, _) :: rest => started rest
  | (some _, none) :: _ => none
  | (some start, some stop) :: rest => (started rest).map ((start, max start stop) :: ·)

/-- The time covered by the union of the intervals: overlapping runs are not
counted twice. -/
def unionLength (intervals : List (Option Nat × Option Nat)) : Option Nat :=
  (started intervals).map fun spans =>
    let sorted := spans.mergeSort fun a b => a.1 ≤ b.1
    let merged := sorted.foldl (fun (state : Nat × Option (Nat × Nat)) span =>
      match state.2 with
      | none => (state.1, some span)
      | some (from_, to) =>
        if span.1 ≤ to then (state.1, some (from_, max to span.2))
        else (state.1 + (to - from_), some span)) (0, none)
    merged.1 + (match merged.2 with
      | none => 0
      | some (from_, to) => to - from_)

/-- The first turn of the first stage's request, by call sequence, as
recorded: a first call that recorded no prompt count leaves it unknown
rather than taking a later turn's. -/
def firstTurn (home : Home) (turns : List Call) : Option Call :=
  home.stages.head?.bind fun first =>
    (turns.filter (·.request == some first)).foldl (fun best call =>
      match best with
      | none => some call
      | some held => if call.seq < held.seq then some call else some held) none

structure Subject where
  requests : Nat
  firstRequest : Bool
  firstPromptTokens : Option Nat
  turns : Totals
  compaction : Totals
  direct : Count
  script : Count
  insideScript : Count
  wallMs : Option Nat
  modelMs : Option Nat
  toolMs : Option Nat
  deriving ToJson

def subjectUsage (home : Home) : Option Subject :=
  let requests := home.requests.filter (subjectRequest home)
  if requests.isEmpty then none else
  let calls := home.calls.filter fun call => own home call.request
  let turns := calls.filter (·.kind == "inference")
  let tools := home.tools.filter fun tool => own home tool.request
  let first := firstTurn home turns
  some
    { requests := requests.length
      firstRequest := first.isSome
      firstPromptTokens := first.bind (·.input)
      turns := totals turns
      compaction := totals (calls.filter (·.kind == "compaction"))
      direct := count tools .direct
      script := count tools .script
      insideScript := count tools .insideScript
      wallMs := unionLength (requests.map fun request => (request.claimed, request.terminal))
      modelMs := unionLength (turns.map fun call => (call.started, call.ended))
      toolMs := unionLength ((tools.filter fun tool =>
          toolClass tool == some .direct || toolClass tool == some .script).map
        fun tool => (tool.started, tool.completed)) }

structure Usage where
  home : HomeUsage
  subject : Option Subject
  deriving ToJson

def trialUsage (home : Home) : Usage :=
  { home := homeUsage home, subject := subjectUsage home }

theorem total_unknown_of_missing :
    ∀ counts : List (Option Nat), none ∈ counts → total counts = none
  | [], missing => by simp at missing
  | count :: rest, missing => by
    rcases List.mem_cons.mp missing with first | later
    · subst first
      simp [total]
    · have unknown := total_unknown_of_missing rest later
      cases count <;> simp_all [total]

theorem title_audit_not_charged (home : Home) (call : Call) (request : Request)
    (held : request ∈ home.requests) (named : call.request = some request.id)
    (audit : request.purpose = "title-audit") :
    charged home call = false := by
  simp only [charged, titleAudit, Bool.not_eq_false', List.any_eq_true]
  exact ⟨request, held, by simp [named, audit]⟩

theorem subject_request_is_public (home : Home) (request : Request)
    (own : subjectRequest home request = true) : request.purpose = "normal" := by
  simp only [subjectRequest, Bool.and_eq_true, beq_iff_eq] at own
  exact own.1

theorem script_call_is_inside_its_script (tool : Tool)
    (process : tool.spawned = false) (parent : tool.pluginParent = true) :
    toolClass tool = some .insideScript := by
  simp [toolClass, process, parent]

end Runtime.EvalUsage
