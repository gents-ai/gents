import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import type { Shell } from "@/hooks/useShell";
import { MailboxScreen } from "../src/ui/screens/MailboxScreen";

describe("empty mailbox", () => {
  it("says what arrives here and offers a session as the way to start new work", () => {
    const shell = { selectedDeployment: { mailboxItems: [] } } as unknown as Shell;
    render(<MailboxScreen shell={shell} />);
    expect(screen.getByText("Nothing needs your attention")).toBeVisible();
    expect(
      screen.getByText(/When an agent has a question, needs your approval/),
    ).toBeVisible();
    expect(screen.getByText("Start a session").closest("a")).toHaveAttribute("href");
    expect(screen.queryByText("New session")).not.toBeInTheDocument();
  });
});
