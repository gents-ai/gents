import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { PacksPanel } from "../src/ui/screens/agent/PacksPanel";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

const scan = vi.hoisted(() => vi.fn(async (_pack: string) => {}));
vi.mock("../src/ui/contrib/runtime-door", () => ({
  refreshRuntimePackUxPlugins: scan,
}));
vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const listed = vi.fn();

function show(options: { installed?: boolean; fail?: boolean } = {}) {
  const action = vi.fn(async () => {
    if (options.fail) throw new Error("operation failed");
  });
  renderIn(
    testApp({
      api: {
        listInstalledPacks: listed.mockImplementation(async () => ({
          packs: options.installed
            ? [
                {
                  pack: "demo/game",
                  installed: "1.0.0",
                  latest: "2.0.0",
                  outdated: true,
                  error: null,
                },
              ]
            : [],
        })),
        listPackPluginSlots: async () => ({ plugins: [], profiles: [] }),
        listPackWireSources: async () => ({ plugins: [], target_packs: [] }),
        readPackAccount: async () => {
          throw new Error("signed out");
        },
        searchPacks: async () => ({
          packs: [{ namespace: "demo", name: "game", latest: "1.0.0" }],
          has_more: false,
        }),
        installPack: action,
        updatePack: action,
        removePack: action,
      },
    }),
    <PacksPanel deployment={deployment} />,
  );
  return action;
}

describe("pack page discovery", () => {
  beforeEach(() => {
    scan.mockClear();
    listed.mockReset();
  });

  it("scans newly installed pack pages", async () => {
    const action = show();
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Install" }));
    await waitFor(() => expect(action).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(scan).toHaveBeenCalledWith("demo/game"));
  });

  it("reloads existing pages when a pack is updated", async () => {
    const action = show({ installed: true });
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Update" }));
    await waitFor(() => expect(action).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(scan).toHaveBeenCalledWith("demo/game"));
  });

  it("retires removed pages after confirming removal", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const action = show({ installed: true });
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Remove" }));
    await waitFor(() => expect(action).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(scan).toHaveBeenCalledWith("demo/game"));
    confirm.mockRestore();
  });

  it("re-reads the list while a page reload still waits", async () => {
    scan.mockImplementationOnce(() => new Promise<void>(() => {}));
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const action = show({ installed: true });
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Remove" }));
    await waitFor(() => expect(action).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(listed).toHaveBeenCalledTimes(2));
    confirm.mockRestore();
  });

  it("does not rescan after a failed install", async () => {
    const action = show({ fail: true });
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Install" }));
    await waitFor(() => expect(action).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Install" })).toBeEnabled(),
    );
    expect(scan).not.toHaveBeenCalled();
  });
});
