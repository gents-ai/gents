import { describe, expect, it } from "vitest";

import { backendSave } from "../src/ui/screens/agent/InferencePanel";
import {
  optionalInteger,
  optionalNumber,
  optionalAbsolutePath,
  optionalGraphqlFilter,
  requiredGraphqlCollection,
  requiredGraphqlName,
  requiredHttpUrl,
  validateCronSchedule,
} from "../src/ui/screens/agent/draft";

const baseBackend = {
  backendId: "backend",
  name: "Backend",
  openaiWireApi: null,
  endpoint: "http://workstation-1:8000/v1",
  apiKey: null,
  apiKeyEnvVar: null,
  connectTimeoutSecs: 10,
  discoveryTimeoutSecs: 10,
  maxConcurrent: 2,
  maxQueueDepth: 8,
  enabled: true,
};

describe("configuration validation", () => {
  it("rejects truncated integers and enforces documented bounds", () => {
    expect(() => optionalInteger("Max concurrent", "1.5", { min: 1 })).toThrow(
      "whole number",
    );
    expect(() => optionalInteger("Max concurrent", "0", { min: 1 })).toThrow(
      "1 or more",
    );
    expect(optionalInteger("Max queue depth", "0", { min: 0 })).toBe(0);
    expect(optionalNumber("Temperature", "1", { min: 0 })).toBe(1);
  });

  it("accepts only HTTP inference endpoints", () => {
    expect(requiredHttpUrl("Endpoint", "http://workstation-1:8000/v1/")).toBe(
      "http://workstation-1:8000/v1",
    );
    expect(() => requiredHttpUrl("Endpoint", "workstation-1:8000")).toThrow(
      "http or https",
    );
    expect(() => requiredHttpUrl("Endpoint", "file:///tmp/model")).toThrow(
      "http or https",
    );
  });

  it("rejects relative tool roots before they reach the runtime", () => {
    expect(optionalAbsolutePath("Workspace root", "/Users/test/repo")).toBe(
      "/Users/test/repo",
    );
    expect(optionalAbsolutePath("Workspace root", "")).toBeNull();
    expect(() => optionalAbsolutePath("Workspace root", "src/repo")).toThrow(
      "absolute path",
    );
  });

  it("validates event identifiers and filter fragments before persistence", () => {
    expect(requiredGraphqlCollection("Collection", "AgentRequest")).toBe(
      "AgentRequest",
    );
    expect(requiredGraphqlName("Field", "session_id")).toBe("session_id");
    expect(optionalGraphqlFilter("Filter", '{session_id: {_eq: "s1"}}')).toBe(
      '{session_id: {_eq: "s1"}}',
    );
    expect(() => requiredGraphqlCollection("Collection", "__schema")).toThrow(
      "reserved",
    );
    expect(() => requiredGraphqlName("Field", "session-id")).toThrow("GraphQL name");
    expect(() => optionalGraphqlFilter("Filter", "{id: 1}) { broken")).toThrow();
  });

  it("validates the runtime's five-field cron and IANA timezone shape", () => {
    expect(validateCronSchedule("30 3 * * MON", "America/Los_Angeles")).toEqual({
      expression: "30 3 * * MON",
      timezone: "America/Los_Angeles",
    });
    expect(() => validateCronSchedule("30 3 * *", "UTC")).toThrow("exactly 5 fields");
    expect(() => validateCronSchedule("61 3 * * *", "UTC")).toThrow("invalid value");
    expect(() => validateCronSchedule("30 3 * * *", "Mars/Olympus")).toThrow(
      "IANA timezone",
    );
  });

  it.each(["ChatGptCodex", "XaiGrokOAuth", "ClaudeCliSubscription"])(
    "stores %s as principal OAuth instead of unauthenticated",
    (providerKind) => {
      expect(
        backendSave("did:key:agent", { ...baseBackend, providerKind }),
      ).toMatchObject({ document: { auth: { kind: "principal_oauth" } } });
    },
  );
});
