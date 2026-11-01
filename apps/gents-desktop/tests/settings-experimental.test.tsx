import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { FLAGS } from "../src/ui/lib/flags";
import { flag } from "../src/ui/preferences";
import { SettingsScreen } from "../src/ui/screens/settings/SettingsScreen";
import { SETTINGS_SECTIONS } from "../src/ui/screens/settings/sections";
import { renderIn, testApp, withFlags } from "./app-fixture";

const DEMO = FLAGS.find((f) => f.id === "demo-feature")!;

const openSection = () =>
  renderIn(testApp(), <SettingsScreen section="experimental" />);

async function choose(user: ReturnType<typeof userEvent.setup>, option: string) {
  await user.click(screen.getByRole("combobox", { name: DEMO.name }));
  await user.click(await screen.findByRole("option", { name: option }));
}

describe("the experimental features settings", () => {
  it("sits last in the settings, as its own section", () => {
    expect(SETTINGS_SECTIONS[SETTINGS_SECTIONS.length - 1]!.id).toBe("experimental");
    openSection();
    expect(screen.getByTestId("settings-screen").dataset.section).toBe("experimental");
    expect(
      screen.getByRole("heading", { name: "Experimental features" }),
    ).toBeVisible();
    expect(
      screen.getByText(
        "Try features that aren’t finished. They may change or go away.",
      ),
    ).toBeVisible();
  });

  it("shows a row per flag with its description and its default", () => {
    openSection();
    for (const f of FLAGS) {
      const control = screen.getByRole("combobox", { name: f.name });
      expect(control).toHaveTextContent(`Default (${f.default ? "on" : "off"})`);
      expect(screen.getByText(f.description, { exact: false })).toBeVisible();
    }
  });

  it("switches a flag on, off and back to its default", async () => {
    const user = userEvent.setup();
    openSection();
    await choose(user, "Enabled");
    expect(flag("demo-feature")).toBe(true);
    expect(screen.getByRole("combobox", { name: DEMO.name })).toHaveTextContent(
      "Enabled",
    );
    await choose(user, "Disabled");
    expect(flag("demo-feature")).toBe(false);
    expect(screen.getByRole("combobox", { name: DEMO.name })).toHaveTextContent(
      "Disabled",
    );
    await choose(user, "Default (off)");
    expect(flag("demo-feature")).toBe(DEMO.default);
  });

  it("puts every flag back to its default with Reset all", async () => {
    const user = userEvent.setup();
    withFlags({ "demo-feature": true });
    openSection();
    const reset = screen.getByRole("button", { name: "Reset all" });
    expect(reset).toBeEnabled();
    await user.click(reset);
    expect(flag("demo-feature")).toBe(false);
    expect(screen.getByRole("combobox", { name: DEMO.name })).toHaveTextContent(
      "Default (off)",
    );
    expect(reset).toBeDisabled();
  });

  it("offers a relaunch once a flag that applies at launch changes", async () => {
    expect(DEMO.restart).toBe(true);
    const reload = vi.fn();
    const original = window.location;
    Object.defineProperty(window, "location", {
      configurable: true,
      value: { ...original, reload },
    });
    try {
      const user = userEvent.setup();
      openSection();
      expect(
        screen.getByText("Takes effect after relaunch.", { exact: false }),
      ).toBeVisible();
      expect(screen.queryByRole("button", { name: "Relaunch" })).toBeNull();
      await choose(user, "Enabled");
      await user.click(screen.getByRole("button", { name: "Relaunch" }));
      expect(reload).toHaveBeenCalledTimes(1);
      await choose(user, "Default (off)");
      expect(screen.queryByRole("button", { name: "Relaunch" })).toBeNull();
    } finally {
      Object.defineProperty(window, "location", {
        configurable: true,
        value: original,
      });
    }
  });

  it("asks for no relaunch for what the app launched with", () => {
    withFlags({ "demo-feature": true });
    openSection();
    expect(screen.queryByRole("button", { name: "Relaunch" })).toBeNull();
    const row = screen.getByRole("combobox", { name: DEMO.name });
    expect(within(row).getByText("Enabled")).toBeVisible();
  });
});
