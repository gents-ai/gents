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

const sessions: Route = { name: "sessions" };
const newSession: Route = { name: "session", sessionId: null };
const openSession: Route = { name: "session", sessionId: "s1" };
const mailbox: Route = { name: "mailbox" };
const agents: Route = { name: "agents" };
const agent: Route = { name: "agent", agentDid: "did:x", section: "agent" };

describe("nav registry", () => {
  it("lights each built-in row for exactly its own routes", () => {
    const routes = [sessions, newSession, openSession, mailbox, agents, agent];
    const litBy = (id: string) => routes.filter((r) => byId(id).active(r));

    expect(litBy("new-session")).toEqual([newSession]);
    expect(litBy("mailbox")).toEqual([mailbox]);
    expect(litBy("sessions")).toEqual([sessions, openSession]);
    expect(litBy("agents")).toEqual([agents]);
  });

  it("puts the app's rows on the rail and Agents at the foot", () => {
    const placement = (id: string) => byId(id).placement;
    expect(["new-session", "mailbox", "sessions"].map(placement)).toEqual([
      "primary",
      "primary",
      "primary",
    ]);
    expect(placement("agents")).toBe("footer");
  });

  it("badges only the mailbox, from the context", () => {
    const ctx = { mailboxCount: 3 };
    expect(byId("mailbox").count?.(ctx)).toBe(3);
    expect(byId("sessions").count).toBeUndefined();
  });

  it("orders by `order`, then registration, and disposes cleanly", () => {
    const before = navItems();
    const make = (id: string, order?: number): NavItem => ({
      id,
      label: id,
      icon: null,
      to: mailbox,
      active: () => false,
      placement: "primary",
      order,
    });
    disposers.push(registerNavItem(make("z", 15)));
    disposers.push(registerNavItem(make("a", 15)));
    disposers.push(registerNavItem(make("first", -1)));

    const ids = navItems().map((item) => item.id);
    expect(ids[0]).toBe("first");
    expect(ids.indexOf("new-session")).toBeLessThan(ids.indexOf("z"));
    expect(ids.indexOf("z")).toBeLessThan(ids.indexOf("a"));
    expect(ids.indexOf("a")).toBeLessThan(ids.indexOf("mailbox"));
    expect(navItems()).toBe(navItems());

    disposers.splice(0).forEach((dispose) => dispose());
    expect(navItems().map((item) => item.id)).toEqual(before.map((item) => item.id));
  });

  it("replaces on re-register and a stale disposer is inert", () => {
    const original = byId("mailbox");
    const dispose1 = registerNavItem({ ...original, label: "Inbox" });
    expect(byId("mailbox").label).toBe("Inbox");
    const dispose2 = registerNavItem({ ...original, label: "Letters" });
    dispose1();
    expect(byId("mailbox").label).toBe("Letters");
    dispose2();
    expect(navItems().find((item) => item.id === "mailbox")).toBeUndefined();
    registerNavItem(original);
    expect(byId("mailbox")).toBe(original);
  });
});
