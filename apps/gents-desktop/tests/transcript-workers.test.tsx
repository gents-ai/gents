import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  DesktopSessionSnapshot,
  CausedCallView,
  DesktopSessionProvenanceRequest,
  LinkedSessionView,
  RenderedTimelineItem,
  RenderedToolCallView,
  SessionProvenanceView,
} from "@source-inc/gents-desktop-client";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { node, publish, testApp, withApp } from "./app-fixture";

import {
  subagentsOf,
  useSessionProvenance,
  useWorkers,
} from "../src/ui/screens/workers";
import { workerNow } from "../src/ui/screens/WorkerStep";
import { writeSession } from "../src/hooks/sessionStore";

const AGENT = "did:key:parent";
const PERSON = "did:key:person";

const call = (
  requestId: string,
  toolCallId: string,
  statusKind = "success",
  action: "start" | "message" = "start",
): RenderedToolCallView =>
  ({
    itemKey: `item-${toolCallId}`,
    toolName: action === "start" ? "agent_new" : "agent_message",
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

/* call `byToolCall` of request `byRequest` caused `requestId` in `sessionId` */
const caused = (
  requestId: string,
  sessionId: string,
  lifecycleState: string,
  byRequest: string,
  byToolCall: string,
): CausedCallView => ({
  requestId: byRequest,
  toolCallId: byToolCall,
  caused: {
    requestId,
    agentDid: AGENT,
    sessionId,
    requesterDid: null,
    lifecycleState,
    createdAt: null,
  },
});

const link = (sessionId: string): LinkedSessionView => ({
  agentDid: AGENT,
  sessionId,
  requesterDid: null,
  causeRequestDocId: "doc-req-1",
});

/* `started` is the lineage owner's answer: only sessions this one began */
const view = (
  calls: CausedCallView[],
  started: string[] = calls.map((c) => c.caused.sessionId),
): SessionProvenanceView => ({
  sessionId: "parent-session",
  startedBy: null,
  started: started.map(link),
  sent: [],
  received: [],
  senders: [],
  calls,
});

/* the agent's node, listing `sessions` */
const nodeListing = (sessions: unknown[]) => [node({ agentDid: AGENT, sessions })];

/* An app selecting `sessionId` on the agent's node, which lists
   `sessions`; one per transcript and session a test passes, as the
   projection keeps one store across reads. */
const apps = new WeakMap<RenderedTimelineItem[], Map<string, DesktopApp>>();
function appFor(
  api: object,
  timelineItems: RenderedTimelineItem[],
  {
    sessionId = "parent-session",
    sessions = null as unknown[] | null,
  }: { sessionId?: string; sessions?: unknown[] | null } = {},
): DesktopApp {
  const bySession = apps.get(timelineItems) ?? new Map<string, DesktopApp>();
  apps.set(timelineItems, bySession);
  const deployments = nodeListing(
    sessions ?? [{ agentDid: AGENT, sessionId, requesterDid: PERSON }],
  );
  const known = bySession.get(sessionId);
  if (known) {
    publish(known, deployments);
    return known;
  }
  const app = testApp({
    api,
    deployments,
    session: { sessionId, timelineItems } as unknown as DesktopSessionSnapshot,
    selection: { agentDid: AGENT, sessionId },
  });
  bySession.set(sessionId, app);
  return app;
}

function apiWith(
  provenance: (
    request: DesktopSessionProvenanceRequest,
  ) => Promise<SessionProvenanceView>,
) {
  return {
    sessionProvenance: vi.fn(provenance),
    fetchOperationsSnapshot: vi.fn().mockResolvedValue({ backgroundedTools: [] }),
  } as unknown as DesktopApiAdapter & {
    sessionProvenance: ReturnType<typeof vi.fn>;
    fetchOperationsSnapshot: ReturnType<typeof vi.fn>;
  };
}

function useBoth() {
  return useWorkers(useSessionProvenance());
}

/* the workers of the session `app` selects, under it as the root provides it */
function renderWorkers(app: DesktopApp) {
  return renderHook(() => useBoth(), { wrapper: withApp(app) });
}

describe("subagents of a session", () => {
  it("are the lineage owner's started sessions, matched to summaries by full scope", () => {
    const all = subagentsOf(view([], ["session-a"]), [
      {
        agentDid: AGENT,
        sessionId: "session-a",
        requesterDid: null,
        title: "Explorer",
      },
      /* the same label under another requester is another session */
      { agentDid: AGENT, sessionId: "session-a", requesterDid: PERSON, title: "Other" },
    ] as never);
    expect(all.map((s) => s.sessionId)).toEqual(["session-a"]);
    expect(all[0]!.summary?.title).toBe("Explorer");
  });

  it("joins each call to the request it caused, subagent or not", async () => {
    const started = caused("r-done", "session-done", "completed", "req-1", "call-done");
    const messaged = caused("r-m", "session-old", "processing", "req-1", "call-m");
    const api = apiWith(async () => view([started, messaged], ["session-done"]));
    const start = call("req-1", "call-done");
    const message = call("req-1", "call-m", "running", "message");
    const { result } = renderWorkers(appFor(api, [group(start, message)]));

    await waitFor(() => expect(result.current.byToolCall(start)).toBeTruthy());
    const reachedStart = result.current.byToolCall(start)!;
    expect(reachedStart.request.requestId).toBe("r-done");
    expect(reachedStart.subagent?.sessionId).toBe("session-done");
    expect(workerNow(start, reachedStart, Date.now())).toEqual({
      tone: "done",
      text: "finished",
    });

    const reachedMessage = result.current.byToolCall(message)!;
    expect(reachedMessage.request.sessionId).toBe("session-old");
    expect(reachedMessage.subagent, "a messaged session is not a subagent").toBeNull();
    expect(result.current.all.map((s) => s.sessionId)).toEqual(["session-done"]);
  });

  it("asks for the session's exact scope, requester included", async () => {
    const api = apiWith(async () => view([]));
    renderWorkers(appFor(api, [group(call("req-1", "call-1"))]));
    await waitFor(() =>
      expect(api.sessionProvenance).toHaveBeenCalledWith({
        sessionId: "parent-session",
        agentDid: AGENT,
        requesterDid: PERSON,
      }),
    );
  });

  it("does not ask while the session's scope is unknown", async () => {
    const api = apiWith(async () => view([]));
    renderWorkers(appFor(api, [group(call("req-1", "call-1"))], { sessions: [] }));
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(api.sessionProvenance).not.toHaveBeenCalled();
  });

  it("does not ask while two listed scopes share the session's label", async () => {
    const api = apiWith(async () => view([]));
    renderWorkers(
      appFor(api, [group(call("req-1", "call-1"))], {
        sessions: [
          { agentDid: AGENT, sessionId: "parent-session", requesterDid: PERSON },
          { agentDid: AGENT, sessionId: "parent-session", requesterDid: null },
        ],
      }),
    );
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(api.sessionProvenance).not.toHaveBeenCalled();
  });

  it("never makes a steering message into a started session a start", async () => {
    /* agent_message into a session this one started, while it works */
    const steer = caused("r-steer", "session-a", "processing", "req-2", "call-steer");
    const api = apiWith(async () => view([steer], ["session-a"]));
    const message = call("req-2", "call-steer", "running", "message");
    const { result } = renderWorkers(appFor(api, [group(message)]));
    await waitFor(() => expect(result.current.byToolCall(message)).toBeTruthy());
    expect(result.current.byToolCall(message)!.subagent?.sessionId).toBe("session-a");
    expect(
      message.presentation.kind === "subagent" && message.presentation.action,
    ).toBe("message");
  });

  it("joins a call only through the lineage, never through a summary's latest request", async () => {
    const api = apiWith(async () => view([]));
    const unknown = call("req-1", "call-unknown", "running");
    const app = appFor(api, [group(unknown)], {
      sessions: [
        { agentDid: AGENT, sessionId: "parent-session", requesterDid: PERSON },
        {
          agentDid: AGENT,
          sessionId: "unrelated-session",
          latestRequestId: "call-unknown",
        },
      ],
    });
    const { result } = renderWorkers(app);
    await waitFor(() => expect(result.current.loaded).toBe(true));
    expect(result.current.byToolCall(unknown)).toBeNull();
  });

  it("does not join a call from another request with the same call id", async () => {
    const api = apiWith(async () =>
      view([caused("r-1", "session-1", "completed", "req-1", "call-1")]),
    );
    const other = call("req-2", "call-1");
    const { result } = renderWorkers(appFor(api, [group(other)]));
    await waitFor(() => expect(result.current.loaded).toBe(true));
    expect(result.current.byToolCall(other)).toBeNull();
  });

  it("does not keep another session's provenance for the render that switches", async () => {
    const api = apiWith(async () =>
      view([caused("r-a", "session-a", "completed", "req-1", "call-a")]),
    );
    const items = [group(call("req-1", "call-a"))];
    const app = appFor(api, items, { sessionId: "session-x" });
    const { result } = renderWorkers(app);
    await waitFor(() => expect(result.current.all).toHaveLength(1));
    api.sessionProvenance.mockImplementationOnce(() => new Promise(() => {}));
    act(() => {
      publish(
        app,
        nodeListing([
          { agentDid: AGENT, sessionId: "session-y", requesterDid: PERSON },
        ]),
      );
      app.stores.selection.setState({ sessionId: "session-y" });
      writeSession(app.stores.session, {
        sessionId: "session-y",
        timelineItems: items,
      } as unknown as DesktopSessionSnapshot);
    });
    expect(result.current.all).toHaveLength(0);
  });
});

describe("subagent lineage freshness", () => {
  it("asks again on a session-list change and keeps the last view on a failed ask", async () => {
    let state = "processing";
    const api = apiWith(async () =>
      view([caused("r-1", "session-1", state, "req-1", "call-1")]),
    );
    const tool = call("req-1", "call-1", "success");
    const items = [group(tool)];
    const own = { agentDid: AGENT, sessionId: "parent-session", requesterDid: PERSON };
    const app = appFor(api, items, { sessions: [own] });
    const { result } = renderWorkers(app);
    await waitFor(() =>
      expect(result.current.byToolCall(tool)?.request.lifecycleState).toBe(
        "processing",
      ),
    );

    api.sessionProvenance.mockRejectedValueOnce(new Error("bridge busy"));
    act(() =>
      publish(app, nodeListing([own, { sessionId: "other", turnState: "running" }])),
    );
    await waitFor(() => expect(api.sessionProvenance).toHaveBeenCalledTimes(2));
    expect(result.current.byToolCall(tool)?.request.lifecycleState).toBe("processing");

    state = "completed";
    act(() =>
      publish(app, nodeListing([own, { sessionId: "other", turnState: "completed" }])),
    );
    await waitFor(() =>
      expect(result.current.byToolCall(tool)?.request.lifecycleState).toBe("completed"),
    );
  });

  it("asks again when the store observation moves, with no timer of its own", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      let state = "processing";
      const api = apiWith(async () =>
        view([caused("r-remote", "session-remote", state, "req-1", "call-1")]),
      );
      const tool = call("req-1", "call-1", "success");
      const items = [group(tool)];
      const app = appFor(api, items);
      const observe = (storeVersion: number) =>
        writeSession(app.stores.session, (session) =>
          session
            ? { ...session, projectionRevision: { storeVersion, reconcileVersion: 1 } }
            : session,
        );
      observe(1);
      const { result } = renderWorkers(app);
      await waitFor(() => expect(api.sessionProvenance).toHaveBeenCalledTimes(1));
      await vi.advanceTimersByTimeAsync(60_000);
      expect(api.sessionProvenance).toHaveBeenCalledTimes(1);

      state = "completed";
      act(() => observe(2));
      await waitFor(() =>
        expect(result.current.byToolCall(tool)?.request.lifecycleState).toBe(
          "completed",
        ),
      );
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not follow streamed live deltas once every caused request settled", async () => {
    const api = apiWith(async () =>
      view([caused("r-1", "session-1", "completed", "req-1", "call-1")]),
    );
    const tool = call("req-1", "call-1", "success");
    /* a live delta replaces only the live reply; the rows stay the same objects */
    const toolRow = group(tool);
    const items = [toolRow];
    const app = appFor(api, items);
    const stream = (storeVersion: number) =>
      writeSession(app.stores.session, (session) =>
        session
          ? {
              ...session,
              timelineItems: [
                toolRow,
                {
                  kind: "liveAssistant",
                  itemKey: "live",
                  content: `chunk ${storeVersion}`,
                  reasoning: null,
                } as RenderedTimelineItem,
              ],
              projectionRevision: { storeVersion, reconcileVersion: 1 },
            }
          : session,
      );
    stream(1);
    const { result } = renderWorkers(app);
    await waitFor(() =>
      expect(result.current.byToolCall(tool)?.request.lifecycleState).toBe("completed"),
    );
    for (let version = 2; version <= 50; version += 1) act(() => stream(version));
    await Promise.resolve();
    expect(api.sessionProvenance).toHaveBeenCalledTimes(1);
  });

  it("asks for operations facts only when the transcript has a background process", async () => {
    const api = apiWith(async () => view([]));
    renderWorkers(appFor(api, [group(call("req-1", "call-1"))]));
    await waitFor(() => expect(api.sessionProvenance).toHaveBeenCalled());
    expect(api.fetchOperationsSnapshot).not.toHaveBeenCalled();

    const process = {
      ...call("req-1", "call-p"),
      toolName: "spawn_process",
      presentation: { kind: "process", action: "spawn", target: "bash" },
    } as unknown as RenderedToolCallView;
    renderWorkers(appFor(api, [group(process)]));
    await waitFor(() => expect(api.fetchOperationsSnapshot).toHaveBeenCalled());
  });
});
