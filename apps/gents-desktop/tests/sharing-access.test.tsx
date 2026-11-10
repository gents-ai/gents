import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { resetPreferences } from "../src/ui/preferences";
import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { historyNote } from "../src/ui/screens/sharing/History";
import { missingText, resetSharingTab } from "../src/ui/screens/sharing/SharingTabs";
import { node, renderIn, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import {
  HARNESS_BRUNO,
  createHarnessSharing,
  type SharingScenario,
} from "./ui-harness/sharingHarness";

const OWN = deployment.nodeDid;

function show(scenario: SharingScenario = "sharing") {
  withFlags({ sharing: true });
  const sharing = createHarnessSharing(scenario);
  const revoke = vi.spyOn(sharing, "revoke");
  const app = testApp({
    sharing,
    snapshot: { bootstrap, client: { deployments: [node()] } },
  });
  renderIn(app, <AgentScreen nodeDid={OWN} section="sharing" />);
  return { sharing, revoke };
}

const members = () => screen.findByRole("list", { name: "Who has access" });

async function menu(
  user: ReturnType<typeof userEvent.setup>,
  row: HTMLElement,
  item: string,
) {
  await user.click(within(row).getByRole("button", { name: /^More for/ }));
  await user.click(await screen.findByRole("menuitem", { name: item }));
  /* the closing menu ignores the pointer until it is gone */
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
}

afterEach(() => {
  cleanup();
  resetSharingTab();
  resetPreferences();
});

describe("who has access", () => {
  it("lists each grant compactly: who, what, profile and until when", async () => {
    const user = userEvent.setup();
    show();
    const rows = within(await members()).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("aria-label"))).toEqual([
      "David's phone: Whole node",
      "Bruno: Whole node",
    ]);
    const [phone, bruno] = rows as [HTMLElement, HTMLElement];
    expect(phone.textContent).toContain("client");
    expect(within(phone).getByText("Ends in 78d").className).not.toContain(
      "text-warning",
    );
    expect(within(bruno).getByText("Ends in 2d").className).toContain("text-warning");
    await user.click(within(phone).getByRole("button", { name: /^More for/ }));
    expect(await screen.findByRole("menuitem", { name: "Revoke…" })).toBeTruthy();
    expect(screen.queryByRole("menuitem", { name: "Renew…" })).toBeNull();
  });

  it("asks on the page before revoking, and keeps access when told to", async () => {
    const user = userEvent.setup();
    const confirm = vi.spyOn(window, "confirm");
    const { revoke } = show();
    const bruno = within(await members()).getByRole("listitem", {
      name: "Bruno: Whole node",
    });
    await menu(user, bruno, "Revoke…");
    const ask = within(bruno).getByRole("group", { name: "Revoke Bruno's access" });
    expect(ask.textContent).toContain(
      "Revoke Bruno's access to the whole node? It ends right away.",
    );
    await user.click(within(ask).getByRole("button", { name: "Keep" }));
    expect(revoke).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
    await menu(user, bruno, "Revoke…");
    await user.click(within(bruno).getByRole("button", { name: "Revoke" }));
    expect(revoke).toHaveBeenCalledWith(expect.any(String), HARNESS_BRUNO);
    await waitFor(() =>
      expect(screen.queryByRole("listitem", { name: "Bruno: Whole node" })).toBeNull(),
    );
    expect(
      screen.getByRole("listitem", { name: "David's phone: Whole node" }),
    ).toBeTruthy();
    await user.click(screen.getByRole("tab", { name: /^History/ }));
    const history = await screen.findByRole("list", { name: "Sharing history" });
    expect(
      within(history).getAllByRole("listitem")[0]!.getAttribute("aria-label"),
    ).toBe("Revoked: Bruno");
  });

  it("shows a refused revoke in the grant's row", async () => {
    const user = userEvent.setup();
    const { revoke } = show();
    revoke.mockRejectedValueOnce(new Error("the node is offline"));
    const bruno = within(await members()).getByRole("listitem", {
      name: "Bruno: Whole node",
    });
    await menu(user, bruno, "Revoke…");
    await user.click(within(bruno).getByRole("button", { name: "Revoke" }));
    expect((await within(bruno).findByRole("alert")).textContent).toContain(
      "the node is offline",
    );
  });
});

describe("the sharing history", () => {
  it("lists what was decided, by whom, newest first", async () => {
    const user = userEvent.setup();
    show();
    await user.click(await screen.findByRole("tab", { name: /^History/ }));
    const history = await screen.findByRole("list", { name: "Sharing history" });
    const rows = within(history).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("aria-label"))).toEqual([
      "Expired: Jack",
      "Revoked: Chris",
      "Approved: Bruno",
      "Approved: David's phone",
    ]);
    expect(rows[0]!.textContent).not.toContain("By");
    expect(rows[1]!.textContent).toContain("By David");
  });

  it("says nothing of limits when the runtime reports none", async () => {
    const user = userEvent.setup();
    show();
    await user.click(await screen.findByRole("tab", { name: /^History/ }));
    await screen.findByRole("list", { name: "Sharing history" });
    expect(screen.queryByText(/aren't shown|Showing the latest/)).toBeNull();
  });

  it("names what it leaves out in one line under the list", async () => {
    const user = userEvent.setup();
    show("sharing-history-limited");
    await user.click(await screen.findByRole("tab", { name: /^History/ }));
    expect(
      await screen.findByText(
        "Showing the latest 50 of 73. Decisions replaced by a later one, or in conflict, aren't shown.",
      ),
    ).toBeTruthy();
  });

  it("words what it leaves out for each thing the runtime says", () => {
    const limits = {
      observed: 3,
      limit: 50,
      omitsSuperseded: false,
      conflictsOmitted: false,
    };
    expect(historyNote(null)).toBeNull();
    expect(historyNote(limits)).toBeNull();
    expect(historyNote({ ...limits, observed: 50 })).toBeNull();
    expect(historyNote({ ...limits, observed: 51 })).toBe(
      "Showing the latest 50 of 51.",
    );
    expect(historyNote({ ...limits, omitsSuperseded: true })).toBe(
      "Decisions replaced by a later one aren't shown.",
    );
    expect(historyNote({ ...limits, conflictsOmitted: true })).toBe(
      "Decisions in conflict aren't shown.",
    );
    expect(historyNote({ ...limits, observed: 0, omitsSuperseded: true })).toBeNull();
    expect(historyNote({ ...limits, observed: 0, conflictsOmitted: true })).toBe(
      "Decisions in conflict aren't shown.",
    );
  });
});

describe("the tabs", () => {
  it("show one view at a time, and the one chosen stays while the app runs", async () => {
    const user = userEvent.setup();
    show();
    const tabs = await screen.findByRole("tablist");
    expect(
      within(tabs)
        .getAllByRole("tab")
        .map((t) => t.textContent),
    ).toEqual(["Access2", "History"]);
    expect(screen.getByRole("list", { name: "Who has access" })).toBeTruthy();
    await user.click(within(tabs).getByRole("tab", { name: /^History/ }));
    expect(await screen.findByRole("list", { name: "Sharing history" })).toBeTruthy();
    await waitFor(() =>
      expect(screen.queryByRole("list", { name: "Who has access" })).toBeNull(),
    );
    cleanup();
    show();
    expect(await screen.findByRole("list", { name: "Sharing history" })).toBeTruthy();
  });
});

describe("nothing shared yet", () => {
  it("shows one empty state in place of the tabs", async () => {
    show("sharing-empty");
    const empty = await screen.findByTestId("sharing-empty");
    expect(empty.textContent).toContain("Nothing is shared yet.");
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.queryByRole("region", { name: "Requests to join" })).toBeNull();
    expect(screen.queryByRole("button")).toBeNull();
  });
});

describe("what the runtime offers", () => {
  it("names what it doesn't offer in one line, and leaves those views out", async () => {
    show("sharing-requests-only");
    expect(
      await screen.findByRole("region", { name: "Requests to join" }),
    ).toBeTruthy();
    expect(
      screen.getByText("Access and history need a newer runtime on Local Agent."),
    ).toBeTruthy();
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.queryByText(/Nobody else has access/)).toBeNull();
  });

  it("says requests aren't offered while the rest is", async () => {
    show("sharing-no-requests");
    expect(
      await screen.findByText("Requests to join need a newer runtime on Local Agent."),
    ).toBeTruthy();
    expect(screen.queryByRole("region", { name: "Requests to join" })).toBeNull();
    expect(
      within(screen.getByRole("tablist"))
        .getAllByRole("tab")
        .map((t) => t.textContent),
    ).toEqual(["Access2", "History"]);
  });

  it("words what's missing as one sentence", () => {
    const all = { requests: true, members: true, history: true };
    expect(missingText(all, "Forge")).toBeNull();
    expect(missingText({ ...all, members: false, history: false }, "Forge")).toBe(
      "Access and history need a newer runtime on Forge.",
    );
    expect(missingText({ ...all, history: false }, "Forge")).toBe(
      "History needs a newer runtime on Forge.",
    );
  });

  it("says so where the node can't share", async () => {
    show("sharing-unavailable");
    expect(
      await screen.findByText(/Sharing isn't available for Local Agent/),
    ).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });
});
