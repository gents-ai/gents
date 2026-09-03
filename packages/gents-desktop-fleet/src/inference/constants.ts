export const OPENAI_ENDPOINT = "https://api.openai.com/v1";
export const OPENAI_DEFAULT_MODEL = "gpt-5.4-mini";
export const OLLAMA_DEFAULT_URL = "http://127.0.0.1:11434/v1";
export const LOCAL_PROBE_URLS = [
  "http://127.0.0.1:8080/v1",
  "http://127.0.0.1:11434/v1",
];
export const CODEX_ENDPOINT = "https://chatgpt.com/backend-api/codex";
export const CODEX_DEFAULT_MODEL = "gpt-5.5";
export const GROK_ENDPOINT = "https://cli-chat-proxy.grok.com/v1";
export const GROK_DEFAULT_MODEL = "grok-4.5";

export const PROVIDER_OPENAI = "OpenAiCompatible";
export const PROVIDER_CODEX = "ChatGptCodex";
export const PROVIDER_GROK = "XaiGrokOAuth";

export const WIRE_RESPONSES = "responses";
export const WIRE_CHAT_COMPLETIONS = "chat_completions";

export type WizardStep =
  | "choose"
  | "openai"
  | "local"
  | "custom"
  | "codex"
  | "grok";

export type Detection = {
  status: "idle" | "probing" | "found" | "none";
  url: string;
  models: string[];
};
