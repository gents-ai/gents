/** Build the existing fixture's provider arguments without placing a secret on argv. */
export function iosReconnectInferenceArgs(real, env) {
  if (!real) {
    return [
      "--model-name",
      "ios-reconnect",
      "--inference-url",
      "http://127.0.0.1:9/v1",
    ];
  }
  const endpoint = (
    env.GENTS_TAURI_LIVE_INFERENCE_URL ?? env.GENTS_DESKTOP_LIVE_BACKEND_ENDPOINT
  )?.trim();
  const model = (
    env.GENTS_TAURI_LIVE_MODEL_NAME ?? env.GENTS_DESKTOP_LIVE_BACKEND_MODEL
  )?.trim();
  const provider = env.GENTS_TAURI_LIVE_PROVIDER?.trim();
  if (!endpoint || !model || !provider) {
    throw new Error(
      "--real-inference requires GENTS_TAURI_LIVE_INFERENCE_URL, GENTS_TAURI_LIVE_MODEL_NAME and GENTS_TAURI_LIVE_PROVIDER",
    );
  }
  const key = env.GENTS_TAURI_LIVE_API_KEY;
  const keyVariable = env.GENTS_TAURI_LIVE_API_KEY_ENV_VAR?.trim();
  if (key && keyVariable) {
    throw new Error(
      "set only one of GENTS_TAURI_LIVE_API_KEY or GENTS_TAURI_LIVE_API_KEY_ENV_VAR",
    );
  }
  const variable = key ? "GENTS_TAURI_LIVE_API_KEY" : keyVariable;
  if (variable && (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(variable) || !env[variable])) {
    throw new Error(
      "the configured live API key environment variable must name a nonempty value",
    );
  }
  return [
    "--model-name",
    model,
    "--inference-url",
    endpoint,
    "--provider-kind",
    provider,
    ...(variable ? ["--api-key-env-var", variable] : []),
  ];
}
