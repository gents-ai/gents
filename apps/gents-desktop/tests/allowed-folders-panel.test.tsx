import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("sonner", () => ({ toast: { error: vi.fn() } }));

import { AllowedFoldersPanel } from "../src/ui/screens/agent/AllowedFoldersPanel";
import { renderIn, testApp } from "./app-fixture";

const list = vi.fn();
const add = vi.fn();
const remove = vi.fn();
/* every folder command answers with the list as it now stands */
const answering = (dirs: { path: string; access: string }[]) => {
  for (const command of [list, add, remove]) command.mockResolvedValue({ dirs });
};
const show = () =>
  renderIn(
    testApp({
      api: {
        listAllowedFolders: list,
        addAllowedFolder: add,
        removeAllowedFolder: remove,
      },
    }),
    <AllowedFoldersPanel />,
  );

describe("AllowedFoldersPanel", () => {
  beforeEach(() => {
    for (const command of [list, add, remove]) command.mockReset();
  });

  it("says none are added and that the working folder is readable", async () => {
    answering([]);
    show();
    await screen.findByText("None added");
    expect(list).toHaveBeenCalled();
    expect(screen.queryByText("Remove")).toBeNull();
  });

  it("toggles the access of a configured folder and removes it", async () => {
    answering([{ path: "/docs", access: "read" }]);
    show();
    fireEvent.click(await screen.findByText("Read only"));
    await waitFor(() => expect(add).toHaveBeenCalledWith("/docs", "read_write"));
    fireEvent.click(await screen.findByText("Remove"));
    await waitFor(() => expect(remove).toHaveBeenCalledWith("/docs"));
  });

  it("adds a typed folder read-only when no picker exists", async () => {
    answering([]);
    show();
    fireEvent.change(await screen.findByLabelText("Folder path"), {
      target: { value: "/data" },
    });
    fireEvent.click(screen.getByText("Add"));
    await waitFor(() => expect(add).toHaveBeenCalledWith("/data", "read"));
  });
});
