import { describe, expect, it } from "vitest";

import {
  applyMockInference,
  DEFAULT_MOCK_MODEL_NAME,
  resolveLivePlaywrightOptions,
} from "./live-playwright-options.mjs";

describe("live Playwright option resolution", () => {
  it("uses mock inference by default when no live backend is configured", () => {
    const options = resolveLivePlaywrightOptions(["--list"], {});

    expect(options.argv).toEqual(["--list"]);
    expect(options.shouldStartMockInference).toBe(true);
    expect(options.mockModelName).toBe(DEFAULT_MOCK_MODEL_NAME);
    expect(options.env.GENTS_TAURI_LIVE).toBe("1");
    expect(options.env.CARGO_NET_GIT_FETCH_WITH_CLI).toBe("true");

    const env = applyMockInference(options.env, {
      endpoint: "http://127.0.0.1:1234/v1",
      modelName: "mock-model",
    });
    expect(env.GENTS_TAURI_LIVE_INFERENCE_URL).toBe("http://127.0.0.1:1234/v1");
    expect(env.GENTS_TAURI_LIVE_MODEL_NAME).toBe("mock-model");
    expect(env.GENTS_TAURI_LIVE_PROVIDER).toBe("openai-compatible");
    expect(env.GENTS_TAURI_LIVE_API_KEY).toBe("desktop-live-browser-test-key");
  });

  it("fails loudly in real-provider mode when no backend is configured", () => {
    expect(() =>
      resolveLivePlaywrightOptions(["--require-real-inference", "--list"], {}),
    ).toThrow(/real-inference mode requires a backend/);

    expect(() =>
      resolveLivePlaywrightOptions(["--list"], {
        GENTS_TAURI_LIVE_REQUIRE_REAL_INFERENCE: "1",
      }),
    ).toThrow(/real-inference mode requires a backend/);
  });

  it("accepts explicit real-provider flags and forwards remaining Playwright args", () => {
    const options = resolveLivePlaywrightOptions(
      [
        "--require-real-inference",
        "--inference-url",
        "https://api.example.test/v1",
        "--model-name=provider-model",
        "--api-key-env-var",
        "OPENAI_API_KEY",
        "--provider",
        "openai-compatible",
        "--grep",
        "desktop live browser smoke",
      ],
      {},
    );

    expect(options.shouldStartMockInference).toBe(false);
    expect(options.argv).toEqual(["--grep", "desktop live browser smoke"]);
    expect(options.env.GENTS_TAURI_LIVE_INFERENCE_URL).toBe(
      "https://api.example.test/v1",
    );
    expect(options.env.GENTS_DESKTOP_LIVE_BACKEND_ENDPOINT).toBe(
      "https://api.example.test/v1",
    );
    expect(options.env.GENTS_TAURI_LIVE_MODEL_NAME).toBe("provider-model");
    expect(options.env.GENTS_TAURI_LIVE_PROVIDER).toBe("openai-compatible");
    expect(options.env.GENTS_TAURI_LIVE_API_KEY_ENV_VAR).toBe("OPENAI_API_KEY");
  });

  it("uses existing live backend env vars without enabling mock inference", () => {
    const options = resolveLivePlaywrightOptions(["--list"], {
      GENTS_TAURI_LIVE_INFERENCE_URL: "http://workstation-1:8000/v1",
      GENTS_TAURI_LIVE_MODEL_NAME: "MiniMax-M2.7-NVFP4",
    });

    expect(options.shouldStartMockInference).toBe(false);
    expect(options.env.GENTS_TAURI_LIVE_INFERENCE_URL).toBe(
      "http://workstation-1:8000/v1",
    );
    expect(options.env.GENTS_TAURI_LIVE_MODEL_NAME).toBe("MiniMax-M2.7-NVFP4");
  });

  it("allows OpenRouter API key fallback in real-provider mode", () => {
    const options = resolveLivePlaywrightOptions(["--require-real-inference"], {
      OPENROUTER_API_KEY: "test-openrouter-key",
    });

    expect(options.shouldStartMockInference).toBe(false);
    expect(options.env.OPENROUTER_API_KEY).toBe("test-openrouter-key");
  });

  it("rejects flags missing a value before launching Playwright", () => {
    expect(() => resolveLivePlaywrightOptions(["--inference-url"], {})).toThrow(
      "missing value for --inference-url",
    );
  });
});
