import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { PacksPanel } from "../src/ui/screens/agent/PacksPanel";
import { renderIn, testApp } from "./app-fixture";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const bind = vi.fn();

const profiles = [
  {
    profile_id: "chandra",
    display_name: "Chandra",
    model_name: "chandra",
    usable: true,
  },
  { profile_id: "down", display_name: "Down", model_name: "down", usable: false },
];

/* no packs, signed out of the registry, these plugins able to call a model */
function show(plugins: { plugin: string; slot: string; profile: string | null }[]) {
  return renderIn(
    testApp({
      api: {
        listInstalledPacks: async () => ({ packs: [] }),
        readPackAccount: async () => {
          throw new Error("signed out");
        },
        listPackPluginSlots: async () => ({ plugins, profiles }),
        bindPackPlugin: bind,
      },
    }),
    <PacksPanel />,
  );
}

describe("PacksPanel model calls", () => {
  beforeEach(() => {
    bind.mockReset();
    bind.mockResolvedValue(undefined);
  });

  it("shows nothing when no installed plugin can call a model", async () => {
    show([]);
    await screen.findByText("No packs installed");
    expect(screen.queryByText("Model calls")).toBeNull();
  });

  it("leaves a plugin's slot unset and binds it to a usable profile later", async () => {
    const user = userEvent.setup();
    show([{ plugin: "gents/ocr", slot: "remote_ocr", profile: null }]);
    const select = await screen.findByRole("combobox", { name: "gents/ocr" });
    expect(select).toHaveTextContent("Not set");
    await user.click(select);
    expect(screen.queryByRole("option", { name: "Down" })).toBeNull();
    await user.click(await screen.findByRole("option", { name: "Chandra" }));
    await waitFor(() => expect(bind).toHaveBeenCalledWith("gents/ocr", "chandra"));
  });

  it("unbinds a bound slot by choosing Not set", async () => {
    const user = userEvent.setup();
    show([{ plugin: "gents/ocr", slot: "remote_ocr", profile: "chandra" }]);
    const select = await screen.findByRole("combobox", { name: "gents/ocr" });
    expect(select).toHaveTextContent("Chandra");
    await user.click(select);
    await user.click(await screen.findByRole("option", { name: "Not set" }));
    await waitFor(() => expect(bind).toHaveBeenCalledWith("gents/ocr", null));
  });
});
