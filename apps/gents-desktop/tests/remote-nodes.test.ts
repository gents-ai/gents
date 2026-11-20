import { describe, expect, it } from "vitest";

import type { SavedRuntimeView } from "@source-inc/gents-desktop-client/generated/SavedRuntimeView";

import { continueLabel } from "../src/lib/remoteNodes";

const peer = (
  label: string,
  source: string | null,
  local = false,
): SavedRuntimeView => ({
  label,
  nodeDid: `did:key:${label}`,

  graphql: null,
  local: local || source === "local-standard" || source === "local",
});

describe("continueLabel", () => {
  it.each([
    ["no peers", [], null],
    ["only a local node", [peer("Mine", "local-standard")], null],
    ["one remote", [peer("Studio", "enrollment")], "Continue with Studio"],
    ["only an enrolled local node", [peer("Forge", "enrollment", true)], null],
    [
      "two remotes",
      [peer("Studio", "enrollment"), peer("Lab", "enrollment")],
      "Continue with your other nodes",
    ],
    [
      "a local node and one remote",
      [peer("Mine", "local"), peer("Studio", "enrollment")],
      "Continue with Studio",
    ],
  ])("%s", (_name, peers, expected) => {
    expect(continueLabel(peers)).toBe(expected);
  });
});
