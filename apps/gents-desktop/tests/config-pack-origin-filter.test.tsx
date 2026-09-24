import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/lib/router", () => ({
  href: () => "#",
  navigate: vi.fn(),
}));

import { ListDetail } from "../src/ui/screens/agent/ListDetail";

describe("pack origin configuration filters", () => {
  it("facets pack rows without hiding user rows from All", async () => {
    const user = userEvent.setup();
    render(
      <ListDetail
        base={{ name: "agent", agentDid: "did:key:test", section: "skills" }}
        createLabel="New skill"
        detail={() => null}
        empty="No skills."
        rows={[
          {
            id: "review-scan",
            title: "Review scanner",
            meta: "enabled",
            tags: ["review", "gents:pack:code_review"],
          },
          { id: "user-coder", title: "User coder", meta: "enabled", tags: ["coding"] },
        ]}
      />,
    );

    expect(screen.getByText("Review scanner")).toBeTruthy();
    expect(screen.getByText("User coder")).toBeTruthy();
    expect(screen.getByText("enabled · pack code_review")).toBeTruthy();

    await user.click(screen.getByTestId("skills-origin-filter"));
    await user.click(await screen.findByRole("option", { name: "Pack code_review" }));
    expect(screen.getByText("Review scanner")).toBeTruthy();
    expect(screen.queryByText("User coder")).toBeNull();

    await user.click(screen.getByTestId("skills-origin-filter"));
    await user.click(await screen.findByRole("option", { name: "Not from a pack" }));
    expect(screen.queryByText("Review scanner")).toBeNull();
    expect(screen.getByText("User coder")).toBeTruthy();
  });

  it("composes text and provenance filters on long lists", async () => {
    const user = userEvent.setup();
    render(
      <ListDetail
        base={{ name: "agent", agentDid: "did:key:test", section: "skills" }}
        createLabel="New skill"
        detail={() => null}
        empty="No skills."
        rows={[
          {
            id: "review-scan",
            title: "Review scanner",
            meta: "enabled",
            tags: ["gents:pack:code_review"],
          },
          {
            id: "review-summary",
            title: "Review summary",
            meta: "enabled",
            tags: ["gents:pack:reporting"],
          },
          ...Array.from({ length: 7 }, (_, index) => ({
            id: `user-${index}`,
            title: `User skill ${index}`,
            meta: "enabled",
            tags: ["custom"],
          })),
        ]}
      />,
    );

    fireEvent.change(screen.getByRole("searchbox", { name: "Filter the list" }), {
      target: { value: "Review" },
    });
    expect(screen.getByText("Review scanner")).toBeTruthy();
    expect(screen.getByText("Review summary")).toBeTruthy();
    expect(screen.queryByText("User skill 0")).toBeNull();

    await user.click(screen.getByTestId("skills-origin-filter"));
    await user.click(await screen.findByRole("option", { name: "Pack code_review" }));
    expect(screen.getByText("Review scanner")).toBeTruthy();
    expect(screen.queryByText("Review summary")).toBeNull();
    expect(screen.getByText("1 of 9")).toBeTruthy();
  });
});
