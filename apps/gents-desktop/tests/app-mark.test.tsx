import { screen } from "@testing-library/react";
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

/* the mark in the window bar, as wide and narrow layouts each place one */
const marks = () => {
  const found = screen
    .getAllByRole("link", { name: "Nodes" })
    .filter((a) => a.closest("header"));
  expect(found.length).toBeGreaterThan(0);
  return found;
};

describe("the Gents mark", () => {
  it("is ringed as the current page on Nodes, where it leads", () => {
    shell({ name: "agents" });
    for (const mark of marks()) {
      expect(mark).toHaveAttribute("aria-current", "page");
      expect(mark).toHaveClass("ring-2");
    }
  });

  it("is plain elsewhere", () => {
    shell({ name: "session", sessionId: null });
    for (const mark of marks()) {
      expect(mark).not.toHaveAttribute("aria-current");
      expect(mark).not.toHaveClass("ring-2");
    }
  });
});
