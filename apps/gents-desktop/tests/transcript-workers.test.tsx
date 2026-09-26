import { renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  CausedRequestView,
  DesktopSessionProvenanceRequest,
  RenderedTimelineItem,
  RenderedToolCallView,
  SessionProvenanceView,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "@/hooks/useShell";

import {
  LINEAGE_REFRESH_MS,
  subagentsOf,
  useSessionProvenance,
  useWorkers,
} from "../src/ui/screens/workers";
import { workerNow } from "../src/ui/screens/WorkerStep";

const AGENT = "did:key:parent";

const call = (
  requestId: string,
  toolCallId: string,
  statusKind = "success",
  action: "start" | "message" = "start",
): RenderedToolCallView =>
  ({
    itemKey: `item-${toolCallId}`,
    toolName: action === "start" ? "create_session" : "send_message",
    toolCallId,
    statusKind,
    requestId,
    awaitMode: "background",
    presentation: {
      kind: "subagent",
      action,
      name: "crew-explorer",
      sessionId: null,
      description: "explore",
      output: null,
    },
  }) as unknown as RenderedToolCallView;

const group = (...tools: RenderedToolCallView[]): RenderedTimelineItem =>
  ({
    kind: "toolGroup",
    itemKey: `group-${tools[0]?.itemKey}`,
    messageSequence: null,
    tools,
  }) as RenderedTimelineItem;

const caused = (
  requestId: string,
  sessionId: string,
  lifecycleState: string,
  byRequest: string,
  byToolCall: string,
  createdAt: string,
): CausedRequestView => ({
  requestId,
  requestDocId: `doc-${requestId}`,
  sessionId,
  agentDid: AGENT,
  behaviorId: "crew-explorer",
  lifecycleState,
  interruptRequestedAt: null,
  createdAt,
  hop: 1,
  causedByRequestId: byRequest,
  causedByRequestDocId: `doc-${byRequest}`,
  causedByToolCallId: byToolCall,
  causedBySessionId: "parent-session",
});

const view = (sent: CausedRequestView[]): SessionProvenanceView => ({
  sessionId: "parent-session",
  received: [],
  sent,
  truncated: false,
});

function shellFor(
  api: Shell["api"],
  timelineItems: RenderedTimelineItem[],
  { sessionId = "parent-session", sessions = [] as unknown[] } = {},
): Shell {
  return {
    api,
    selectedSession: { sessionId, timelineItems },
    selectedDeployment: { agentDid: AGENT, sessions },
  } as unknown as Shell;
}

function apiWith(
  provenance: (
    request: DesktopSessionProvenanceRequest,
  ) => Promise<SessionProvenanceView>,
) {
  return {
    sessionProvenance: vi.fn(provenance),
    fetchOperationsSnapshot: vi.fn().mockResolvedValue({ backgroundedTools: [] }),
  } as unknown as Shell["api"] & {
    sessionProvenance: ReturnType<typeof vi.fn>;
    fetchOperationsSnapshot: ReturnType<typeof vi.fn>;
  };
}

function useBoth(shell: Shell) {
  return useWorkers(shell, useSessionProvenance(shell));
}

describe("subagents of a session", () => {
  it("groups the requests this session caused by the session each landed in, oldest first", () => {
    const all = subagentsOf(
      [
        caused(
          "r-b1",
          "session-b",
          "completed",
          "req-1",
          "call-b",
          "2026-09-26T00:00:02Z",
        ),
        caused(
          "r-a2",
          "session-a",
          "processing",
          "req-2",
          "call-a2",
          "2026-09-26T00:00:03Z",
        ),
        caused(
          "r-a1",
          "session-a",
          "completed",
          "req-1",
          "call-a1",
          "2026-09-26T00:00:01Z",
        ),
      ],
      [{ sessionId: "session-a", title: "Explorer" } as never],
    );
    expect(all.map((s) => s.sessionId)).toEqual(["session-a", "session-b"]);
    expect(all[0]!.requests.map((r) => r.requestId)).toEqual(["r-a1", "r-a2"]);
    expect(all[0]!.live?.requestId).toBe("r-a2");
    expect(all[0]!.summary?.title).toBe("Explorer");
    expect(all[1]!.live).toBeNull();
    expect(all[1]!.summary).toBeNull();
  });

  it("joins each call to the request it caused and the session it reached", async () => {
    const api = apiWith(async () =>
      view([
        caused("r-done", "session-done", "completed", "req-1", "call-done", "1"),
        caused("r-live", "session-live", "processing", "req-1", "call-live", "2"),
      ]),
    );
    const done = call("req-1", "call-done");
    const items = [group(done, call("req-1", "call-live", "running"))];
    const { result } = renderHook(() => useBoth(shellFor(api, items)));

    await waitFor(() => expect(result.current.byToolCall(done)).toBeTruthy());
    const reached = result.current.byToolCall(done)!;
    expect(reached.request.requestId).toBe("r-done");
    expect(reached.subagent.sessionId).toBe("session-done");
    expect(workerNow(done, reached)).toEqual({ tone: "done", text: "finished" });
    expect(result.current.all.map((s) => s.sessionId)).toEqual([
      "session-done",
      "session-live",
    ]);
    expect(api.sessionProvenance).toHaveBeenCalledWith({
      sessionId: "parent-session",
      agentDid: AGENT,
    });
  });

  it("joins a call only through the lineage, never through a summary's latest request", async () => {
    const api = apiWith(async () => view([]));
    const unknown = call("req-1", "call-unknown", "running");
    const shell = shellFor(api, [group(unknown)], {
      sessions: [{ sessionId: "unrelated-session", latestRequestId: "call-unknown" }],
    });
    const { result } = renderHook(() => useBoth(shell));
    await waitFor(() => expect(result.current.loaded).toBe(true));
    expect(result.current.byToolCall(unknown)).toBeNull();
  });

  it("does not join a call from another request with the same call id", async () => {
    const api = apiWith(async () =>
      view([caused("r-1", "session-1", "completed", "req-1", "call-1", "1")]),
    );
    const other = call("req-2", "call-1");
    const { result } = renderHook(() => useBoth(shellFor(api, [group(other)])));
    await waitFor(() => expect(result.current.loaded).toBe(true));
    expect(result.current.byToolCall(other)).toBeNull();
  });

  it("does not keep another session's provenance for the render that switches", async () => {
    const api = apiWith(async () =>
      view([caused("r-a", "session-a", "completed", "req-1", "call-a", "1")]),
    );
    const items = [group(call("req-1", "call-a"))];
    const { result, rerender } = renderHook(
      ({ sessionId }: { sessionId: string }) =>
        useBoth(shellFor(api, items, { sessionId })),
      { initialProps: { sessionId: "session-x" } },
    );
    await waitFor(() => expect(result.current.all).toHaveLength(1));
    api.sessionProvenance.mockImplementationOnce(() => new Promise(() => {}));
    rerender({ sessionId: "session-y" });
    expect(result.current.all).toHaveLength(0);
  });
});

describe("subagent lineage freshness", () => {
  it("asks again on a session-list change and keeps the last view on a failed ask", async () => {
    let state = "processing";
    const api = apiWith(async () =>
      view([caused("r-1", "session-1", state, "req-1", "call-1", "1")]),
    );
    const items = [group(call("req-1", "call-1", "success"))];
    const { result, rerender } = renderHook(
      ({ sessions }: { sessions: unknown[] }) =>
        useBoth(shellFor(api, items, { sessions })),
      { initialProps: { sessions: [] as unknown[] } },
    );
    await waitFor(() => expect(result.current.all[0]?.live?.requestId).toBe("r-1"));

    api.sessionProvenance.mockRejectedValueOnce(new Error("bridge busy"));
    rerender({ sessions: [{ sessionId: "other", turnState: "running" }] });
    await waitFor(() => expect(api.sessionProvenance).toHaveBeenCalledTimes(2));
    expect(result.current.all[0]?.live?.requestId).toBe("r-1");

    state = "completed";
    rerender({ sessions: [{ sessionId: "other", turnState: "completed" }] });
    await waitFor(() => expect(result.current.all[0]?.live).toBeNull());
  });

  it("polls while a caused request runs with no local cue, and stops once it settles", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      let state = "processing";
      const api = apiWith(async () =>
        view([caused("r-remote", "session-remote", state, "req-1", "call-1", "1")]),
      );
      const shell = shellFor(api, [group(call("req-1", "call-1", "success"))]);
      const { result } = renderHook(() => useBoth(shell));
      await waitFor(() => expect(api.sessionProvenance).toHaveBeenCalledTimes(1));

      state = "completed";
      await vi.advanceTimersByTimeAsync(LINEAGE_REFRESH_MS + 1_000);
      await waitFor(() => expect(result.current.all[0]?.live).toBeNull());
      const asked = api.sessionProvenance.mock.calls.length;
      await vi.advanceTimersByTimeAsync(LINEAGE_REFRESH_MS * 3);
      expect(api.sessionProvenance).toHaveBeenCalledTimes(asked);
    } finally {
      vi.useRealTimers();
    }
  });

  it("asks for operations facts only when the transcript has a background process", async () => {
    const api = apiWith(async () => view([]));
    renderHook(() => useBoth(shellFor(api, [group(call("req-1", "call-1"))])));
    await waitFor(() => expect(api.sessionProvenance).toHaveBeenCalled());
    expect(api.fetchOperationsSnapshot).not.toHaveBeenCalled();

    const process = {
      ...call("req-1", "call-p"),
      toolName: "spawn_process",
      presentation: { kind: "process", action: "spawn", target: "bash" },
    } as unknown as RenderedToolCallView;
    renderHook(() => useBoth(shellFor(api, [group(process)])));
    await waitFor(() => expect(api.fetchOperationsSnapshot).toHaveBeenCalled());
  });
});
