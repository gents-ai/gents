import Proofs.PromptAssembly.ClaudeMap

/-!
# Claude Code wire mode of a Gents request

Claude Code 2.1.311 sends a different identity prefix, entrypoint, `x-app`,
workload, subagent marking, request class and cache source depending on how a
query started (`f6n`, `rF`, `Pl`, `J6o`, `rci`, `Tbe` in the extracted client).
A Gents request takes the mode of the Claude Code query it corresponds to:

* a session a client opened (desktop or CLI chat) is the interactive CLI
  (`cli`, request class `main`);
* a session a task trigger opened is a background `claude -p` job
  (`sdk-cli`, `x-app: cli-bg`);
* a session another agent opened (`agent_new`) is a subagent, and one a
  plugin job opened is a workflow subagent (Claude Code's `subagent` with a
  workflow run): both inherit the root session's interactivity;
* scheduled work carries the `cron` workload (scheduled tasks, loops and
  auto-continuation set it in Claude Code);
* compaction is the `compaction` class and titles and one-shot calls are
  `auxiliary`; neither is agentic, so neither takes the one-hour cache source,
  the agentic betas or the `diagnostics` field.

Agentic is `isAgenticQuery` (`repl_main_thread*`, `agent:*`, `sdk`,
`hook_agent` sources); `diagnostics` further needs a first-party request with a
non-empty beta list, which the body owner decides. The one-hour allowlist
(`A3e`: `repl_main_thread*`, `sdk`, `auto_mode`, `memdir_relevance`) holds no
`agent:` source, so subagent turns keep the five-minute TTL.

Gents never claims an Agent SDK version, so non-interactive work uses the
non-interactive CLI entrypoint rather than an SDK one.
-/

namespace PromptAssembly.ClaudeOrigin

open PromptAssembly.ClaudeMap

/-- How the request's session was created (`SessionCreationCause`); `client`
when no creation receipt exists because a client opened it. -/
inductive Cause where
  | client
  | agentNew
  | taskTrigger
  | pluginJob
  deriving DecidableEq, Repr

/-- What the provider call is for. -/
inductive Kind where
  | turn
  | compaction
  | title
  | oneShot
  deriving DecidableEq, Repr

structure Origin where
  cause : Cause
  /-- The first session of the creation chain was opened by a client. -/
  rootClient : Bool
  /-- The request's execution origin is scheduled. -/
  scheduled : Bool
  kind : Kind
  /-- The agent supplies its own system prompt. -/
  appendsSystemPrompt : Bool
  deriving DecidableEq, Repr

/-- `rci`'s classes, sent as `x-claude-code-request-class`. -/
inductive RequestClass where
  | main
  | subagent
  | workflow
  | compaction
  | auxiliary
  deriving DecidableEq, Repr

def RequestClass.name : RequestClass → String
  | .main => "main"
  | .subagent => "subagent"
  | .workflow => "workflow"
  | .compaction => "compaction"
  | .auxiliary => "auxiliary"

structure WireMode where
  session : SessionMode
  entrypoint : String
  /-- `x-app: cli-bg`. -/
  background : Bool
  workload : Option String
  /-- `cc_is_subagent=true` and the agent headers. -/
  subagent : Bool
  requestClass : RequestClass
  /-- The query source is on the one-hour cache allowlist. -/
  mainThreadSource : Bool
  /-- `isAgenticQuery`: the agent loop's own call; gates the agentic betas and
  `diagnostics`. -/
  agentic : Bool
  deriving DecidableEq, Repr

def Origin.interactive (o : Origin) : Bool :=
  match o.cause with
  | .client => true
  | .agentNew | .pluginJob => o.rootClient
  | .taskTrigger => false

def Origin.subagent (o : Origin) : Bool :=
  o.cause == .agentNew || o.cause == .pluginJob

def wireMode (o : Origin) : WireMode :=
  let turn := o.kind == .turn
  { session := if o.interactive then .interactive else .nonInteractive o.appendsSystemPrompt
    entrypoint := if o.interactive then "cli" else "sdk-cli"
    background := o.cause == .taskTrigger
    workload := if o.scheduled then some "cron" else none
    subagent := o.subagent
    requestClass :=
      match o.kind, o.cause with
      | .compaction, _ => .compaction
      | .title, _ | .oneShot, _ => .auxiliary
      | .turn, .agentNew => .subagent
      | .turn, .pluginJob => .workflow
      | .turn, _ => .main
    mainThreadSource := turn && !o.subagent
    agentic := turn }

theorem interactive_iff_cli (o : Origin) :
    (wireMode o).entrypoint = "cli" ↔ o.interactive = true := by
  unfold wireMode
  cases h : o.interactive <;> simp [h]

theorem background_is_non_interactive (o : Origin) (h : (wireMode o).background = true) :
    o.interactive = false := by
  simp only [wireMode, beq_iff_eq] at h
  simp [Origin.interactive, h]

theorem subagent_turns_carry_agent_classes (o : Origin) (hk : o.kind = .turn)
    (hs : (wireMode o).subagent = true) :
    (wireMode o).requestClass = .subagent ∨ (wireMode o).requestClass = .workflow := by
  simp only [wireMode, Origin.subagent, Bool.or_eq_true, beq_iff_eq] at hs
  rcases hs with hc | hc <;> simp [wireMode, hk, hc]

theorem subagents_never_take_the_main_thread_cache (o : Origin)
    (hs : (wireMode o).subagent = true) : (wireMode o).mainThreadSource = false := by
  simp only [wireMode] at hs ⊢
  simp [hs]

theorem auxiliary_calls_are_not_agentic (o : Origin) (hk : o.kind ≠ .turn) :
    (wireMode o).agentic = false ∧ (wireMode o).mainThreadSource = false := by
  simp [wireMode, hk]

theorem scheduled_work_is_cron (o : Origin) :
    (wireMode o).workload = some "cron" ↔ o.scheduled = true := by
  cases h : o.scheduled <;> simp [wireMode, h]

/-- `cc_turn_origin` of the request's prompt (`wBs`). Only a top-level turn is
stamped: a subagent's or workflow's first prompt carries none, and compaction,
titles and one-shot calls build their own messages. Scheduled work is
`scheduled` even in a chat (a wakeup stamps it), otherwise a client-rooted
interactive prompt is `human` and a non-interactive one is `sdk`. -/
def turnOrigin (o : Origin) : Option String :=
  if o.kind == .turn && !o.subagent then
    some (if o.scheduled then "scheduled" else if o.interactive then "human" else "sdk")
  else none

/-- `cc_prompt_index` and `cc_turn_index` of a turn from the number of prompt
rows in its history (`mKn`): each prompt row opens a turn, and a human prompt
also advances the prompt index. Gents does not record the origin of earlier
prompts, so they are taken to share the current turn's. A compaction summary
that lost the position leaves both unset. -/
def turnPosition (o : Origin) (prompts : Nat) (positionLost : Bool) : Option (Nat × Nat) :=
  match turnOrigin o with
  | none => none
  | some origin =>
    if positionLost then none
    else some (if origin == "human" then prompts else 0, max prompts 1)

theorem subagent_turns_carry_no_turn_origin (o : Origin) (h : o.subagent = true) :
    turnOrigin o = none := by
  simp [turnOrigin, h]

theorem auxiliary_calls_carry_no_turn_origin (o : Origin) (h : o.kind ≠ .turn) :
    turnOrigin o = none := by
  simp [turnOrigin, h]

theorem unscheduled_client_turns_are_human (o : Origin) (hc : o.cause = .client)
    (hk : o.kind = .turn) (hs : o.scheduled = false) : turnOrigin o = some "human" := by
  simp [turnOrigin, Origin.subagent, Origin.interactive, hc, hk, hs]

theorem lost_position_has_no_indices (o : Origin) (prompts : Nat) :
    turnPosition o prompts true = none := by
  unfold turnPosition
  cases turnOrigin o <;> simp

/-- The identity prefix a request sends (`f6n` on a first-party request). -/
def identity (o : Origin) : String :=
  identityFor false (wireMode o).session

theorem client_chat_is_the_cli_identity (o : Origin) (h : o.cause = .client) :
    identity o = cliIdentity := by
  simp [identity, identityFor, wireMode, Origin.interactive, h]

end PromptAssembly.ClaudeOrigin
