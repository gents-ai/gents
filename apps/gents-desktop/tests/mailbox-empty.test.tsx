import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { MailboxScreen } from "../src/ui/screens/MailboxScreen";
import { testApp, withApp } from "./app-fixture";

describe("empty mailbox", () => {
  it("says what arrives here and offers a session as the way to start new work", () => {
    render(<MailboxScreen />, { wrapper: withApp(testApp()) });
    expect(screen.getByText("Nothing needs your attention")).toBeVisible();
    expect(
      screen.getByText(/When an agent has a question, needs your approval/),
    ).toBeVisible();
    expect(screen.getByText("Start a session").closest("a")).toHaveAttribute("href");
    expect(screen.queryByText("New session")).not.toBeInTheDocument();
  });
});
