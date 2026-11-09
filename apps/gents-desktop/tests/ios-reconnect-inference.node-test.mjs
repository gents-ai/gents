import { strict as assert } from "node:assert";
import { test } from "node:test";
import { iosReconnectInferenceArgs } from "./ios-reconnect-inference.mjs";

test("sync-only reconnect keeps its isolated unreachable provider", () => {
  assert.deepEqual(iosReconnectInferenceArgs(false, {}), [
    "--model-name",
    "ios-reconnect",
    "--inference-url",
    "http://127.0.0.1:9/v1",
  ]);
});

test("real reconnect requires explicit provider, endpoint and model", () => {
  assert.throws(() => iosReconnectInferenceArgs(true, {}), /requires GENTS_TAURI_LIVE/);
  const env = {
    GENTS_TAURI_LIVE_PROVIDER: "OpenAiCompatible",
    GENTS_TAURI_LIVE_INFERENCE_URL: "http://workstation-1:8000/v1",
    GENTS_TAURI_LIVE_MODEL_NAME: "GLM-5.3-Flash-NVFP4",
    GENTS_TAURI_LIVE_API_KEY: "test-private-key",
  };
  const args = iosReconnectInferenceArgs(true, env);
  assert.deepEqual(args.slice(0, 6), [
    "--model-name",
    env.GENTS_TAURI_LIVE_MODEL_NAME,
    "--inference-url",
    env.GENTS_TAURI_LIVE_INFERENCE_URL,
    "--provider-kind",
    env.GENTS_TAURI_LIVE_PROVIDER,
  ]);
  assert.deepEqual(args.slice(6), ["--api-key-env-var", "GENTS_TAURI_LIVE_API_KEY"]);
  assert(!args.includes(env.GENTS_TAURI_LIVE_API_KEY));
  assert.throws(
    () =>
      iosReconnectInferenceArgs(true, {
        ...env,
        GENTS_TAURI_LIVE_API_KEY_ENV_VAR: "OTHER",
      }),
    /only one/,
  );
  assert.throws(
    () =>
      iosReconnectInferenceArgs(true, {
        ...env,
        GENTS_TAURI_LIVE_API_KEY: "",
        GENTS_TAURI_LIVE_API_KEY_ENV_VAR: "MISSING",
      }),
    /nonempty/,
  );
});
