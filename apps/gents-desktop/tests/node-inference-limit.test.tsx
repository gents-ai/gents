import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { AccountUsageView } from "@source-inc/gents-desktop-client";
import { limitNote } from "../src/ui/screens/agent/ProviderUsage";
import { InferenceSummary } from "../src/ui/screens/NodeFacts";
import { deployment } from "./config-panel-wiring/fixtures";

const NOW = Date.parse("2026-11-03T12:00:00Z");

const account = (
  usedPct: number,
  resetsAt: string | null = null,
): AccountUsageView => ({
  label: "work",
  provider: "anthropic",
  backendIds: ["claude"],
  windows: [
    {
      label: "Weekly",
      windowMinutes: 10_080,
      usedPct,
      resetsAt,
      source: "header",
      observedAt: "2026-11-03T11:59:00Z",
      lastKnown: false,
    },
  ],
  plan: null,
  note: null,
  readAt: null,
  readError: null,
  read: null,
});

describe("an account's limit on the node's Inference card", () => {
  it("says nothing while every account has room", () => {
    expect(limitNote([account(40)], NOW)).toBeNull();
  });

  it("says Near limit once the closest window is near its limit", () => {
    expect(limitNote([account(40), account(82)], NOW)).toMatchObject({
      text: "Near limit",
      tone: "warning",
    });
  });

  it("forgets a used-up window once its reset has passed", () => {
    expect(limitNote([account(100, "2026-11-03T11:00:00Z")], NOW)).toBeNull();
  });

  it("says Limited and when it resets once a window is used up", () => {
    expect(limitNote([account(100, "2026-11-03T14:00:00Z")], NOW)).toMatchObject({
      text: "Limited · resets in 2h",
      tone: "destructive",
    });
  });

  it("shows the note in place of Ready, the account and window on hover", () => {
    const ready = deployment.inferenceBackends.map((b) => ({
      ...b,
      probeStatus: "ok",
    }));
    const limit = limitNote([account(82)], NOW);
    render(<InferenceSummary backends={ready} limit={limit} />);
    const note = screen.getByText("Near limit");
    expect(note.getAttribute("title")).toMatch(/Weekly 82%/);
    expect(screen.queryByText("Ready")).toBeNull();
  });
});
