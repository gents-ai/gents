import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

const { toast } = vi.hoisted(() => ({ toast: vi.fn() }));
vi.mock("sonner", async (actual) => ({
  ...(await actual<typeof import("sonner")>()),
  toast,
}));

import { AgentsScreen } from "../src/ui/screens/AgentsScreen";
import { node, renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const JUNO = "did:key:juno";
const juno = node({
  nodeDid: JUNO,
  label: "Juno",
  source: "enrollment",
  node: { ...deployment.node, nodeDid: JUNO, displayName: "Juno" },
  agents: [],
  runtimeConnection: "disconnected",
});

describe("checking a node's connection", () => {
  afterEach(() => toast.mockClear());

  const check = async (answer: string | null) => {
    const reconnectNode = vi.fn().mockResolvedValue(answer);
    renderIn(
      testApp({
        api: { reconnectNode },
        deployments: [node(), juno],
        selection: { nodeDid: deployment.nodeDid },
      }),
      <AgentsScreen />,
    );
    const user = userEvent.setup({ pointerEventsCheck: 0 });
    await user.click(screen.getByRole("button", { name: "Juno actions" }));
    await user.click(await screen.findByRole("menuitem", { name: "Check connection" }));
    await waitFor(() => expect(toast).toHaveBeenCalledTimes(1));
    expect(reconnectNode).toHaveBeenCalledWith(JUNO);
    return toast.mock.calls[0]![0];
  };

  it("asks the observer and says the node is connected once it answers", async () => {
    expect(await check("connected")).toBe("Connected to Juno");
  });

  it("names why it isn't connected", async () => {
    expect(await check("disconnected")).toBe("Can’t reach Juno");
  });

  it("says a node refuses this app", async () => {
    expect(await check("refused")).toBe("Juno doesn’t accept this app");
  });

  it("says a node that hasn't answered in time is still connecting", async () => {
    expect(await check(null)).toBe("Connecting to Juno");
  });
});
