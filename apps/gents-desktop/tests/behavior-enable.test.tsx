import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Shell } from "../src/ui/hooks/useShell";
import { BehaviorsPanel } from "../src/ui/screens/agent/BehaviorsPanel";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import { renderIn, testApp } from "./app-fixture";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

/* the panel under an app whose failed actions are toasted, as the root's */
function renderPanel(patch: ReturnType<typeof vi.fn>, node: typeof deployment) {
  const api = { patchConfigComponents: patch };
  const shell = { api, snapshot: { bootstrap } } as unknown as Shell;
  return renderIn(
    testApp({ api, reportFailure: toast }),
    <BehaviorsPanel shell={shell} deployment={node} />,
  );
}

const withOps = (changes: Partial<(typeof deployment.behaviors)[number]>) => ({
  ...deployment,
  behaviors: deployment.behaviors.map((b) =>
    b.behaviorId === "ops" ? { ...b, ...changes } : b,
  ),
});

beforeEach(() => vi.clearAllMocks());

/* Publication decides whether a behavior can run; the switch does not
   second-guess it. An absent context is allowed, a dangling one refused. */
describe("enabling a behavior", () => {
  it("enables a behavior with no context directly", async () => {
    const patch = vi.fn().mockResolvedValue({});
    renderPanel(patch, withOps({ enabled: false, contextId: null }));
    const toggle = screen.getAllByRole("switch", { name: "Ops is disabled" })[0]!;
    expect(toggle).not.toHaveAttribute("aria-disabled");
    await userEvent.setup().click(toggle);
    await waitFor(() =>
      expect(patch).toHaveBeenCalledWith({
        agentDid: deployment.agentDid,
        patches: [
          { collection: "AgentBehavior", id: "ops", changes: { enabled: true } },
        ],
      }),
    );
    expect(toast).toHaveBeenCalledWith("Ops is enabled");
  });

  it("shows the publication refusal for a dangling context", async () => {
    const refusal =
      'AgentBehavior ops field context_id references missing AgentContext "gone" within agent_did did:key:z6MkAgent';
    const patch = vi.fn().mockRejectedValue(new Error(refusal));
    renderPanel(patch, withOps({ enabled: false, contextId: "gone" }));
    await userEvent
      .setup()
      .click(screen.getAllByRole("switch", { name: "Ops is disabled" })[0]!);
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(`Couldn’t save the configuration: ${refusal}`),
    );
  });
});
