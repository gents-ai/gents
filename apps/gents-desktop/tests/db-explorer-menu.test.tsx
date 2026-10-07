import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { describe, expect, it, vi } from "vitest";

import { AppShell } from "../src/ui/app/AppShell";
import { renderIn, testApp } from "./app-fixture";

/* the window is narrow under test: settings sit in the menu's nav sheet */
function renderShell(openDbExplorer?: () => Promise<string>) {
  return renderIn(
    testApp({ api: openDbExplorer ? { openDbExplorer } : {} }),
    <TooltipProvider>
      <AppShell route={{ name: "agents" }}>
        <p>Content</p>
      </AppShell>
    </TooltipProvider>,
  );
}

describe("DB Explorer developer option", () => {
  it("invokes the handler from the settings menu", async () => {
    const openDbExplorer = vi.fn(() => Promise.resolve(""));
    const user = userEvent.setup();
    renderShell(openDbExplorer);

    await user.click(screen.getByLabelText("Menu"));
    await user.click(await screen.findByLabelText("Settings"));
    await user.click(await screen.findByText("DB Explorer"));

    expect(openDbExplorer).toHaveBeenCalledOnce();
  });

  it("hides the developer section without a handler", async () => {
    const user = userEvent.setup();
    renderShell();

    await user.click(screen.getByLabelText("Menu"));
    await user.click(await screen.findByLabelText("Settings"));

    expect(await screen.findByText("Theme")).toBeInTheDocument();
    expect(screen.queryByText("DB Explorer")).not.toBeInTheDocument();
  });
});
