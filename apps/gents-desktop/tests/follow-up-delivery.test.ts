import { describe, expect, it, vi } from "vitest";
import type {
  DeploymentView,
  DesktopApiAdapter,
  DesktopClientSnapshot,
} from "@source-inc/gents-desktop-client";
import { createChatActions } from "../src/hooks/chatActions";
import { applyFleetSnapshot } from "../src/hooks/fleetStore";
import type { ShellProjection } from "../src/hooks/shellProjection";
import { admittingProjection, shellStores } from "./shell-fixture";

const deployment = {
  nodeDid: "agent",
  node: { nodeDid: "agent", defaultAgentId: "setup" },
  agents: [{ agentId: "setup", enabled: true, isDefault: true }],
  sessions: [{ sessionId: "session-1", agentId: "setup" }],
  mailboxItems: [],
} as unknown as DeploymentView;

function send(running: boolean) {
  const stores = shellStores({
    selection: { nodeDid: "agent", agentId: "setup", sessionId: "session-1" },
  });
  applyFleetSnapshot(stores.fleet, {
    bootstrap: {},
    client: { deployments: [deployment] },
  } as unknown as DesktopClientSnapshot);
  const sendChatMessage = vi.fn().mockResolvedValue({
    nodeDid: "agent",
    agentId: "setup",
    sessionId: "session-1",
    requestId: "request-1",
  });
  const projection = admittingProjection("setup");
  const actions = createChatActions({
    api: { sendChatMessage } as unknown as DesktopApiAdapter,
    stores,
    project: () =>
      ({
        ...projection,
        shellProjection: {
          nonEmptyContentSendStatus: running
            ? { kind: "queue", turnState: "running", hint: "Queue message" }
            : { kind: "ready" },
        },
      }) as unknown as ShellProjection,
    refreshSession: async () => null,
    refreshSnapshot: async () => {},
    reportFailure: vi.fn(),
  });
  return { actions, sendChatMessage };
}

describe("a follow-up sent while a turn runs", () => {
  it("goes the way it was asked: queued after the turn or steered into it", async () => {
    for (const followUp of ["queue", "steer"] as const) {
      const { actions, sendChatMessage } = send(true);
      await actions.sendMessage("and also this", undefined, followUp);
      expect(sendChatMessage).toHaveBeenCalledWith(
        expect.objectContaining({ delivery: followUp }),
      );
    }
  });

  it("names no delivery for a message that starts a turn", async () => {
    const { actions, sendChatMessage } = send(false);
    await actions.sendMessage("hello", undefined, "queue");
    expect(sendChatMessage).toHaveBeenCalledWith(
      expect.objectContaining({ delivery: null }),
    );
  });
});
