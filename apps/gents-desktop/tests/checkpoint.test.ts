import { describe, expect, it } from "vitest";
import { checkpointSections, isLeadingSection, sectionLabel } from "@/lib/checkpoint";

const summary = [
  "# Continuation checkpoint",
  "",
  "## Goal",
  "",
  "Write a series of different snake stories,",
  "using no tools.",
  "",
  "## Progress",
  "",
  "### Done",
  "",
  "- Six stories, each a different genre.",
  "",
  "### Blocked",
  "",
  "- Nothing.",
  "",
  "## Verification",
  "",
  "- NOT RUN: word counts were never measured.",
  "",
  "## Next actions",
  "",
  "1. Pick a genre not yet used.",
  "2. Keep the same quality.",
].join("\n");

describe("checkpointSections", () => {
  it("splits the runtime's checkpoint into its sections", () => {
    expect(checkpointSections(summary)).toEqual([
      {
        title: "Goal",
        text: "Write a series of different snake stories, using no tools.",
        items: [],
        ordered: false,
      },
      {
        title: "Done",
        text: null,
        items: ["Six stories, each a different genre."],
        ordered: false,
      },
      { title: "Blocked", text: null, items: ["Nothing."], ordered: false },
      {
        title: "Verification",
        text: null,
        items: ["NOT RUN: word counts were never measured."],
        ordered: false,
      },
      {
        title: "Next actions",
        text: null,
        items: ["Pick a genre not yet used.", "Keep the same quality."],
        ordered: true,
      },
    ]);
  });

  it("leaves a summary in any other shape to be shown as written", () => {
    expect(
      checkpointSections("The agent read the parser and fixed the race."),
    ).toBeNull();
    expect(checkpointSections("# Continuation checkpoint\n\nJust prose.")).toBeNull();
  });

  it("labels the sections briefly and leads with the goal, next actions and doubts", () => {
    const sections = checkpointSections(summary)!;
    expect(sections.map((section) => sectionLabel(section.title))).toEqual([
      "Goal",
      "Done",
      "Blocked",
      "Checked",
      "Next",
    ]);
    expect(sections.filter(isLeadingSection).map((section) => section.title)).toEqual([
      "Goal",
      "Blocked",
      "Next actions",
    ]);
    expect(sectionLabel("Something new")).toBe("Something new");
  });
});
