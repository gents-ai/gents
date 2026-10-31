import Proofs.Basic

/-!
# Claude Code Messages body fields

Claude Code 2.1.311 decides `max_tokens`, `thinking`, `temperature`,
`context_management`, `output_config.effort`, `diagnostics` and the per-turn
effort row from the model's facts and the request (`zN` in the query generator
`Zfn`, `chunk-ps7vnt4g.js`; `zci`, `Gci`, `E$e`, `vot`, `kot`, `oXo`, `Fci`,
`rar`, `QS`, `lse`, `vEt`, `Xqr`, `I`, `Wsn` in `chunk-v2tvd4xz.js` and
`chunk-25m36221.js`). The model facts are inputs here: the catalog lists that
derive them live in the Rust catalog consumer, which turns a catalog entry into
a `ModelFacts` for a first-party request (every Gents Claude request is
first-party, so the provider-dependent fallbacks in those lists resolve to
true).

Decisions Gents takes where Claude Code reads its own environment:

* Claude Code's env knobs (`CLAUDE_CODE_MAX_OUTPUT_TOKENS`,
  `CLAUDE_CODE_DISABLE_THINKING`, `CLAUDE_CODE_EXTRA_BODY`, ...), the
  `max_thinking_tokens` permission layer, settings and server-served
  overrides are not Gents inputs; the baked catalog value is the limit.
* `max_tokens` is the catalog default, lowered by the request's override
  (`maxTokensOverride`, the context-overflow retry value). Claude Code reads the
  override with `||`, so an override of zero is no override.
* Non-interactive sessions emulate Claude Code's streaming programmatic mode
  (`-p --output-format stream-json --verbose`), so thinking display follows
  `displayUpdates` exactly as an interactive session does. `agent_new`
  subagents run as asynchronous subagents (`jci` returns the parent's config
  for them) and inherit that display. Gents has no `showThinkingSummaries`,
  so `summarized` and `omitted` displays are never sent.
* Gents keeps one effort per conversation, so the per-turn control carries a
  single `output_config.effort` row after the first user prompt (`Wsn` only
  inserts another row where the effort changes). The row carries the effort
  before the thinking-disabled clamp (`Pa` reads `di`, the clamp rewrites the
  body's `output_config.effort` only).
* A requested effort that is not one of the five levels falls back to the
  model's default effort, then to `high`.
-/

namespace PromptAssembly.ClaudeBody

/-- What the model supports, derived from the catalog entry. -/
structure ModelFacts where
  /-- `oXo`: the model takes `{type: "adaptive"}` and ignores a budget. -/
  adaptive : Bool
  /-- `vot`: thinking cannot be turned off, so a plain disable becomes adaptive. -/
  alwaysThinks : Bool
  /-- `Gci`: the model accepts a `thinking` field at all. -/
  thinkingSupported : Bool
  /-- `E$e`: the API rejects `{type: "disabled"}`. -/
  rejectsDisabled : Bool
  /-- `rar`: the model accepts `temperature`. -/
  acceptsTemperature : Bool
  /-- `QS`: the model accepts `output_config.effort`. -/
  effortCapable : Bool
  /-- `lse`: the model accepts the `max` effort. -/
  maxEffort : Bool
  /-- `vEt`: the model accepts the `xhigh` effort. -/
  xhighEffort : Bool
  /-- `Xqr`: the model holds effort to `high` while thinking is disabled. -/
  capsEffortWhenDisabled : Bool
  /-- `Ywn`: the model takes the per-turn effort control. -/
  perTurnEffort : Bool
  /-- `Wfe` default `max_tokens`. -/
  outputDefault : Nat
  /-- `Wfe` upper `max_tokens`. -/
  outputUpper : Nat
  /-- The catalog's default effort. -/
  defaultEffort : Option String
  deriving DecidableEq, Repr

/-- The session's thinking config (`thinkingConfig`). -/
inductive ThinkingConfig where
  | adaptive
  /-- `mechanical` marks a disable the model's always-thinking rule must not
  override (titles and other helper calls). -/
  | disabled (mechanical : Bool)
  | enabled (budget : Option Nat)
  deriving DecidableEq, Repr

/-- The `thinking` body field; `updates` is `display: "updates"`. -/
inductive Thinking where
  | adaptive (updates : Bool)
  | enabled (budget : Nat) (updates : Bool)
  | disabled
  deriving DecidableEq, Repr

structure Request where
  thinking : ThinkingConfig
  /-- The context-overflow retry value. -/
  maxTokensOverride : Option Nat
  requestedEffort : Option String
  firstParty : Bool
  firstPartyDefaultUrl : Bool
  experimentalBetasDisabled : Bool
  /-- `isAgenticQuery`. -/
  agentic : Bool
  /-- The call carries a fork point (cache-sharing compaction). -/
  fork : Bool
  /-- The `thinking-display-updates` beta is taken (`Xon` and `yYn`). -/
  displayUpdates : Bool
  /-- `context-management-2025-06-27` is among the betas. -/
  contextManagementBeta : Bool
  /-- `per-turn-control-2026-07-01` is among the betas. -/
  perTurnControlBeta : Bool
  previousMessageId : Option String
  deriving DecidableEq, Repr

/-- `Math.min(override || default, default)`. -/
def maxTokens (f : ModelFacts) (r : Request) : Nat :=
  min ((r.maxTokensOverride.filter (· != 0)).getD f.outputDefault) f.outputDefault

/-- `zci`: an always-thinking model turns a plain disable into adaptive. -/
def resolved (f : ModelFacts) : ThinkingConfig → ThinkingConfig
  | .disabled false => if f.alwaysThinks then .adaptive else .disabled false
  | c => c

/-- `Wu`: thinking is on for the request. -/
def thinkingOn (f : ModelFacts) (r : Request) : Bool :=
  match resolved f r.thinking with
  | .disabled _ => false
  | _ => true

/-- `Qt.type === "disabled" && Qt.mechanical === true`. -/
def mechanicallyDisabled (r : Request) : Bool :=
  match r.thinking with
  | .disabled mechanical => mechanical
  | _ => false

/-- The `thinking` field: adaptive models ignore a budget, other supported
models send the budget (the requested one or the model's upper limit minus one)
clamped to `[1024, max_tokens - 1]` with the floor winning, and a disable is
sent only to a first-party model that accepts it. -/
def thinking (f : ModelFacts) (r : Request) : Option Thinking :=
  let cfg := resolved f r.thinking
  if thinkingOn f r && f.thinkingSupported then
    if f.adaptive then some (.adaptive r.displayUpdates)
    else
      let requested := match cfg with
        | .enabled (some budget) => budget
        | _ => f.outputUpper - 1
      some (.enabled (max 1024 (min (maxTokens f r - 1) requested)) r.displayUpdates)
  else
    match cfg with
    | .disabled _ =>
        if r.firstParty && f.thinkingSupported && !f.rejectsDisabled then some .disabled else none
    | _ => none

/-- `temperature`: `1` only while thinking is off and the model takes one. -/
def temperature (f : ModelFacts) (r : Request) : Option Nat :=
  if !thinkingOn f r && f.acceptsTemperature then some 1 else none

/-- `context_management`: the `clear_thinking_20251015` edit. -/
def contextManagement (f : ModelFacts) (r : Request) : Bool :=
  thinkingOn f r && r.contextManagementBeta

def effortLevels : List String := ["low", "medium", "high", "xhigh", "max"]

/-- Above `high` (`Yqr`). -/
def aboveHigh (effort : String) : Bool := effort == "xhigh" || effort == "max"

def chosenEffort (f : ModelFacts) (r : Request) : String :=
  match r.requestedEffort with
  | some effort => if effortLevels.contains effort then effort else f.defaultEffort.getD "high"
  | none => f.defaultEffort.getD "high"

/-- `I`: `max` and `xhigh` fall back to `high` on a model without them. -/
def clampEffort (f : ModelFacts) (effort : String) : String :=
  if effort == "max" && !f.maxEffort then "high"
  else if effort == "xhigh" && !f.xhighEffort then "high"
  else effort

/-- `_S`: the effort before the body's thinking-disabled clamp. -/
def baseEffort (f : ModelFacts) (r : Request) : Option String :=
  if f.effortCapable then some (clampEffort f (chosenEffort f r)) else none

/-- `output_config.effort`: a disabled-thinking request is held to `high` when
the disable is mechanical or the model caps effort. -/
def effort (f : ModelFacts) (r : Request) : Option String :=
  (baseEffort f r).map fun e =>
    if thinking f r == some .disabled && (mechanicallyDisabled r || f.capsEffortWhenDisabled)
        && aboveHigh e then "high"
    else e

/-- `diagnostics.previous_message_id`: `some none` is the JSON `null` of a
conversation's first request. -/
def diagnostics (r : Request) : Option (Option String) :=
  if r.firstParty && r.firstPartyDefaultUrl && !r.experimentalBetasDisabled
      && (r.agentic || r.fork) then some r.previousMessageId
  else none

/-- The effort of the per-turn control row (`Pa`). -/
def perTurnEffort (f : ModelFacts) (r : Request) : Option String :=
  if r.perTurnControlBeta && f.perTurnEffort && !mechanicallyDisabled r then baseEffort f r
  else none

/-- The length of the leading run of user messages. -/
def userRun : List Bool → Nat
  | true :: rest => 1 + userRun rest
  | _ => 0

/-- Where the per-turn effort row goes (`Wsn`): after the first run of user
messages, given which messages are users; `none` when there is no user. -/
def rowIndex : List Bool → Option Nat
  | [] => none
  | false :: rest => (rowIndex rest).map (· + 1)
  | true :: rest => some (1 + userRun rest)

/-- Every decision of one request. -/
structure Decision where
  maxTokens : Nat
  thinking : Option Thinking
  temperature : Option Nat
  contextManagement : Bool
  effort : Option String
  diagnostics : Option (Option String)
  perTurnEffort : Option String
  deriving DecidableEq, Repr

def evaluate (f : ModelFacts) (r : Request) : Decision :=
  { maxTokens := maxTokens f r, thinking := thinking f r, temperature := temperature f r
    contextManagement := contextManagement f r, effort := effort f r
    diagnostics := diagnostics r, perTurnEffort := perTurnEffort f r }

theorem maxTokens_le_default (f : ModelFacts) (r : Request) : maxTokens f r ≤ f.outputDefault := by
  unfold maxTokens
  exact Nat.min_le_right _ _

theorem adaptive_models_send_no_budget (f : ModelFacts) (r : Request) (h : f.adaptive = true)
    (budget : Nat) (updates : Bool) : thinking f r ≠ some (.enabled budget updates) := by
  intro hc
  unfold thinking at hc
  simp only [h] at hc
  split at hc <;> split at hc <;> simp_all

theorem enabled_budget_in_range (f : ModelFacts) (r : Request) (budget : Nat) (updates : Bool)
    (h : thinking f r = some (.enabled budget updates)) :
    1024 ≤ budget ∧ budget ≤ max 1024 (maxTokens f r - 1) := by
  unfold thinking at h
  split at h
  · split at h
    · simp at h
    · simp only [Option.some.injEq, Thinking.enabled.injEq] at h
      obtain ⟨hb, _⟩ := h
      subst hb
      constructor <;> omega
  · split at h <;> simp_all <;> split at h <;> simp_all

theorem temperature_implies_thinking_off (f : ModelFacts) (r : Request)
    (h : temperature f r ≠ none) : thinkingOn f r = false := by
  unfold temperature at h
  split at h
  · rename_i hc
    simp only [Bool.and_eq_true, Bool.not_eq_true'] at hc
    exact hc.1
  · simp at h

theorem context_management_implies_thinking_on (f : ModelFacts) (r : Request)
    (h : contextManagement f r = true) : thinkingOn f r = true := by
  unfold contextManagement at h
  simp only [Bool.and_eq_true] at h
  exact h.1

theorem clampEffort_max (f : ModelFacts) (e : String) (h : clampEffort f e = "max") :
    f.maxEffort = true := by
  unfold clampEffort at h
  cases hm : f.maxEffort
  · exfalso
    simp only [hm, Bool.not_false, Bool.and_true, beq_iff_eq] at h
    split at h <;> try split at h
    all_goals simp_all
  · rfl

theorem clampEffort_xhigh (f : ModelFacts) (e : String) (h : clampEffort f e = "xhigh") :
    f.xhighEffort = true := by
  unfold clampEffort at h
  cases hx : f.xhighEffort
  · exfalso
    simp only [hx, Bool.not_false, Bool.and_true, beq_iff_eq] at h
    split at h <;> try split at h
    all_goals simp_all
  · rfl

/-- The effort never exceeds what the model accepts. -/
theorem effort_within_capabilities (f : ModelFacts) (r : Request) (e : String)
    (h : effort f r = some e) :
    f.effortCapable = true ∧ (e = "max" → f.maxEffort = true) ∧
      (e = "xhigh" → f.xhighEffort = true) := by
  unfold effort baseEffort at h
  by_cases hc : f.effortCapable = true
  · rw [if_pos hc] at h
    simp only [Option.map_some'] at h
    have h := Option.some.inj h
    refine ⟨hc, fun he => ?_, fun he => ?_⟩ <;> rw [he] at h <;>
      by_cases hb : (thinking f r == some .disabled &&
        (mechanicallyDisabled r || f.capsEffortWhenDisabled) &&
        aboveHigh (clampEffort f (chosenEffort f r))) = true
    · rw [if_pos hb] at h
      exact absurd h (by decide)
    · rw [if_neg hb] at h
      exact clampEffort_max _ _ h
    · rw [if_pos hb] at h
      exact absurd h (by decide)
    · rw [if_neg hb] at h
      exact clampEffort_xhigh _ _ h
  · rw [if_neg hc] at h
    simp at h

theorem diagnostics_imply_agentic_or_fork (r : Request) (h : diagnostics r ≠ none) :
    r.firstParty = true ∧ (r.agentic = true ∨ r.fork = true) := by
  unfold diagnostics at h
  split at h
  · rename_i hc
    simp only [Bool.and_eq_true, Bool.or_eq_true] at hc
    exact ⟨hc.1.1.1, hc.2⟩
  · simp at h

theorem per_turn_effort_needs_the_beta (f : ModelFacts) (r : Request)
    (h : perTurnEffort f r ≠ none) :
    r.perTurnControlBeta = true ∧ f.perTurnEffort = true ∧ f.effortCapable = true := by
  unfold perTurnEffort at h
  split at h
  · rename_i hc
    simp only [Bool.and_eq_true] at hc
    refine ⟨hc.1.1, hc.1.2, ?_⟩
    unfold baseEffort at h
    split at h
    · assumption
    · simp at h
  · simp at h

theorem display_updates_need_thinking (f : ModelFacts) (r : Request)
    (h : r.displayUpdates = false) :
    thinking f r ≠ some (.adaptive true) ∧ ∀ b, thinking f r ≠ some (.enabled b true) := by
  unfold thinking
  simp only [h]
  constructor
  · intro hc
    split at hc <;> try split at hc
    all_goals simp_all
  · intro b hc
    split at hc <;> try split at hc
    all_goals simp_all

theorem disabled_is_first_party_only (f : ModelFacts) (r : Request)
    (h : thinking f r = some .disabled) :
    r.firstParty = true ∧ f.thinkingSupported = true ∧ f.rejectsDisabled = false := by
  by_cases hs : (thinkingOn f r && f.thinkingSupported) = true
  · simp only [thinking, hs, ↓reduceIte] at h
    split at h <;> simp at h
  · simp only [thinking, hs, ↓reduceIte] at h
    cases hc : resolved f r.thinking with
    | adaptive => simp [hc] at h
    | enabled b => simp [hc] at h
    | disabled m => simpa [hc] using h

theorem userRun_le_length (l : List Bool) : userRun l ≤ l.length := by
  induction l with
  | nil => simp [userRun]
  | cons head rest ih => cases head <;> simp [userRun] <;> omega

theorem rowIndex_le_length (isUser : List Bool) (k : Nat) (h : rowIndex isUser = some k) :
    k ≤ isUser.length := by
  induction isUser generalizing k with
  | nil => simp [rowIndex] at h
  | cons head rest ih =>
    cases head with
    | false =>
      simp only [rowIndex, Option.map_eq_some'] at h
      obtain ⟨j, hj, rfl⟩ := h
      have := ih j hj
      simp
      omega
    | true =>
      simp only [rowIndex, Option.some.injEq] at h
      subst h
      have := userRun_le_length rest
      simp
      omega

/-- `claude-sonnet-5` on a first-party request: adaptive, not always thinking, no
temperature, effort `high` by default, no per-turn control. The Rust catalog
consumer derives the same facts from the model's catalog entry. -/
def sonnet5 : ModelFacts :=
  { adaptive := true, alwaysThinks := false, thinkingSupported := true
    rejectsDisabled := false, acceptsTemperature := false, effortCapable := true
    maxEffort := true, xhighEffort := true, capsEffortWhenDisabled := false
    perTurnEffort := false, outputDefault := 64000, outputUpper := 128000
    defaultEffort := some "high" }

/-- A turn of an interactive first-party session on a model with the context-management
beta and display updates, asking for `requested` effort. -/
def interactiveTurn (requested : Option String) : Request :=
  { thinking := .adaptive, maxTokensOverride := none, requestedEffort := requested
    firstParty := true, firstPartyDefaultUrl := true, experimentalBetasDisabled := false
    agentic := true, fork := false, displayUpdates := true
    contextManagementBeta := true, perTurnControlBeta := false, previousMessageId := none }

end PromptAssembly.ClaudeBody
