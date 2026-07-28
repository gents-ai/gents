import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import {
  OperationsApiProvider,
  WorkspaceTreePanel,
} from "@source-inc/gents-desktop-operations";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

const TREE: Record<string, unknown> = {
  "": {
    root: "/tmp/root",
    subpath: "",
    entries: [
      { name: "src", kind: "dir" },
      { name: "Cargo.toml", kind: "file", size: 812 },
    ],
    truncated: false,
  },
  src: {
    root: "/tmp/root",
    subpath: "src",
    entries: [{ name: "lib.rs", kind: "file", size: 4096 }],
    truncated: false,
  },
};

function withTree(fail = false) {
  return {
    listWorkspace: fail
      ? vi.fn().mockRejectedValue(new Error("this agent has no tool root configured"))
      : vi.fn().mockImplementation(async (subpath?: string | null) => {
          const listing = TREE[subpath ?? ""];
          if (!listing) throw new Error("no such directory");
          return listing;
        }),
  } as unknown as DesktopApiAdapter;
}

function renderTree(api: DesktopApiAdapter) {
  render(
    <OperationsApiProvider api={api}>
      <WorkspaceTreePanel />
    </OperationsApiProvider>,
  );
}

describe("workspace tree", () => {
  it("lists the root and descends into directories lazily", async () => {
    renderTree(withTree());

    await waitFor(() => expect(screen.getByText("Cargo.toml")).toBeInTheDocument());
    expect(screen.getByText("812 B")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("workspace-dir-src"));
    await waitFor(() => expect(screen.getByText("lib.rs")).toBeInTheDocument());
    expect(screen.getByText("4 KiB")).toBeInTheDocument();

    fireEvent.click(screen.getByTestId("workspace-up"));
    await waitFor(() => expect(screen.getByText("Cargo.toml")).toBeInTheDocument());
  });

  it("shows the no-tool-root error honestly", async () => {
    renderTree(withTree(true));
    await waitFor(() =>
      expect(screen.getByTestId("workspace-error")).toHaveTextContent(
        "no tool root configured",
      ),
    );
  });
});
