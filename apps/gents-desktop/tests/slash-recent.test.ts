import { beforeEach, describe, expect, it, vi } from "vitest";
import { noteSlash, rankSlash, recentSlash } from "../src/ui/lib/slashRecent";

const rows = [
  { key: "command:goal", name: "goal", group: "Commands" },
  { key: "skill:review", name: "review", also: "Code review", group: "Skills" },
  { key: "skill:summarize", name: "summarize", group: "Skills" },
  { key: "skill:preview", name: "preview", group: "Skills" },
  { key: "skill:release", name: "release", group: "Skills" },
];
const keys = (ranked: { key: string }[]) => ranked.map((row) => row.key);

beforeEach(() => {
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => void stored.set(key, value),
    removeItem: (key: string) => void stored.delete(key),
  });
});

describe("recently used slash rows", () => {
  it("leads with the last three used before anything is typed, and keeps the rest in order", () => {
    const ranked = rankSlash(rows, "", [
      "skill:release",
      "skill:review",
      "command:goal",
      "skill:preview",
    ]);
    expect(keys(ranked)).toEqual([
      "skill:release",
      "skill:review",
      "command:goal",
      "skill:summarize",
      "skill:preview",
    ]);
    expect(ranked.slice(0, 3).every((row) => row.group === "Recent")).toBe(true);
    expect(ranked[3]?.group).toBe("Skills");
  });

  it("puts names that start with the query first, and breaks ties by recent use", () => {
    const ranked = rankSlash(rows, "re", ["skill:release", "skill:preview"]);
    expect(keys(ranked)).toEqual(["skill:release", "skill:review", "skill:preview"]);
    expect(ranked.every((row) => row.group === undefined)).toBe(true);
  });

  it("matches a skill's other names too", () => {
    expect(keys(rankSlash(rows, "code", []))).toEqual(["skill:review"]);
  });

  it("remembers picks newest first, once each", () => {
    noteSlash("skill:review");
    noteSlash("command:goal");
    noteSlash("skill:review");
    expect(recentSlash()).toEqual(["skill:review", "command:goal"]);
  });
});
