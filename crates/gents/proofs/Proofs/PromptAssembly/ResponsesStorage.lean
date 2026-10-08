namespace PromptAssembly.ResponsesStorage

/-!
Stateless Responses storage: which provider requests ask the server not to
store them (`store: false`) and, because a stateless server keeps no
reasoning items, ask for encrypted reasoning so it can replay
(`ClaudeMap.replayableReasoning .responses`).

Premise for `storage`/`cases`: the request is built with reasoning unset and a
non-empty preamble.
Outside it the runtime may differ without contradicting this model: rig adds
the encrypted include on its own whenever reasoning is set, and the Codex
transport skips `store: false` when no system item exists. The model does not
claim those cases.

Reasoning effort at the xAI endpoint (`sentEffort`/`effortCases`): xAI's
`/v1/models` advertises `capabilities.reasoning_effort` exactly for the models
that accept an effort and returns 400 on any effort for the others (live,
2026-10-01). So the request sends the profile's effort only when the model's
discovered catalog lists it; otherwise it is omitted and the model default
applies. Premise for `sentEffort`/`effortCases`: `OpenAiCompatible` on the
Responses wire. Kind and wire are not modelled; the Rust guard on them is
pinned by `inference_setup::tests::sent_reasoning_effort_leaves_other_kinds_and_wires_alone`.
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

/-- The effort an `OpenAiCompatible` Responses request sends. At the xAI API
endpoint only an effort the model's catalog advertises is sent; unknown support
and unadvertised efforts are omitted (mirrors `ClaudeMap.selectedEffort`). -/
def sentEffort (endpoint : String) (advertised : Option (List String))
    (requested : Option String) : Option String :=
  if atXaiApi endpoint then
    advertised.bind (fun choices => requested.filter (· ∈ choices))
  else requested

theorem xai_sent_effort_is_advertised (advertised : Option (List String))
    (requested : Option String) (e : String)
    (h : sentEffort xaiApiEndpoint advertised requested = some e) :
    ∃ choices, advertised = some choices ∧ e ∈ choices := by
  have hx : atXaiApi xaiApiEndpoint = true := by native_decide
  cases advertised with
  | none => simp [sentEffort, hx] at h
  | some choices =>
    cases requested with
    | none => simp [sentEffort, hx] at h
    | some r =>
      simp [sentEffort, hx, Option.filter] at h
      obtain ⟨hm, rfl⟩ := h
      exact ⟨choices, rfl, hm⟩

theorem xai_unknown_support_omitted (requested : Option String) :
    sentEffort xaiApiEndpoint none requested = none := by
  have hx : atXaiApi xaiApiEndpoint = true := by native_decide
  simp [sentEffort, hx]

theorem xai_unadvertised_effort_omitted (requested : Option String) :
    sentEffort xaiApiEndpoint (some []) requested = none := by
  have hx : atXaiApi xaiApiEndpoint = true := by native_decide
  cases requested <;> simp [sentEffort, hx, Option.filter]

theorem other_endpoints_send_requested (endpoint : String)
    (advertised : Option (List String)) (requested : Option String)
    (h : atXaiApi endpoint = false) :
    sentEffort endpoint advertised requested = requested := by
  simp [sentEffort, h]

def grok43Efforts : List String := ["none", "low", "medium", "high", "xhigh"]
def grok45Efforts : List String := ["low", "medium", "high", "xhigh"]

theorem grok_4_3_keeps_none :
    sentEffort xaiApiEndpoint (some grok43Efforts) (some "none") = some "none" ∧
      sentEffort xaiApiEndpoint (some grok45Efforts) (some "none") = none := by
  native_decide

structure EffortCase where
  name : String
  endpoint : String
  advertised : Option (List String)
  requested : Option String
  expected : Option String

def effortCase (name endpoint : String) (advertised : Option (List String))
    (requested : Option String) : EffortCase :=
  { name, endpoint, advertised, requested,
    expected := sentEffort endpoint advertised requested }

def effortCases : List EffortCase :=
  [ effortCase "xai_grok_4_3_none_sent" xaiApiEndpoint (some grok43Efforts)
      (some "none")
  , effortCase "xai_grok_4_5_high_sent" xaiApiEndpoint (some grok45Efforts)
      (some "high")
  , effortCase "xai_grok_4_5_none_omitted" xaiApiEndpoint (some grok45Efforts)
      (some "none")
  , effortCase "xai_grok_4_3_minimal_omitted" xaiApiEndpoint (some grok43Efforts)
      (some "minimal")
  , effortCase "xai_grok_4_20_low_omitted" xaiApiEndpoint (some []) (some "low")
  , effortCase "xai_uncatalogued_high_omitted" xaiApiEndpoint none (some "high")
  , effortCase "xai_unset_effort" xaiApiEndpoint (some grok45Efforts) none
  , effortCase "xai_trailing_slash_low_sent" "https://api.x.ai/v1/"
      (some grok43Efforts) (some "low")
  , effortCase "openai_unknown_high_sent" "https://api.openai.com/v1" none
      (some "high")
  , effortCase "local_empty_low_sent" "http://127.0.0.1:11434/v1" (some [])
      (some "low") ]

end PromptAssembly.ResponsesStorage
