import {
  act,
  fireEvent,
  renderHook,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { MailboxItemView, SessionSummary } from "@source-inc/gents-desktop-client";
import {
  openSearch,
  SearchButton,
  SearchPalette,
} from "../src/ui/screens/SearchPalette";
import { node, renderIn, renderRouted, testApp } from "./app-fixture";
import { useExclusivePopover } from "../src/ui/hooks/useExclusivePopover";

const NODE = "did:key:z6MkAgent";

const session = (
  sessionId: string,
  over: Partial<SessionSummary> = {},
): SessionSummary => ({
  sessionId,
  nodeDid: NODE,
  requesterDid: null,
  startedBy: null,
  latestRequestDocId: null,
  latestRequestId: null,
  closedAt: null,
  archivedAt: null,
  archiveRevision: "",
  tags: [],
  provenance: null,
  title: sessionId,
  previewText: null,
  status: null,
  agentId: "default",
  taskId: null,
  taskName: null,
  triggerId: null,
  triggerKind: null,
  createdAt: null,
  updatedAt: null,
  turnState: "completed",
  messageCount: null,
  toolCallCount: null,
  ...over,
});

const question = {
  itemId: "q1",
  itemKey: "q1",
  requesterDid: "did:key:me",
  nodeDid: NODE,
  status: "open",
  kind: "ask",
  action: "answer",
  title: "Which branch should I use?",
  summary: null,
  payload: null,
  sourceKind: "request",
  sourceId: "r1",
  sessionId: "release",
  requestId: null,
  causeDocId: null,
  targetNodeDid: NODE,
  targetAgentId: "default",
  expectedCollection: null,
  parentItemId: null,
  deadlineAt: null,
  createdAt: "2026-10-26T10:00:00Z",
} satisfies MailboxItemView;

function open() {
  const app = testApp({
    deployments: [
      node({
        sessions: [
          session("release", {
            title: "Release check",
            updatedAt: "2026-10-26T09:00:00Z",
          }),
          session("notes", {
            title: "Unreleased notes",
            updatedAt: "2026-10-26T11:00:00Z",
          }),
          session("worker", {
            title: "Worker run",
            updatedAt: "2026-10-26T12:00:00Z",
            startedBy: {
              sessionId: "release",
              nodeDid: NODE,
              requesterDid: null,
              causeRequestDocId: "d",
            },
          }),
        ],
        mailboxItems: [question],
      }),
    ],
    selection: { nodeDid: NODE },
  });
  const view = renderRouted(app, { name: "sessions" }, () => <SearchPalette />);
  act(() => openSearch());
  return view;
}

const groupNames = () =>
  screen.getAllByRole("group").map((g) => g.getAttribute("aria-label"));
const options = (group: string) =>
  within(screen.getByRole("group", { name: group }))
    .getAllByRole("option")
    .map((o) => o.textContent);

describe("the search palette", () => {
  it("waits for the current overlay to acknowledge its exit before opening", async () => {
    const previous = renderHook(() => useExclusivePopover());
    act(() => previous.result.current.onOpenChange(true));
    previous.result.current.popupRef.current = document.createElement("div");
    renderIn(testApp({ deployments: [node()] }), <SearchPalette />);
    act(() => openSearch());
    expect(previous.result.current.open).toBe(false);
    expect(screen.queryByRole("combobox", { name: "Search" })).toBeNull();
    act(() => previous.result.current.onOpenChangeComplete(false));
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "Search" })).toHaveFocus(),
    );
  });

  it("clears the external open request when another popup replaces Search", async () => {
    const next = renderHook(() => useExclusivePopover());
    renderIn(testApp({ deployments: [node()] }), <SearchPalette />);
    act(() => openSearch());
    expect(screen.getByRole("combobox", { name: "Search" })).toBeVisible();
    act(() => next.result.current.onOpenChange(true));
    await waitFor(() => expect(next.result.current.open).toBe(true));
    expect(screen.queryByRole("combobox", { name: "Search" })).toBeNull();
  });

  it("opens on what needs attention, recent sessions and actions", () => {
    open();
    expect(groupNames()).toEqual(["Needs attention", "Recent sessions", "Actions"]);
    expect(options("Needs attention")[0]).toContain("Which branch should I use?");
    expect(options("Recent sessions").map((t) => t!.split("Default")[0])).toEqual([
      "Unreleased notes",
      "Release check",
    ]);
  });

  it("ranks matches, narrows by kind and opens the chosen session", async () => {
    const view = open();
    const box = screen.getByRole("combobox", { name: "Search" });
    fireEvent.change(box, { target: { value: "rel" } });
    expect((await screen.findAllByRole("option"))[0]).toHaveTextContent(
      "Release check",
    );
    expect(options("Sessions").map((t) => t!.split("Default")[0])).toEqual([
      "Release check",
      "Unreleased notes",
    ]);
    fireEvent.keyDown(box, { key: "Tab" });
    expect(screen.getByRole("radio", { name: "Sessions" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    fireEvent.keyDown(box, { key: "Enter" });
    expect(view.nav().route).toEqual({
      name: "session",
      sessionId: "release",
      nodeDid: NODE,
    });
    expect(screen.queryByRole("combobox", { name: "Search" })).toBeNull();
  });

  it("opens from the title bar's Search button", async () => {
    renderIn(
      testApp({ deployments: [node()] }),
      <>
        <SearchButton />
        <SearchPalette />
      </>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Search" }));
    await waitFor(() =>
      expect(screen.getByRole("combobox", { name: "Search" })).toHaveFocus(),
    );
  });
});
