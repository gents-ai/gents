import { act, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { unavailableSharing } from "../src/hooks/sharing";
import { useSharing } from "../src/ui/hooks/useSharing";
import { renderIn, testApp } from "./app-fixture";
import { createDesktopUiHarness } from "./ui-harness/desktopHarness";
import {
  HARNESS_BRUNO,
  SHARING_SCENARIOS,
  createHarnessSharing,
} from "./ui-harness/sharingHarness";

const NODE = "did:key:z6MkSharingNode";
const DAY = 24 * 60 * 60;

describe("sharing before the runtime offers it", () => {
  it("reads nothing and refuses every action in words", async () => {
    expect(await unavailableSharing.read(NODE)).toBeNull();
    await expect(unavailableSharing.approve(NODE, "r", DAY)).rejects.toThrow(
      "Sharing needs a newer runtime on this node.",
    );
    await expect(unavailableSharing.revoke(NODE, "r")).rejects.toThrow("newer runtime");
  });

  it("is the app's adapter unless a bridge brings one", async () => {
    expect(testApp().sharing).toBe(unavailableSharing);
  });
});

describe("the harness's sharing", () => {
  it("approves a request into a member with a lease, and records it", async () => {
    const sharing = createHarnessSharing("sharing", () => 0);
    await sharing.approve(NODE, "request-harness-jack", 30 * DAY);
    const state = (await sharing.read(NODE))!;
    expect(state.requests.map((r) => r.requestId)).toEqual(["request-harness-laptop"]);
    const jack = state.members.find((m) => m.party.name === "Jack")!;
    expect(jack.leaseExpiresAt).toBe(new Date(30 * DAY * 1000).toISOString());
    expect(state.history[0]).toMatchObject({
      kind: "approved",
      party: { name: "Jack" },
    });
  });

  it("denies a request and revokes a member, each once", async () => {
    const sharing = createHarnessSharing("sharing");
    await sharing.deny(NODE, "request-harness-laptop");
    await expect(sharing.deny(NODE, "request-harness-laptop")).rejects.toThrow(
      "already decided",
    );
    await sharing.revoke(NODE, HARNESS_BRUNO);
    await expect(sharing.revoke(NODE, HARNESS_BRUNO)).rejects.toThrow(
      "no longer have access",
    );
    const state = (await sharing.read(NODE))!;
    expect(state.members.map((m) => m.party.name)).toEqual(["David's phone"]);
    expect(state.history.slice(0, 2).map((d) => d.kind)).toEqual(["revoked", "denied"]);
  });

  it("reads a part its runtime doesn't offer as empty", async () => {
    const state = (await createHarnessSharing("sharing-requests-only").read(NODE))!;
    expect(state.requests).toHaveLength(2);
    expect([state.members, state.history]).toEqual([[], []]);
  });

  it("stands in for a runtime that can't share", async () => {
    expect(createHarnessSharing("sharing-unavailable")).toBe(unavailableSharing);
  });

  it("opens every sharing scenario by name", () => {
    for (const scenario of SHARING_SCENARIOS) {
      const harness = createDesktopUiHarness({ scenario });
      expect(harness.scenario).toBe(scenario);
      expect(harness.sharing).toBeDefined();
    }
  });
});

function Requests({ nodeDid }: { nodeDid: string | null }) {
  const view = useSharing(nodeDid);
  if (view.status !== "ready") return <p>{view.status}</p>;
  return <p>{view.state.requests.length} waiting</p>;
}

describe("useSharing", () => {
  it("follows the adapter's changes", async () => {
    const sharing = createHarnessSharing("sharing");
    renderIn(testApp({ sharing }), <Requests nodeDid={NODE} />);
    expect(await screen.findByText("2 waiting")).toBeTruthy();
    await act(() => sharing.deny(NODE, "request-harness-jack"));
    expect(await screen.findByText("1 waiting")).toBeTruthy();
  });

  it("says when the node can't share, or there's no node", async () => {
    renderIn(testApp(), <Requests nodeDid={NODE} />);
    expect(await screen.findByText("unavailable")).toBeTruthy();
  });
});
