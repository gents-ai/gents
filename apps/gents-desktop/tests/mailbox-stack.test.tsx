import { fireEvent, render, screen } from "@testing-library/react";
import { expect, it, vi } from "vitest";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

import { MailboxStack } from "../src/ui/screens/MailboxStack";
import { node, testApp, withApp } from "./app-fixture";

it("says nothing needs the person when ⌘⇧M finds the mailbox empty", () => {
  const app = testApp({ deployments: [node({ mailboxItems: [] })] });
  render(<MailboxStack route={{ name: "sessions" }} />, { wrapper: withApp(app) });
  fireEvent.keyDown(window, { key: "M", code: "KeyM", ctrlKey: true, shiftKey: true });
  expect(toast).toHaveBeenCalledWith("Nothing needs you right now", {
    id: "mailbox-empty",
  });
  expect(screen.queryByTestId("mailbox-dialog")).toBeNull();
});
