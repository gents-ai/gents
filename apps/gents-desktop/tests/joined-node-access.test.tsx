import { act, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type {
  DesktopClientSnapshot,
  EndedAccessView,
} from "@source-inc/gents-desktop-client";

const { toast } = vi.hoisted(() => ({ toast: vi.fn() }));
vi.mock("sonner", async (actual) => ({
  ...(await actual<typeof import("sonner")>()),
  toast,
}));

import { applyFleetSnapshot } from "../src/hooks/fleetStore";
import { createDraftStore } from "../src/hooks/draftStore";
import { noticeEndedAccess } from "../src/hooks/endedAccessNotices";
import { leaseLeft } from "../src/hooks/nodeAccess";
import { reconcileSelection } from "../src/hooks/selectionReconcile";
import { selection } from "../src/hooks/selectionStore";
import { preferences } from "../src/ui/preferences";
import { AgentsScreen } from "../src/ui/screens/AgentsScreen";
import { node, publishSnapshot, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";
import { shellStores } from "./shell-fixture";

const HOME = deployment.nodeDid;
const HOUR = 60 * 60 * 1000;
const DAY = 24 * HOUR;
const NOW = Date.parse("2026-11-05T12:00:00Z");

const joined = (nodeDid: string, name: string, leaseExpiresAt: string | null) =>
  node({
    nodeDid,
    label: name,
    source: "enrollment",
    operatorAccess: false,
    leaseExpiresAt,
    node: { ...deployment.node, nodeDid, displayName: name },
    agents: deployment.agents.map((a) => ({ ...a, nodeDid })),
  });

const revoked = (at = new Date(NOW).toISOString()): EndedAccessView => ({
  nodeDid: "did:key:harbor",
  label: "Harbor",
  reason: "revoked",
  endedAt: at,
  leaseExpiresAt: new Date(NOW + 6 * DAY).toISOString(),
});

const snapshot = (
  deployments: ReturnType<typeof node>[],
  endedAccess: EndedAccessView[] = [],
) =>
  ({
    bootstrap: { initNodeDid: HOME },
    client: { clientDid: "test-client", deployments, endedAccess },
  }) as unknown as DesktopClientSnapshot;

afterEach(() => {
  vi.useRealTimers();
  toast.mockClear();
});

describe("how long a joined node's access lasts", () => {
  it("is said in the unit a person plans by, and warns in its last days", () => {
    const at = (ms: number) => new Date(NOW + ms).toISOString();
    expect(leaseLeft(at(20 * DAY + HOUR), NOW)).toEqual({
      text: "Access ends in 20 days",
      soon: false,
      ended: false,
    });
    expect(leaseLeft(at(2 * DAY), NOW)?.soon).toBe(true);
    expect(leaseLeft(at(DAY), NOW)?.text).toBe("Access ends in 1 day");
    expect(leaseLeft(at(5 * HOUR + 1), NOW)?.text).toBe("Access ends in 5 hours");
    expect(leaseLeft(at(90_000), NOW)?.text).toBe("Access ends in 2 minutes");
    expect(leaseLeft(at(-1), NOW)).toMatchObject({ text: "Access ended", ended: true });
    expect(leaseLeft(null, NOW)).toBeNull();
    expect(leaseLeft("not a time", NOW)).toBeNull();
  });
});

describe("the Nodes page", () => {
  it("shows how long a joined node lets this app in, and not for this machine's node", () => {
    vi.useFakeTimers({ now: NOW, toFake: ["Date"] });
    const app = testApp({
      deployments: [
        node(),
        joined("did:key:amy", "Amy", new Date(NOW + 2 * DAY + HOUR).toISOString()),
      ],
      selection: { nodeDid: HOME },
    });
    renderIn(app, <AgentsScreen />);
    const lease = screen.getByTestId("node-lease");
    expect(lease).toHaveTextContent("Access ends in 2 days");
    expect(lease).toHaveClass("text-warning");
    expect(screen.getAllByTestId("node-lease")).toHaveLength(1);
  });

  it("keeps a node whose access ended where it was, says why, and Dismiss forgets it", async () => {
    let ended = [revoked()];
    const dismissEndedAccess = vi.fn(async () => {
      ended = [];
      return snapshot([node()]);
    });
    const fetchDesktopSnapshot = vi.fn(async () => snapshot([node()], ended));
    const app = testApp({
      api: { dismissEndedAccess, fetchDesktopSnapshot },
      snapshot: snapshot([node()], [revoked()]),
      selection: { nodeDid: HOME },
    });
    renderIn(app, <AgentsScreen />);
    const remotes = screen.getByText("Remote nodes").closest("section")!;
    const row = within(remotes).getByTestId("ended-node");
    expect(row).toHaveTextContent("Harbor");
    expect(row).toHaveTextContent("Access revoked");
    expect(row).toHaveTextContent("Its owner ended this app's access.");
    /* the empty group's line doesn't show beside it */
    expect(screen.queryByText("No remote nodes paired.")).toBeNull();

    await userEvent.click(
      within(row).getByRole("button", {
        name: "Dismiss: Your access to Harbor was revoked",
      }),
    );
    expect(dismissEndedAccess).toHaveBeenCalledWith("did:key:harbor");
    expect(screen.queryByTestId("ended-node")).toBeNull();
  });

  it("says a lease ran out differently from a revocation", () => {
    const app = testApp({
      snapshot: snapshot([node()], [{ ...revoked(), reason: "expired" }]),
      selection: { nodeDid: HOME },
    });
    renderIn(app, <AgentsScreen />);
    const row = screen.getByTestId("ended-node");
    expect(row).toHaveTextContent("Access ran out");
    expect(row).toHaveTextContent("The time its owner allowed is over.");
  });
});

describe("when a joined node's access ends", () => {
  it("tells the person once, for access that ended while the app ran", () => {
    const stores = shellStores({ selection: { nodeDid: HOME } });
    noticeEndedAccess(stores.fleet, NOW);
    /* ended before this run: found again at launch, left to the Nodes page */
    applyFleetSnapshot(
      stores.fleet,
      snapshot(
        [node()],
        [{ ...revoked(new Date(NOW - DAY).toISOString()), nodeDid: "did:key:old" }],
      ),
    );
    expect(toast).not.toHaveBeenCalled();

    applyFleetSnapshot(
      stores.fleet,
      snapshot([node()], [revoked(new Date(NOW + HOUR).toISOString())]),
    );
    expect(toast).toHaveBeenCalledTimes(1);
    expect(toast).toHaveBeenCalledWith(
      "Your access to Harbor was revoked",
      expect.objectContaining({
        description: expect.stringContaining("ask its owner to let you in"),
      }),
    );

    applyFleetSnapshot(
      stores.fleet,
      snapshot([node()], [revoked(new Date(NOW + HOUR).toISOString())]),
    );
    expect(toast).toHaveBeenCalledTimes(1);
  });

  it("moves the app off that node when it was the one in use", () => {
    const stores = shellStores({ selection: { nodeDid: null } });
    applyFleetSnapshot(
      stores.fleet,
      snapshot([node(), joined("did:key:harbor", "Harbor", null)]),
    );
    reconcileSelection(
      stores,
      (nodeDid) => selection.selectNode(stores.selection, nodeDid),
      createDraftStore(),
    );
    act(() => selection.selectNode(stores.selection, "did:key:harbor"));
    expect(stores.selection.getState().nodeDid).toBe("did:key:harbor");

    act(() => applyFleetSnapshot(stores.fleet, snapshot([node()], [revoked()])));
    expect(stores.selection.getState().nodeDid).toBe(HOME);
  });

  it("names it when the app was set to open on that node", () => {
    try {
      localStorage.removeItem("gents.activeNode");
    } catch {}
    preferences.setOpenOn("did:key:harbor");
    const stores = shellStores({ selection: { nodeDid: null } });
    applyFleetSnapshot(stores.fleet, snapshot([node()], [revoked()]));
    reconcileSelection(
      stores,
      (nodeDid) => selection.selectNode(stores.selection, nodeDid),
      createDraftStore(),
    );
    expect(stores.selection.getState().nodeDid).toBe(HOME);
    expect(toast).toHaveBeenCalledWith(
      "Your access to Harbor ended, so the app opened on Local Agent",
    );
    preferences.setOpenOn(null);
  });

  it("leaves a node that is merely missing alone", () => {
    const stores = shellStores({ selection: { nodeDid: "did:key:gone" } });
    reconcileSelection(
      stores,
      (nodeDid) => selection.selectNode(stores.selection, nodeDid),
      createDraftStore(),
    );
    act(() => applyFleetSnapshot(stores.fleet, snapshot([node()])));
    expect(stores.selection.getState().nodeDid).toBe("did:key:gone");
  });
});

describe("a later snapshot", () => {
  it("keeps the ended list when nothing about it changed", () => {
    const app = testApp({ snapshot: snapshot([node()], [revoked()]) });
    const first = app.stores.fleet.getState().endedAccess;
    publishSnapshot(app, snapshot([node()], [revoked()]));
    expect(app.stores.fleet.getState().endedAccess).toBe(first);
  });
});
