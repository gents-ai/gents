import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { SettingsScreen } from "../src/ui/screens/settings/SettingsScreen";
import { renderIn, testApp } from "./app-fixture";

function renderDeveloper(openDbExplorer?: () => Promise<string>) {
  return renderIn(
    testApp({ api: openDbExplorer ? { openDbExplorer } : {} }),
    <SettingsScreen section="developer" />,
  );
}

describe("DB Explorer developer option", () => {
  it("invokes the handler from the developer settings", async () => {
    const openDbExplorer = vi.fn(() => Promise.resolve(""));
    const user = userEvent.setup();
    renderDeveloper(openDbExplorer);

    expect(screen.getByText("DB Explorer")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Open" }));

    expect(openDbExplorer).toHaveBeenCalledOnce();
  });

  it("hides the option without a handler", () => {
    renderDeveloper();

    expect(screen.getByTestId("settings-screen").dataset.section).toBe("developer");
    expect(screen.queryByText("DB Explorer")).not.toBeInTheDocument();
  });
});
