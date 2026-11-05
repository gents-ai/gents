import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  BridgeInvokeError,
  type JoinHistoryView,
  type JoinMemberView,
  type JoinRequestDecideRequest,
  type JoinRequestDecisionView,
  type JoinRequestView,
} from "@source-inc/gents-desktop-client";

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

const PHONE = "did:key:z6MkMemberPhoneABC789";
const OPERATOR = "did:key:z6MkOperatorDEF456";

const lapsesIn = (seconds: number) =>
  new Date(Date.now() + seconds * 1000).toISOString();

const member = (overrides: Partial<JoinMemberView> = {}): JoinMemberView => ({
  requestId: "request-phone",
  networkId: "network-1",
  candidateDid: PHONE,
  candidatePeer: "peer-2",
  profile: "client",
  authorizationExpiresAt: lapsesIn(30 * DAY),
  ...overrides,
});

const recorded = (
  overrides: Partial<JoinHistoryView["history"][number]> = {},
): JoinHistoryView["history"][number] => ({
  requestId: "request-phone",
  candidateDid: PHONE,
  candidatePeer: "peer-2",
  profile: "client",
  state: "approved",
  decidedAt: "2026-11-01T12:00:00Z",
  decidedBy: OPERATOR,
  revokedAt: null,
  revokedBy: null,
  ...overrides,
});

const tooOld = () =>
  new BridgeInvokeError({
    code: "unsupported",
    message: "This node's runtime is too old to say who has access.",
    retryable: false,
    endpoint: null,
  });

/* a runtime that follows its decisions: an approved request becomes a member,
   a revoked member a record of it */
function show({
  requests = [pending()],
  members = [],
  records = [],
  limits = {},
  olderRuntime = false,
  clientDid,
  initNodeDid = OWN,
  decide = vi.fn(async () => ({
    requestId: "request-laptop",
    state: "approved",
    deliveryPending: false,
  })),
}: {
  requests?: JoinRequestView[];
  members?: JoinMemberView[];
  records?: JoinHistoryView["history"];
  limits?: Partial<JoinHistoryView>;
  olderRuntime?: boolean;
  /** this app's own DID, as the client snapshot reports it */
  clientDid?: string;
  initNodeDid?: string;
  decide?: (request: unknown) => Promise<JoinRequestDecisionView>;
} = {}) {
  withFlags({ sharing: true });
  let listed = requests;
  let active = members;
  let history = records;
  const api = {
    listJoinRequests: vi.fn(async () => listed),
    listJoinMembers: vi.fn(async () => {
      if (olderRuntime) throw tooOld();
      return active;
    }),
    readJoinHistory: vi.fn(async (): Promise<JoinHistoryView> => {
      if (olderRuntime) throw tooOld();
      return {
        history,
        observedCount: history.length,
        limit: 50,
        omitsSuperseded: false,
        conflictsOmitted: false,
        ...limits,
      };
    }),
    decideJoinRequest: vi.fn(async (request: JoinRequestDecideRequest) => {
      const decided = await decide(request);
      const { decision, requestId } = request;
      if (decision.kind === "approve")
        active = [
          ...active,
          ...listed
            .filter((r) => r.requestId === requestId)
            .map((r) =>
              member({
                requestId,
                candidateDid: r.candidateDid,
                authorizationExpiresAt: lapsesIn(decision.leaseSeconds),
              }),
            ),
        ];
      if (decision.kind === "revoke") {
        active = active.filter((m) => m.requestId !== requestId);
        history = [
          recorded({
            requestId,
            state: "revoked",
            revokedAt: new Date().toISOString(),
            revokedBy: OPERATOR,
          }),
          ...history,
        ];
      }
      listed = listed.filter((r) => r.requestId !== requestId);
      return decided;
    }),
  };
  const app = testApp({
    api,
    snapshot: {
      bootstrap: { ...bootstrap, initNodeDid },
      client: { deployments: [node()], ...(clientDid ? { clientDid } : {}) },
    },
  });
  renderIn(app, <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />);
  return api;
}

afterEach(() => {
  cleanup();
  resetJoins();
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
      screen.getByText("Invites need a newer runtime on Local Agent."),
    ).toBeTruthy();
    expect(
      within(screen.getByRole("tablist"))
        .getAllByRole("tab")
        .map((t) => t.textContent),
    ).toEqual(["Access", "History"]);
  });

  it("offers requests alone from a runtime too old for the rest", async () => {
    show({ olderRuntime: true });
    expect(
      await screen.findByRole("region", { name: "Requests to join" }),
    ).toBeTruthy();
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

  it("approves for the lease chosen, then lists them under Access", async () => {
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
    expect(api.listJoinMembers).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("region", { name: "Requests to join" })).toBeNull();
    const approved = within(access).getByRole("listitem", {
      name: /^did:key:z6MkCa…XYZ123: Whole node/,
    });
    expect(approved.textContent).toContain("Ends in 90d");
    expect(screen.getByRole("tab", { name: /^Access/ }).textContent).toContain("1");
    expect(screen.queryByRole("tab", { name: /^Invites/ })).toBeNull();
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
    expect(await screen.findByText("Nothing is shared yet.")).toBeTruthy();
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

  it("says so in one place when nothing is shared, with the way to join", async () => {
    show({ requests: [] });
    const empty = await screen.findByTestId("sharing-empty");
    expect(empty.textContent).toContain("Nothing is shared yet.");
    expect(
      within(empty).getByRole("button", { name: "Join with an invite" }),
    ).toBeTruthy();
    expect(within(empty).queryByRole("button", { name: "Invite someone" })).toBeNull();
    expect(screen.queryByRole("region", { name: "Requests to join" })).toBeNull();
    expect(screen.queryByRole("tablist")).toBeNull();
  });

  it("says only that nobody is waiting where the runtime can't say who has access", async () => {
    show({ requests: [], olderRuntime: true });
    const empty = await screen.findByTestId("sharing-empty");
    expect(empty.textContent).toContain("Nobody is waiting to join Local Agent.");
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
        listJoinMembers: vi.fn(async () => []),
        readJoinHistory: vi.fn(),
        decideJoinRequest: vi.fn(),
      },
      snapshot: { bootstrap, client: { deployments: [node()] } },
    });
    renderIn(app, <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />);
    expect((await screen.findByRole("alert")).textContent).toContain(
      "runtime is restarting",
    );
  });

  it("shows a failure to list who has access as a failure, not as an older runtime", async () => {
    withFlags({ sharing: true });
    const app = testApp({
      api: {
        listJoinRequests: vi.fn(async () => []),
        listJoinMembers: vi.fn(async () => {
          throw new BridgeInvokeError({
            code: "unknown",
            message: "enrollment authority is not ready",
            retryable: false,
            endpoint: null,
          });
        }),
        readJoinHistory: vi.fn(),
        decideJoinRequest: vi.fn(),
      },
      snapshot: { bootstrap, client: { deployments: [node()] } },
    });
    renderIn(app, <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />);
    expect((await screen.findByRole("alert")).textContent).toContain(
      "enrollment authority is not ready",
    );
    expect(screen.queryByText(/newer runtime/)).toBeNull();
  });

  it("lists who has access by DID, with their profile and when it ends", async () => {
    const user = userEvent.setup();
    show({ requests: [], members: [member()] });
    const access = await screen.findByRole("list", { name: "Who has access" });
    const row = within(access).getByRole("listitem");
    expect(row.getAttribute("aria-label")).toBe("did:key:z6MkMe…ABC789: Whole node");
    expect(row.textContent).toContain("client");
    expect(row.textContent).toContain("Ends in 30d");
    expect(row.textContent).not.toContain("Since");
    await user.click(within(row).getByRole("button", { name: /^More for/ }));
    expect(await screen.findByRole("menuitem", { name: "Revoke…" })).toBeTruthy();
    expect(screen.queryByRole("menuitem", { name: "Renew…" })).toBeNull();
  });

  it("revokes by the member's request, after asking", async () => {
    const user = userEvent.setup();
    const api = show({ requests: [], members: [member()] });
    const row = within(
      await screen.findByRole("list", { name: "Who has access" }),
    ).getByRole("listitem");
    await user.click(within(row).getByRole("button", { name: /^More for/ }));
    await user.click(await screen.findByRole("menuitem", { name: "Revoke…" }));
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    await user.click(within(row).getByRole("button", { name: "Revoke" }));
    expect(api.decideJoinRequest).toHaveBeenCalledWith({
      nodeDid: OWN,
      requestId: "request-phone",
      decision: { kind: "revoke" },
    });
    expect(
      await screen.findByText("Nobody else has access to Local Agent."),
    ).toBeTruthy();
    await user.click(screen.getByRole("tab", { name: /^History/ }));
    const history = await screen.findByRole("list", { name: "Sharing history" });
    const line = within(history).getByRole("listitem");
    expect(line.getAttribute("aria-label")).toBe("Revoked: did:key:z6MkMe…ABC789");
    expect(line.textContent).toContain("By did:key:z6MkOp…DEF456");
  });

  it("marks this app's own way into the node, and doesn't offer to revoke it", async () => {
    show({
      requests: [],
      members: [member(), member({ requestId: "request-app", candidateDid: OPERATOR })],
      clientDid: OPERATOR,
    });
    const rows = within(
      await screen.findByRole("list", { name: "Who has access" }),
    ).getAllByRole("listitem");
    const app = rows.find((row) => row.textContent?.includes("This app"))!;
    expect(app).toBeTruthy();
    expect(within(app).queryByRole("button", { name: /^More for/ })).toBeNull();
    const other = rows.find((row) => row !== app)!;
    expect(within(other).getByRole("button", { name: /^More for/ })).toBeTruthy();
  });

  it("lists what was decided, and what the runtime says it left out", async () => {
    const user = userEvent.setup();
    show({
      requests: [],
      records: [recorded(), recorded({ requestId: "request-2", state: "denied" })],
      limits: { observedCount: 73, omitsSuperseded: true },
    });
    await user.click(await screen.findByRole("tab", { name: /^History/ }));
    const history = await screen.findByRole("list", { name: "Sharing history" });
    expect(
      within(history)
        .getAllByRole("listitem")
        .map((r) => r.getAttribute("aria-label")),
    ).toEqual(["Approved: did:key:z6MkMe…ABC789", "Denied: did:key:z6MkMe…ABC789"]);
    expect(
      screen.getByText(
        "Showing the latest 50 of 73. Decisions replaced by a later one aren't shown.",
      ),
    ).toBeTruthy();
  });

  it("keeps History open when all the runtime has is in conflict", async () => {
    const user = userEvent.setup();
    show({ requests: [], limits: { conflictsOmitted: true } });
    await user.click(await screen.findByRole("tab", { name: /^History/ }));
    expect(await screen.findByText("Decisions in conflict aren't shown.")).toBeTruthy();
    expect(screen.getByText("Nothing decided yet.")).toBeTruthy();
    expect(screen.queryByTestId("sharing-empty")).toBeNull();
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
