import { screen, waitFor, within } from "@testing-library/react";
import { expect } from "vitest";

import type { LiveDesktopDriver } from "./harness";

export async function openOperationsDrawer(driver: LiveDesktopDriver) {
  await driver.user.click(
    screen.getByRole("button", { name: /open operations drawer/i }),
  );
  expect(screen.getByRole("heading", { name: "Operations" })).toBeInTheDocument();
  expect(screen.getByRole("tablist", { name: "Operations" })).toBeInTheDocument();
}

export async function closeOperationsDrawer(driver: LiveDesktopDriver) {
  await driver.user.click(
    screen.getByRole("button", { name: /close operations drawer/i }),
  );
  await waitFor(() => {
    expect(
      screen.getByRole("button", { name: /open operations drawer/i }),
    ).toBeInTheDocument();
  });
}

export async function exerciseOperationsDrawerTabs(driver: LiveDesktopDriver) {
  await openOperationsDrawer(driver);

  const backgroundPanel = await expectOperationsPanel("background-tools");
  expect(backgroundPanel).toHaveTextContent(/parent/i);
  expect(backgroundPanel).toHaveTextContent(/live/i);
  expectNoBridgeError(backgroundPanel);

  await driver.user.click(screen.getByRole("tab", { name: "Lineage" }));
  const lineagePanel = await expectOperationsPanel("lineage");
  expect(
    within(lineagePanel).getByRole("heading", { name: "Lineage" }),
  ).toBeInTheDocument();
  expect(lineagePanel).toHaveTextContent(
    /no active subagent dispatches|subagent lineage|loading lineage/i,
  );
  expectNoBridgeError(lineagePanel);

  await driver.user.click(screen.getByRole("tab", { name: "Backends" }));
  const backendPanel = await expectOperationsPanel("backend-health");
  expect(
    within(backendPanel).getByRole("heading", { name: "Backend health" }),
  ).toBeInTheDocument();
  expect(backendPanel).toHaveTextContent(/registered|backends/i);
  expectNoBridgeError(backendPanel);

  await driver.user.click(screen.getByRole("tab", { name: "MCP health" }));
  const mcpPanel = await expectOperationsPanel("mcp-health");
  expect(
    within(mcpPanel).getByRole("heading", { name: "MCP services / health" }),
  ).toBeInTheDocument();
  expect(mcpPanel).toHaveTextContent(/no mcp services registered|healthy|all/i);
  expectNoBridgeError(mcpPanel);
}

export async function expectOperationsPanel(tabId: string) {
  const panelId = `operations-rail-panel-${tabId}`;
  await waitFor(() => {
    expect(document.getElementById(panelId)).toBeInTheDocument();
  });
  return document.getElementById(panelId)!;
}

export function expectNoBridgeError(panel: HTMLElement) {
  expect(within(panel).queryByRole("alert")).not.toBeInTheDocument();
  expect(panel).not.toHaveTextContent(
    /bridge unavailable|desktop bridge|failed to load|not initialized|not running|fetch failed/i,
  );
}
