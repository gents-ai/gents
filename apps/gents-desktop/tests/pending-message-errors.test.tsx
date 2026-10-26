import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type {
  DesktopApiAdapter,
  DesktopSessionSnapshot,
  PendingQueueEntryView,
  PendingTurnView,
} from "@source-inc/gents-desktop-client";
import { createChatActions } from "../src/hooks/chatActions";
import type { ShellProjection } from "../src/hooks/shellProjection";
import { PendingTray } from "../src/ui/screens/PendingTray";
import { moveDisabled, moveItem } from "./pending-tray-menu";
import { deployment } from "./config-panel-wiring/fixtures";
import { shellStores } from "./shell-fixture";
import { writeSession } from "../src/hooks/sessionStore";
import { deferred } from "./deferred";

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

/* rows by request, since a row being edited shows its editor, not its text */
function rowFor(id: string) {
  const row = screen
    .getAllByTestId("queued-input")
    .find((node) => node.dataset.requestId === id);
  if (!row) throw new Error(`no queued row for ${id}`);
  return row;
}

function rows() {
  return screen.getAllByTestId("queued-input").map((row) => row.dataset.requestId);
}

describe("pending message failed mutations and async controls", () => {
  it("sends a removal fenced on the whole queue, holding the tray until it is answered", async () => {
    const gate = deferred();
    const onEdit = vi.fn().mockReturnValue(gate.promise);
    const view = render(
      <PendingTray
        queued={[turn(entry("a")), turn(entry("b"))]}
        queue={[entry("a"), entry("b")]}
        onEdit={onEdit}
      />,
    );
    fireEvent.click(within(rowFor("b")).getByRole("button", { name: "Remove" }));
    expect(onEdit).toHaveBeenCalledWith({
      expectedRequestDocIds: ["doc-a", "doc-b"],
      selectedRequestDocIds: ["doc-b"],
      messages: [],
    });
    expect(rows()).toEqual(["a", "b"]);
    expect(within(rowFor("a")).getByRole("button", { name: "Remove" })).toBeDisabled();
    await act(async () => gate.resolve());
    view.rerender(
      <PendingTray queued={[turn(entry("a"))]} queue={[entry("a")]} onEdit={onEdit} />,
    );
    expect(rows()).toEqual(["a"]);
    expect(within(rowFor("a")).getByRole("button", { name: "Remove" })).toBeEnabled();
  });

  it("leaves the queue as it was when the runtime refuses", async () => {
    let reject!: (error: Error) => void;
    const onEdit = vi.fn(() => new Promise<void>((_, fail) => (reject = fail)));
    render(
      <PendingTray
        queued={[turn(entry("a")), turn(entry("b"))]}
        queue={[entry("a"), entry("b")]}
        onEdit={onEdit}
      />,
    );
    fireEvent.click(within(rowFor("a")).getByRole("button", { name: "Remove" }));
    await act(async () => reject(new Error("mutation failed")));
    expect(rows()).toEqual(["a", "b"]);
    expect(within(rowFor("a")).getByRole("button", { name: "Remove" })).toBeEnabled();
  });

  it("moves from the menu, fenced on the whole queue, in the order the runtime confirms", async () => {
    const gate = deferred();
    const onEdit = vi.fn().mockReturnValue(gate.promise);
    const view = render(
      <PendingTray
        queued={[turn(entry("a")), turn(entry("b"))]}
        queue={[entry("a"), entry("b")]}
        onEdit={onEdit}
      />,
    );
    fireEvent.click(await moveItem(rowFor("a"), "down"));
    expect(onEdit).toHaveBeenCalledWith({
      expectedRequestDocIds: ["doc-a", "doc-b"],
      selectedRequestDocIds: ["doc-a", "doc-b"],
      messages: [
        { requestDocId: "doc-b", content: "message b" },
        { requestDocId: "doc-a", content: "message a" },
      ],
    });
    await act(async () => gate.resolve());
    view.rerender(
      <PendingTray
        queued={[turn(entry("b2")), turn(entry("a2"))]}
        queue={[entry("b2"), entry("a2")]}
        onEdit={onEdit}
      />,
    );
    expect(rows()).toEqual(["b2", "a2"]);
  });

  it("cannot cross the cross-requester barrier, before or after a failed mutation", async () => {
    const onEdit = vi
      .fn()
      .mockRejectedValueOnce(new Error("mutation failed"))
      .mockResolvedValue(undefined);
    render(
      <PendingTray
        queued={[turn(entry("a")), turn(entry("b"))]}
        queue={[entry("a"), entry("barrier", null), entry("b")]}
        onEdit={onEdit}
      />,
    );
    const before = within(rowFor("a"));
    expect(await moveDisabled(rowFor("a"), "down")).toBe(true);
    expect(await moveDisabled(rowFor("b"), "up")).toBe(true);

    fireEvent.click(before.getByRole("button", { name: "Remove" }));
    await waitFor(() =>
      expect(before.getByRole("button", { name: "Remove" })).toBeEnabled(),
    );
    expect(onEdit).toHaveBeenCalledWith({
      expectedRequestDocIds: ["doc-a", "doc-barrier", "doc-b"],
      selectedRequestDocIds: ["doc-a"],
      messages: [],
    });
    expect(rows()).toEqual(["a", "b"]);
    expect(await moveDisabled(rowFor("a"), "down")).toBe(true);
    expect(await moveDisabled(rowFor("b"), "up")).toBe(true);
  });

  it("says a message already reached the agent when the edit lost that race", async () => {
    const stores = shellStores({
      deployments: [deployment],
      selection: { nodeDid: deployment.nodeDid, sessionId: "s1" },
    });
    writeSession(stores.session, {
      sessionId: "s1",
      nodeDid: deployment.nodeDid,
      pendingQueue: [entry("a"), entry("b")],
      foldedInputs: [],
      latestRequestId: "earlier",
    } as unknown as DesktopSessionSnapshot);
    const refreshSession = vi.fn().mockResolvedValue({
      sessionId: "s1",
      nodeDid: deployment.nodeDid,
      pendingQueue: [],
      foldedInputs: [{ requestId: "a" }, { requestId: "b" }],
      latestRequestId: "earlier",
    } as unknown as DesktopSessionSnapshot);
    const reportFailure = vi.fn();
    const actions = createChatActions({
      api: {
        editPendingQueue: vi
          .fn()
          .mockRejectedValue(
            new Error("The message queue changed. Refresh it and try again."),
          ),
      } as unknown as DesktopApiAdapter,
      stores,
      project: () => ({}) as ShellProjection,
      refreshSession,
      refreshSnapshot: vi.fn(),
      reportFailure,
    });
    await expect(
      actions.editPendingQueue({
        nodeDid: deployment.nodeDid,
        sessionId: "s1",
        expectedRequestDocIds: ["doc-a", "doc-b"],
        selectedRequestDocIds: ["doc-a", "doc-b"],
        messages: [
          { requestDocId: "doc-b", content: "message b" },
          { requestDocId: "doc-a", content: "message a" },
        ],
      }),
    ).rejects.toThrow();
    expect(reportFailure).toHaveBeenCalledWith(
      "These messages already reached the agent",
    );
  });

  it("refreshes the session when the runtime rejects an edit", async () => {
    const refreshSession = vi.fn().mockResolvedValue(null);
    const reportFailure = vi.fn();
    const actions = createChatActions({
      api: {
        editPendingQueue: vi
          .fn()
          .mockRejectedValue(new Error("The message queue changed.")),
      } as unknown as DesktopApiAdapter,
      stores: shellStores({
        deployments: [deployment],
        selection: { nodeDid: deployment.nodeDid, sessionId: "s1" },
      }),
      project: () => ({}) as ShellProjection,
      refreshSession,
      refreshSnapshot: vi.fn(),
      reportFailure,
    });
    await expect(
      actions.editPendingQueue({
        nodeDid: deployment.nodeDid,
        sessionId: "s1",
        expectedRequestDocIds: ["doc-a"],
        selectedRequestDocIds: ["doc-a"],
        messages: [{ requestDocId: "doc-a", content: "corrected" }],
      }),
    ).rejects.toThrow();
    expect(reportFailure).toHaveBeenCalledOnce();
    expect(refreshSession).toHaveBeenCalledWith("s1");
  });

  it("names a failed steer as one, and says when the messages already reached the agent", async () => {
    const stores = shellStores({
      deployments: [deployment],
      selection: { nodeDid: deployment.nodeDid, sessionId: "s1" },
    });
    writeSession(stores.session, {
      sessionId: "s1",
      nodeDid: deployment.nodeDid,
      pendingQueue: [entry("a"), entry("b")],
      foldedInputs: [],
      latestRequestId: "earlier",
    } as unknown as DesktopSessionSnapshot);
    const reached = {
      sessionId: "s1",
      nodeDid: deployment.nodeDid,
      pendingQueue: [],
      foldedInputs: [{ requestId: "a" }, { requestId: "b" }],
      latestRequestId: "earlier",
    } as unknown as DesktopSessionSnapshot;
    const refreshSession = vi
      .fn()
      .mockResolvedValueOnce(reached)
      .mockResolvedValueOnce(null);
    const reportFailure = vi.fn();
    const actions = createChatActions({
      api: {
        steerPendingMessage: vi
          .fn()
          .mockRejectedValue(new Error("This message is no longer pending.")),
      } as unknown as DesktopApiAdapter,
      stores,
      project: () => ({}) as ShellProjection,
      refreshSession,
      refreshSnapshot: vi.fn(),
      reportFailure,
    });
    const request = {
      nodeDid: deployment.nodeDid,
      sessionId: "s1",
      expectedRequestDocIds: ["doc-a", "doc-b"],
      requestDocId: "doc-b",
    };
    await expect(actions.steerPendingMessage(request)).rejects.toThrow();
    expect(reportFailure).toHaveBeenLastCalledWith(
      "These messages already reached the agent",
    );
    await expect(actions.steerPendingMessage(request)).rejects.toThrow();
    expect(reportFailure.mock.lastCall?.[0]).toMatch(/steer the message/);
  });
});
