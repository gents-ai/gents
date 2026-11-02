import { describe, expect, it } from "vitest";
import {
  chipSkills,
  splitSkillLines,
  withSkillLines,
  writtenText,
} from "../src/ui/lib/skillLines";

const chip = (id: string, kind = "command") =>
  ({ type: "chip", chip: { kind, id, label: `/${id}` } }) as const;

describe("skills sent with a message", () => {
  it("come from its skill chips, once each, wherever they sit", () => {
    const parts = [
      { type: "text", text: "please " } as const,
      chip("review"),
      { type: "text", text: " and " } as const,
      chip("review"),
      chip("lint"),
      chip("session-1", "session"),
      chip("not-offered"),
    ];
    expect(chipSkills(parts, ["review", "lint"])).toEqual(["review", "lint"]);
  });

  it("are named on opening /skill lines, which gents reads for any id, reserved or not", () => {
    expect(withSkillLines("/lint fix\nthen /review", ["review", "lint"])).toBe(
      "/skill review\n/skill lint\n/lint fix\nthen /review",
    );
    expect(withSkillLines("no skills here /etc", [])).toBe("no skills here /etc");
  });

  it("come back off a message to show it as gents will store it", () => {
    expect(splitSkillLines("/skill review\n/skill lint\nplease /review it")).toEqual({
      text: "please /review it",
      skills: ["review", "lint"],
    });
    expect(splitSkillLines("/skill review")).toEqual({
      text: "/skill review",
      skills: [],
    });
    expect(splitSkillLines("/review it")).toEqual({ text: "/review it", skills: [] });
  });
});

describe("a sent message copied as it was written", () => {
  it("opens with the skills its text doesn't name, as the composer chips them", () => {
    expect(writtenText("test", ["test-pirate"])).toBe("/test-pirate test");
    expect(writtenText("fix it", ["review", "lint"])).toBe("/review\n/lint fix it");
    expect(writtenText("", ["test-pirate"])).toBe("/test-pirate");
  });

  it("leaves a skill its text names where it is, and plain text as it was", () => {
    expect(writtenText("please /review this", ["review"])).toBe("please /review this");
    expect(writtenText("see /review.md", ["review"])).toBe("/review see /review.md");
    expect(writtenText("plain", [])).toBe("plain");
    expect(writtenText("plain", null)).toBe("plain");
  });
});
