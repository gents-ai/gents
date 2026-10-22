import { screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SettingsScreen } from "../src/ui/screens/settings/SettingsScreen";
import {
  DEFAULT_SETTINGS_SECTION,
  SETTINGS_SECTIONS,
} from "../src/ui/screens/settings/sections";
import { renderIn, testApp } from "./app-fixture";

describe("the app's settings screen", () => {
  it("lists each section once, the general one first", () => {
    const ids = SETTINGS_SECTIONS.map((s) => s.id);
    expect(new Set(ids).size).toBe(ids.length);
    expect(DEFAULT_SETTINGS_SECTION).toBe("general");
  });

  it("opens on the general section without one named, and on the one named", () => {
    const { unmount } = renderIn(testApp(), <SettingsScreen />);
    expect(screen.getByTestId("settings-screen").dataset.section).toBe("general");
    expect(screen.getByRole("combobox", { name: "Theme" })).toBeVisible();
    unmount();
    renderIn(testApp(), <SettingsScreen section="developer" />);
    expect(screen.getByTestId("settings-screen").dataset.section).toBe("developer");
  });

  it("falls back to the general section for a section it does not have", () => {
    renderIn(testApp(), <SettingsScreen section="nothing" />);
    expect(screen.getByTestId("settings-screen").dataset.section).toBe("general");
  });
});
