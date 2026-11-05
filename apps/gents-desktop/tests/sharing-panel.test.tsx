import { cleanup, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  BridgeInvokeError,
  type JoinRequestDecisionView,
  type JoinRequestView,
} from "@source-inc/gents-desktop-client";

import { resetApprovedHere } from "../src/ui/app/approvedHere";
import { resetJoins } from "../src/ui/lib/joinInvite";
import { parseRoute, pathFor } from "../src/ui/lib/router";
import { resetPreferences } from "../src/ui/preferences";
import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { resetSharingTab } from "../src/ui/screens/sharing/SharingTabs";
import { node, renderIn, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import { createHarnessSharing } from "./ui-harness/sharingHarness";

const OWN = deployment.nodeDid;
const DAY = 24 * 60 * 60;
const LAPTOP = "did:key:z6MkCandidateLaptopXYZ123";

const pending = (overrides: Partial<JoinRequestView> = {}): JoinRequestView => ({
  requestId: "request-laptop",
  networkId: "network-1",
  candidateDid: LAPTOP,
  candidatePeer: "peer-1",
  ownerAgent: OWN,
  expiresAt: "2026-11-05T12:00:00Z",
  profile: "client",
  issuedAt: "2026-11-05T11:55:00Z",
  ...overrides,
});

function show({
  requests = [pending()],
  initNodeDid = OWN,
  decide = vi.fn(async () => ({
    requestId: "request-laptop",
    state: "approved",
    deliveryPending: false,
  })),
}: {
  requests?: JoinRequestView[];
  initNodeDid?: string;
  decide?: (request: unknown) => Promise<JoinRequestDecisionView>;
} = {}) {
  withFlags({ sharing: true });
  let listed = requests;
  const api = {
    listJoinRequests: vi.fn(async () => listed),
    decideJoinRequest: vi.fn(async (request: unknown) => {
      const decided = await decide(request);
      listed = [];
      return decided;
    }),
  };
  const app = testApp({
    api,
    snapshot: {
      bootstrap: { ...bootstrap, initNodeDid },
      client: { deployments: [node()] },
    },
  });
  renderIn(app, <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />);
  return api;
}

afterEach(() => {
  cleanup();
  resetJoins();
  resetApprovedHere();
  resetSharingTab();
  resetPreferences();
  localStorage.clear();
});

describe("the Sharing page", () => {
  it("heads the section with its name, what it is for and the ways in", async () => {
    show();
    const header = (await screen.findByRole("heading", { name: "Sharing" }))
      .parentElement!.parentElement!;
    expect(header.textContent).toContain(
      "Who can use Local Agent, and how they got in.",
    );
    expect(
      within(header).getByRole("button", { name: "Join with an invite…" }),
    ).toBeTruthy();
    expect(within(header).queryByRole("button", { name: "Invite…" })).toBeNull();
  });

  it("lists who is waiting, by shortened DID, with the profile and the times", async () => {
    show();
    const strip = await screen.findByRole("region", { name: "Requests to join" });
    expect(strip.textContent).toContain("1 request to join");
    const row = within(strip).getByRole("listitem", {
      name: /^Request from did:key:z6MkC/,
    });
    const who = within(row).getByTitle(LAPTOP, { exact: true });
    expect(who.textContent).toBe("did:key:z6MkCa…XYZ123");
    expect(within(row).getByRole("button", { name: `Copy ${LAPTOP}` })).toBeTruthy();
    expect(row.textContent).toContain("client");
    expect(row.textContent).toContain("Asked");
    expect(row.textContent).toContain("Expires");
    expect(within(row).getByRole("combobox", { name: /lasts$/ }).textContent).toContain(
      "30 days",
    );
    expect(
      screen.getByText(
        "Access, invites and history need a newer runtime on Local Agent.",
      ),
    ).toBeTruthy();
    expect(screen.queryByRole("tablist")).toBeNull();
  });

  it("folds the requests under their count, and opens them again", async () => {
    const user = userEvent.setup();
    show({ requests: [pending(), pending({ requestId: "request-2" })] });
    const strip = await screen.findByRole("region", { name: "Requests to join" });
    const fold = within(strip).getByRole("button", { name: "2 requests to join" });
    expect(fold.getAttribute("aria-expanded")).toBe("true");
    expect(within(strip).getAllByRole("listitem")).toHaveLength(2);
    await user.click(fold);
    expect(within(strip).queryAllByRole("listitem")).toHaveLength(0);
    await user.click(fold);
    expect(within(strip).getAllByRole("listitem")).toHaveLength(2);
  });

  it("leaves out when it was asked when the runtime doesn't say", async () => {
    show({ requests: [pending({ issuedAt: null, profile: null })] });
    const row = await screen.findByRole("listitem", { name: /^Request from/ });
    expect(row.textContent).not.toContain("Asked");
    expect(row.textContent).not.toContain("client");
  });

  it("approves for the lease chosen, then remembers the approval under Access", async () => {
    const user = userEvent.setup();
    const api = show();
    const row = await screen.findByRole("listitem", { name: /^Request from/ });
    await user.click(within(row).getByRole("combobox", { name: /lasts$/ }));
    await user.click(await screen.findByRole("option", { name: "90 days" }));
    await user.click(within(row).getByRole("button", { name: "Approve" }));
    expect(api.decideJoinRequest).toHaveBeenCalledWith({
      nodeDid: OWN,
      requestId: "request-laptop",
      decision: { kind: "approve", leaseSeconds: 90 * DAY },
    });
    const access = await screen.findByRole("list", { name: "Who has access" });
    expect(api.listJoinRequests).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("region", { name: "Requests to join" })).toBeNull();
    const approved = within(access).getByRole("listitem", {
      name: /^did:key:z6MkCa…XYZ123: Whole node/,
    });
    expect(approved.textContent).toContain("Approved here");
    expect(approved.textContent).toContain("Ends in 90d");
    /* the bridge decides only approve and deny, so nothing revokes it here */
    expect(within(approved).queryByRole("button", { name: /^More for/ })).toBeNull();
    expect(screen.getByRole("tab", { name: /^Access/ })).toBeTruthy();
    expect(screen.queryByRole("tab", { name: /^Invites/ })).toBeNull();
    expect(localStorage.getItem("gents-sharing-approved-here")).toContain(LAPTOP);
  });

  it("approves for 30 days unless another lease is chosen", async () => {
    const user = userEvent.setup();
    const api = show();
    const row = await screen.findByRole("listitem", { name: /^Request from/ });
    await user.click(within(row).getByRole("button", { name: "Approve" }));
    expect(api.decideJoinRequest).toHaveBeenCalledWith(
      expect.objectContaining({
        decision: { kind: "approve", leaseSeconds: 30 * DAY },
      }),
    );
  });

  it("denies a request", async () => {
    const user = userEvent.setup();
    const api = show();
    const row = await screen.findByRole("listitem", { name: /^Request from/ });
    await user.click(within(row).getByRole("button", { name: "Deny" }));
    expect(api.decideJoinRequest).toHaveBeenCalledWith({
      nodeDid: OWN,
      requestId: "request-laptop",
      decision: { kind: "deny" },
    });
    expect(
      await screen.findByText("Nobody is waiting to join Local Agent."),
    ).toBeTruthy();
    expect(screen.queryByText("Approved here")).toBeNull();
  });

  it("shows a refused decision in the request's row", async () => {
    const user = userEvent.setup();
    show({
      decide: vi.fn(async () => {
        throw new BridgeInvokeError({
          code: "unknown",
          message: "runtime rejected enrollment decision (409 Conflict): conflicted",
          retryable: false,
          endpoint: null,
        });
      }),
    });
    const row = await screen.findByRole("listitem", { name: /^Request from/ });
    await user.click(within(row).getByRole("button", { name: "Deny" }));
    expect((await screen.findByRole("alert")).textContent).toContain("conflicted");
  });

  it("says so in one place when nobody is waiting, with the way to join", async () => {
    show({ requests: [] });
    const empty = await screen.findByTestId("sharing-empty");
    expect(empty.textContent).toContain("Nobody is waiting to join Local Agent.");
    expect(
      within(empty).getByRole("button", { name: "Join with an invite" }),
    ).toBeTruthy();
    expect(within(empty).queryByRole("button", { name: "Invite someone" })).toBeNull();
    expect(screen.queryByRole("region", { name: "Requests to join" })).toBeNull();
    expect(screen.queryByRole("tablist")).toBeNull();
  });

  it("says sharing isn't available for a node this desktop doesn't run", async () => {
    const api = show({ initNodeDid: "did:key:z6MkSomeoneElse" });
    expect(
      await screen.findByText(/Sharing isn't available for Local Agent/),
    ).toBeTruthy();
    expect(api.listJoinRequests).not.toHaveBeenCalled();
  });

  it("shows a failed read inline", async () => {
    withFlags({ sharing: true });
    const app = testApp({
      api: {
        listJoinRequests: vi.fn(async () => {
          throw new Error("runtime is restarting");
        }),
        decideJoinRequest: vi.fn(),
      },
      snapshot: { bootstrap, client: { deployments: [node()] } },
    });
    renderIn(app, <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />);
    expect((await screen.findByRole("alert")).textContent).toContain(
      "runtime is restarting",
    );
  });

  it("names the harness's requests by their names", async () => {
    withFlags({ sharing: true });
    const app = testApp({
      sharing: createHarnessSharing("sharing"),
      snapshot: { bootstrap, client: { deployments: [node()] } },
    });
    renderIn(app, <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />);
    expect(
      await screen.findByRole("listitem", { name: "Request from Jack" }),
    ).toBeTruthy();
    expect(screen.queryByText(/isn't available|newer runtime/)).toBeNull();
  });

  it("is a section of the node's configuration only while the feature is on", async () => {
    withFlags({ sharing: false });
    const app = testApp({ snapshot: { bootstrap, client: { deployments: [node()] } } });
    renderIn(app, <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />);
    expect(screen.queryByTestId("sharing-panel")).toBeNull();
    expect(screen.queryByRole("link", { name: /^Sharing/ })).toBeNull();
    cleanup();
    show({ requests: [] });
    expect(await screen.findByTestId("sharing-panel")).toBeTruthy();
    const link = screen.getByRole("link", { name: /^Sharing/ });
    expect(link.getAttribute("href")).toBe(
      pathFor({ name: "agent", nodeDid: OWN, section: "sharing" }),
    );
  });

  it("isn't a page of its own", () => {
    expect(parseRoute("/sharing")).not.toEqual({ name: "sharing" });
  });
});
