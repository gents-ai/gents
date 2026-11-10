import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, describe, expect, it } from "vitest";
import { ChipsRow, RefRow, type Choice } from "../src/ui/screens/agent/editors";
import { renderIn, testApp } from "./app-fixture";

const items: Choice[] = [
  { value: "worker", label: "DemoResearch worker", pack: "demo_research" },
  { value: "engineer", label: "The Engineer" },
  { value: "skills", label: "DemoSkills worker", pack: "demo_skills" },
  { value: "lister", label: "Lister" },
  { value: "reviewer", label: "Reviewer" },
  { value: "writer", label: "Writer" },
  { value: "planner", label: "Planner" },
  { value: "editor", label: "Editor" },
];

function Field({ initial }: { initial: string }) {
  const [value, setValue] = useState(initial);
  return (
    <RefRow
      id="default"
      label="Default agent"
      value={value}
      onChange={setValue}
      items={items}
      createLabel="New agent…"
    />
  );
}

const open = async (initial = "engineer") => {
  const user = userEvent.setup();
  renderIn(testApp(), <Field initial={initial} />);
  await user.click(screen.getByRole("combobox", { name: "Default agent" }));
  return { user, list: await screen.findByRole("listbox") };
};

const names = (list: HTMLElement) =>
  within(list)
    .getAllByRole("option")
    .map((o) => o.textContent);

const own = ["The Engineer", "Lister", "Reviewer", "Writer", "Planner", "Editor"];

/* a pack's folding is remembered per field */
beforeEach(() => {
  try {
    localStorage.clear();
  } catch {
    /* no storage: nothing remembered */
  }
});

describe("a reference field", () => {
  it("lists the person's own documents first, each pack folded under its name", async () => {
    const { list } = await open();
    expect(names(list)).toEqual(own);
    const heading = within(list).getByRole("button", { name: /demo_research · 1/ });
    expect(heading).toHaveAttribute("aria-expanded", "false");
    expect(within(list).getByRole("button", { name: /demo_skills · 1/ })).toBeVisible();
  });

  it("opens a pack's documents from its heading", async () => {
    const { user, list } = await open();
    await user.click(within(list).getByRole("button", { name: /demo_research/ }));
    expect(names(list)).toEqual([...own, "DemoResearch worker"]);
    const pack = within(list).getByRole("group", { name: "demo_research" });
    expect(within(pack).getByRole("option")).toHaveTextContent("DemoResearch worker");
    expect(within(list).getByRole("button", { name: /demo_research/ })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
  });

  it("holds open the pack the chosen document came from", async () => {
    const { list } = await open("skills");
    expect(names(list)).toContain("DemoSkills worker");
    expect(names(list)).not.toContain("DemoResearch worker");
    /* held open, its heading is no toggle */
    expect(within(list).queryByRole("button", { name: /demo_skills/ })).toBeNull();
  });

  it("finds a pack's documents by the pack's name, opening it while searching", async () => {
    const { user, list } = await open();
    await user.type(
      screen.getByRole("combobox", { name: "Search default agent" }),
      "skills",
    );
    expect(names(list)).toEqual(["DemoSkills worker"]);
    expect(within(list).queryByRole("group", { name: "demo_research" })).toBeNull();
  });

  it("moves through only the documents shown with the arrow keys", async () => {
    const { user, list } = await open();
    const seen = new Set<string>();
    for (let i = 0; i < items.length + 2; i += 1) {
      await user.keyboard("{ArrowDown}");
      /* the highlight always names an option on screen, never a folded one */
      const active = list.getAttribute("aria-activedescendant");
      if (!active) continue;
      const option = document.getElementById(active);
      expect(option).not.toBeNull();
      seen.add(option!.textContent ?? "");
    }
    expect([...seen].every((name) => own.includes(name))).toBe(true);
    expect(seen.size).toBeGreaterThan(1);
  });
});

function Skills({ initial }: { initial: string[] }) {
  const [value, setValue] = useState(initial);
  return (
    <ChipsRow
      id="skills"
      label="Skills"
      value={value}
      onChange={setValue}
      items={[
        { value: "summarize", label: "Summarize a file" },
        { value: "todos", label: "Find TODOs" },
        { value: "cite", label: "Cite sources", pack: "demo_research" },
      ]}
    />
  );
}

describe("a multi-select", () => {
  const openSkills = async (initial: string[] = []) => {
    const user = userEvent.setup();
    renderIn(testApp(), <Skills initial={initial} />);
    await user.click(screen.getByRole("combobox", { name: "Skills" }));
    return { user, list: await screen.findByRole("listbox") };
  };

  it("folds a pack's items under its name, after the person's own", async () => {
    const { user, list } = await openSkills();
    expect(names(list)).toEqual(["Summarize a file", "Find TODOs"]);
    await user.click(within(list).getByRole("button", { name: /demo_research · 1/ }));
    expect(names(list)).toEqual(["Summarize a file", "Find TODOs", "Cite sources"]);
  });

  it("holds a pack open while one of its items is picked, or while searching", async () => {
    const { list } = await openSkills(["cite"]);
    expect(names(list)).toContain("Cite sources");
    expect(within(list).queryByRole("button", { name: /demo_research/ })).toBeNull();
  });

  it("finds a pack's items by the pack's name", async () => {
    const { user, list } = await openSkills();
    await user.type(screen.getByRole("combobox", { name: "Skills" }), "research");
    expect(names(list)).toEqual(["Cite sources"]);
  });
});
