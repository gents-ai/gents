import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ConfigDocumentList } from "../src/components/config/ConfigChrome";

describe("pack origin configuration filters", () => {
  it("facets pack rows without hiding user rows from All", () => {
    render(
      <ConfigDocumentList
        eyebrow="Behaviors"
        title="Agent behaviors"
        testPrefix="behavior"
        selectedId={null}
        onSelect={vi.fn()}
        items={[
          {
            id: "review-scan",
            title: "Review scanner",
            meta: "enabled",
            tags: ["review", "gents:pack:code_review"],
          },
          {
            id: "user-coder",
            title: "User coder",
            meta: "enabled",
            tags: ["coding"],
          },
        ]}
      />,
    );

    expect(screen.getByTestId("config-behavior-review-scan")).toBeTruthy();
    expect(screen.getByTestId("config-behavior-user-coder")).toBeTruthy();
    expect(screen.getByText("enabled · pack:code_review")).toBeTruthy();

    fireEvent.change(screen.getByTestId("behavior-origin-filter"), {
      target: { value: "code_review" },
    });
    expect(screen.getByTestId("config-behavior-review-scan")).toBeTruthy();
    expect(screen.queryByTestId("config-behavior-user-coder")).toBeNull();

    fireEvent.change(screen.getByTestId("behavior-origin-filter"), {
      target: { value: "__not_from_pack__" },
    });
    expect(screen.queryByTestId("config-behavior-review-scan")).toBeNull();
    expect(screen.getByTestId("config-behavior-user-coder")).toBeTruthy();

    fireEvent.change(screen.getByTestId("behavior-origin-filter"), {
      target: { value: "all" },
    });
    expect(screen.getByTestId("config-behavior-review-scan")).toBeTruthy();
    expect(screen.getByTestId("config-behavior-user-coder")).toBeTruthy();
  });
});
