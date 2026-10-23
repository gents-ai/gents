import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { PacksPanel } from "../src/ui/screens/agent/PacksPanel";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const wirePack = vi.fn();
const installPack = vi.fn();

const job = {
  agent_targets: ["fixture/worker", "fixture/reviewer"],
  max_live: 3,
  max_spawns: 12,
  max_messages_per_slot: 4,
  result_bytes: 4096,
  max_wall_secs: 600,
  policy: {
    dynamic: "check",
    dynamic_targets: ["fixture/dynamic_worker"],
    trusted_packs: ["fixture/trusted"],
  },
};

const wired = (request: { plugin: string; agent: string; preview: boolean }) => ({
  plugin: request.plugin,
  agent: request.agent,
  preview: request.preview,
  authority: "calls the model and reads files",
  report: { changed: ["Tools"], job, warnings: [], next_call: null },
});

function show(
  installed: string[],
  sources: {
    plugins: { plugin: string; pack: string | null; job: boolean }[];
    target_packs: { pack: string; targets: string[] }[];
  },
) {
  return renderIn(
    testApp({
      api: {
        listInstalledPacks: async () => ({
          packs: installed.map((pack) => ({
            pack,
            installed: "1.0.0",
            latest: null,
            outdated: false,
          })),
        }),
        readPackAccount: async () => {
          throw new Error("signed out");
        },
        listPackPluginSlots: async () => ({ plugins: [], profiles: [] }),
        listPackWireSources: async () => sources,
        wirePack,
        installPack,
      },
    }),
    <PacksPanel deployment={deployment} />,
  );
}

const jobPack = {
  plugins: [{ plugin: "fixture/job_fixture", pack: "fixture/job_pack", job: true }],
  target_packs: [],
};

describe("PacksPanel Enable for agent", () => {
  beforeEach(() => {
    wirePack.mockReset();
    wirePack.mockImplementation(async (request) => wired(request));
    installPack.mockReset();
    installPack.mockResolvedValue(undefined);
  });

  it("offers Enable for agent on a pack that ships a plugin", async () => {
    show(["fixture/job_pack", "fixture/plain_pack"], jobPack);
    expect(
      await screen.findAllByRole("button", { name: "Enable for agent" }),
    ).toHaveLength(1);
    expect(wirePack).not.toHaveBeenCalled();
  });

  it("the consent dialog shows the previewed job block before wiring", async () => {
    const user = userEvent.setup();
    show(["fixture/job_pack"], jobPack);
    await user.click(await screen.findByRole("button", { name: "Enable for agent" }));
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByText("Max spawns: 12");
    expect(
      within(dialog).getByText("Targets: fixture/worker, fixture/reviewer"),
    ).toBeTruthy();
    expect(within(dialog).getByText("Max live: 3")).toBeTruthy();
    expect(within(dialog).getByText("Dynamic scripts: check")).toBeTruthy();
    expect(
      within(dialog).getByText("Dynamic targets: fixture/dynamic_worker"),
    ).toBeTruthy();
    expect(within(dialog).getByText("Trusted packs: fixture/trusted")).toBeTruthy();
    expect(within(dialog).getByText("Project roots: /work/project")).toBeTruthy();
    expect(
      within(dialog).getByText("Authority: calls the model and reads files"),
    ).toBeTruthy();
    expect(wirePack).toHaveBeenCalledTimes(1);
    expect(wirePack).toHaveBeenCalledWith({
      plugin: "fixture/job_fixture",
      agent: "default",
      targetsFrom: [],
      remove: false,
      preview: true,
    });
  });

  it("confirming wires the chosen agent", async () => {
    const user = userEvent.setup();
    show(["fixture/job_pack"], jobPack);
    await user.click(await screen.findByRole("button", { name: "Enable for agent" }));
    const dialog = await screen.findByRole("dialog");
    await user.click(within(dialog).getByRole("combobox"));
    await user.click(await screen.findByRole("option", { name: "Ops" }));
    await waitFor(() =>
      expect(wirePack).toHaveBeenLastCalledWith(
        expect.objectContaining({ agent: "ops", preview: true }),
      ),
    );
    await within(dialog).findByText("Max spawns: 12");
    await user.click(within(dialog).getByRole("button", { name: "Enable" }));
    await waitFor(() =>
      expect(wirePack).toHaveBeenLastCalledWith({
        plugin: "fixture/job_fixture",
        agent: "ops",
        targetsFrom: [],
        remove: false,
        preview: false,
      }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("a preview without a job block shows the next call", async () => {
    const user = userEvent.setup();
    wirePack.mockImplementation(async (request) => ({
      ...wired(request),
      report: {
        changed: [],
        job: null,
        warnings: [],
        next_call:
          "gents pack wire fixture/job_fixture --agent default --targets-from <pack>",
      },
    }));
    show(["fixture/job_pack"], jobPack);
    await user.click(await screen.findByRole("button", { name: "Enable for agent" }));
    const dialog = await screen.findByRole("dialog");
    await within(dialog).findByText(/--targets-from <pack>/);
    expect(within(dialog).queryByText(/Max spawns/)).toBeNull();
  });
});
