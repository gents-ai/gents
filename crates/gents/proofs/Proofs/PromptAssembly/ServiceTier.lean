namespace PromptAssembly.ServiceTier

inductive Family where
  | openAiCompatible | chatGptCodex | openRouter | xaiGrokOAuth
  | claudeSubscription | anthropicApiKey
  | claudeBedrock | claudeVertex | claudeFoundry | claudeAnthropicAws
  | claudeMantle | claudeGoogleCloud | claudeGateway
  deriving DecidableEq, Repr

inductive Tier where
  | standard | fast
  deriving DecidableEq, Repr

def Family.wireName : Family → String
  | .openAiCompatible => "OpenAiCompatible"
  | .chatGptCodex => "ChatGptCodex"
  | .openRouter => "OpenRouter"
  | .xaiGrokOAuth => "XaiGrokOAuth"
  | .claudeSubscription => "ClaudeCliSubscription"
  | .anthropicApiKey => "AnthropicApiKey"
  | .claudeBedrock => "ClaudeBedrock"
  | .claudeVertex => "ClaudeVertex"
  | .claudeFoundry => "ClaudeFoundry"
  | .claudeAnthropicAws => "ClaudeAnthropicAws"
  | .claudeMantle => "ClaudeMantle"
  | .claudeGoogleCloud => "ClaudeGoogleCloud"
  | .claudeGateway => "ClaudeGateway"

def Tier.configName : Tier → String
  | .standard => "default"
  | .fast => "fast"

/-- Fast is persisted as a product selection, then encoded as priority on the
provider wire. The public API accepts both names, while the ChatGPT subscription
endpoint rejects the fast alias. Priority is the common transport value.
External premise: https://developers.openai.com/api/docs/guides/fast-mode
and the subscription endpoint's tier validation (2026-10-18). -/
def Tier.wireName : Tier → String
  | .standard => "default"
  | .fast => "priority"

/-- The Responses and Chat Completions request contracts accept service_tier;
ChatGPT subscriptions use the Codex Responses transport. Other transports have
no equivalent field here and must reject an explicit selection. Account/model
eligibility and actual processing speed remain provider decisions.
External premises: https://developers.openai.com/api/docs/guides/fast-mode and
https://learn.chatgpt.com/docs/agent-configuration/speed (2026-10-18). -/
def supports : Family → Bool
  | .openAiCompatible | .chatGptCodex => true
  | _ => false

structure Decision where
  allowed : Bool
  wire : Option String
  deriving DecidableEq, Repr

/-- Unset preserves existing provider defaults. Explicit Standard uses default,
so turning Fast off does not inherit a project-wide Fast default. -/
def decide (family : Family) (requested : Option Tier) : Decision :=
  if requested.isSome && !supports family then
    { allowed := false, wire := none }
  else
    { allowed := true, wire := requested.map Tier.wireName }

theorem unset_preserves_defaults (family : Family) :
    decide family none = { allowed := true, wire := none } := by
  simp [decide]

theorem supported_selection_is_preserved (family : Family) (requested : Option Tier)
    (h : supports family = true) :
    decide family requested = { allowed := true, wire := requested.map Tier.wireName } := by
  simp [decide, h]

theorem fast_uses_priority (family : Family) (h : supports family = true) :
    (decide family (some .fast)).wire = some "priority" := by
  simp [decide, h, Tier.wireName]

theorem unsupported_selection_is_rejected (family : Family) (tier : Tier)
    (h : supports family = false) :
    (decide family (some tier)).allowed = false := by
  simp [decide, h]

structure Case where
  family : Family
  requested : Option Tier
  deriving Repr

def cases : List Case :=
  [Family.openAiCompatible, .chatGptCodex, .openRouter, .xaiGrokOAuth,
    .claudeSubscription, .anthropicApiKey, .claudeBedrock, .claudeVertex, .claudeFoundry,
    .claudeAnthropicAws, .claudeMantle, .claudeGoogleCloud, .claudeGateway].flatMap fun family =>
      [none, some Tier.standard, some Tier.fast].map fun requested => { family, requested }

end PromptAssembly.ServiceTier
