import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { DockSheet } from "../src/ui/app/ShellDock";
import type { ShellLayout } from "../src/ui/app/useShellLayout";
import { renderIn, testApp } from "./app-fixture";

/* an open dock, shown as a sheet until the window is wide enough to dock it */
const layout = (docked: boolean, closeDock: () => void) =>
  ({ docked, dockOpen: true, closeDock }) as unknown as ShellLayout;
const route = { name: "sessions" } as const;

describe("the dock as a bottom sheet", () => {
  it("hands an open dock to the column when the window widens", async () => {
    const closeDock = vi.fn();
    const view = renderIn(
      testApp(),
      <DockSheet route={route} layout={layout(false, closeDock)} />,
    );
    await screen.findByRole("dialog");

    act(() =>
      view.rerender(<DockSheet route={route} layout={layout(true, closeDock)} />),
    );

    expect(closeDock).not.toHaveBeenCalled();
  });

  it("closes the dock when the person dismisses the sheet", async () => {
    const closeDock = vi.fn();
    renderIn(testApp(), <DockSheet route={route} layout={layout(false, closeDock)} />);
    await screen.findByRole("dialog");

    await userEvent.keyboard("{Escape}");

    await waitFor(() => expect(closeDock).toHaveBeenCalledOnce());
  });
});
