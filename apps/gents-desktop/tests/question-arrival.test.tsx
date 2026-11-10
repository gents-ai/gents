import { acceptedChatReply } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  DesktopSessionSnapshot,
  MailboxItemView,
} from "@source-inc/gents-desktop-client";
import { MemoryNavProvider } from "@gents/shell";
import { SessionScreen } from "../src/ui/screens/SessionScreen";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { node, publish, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";
import { composerText, typeText } from "./composer-field";

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));
vi.mock("../src/ui/screens/BehaviorPicker", () => ({
  AgentPicker: () => null,
}));

const NODE = deployment.nodeDid;

const session = {
  sessionId: "session",
  nodeDid: NODE,
  agentDid: "default",
  title: "Session",
  turnState: "interrupted",
  timelineItems: [],
  timelinePage: { hasOlder: false },
  latestRequestId: "request",
  latestRequestOutcome: null,
  goal: null,
  context: null,
  queuedTurns: [],
  foldedInputs: [],
} as unknown as DesktopSessionSnapshot;

const question: MailboxItemView = {
  itemId: "question",
  itemKey: "question",
  requesterDid: "did:key:person",
  nodeDid: NODE,
  status: "open",
  kind: "ask",
  action: "start_request",
  title: "Question",
  summary: null,
  payload: JSON.stringify({
    version: 1,
    prompt: "Which backend should the crew use?",
    options: [
      { id: "local", label: "Local model" },
      { id: "claude", label: "Claude" },
    ],
    multi_select: false,
    allow_free_text: true,
  }),
  sourceKind: "session",
  sourceId: "session",
  sessionId: "session",
  requestId: "request",
  causeDocId: null,
  targetNodeDid: NODE,
  targetAgentDid: "default",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: new Date().toISOString(),
};

function renderSession() {
  const sendChatMessage = vi.fn(acceptedChatReply);
  const app = testApp({
    api: {
      sendChatMessage,
      sessionProvenance: vi.fn().mockResolvedValue(null),
      fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
    },
    deployments: [node()],
    session,
    selection: { nodeDid: NODE, sessionId: session.sessionId },
  });
  render(
    <MemoryNavProvider initial={{ name: "session", sessionId: session.sessionId }}>
      <SessionScreen />
    </MemoryNavProvider>,
    { wrapper: withApp(app) },
  );
  return { app, sendChatMessage, message: screen.getByLabelText("Message") };
}

const ask = (app: DesktopApp) =>
  act(() => publish(app, [node({ mailboxItems: [question] })]));

beforeEach(() => {
  vi.stubGlobal("localStorage", goalDeliveryStorage());
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
});
afterEach(() => vi.unstubAllGlobals());

describe("a question that arrives in an open session", () => {
  it("waits beside the composer while the person writes, keeping their draft and keys", async () => {
    const { app, sendChatMessage, message } = renderSession();
    await typeText(message, "half a thought");
    ask(app);
    expect(screen.queryByTestId("question-panel")).toBeNull();
    expect(screen.getByText(/is waiting for your answer/)).toBeVisible();
    expect(screen.getByTestId("composer")).toBeVisible();
    expect(message).toHaveFocus();
    expect(composerText(message)).toBe("half a thought");
    fireEvent.keyDown(message, { key: "1" });
    expect(sendChatMessage).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Answer" }));
    expect(screen.getByTestId("question-panel")).toHaveFocus();
  });

  it("takes the place of a composer no one is writing in", () => {
    const { app, message } = renderSession();
    act(() => message.blur());
    ask(app);
    expect(screen.getByTestId("question-panel")).toBeVisible();
    expect(screen.getByTestId("composer")).not.toBeVisible();
  });
});
