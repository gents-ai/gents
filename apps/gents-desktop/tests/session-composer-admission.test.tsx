import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { SessionScreen } from "../src/ui/screens/SessionScreen";
import {
  type DesktopSessionSnapshot,
  projectDeploymentOperationalState,
  type DeploymentView,
} from "@source-inc/gents-desktop-client";
import { projectChatShell } from "@source-inc/gents-desktop-chat";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { node, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";
import { MemoryNavProvider } from "@gents/shell";
import { writeSession } from "../src/hooks/sessionStore";

const navigate = vi.hoisted(() => vi.fn());
const markdownRender = vi.hoisted(() => vi.fn());
vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => {
    markdownRender(children);
    return <div>{children}</div>;
  },
}));
vi.mock("@/lib/router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../src/ui/lib/router")>()),
  navigate,
}));

vi.stubGlobal(
  "IntersectionObserver",
  class {
    observe() {}
    disconnect() {}
  },
);

vi.mock("../src/ui/screens/BehaviorPicker", () => ({
  BehaviorPicker: () => null,
}));

const AGENT = deployment.agentDid;

/* a session held for the screen: interrupted (so a message may follow)
   unless a test says otherwise */
function heldSession(over: Partial<DesktopSessionSnapshot> = {}) {
  return {
    sessionId: "session",
    agentDid: AGENT,
    behaviorId: "default",
    title: "Session",
    turnState: "interrupted",
    timelineItems: [],
    timelinePage: { hasOlder: false },
    latestRequestId: "request",
    latestRequestOutcome: null,
    goal: null,
    context: null,
    ...over,
  } as unknown as DesktopSessionSnapshot;
}

/* the screen's app: the fixture node, ready to chat unless a test gives
   another, and the session it holds (none for the new-session screen) */
function screenApp({
  session = null,
  nodeOverrides = {},
  sendChatMessage = vi
    .fn()
    .mockResolvedValue({ sessionId: "session", requestId: "accepted" }),
  api = {},
}: {
  session?: DesktopSessionSnapshot | null;
  nodeOverrides?: Record<string, unknown>;
  sendChatMessage?: ReturnType<typeof vi.fn>;
  /** more of the bridge, for what a test asks of it */
  api?: Record<string, unknown>;
} = {}) {
  const app = testApp({
    api: {
      sendChatMessage,
      sessionProvenance: vi.fn().mockResolvedValue(null),
      fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
      ...api,
    },
    deployments: [node(nodeOverrides)],
    session,
    selection: { agentDid: AGENT, sessionId: session?.sessionId ?? null },
  });
  return { app, sendChatMessage };
}

function renderScreen(app: DesktopApp) {
  return render(
    <MemoryNavProvider
      initial={{
        name: "session",
        sessionId: app.stores.selection.getState().sessionId,
      }}
    >
      <SessionScreen />
    </MemoryNavProvider>,
    { wrapper: withApp(app) },
  );
}

/* another session selected and held, as the projection would */
function hold(app: DesktopApp, session: DesktopSessionSnapshot) {
  act(() => {
    app.stores.selection.setState({ sessionId: session.sessionId });
    writeSession(app.stores.session, session);
  });
}

describe("SessionScreen canonical composer admission", () => {
  it("keeps transcript markdown out of real context-owned draft updates", async () => {
    const { app } = screenApp({
      session: heldSession({
        timelineItems: [
          {
            kind: "assistantMessage",
            itemKey: "assistant-stable",
            sequence: 1,
            content: "Stable transcript",
            reasoning: null,
            timestamp: null,
          },
        ] as DesktopSessionSnapshot["timelineItems"],
      }),
    });
    markdownRender.mockClear();
    renderScreen(app);
    expect(markdownRender).toHaveBeenCalledTimes(1);
    await userEvent.type(screen.getByLabelText("Message"), "a real draft");
    expect(screen.getByLabelText("Message")).toHaveValue("a real draft");
    expect(markdownRender).toHaveBeenCalledTimes(1);
  });

  it("does not erase text edited while the prior draft is being accepted", async () => {
    let resolve!: (result: { sessionId: string; requestId: string }) => void;
    const pending = new Promise<{ sessionId: string; requestId: string }>((done) => {
      resolve = done;
    });
    const { app } = screenApp({
      session: heldSession(),
      sendChatMessage: vi.fn(() => pending),
    });
    renderScreen(app);
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "send this" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "keep this next message" },
    });
    await act(async () => {
      resolve({ sessionId: "session", requestId: "accepted" });
      await pending;
    });
    expect(screen.getByLabelText("Message")).toHaveValue("keep this next message");
  });

  it("restores each session draft without carrying text into another session", () => {
    const first = heldSession();
    const second = heldSession({ sessionId: "second" });
    const { app } = screenApp({ session: first });
    renderScreen(app);
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "first draft" },
    });
    hold(app, second);
    expect(screen.getByLabelText("Message")).toHaveValue("");
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "second draft" },
    });
    hold(app, first);
    expect(screen.getByLabelText("Message")).toHaveValue("first draft");
    hold(app, second);
    expect(screen.getByLabelText("Message")).toHaveValue("second draft");
  });
  it("uses the selected non-default behavior decision, never the default or retry path", () => {
    const selected = (defaultReady: boolean, selectedReady: boolean) => {
      const deployment = {
        agentDid: "did:key:agent",
        dialSucceeded: true,
        chatSafe: true,
        agentPrincipal: { agentDid: "did:key:agent", defaultBehaviorId: "default" },
        behaviors: [
          {
            behaviorId: "default",
            displayName: "Default",
            enabled: true,
            isDefault: true,
          },
          {
            behaviorId: "selected",
            displayName: "Selected",
            enabled: true,
            isDefault: false,
          },
        ],
        behaviorReadiness: {
          source: { state: "current" },
          activeGeneration: 1,
          routerGeneration: 1,
          updatedAt: "2026-09-15T00:00:00Z",
          behaviors: [
            defaultReady
              ? { state: "ready", behaviorId: "default" }
              : {
                  state: "unavailable",
                  behaviorId: "default",
                  reason: "backend_disabled",
                },
            selectedReady
              ? { state: "ready", behaviorId: "selected" }
              : {
                  state: "unavailable",
                  behaviorId: "selected",
                  reason: "backend_disabled",
                },
          ],
        },
        sessions: [],
      } as unknown as DeploymentView;
      return projectChatShell({
        clientAvailable: true,
        selectedAgentDid: deployment.agentDid,
        selectedSessionId: null,
        sending: false,
        session: null,
        selectedSessionSummary: null,
        localWorkflow: { kind: "ready" },
        operationalState: projectDeploymentOperationalState(
          deployment,
          "selected",
          null,
        ),
      }).nonEmptyContentSendStatus;
    };

    expect(selected(false, true)).toEqual({ kind: "ready" });
    expect(selected(true, false)).toMatchObject({
      kind: "disabled",
      reason: "behaviorUnavailable",
    });
  });

  it("keeps an empty composer editable, then submits when canonical admission is ready", async () => {
    const user = userEvent.setup();
    const { app, sendChatMessage } = screenApp();
    renderScreen(app);

    const message = screen.getByRole("textbox", { name: "Message" });
    const send = screen.getByRole("button", { name: "Send" });
    expect(message).toBeEnabled();
    expect(send).toBeDisabled();
    await user.type(message, "hello");
    expect(send).toBeEnabled();
    await user.click(send);
    expect(sendChatMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        content: "hello",
        behaviorId: "default",
        sessionId: null,
      }),
    );
  });

  it("clears accepted text only in its origin after navigating away", async () => {
    navigate.mockClear();
    let resolve!: (value: { sessionId: string; requestId: string }) => void;
    const sendChatMessage = vi.fn(
      () =>
        new Promise<{ sessionId: string; requestId: string }>((next) => {
          resolve = next;
        }),
    );
    const { app } = screenApp({ sendChatMessage });
    renderScreen(app);
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "keep this draft" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    /* the person moves to another node while the send is in flight */
    act(() => app.actions.selectAgent("did:key:other-agent"));
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "unrelated draft" },
    });
    await act(async () => {
      resolve({ sessionId: "old-session", requestId: "old-request" });
      await Promise.resolve();
    });

    expect(sendChatMessage).toHaveBeenCalledOnce();
    expect(screen.getByLabelText("Message")).toHaveValue("unrelated draft");
    expect(navigate).not.toHaveBeenCalled();
    act(() => app.actions.selectAgent(AGENT));
    expect(screen.getByLabelText("Message")).toHaveValue("");
  });

  it("preserves a canonical blocker for a non-empty local draft", () => {
    /* a node whose route is not safe to chat over */
    const { app, sendChatMessage } = screenApp({ nodeOverrides: { chatSafe: false } });
    renderScreen(app);
    const status = app.view.getState().shellProjection.nonEmptyContentSendStatus;
    expect(status.kind).toBe("disabled");

    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "hello" } });
    const send = screen.getByRole("button", { name: "Send" });
    expect(screen.getByRole("textbox", { name: "Message" })).toBeDisabled();
    expect(send).toBeDisabled();
    expect(screen.getByLabelText("Message")).toHaveAttribute(
      "placeholder",
      status.kind === "disabled" ? status.hint : "",
    );
    fireEvent.click(send);
    expect(sendChatMessage).not.toHaveBeenCalled();
  });

  it("re-enables the rendered existing-session composer after terminal interruption", () => {
    const { app } = screenApp({ session: heldSession({ turnState: "running" }) });
    renderScreen(app);
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "follow up" },
    });
    /* while the turn runs the composer offers Stop, not Send */
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();

    hold(app, heldSession({ turnState: "interrupted" }));
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled();
    expect(screen.getByLabelText("Message")).toHaveValue("follow up");
  });
});

describe("SessionScreen goal label", () => {
  const goal = (status: string, wrapupCompleted: boolean) => ({
    goalId: "g",
    status,
    objective: "Unfinished work",
    wrapupRequested: true,
    wrapupCompleted,
    tokensUsed: 1000,
    tokenBudget: 1000,
    consecutiveBlockedAudits: 0,
    activeTimeSeconds: 12,
    continuationSequence: 1,
    lastBlockedReason: null,
    lastFailure: null,
    completionEvidence: null,
  });

  it("does not report a wrapped-up budget-limited goal as met", () => {
    const { app } = screenApp({
      session: heldSession({ goal: goal("budget_limited", true) } as never),
    });
    renderScreen(app);
    expect(screen.queryByText("Goal met")).not.toBeInTheDocument();
    expect(screen.getByText("Goal · budget reached")).toBeInTheDocument();
  });

  it("reports a complete goal as met", () => {
    const { app } = screenApp({
      session: heldSession({ goal: goal("complete", true) } as never),
    });
    renderScreen(app);
    expect(screen.getByText("Goal met")).toBeInTheDocument();
  });
});

describe("renaming a session", () => {
  it("asks the node holding it while a read does not list that node", async () => {
    const renameSession = vi.fn().mockResolvedValue(undefined);
    const app = testApp({
      api: {
        renameSession,
        fetchSessionSnapshot: vi.fn().mockResolvedValue(null),
        fetchDesktopSnapshot: vi
          .fn()
          .mockResolvedValue({ bootstrap: {}, client: null }),
      },
      deployments: [],
      session: heldSession(),
    });
    await app.actions.renameSession("session", "Renamed");
    expect(renameSession).toHaveBeenCalledWith({
      agentDid: AGENT,
      sessionId: "session",
      title: "Renamed",
    });
  });

  it("fails, and says so, when no node holds the session", async () => {
    const renameSession = vi.fn();
    const reportFailure = vi.fn();
    const app = testApp({ api: { renameSession }, deployments: [], reportFailure });
    await expect(app.actions.renameSession("session", "Renamed")).rejects.toBeDefined();
    expect(renameSession).not.toHaveBeenCalled();
    expect(reportFailure).toHaveBeenCalledOnce();
  });

  it("renames through its action, then reads the session again", async () => {
    const renameSession = vi.fn().mockResolvedValue(undefined);
    const fetchSessionSnapshot = vi.fn().mockResolvedValue(null);
    const { app } = screenApp({
      session: heldSession(),
      api: {
        renameSession,
        fetchSessionSnapshot,
        fetchDesktopSnapshot: vi
          .fn()
          .mockResolvedValue({ bootstrap: {}, client: null }),
      },
    });
    renderScreen(app);
    const user = userEvent.setup();
    await user.click(screen.getAllByRole("button", { name: "Rename Session" })[0]!);
    const input = screen.getByRole("textbox", { name: "Rename Session" });
    await user.clear(input);
    await user.type(input, "Renamed session{Enter}");
    await vi.waitFor(() =>
      expect(renameSession).toHaveBeenCalledWith({
        agentDid: AGENT,
        sessionId: "session",
        title: "Renamed session",
      }),
    );
    /* the title shown is the held session's, so it is read again */
    await vi.waitFor(() =>
      expect(fetchSessionSnapshot).toHaveBeenCalledWith(
        "session",
        AGENT,
        null,
        expect.objectContaining({ limit: expect.any(Number) }),
      ),
    );
  });
});
