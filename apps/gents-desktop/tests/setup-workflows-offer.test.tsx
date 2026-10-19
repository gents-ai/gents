import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { WorkflowsOffer } from "../src/ui/screens/setup/WorkflowsOffer";
import { ENGINEER_AGENT_TAG, engineerPatches } from "../src/ui/lib/engineer";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const wirePack = vi.fn();
const installPack = vi.fn();

const job = {
  agent_targets: ["fixture/worker"],
  max_live: 3,
  max_spawns: 12,
  max_messages_per_slot: 4,
  result_bytes: 4096,
  max_wall_secs: 600,
  policy: { dynamic: "check", dynamic_targets: [], trusted_packs: [], project_roots: [] },
};

function show(
  plugins: { plugin: string; pack: string | null; job: boolean }[],
  node: typeof deployment = deployment,
) {
  return renderIn(
    testApp({
      api: { listPackWireSources: async () => ({ plugins, target_packs: [] }), wirePack, installPack },
    }),
    <WorkflowsOffer deployment={node} />,
  );
}

describe("setup workflows offer", () => {
  beforeEach(() => {
    wirePack.mockReset();
    wirePack.mockImplementation(async (request) => ({
      plugin: request.plugin,
      agent: request.agent,
      preview: request.preview,
      authority: "calls the model",
      report: { changed: ["Tools"], job, warnings: [], next_call: null },
    }));
    installPack.mockReset();
    installPack.mockResolvedValue(undefined);
  });

  it("with the engine installed, setup offers Enable workflows for the Engineer", async () => {
    const tagged = {
      ...deployment,
      agents: deployment.agents.map((agent) => ({
        ...agent,
        tags: agent.agentId === "ops" ? [ENGINEER_AGENT_TAG] : [],
      })),
    };
    const before = JSON.stringify(engineerPatches(tagged));
    const user = userEvent.setup();
    show([{ plugin: "gents/workflow", pack: "gents/rhai_workflows", job: true }], tagged);
    await user.click(await screen.findByRole("button", { name: "Enable workflows for the Engineer" }));
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByText("Max spawns: 12");
    expect(installPack).not.toHaveBeenCalled();
    expect(wirePack).toHaveBeenCalledWith({
      plugin: "gents/workflow",
      agent: "ops",
      targetsFrom: [],
      remove: false,
      preview: true,
    });
    expect(JSON.stringify(engineerPatches(tagged))).toBe(before);
  });

  it("without the engine, Install workflows installs with consent, then wires", async () => {
    const user = userEvent.setup();
    installPack
      .mockRejectedValueOnce(new Error("the plugin asks for authority; pass --grant-authority"))
      .mockResolvedValueOnce(undefined);
    show([]);
    await user.click(await screen.findByRole("button", { name: "Install workflows" }));
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByText("Installs gents/rhai_workflows first")).toBeTruthy();
    await user.click(within(dialog).getByRole("button", { name: "Install" }));
    await within(dialog).findByText(/--grant-authority/);
    await user.click(within(dialog).getByRole("button", { name: "Allow" }));
    await waitFor(() =>
      expect(installPack).toHaveBeenLastCalledWith({
        package: "gents/rhai_workflows",
        edited: "refuse",
        grantAuthority: true,
      }),
    );
    await within(dialog).findByText("Max spawns: 12");
    await user.click(within(dialog).getByRole("button", { name: "Enable" }));
    await waitFor(() =>
      expect(wirePack).toHaveBeenLastCalledWith(
        expect.objectContaining({ plugin: "gents/workflow", preview: false }),
      ),
    );
  });

  it("renders nothing when the sources read fails", async () => {
    const listPackWireSources = vi.fn().mockRejectedValue(new Error("offline"));
    renderIn(
      testApp({ api: { listPackWireSources } }),
      <WorkflowsOffer deployment={deployment} />,
    );
    await waitFor(() => expect(listPackWireSources).toHaveBeenCalled());
    await act(async () => {});
    expect(screen.queryByRole("button")).toBeNull();
  });
});
