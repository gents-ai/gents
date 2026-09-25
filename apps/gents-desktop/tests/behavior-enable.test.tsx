import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Shell } from "../src/ui/hooks/useShell";
import { BehaviorsPanel } from "../src/ui/screens/agent/BehaviorsPanel";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

function shellWith(patch: ReturnType<typeof vi.fn>) {
  const api = { patchConfigComponents: patch };
  return {
    api,
    snapshot: { bootstrap },
    applyConfig: (run: (bridge: typeof api) => Promise<unknown>) => run(api),
  } as unknown as Shell;
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
    render(
      <BehaviorsPanel
        shell={shellWith(patch)}
        deployment={withOps({ enabled: false, contextId: null })}
      />,
    );
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
    render(
      <BehaviorsPanel
        shell={shellWith(patch)}
        deployment={withOps({ enabled: false, contextId: "gone" })}
      />,
    );
    await userEvent
      .setup()
      .click(screen.getAllByRole("switch", { name: "Ops is disabled" })[0]!);
    await waitFor(() =>
      expect(toast).toHaveBeenCalledWith(`Couldn’t turn it on: ${refusal}`),
    );
  });
});
