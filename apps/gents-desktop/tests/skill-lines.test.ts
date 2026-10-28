import { describe, expect, it } from "vitest";
import { chipSkills, splitSkillLines, withSkillLines } from "../src/ui/lib/skillLines";

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
