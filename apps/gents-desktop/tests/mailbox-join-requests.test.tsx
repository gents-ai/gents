import { cleanup, fireEvent, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Toaster } from "@gents/ui/components/sonner";
import { toast } from "sonner";
import type { JoinRequestView } from "@source-inc/gents-desktop-client";
import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import {
  JoinRequestDialog,
  resetJoinRequestReview,
} from "../src/ui/screens/sharing/JoinRequestDialog";
import { resetPreferences } from "../src/ui/preferences";
import { node, renderIn, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const OWN = deployment.nodeDid;

const request = (id: string, did: string): JoinRequestView => ({
  requestId: id,
  networkId: "network-1",
  candidateDid: did,
  candidatePeer: "peer-1",
  ownerAgent: OWN,
  expiresAt: new Date(Date.now() + 4 * 60_000).toISOString(),
  profile: "client",
  issuedAt: new Date().toISOString(),
});

function show(listed: JoinRequestView[], sharing = true) {
  withFlags({ sharing });
  const api = {
    listJoinRequests: vi.fn(async () => listed),
    decideJoinRequest: vi.fn(async (decision: { requestId: string }) => {
      listed = listed.filter((r) => r.requestId !== decision.requestId);
      return { requestId: decision.requestId, state: "denied", deliveryPending: false };
    }),
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
      <MailboxScreen />
      <JoinRequestDialog />
    </>,
  );
  return api;
}

afterEach(() => {
  toast.dismiss();
  cleanup();
  resetJoinRequestReview();
  resetPreferences();
});

describe("requests to join in the Mailbox", () => {
  it("lists a waiting request with Deny and Review, in place of the empty Mailbox", async () => {
    show([request("r1", "did:key:z6MkLaptopAAAA1111")]);
    const list = await screen.findByRole("list", { name: "Requests to join" });
    const row = within(list).getByRole("listitem", { name: /asks to join/ });
    expect(row.textContent).toContain("wants the whole node");
    expect(screen.queryByText("Nothing needs your attention")).toBeNull();
    fireEvent.click(within(row).getByRole("button", { name: "Review" }));
    expect(await screen.findByTestId("join-request-dialog")).toBeTruthy();
  });

  it("denies from the Mailbox", async () => {
    const api = show([request("r1", "did:key:z6MkLaptopAAAA1111")]);
    const row = await screen.findByRole("listitem", { name: /asks to join/ });
    fireEvent.click(within(row).getByRole("button", { name: "Deny" }));
    await vi.waitFor(() =>
      expect(api.decideJoinRequest).toHaveBeenCalledWith(
        expect.objectContaining({ requestId: "r1" }),
      ),
    );
    expect(await screen.findByText("Denied: that device won't get in")).toBeTruthy();
  });

  it("shows nothing of joining with sharing off", async () => {
    const api = show([request("r1", "did:key:z6MkLaptopAAAA1111")], false);
    expect(await screen.findByText("Nothing needs your attention")).toBeTruthy();
    expect(api.listJoinRequests).not.toHaveBeenCalled();
    expect(screen.queryByRole("list", { name: "Requests to join" })).toBeNull();
  });
});
