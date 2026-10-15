import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { AgentsPanel } from "../src/ui/screens/agent/BehaviorsPanel";
import { deployment } from "./config-panel-wiring/fixtures";
import { renderIn, testApp } from "./app-fixture";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

/* the panel under an app whose failed actions are toasted, as the root's */
function renderPanel(patch: ReturnType<typeof vi.fn>, node: typeof deployment) {
  const api = { patchConfigComponents: patch };
  return renderIn(
    testApp({ api, reportFailure: toast }),
    <AgentsPanel deployment={node} />,
  );
}

const withOps = (changes: Partial<(typeof deployment.agents)[number]>) => ({
  ...deployment,
  agents: deployment.agents.map((a) =>
    a.agentId === "ops" ? { ...a, ...changes } : a,
  ),
});

beforeEach(() => vi.clearAllMocks());

/* Publication decides whether an agent can run; the switch does not
   second-guess it. An absent context is allowed, a dangling one refused. */
describe("enabling an agent", () => {
  it("enables an agent with no context directly", async () => {
    const patch = vi.fn().mockResolvedValue({});
    renderPanel(patch, withOps({ enabled: false, contextId: null }));
    const toggle = screen.getAllByRole("switch", { name: "Ops is disabled" })[0]!;
    expect(toggle).not.toHaveAttribute("aria-disabled");
    await userEvent.setup().click(toggle);
    await waitFor(() =>
      expect(patch).toHaveBeenCalledWith({
        nodeDid: deployment.nodeDid,
        patches: [{ collection: "Agent", id: "ops", changes: { enabled: true } }],
      }),
    );
    expect(toast).toHaveBeenCalledWith("Ops is enabled");
  });

  it("shows the publication refusal for a dangling context", async () => {
    const refusal =
      'Agent ops field context_id references missing AgentContext "gone" within node_did did:key:z6MkAgent';
    const patch = vi.fn().mockRejectedValue(new Error(refusal));
    renderPanel(patch, withOps({ enabled: false, contextId: "gone" }));
    await userEvent
      .setup()
      .click(screen.getAllByRole("switch", { name: "Ops is disabled" })[0]!);
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(`Couldn’t turn it on: ${refusal}`),
    );
    /* the action reports it; the switch's own catch does not again (#2043) */
    expect(toast).toHaveBeenCalledTimes(1);
  });
});
