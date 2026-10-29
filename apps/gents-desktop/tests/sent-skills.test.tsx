import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { SentText } from "../src/ui/screens/SkillChips";

vi.mock("@/hooks/useClient", () => ({
  useSelectedNode: () => ({
    skills: [
      { skillId: "review", displayName: "Review", description: "Looks it over" },
    ],
  }),
}));

vi.mock("../src/ui/hooks/useFleet", () => ({
  useFleet: (select: (state: unknown) => unknown) =>
    select({ sessions: {}, nodes: {} }),
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

  it("sit beside the message's session references, each drawn as its own chip", () => {
    const { container } = render(
      <SentText
        content="/review compare with [Lexer work](gents:session/s-1)"
        skills={["review"]}
      />,
    );
    expect(chips()).toEqual(["/review"]);
    expect(
      [...document.querySelectorAll('[data-slot="session-chip"]')].map(
        (chip) => chip.textContent,
      ),
    ).toEqual(["Lexer work"]);
    expect(container.textContent).toBe("/review compare with Lexer work");
  });

  it("leave a message without selected skills as plain text, slash words and all", () => {
    render(<SentText content="see /review and /etc" skills={[]} />);
    expect(chips()).toEqual([]);
    expect(screen.getByText("see /review and /etc")).toBeInTheDocument();
  });
});
