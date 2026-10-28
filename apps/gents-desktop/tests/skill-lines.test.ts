import { describe, expect, it } from "vitest";
import { withSkillLines } from "../src/ui/lib/skillLines";

const skills = ["review", "summarize"];

describe("skills named after the opening lines", () => {
  it("get an opening line each, so they run, and the text stays as written", () => {
    expect(withSkillLines("please /review this and /summarize", skills)).toBe(
      "/review\n/summarize\nplease /review this and /summarize",
    );
  });

  it("are left alone when the opening lines already name them", () => {
    expect(withSkillLines("/review this\nthen /review again", skills)).toBe(
      "/review this\nthen /review again",
    );
    expect(withSkillLines("\n/review\nbody", skills)).toBe("\n/review\nbody");
  });

  it("ignore names that aren't the agent's skills, paths, and words with a slash inside", () => {
    expect(withSkillLines("see /etc and a/review or /review/x", skills)).toBe(
      "see /etc and a/review or /review/x",
    );
  });

  it("add only what's missing when a later line names another skill", () => {
    expect(withSkillLines("/review this\nand /summarize it", skills)).toBe(
      "/summarize\n/review this\nand /summarize it",
    );
  });
});
