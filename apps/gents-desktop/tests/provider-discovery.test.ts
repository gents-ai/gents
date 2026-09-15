import { describe, expect, it } from "vitest";
import type { InferenceDiscoveryResult } from "@source-inc/gents-desktop-client";
import {
  currentInferenceDiscovery,
  inferenceDiscoveryKey,
} from "../src/ui/lib/providerDiscovery";

function result(requestKey: string): InferenceDiscoveryResult {
  return {
    requestKey,
    contractVersion: 1,
    defaultsVersion: "test",
    requestedEndpoint: "http://old.test/v1",
    effectiveEndpoint: "http://old.test/v1",
    backendName: "Local server",
    providerKind: "OpenAiCompatible",
    openaiWireApi: "chat_completions",
    reachable: true,
    models: [],
    failure: null,
    manualEntryAllowed: false,
  };
}

describe("provider discovery ordering", () => {
  it("rejects a late result after provider or endpoint input changes", () => {
    const oldKey = inferenceDiscoveryKey(1, "local", "optional_api_key", "old");
    const currentKey = inferenceDiscoveryKey(2, "local", "optional_api_key", "new");
    expect(currentInferenceDiscovery(currentKey, result(oldKey))).toBeNull();
  });

  it("accepts the response for the current exact connection", () => {
    const key = inferenceDiscoveryKey(
      3,
      "openrouter",
      "api_key",
      "https://openrouter.ai/api/v1",
    );
    expect(currentInferenceDiscovery(key, result(key))).not.toBeNull();
  });
});
