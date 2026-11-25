import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { describe, expect, it } from "vitest";

import { AppShell } from "../src/ui/app/AppShell";
import type { Route } from "../src/ui/lib/router";
import { renderIn, testApp } from "./app-fixture";

const shell = (route: Route) =>
  renderIn(
    testApp(),
    <TooltipProvider>
      <AppShell route={route}>
        <p>Content</p>
      </AppShell>
    </TooltipProvider>,
  );

/* the test window matches no width query, so it is narrow: its bar has no
   room for the mark, which heads the menu */
const mark = async () => {
  await userEvent.click(screen.getByRole("button", { name: "Menu" }));
  const menu = await screen.findByRole("dialog", { name: "Sidebar" });
  const [first] = within(menu).getAllByRole("link", { name: "Nodes" });
  expect(first).toHaveClass("bg-ink");
  return first!;
};

describe("the Gents mark", () => {
  it("is ringed as the current page on Nodes, where it leads", async () => {
    shell({ name: "agents" });
    const found = await mark();
    expect(found).toHaveAttribute("aria-current", "page");
    expect(found).toHaveClass("ring-2");
  });

  it("is plain elsewhere", async () => {
    shell({ name: "session", sessionId: null });
    const found = await mark();
    expect(found).not.toHaveAttribute("aria-current");
    expect(found).not.toHaveClass("ring-2");
  });
});
