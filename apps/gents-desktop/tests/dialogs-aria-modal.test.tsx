import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import {
  AlertDialog,
  AlertDialogContent,
  AlertDialogTitle,
} from "@gents/ui/components/alert-dialog";
import { Dialog, DialogContent, DialogTitle } from "@gents/ui/components/dialog";

afterEach(cleanup);

/* The app's dialogs rely on the vendored kit to set aria-modal; without it a
   screen reader leaves an open dialog for the page behind it. */
describe("dialogs", () => {
  it("are marked modal by the kit", async () => {
    render(
      <Dialog open>
        <DialogContent>
          <DialogTitle>Customize sidebar</DialogTitle>
        </DialogContent>
      </Dialog>,
    );
    expect((await screen.findByRole("dialog")).getAttribute("aria-modal")).toBe("true");
  });

  it("include alert dialogs", async () => {
    render(
      <AlertDialog open>
        <AlertDialogContent>
          <AlertDialogTitle>Remove this agent?</AlertDialogTitle>
        </AlertDialogContent>
      </AlertDialog>,
    );
    expect((await screen.findByRole("alertdialog")).getAttribute("aria-modal")).toBe(
      "true",
    );
  });
});
