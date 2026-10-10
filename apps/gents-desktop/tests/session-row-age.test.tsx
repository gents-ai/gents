import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { Age, when } from "../src/ui/screens/time";
import { WorkersSurface } from "../src/ui/screens/WorkersSurface";
import { node, renderIn, testApp } from "./app-fixture";

describe("an age on screen", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-03T12:00:00Z"));
  });
  afterEach(() => vi.useRealTimers());

  it("advances with the clock while nothing else re-renders it", () => {
    render(<Age iso="2026-10-03T11:56:00Z" />);
    expect(screen.getByText("4m")).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(2 * 60_000);
    });
    expect(screen.getByText("6m")).toBeInTheDocument();
  });

  it("reads as when() does", () => {
    expect(when("2026-10-03T11:59:50Z", Date.now())).toBe("now");
    expect(when("2026-10-03T09:00:00Z", Date.now())).toBe("3h");
    expect(when(null, Date.now())).toBe("");
  });
});

describe("a worker's age in the workers surface", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-03T12:00:00Z"));
  });
  afterEach(() => vi.useRealTimers());

  it("advances with the clock", () => {
    const summary = (sessionId: string, over: Record<string, unknown> = {}) => ({
      sessionId,
      nodeDid: "did:key:a",
      requesterDid: null,
      agentId: null,
      title: sessionId,
      turnState: "completed",
      updatedAt: "2026-10-03T11:56:00Z",
      startedBy: null,
      ...over,
    });
    const app = testApp({
      deployments: [
        node({
          nodeDid: "did:key:a",
          sessions: [
            summary("parent"),
            summary("worker", {
              startedBy: {
                nodeDid: "did:key:a",
                sessionId: "parent",
                requesterDid: null,
              },
            }),
          ],
        }),
      ],
    });
    renderIn(app, <WorkersSurface sessionId="parent" placement="dock" />);
    expect(screen.getByText("4m")).toBeInTheDocument();
    act(() => {
      vi.advanceTimersByTime(2 * 60_000);
    });
    expect(screen.getByText("6m")).toBeInTheDocument();
  });
});
