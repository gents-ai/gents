import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { MAX_LEASE_SECONDS } from "../src/hooks/sharing";
import {
  approvedHere,
  resetApprovedHere,
  restoreApprovedHere,
  type ApprovedHere,
} from "../src/ui/app/approvedHere";
import { resetJoins } from "../src/ui/lib/joinInvite";
import { resetPreferences } from "../src/ui/preferences";
import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { missingText, resetSharingTab } from "../src/ui/screens/sharing/SharingTabs";
import { node, renderIn, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import {
  HARNESS_GAME_PACK,
  createHarnessSharing,
  seedHarnessApprovals,
  type SharingScenario,
} from "./ui-harness/sharingHarness";

const DAY = 24 * 60 * 60;
const BRUNO = "did:key:z6MkHarnessBruno";
const GAME = { kind: "pack", pack: HARNESS_GAME_PACK, name: "Tic-tac-toe" };
const OWN = deployment.nodeDid;

function show(scenario: SharingScenario = "sharing") {
  withFlags({ sharing: true });
  seedHarnessApprovals(scenario, OWN);
  const sharing = createHarnessSharing(scenario);
  const spied = {
    renew: vi.spyOn(sharing, "renew"),
    revoke: vi.spyOn(sharing, "revoke"),
    revokeApproval: sharing.revokeApproval
      ? vi.spyOn(sharing, "revokeApproval")
      : undefined,
  };
  const app = testApp({
    sharing,
    api: { listInstalledPacks: async () => ({ packs: [] }) },
    snapshot: { bootstrap, client: { deployments: [node()] } },
  });
  renderIn(app, <AgentScreen nodeDid={OWN} section="sharing" />);
  return { sharing, ...spied };
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

/* the whole node for someone known by DID alone, as today's runtime asks */
const entry = (overrides: Partial<ApprovedHere> = {}): ApprovedHere => ({
  requestId: "request-1",
  party: { did: "did:key:z6MkCandidate", name: null, device: false },
  scope: { kind: "node" },
  profile: { id: "client", name: "client", grants: [] },
  leaseExpiresAt: new Date(Date.now() + 30 * DAY * 1000).toISOString(),
  approvedAt: new Date().toISOString(),
  ...overrides,
});

afterEach(() => {
  cleanup();
  resetJoins();
  resetApprovedHere();
  resetSharingTab();
  resetPreferences();
});

describe("who has access", () => {
  it("lists each grant compactly: who, what, profile and until when", async () => {
    const user = userEvent.setup();
    show();
    const list = await members();
    const rows = within(list).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("aria-label"))).toEqual([
      "David's phone: Whole node",
      "Bruno: Tic-tac-toe pack",
      "Bruno: Coach agent",
    ]);
    const [phone, game, coach] = rows as [HTMLElement, HTMLElement, HTMLElement];
    expect(phone.textContent).toContain("Doesn't lapse");
    expect(game.textContent).toContain("Player");
    expect(within(game).getByText("Ends in 2d").className).toContain("text-warning");
    expect(within(coach).getByText("Ends in 78d").className).not.toContain(
      "text-warning",
    );
    await user.click(within(phone).getByRole("button", { name: /^More for/ }));
    expect(await screen.findByRole("menuitem", { name: "Revoke…" })).toBeTruthy();
    expect(screen.queryByRole("menuitem", { name: "Renew…" })).toBeNull();
  });

  it("renews one grant for the lease chosen, up to the longest allowed", async () => {
    const user = userEvent.setup();
    const { renew } = show();
    const game = within(await members()).getByRole("listitem", {
      name: "Bruno: Tic-tac-toe pack",
    });
    await menu(user, game, "Renew…");
    const lease = within(game).getByRole("combobox", {
      name: "Renew Bruno's access for",
    });
    expect(lease.textContent).toContain("30 days");
    await user.click(lease);
    const options = (await screen.findAllByRole("option")).map((o) => o.textContent);
    expect(options.at(-1)).toBe(`${MAX_LEASE_SECONDS / DAY} days`);
    await user.click(screen.getByRole("option", { name: "90 days" }));
    await user.click(within(game).getByRole("button", { name: "Renew" }));
    expect(renew).toHaveBeenCalledWith(expect.any(String), BRUNO, GAME, 90 * DAY);
    await waitFor(() =>
      expect(
        within(
          screen.getByRole("listitem", { name: "Bruno: Tic-tac-toe pack" }),
        ).getByText("Ends in 90d"),
      ).toBeTruthy(),
    );
  });

  it("asks on the page before revoking, and keeps access when told to", async () => {
    const user = userEvent.setup();
    const confirm = vi.spyOn(window, "confirm");
    const { revoke } = show();
    const game = within(await members()).getByRole("listitem", {
      name: "Bruno: Tic-tac-toe pack",
    });
    await menu(user, game, "Revoke…");
    const ask = within(game).getByRole("group", { name: "Revoke Bruno's access" });
    expect(ask.textContent).toContain(
      "Revoke Bruno's access to Tic-tac-toe pack? It ends right away.",
    );
    await user.click(within(ask).getByRole("button", { name: "Keep" }));
    expect(revoke).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
    await menu(user, game, "Revoke…");
    await user.click(within(game).getByRole("button", { name: "Revoke" }));
    expect(revoke).toHaveBeenCalledWith(expect.any(String), BRUNO, GAME);
    await waitFor(() =>
      expect(
        screen.queryByRole("listitem", { name: "Bruno: Tic-tac-toe pack" }),
      ).toBeNull(),
    );
    expect(screen.getByRole("listitem", { name: "Bruno: Coach agent" })).toBeTruthy();
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
    const coach = within(await members()).getByRole("listitem", {
      name: "Bruno: Coach agent",
    });
    await menu(user, coach, "Revoke…");
    await user.click(within(coach).getByRole("button", { name: "Revoke" }));
    expect((await within(coach).findByRole("alert")).textContent).toContain(
      "the node is offline",
    );
  });

  it("says so in its tab when nobody else has access", async () => {
    const { sharing } = show("sharing-empty");
    await screen.findByTestId("sharing-empty");
    await act(() =>
      sharing.createInvite(OWN, {
        kind: "device",
        scope: { kind: "node" },
        profileId: "device",
        expiresInSeconds: DAY,
        uses: 1,
        preApproved: false,
      }),
    );
    expect(
      await screen.findByText("Nobody else has access to Local Agent."),
    ).toBeTruthy();
    expect(
      screen.getByRole("tab", { name: /^Access/ }).getAttribute("aria-selected"),
    ).toBe("true");
  });
});

describe("approvals made on this desktop", () => {
  it("shows them under Access, marked, while the runtime can't list members", async () => {
    show("sharing-approved-here");
    const list = await members();
    const rows = within(list).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("aria-label"))).toEqual([
      "did:key:z6MkHa…Tablet: Whole node",
      "did:key:z6MkHa…dioMac: Whole node",
    ]);
    const [tablet, studio] = rows as [HTMLElement, HTMLElement];
    expect(within(tablet).getByText("Approved here")).toBeTruthy();
    expect(tablet.textContent).toContain("Ends in 29d");
    expect(within(studio).getByText("Ends in 3d").className).toContain("text-warning");
    expect(screen.getByRole("tab", { name: /^Access/ }).textContent).toContain("2");
    expect(screen.queryByRole("tab", { name: /^Invites/ })).toBeNull();
    expect(
      screen.getByText(
        "Access, invites and history need a newer runtime on Local Agent.",
      ),
    ).toBeTruthy();
  });

  it("explains the mark", async () => {
    const user = userEvent.setup();
    show("sharing-approved-here");
    const list = await members();
    await user.hover(within(list).getAllByText("Approved here")[0]!);
    expect(
      await screen.findByText(
        "This runtime can't list who has access yet; this device remembers the ones you approved.",
      ),
    ).toBeTruthy();
  });

  it("revokes one by its request, after asking", async () => {
    const user = userEvent.setup();
    const { revokeApproval } = show("sharing-approved-here");
    const tablet = within(await members()).getByRole("listitem", {
      name: "did:key:z6MkHa…Tablet: Whole node",
    });
    await menu(user, tablet, "Revoke…");
    await user.click(within(tablet).getByRole("button", { name: "Revoke" }));
    expect(revokeApproval).toHaveBeenCalledWith(OWN, "request-harness-tablet");
    await waitFor(() =>
      expect(screen.queryByRole("listitem", { name: /Tablet/ })).toBeNull(),
    );
    expect(screen.getByRole("listitem", { name: /dioMac/ })).toBeTruthy();
  });

  it("remembers an approval where the runtime lists no members, with no way to revoke it", async () => {
    const user = userEvent.setup();
    show("sharing-requests-only");
    const strip = await screen.findByRole("region", { name: "Requests to join" });
    const jack = within(strip).getByRole("listitem", { name: "Request from Jack" });
    await user.click(within(jack).getByRole("button", { name: "Approve" }));
    const row = within(await members()).getByRole("listitem", {
      name: "Jack: Tic-tac-toe pack",
    });
    expect(row.textContent).toContain("Approved here");
    expect(row.textContent).toContain("Ends in 30d");
    expect(within(row).queryByRole("button", { name: /^More for/ })).toBeNull();
  });

  it("drops one once the runtime lists that member", async () => {
    const user = userEvent.setup();
    show();
    const strip = await screen.findByRole("region", { name: "Requests to join" });
    const jack = within(strip).getByRole("listitem", { name: "Request from Jack" });
    await user.click(within(jack).getByRole("button", { name: "Approve" }));
    const row = await within(await members()).findByRole("listitem", {
      name: "Jack: Tic-tac-toe pack",
    });
    expect(row.textContent).not.toContain("Approved here");
    await waitFor(() =>
      expect(localStorage.getItem("gents-sharing-approved-here")).not.toContain("Jack"),
    );
  });

  it("drops one listed by the runtime, or whose lease has passed", () => {
    const now = Date.now();
    approvedHere.add(OWN, entry({ requestId: "listed" }));
    approvedHere.add(
      OWN,
      entry({
        requestId: "lapsed",
        party: { did: "did:key:z6MkOld", name: null, device: false },
        leaseExpiresAt: new Date(now - 1000).toISOString(),
      }),
    );
    approvedHere.add(
      OWN,
      entry({
        requestId: "kept",
        party: { did: "did:key:z6MkKept", name: null, device: false },
      }),
    );
    approvedHere.prune(OWN, [{ party: entry().party, scope: { kind: "node" } }], now);
    const kept = restoreApprovedHere(
      JSON.parse(localStorage.getItem("gents-sharing-approved-here")!).state,
    );
    expect(kept.byNode[OWN]?.map((e) => e.requestId)).toEqual(["kept"]);
  });

  it("reads back only well-formed entries", () => {
    expect(
      restoreApprovedHere({
        byNode: { [OWN]: [entry(), { requestId: 3 }, null], other: "nope" },
      }),
    ).toEqual({ byNode: { [OWN]: [entry()] } });
    expect(restoreApprovedHere(undefined)).toEqual({ byNode: {} });
  });
});

describe("the sharing history", () => {
  it("lists what was decided, for what, by whom, newest first", async () => {
    const user = userEvent.setup();
    show();
    await user.click(await screen.findByRole("tab", { name: /^History/ }));
    const history = await screen.findByRole("list", { name: "Sharing history" });
    const rows = within(history).getAllByRole("listitem");
    expect(rows.map((r) => r.getAttribute("aria-label"))).toEqual([
      "Expired: Jack",
      "Revoked: Chris",
      "Approved: Bruno",
      "Approved: Bruno",
      "Approved: David's phone",
    ]);
    expect(rows[0]!.textContent).not.toContain("By");
    expect(rows[1]!.textContent).toContain("By David");
    expect(rows[2]!.textContent).toContain("Coach agent");
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
    ).toEqual(["Access3", "Invites2", "History"]);
    expect(screen.getByRole("list", { name: "Who has access" })).toBeTruthy();
    await user.click(within(tabs).getByRole("tab", { name: /^Invites/ }));
    expect(await screen.findByRole("list", { name: "Open invites" })).toBeTruthy();
    await waitFor(() =>
      expect(screen.queryByRole("list", { name: "Who has access" })).toBeNull(),
    );
    cleanup();
    show();
    expect(await screen.findByRole("list", { name: "Open invites" })).toBeTruthy();
  });
});

describe("nothing shared yet", () => {
  it("shows one empty state with the ways to start, in place of the tabs", async () => {
    const user = userEvent.setup();
    show("sharing-empty");
    const empty = await screen.findByTestId("sharing-empty");
    expect(empty.textContent).toContain("Nothing is shared yet.");
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.queryByRole("region", { name: "Requests to join" })).toBeNull();
    await user.click(within(empty).getByRole("button", { name: "Invite someone" }));
    expect(await screen.findByTestId("invite-dialog")).toBeTruthy();
  });

  it("opens joining from the empty state", async () => {
    const user = userEvent.setup();
    show("sharing-empty");
    const empty = await screen.findByTestId("sharing-empty");
    await user.click(
      within(empty).getByRole("button", { name: "Join with an invite" }),
    );
    const steps = await screen.findByTestId("join-dialog");
    expect(
      within(steps).getByRole("textbox", { name: "Invite link or code" }),
    ).toBeTruthy();
  });
});

describe("what the runtime offers", () => {
  it("names what it doesn't offer in one line, and leaves those views out", async () => {
    show("sharing-requests-only");
    expect(
      await screen.findByRole("region", { name: "Requests to join" }),
    ).toBeTruthy();
    expect(
      screen.getByText(
        "Access, invites and history need a newer runtime on Local Agent.",
      ),
    ).toBeTruthy();
    expect(screen.queryByRole("tablist")).toBeNull();
    expect(screen.queryByText(/Nobody else has access/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Invite…" })).toBeNull();
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
    ).toEqual(["Access3", "Invites2", "History"]);
  });

  it("words what's missing as one sentence", () => {
    const all = { requests: true, members: true, invites: true, history: true };
    expect(missingText(all, "Forge")).toBeNull();
    expect(missingText({ ...all, invites: false, history: false }, "Forge")).toBe(
      "Invites and history need a newer runtime on Forge.",
    );
    expect(missingText({ ...all, history: false }, "Forge")).toBe(
      "History needs a newer runtime on Forge.",
    );
  });

  it("still offers joining from an invite where the node can't share", async () => {
    show("sharing-unavailable");
    expect(
      await screen.findByText(/Sharing isn't available for Local Agent/),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Join with an invite…" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Invite…" })).toBeNull();
  });
});
