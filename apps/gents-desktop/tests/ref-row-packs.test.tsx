import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { RefRow, type Choice } from "../src/ui/screens/agent/editors";
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

function Field() {
  const [value, setValue] = useState("engineer");
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

const open = async () => {
  renderIn(testApp(), <Field />);
  await userEvent
    .setup()
    .click(screen.getByRole("combobox", { name: "Default agent" }));
  return screen.findByRole("listbox");
};

describe("a reference field", () => {
  it("lists the person's own documents first, then each pack's under its name", async () => {
    const list = await open();
    const names = within(list)
      .getAllByRole("option")
      .map((o) => o.textContent);
    expect(names.slice(0, 6)).toEqual([
      "The Engineer",
      "Lister",
      "Reviewer",
      "Writer",
      "Planner",
      "Editor",
    ]);
    expect(names.slice(6)).toEqual(["DemoResearch worker", "DemoSkills worker"]);
    const pack = within(list).getByRole("group", { name: "demo_research" });
    expect(within(pack).getByRole("option")).toHaveTextContent("DemoResearch worker");
  });

  it("finds a pack's documents by the pack's name", async () => {
    const user = userEvent.setup();
    const list = await open();
    await user.type(
      screen.getByRole("combobox", { name: "Search default agent" }),
      "skills",
    );
    expect(
      within(list)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["DemoSkills worker"]);
    expect(within(list).queryByRole("group", { name: "demo_research" })).toBeNull();
  });
});
