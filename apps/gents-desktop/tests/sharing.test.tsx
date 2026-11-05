import { act, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { unavailableSharing } from "../src/hooks/sharing";
import { useSharing } from "../src/ui/hooks/useSharing";
import { renderIn, testApp } from "./app-fixture";
import { createDesktopUiHarness } from "./ui-harness/desktopHarness";
import {
  HARNESS_GAME_PACK,
  HARNESS_INVITE,
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
    await expect(unavailableSharing.readInvite("gents://invite/x")).rejects.toThrow(
      "newer runtime",
    );
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
    const game = {
      kind: "pack",
      pack: HARNESS_GAME_PACK,
      name: "Tic-tac-toe",
    } as const;
    await sharing.revoke(NODE, "did:key:z6MkHarnessBruno", game);
    await expect(
      sharing.revoke(NODE, "did:key:z6MkHarnessBruno", game),
    ).rejects.toThrow("no longer have access");
    const state = (await sharing.read(NODE))!;
    expect(state.members.map((m) => [m.party.name, m.scope.kind])).toEqual([
      ["David's phone", "node"],
      ["Bruno", "agent"],
    ]);
    expect(state.history.slice(0, 2).map((d) => d.kind)).toEqual(["revoked", "denied"]);
  });

  it("makes an invite the joining side can read by link or code, until it's cancelled", async () => {
    const sharing = createHarnessSharing("sharing-empty");
    const game = {
      kind: "pack",
      pack: HARNESS_GAME_PACK,
      name: "Tic-tac-toe",
    } as const;
    const [player] = await sharing.profiles(NODE, game);
    const invite = await sharing.createInvite(NODE, {
      kind: "person",
      scope: game,
      profileId: player!.id,
      expiresInSeconds: 3 * DAY,
      uses: 1,
      preApproved: true,
    });
    expect((await sharing.readInvite(invite.code)).installs).toEqual([
      { pack: HARNESS_GAME_PACK, version: "1.2.0" },
    ]);
    expect(await sharing.acceptInvite(invite.link)).toMatchObject({
      state: "approved",
    });
    await sharing.cancelInvite(NODE, invite.inviteId);
    await expect(sharing.readInvite(invite.link)).rejects.toThrow("isn't valid");
  });

  it("reads the seeded invite, which still needs approval", async () => {
    const sharing = createHarnessSharing("sharing");
    expect((await sharing.readInvite(HARNESS_INVITE.code)).from.name).toBe("David");
    expect(await sharing.acceptInvite(HARNESS_INVITE.link)).toMatchObject({
      state: "pending_approval",
    });
  });

  it("reads a part its runtime doesn't offer as empty", async () => {
    const state = (await createHarnessSharing("sharing-requests-only").read(NODE))!;
    expect(state.requests).toHaveLength(2);
    expect([state.members, state.invites, state.history]).toEqual([[], [], []]);
  });

  it("revokes an approval by its request, except as today's runtime", async () => {
    expect(
      createHarnessSharing("sharing-requests-only").revokeApproval,
    ).toBeUndefined();
    const sharing = createHarnessSharing("sharing-approved-here");
    await sharing.revokeApproval!(NODE, "request-harness-tablet");
    await expect(
      sharing.revokeApproval!(NODE, "request-harness-tablet"),
    ).rejects.toThrow("already revoked");
    const full = createHarnessSharing("sharing", () => 0);
    await full.approve(NODE, "request-harness-jack", 30 * DAY);
    await full.revokeApproval!(NODE, "request-harness-jack");
    const state = (await full.read(NODE))!;
    expect(state.members.some((m) => m.party.name === "Jack")).toBe(false);
    expect(state.history[0]).toMatchObject({
      kind: "revoked",
      party: { name: "Jack" },
    });
  });

  it("stands in for a runtime that can't share", async () => {
    expect(createHarnessSharing("sharing-unavailable")).toBe(unavailableSharing);
  });

  it("opens every sharing scenario by name, with its packs to share or install", async () => {
    for (const scenario of SHARING_SCENARIOS) {
      const harness = createDesktopUiHarness({ scenario });
      expect(harness.scenario).toBe(scenario);
      expect(harness.sharing).toBeDefined();
      const { packs } = await harness.adapter.listInstalledPacks!();
      expect(packs.map((p) => p.pack)).toEqual(
        scenario === "sharing-invite-pack" ? [] : [HARNESS_GAME_PACK],
      );
    }
  });

  it("installs the invite's pack at the version reviewed", async () => {
    const harness = createDesktopUiHarness({ scenario: "sharing-invite-pack" });
    const pack = `${HARNESS_GAME_PACK}@1.2.0`;
    expect(
      await harness.adapter.installPack!({ package: pack, preview: true }),
    ).toMatchObject({ coordinate: HARNESS_GAME_PACK, version: "1.2.0" });
    expect((await harness.adapter.listInstalledPacks!()).packs).toEqual([]);
    await harness.adapter.installPack!({ package: pack });
    expect((await harness.adapter.listInstalledPacks!()).packs).toMatchObject([
      { pack: HARNESS_GAME_PACK, installed: "1.2.0" },
    ]);
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
