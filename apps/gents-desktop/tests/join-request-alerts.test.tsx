import { act, cleanup, fireEvent, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Toaster } from "@gents/ui/components/sonner";
import type { JoinRequestView } from "@source-inc/gents-desktop-client";
import { JoinRequestAlerts } from "../src/ui/app/JoinRequestAlerts";
import type { Route } from "../src/ui/lib/router";
import { resetPreferences } from "../src/ui/preferences";
import { node, renderIn, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const navigate = vi.hoisted(() => vi.fn());
vi.mock("@/lib/router", async (actual) => ({
  ...(await actual<typeof import("../src/ui/lib/router")>()),
  navigate,
}));

const OWN = deployment.nodeDid;

const request = (id: string, did: string): JoinRequestView => ({
  requestId: id,
  networkId: "network-1",
  candidateDid: did,
  candidatePeer: "peer-1",
  ownerAgent: OWN,
  expiresAt: "2026-11-05T12:00:00Z",
  profile: "client",
  issuedAt: "2026-11-05T11:55:00Z",
});

function show(route: Route = { name: "sessions" }, sharing = true) {
  withFlags({ sharing });
  let listed: JoinRequestView[] = [request("r1", "did:key:z6MkLaptopAAAA1111")];
  const api = {
    listJoinRequests: vi.fn(async () => listed),
    decideJoinRequest: vi.fn(),
    listJoinMembers: vi.fn(async () => []),
    readJoinHistory: vi.fn(async () => ({
      history: [],
      observedCount: 0,
      limit: 50,
      omitsSuperseded: false,
      conflictsOmitted: false,
    })),
  };
  const app = testApp({
    api,
    snapshot: {
      bootstrap: { ...bootstrap, initNodeDid: OWN },
      client: { deployments: [node()] },
    },
  });
  renderIn(
    app,
    <>
      <Toaster />
      <JoinRequestAlerts route={route} />
    </>,
  );
  return { api, set: (next: JoinRequestView[]) => (listed = next) };
}

afterEach(() => {
  cleanup();
  resetPreferences();
  vi.useRealTimers();
});

describe("telling the person about requests to join", () => {
  it("tells once about a request, and Review opens Sharing", async () => {
    show();
    expect(await screen.findByText(/asked to join/)).toBeTruthy();
    expect(screen.getAllByText(/asked to join/)).toHaveLength(1);
    fireEvent.click(screen.getByRole("button", { name: "Review" }));
    expect(navigate).toHaveBeenCalledWith({
      name: "agent",
      nodeDid: OWN,
      section: "sharing",
    });
  });

  it("takes the notice away once the request is no longer waiting", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const { set } = show();
    expect(await screen.findByText(/asked to join/)).toBeTruthy();
    set([]);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(20_000);
    });
    await vi.waitFor(() => expect(screen.queryByText(/asked to join/)).toBeNull());
  });

  it("says nothing while Sharing is open, or with the feature off", async () => {
    const { api } = show({ name: "agent", nodeDid: OWN, section: "sharing" });
    await vi.waitFor(() => expect(api.listJoinRequests).toHaveBeenCalled());
    expect(screen.queryByText(/asked to join/)).toBeNull();
    cleanup();
    const off = show({ name: "sessions" }, false);
    await new Promise((r) => setTimeout(r, 50));
    expect(off.api.listJoinRequests).not.toHaveBeenCalled();
    expect(screen.queryByText(/asked to join/)).toBeNull();
  });
});
