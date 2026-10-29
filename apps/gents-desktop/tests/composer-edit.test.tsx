import { acceptedChatReply } from "./chat-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  DesktopSessionSnapshot,
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import { MemoryNavProvider } from "@gents/shell";
import { SessionScreen } from "../src/ui/screens/SessionScreen";
import { writeSession } from "../src/hooks/sessionStore";
import { node, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";
import { composerText, typeInComposer, typeText } from "./composer-field";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));
vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));
vi.mock("../src/ui/screens/BehaviorPicker", () => ({ AgentPicker: () => null }));
beforeEach(() => {
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
});

const NODE = deployment.nodeDid;
const entry = (id: string, group: string | null = "same"): PendingQueueEntryView => ({
  requestDocId: `doc-${id}`,
  requestId: id,
  content: `message ${id}`,
  editable: group !== null,
  steerable: group !== null,
  editGroup: group,
});
const turn = (row: PendingQueueEntryView): PendingTurnView => ({
  requestId: row.requestId,
  content: row.content,
  selectedSkillIds: [],
  lifecycleState: "pending",
  foldedIntoRequestId: null,
  origin: null,
  createdAt: null,
});
const session = (queue: PendingQueueEntryView[]) =>
  ({
    sessionId: "session",
    nodeDid: NODE,
    agentId: "default",
    title: "Session",
    turnState: "running",
    timelineItems: [],
    timelinePage: { hasOlder: false },
    latestRequestId: "request",
    latestRequestOutcome: null,
    goal: null,
    context: null,
    queuedTurns: queue.map(turn),
    pendingQueue: queue,
    foldedInputs: [],
  }) as unknown as DesktopSessionSnapshot;

function setup(
  queue: PendingQueueEntryView[],
  editPendingQueue = vi.fn(),
  sendChatMessage = vi.fn(),
) {
  const held = session(queue);
  const app = testApp({
    api: {
      sendChatMessage,
      editPendingQueue,
      fetchSessionSnapshot: vi.fn().mockResolvedValue(held),
      sessionProvenance: vi.fn().mockResolvedValue(null),
      fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
    },
    deployments: [node({})],
    session: held,
    selection: { nodeDid: NODE, sessionId: "session" },
  });
  render(
    <MemoryNavProvider initial={{ name: "session", sessionId: "session" }}>
      <SessionScreen />
    </MemoryNavProvider>,
    { wrapper: withApp(app) },
  );
  return { app, editPendingQueue };
}
const composerBox = () => within(screen.getByTestId("composer")).getByRole("textbox");
const rowFor = (id: string) =>
  screen.getAllByTestId("queued-input").find((row) => row.dataset.requestId === id)!;

describe("editing a waiting message in the composer", () => {
  it("puts the message in the composer and saves it fenced on the queue", async () => {
    const user = userEvent.setup();
    const { editPendingQueue } = setup(
      [entry("a"), entry("barrier", null), entry("b")],
      vi.fn().mockResolvedValue(undefined),
    );
    await user.click(within(rowFor("a")).getByRole("button", { name: "Edit" }));
    expect(screen.getByTestId("editing-message")).toBeInTheDocument();
    expect(composerText(composerBox())).toBe("message a");
    expect(screen.getByRole("button", { name: "Save" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Send" })).toBeNull();
    typeInComposer(composerBox(), "");
    await typeText(composerBox(), "corrected instruction{Enter}");
    await waitFor(() =>
      expect(editPendingQueue).toHaveBeenCalledWith(
        expect.objectContaining({
          expectedRequestDocIds: ["doc-a", "doc-barrier", "doc-b"],
          selectedRequestDocIds: ["doc-a"],
          messages: [
            {
              requestDocId: "doc-a",
              content: "corrected instruction",
              authoredInput: {
                selected_skill_ids: [],
                captured_context: [],
                attachments: [],
              },
            },
          ],
        }),
      ),
    );
    await waitFor(() => expect(screen.queryByTestId("editing-message")).toBeNull());
  });

  it("keeps the draft aside and gives it back on Esc", async () => {
    const user = userEvent.setup();
    setup([entry("a")]);
    await typeText(composerBox(), "half a thought");
    await user.click(within(rowFor("a")).getByRole("button", { name: "Edit" }));
    expect(composerText(composerBox())).toBe("message a");
    await waitFor(() => expect(composerBox()).toHaveFocus());
    await user.keyboard("{Escape}");
    expect(screen.queryByTestId("editing-message")).toBeNull();
    expect(composerText(composerBox())).toBe("half a thought");
  });

  it("edits the last waiting message on ↑ in an empty composer", async () => {
    const user = userEvent.setup();
    setup([entry("a"), entry("b")]);
    await user.click(composerBox());
    await user.keyboard("{ArrowUp}");
    expect(composerText(composerBox())).toBe("message b");
    expect(rowFor("b").dataset.editing).toBe("true");
    expect(within(rowFor("b")).getByText("Editing…")).toBeInTheDocument();
  });

  it("holds the edit until the runtime answers, and keeps it if refused", async () => {
    const user = userEvent.setup();
    let reject!: (error: Error) => void;
    const editPendingQueue = vi.fn(
      () => new Promise<void>((_, fail) => (reject = fail)),
    );
    setup([entry("a"), entry("b")], editPendingQueue);
    await user.click(within(rowFor("a")).getByRole("button", { name: "Edit" }));
    typeInComposer(composerBox(), "");
    await typeText(composerBox(), "corrected{Enter}");
    await waitFor(() => expect(editPendingQueue).toHaveBeenCalled());
    expect(screen.getByTestId("editing-message")).toBeInTheDocument();
    expect(within(rowFor("b")).getByRole("button", { name: "Remove" })).toBeDisabled();
    await act(async () => reject(new Error("The message queue changed.")));
    expect(screen.getByTestId("editing-message")).toBeInTheDocument();
    expect(composerText(composerBox())).toBe("corrected");
    expect(within(rowFor("a")).getByText("message a")).toBeInTheDocument();
  });

  it("holds an edit the agent took as already sent, sending it only on a click", async () => {
    const user = userEvent.setup();
    const sendChatMessage = vi.fn(acceptedChatReply);
    const { app } = setup([entry("a")], vi.fn(), sendChatMessage);
    await user.click(within(rowFor("a")).getByRole("button", { name: "Edit" }));
    await typeText(composerBox(), " and more");
    act(() => writeSession(app.stores.session, session([])));
    await waitFor(() =>
      expect(screen.getByTestId("editing-message").dataset.reached).toBe("true"),
    );
    await user.click(composerBox());
    await user.keyboard("{Enter}");
    expect(sendChatMessage).not.toHaveBeenCalled();
    expect(composerText(composerBox())).toBe("message a and more");
    await user.click(screen.getByRole("button", { name: "Send as a new message" }));
    await waitFor(() =>
      expect(sendChatMessage).toHaveBeenCalledWith(
        expect.objectContaining({ content: "message a and more" }),
      ),
    );
  });

  it("keeps an edit the agent took as the draft on Esc", async () => {
    const user = userEvent.setup();
    const { app } = setup([entry("a")]);
    await user.click(within(rowFor("a")).getByRole("button", { name: "Edit" }));
    act(() => writeSession(app.stores.session, session([])));
    await waitFor(() =>
      expect(screen.getByTestId("editing-message").dataset.reached).toBe("true"),
    );
    await user.click(composerBox());
    await user.keyboard("{Escape}");
    expect(screen.queryByTestId("editing-message")).toBeNull();
    expect(composerText(composerBox())).toBe("message a");
  });

  it("keeps an edit the agent took when sending it as a new message is refused", async () => {
    const user = userEvent.setup();
    const sendChatMessage = vi.fn().mockRejectedValue(new Error("Sending is paused."));
    const { app } = setup([entry("a")], vi.fn(), sendChatMessage);
    await user.click(within(rowFor("a")).getByRole("button", { name: "Edit" }));
    await typeText(composerBox(), " and more");
    act(() => writeSession(app.stores.session, session([])));
    await waitFor(() =>
      expect(screen.getByTestId("editing-message").dataset.reached).toBe("true"),
    );
    await user.click(screen.getByRole("button", { name: "Send as a new message" }));
    await waitFor(() => expect(sendChatMessage).toHaveBeenCalled());
    await waitFor(() => expect(composerText(composerBox())).toBe("message a and more"));
    expect(screen.getByTestId("editing-message")).toBeInTheDocument();
  });

  it("holds the text while a save is out, so nothing typed meanwhile is dropped", async () => {
    const user = userEvent.setup();
    let resolve!: () => void;
    const editPendingQueue = vi.fn(() => new Promise<void>((done) => (resolve = done)));
    setup([entry("a"), entry("b")], editPendingQueue);
    await user.click(within(rowFor("a")).getByRole("button", { name: "Edit" }));
    typeInComposer(composerBox(), "");
    await typeText(composerBox(), "corrected{Enter}");
    await waitFor(() => expect(editPendingQueue).toHaveBeenCalled());
    await typeText(composerBox(), " again");
    expect(composerText(composerBox())).toBe("corrected");
    await act(async () => resolve());
    await waitFor(() => expect(screen.queryByTestId("editing-message")).toBeNull());
  });
});

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());
