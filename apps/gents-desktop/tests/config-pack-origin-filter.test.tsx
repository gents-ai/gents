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
        base={{ name: "agent", nodeDid: "did:key:test", section: "skills" }}
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
    /* the pack's header names it, so its rows don't repeat it */
    expect(
      screen.getByRole("button", { name: /code_review · 1 from this pack/ }),
    ).toBeTruthy();
    expect(screen.queryByText(/pack code_review/)).toBeNull();

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
        base={{ name: "agent", nodeDid: "did:key:test", section: "skills" }}
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

  it("lists your own entries first, then each pack's under its folding header", async () => {
    const user = userEvent.setup();
    const store = new Map<string, string>();
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => store.set(key, value),
    });
    const rows = [
      { id: "a", title: "Pack A one", tags: ["gents:pack:alpha"] },
      { id: "mine", title: "My own", tags: [] },
      { id: "b", title: "Pack B one", tags: ["gents:pack:beta"] },
      { id: "a2", title: "Pack A two", tags: ["gents:pack:alpha"] },
    ];
    const list = () => (
      <ListDetail
        base={{ name: "agent", nodeDid: "did:key:test", section: "tools" }}
        createLabel="New"
        detail={() => null}
        empty="None."
        rows={rows}
      />
    );
    const view = render(list());
    const order = () =>
      [...view.container.querySelectorAll("li")].map(
        (li) => li.textContent?.trim() ?? "",
      );
    expect(order().map((t) => t.split(/[ ·]/)[0])).toEqual([
      "My",
      "alpha",
      "Pack",
      "Pack",
      "beta",
      "Pack",
    ]);
    expect(order()[2]).toBe("Pack A one");
    expect(order()[5]).toBe("Pack B one");
    const alpha = screen.getByRole("button", { name: /alpha · 2 from this pack/ });
    expect(alpha).toHaveAttribute("aria-expanded", "true");
    await user.click(alpha);
    expect(alpha).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("Pack A one")).toBeNull();
    expect(screen.getByText("Pack B one")).toBeTruthy();

    view.unmount();
    render(list());
    expect(screen.queryByText("Pack A one")).toBeNull();
    vi.unstubAllGlobals();
  });

  it("shows a match inside a folded pack while searching", async () => {
    const user = userEvent.setup();
    const store = new Map([
      ["gents.config.packFolds.skills", JSON.stringify(["alpha"])],
    ]);
    vi.stubGlobal("localStorage", {
      getItem: (key: string) => store.get(key) ?? null,
      setItem: (key: string, value: string) => store.set(key, value),
    });
    render(
      <ListDetail
        base={{ name: "agent", nodeDid: "did:key:test", section: "skills" }}
        createLabel="New"
        detail={() => null}
        empty="None."
        rows={[
          { id: "needle", title: "Needle", tags: ["gents:pack:alpha"] },
          ...Array.from({ length: 8 }, (_, n) => ({ id: `u${n}`, title: `Mine ${n}` })),
        ]}
      />,
    );
    expect(screen.queryByText("Needle")).toBeNull();
    await user.type(
      screen.getByRole("searchbox", { name: "Filter the list" }),
      "Needle",
    );
    expect(screen.getByText("Needle")).toBeTruthy();
    vi.unstubAllGlobals();
  });
});
