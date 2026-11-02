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
import { MailboxStack } from "../src/ui/screens/MailboxStack";

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
  it("asks the system not to autocorrect the query, so ↓ reaches the results", () => {
    open();
    const field = screen.getByRole("combobox");
    expect(field).toHaveAttribute("autocorrect", "off");
    expect(field).toHaveAttribute("autocapitalize", "off");
    expect(field).toHaveAttribute("writingsuggestions", "false");
    expect(field).toHaveAttribute("spellcheck", "false");
  });

  it("opens an item on the active node in the mailbox's dialog, over the page", () => {
    const app = testApp({
      deployments: [
        node({
          sessions: [session("release", { title: "Release check" })],
          mailboxItems: [question],
        }),
      ],
      selection: { nodeDid: NODE },
    });
    renderRouted(app, { name: "sessions" }, (route) => (
      <>
        <SearchPalette />
        <MailboxStack route={route} />
      </>
    ));
    act(() => openSearch());
    fireEvent.click(
      within(screen.getByRole("group", { name: "Needs attention" })).getByRole(
        "option",
      ),
    );
    const dialog = screen.getByTestId("mailbox-dialog");
    expect(
      within(dialog).getByRole("heading", { name: "Which branch should I use?" }),
    ).toBeVisible();
    expect(screen.queryByRole("combobox", { name: "Search" })).toBeNull();
  });

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

  it("opens short, and lists a group's rest from its Show more row", () => {
    open();
    /* five actions: three, then a row for the other two */
    expect(options("Actions")).toEqual([
      "New session",
      "Go to sessions",
      "Go to mailbox",
      "Show 2 more",
    ]);
    const input = screen.getByRole("combobox", { name: "Search" });
    const rows = screen.getAllByRole("option");
    for (let i = 0; i < rows.length - 1; i++)
      fireEvent.keyDown(input, { key: "ArrowDown" });
    expect(screen.getByRole("option", { name: "Show 2 more" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    fireEvent.keyDown(input, { key: "Enter" });
    expect(options("Actions")).toEqual([
      "New session",
      "Go to sessions",
      "Go to mailbox",
      "Go to nodes",
      "Open settings",
    ]);
    /* the palette stays open, on the first row it revealed */
    expect(screen.getByRole("option", { name: "Go to nodes" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
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

  it("goes to the experimental features settings", async () => {
    const view = open();
    const box = screen.getByRole("combobox", { name: "Search" });
    fireEvent.change(box, { target: { value: "experimental" } });
    expect((await screen.findAllByRole("option"))[0]).toHaveTextContent(
      "Experimental features",
    );
    fireEvent.keyDown(box, { key: "Enter" });
    expect(view.nav().route).toEqual({ name: "settings", section: "experimental" });
  });

  it("finds a node by its name and switches to it", async () => {
    const app = testApp({
      deployments: [
        node(),
        node({
          nodeDid: "did:key:juno",
          label: "Juno",
          source: "enrollment",
          node: { ...node().node, nodeDid: "did:key:juno", displayName: null },
        }),
      ],
      selection: { nodeDid: NODE },
    });
    renderIn(app, <SearchPalette />);
    act(() => openSearch());
    fireEvent.change(screen.getByRole("combobox", { name: "Search" }), {
      target: { value: "juno" },
    });
    expect(groupNames()[0]).toBe("Nodes");
    const juno = options("Nodes")[0];
    expect(juno).toContain("Juno");
    expect(juno).toContain("Remote");
    fireEvent.keyDown(screen.getByRole("combobox", { name: "Search" }), {
      key: "Enter",
    });
    await waitFor(() =>
      expect(app.stores.selection.getState().nodeDid).toBe("did:key:juno"),
    );
  });

  it("names the pack an agent came from, and finds its agents by the pack", () => {
    const base = node();
    renderIn(
      testApp({
        deployments: [
          node({
            agents: [
              ...base.agents,
              {
                ...base.agents[0]!,
                agentId: "lister",
                displayName: "Lister",
                isDefault: false,
                tags: ["gents:pack:demo-research"],
              },
            ],
          }),
        ],
        selection: { nodeDid: NODE },
      }),
      <SearchPalette />,
    );
    act(() => openSearch());
    fireEvent.change(screen.getByRole("combobox", { name: "Search" }), {
      target: { value: "demo-research" },
    });
    const agents = options("Agents");
    expect(agents).toHaveLength(1);
    expect(agents[0]).toContain("Lister");
    expect(agents[0]).toContain("demo-research");
    /* the person's own agents name only their node */
    fireEvent.change(screen.getByRole("combobox", { name: "Search" }), {
      target: { value: "ops" },
    });
    expect(options("Agents")[0]).not.toContain("·");
    fireEvent.keyDown(screen.getByRole("combobox", { name: "Search" }), {
      key: "Escape",
    });
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
