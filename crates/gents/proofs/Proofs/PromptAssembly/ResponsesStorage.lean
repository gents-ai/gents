namespace PromptAssembly.ResponsesStorage

/-!
Stateless Responses storage: which provider requests ask the server not to
store them (`store: false`) and, because a stateless server keeps no
reasoning items, ask for encrypted reasoning so it can replay
(`ClaudeMap.replayableReasoning .responses`).

Premise: the request is built with reasoning unset and a non-empty preamble.
Outside it the runtime may differ without contradicting this model: rig adds
the encrypted include on its own whenever reasoning is set, and the Codex
transport skips `store: false` when no system item exists. The model does not
claim those cases.
-/

inductive Family where
  | openAiCompatible
  | xaiGrokOAuth
  | chatGptCodex
  | openRouter
  deriving DecidableEq, Repr

inductive Wire where
  | responses
  | chatCompletions
  deriving DecidableEq, Repr

def xaiApiEndpoint : String := "https://api.x.ai/v1"

def encryptedReasoningInclude : String := "reasoning.encrypted_content"

/-- Exact xAI API endpoint, ignoring ASCII case and any trailing `/`. -/
def atXaiApi (endpoint : String) : Bool :=
  (endpoint.dropRightWhile (· == '/')).toLower == xaiApiEndpoint

def stateless : Family → Wire → String → Bool
  | .xaiGrokOAuth, .responses, _ => true
  | .chatGptCodex, _, _ => true
  | .openAiCompatible, .responses, endpoint => atXaiApi endpoint
  | _, _, _ => false

structure Storage where
  store : Option Bool
  includes : List String
  deriving DecidableEq, Repr

def storage (family : Family) (wire : Wire) (endpoint : String) : Storage :=
  if stateless family wire endpoint then
    { store := some false, includes := [encryptedReasoningInclude] }
  else
    { store := none, includes := [] }

theorem stateless_requests_encrypted_reasoning (family : Family) (wire : Wire)
    (endpoint : String) (h : (storage family wire endpoint).store = some false) :
    encryptedReasoningInclude ∈ (storage family wire endpoint).includes := by
  unfold storage at *
  split <;> simp_all

theorem xai_api_key_responses_is_stateless :
    storage .openAiCompatible .responses "https://api.x.ai/v1/" =
        { store := some false, includes := [encryptedReasoningInclude] } ∧
      storage .openAiCompatible .responses "https://api.x.ai/v1//" =
        { store := some false, includes := [encryptedReasoningInclude] } := by
  native_decide

theorem openai_api_key_stays_stored :
    storage .openAiCompatible .responses "https://api.openai.com/v1" =
      { store := none, includes := [] } := by native_decide

theorem xai_chat_completions_is_unchanged :
    storage .openAiCompatible .chatCompletions xaiApiEndpoint =
      { store := none, includes := [] } := by native_decide

theorem local_endpoint_stays_stored :
    storage .openAiCompatible .responses "http://127.0.0.1:11434/v1" =
      { store := none, includes := [] } := by native_decide

structure Case where
  name : String
  family : Family
  wire : Wire
  endpoint : String
  expected : Storage

def case (name : String) (family : Family) (wire : Wire)
    (endpoint : String) : Case :=
  { name, family, wire, endpoint, expected := storage family wire endpoint }

def cases : List Case :=
  [ case "grok_oauth_responses" .xaiGrokOAuth .responses
      "https://cli-chat-proxy.grok.com/v1"
  , case "codex_responses" .chatGptCodex .responses
      "https://chatgpt.com/backend-api/codex"
  , case "xai_api_key_responses" .openAiCompatible .responses
      "https://api.x.ai/v1"
  , case "xai_api_key_responses_trailing_slash" .openAiCompatible .responses
      "https://api.x.ai/v1/"
  , case "xai_api_key_responses_two_trailing_slashes" .openAiCompatible
      .responses "https://api.x.ai/v1//"
  , case "xai_api_key_responses_upper_case" .openAiCompatible .responses
      "HTTPS://API.X.AI/v1"
  , case "openai_api_key_responses" .openAiCompatible .responses
      "https://api.openai.com/v1"
  , case "local_responses" .openAiCompatible .responses
      "http://127.0.0.1:11434/v1"
  , case "xai_api_key_chat_completions" .openAiCompatible .chatCompletions
      "https://api.x.ai/v1" ]

end PromptAssembly.ResponsesStorage
