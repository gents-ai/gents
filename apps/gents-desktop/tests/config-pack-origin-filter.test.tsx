import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

vi.mock("@/lib/router", () => ({
  href: () => "#",
  navigate: vi.fn(),
}));

import { ListDetail } from "../src/ui/screens/agent/ListDetail";

describe("pack entries in configuration lists", () => {
  it("keeps the person's own entries in view and a pack's under its header", async () => {
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

    /* under the person's own entries, a pack's start folded */
    expect(screen.getByText("User coder")).toBeTruthy();
    expect(screen.queryByText("Review scanner")).toBeNull();
    const header = screen.getByRole("button", {
      name: /code_review · 1 from this pack/,
    });
    expect(header).toHaveAttribute("aria-expanded", "false");
    await user.click(header);
    expect(screen.getByText("Review scanner")).toBeTruthy();
    /* the pack's header names it, so its rows don't repeat it */
    expect(screen.queryByText(/pack code_review/)).toBeNull();
  });

  it("searches entries by their words and by their pack's name", () => {
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

    /* a pack's name finds its group */
    fireEvent.change(screen.getByRole("searchbox", { name: "Filter the list" }), {
      target: { value: "reporting" },
    });
    expect(screen.getByText("Review summary")).toBeTruthy();
    expect(screen.queryByText("Review scanner")).toBeNull();
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
    /* folded by default under the person's own entries */
    expect(order().map((t) => t.split(/[ ·]/)[0])).toEqual(["My", "alpha", "beta"]);
    const alpha = screen.getByRole("button", { name: /alpha · 2 from this pack/ });
    expect(alpha).toHaveAttribute("aria-expanded", "false");
    await user.click(alpha);
    expect(order()[2]).toBe("Pack A one");
    expect(order()[3]).toBe("Pack A two");
    expect(screen.queryByText("Pack B one")).toBeNull();

    view.unmount();
    render(list());
    expect(screen.getByText("Pack A one")).toBeTruthy();
    expect(screen.queryByText("Pack B one")).toBeNull();
    vi.unstubAllGlobals();
  });

  it("shows a match inside a folded pack while searching", async () => {
    const user = userEvent.setup();
    const store = new Map([
      ["gents.config.packGroups.skills", JSON.stringify({ alpha: false })],
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

  it("opens the groups when no entries of the person's own show", () => {
    render(
      <ListDetail
        base={{ name: "agent", nodeDid: "did:key:test", section: "tasks" }}
        createLabel="New"
        detail={() => null}
        empty="None."
        rows={[{ id: "t", title: "Pack task", tags: ["gents:pack:alpha"] }]}
      />,
    );
    expect(screen.getByText("Pack task")).toBeTruthy();
  });
});
