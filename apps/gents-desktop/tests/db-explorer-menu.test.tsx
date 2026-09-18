import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { describe, expect, it, vi } from "vitest";

import { AppShell } from "../src/ui/app/AppShell";

function renderShell(onOpenDbExplorer?: (() => void) | null) {
  return render(
    <TooltipProvider>
      <AppShell
        route={{ name: "agents" }}
        agentName={null}
        agentDid={null}
        deployment={null}
        online
        mailboxCount={0}
        onOpenDbExplorer={onOpenDbExplorer}
      >
        <p>Content</p>
      </AppShell>
    </TooltipProvider>,
  );
}

describe("DB Explorer developer option", () => {
  it("invokes the handler from the settings menu", async () => {
    const openDbExplorer = vi.fn();
    const user = userEvent.setup();
    renderShell(openDbExplorer);

    await user.click(screen.getAllByLabelText("Settings")[0]);
    await user.click(await screen.findByText("DB Explorer"));

    expect(openDbExplorer).toHaveBeenCalledOnce();
  });

  it("hides the developer section without a handler", async () => {
    const user = userEvent.setup();
    renderShell(null);

    await user.click(screen.getAllByLabelText("Settings")[0]);

    expect(await screen.findByText("Theme")).toBeInTheDocument();
    expect(screen.queryByText("DB Explorer")).not.toBeInTheDocument();
  });
});
