import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/lib/router", () => ({
  href: () => "#",
  navigate: vi.fn(),
}));

import { ListDetail } from "../src/ui/screens/agent/ListDetail";

describe("pack origin configuration filters", () => {
  it("facets pack rows without hiding user rows from All", () => {
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
    expect(screen.getByText("enabled · pack:code_review")).toBeTruthy();

    fireEvent.change(screen.getByTestId("skills-origin-filter"), {
      target: { value: "code_review" },
    });
    expect(screen.getByText("Review scanner")).toBeTruthy();
    expect(screen.queryByText("User coder")).toBeNull();

    fireEvent.change(screen.getByTestId("skills-origin-filter"), {
      target: { value: "__not_from_pack__" },
    });
    expect(screen.queryByText("Review scanner")).toBeNull();
    expect(screen.getByText("User coder")).toBeTruthy();
  });
});
