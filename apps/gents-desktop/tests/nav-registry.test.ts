/* The nav registry holds the rows a UX plugin contributes; the app's own
   rows (New session, Mailbox, Sessions, Nodes) are placed by hand in Rail
   and NavPanel, so the registry starts empty and the contract under test
   is ordering, replacement and disposal. */
import { afterEach, describe, expect, it } from "vitest";

import { navItems, registerNavItem, type NavItem } from "@/app/navRegistry";
import type { Route } from "@/lib/router";

const disposers: Array<() => void> = [];
afterEach(() => {
  disposers.splice(0).forEach((dispose) => dispose());
});

function byId(id: string): NavItem {
  const item = navItems().find((item) => item.id === id);
  if (!item) throw new Error(`no nav item ${id}`);
  return item;
}

const mailbox: Route = { name: "mailbox" };
const agent: Route = { name: "agent", agentDid: "did:x", section: "board:page" };

const make = (id: string, extra: Partial<NavItem> = {}): NavItem => ({
  id,
  label: id,
  icon: null,
  to: mailbox,
  active: () => false,
  placement: "primary",
  ...extra,
});

describe("nav registry", () => {
  it("starts empty: the app's own rows are not registry rows", () => {
    expect(navItems()).toEqual([]);
  });

  it("orders by `order`, then registration, and disposes cleanly", () => {
    disposers.push(registerNavItem(make("z", { order: 15 })));
    disposers.push(registerNavItem(make("a", { order: 15 })));
    disposers.push(registerNavItem(make("first", { order: -1 })));

    const ids = navItems().map((item) => item.id);
    expect(ids).toEqual(["first", "z", "a"]);
    expect(navItems()).toBe(navItems());

    disposers.splice(0).forEach((dispose) => dispose());
    expect(navItems()).toEqual([]);
  });

  it("keeps a row's active predicate, badge and placement", () => {
    disposers.push(
      registerNavItem(
        make("board", {
          to: agent,
          active: (r) => r.name === "agent" && r.section === "board:page",
          count: (ctx) => ctx.mailboxCount,
          placement: "footer",
        }),
      ),
    );
    const row = byId("board");
    expect(row.active(agent)).toBe(true);
    expect(row.active(mailbox)).toBe(false);
    expect(row.count?.({ mailboxCount: 3 })).toBe(3);
    expect(row.placement).toBe("footer");
  });

  it("replaces on re-register and a stale disposer is inert", () => {
    const original = make("same", { label: "One" });
    const dispose1 = registerNavItem(original);
    expect(byId("same").label).toBe("One");
    const dispose2 = registerNavItem({ ...original, label: "Two" });
    dispose1();
    expect(byId("same").label).toBe("Two");
    dispose2();
    expect(navItems().find((item) => item.id === "same")).toBeUndefined();
  });

  it("drops a malformed payload rather than crashing the rail", () => {
    disposers.push(registerNavItem(make("ok")));
    disposers.push(registerNavItem({ id: "broken", label: "x" } as unknown as NavItem));
    expect(navItems().map((item) => item.id)).toEqual(["ok"]);
  });
});
