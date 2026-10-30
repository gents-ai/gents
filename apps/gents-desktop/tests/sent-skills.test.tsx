import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { PendingTray } from "../src/ui/screens/PendingTray";
import { SentText } from "../src/ui/screens/SkillChips";

vi.mock("@/hooks/useClient", () => ({
  useSelectedNode: () => ({
    skills: [
      { skillId: "review", displayName: "Review", description: "Looks it over" },
    ],
  }),
}));

const chips = () =>
  [...document.querySelectorAll('[data-slot="command-chip"]')].map(
    (chip) => chip.textContent,
  );

describe("a sent message's skills", () => {
  it("are drawn before the text when gents took them from the opening lines", () => {
    const { container } = render(<SentText content="fix it" skills={["review"]} />);
    expect(chips()).toEqual(["/review"]);
    expect(container.textContent).toBe("/review fix it");
  });

  it("are drawn where the text names them, and not again before it", () => {
    const { container } = render(
      <SentText content="please /review this" skills={["review"]} />,
    );
    expect(chips()).toEqual(["/review"]);
    expect(container.textContent).toBe("please /review this");
  });

  it("are drawn where the text names them even before punctuation", () => {
    const { container } = render(
      <SentText content="use /review, then ship" skills={["review"]} />,
    );
    expect(chips()).toEqual(["/review"]);
    expect(container.textContent).toBe("use /review, then ship");
  });

  it("leave a message without selected skills as plain text, slash words and all", () => {
    render(<SentText content="see /review and /etc" skills={[]} />);
    expect(chips()).toEqual([]);
    expect(screen.getByText("see /review and /etc")).toBeInTheDocument();
  });
});

describe("a queued message's skills", () => {
  it("show as chips in the tray, as they will once it is sent", () => {
    const entry = {
      requestDocId: "doc-q",
      requestId: "q",
      content: "fix it",
      selectedSkillIds: ["review"],
      editable: true,
      steerable: true,
      editGroup: "same",
    };
    render(
      <PendingTray
        queued={[
          {
            requestId: "q",
            content: "fix it",
            selectedSkillIds: ["review"],
            lifecycleState: "pending",
            foldedIntoRequestId: null,
            origin: null,
            createdAt: "2026-10-30T00:00:00Z",
          },
        ]}
        queue={[entry]}
        onEdit={vi.fn()}
      />,
    );
    expect(chips()).toEqual(["/review"]);
    expect(screen.getByText("fix it")).toBeTruthy();
  });
});
