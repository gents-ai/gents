import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { MemoryNavProvider } from "@gents/shell";
import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

import { createChatActions } from "../src/hooks/chatActions";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { writeSession } from "../src/hooks/sessionStore";
import { sessionDraftKey } from "../src/hooks/shellView";
import type { ShellProjection } from "../src/hooks/shellProjection";
import { SessionScreen } from "../src/ui/screens/SessionScreen";
import { node, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";
import { shellStores } from "./shell-fixture";
import { pendingUserTurn, userMessage } from "./timeline-fixture";

const navigate = vi.hoisted(() => vi.fn());
vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));
vi.mock("@/lib/router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../src/ui/lib/router")>()),
  navigate,
}));
vi.mock("../src/ui/screens/BehaviorPicker", () => ({
  AgentPicker: () => null,
}));
vi.stubGlobal(
  "IntersectionObserver",
  class {
    observe() {}
    disconnect() {}
  },
);

const NODE = deployment.nodeDid;
const CLIENT = "did:key:desktop-client";
const REFUSAL =
  "the selected user message is missing, ambiguous, or not editable: message-2";

function heldSession(over: Partial<DesktopSessionSnapshot> = {}) {
  return {
    sessionId: "source",
    nodeDid: NODE,
    agentId: "default",
    title: "Source",
    turnState: "completed",
    timelineItems: [
      userMessage({ itemKey: "message-1", requestId: "r1", content: "first ask" }),
      userMessage({ itemKey: "message-2", requestId: "r2", content: "second ask" }),
    ],
    timelinePage: { hasOlder: false },
    latestRequestId: "r2",
    latestRequestOutcome: null,
    goal: null,
    context: null,
    queuedTurns: [],
    foldedInputs: [],
    ...over,
  } as unknown as DesktopSessionSnapshot;
}

function screenApp(forkSessionBeforeMessage: ReturnType<typeof vi.fn>) {
  const reportFailure = vi.fn();
  const session = heldSession();
  const app = testApp({
    api: {
      forkSessionBeforeMessage,
      fetchDesktopSnapshot: vi.fn().mockResolvedValue(null),
      sessionProvenance: vi.fn().mockResolvedValue(null),
      fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
    },
    snapshot: {
      bootstrap: {},
      client: { clientDid: CLIENT, deployments: [node()] },
    },
    session,
    reportFailure,
  });
  render(
    <MemoryNavProvider initial={{ name: "session", sessionId: session.sessionId }}>
      <SessionScreen />
    </MemoryNavProvider>,
    { wrapper: withApp(app) },
  );
  return { app, reportFailure };
}

const branchButtons = () => screen.getAllByRole("button", { name: "Fork" });

describe("edit an earlier message in a new branch", () => {
  it("forks before the message and opens the branch with its text in the composer", async () => {
    navigate.mockClear();
    const fork = vi.fn().mockResolvedValue({ nodeDid: NODE, sessionId: "branch" });
    const { app } = screenApp(fork);

    fireEvent.click(branchButtons()[1]!);
    await waitFor(() =>
      expect(navigate).toHaveBeenCalledWith({ name: "session", sessionId: "branch" }),
    );
    expect(fork).toHaveBeenCalledWith({
      nodeDid: NODE,
      sessionId: "source",
      messageKey: "message-2",
      clientDid: CLIENT,
      childSessionId: expect.any(String),
    });
    expect(app.drafts.getState().drafts[sessionDraftKey(NODE, "branch")]).toBe(
      "second ask",
    );

    act(() => {
      app.stores.selection.setState({ sessionId: "branch" });
      writeSession(app.stores.session, heldSession({ sessionId: "branch" }));
    });
    const composer = screen.getByLabelText("Message");
    expect(composer).toHaveValue("second ask");
    expect(composer).toHaveFocus();
    expect(app.drafts.getState().drafts[sessionDraftKey(NODE, "source")]).toBe(
      undefined,
    );
  });

  it("disables the action while the fork is in flight", async () => {
    let resolve!: (result: { nodeDid: string; sessionId: string }) => void;
    const fork = vi.fn(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    );
    screenApp(fork);

    fireEvent.click(branchButtons()[0]!);
    await waitFor(() => expect(branchButtons()[0]).toBeDisabled());
    await act(async () => resolve({ nodeDid: NODE, sessionId: "branch" }));
    expect(branchButtons()[0]).toBeEnabled();
  });

  it("reports a refusal and stays on the original session", async () => {
    navigate.mockClear();
    const fork = vi.fn().mockRejectedValue(new Error(REFUSAL));
    const { app, reportFailure } = screenApp(fork);

    fireEvent.click(branchButtons()[1]!);
    await waitFor(() =>
      expect(reportFailure).toHaveBeenCalledWith(`Couldn’t start a branch: ${REFUSAL}`),
    );
    expect(reportFailure).toHaveBeenCalledTimes(1);
    expect(navigate).not.toHaveBeenCalled();
    expect(app.drafts.getState().drafts).toEqual({});
    expect(app.stores.selection.getState().sessionId).toBe("source");
    await waitFor(() => expect(branchButtons()[1]).toBeEnabled());
  });

  it("is offered only on sent messages, not on ones still waiting", () => {
    const app: DesktopApp = testApp({
      api: {
        sessionProvenance: vi.fn().mockResolvedValue(null),
        fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
      },
      snapshot: {
        bootstrap: {},
        client: { clientDid: CLIENT, deployments: [node()] },
      },
      session: heldSession({
        timelineItems: [
          userMessage({ itemKey: "message-1", requestId: "r1", content: "sent" }),
          pendingUserTurn({ itemKey: "pending", requestId: "r2", content: "waiting" }),
        ],
      }),
    });
    render(
      <MemoryNavProvider initial={{ name: "session", sessionId: "source" }}>
        <SessionScreen />
      </MemoryNavProvider>,
      { wrapper: withApp(app) },
    );
    expect(branchButtons()).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Copy" })).toHaveLength(2);
  });
});

describe("forkBeforeMessage", () => {
  function harness(forkSessionBeforeMessage: ReturnType<typeof vi.fn>) {
    const stores = shellStores({ selection: { nodeDid: NODE, sessionId: "source" } });
    stores.client.setState({
      snapshot: {
        bootstrap: {},
        client: { clientDid: CLIENT, deployments: [deployment] },
      } as unknown as DesktopClientSnapshot,
    });
    stores.chat.setState({ folders: { source: "/work/project" } });
    const reportFailure = vi.fn();
    const refreshSnapshot = vi.fn(async () => {});
    const actions = createChatActions({
      api: { forkSessionBeforeMessage } as unknown as DesktopApiAdapter,
      stores,
      project: () => ({}) as ShellProjection,
      refreshSession: async () => null,
      refreshSnapshot,
      reportFailure,
    });
    return { actions, stores, reportFailure, refreshSnapshot };
  }

  it("asks again for the same branch after a failure, and a fresh one after success", async () => {
    const fork = vi
      .fn()
      .mockRejectedValueOnce(new Error("reply lost"))
      .mockImplementation(async (request: { childSessionId: string }) => ({
        nodeDid: NODE,
        sessionId: request.childSessionId,
      }));
    const { actions, stores, reportFailure, refreshSnapshot } = harness(fork);

    await expect(actions.forkBeforeMessage("source", "message-2")).rejects.toThrow(
      "reply lost",
    );
    expect(reportFailure).toHaveBeenCalledWith("Couldn’t start a branch: reply lost");
    const recovered = await actions.forkBeforeMessage("source", "message-2");
    const another = await actions.forkBeforeMessage("source", "message-2");

    const destinations = fork.mock.calls.map(([request]) => request.childSessionId);
    expect(destinations[1]).toBe(destinations[0]);
    expect(destinations[2]).not.toBe(destinations[0]);
    expect(recovered.sessionId).toBe(destinations[0]);
    expect(stores.chat.getState().folders).toMatchObject({
      source: "/work/project",
      [recovered.sessionId]: "/work/project",
      [another.sessionId]: "/work/project",
    });
    expect(refreshSnapshot).toHaveBeenCalledTimes(2);
  });

  it("refuses without a running desktop client", async () => {
    const fork = vi.fn();
    const { actions, stores, reportFailure } = harness(fork);
    stores.client.setState({ snapshot: null });

    await expect(actions.forkBeforeMessage("source", "message-2")).rejects.toThrow();
    expect(fork).not.toHaveBeenCalled();
    expect(reportFailure).toHaveBeenCalledWith(
      "Couldn’t start a branch: the desktop client is not running",
    );
  });
});
